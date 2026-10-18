//! Map tile fetching over HTTPS for the desktop app: worker threads with `ureq` (rustls, no
//! system TLS). The UI asks through `Services::tile_fetch` (GET, the tiles) and
//! `Services::tile_post_fetch` (POST, the Google Map Tiles createSession handshake); answers go
//! back on the per-request channel the UI owns (see `terrazon_ui_egui::tiles`).
//!
//! The workers share one job stack that serves the newest ask first: after a pan or zoom the tiles
//! the view just uncovered jump ahead of the ones it left, so the middle of the screen fills
//! first, Google-Maps order — the single biggest reason tiles felt slow.
//!
//! GETs keep a disk cache: fetched tiles land under the platform's cache folder keyed by the URL's
//! hash, and a later request for the same URL — the next session, a pan back, a restyle — is
//! answered from disk without the network. Entries expire (the world changes) and the cache is
//! pruned to a cap, oldest first, when the app starts. Sessions (POSTs) are never cached: their
//! tokens are short-lived. The Tiles panel reads the cache's size through
//! `Services::tile_cache` ([`TileCacheProbe`]) and can clear it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use terrazon_ui_egui::tiles::{TileCacheProbe, TileCacheStats, TileFetcher, TileMsg, TilePoster};

/// How many tiles fetch in parallel. Tiles come from a handful of hosts and each request waits on
/// the network most of its life, so a small crowd of workers fills a viewport in a fraction of the
/// serial time (browsers run six to eight connections against the same hosts).
const WORKERS: usize = 8;
/// One tile is tens of KB; anything past this is not a tile (a guard against huge responses).
const MAX_TILE_BYTES: u64 = 8 << 20;
/// The fetch timeout per request.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The connect timeout: one dead host must not hold a worker for the whole fetch timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The user agent the tile hosts see (OpenStreetMap's policy asks for an honest, contactable one).
const USER_AGENT: &str = concat!("TerraZone/", env!("CARGO_PKG_VERSION"), " (map illustration app; local user)");

/// The disk cache's cap; when the app starts past it, the oldest files go first.
const DISK_CAP_BYTES: u64 = 512 << 20;
/// A cached tile older than this is fetched again (streets move, imagery refreshes).
const DISK_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// The job stack's cap. Past it the oldest ask goes (its tile still shows as loading and the view
/// asks again): a fast pan across the world must not stack thousands of tiles nobody looks at.
const QUEUE_CAP: usize = 512;
/// How long an idle worker naps between checks. Pushes wake a worker at once — the nap only
/// bounds a quiet pool's idle wake-ups.
const PARK: Duration = Duration::from_millis(250);

/// One queued request: a tile GET, or a POST (the Google session handshake).
enum Job {
    Get { key: String, url: String, tx: Sender<TileMsg> },
    Post { key: String, url: String, content_type: String, body: String, tx: Sender<TileMsg> },
}

/// The workers' shared job stack, newest first: `push` puts a request on top, `pop` takes it back
/// off the top, so the most recent ask — the tiles the view just uncovered — is served first and a
/// pan never waits behind tiles the view left. The mutex is never poisoned on purpose; a poisoned
/// one (a worker panicked mid-job) only means the requests after it are dropped and asked again.
struct JobStack {
    q: Mutex<Vec<Job>>,
    cv: Condvar,
}

impl JobStack {
    fn new() -> Self {
        Self { q: Mutex::new(Vec::new()), cv: Condvar::new() }
    }

    fn lock(&self) -> Option<MutexGuard<'_, Vec<Job>>> {
        self.q.lock().ok()
    }

    /// Queue a request on top; past the cap the oldest ask is dropped — and told so: the UI
    /// already shows that tile as loading, and an ask that never runs would leave the tile dark
    /// forever. The drop notice clears it so the view asks again (the re-ask lands on top).
    fn push(&self, job: Job) {
        let dropped = if let Some(mut q) = self.lock() {
            let dropped = if q.len() >= QUEUE_CAP { Some(q.remove(0)) } else { None };
            q.push(job);
            // Notifying under the lock can't miss: a worker either sees the job when it next
            // locks, or is already parked on the condition variable and wakes now.
            self.cv.notify_one();
            dropped
        } else {
            Some(job)
        };
        if let Some(job) = dropped {
            let (key, tx) = match job {
                Job::Get { key, tx, .. } | Job::Post { key, tx, .. } => (key, tx),
            };
            let _ = tx.send(TileMsg::Dropped { key });
        }
    }

    /// The newest job, waiting up to [`PARK`] for one to arrive (`None` after a quiet nap).
    fn pop(&self) -> Option<Job> {
        let guard = self.lock()?;
        let (mut q, _) = self.cv.wait_timeout(guard, PARK).ok()?;
        q.pop()
    }
}

/// The host-side tile fetcher: requests queue to the workers.
#[derive(Clone)]
pub struct UreqTiles {
    jobs: Arc<JobStack>,
    /// The disk cache's folder, resolved once at start (`None` when this run keeps nothing on
    /// disk, e.g. under `TERRAZONE_NO_PREFS`): the probe reads and clears through it.
    cache: Option<Arc<PathBuf>>,
}

impl UreqTiles {
    /// Start the worker threads.
    pub fn start() -> Self {
        let jobs = Arc::new(JobStack::new());
        // The disk cache is resolved once: swept to its cap before any worker runs, then shared
        // read-only (`None` when this run keeps nothing on disk).
        let cache = tile_cache_dir().map(Arc::new);
        if let Some(dir) = &cache {
            cache_sweep(dir, DISK_CAP_BYTES);
        }
        for i in 0..WORKERS {
            let jobs = jobs.clone();
            let cache = cache.clone();
            let spawned = std::thread::Builder::new().name(format!("tile-fetch-{i}")).spawn(move || {
                let agent = crate::http::agent_builder().timeout(TIMEOUT).timeout_connect(CONNECT_TIMEOUT).user_agent(USER_AGENT).build();
                loop {
                    // A quiet nap (`pop`'s `None`) is not a shutdown: the workers serve the app's
                    // whole life, and a fetch wave can start any moment after any idle gap — a
                    // worker that quit on the first quiet wake left every later ask unpopped and
                    // its tile "loading" forever.
                    let Some(job) = jobs.pop() else { continue };
                    let msg = match job {
                        Job::Get { key, url, tx } => {
                            // A fresh disk copy answers without the network — the next session, a
                            // pan back, a restyle all read from here. A miss fetches and keeps a
                            // copy for next time.
                            let msg = match cache.as_ref().and_then(|dir| cache_lookup(dir, &url)) {
                                Some(bytes) => TileMsg::Done { key, bytes },
                                None => match agent.get(&url).call() {
                                    Ok(resp) => {
                                        let msg = read_tile(key, resp);
                                        if let TileMsg::Done { bytes, .. } = &msg
                                            && let Some(dir) = &cache
                                        {
                                            cache_store(dir, &url, bytes);
                                        }
                                        msg
                                    }
                                    Err(_) => TileMsg::Failed { key },
                                },
                            };
                            (msg, tx)
                        }
                        // The Google session token is short-lived: never cached.
                        Job::Post { key, url, content_type, body, tx } => {
                            let msg = match agent.post(&url).set("Content-Type", &content_type).send(body.as_bytes()) {
                                Ok(resp) => read_tile(key, resp),
                                Err(_) => TileMsg::Failed { key },
                            };
                            (msg, tx)
                        }
                    };
                    // The UI gone (closed document, quit) has no use for the answer.
                    let (msg, tx) = msg;
                    let _ = tx.send(msg);
                }
            });
            if spawned.is_err() {
                log::warn!("terrazon: could not start a tile fetcher");
            }
        }
        Self { jobs, cache }
    }
}

/// Read a response's body into a `Done` (a full, non-empty answer) or a `Failed`.
fn read_tile(key: String, resp: ureq::Response) -> TileMsg {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    match resp.into_reader().take(MAX_TILE_BYTES).read_to_end(&mut bytes) {
        Ok(_) if !bytes.is_empty() => TileMsg::Done { key, bytes },
        _ => TileMsg::Failed { key },
    }
}

impl TileFetcher for UreqTiles {
    fn fetch(&self, key: String, url: String, tx: Sender<TileMsg>) {
        self.jobs.push(Job::Get { key, url, tx });
    }
}

/// The DEM fetcher: the tile pool's own workers serve the Terrarium elevation asks (the same
/// timeouts, the same disk cache — a DEM tile answers from it on the next session). Each ask
/// gets a tiny converter that turns the PNG answer into the elevation floats the scene stitches.
pub struct UreqDem {
    jobs: Arc<JobStack>,
}

impl UreqDem {
    /// The DEM fetcher over [`UreqTiles`]'s worker pool.
    pub fn of(tiles: &UreqTiles) -> Self {
        Self { jobs: tiles.jobs.clone() }
    }
}

impl terrazon_scene3d::DemFetcher for UreqDem {
    fn fetch(&self, key: String, url: String, tx: Sender<terrazon_scene3d::dem::DemMsg>) {
        let (ttx, trx) = std::sync::mpsc::channel::<TileMsg>();
        let name = format!("dem-{key}");
        let job_key = format!("dem/{key}");
        let spawned = std::thread::Builder::new().name(name).spawn(move || {
            let elev = match trx.recv().ok().as_ref() {
                Some(TileMsg::Done { bytes, .. }) => terrazon_scene3d::dem::decode_terrarium_png(bytes).unwrap_or_default(),
                // A failed/lost ask answers empty: the loader forgets the tile (the next
                // re-anchor asks again) instead of waiting forever.
                _ => Vec::new(),
            };
            let _ = tx.send(terrazon_scene3d::dem::DemMsg { key, elev });
        });
        if spawned.is_err() {
            // No converter: the ask is dropped rather than left pending forever.
            return;
        }
        self.jobs.push(Job::Get { key: job_key, url, tx: ttx });
    }
}

impl TilePoster for UreqTiles {
    fn post(&self, key: String, url: String, content_type: String, body: String, tx: Sender<TileMsg>) {
        self.jobs.push(Job::Post { key, url, content_type, body, tx });
    }
}

impl TileCacheProbe for UreqTiles {
    /// The disk cache's size: its tile count and bytes (`None` when this run keeps no cache).
    fn stats(&self) -> Option<TileCacheStats> {
        cache_stats(self.cache.as_ref()?)
    }

    /// Empty the disk cache (the Tiles panel's Clear); the next fetch repopulates it.
    fn clear(&self) {
        if let Some(dir) = self.cache.as_deref() {
            cache_clear(dir);
        }
    }
}

/// Where the tile disk cache lives: ~/Library/Caches/TerraZone (macOS), %LOCALAPPDATA%\TerraZone
/// (Windows), $XDG_CACHE_HOME or ~/.cache/terrazon (Linux). A run without preferences
/// (`TERRAZONE_NO_PREFS`, agents' test runs) keeps nothing on disk.
fn tile_cache_dir() -> Option<PathBuf> {
    if std::env::var_os("TERRAZONE_NO_PREFS").is_some() {
        return None;
    }
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches/TerraZone/tile-cache"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")).map(|a| PathBuf::from(a).join("TerraZone/tile-cache"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|c| c.join("terrazon/tile-cache"))
    }
}

/// The cache file of a URL: the two-shard hex of its hash. The URL (not the UI's cache key, which
/// may carry a style stamp) is what names the bytes — one disk copy serves every style of a tile.
fn cache_file(dir: &Path, url: &str) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    let hex = format!("{:016x}", h.finish());
    dir.join(&hex[..2]).join(&hex[2..])
}

/// Read a URL's tile from the disk cache: the bytes when a fresh, tile-sized, non-empty copy is
/// there, else `None` (a stale copy is a miss — the fetch overwrites it).
fn cache_lookup(dir: &Path, url: &str) -> Option<Vec<u8>> {
    let file = cache_file(dir, url);
    let meta = std::fs::metadata(&file).ok()?;
    if meta.len() == 0 || meta.len() > MAX_TILE_BYTES {
        return None;
    }
    if meta.modified().ok().is_some_and(|m| m.elapsed().unwrap_or_default() > DISK_MAX_AGE) {
        return None;
    }
    std::fs::read(&file).ok().filter(|b| !b.is_empty())
}

/// Put a fetched tile in the disk cache, best effort: a temp file renamed into place, so a reader
/// never sees a half-written tile. A failure just means the next ask hits the network.
fn cache_store(dir: &Path, url: &str, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let file = cache_file(dir, url);
    let Some(parent) = file.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!("{name}.tmp{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
    let _ = std::fs::remove_file(&tmp);
}

/// The disk cache's size: one entry per tile file, bytes as they are on disk (`None` when the
/// folder is missing or holds nothing — the panel then hides the cache line).
fn cache_stats(dir: &Path) -> Option<TileCacheStats> {
    let shards = std::fs::read_dir(dir).ok()?;
    let mut stats = TileCacheStats::default();
    for shard in shards.flatten() {
        let Ok(entries) = std::fs::read_dir(shard.path()) else { continue };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_file() && meta.len() > 0 {
                stats.tiles += 1;
                stats.bytes += meta.len();
            }
        }
    }
    (stats.tiles > 0).then_some(stats)
}

/// Empty the disk cache: the shard folders go with it; the next fetch recreates them.
fn cache_clear(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Prune the disk cache to `cap` bytes, oldest first. Run once at start: the walk is a couple of
/// hundred shard folders of small files, and the workers share the read-only dir after it.
fn cache_sweep(dir: &Path, cap: u64) {
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    let Ok(shards) = std::fs::read_dir(dir) else { return };
    for shard in shards.flatten() {
        let Ok(entries) = std::fs::read_dir(shard.path()) else { continue };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            total += meta.len();
            files.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len(), e.path()));
        }
    }
    if total <= cap {
        return;
    }
    files.sort_by_key(|(m, _, _)| *m);
    for (_, size, path) in files {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A scratch cache dir the test removes after (no tempfile dep in the bin crate).
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("terrazon-tiles-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn job() -> (Job, std::sync::mpsc::Receiver<TileMsg>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Job::Get { key: "k".into(), url: "https://tiles.example/1.png".into(), tx }, rx)
    }

    #[test]
    fn cache_file_shards_by_hash_stably() {
        let a = cache_file(Path::new("/tmp/x"), "https://tile.openstreetmap.org/4/8/8.png");
        let b = cache_file(Path::new("/tmp/x"), "https://tile.openstreetmap.org/4/8/8.png");
        assert_eq!(a, b, "the same URL reads the same file");
        let hex = a.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned() + a.file_name().unwrap().to_string_lossy().as_ref();
        assert_eq!(hex.len(), 16, "the file name is the 16-hex hash, sharded 2 + 14");
        assert_ne!(a, cache_file(Path::new("/tmp/x"), "https://tile.openstreetmap.org/4/8/9.png"));
    }

    #[test]
    fn disk_cache_roundtrip_and_misses() {
        let dir = Scratch::new("roundtrip");
        let url = "https://tiles.example/2/1/1.png";
        assert!(cache_lookup(&dir.0, url).is_none(), "nothing stored yet");
        cache_store(&dir.0, url, b"PNG bytes");
        assert_eq!(cache_lookup(&dir.0, url).as_deref(), Some(b"PNG bytes".as_slice()));
        // A store of nothing is ignored (the copy stays); a truncated (empty) file on disk — a
        // crash mid-write, say — is not a tile and reads as a miss.
        cache_store(&dir.0, url, b"");
        assert_eq!(cache_lookup(&dir.0, url).as_deref(), Some(b"PNG bytes".as_slice()));
        std::fs::write(cache_file(&dir.0, url), b"").unwrap();
        assert!(cache_lookup(&dir.0, url).is_none(), "an empty file is not a tile");
    }

    #[test]
    fn sweep_keeps_the_cache_under_its_cap() {
        let dir = Scratch::new("sweep");
        for i in 0..4 {
            cache_store(&dir.0, &format!("https://tiles.example/{i}.png"), &[b'x'; 100]);
        }
        cache_sweep(&dir.0, 1 << 20);
        assert!(cache_lookup(&dir.0, "https://tiles.example/0.png").is_some(), "under the cap nothing goes");
        cache_sweep(&dir.0, 0);
        assert!(cache_lookup(&dir.0, "https://tiles.example/0.png").is_none(), "a zero cap sweeps everything");
    }

    #[test]
    fn stats_counts_the_tiles_and_bytes() {
        let dir = Scratch::new("stats");
        assert!(cache_stats(&dir.0).is_none(), "no folder yet, no stats");
        cache_store(&dir.0, "https://tiles.example/a.png", &[b'a'; 300]);
        cache_store(&dir.0, "https://tiles.example/b.png", &[b'b'; 100]);
        let s = cache_stats(&dir.0).unwrap();
        assert_eq!(s.tiles, 2, "one entry per tile file");
        assert_eq!(s.bytes, 400, "the bytes as they are on disk");
        cache_clear(&dir.0);
        assert!(cache_stats(&dir.0).is_none(), "a cleared cache reads as none");
    }

    #[test]
    fn the_stack_serves_the_newest_ask_first() {
        let jobs = JobStack::new();
        let (a, _ra) = job();
        let (b, _rb) = job();
        let (c, _rc) = job();
        jobs.push(a);
        jobs.push(b);
        jobs.push(c);
        // The last push pops first: after a pan, the tiles the view just uncovered load before
        // the ones it left.
        assert!(matches!(jobs.pop(), Some(Job::Get { .. })), "the newest job is on top");
        // Past the cap the oldest ask goes — a fast pan never stacks unbounded work.
        let (last, _) = job();
        jobs.push(last);
        for _ in 1..(QUEUE_CAP + 8) {
            let (j, _) = job();
            jobs.push(j);
        }
        assert_eq!(jobs.lock().unwrap().len(), QUEUE_CAP, "the stack holds at most the cap");
        // The first job went off the bottom; the newest of the flood is on top.
        assert!(matches!(jobs.pop(), Some(Job::Get { .. })));
        assert_eq!(jobs.lock().unwrap().len(), QUEUE_CAP - 1);
    }

    #[test]
    fn a_dropped_ask_tells_its_tile() {
        let jobs = JobStack::new();
        let (first, rfirst) = job();
        jobs.push(first);
        for _ in 0..QUEUE_CAP {
            let (j, _) = job();
            jobs.push(j);
        }
        // The first ask fell off the bottom of the full stack: its channel heard about it, so the
        // UI clears that tile's entry and asks again instead of showing a dark tile forever.
        match rfirst.try_recv() {
            Ok(TileMsg::Dropped { key }) => assert_eq!(key, "k"),
            other => panic!("the dropped ask was not reported: {other:?}"),
        }
    }

    #[test]
    fn a_worker_answers_after_an_idle_gap() {
        // The workers serve the app's whole life: `pop`'s quiet wake (`None` after the PARK nap)
        // is not a shutdown. A worker that quit on it left every later ask unpopped — the tile
        // shows "loading" forever and the disk cache never fills (the zero-fetches bug this
        // pins). The ask's URL is deliberately unfetchable, so the answer comes fast and needs
        // no network: a fast `Failed` still proves a worker took the job.
        let host = UreqTiles::start();
        let (tx1, rx1) = std::sync::mpsc::channel();
        host.fetch("k1".into(), "urn:terrazon-test:unreachable".into(), tx1);
        match rx1.recv_timeout(Duration::from_secs(10)) {
            Ok(TileMsg::Failed { key }) => assert_eq!(key, "k1"),
            other => panic!("the first ask was not answered: {other:?}"),
        }
        // Longer than the workers' quiet nap: the next ask must still be picked up.
        std::thread::sleep(PARK * 3);
        let (tx2, rx2) = std::sync::mpsc::channel();
        host.fetch("k2".into(), "urn:terrazon-test:unreachable".into(), tx2);
        match rx2.recv_timeout(Duration::from_secs(10)) {
            Ok(TileMsg::Failed { key }) => assert_eq!(key, "k2"),
            other => panic!("the ask after the idle gap was never fetched: {other:?}"),
        }
    }
}
