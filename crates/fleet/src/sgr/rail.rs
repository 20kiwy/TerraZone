//! The SGR rail module: Tanzania's Standard Gauge Railway as **schedules, not live positions**.
//! The TRC TICIDIS ticketing API is a booking service — it knows stations and trip times, and
//! says nothing about where a train is right now. So TerraZone does what the timetable itself
//! does: the system sweeps the API once per poll cycle (one station-pair search per step —
//! gentle, the vendor is never hammered), merges every trip's stop times into one timetable,
//! and the trains are then placed **algorithmically** along the OSM rail network by their own
//! times — the same arithmetic the passenger does: the train left Morogoro at 07:45 and takes
//! 35 minutes to Kilosa, so at 08:00 it is about halfway there, and the ETA at every later
//! station is just the schedule minus now.
//!
//! Everything here is network-free: the sweep's requests are *described* by [`SgrAuth`], the
//! answers are parsed by [`absorb_answer`] into a [`SgrTimetable`] (the document keeps it), and
//! [`SgrTimetable::project`] turns it into train markers riding the [`RailAtlas`] — the OSM
//! railway lines and stations the map highlights underneath.

use serde::{Deserialize, Serialize};

use crate::{Provider, Vehicle, VehicleKind, feed::FleetRequest, sgr::seats};

/// The TICIDIS public API base (keyless reads only — the booking flow stays the vendor's).
pub const SGR_BASE: &str = "https://sgrticket-api.trc.co.tz/TICIDIS/api/v1";

/// The sweep's step count: step 0 re-reads the station list, steps 1–11 ask each station's
/// schedule toward Dodoma (the up terminal), steps 12–22 each station toward Dar es Salaam.
/// One step per poll tick: a full sweep takes 23 × the poll cadence (~6 min at 15 s), which is
/// as often as a timetable can honestly change.
pub const SWEEP_STEPS: u64 = 23;

/// The poll number a sweep step rides: poll 1 lands on step 0 (the stations), so the very
/// first sweep of a session names the stations before any pair search goes out.
pub fn sweep_step(poll_no: u64) -> u64 {
    (poll_no + SWEEP_STEPS - 1) % SWEEP_STEPS
}

/// Which terminal a sweep step searches toward (the row the trip list groups by).
fn sweep_pair(step: u64) -> (Option<i64>, i64) {
    match step {
        0 => (None, 0), // the station list itself
        s if s <= 11 => (Some(s as i64), 12),
        // steps 12..=22 → boarding stations 2..=12, landing Dar es Salaam (id 1)
        s => (Some((s - 10) as i64), 1),
    }
}

/// A nearest-point cut on a polyline: `(segment index, segment fraction, distance²)` — the
/// shape [`nearest_on`] answers and the slicers cut by.
pub type Cut = (usize, f64, f64);

/// One OSM railway line: the polyline the trains ride and the map highlights.
#[derive(Clone, Debug, PartialEq)]
pub struct RailLine {
    pub pts: Vec<[f64; 2]>,
    /// `(min_lon, min_lat, max_lon, max_lat)` — the cull box the 2D draw skips by.
    pub bbox: [f64; 4],
}

/// One named OSM railway station (the timetable's stops land on these).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RailStop {
    pub name: String,
    pub lon: f64,
    pub lat: f64,
}

/// The rail network the trains ride and the map highlights: OSM's Tanzania railways
/// (lines) and railway stations (named stops), embedded with the app. Parsed from the
/// compact JSON asset — `{"lines":[[lon,lat,…],…],"stations":[[name,lon,lat],…]}`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RailAtlas {
    pub lines: Vec<RailLine>,
    pub stations: Vec<RailStop>,
}

/// The crate's own embedded atlas asset (`crates/fleet/assets/tz-rail.json` — the OSM
/// railways and stations the SGR rides, extracted once from GeoFabrik's Tanzania extract).
/// `cargo xtask rail` loads and checks this same file, so the build's tooling and the app
/// read one asset: a broken or missing atlas fails the check before it ever reaches a map.
pub fn embedded_atlas() -> &'static RailAtlas {
    static ATLAS: std::sync::OnceLock<RailAtlas> = std::sync::OnceLock::new();
    ATLAS.get_or_init(|| RailAtlas::parse(include_bytes!("../../assets/tz-rail.json")).unwrap_or_default())
}

/// A timetable stop: one train, one station, one clock time (minutes since midnight EAT).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SgrStop {
    pub station_id: i64,
    pub name: String,
    /// Minutes since midnight, East Africa Time (the vendor's own clock, UTC+3).
    pub time_min: u32,
    /// True when this stop's time is the NEXT day (a train that departs 23:25 and arrives
    /// 00:45 crosses midnight; the vendor's row shows end < start). Keeps the running order
    /// through the by-minute sort.
    #[serde(default)]
    pub next_day: bool,
}

/// One coach of a train set, as the vendor's own `SearchTrip` answer reports it: the class
/// name, the seats it carries and how many are still free — the seat-availability ladder the
/// panel's selection reads (0 free reads "No seat available").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SgrCoach {
    pub id: i64,
    /// The vendor's class name ("Royal Class", "Business", "Economy").
    #[serde(rename = "typeName")]
    pub name: String,
    /// The coach's position in the set (the vendor's `orderNo`).
    #[serde(rename = "orderNo")]
    pub order: i32,
    pub capacity: i32,
    /// The vendor's `emptySeats` — the seats still bookable (which needs the vendor's login;
    /// this flow ends at the selection).
    #[serde(rename = "emptySeats")]
    pub empty: i32,
}

/// One scheduled train: its vendor identity (`trips_no`, e.g. `0011904`) and the stops the
/// sweep has merged so far, in running order — with the coaches the vendor reports for the
/// set (the latest non-empty answer's, so availability refreshes as the sweep walks).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SgrTrain {
    pub trips_no: String,
    pub name: String,
    /// 1 = toward Dodoma (the sweep's up searches), 2 = toward Dar es Salaam.
    pub direction: u8,
    pub stops: Vec<SgrStop>,
    #[serde(default)]
    pub cars: Vec<SgrCoach>,
    /// The vendor's `tripTimeTableTrainSetId` for this trip row — the id the seat-set ask
    /// (`TrainSetBy`) names. `None` until a trip answer carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trip_set_id: Option<i64>,
}

/// The SGR timetable the sweep builds: the trip date (Dar es Salaam days), the station list
/// and every train seen so far. Lives on the document (the SGR layer's `schedule`), so a
/// saved map file keeps its trains' world.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SgrTimetable {
    /// `YYYY-MM-DD` in EAT — the schedule day the sweep is building. A new day resets it.
    pub date: String,
    /// The station list, in the API's own order (Dar es Salaam first, Dodoma last).
    pub stations: Vec<(i64, String)>,
    pub trains: Vec<SgrTrain>,
    /// The active train's seat set (the sweep's `TrainSetBy` ask) and the `trips_no` it
    /// belongs to — the panel shows the coach grids under that train.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_set: Option<seats::TrainSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_set_trip: Option<String>,
}

/// One trip row out of a `SearchTrip` answer, normalized for the merge.
#[derive(Clone, Debug, PartialEq)]
pub struct SweptTrip {
    pub trips_no: String,
    pub trip_name: String,
    pub boarding_id: i64,
    pub boarding_name: String,
    pub landing_id: i64,
    pub landing_name: String,
    pub start_min: u32,
    pub end_min: u32,
    /// The trip row's own `railwayCars` (class, capacity, free seats), when it carried them.
    pub cars: Vec<SgrCoach>,
    /// The vendor's `tripTimeTableTrainSetId` (the seat-set ask names it).
    pub trip_set_id: Option<i64>,
}

/// What one sweep answer carried (the two answer shapes share an endpoint family, so the
/// reader fingerprints them — the runtime never needs to know which step a key rode).
#[derive(Clone, Debug, PartialEq)]
pub enum Absorb {
    Stations(Vec<(i64, String)>),
    Trips(Vec<SweptTrip>),
    /// A `TrainSetBy` answer: the active trip's coach grids (the seat map the panel reads).
    Seats(seats::TrainSet),
    /// `TRIP112 - Selected Journey is not found!` — a pair with no service that day: the
    /// sweep keeps walking.
    Empty,
}

/// `HH:MM:SS` (the vendor's clock) → minutes since midnight. `None` when unreadable.
pub fn parse_clock(s: &str) -> Option<u32> {
    let mut it = s.trim().split(':');
    let h: u32 = it.next()?.trim().parse().ok()?;
    let m: u32 = it.next()?.trim().parse().ok()?;
    if h > 47 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// Minutes since midnight → the `HH:MM` the panel and the map's scheduled markers show
/// (a minutes value of the next day's small hours keeps its own wall clock: 1470 → "00:30").
pub fn clock_label(min: u32) -> String {
    let m = min % 1440;
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// `YYYY-MM-DD` for the given unix seconds in East Africa Time (UTC+3, no DST) — the
/// schedule day the sweep builds.
pub fn today_eat(unix_secs: i64) -> String {
    let days = (unix_secs + 3 * 3600).div_euclid(86_400);
    civil_from_days(days)
}

/// Minutes since midnight in EAT for the given unix seconds.
pub fn now_minutes_eat(unix_secs: i64) -> u32 {
    let secs = (unix_secs + 3 * 3600).rem_euclid(86_400);
    (secs / 60) as u32
}

/// Howard Hinnant's civil-from-days — the proleptic Gregorian date for a day number.
fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

/// A stop name normalized for matching against the atlas: lowercase, punctuation out, a
/// trailing "station" dropped — "Mkata Station" and "Mkata" are one stop.
fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for w in name.split_whitespace() {
        let w = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if w.is_empty() || w.eq_ignore_ascii_case("station") || w.eq_ignore_ascii_case("halt") {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w.to_ascii_lowercase());
    }
    out
}

impl RailAtlas {
    /// Parse the embedded asset's JSON shape.
    pub fn parse(bytes: &[u8]) -> Result<RailAtlas, String> {
        let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("the rail atlas is not JSON: {e}"))?;
        let mut atlas = RailAtlas::default();
        for line in v.get("lines").and_then(|l| l.as_array()).map(Vec::as_slice).unwrap_or_default() {
            let Some(flat) = line.as_array() else { continue };
            let pts: Vec<[f64; 2]> = flat
                .as_chunks::<2>()
                .0
                .iter()
                .filter_map(|c| match (c[0].as_f64(), c[1].as_f64()) {
                    (Some(lon), Some(lat)) => Some([lon, lat]),
                    _ => None,
                })
                .collect();
            if pts.len() < 2 {
                continue;
            }
            atlas.lines.push(RailLine::new(pts));
        }
        for st in v.get("stations").and_then(|s| s.as_array()).map(Vec::as_slice).unwrap_or_default() {
            let Some(row) = st.as_array() else { continue };
            if row.len() < 3 {
                continue;
            }
            let (Some(name), Some(lon), Some(lat)) = (row[0].as_str(), row[1].as_f64(), row[2].as_f64()) else { continue };
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                continue;
            }
            atlas.stations.push(RailStop { name: name.to_string(), lon, lat });
        }
        if atlas.lines.is_empty() {
            return Err("the rail atlas carried no lines".into());
        }
        Ok(atlas)
    }

    /// The named station, by normalized name (exact first, then a contains match — the
    /// vendor says "Dar Es Salaam", OSM may say "Dar es Salaam railway station").
    pub fn station(&self, name: &str) -> Option<&RailStop> {
        let want = normalize(name);
        if want.is_empty() {
            return None;
        }
        self.stations.iter().find(|s| normalize(&s.name) == want).or_else(|| {
            self.stations
                .iter()
                .filter(|s| {
                    let have = normalize(&s.name);
                    have.contains(&want) || want.contains(&have)
                })
                .min_by_key(|s| normalize(&s.name).len())
        })
    }

    /// The polyline slice riding station `from` to station `to` along one line — the line
    /// that runs nearest both, cut between the two points. `None` when no line serves both.
    pub fn slice(&self, from: &str, to: &str) -> Option<Vec<[f64; 2]>> {
        let a = self.station(from)?;
        let b = self.station(to)?;
        if (a.lon, a.lat) == (b.lon, b.lat) {
            return Some(vec![[a.lon, a.lat], [b.lon, b.lat]]);
        }
        // Walk the lines once, scoring each by how near it runs to both stops.
        let mut best: Option<(f64, usize, Cut, Cut)> = None;
        for (li, line) in self.lines.iter().enumerate() {
            // Cheap reject: the pair's rough box must touch the line's bbox.
            let (min_lon, max_lon) = (a.lon.min(b.lon) - 0.05, a.lon.max(b.lon) + 0.05);
            let (min_lat, max_lat) = (a.lat.min(b.lat) - 0.05, a.lat.max(b.lat) + 0.05);
            if line.bbox[2] < min_lon || line.bbox[0] > max_lon || line.bbox[3] < min_lat || line.bbox[1] > max_lat {
                continue;
            }
            let (Some(pa), Some(pb)) = (nearest_on(&line.pts, a.lon, a.lat), nearest_on(&line.pts, b.lon, b.lat)) else { continue };
            let da = meters(pa.2);
            let db = meters(pb.2);
            if da > 3_000.0 || db > 3_000.0 {
                continue;
            }
            let score = da + db;
            if best.is_none_or(|(bs, ..)| score < bs) {
                best = Some((score, li, pa, pb));
            }
        }
        let (_, li, pa, pb) = best?;
        Some(slice_between(&self.lines[li].pts, pa, pb))
    }
}

impl RailLine {
    pub fn new(pts: Vec<[f64; 2]>) -> Self {
        let mut bbox = [f64::MAX; 4];
        for [lon, lat] in &pts {
            bbox[0] = bbox[0].min(*lon);
            bbox[1] = bbox[1].min(*lat);
            bbox[2] = bbox[2].max(*lon);
            bbox[3] = bbox[3].max(*lat);
        }
        Self { pts, bbox }
    }
}

/// The rail router: the timetable's stops sit hundreds of kilometres apart, and no single
/// OSM way does — the path between two stations is a **chain of connected ways**. The router
/// indexes the atlas's endpoints (ways that share a node share a bit-identical coordinate)
/// and walks that graph once per station pair, caching the result: the first projection of a
/// timetable pays for the walk, every later tick just cuts the cached polyline.
#[derive(Clone, Debug, Default)]
pub struct RailRouter {
    /// Quantized endpoint (5 decimals, the atlas's own rounding) → the lines touching it.
    endpoints: std::collections::HashMap<[i64; 2], Vec<usize>>,
    cache: std::collections::HashMap<(String, String), Option<Vec<[f64; 2]>>>,
}

/// The endpoint-quantization step (the atlas rounds to this, so shared nodes collide).
const ENDPOINT_STEP: f64 = 1e-5;

fn quant(v: f64) -> i64 {
    (v / ENDPOINT_STEP).round() as i64
}

impl RailRouter {
    pub fn new(atlas: &RailAtlas) -> Self {
        let mut endpoints: std::collections::HashMap<[i64; 2], Vec<usize>> = std::collections::HashMap::new();
        for (li, line) in atlas.lines.iter().enumerate() {
            let Some(last) = line.pts.last() else { continue };
            for end in [line.pts[0], *last] {
                endpoints.entry([quant(end[0]), quant(end[1])]).or_default().push(li);
            }
        }
        Self { endpoints, cache: std::collections::HashMap::new() }
    }

    /// The polyline slice riding `from` → `to`: the chained way path when the network
    /// connects them, the straight two-point path when it does not (a train still moves —
    /// the timing is the truth, the line just reads straighter).
    pub fn slice(&mut self, atlas: &RailAtlas, from: &str, to: &str) -> Option<Vec<[f64; 2]>> {
        let key = (from.to_string(), to.to_string());
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let path = self.uncached_slice(atlas, from, to);
        self.cache.insert(key, path.clone());
        path
    }

    fn uncached_slice(&mut self, atlas: &RailAtlas, from: &str, to: &str) -> Option<Vec<[f64; 2]>> {
        let a = atlas.station(from)?;
        let b = atlas.station(to)?;
        if (a.lon, a.lat) == (b.lon, b.lat) {
            return Some(vec![[a.lon, a.lat], [b.lon, b.lat]]);
        }
        let (la, _) = atlas.nearest_line(a.lon, a.lat)?;
        let (lb, _) = atlas.nearest_line(b.lon, b.lat)?;
        // One line serving both stops: the plain cut.
        if la == lb {
            let line = &atlas.lines[la];
            let (Some(pa), Some(pb)) = (nearest_on(&line.pts, a.lon, a.lat), nearest_on(&line.pts, b.lon, b.lat)) else { return None };
            return Some(slice_between(&line.pts, pa, pb));
        }
        // Chain the ways: from `la`'s end nearer `b`, hop across shared endpoints toward the
        // line that runs nearest `b`, until we stand on it (or give up — the fallback is the
        // straight pair, and a straight train beats a missing one).
        let mut visited = vec![false; atlas.lines.len()];
        visited[la] = true;
        let mut path: Vec<[f64; 2]> = atlas.lines[la].pts.clone();
        let mut cur = la;
        let mut hops = 0;
        while cur != lb && hops < 2_000 {
            hops += 1;
            // The chain leaves from whichever end of the current way sits nearer Dodoma (the
            // travel direction's far end).
            let line = &atlas.lines[cur];
            let (end_far, _end_near) = {
                let Some(e1) = line.pts.last().copied() else { break };
                let e0 = line.pts[0];
                let d0 = (e0[0] - b.lon).hypot(e0[1] - b.lat);
                let d1 = (e1[0] - b.lon).hypot(e1[1] - b.lat);
                if d0 <= d1 { (e0, e1) } else { (e1, e0) }
            };
            // Connected ways at that far end (shared node), best next hop = the one whose
            // other end runs nearest the target.
            let q = [quant(end_far[0]), quant(end_far[1])];
            let mut best: Option<(f64, usize, bool)> = None; // (other-end distance, line, reversed)
            for &li in self.endpoints.get(&q).into_iter().flatten() {
                if visited[li] || li == cur {
                    continue;
                }
                let cand = &atlas.lines[li];
                let (Some(c1v), c0) = (cand.pts.last().copied(), cand.pts[0]) else { continue };
                let (c0, c1) = (c0, c1v);
                let (near_far, reversed) = if (c0[0] - end_far[0]).hypot(c0[1] - end_far[1]) <= (c1[0] - end_far[0]).hypot(c1[1] - end_far[1]) {
                    (c1, false)
                } else {
                    (c0, true)
                };
                let d = (near_far[0] - b.lon).hypot(near_far[1] - b.lat);
                if best.is_none_or(|(bd, ..)| d < bd) {
                    best = Some((d, li, reversed));
                }
            }
            let Some((_, li, reversed)) = best else { break };
            visited[li] = true;
            let mut pts = atlas.lines[li].pts.clone();
            if reversed {
                pts.reverse();
            }
            path.extend(pts);
            cur = li;
        }
        // Cut the chained path between the two stations' nearest points.
        let (Some(pa), Some(pb)) = (nearest_on(&path, a.lon, a.lat), nearest_on(&path, b.lon, b.lat)) else {
            return Some(vec![[a.lon, a.lat], [b.lon, b.lat]]);
        };
        Some(slice_between(&path, pa, pb))
    }
}

impl RailAtlas {
    /// The line that runs nearest `(lon, lat)` and its nearest-point cut:
    /// `(line index, (segment, fraction, distance²))`.
    pub fn nearest_line(&self, lon: f64, lat: f64) -> Option<(usize, Cut)> {
        let mut best: Option<(f64, usize, (usize, f64, f64))> = None;
        for (li, line) in self.lines.iter().enumerate() {
            let [min_lon, min_lat, max_lon, max_lat] = line.bbox;
            if max_lon < lon - 0.05 || min_lon > lon + 0.05 || max_lat < lat - 0.05 || min_lat > lat + 0.05 {
                continue;
            }
            let Some(p) = nearest_on(&line.pts, lon, lat) else { continue };
            if best.is_none_or(|(bd, ..)| p.2 < bd) {
                best = Some((p.2, li, p));
            }
        }
        let (_, li, p) = best?;
        Some((li, p))
    }
}

/// The nearest point on a polyline: `(segment index, segment fraction, distance²)`.
fn nearest_on(pts: &[[f64; 2]], lon: f64, lat: f64) -> Option<Cut> {
    let mut best: Option<(usize, f64, f64)> = None;
    for i in 0..pts.len().saturating_sub(1) {
        let [x1, y1] = pts[i];
        let [x2, y2] = pts[i + 1];
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= f64::EPSILON { 0.0 } else { (((lon - x1) * dx + (lat - y1) * dy) / len2).clamp(0.0, 1.0) };
        let (px, py) = (x1 + t * dx, y1 + t * dy);
        let d2 = (px - lon) * (px - lon) + (py - lat) * (py - lat);
        if best.is_none_or(|(_, _, bd2)| d2 < bd2) {
            best = Some((i, t, d2));
        }
    }
    best
}

/// Metres from a squared-degree distance (1° ≈ 111 320 m — a walk, not a geodesic, is
/// plenty for the 3 km "this line serves this stop" gate).
fn meters(d2: f64) -> f64 {
    d2.sqrt() * 111_320.0
}

/// Cut a polyline between two nearest-points (ordered a → b as the stops name them).
fn slice_between(pts: &[[f64; 2]], a: (usize, f64, f64), b: (usize, f64, f64)) -> Vec<[f64; 2]> {
    let (ai, at, _) = a;
    let (bi, bt, _) = b;
    // Order the two cut points by arc position, remembering whether that ran against the
    // travel direction.
    let ((i0, t0), (i1, t1), flipped) = if (ai, at) <= (bi, bt) { ((ai, at), (bi, bt), false) } else { ((bi, bt), (ai, at), true) };
    let point = |i: usize, t: f64| -> [f64; 2] {
        let [x1, y1] = pts[i];
        let [x2, y2] = pts[i + 1];
        [x1 + t * (x2 - x1), y1 + t * (y2 - y1)]
    };
    let mut out = vec![point(i0, t0)];
    for i in i0..i1 {
        out.push(pts[i + 1]);
    }
    out.push(point(i1, t1));
    if flipped {
        out.reverse();
    }
    // Never a one-point path.
    if out.len() < 2 {
        let [lon, lat] = out.first().copied().unwrap_or([0.0, 0.0]);
        out.push([lon + 1e-5, lat]);
    }
    out
}

/// The point `f` (0…1) along a polyline by arc length, and the bearing there (degrees
/// clockwise from north).
pub fn point_at(path: &[[f64; 2]], f: f64) -> Option<([f64; 2], f32)> {
    if path.is_empty() {
        return None;
    }
    if path.len() == 1 {
        return Some((path[0], -1.0));
    }
    let mut total = 0.0;
    let mut lens = Vec::with_capacity(path.len() - 1);
    for w in path.windows(2) {
        let d = deg_dist(w[0], w[1]);
        lens.push(d);
        total += d;
    }
    if total <= f64::EPSILON {
        return Some((path[0], -1.0));
    }
    let mut target = f.clamp(0.0, 1.0) * total;
    for (i, d) in lens.iter().enumerate() {
        if target <= *d || i == lens.len() - 1 {
            let t = if *d <= f64::EPSILON { 0.0 } else { target / *d };
            let [x1, y1] = path[i];
            let [x2, y2] = path[i + 1];
            let p = [x1 + t * (x2 - x1), y1 + t * (y2 - y1)];
            let bearing = deg_bearing(x1, y1, x2, y2);
            return Some((p, bearing as f32));
        }
        target -= *d;
    }
    None
}

/// Rough metre distance between two lon/lat points.
fn deg_dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dlat, dlon) = (b[1] - a[1], b[0] - a[0]);
    let mlat = (a[1] + b[1]) * 0.5;
    let dy = dlat * 111_320.0;
    let dx = dlon * 111_320.0 * mlat.to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

/// Degrees clockwise from north.
fn deg_bearing(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

impl SgrTimetable {
    /// Absorb one sweep answer. A new schedule day (the `date` the requests ride) resets the
    /// tables — yesterday's trains do not run today.
    pub fn absorb(&mut self, date: &str, answer: &Absorb) {
        if self.date != date {
            self.date = date.to_string();
            self.trains.clear();
        }
        match answer {
            Absorb::Stations(list) => {
                if !list.is_empty() {
                    self.stations = list.clone();
                }
            }
            Absorb::Trips(trips) => {
                for t in trips {
                    self.absorb_trip(t);
                }
            }
            Absorb::Seats(set) => {
                // The set belongs to the train whose coaches it names: the car ids both
                // answers share are the link (a TrainSetBy answer carries no trip id of its
                // own). No match — the sweep asked before the trip rows landed; the next
                // seat ask (every third poll) re-fits it.
                let set_car_ids: Vec<i64> = set.coaches.iter().map(|c| c.id).collect();
                let trip = self.trains.iter().find(|t| t.cars.iter().any(|c| set_car_ids.contains(&c.id))).map(|t| t.trips_no.clone());
                self.seat_set = Some(set.clone());
                self.seat_set_trip = trip;
            }
            Absorb::Empty => {}
        }
    }

    fn absorb_trip(&mut self, t: &SweptTrip) {
        let train = match self.trains.iter_mut().find(|x| x.trips_no == t.trips_no) {
            Some(x) => x,
            None => {
                self.trains.push(SgrTrain {
                    trips_no: t.trips_no.clone(),
                    name: t.trip_name.clone(),
                    direction: if t.landing_id == 12 { 1 } else { 2 },
                    stops: Vec::new(),
                    cars: Vec::new(),
                    trip_set_id: None,
                });
                let n = self.trains.len();
                &mut self.trains[n - 1]
            }
        };
        if train.name.is_empty() {
            train.name = t.trip_name.clone();
        }
        // The seat-set id rides the newest answer that carries it.
        if t.trip_set_id.is_some() {
            train.trip_set_id = t.trip_set_id;
        }
        // The coaches ride the newest answer that carries them (availability moves; an answer
        // without cars — an older endpoint shape — leaves what we have).
        if !t.cars.is_empty() {
            train.cars = t.cars.clone();
        }
        for (id, name, min, next_day) in [
            (t.boarding_id, &t.boarding_name, t.start_min, false),
            // A row whose end reads at/below its start lands after midnight (the 23:25 → 00:45
            // shape): the landing stop is the next day.
            (t.landing_id, &t.landing_name, t.end_min, t.end_min < t.start_min),
        ] {
            if id <= 0 || name.is_empty() {
                continue;
            }
            match train.stops.iter_mut().find(|s| s.station_id == id) {
                Some(s) => {
                    s.time_min = min;
                    s.next_day = next_day;
                }
                None => train.stops.push(SgrStop { station_id: id, name: name.clone(), time_min: min, next_day }),
            }
        }
        train.stops.sort_by_key(|s| (s.next_day, s.time_min));
    }

    /// The train's running window in minutes-since-midnight: `(departure, arrival)`, where an
    /// arrival at/below the departure reads the next day (the 23:25 → 00:45 overnight).
    fn running_window(train: &SgrTrain) -> Option<(u32, u32)> {
        if train.stops.len() < 2 {
            return None;
        }
        let first = train.stops[0].time_min;
        let last = train.stops.last()?.time_min;
        Some((first, last + u32::from(last <= first) * 1440))
    }

    /// `now` mapped into the train's own frame: a midnight-crossing train carries the clock
    /// over with it (00:05 on board the 23:25 → 00:45 reads 1445). `None` when off the board.
    fn now_on_board(train: &SgrTrain, now_min: u32) -> Option<u32> {
        let (first, last) = Self::running_window(train)?;
        let now = if last > 1440 && now_min < first { now_min + 1440 } else { now_min };
        (now >= first && now <= last).then_some(now)
    }

    /// A segment's far end in the train's frame (the arrival over the midnight wrap).
    fn segment_end(w: (&SgrStop, &SgrStop)) -> u32 {
        w.1.time_min + u32::from(w.1.time_min <= w.0.time_min) * 1440
    }

    /// The trains running at `now_min` (between their first and last stop, midnight-crossing
    /// arrivals included).
    pub fn running(&self, now_min: u32) -> Vec<&SgrTrain> {
        self.trains.iter().filter(|t| Self::now_on_board(t, now_min).is_some()).collect()
    }

    /// Where a train stands at `now_min`: the next stop, the terminal, and the minutes to
    /// each — the ETA table the panel shows. `None` when it is not running.
    pub fn progress<'a>(&'a self, train: &'a SgrTrain, now_min: u32) -> Option<(&'a SgrStop, &'a SgrStop, u32, u32)> {
        let last = train.stops.last()?;
        let now = Self::now_on_board(train, now_min)?;
        let terminal_min = Self::running_window(train)?.1;
        for w in train.stops.windows(2) {
            let b = Self::segment_end((&w[0], &w[1]));
            if now < b {
                return Some((&w[1], last, b - now, terminal_min - now));
            }
        }
        Some((last, last, 0, terminal_min - now))
    }

    /// The trains as markers, placed by their own times: the segment now underway, walked to
    /// the time's fraction along the OSM rail slice, headed the way the line runs. The ETA
    /// rides `category` (the next stop) so the map can whisper it under the marker. Uses the
    /// plain per-call slice (tests, one-offs); the runtime keeps a [`RailRouter`] and calls
    /// [`Self::project_routed`].
    pub fn project(&self, atlas: &RailAtlas, now_min: u32) -> Vec<Vehicle> {
        let mut router = RailRouter::new(atlas);
        self.project_routed(atlas, &mut router, now_min)
    }

    /// [`Self::project`] over a cached router — the runtime's per-tick path.
    pub fn project_routed(&self, atlas: &RailAtlas, router: &mut RailRouter, now_min: u32) -> Vec<Vehicle> {
        let mut out = Vec::new();
        for train in self.trains.iter() {
            let Some(now) = Self::now_on_board(train, now_min) else { continue };
            let Some(next) = self.progress(train, now_min).map(|(next, _, _, _)| next) else { continue };
            // The segment now underway: the window whose far end (over the midnight wrap) is
            // past the clock.
            let Some(w) = train.stops.windows(2).find(|w| now < Self::segment_end((&w[0], &w[1]))) else {
                // At (or past) the final stop: the train parks at the terminal.
                let Some(last) = train.stops.last() else { continue };
                let Some(p) = atlas.station(&last.name) else { continue };
                out.push(Vehicle {
                    id: format!("sgr-{}", train.trips_no),
                    kind: VehicleKind::Train,
                    lon: p.lon,
                    lat: p.lat,
                    heading_deg: -1.0,
                    length_m: None,
                    name: Some(train.name.clone()),
                    category: Some(last.name.clone()),
                    icon_url: None,
                    alt_m: None,
                    scheduled: false,
                });
                continue;
            };
            let span = (Self::segment_end((&w[0], &w[1])) - w[0].time_min).max(1) as f64;
            let f = (now - w[0].time_min) as f64 / span;
            let Some(path) = router.slice(atlas, &w[0].name, &w[1].name) else { continue };
            let Some((p, bearing)) = point_at(&path, f) else { continue };
            out.push(Vehicle {
                id: format!("sgr-{}", train.trips_no),
                kind: VehicleKind::Train,
                lon: p[0],
                lat: p[1],
                heading_deg: bearing,
                length_m: None,
                name: Some(train.name.clone()),
                category: Some(next.name.clone()),
                icon_url: None,
                alt_m: None,
                scheduled: false,
            });
        }
        // The day's WHOLE data, not just what the clock has on the rails: a trip still
        // waiting for its departure draws dimmed at the origin station (the draw reads
        // `scheduled`), so the map carries the timetable the sweep published from frame one —
        // early morning, midday or after the last service, the data is the whole day's. A
        // trip that has finished its run drops off the map (it is history; the panel's day
        // list keeps the row).
        for train in self.trains.iter() {
            if Self::now_on_board(train, now_min).is_some() {
                continue; // already drawn above, riding its segment
            }
            let Some(first) = train.stops.first() else { continue };
            if now_min >= first.time_min {
                continue; // the run is over (a midnight-crossing trip off the board here reads below)
            }
            let Some(p) = atlas.station(&first.name) else { continue };
            out.push(Vehicle {
                id: format!("sgr-{}-scheduled", train.trips_no),
                kind: VehicleKind::Train,
                lon: p.lon,
                lat: p.lat,
                heading_deg: -1.0,
                length_m: None,
                name: Some(train.name.clone()),
                category: Some(format!("departs {} · {}", clock_label(first.time_min), first.name)),
                icon_url: None,
                alt_m: None,
                scheduled: true,
            });
        }
        out
    }
}

/// Fingerprint an SGR answer into the merge shape. The vendor's envelope is
/// `{code, ok, message, data}` — `code 200` with `data` rows that carry `title` is the
/// station list, rows with a `tripsNo` are trip searches, and `TRIP112` (a pair with no
/// service) is simply an empty day for that pair.
pub fn absorb_answer(bytes: &[u8]) -> Result<Absorb, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the answer is not JSON: {e}"))?;
    let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(-1);
    // A TrainSetBy answer (the seat-set ask): `data` is an OBJECT carrying
    // `trainSetRailwayCars` — before the array shapes, which expect `data` as a list.
    if code == 200 && v.pointer("/data/trainSetRailwayCars").is_some_and(|x| x.is_array()) {
        return seats::parse_train_set(bytes).map(Absorb::Seats);
    }
    let data = v.get("data").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    if code != 200 {
        // A pair with no service that day answers code 404 / "TRIP112 …" — the sweep's
        // honest empty. Any other refusal is an error the row should show.
        let message = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
        if message.contains("TRIP112") {
            return Ok(Absorb::Empty);
        }
        return Err(format!("the SGR feed answered: {message} (code {code})"));
    }
    let is_stations = data.first().is_some_and(|row| row.get("title").is_some() && row.get("tripsNo").is_none());
    if is_stations {
        let list = data
            .iter()
            .filter_map(|row| {
                let id = row.get("id").and_then(|x| x.as_i64())?;
                let title = row.get("title").and_then(|x| x.as_str())?.trim().to_string();
                (!title.is_empty()).then_some((id, title))
            })
            .collect();
        return Ok(Absorb::Stations(list));
    }
    let mut trips = Vec::new();
    for row in &data {
        let trips_no = row.get("tripsNo").and_then(as_text).unwrap_or_default();
        if trips_no.is_empty() {
            continue;
        }
        let (Some(start), Some(end)) =
            (row.get("startTime").and_then(|x| x.as_str()).and_then(parse_clock), row.get("endTime").and_then(|x| x.as_str()).and_then(parse_clock))
        else {
            continue;
        };
        let (Some(bid), Some(lid)) = (row.get("boardingStationId").and_then(|x| x.as_i64()), row.get("landingStationId").and_then(|x| x.as_i64()))
        else {
            continue;
        };
        let cars = row
            .get("railwayCars")
            .and_then(|x| x.as_array())
            .map(|cars| {
                cars.iter()
                    .filter_map(|c| {
                        let id = c.get("id").and_then(|x| x.as_i64())?;
                        let name = c.get("typeName").and_then(as_text).unwrap_or_default();
                        let empty = c.get("emptySeats").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                        let capacity = c.get("capacity").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                        let order = c.get("orderNo").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                        (!name.is_empty()).then_some(())?;
                        Some(SgrCoach { id, name, order, capacity, empty })
                    })
                    .collect()
            })
            .unwrap_or_default();
        trips.push(SweptTrip {
            trip_name: row.get("tripName").and_then(as_text).unwrap_or_else(|| "SGR".into()),
            trips_no,
            boarding_id: bid,
            boarding_name: row.get("boardingStationName").and_then(as_text).unwrap_or_default(),
            landing_id: lid,
            landing_name: row.get("landingStationName").and_then(as_text).unwrap_or_default(),
            start_min: start,
            end_min: end,
            cars,
            trip_set_id: row.get("tripTimeTableTrainSetId").and_then(|x| x.as_i64()),
        });
    }
    Ok(Absorb::Trips(trips))
}

/// A field that may arrive a number or a string.
fn as_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()).filter(|s| !s.trim().is_empty()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The SGR flow: public schedule data, swept one step per poll. No credential, no sign-in,
/// no live positions — the trains ride their own times (see [`crate::rail`]).
pub struct SgrAuth;

impl crate::auth::AuthFlow for SgrAuth {
    fn id(&self) -> &'static str {
        "sgr"
    }

    fn credential(&self) -> crate::auth::Credential {
        crate::auth::Credential::None
    }

    fn needs_credential(&self) -> bool {
        false
    }

    fn bootstrap(&self, _poll: u64) -> Option<crate::auth::AuthRequest> {
        None
    }

    fn poll(&self, _secret: &str, _view: crate::feed::ViewBox, _zoom: f64, poll_no: u64) -> Result<FleetRequest, String> {
        let date = today_eat(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
        let step = sweep_step(poll_no);
        let (boarding, landing) = sweep_pair(step);
        let req = match boarding {
            None => FleetRequest {
                key: format!("sgr/poll{poll_no}"),
                provider: Provider::Sgr,
                url: format!("{SGR_BASE}/Public/GetTripAssignedStations"),
                headers: sweep_headers(),
                body: None,
                cells: Vec::new(),
            },
            Some(b) => FleetRequest {
                key: format!("sgr/poll{poll_no}"),
                provider: Provider::Sgr,
                url: format!("{SGR_BASE}/Public/SearchTrip"),
                headers: sweep_headers(),
                body: Some((
                    format!(
                        r#"{{"date":"{date}","boardingStationId":{b},"landingStationId":{landing},"passengerCount":1,"isReservation":false,"isOneWay":true}}"#
                    ),
                    "application/json".into(),
                )),
                cells: Vec::new(),
            },
        };
        Ok(req)
    }
}

/// The sweep's own header block (the public reads ride it — schedule, seat sets alike).
pub fn sweep_headers() -> Vec<(String, String)> {
    vec![
        ("Accept".into(), "application/json".into()),
        ("Accept-Language".into(), "en-GB,en;q=0.9".into()),
        ("User-Agent".into(), "TerraZone/0.22.0 (SGR schedule sweep)".into()),
    ]
}

/// The seat-set ask: every third poll (gentle — the seat grid is heavy and changes slowly),
/// for the train the sweep is currently riding — the running one, else the next to depart —
/// when the vendor gave that trip its `tripTimeTableTrainSetId`. The answer fingerprints
/// into [`Absorb::Seats`] and the timetable keeps the set under the matching train.
pub fn seat_request(tt: &SgrTimetable, poll_no: u64) -> Option<FleetRequest> {
    if poll_no % 3 != 1 {
        return None;
    }
    let today = today_eat(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
    if tt.date != today {
        return None; // a stale schedule's seats are nobody's seats
    }
    let now = now_minutes_eat(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
    let starts_after_now = |t: &SgrTrain| t.stops.first().map(|s| s.time_min).unwrap_or(u32::MAX) >= now;
    let active = tt.running(now).into_iter().find(|t| t.trip_set_id.is_some()).or_else(|| {
        let mut next: Vec<&SgrTrain> = tt.trains.iter().filter(|t| starts_after_now(t)).collect();
        next.sort_by_key(|t| t.stops.first().map(|s| s.time_min).unwrap_or(u32::MAX));
        next.into_iter().find(|t| t.trip_set_id.is_some())
    })?;
    let trip_set_id = active.trip_set_id?;
    let (Some(first), Some(last)) = (active.stops.first(), active.stops.last()) else { return None };
    Some(seats::train_set_request(first.station_id, last.station_id, trip_set_id, poll_no))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::auth::AuthFlow;

    #[test]
    fn the_embedded_atlas_loads_and_carries_the_corridor() {
        let atlas = embedded_atlas();
        assert!(atlas.lines.len() > 1_000, "{} railway lines embedded", atlas.lines.len());
        assert!(atlas.stations.len() > 100, "{} stations embedded", atlas.stations.len());
        for name in [
            "Dar Es Salaam",
            "Pugu",
            "Soga",
            "Ruvu",
            "Ngerengere",
            "Morogoro",
            "Mkata",
            "Kilosa",
            "Kidete",
            "Gulwe",
            "Igandu",
            "Makutupora",
            "Dodoma",
        ] {
            assert!(atlas.station(name).is_some(), "{name} must be in the embedded atlas");
        }
    }

    /// A two-station atlas on a straight north-south line (Dar at the top, Dodoma 1° south).
    fn straight_atlas() -> RailAtlas {
        let line = RailLine::new(vec![[39.0, -6.0], [39.0, -7.0]]);
        RailAtlas {
            lines: vec![line],
            stations: vec![RailStop { name: "Dar Es Salaam".into(), lon: 39.0, lat: -6.0 }, RailStop { name: "Dodoma".into(), lon: 39.0, lat: -7.0 }],
        }
    }

    fn timetable() -> SgrTimetable {
        let mut t = SgrTimetable::default();
        t.absorb(
            "2026-10-17",
            &Absorb::Trips(vec![SweptTrip {
                trips_no: "0011904".into(),
                trip_name: "DAR -- DOM | EXPRESS".into(),
                boarding_id: 1,
                boarding_name: "Dar Es Salaam".into(),
                landing_id: 12,
                landing_name: "Dodoma".into(),
                start_min: 6 * 60,
                end_min: 9 * 60 + 50,
                cars: Vec::new(),
                trip_set_id: None,
            }]),
        );
        // The intermediate pass times the sweep's later steps merge in.
        t.absorb(
            "2026-10-17",
            &Absorb::Trips(vec![SweptTrip {
                trips_no: "0011904".into(),
                trip_name: "DAR -- DOM | EXPRESS".into(),
                boarding_id: 2,
                boarding_name: "Pugu".into(),
                landing_id: 12,
                landing_name: "Dodoma".into(),
                start_min: 6 * 60 + 16,
                end_min: 9 * 60 + 50,
                cars: Vec::new(),
                trip_set_id: None,
            }]),
        );
        t
    }

    #[test]
    fn clocks_and_dates_read_east_africa_time() {
        assert_eq!(parse_clock("06:00:00"), Some(360));
        assert_eq!(parse_clock("09:50:00"), Some(590));
        assert_eq!(parse_clock("bad"), None);
        // 2026-10-16 13:20 UTC is 16:20 in Dar (1792156800 = that instant).
        assert_eq!(today_eat(1_792_156_800), "2026-10-16");
        assert_eq!(now_minutes_eat(1_792_156_800), 16 * 60 + 20);
        // 2026-01-01 00:00 UTC is 03:00 in Dar.
        assert_eq!(today_eat(1_767_225_600), "2026-01-01");
        assert_eq!(now_minutes_eat(1_767_225_600), 3 * 60);
    }

    #[test]
    fn the_sweep_walks_stations_then_every_pair() {
        assert_eq!(sweep_step(1), 0, "poll 1 names the stations");
        assert_eq!(sweep_pair(sweep_step(1)), (None, 0));
        assert_eq!(sweep_pair(sweep_step(2)), (Some(1), 12), "poll 2 asks Dar es Salaam toward Dodoma");
        assert_eq!(sweep_pair(sweep_step(12)), (Some(11), 12));
        assert_eq!(sweep_pair(sweep_step(13)), (Some(2), 1), "then the down direction toward Dar es Salaam");
        assert_eq!(sweep_pair(sweep_step(23)), (Some(12), 1));
        assert_eq!(sweep_pair(sweep_step(24)), (None, 0), "and the sweep wraps");
    }

    #[test]
    fn the_sgr_poll_describes_the_vendors_own_calls() {
        let req = SgrAuth.poll("", (0.0, 0.0, 0.0, 0.0), 16.0, 1).unwrap();
        assert_eq!(req.provider, Provider::Sgr);
        assert!(req.url.ends_with("/Public/GetTripAssignedStations"), "{}", req.url);
        assert!(req.body.is_none());
        let req = SgrAuth.poll("", (0.0, 0.0, 0.0, 0.0), 16.0, 2).unwrap();
        assert!(req.url.ends_with("/Public/SearchTrip"), "{}", req.url);
        let (body, ct) = req.body.expect("the search rides a POST");
        assert_eq!(ct, "application/json");
        assert!(body.contains(r#""boardingStationId":1"#) && body.contains(r#""landingStationId":12"#), "{body}");
        assert!(body.contains(r#""date":"#), "the schedule day rides the body");
        assert!(SgrAuth.credential().vault_key().is_none(), "the schedule service is public");
        assert!(!SgrAuth.needs_credential());
    }

    #[test]
    fn answers_fingerprint_into_stations_trips_and_empty_days() {
        let stations = absorb_answer(
            br#"{"code":200,"ok":true,"message":"GNRL100 - Success.","data":[{"id":1,"title":"Dar Es Salaam"},{"id":12,"title":"Dodoma"}]}"#,
        )
        .unwrap();
        match stations {
            Absorb::Stations(list) => {
                assert_eq!(list, vec![(1, "Dar Es Salaam".into()), (12, "Dodoma".into())]);
            }
            other => panic!("{other:?}"),
        }
        let trips = absorb_answer(
            br#"{"code":200,"ok":true,"data":[{"tripId":153,"tripName":"DAR -- DOM | EXPRESS","tripsNo":"0011904","startTime":"06:00:00","endTime":"09:50:00","boardingStationId":1,"boardingStationName":"Dar Es Salaam","landingStationId":12,"landingStationName":"Dodoma","railwayCars":[{"id":417,"railwayCarId":19,"orderNo":1,"typeId":151,"typeName":"Royal Class","capacity":44,"emptySeats":0,"hasSeat":true,"bookedSeats":[]},{"id":418,"railwayCarId":13,"orderNo":2,"typeId":11,"typeName":"Business","capacity":45,"emptySeats":12,"hasSeat":true}]}]}"#,
        )
        .unwrap();
        match trips {
            Absorb::Trips(t) => {
                assert_eq!(t.len(), 1);
                assert_eq!(t[0].start_min, 360);
                assert_eq!(t[0].end_min, 590);
                assert_eq!(t[0].cars.len(), 2, "the trip row's own coaches ride along");
                assert_eq!(t[0].cars[0].name, "Royal Class");
                assert_eq!(t[0].cars[0].empty, 0, "a sold-out coach reads zero free");
                assert_eq!(t[0].cars[1].name, "Business");
                assert_eq!(t[0].cars[1].empty, 12);
                assert_eq!(t[0].cars[1].order, 2);
            }
            other => panic!("{other:?}"),
        }
        let empty = absorb_answer(br#"{"code":404,"ok":false,"message":"TRIP112 - Selected Journey is not found!","data":[]}"#).unwrap();
        assert_eq!(empty, Absorb::Empty, "a pair with no service keeps the sweep walking");
        assert!(absorb_answer(br#"{"code":500,"ok":false,"message":"BOOM","data":[]}"#).is_err(), "a real refusal is an error");
    }

    #[test]
    fn the_timetable_merges_every_sweeps_stop_times() {
        let t = timetable();
        assert_eq!(t.date, "2026-10-17");
        assert_eq!(t.trains.len(), 1, "one trip identity across the sweep");
        let train = &t.trains[0];
        assert_eq!(train.direction, 1, "toward Dodoma");
        assert_eq!(train.stops.len(), 3, "Dar es Salaam, Pugu and Dodoma merged by tripsNo");
        assert_eq!(train.stops[0].name, "Dar Es Salaam");
        assert_eq!(train.stops[0].time_min, 360);
        assert_eq!(train.stops[1].name, "Pugu");
        assert_eq!(train.stops[1].time_min, 376);
        assert_eq!(train.stops[2].name, "Dodoma");
        // A new day resets the tables.
        let mut t2 = timetable();
        t2.absorb("2026-10-18", &Absorb::Empty);
        assert_eq!(t2.date, "2026-10-18");
        assert!(t2.trains.is_empty(), "yesterday's trains do not run today");
    }

    /// A train that departs 23:25 and arrives 00:45 is on the board at 00:05: the clock rides
    /// over midnight with it, and the placement walks the leg in the train's own frame.
    #[test]
    fn the_overnight_train_stays_on_the_board_past_midnight() {
        let mut t = SgrTimetable::default();
        t.absorb(
            "2026-10-17",
            &Absorb::Trips(vec![SweptTrip {
                trips_no: "0099001".into(),
                trip_name: "DAR -- DOM | NIGHT".into(),
                boarding_id: 1,
                boarding_name: "Dar Es Salaam".into(),
                landing_id: 12,
                landing_name: "Dodoma".into(),
                start_min: 23 * 60 + 25,
                end_min: 45,
                cars: Vec::new(),
                trip_set_id: None,
            }]),
        );
        let train = &t.trains[0];
        // The landing stop keeps its wall time but reads as the next day, so the sort keeps
        // the departure first.
        assert_eq!((train.stops[0].name.as_str(), train.stops[0].time_min), ("Dar Es Salaam", 1405));
        assert_eq!((train.stops[1].name.as_str(), train.stops[1].time_min, train.stops[1].next_day), ("Dodoma", 45, true));
        // 00:05 is on board: the ETA to the terminal reads the 40 minutes the clock has left.
        assert_eq!(t.running(5).len(), 1, "00:05 is on board the 23:25 → 00:45");
        let (next, terminal, _to_next, to_terminal) = t.progress(train, 5).unwrap();
        assert_eq!(next.name, "Dodoma");
        assert_eq!(terminal.name, "Dodoma");
        assert_eq!(to_terminal, 40);
        // And off the board on either side: before departure and after arrival.
        assert!(t.running(1400).is_empty(), "not yet departed at 23:20");
        assert!(t.running(100).is_empty(), "arrived 00:45, gone by 01:40");
    }

    #[test]
    fn the_timetable_carries_the_coaches_the_vendor_reports() {
        let mut t = SgrTimetable::default();
        t.absorb(
            "2026-10-17",
            &Absorb::Trips(vec![SweptTrip {
                trips_no: "0011904".into(),
                trip_name: "DAR -- DOM | EXPRESS".into(),
                boarding_id: 1,
                boarding_name: "Dar Es Salaam".into(),
                landing_id: 12,
                landing_name: "Dodoma".into(),
                start_min: 6 * 60,
                end_min: 9 * 60 + 50,
                cars: vec![SgrCoach { id: 417, name: "Royal Class".into(), order: 1, capacity: 44, empty: 0 }],
                trip_set_id: None,
            }]),
        );
        assert_eq!(t.trains[0].cars.len(), 1);
        assert_eq!(t.trains[0].cars[0].empty, 0, "the sold-out class reads No seat available");
        // A later answer's refresh wins (availability moves through the day)…
        t.absorb(
            "2026-10-17",
            &Absorb::Trips(vec![SweptTrip {
                trips_no: "0011904".into(),
                trip_name: "DAR -- DOM | EXPRESS".into(),
                boarding_id: 2,
                boarding_name: "Pugu".into(),
                landing_id: 12,
                landing_name: "Dodoma".into(),
                start_min: 6 * 60 + 16,
                end_min: 9 * 60 + 50,
                cars: vec![
                    SgrCoach { id: 417, name: "Royal Class".into(), order: 1, capacity: 44, empty: 3 },
                    SgrCoach { id: 418, name: "Business".into(), order: 2, capacity: 45, empty: 12 },
                ],
                trip_set_id: None,
            }]),
        );
        assert_eq!(t.trains[0].cars.len(), 2);
        assert_eq!(t.trains[0].cars[0].empty, 3);
        // …and the coaches ride the document save (serde camelCase roundtrip).
        let json = serde_json::to_string(&t.trains[0]).unwrap();
        assert!(json.contains("\"emptySeats\":3"), "the wire keeps the vendor's own field name: {json}");
        let back: SgrTrain = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t.trains[0]);
        // A timetable saved before the coaches existed (no cars field) still loads.
        let old: SgrTrain = serde_json::from_str(r#"{"tripsNo":"1","name":"X","direction":1,"stops":[]}"#).unwrap();
        assert!(old.cars.is_empty());
    }

    /// A two-leg train (Dar 06:00 → Kilosa 06:30 → Dodoma 09:50) with Kilosa on the atlas.
    fn kilosa_train() -> (SgrTimetable, RailAtlas) {
        let mut atlas = straight_atlas();
        atlas.stations.push(RailStop { name: "Kilosa".into(), lon: 39.0, lat: -6.25 });
        let mut t = SgrTimetable::default();
        for (bid, bname, start) in [(1, "Dar Es Salaam", 6 * 60), (9, "Kilosa", 6 * 60 + 30)] {
            t.absorb(
                "2026-10-17",
                &Absorb::Trips(vec![SweptTrip {
                    trips_no: "0011904".into(),
                    trip_name: "DAR -- DOM | EXPRESS".into(),
                    boarding_id: bid,
                    boarding_name: bname.into(),
                    landing_id: 12,
                    landing_name: "Dodoma".into(),
                    start_min: start,
                    end_min: 9 * 60 + 50,
                    cars: Vec::new(),
                    trip_set_id: None,
                }]),
            );
        }
        (t, atlas)
    }

    #[test]
    fn trains_project_along_the_atlas_by_their_own_times() {
        let (t, atlas) = kilosa_train();
        assert_eq!(t.trains[0].stops.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["Dar Es Salaam", "Kilosa", "Dodoma"]);

        // Halfway through the first leg: 06:15 → lat −6.125, headed south (bearing 180).
        let markers = t.project(&atlas, 6 * 60 + 15);
        assert_eq!(markers.len(), 1);
        let m = &markers[0];
        assert_eq!(m.kind, VehicleKind::Train);
        assert!((m.lat - (-6.125)).abs() < 0.01, "halfway down the leg: {}", m.lat);
        assert!((m.lon - 39.0).abs() < 1e-6, "the straight line keeps the longitude: {}", m.lon);
        assert!((m.heading_deg - 180.0).abs() < 1.0, "headed south: {}", m.heading_deg);
        assert_eq!(m.category.as_deref(), Some("Kilosa"), "the next stop rides the marker");

        // Before the first departure the trip stands at the origin as a scheduled event —
        // the day's whole data stays on the map (dimmed by the draw), not just what runs now.
        let early = t.project(&atlas, 5 * 60);
        assert_eq!(early.len(), 1, "the not-yet-departed trip waits at its origin");
        assert!(early[0].scheduled, "it reads as a scheduled event");
        assert_eq!(early[0].id, "sgr-0011904-scheduled", "a distinct id from the running marker");
        assert!((early[0].lat - (-6.0)).abs() < 1e-6, "parked at Dar Es Salaam");
        assert_eq!(early[0].category.as_deref(), Some("departs 06:00 · Dar Es Salaam"));
        // After the arrival the run is history — off the map (the panel's day list keeps it).
        assert!(t.project(&atlas, 10 * 60).is_empty());

        // At the terminal minute the train parks at the last station.
        let end = t.project(&atlas, 9 * 60 + 50);
        assert_eq!(end.len(), 1);
        assert!(!end[0].scheduled, "arrived reads live (parked at the terminal)");
        assert!((end[0].lat - (-7.0)).abs() < 1e-6, "parked at Dodoma");
    }

    #[test]
    fn progress_reads_the_eta_table() {
        let (t, atlas) = kilosa_train();
        let _ = atlas;
        let train = &t.trains[0];
        // 06:15: next Kilosa in 15, terminal Dodoma in 09:50 − 06:15.
        let (next, terminal, to_next, to_end) = t.progress(train, 6 * 60 + 15).unwrap();
        assert_eq!(next.name, "Kilosa");
        assert_eq!(to_next, 15);
        assert_eq!(terminal.name, "Dodoma");
        assert_eq!(to_end, 590 - 375);
        assert!(t.progress(train, 5 * 60).is_none(), "not departed yet");
    }

    #[test]
    fn the_atlas_slices_between_named_stations() {
        let atlas = straight_atlas();
        let path = atlas.slice("Dar Es Salaam", "Dodoma").expect("one line serves both");
        assert!(path.len() >= 2);
        let (p, bearing) = point_at(&path, 0.5).unwrap();
        assert!((p[1] - (-6.5)).abs() < 1e-9, "the midpoint of the straight line");
        assert!((bearing - 180.0).abs() < 1.0);
        // A stop the atlas does not know slices to nothing.
        assert!(atlas.slice("Dar Es Salaam", "Mwanza").is_none());
    }

    #[test]
    fn atlas_names_match_case_and_station_suffixes() {
        let atlas = straight_atlas();
        assert_eq!(atlas.station("dar es salaam").map(|s| s.lat), Some(-6.0));
        assert_eq!(atlas.station("Dodoma Station").map(|s| s.lat), Some(-7.0), "the suffix drops");
    }

    #[test]
    fn the_atlas_parses_the_asset_shape() {
        let atlas = RailAtlas::parse(br#"{"lines":[[39.0,-6.0,39.0,-7.0]],"stations":[["Dar Es Salaam",39.0,-6.0],["Dodoma",39.0,-7.0]]}"#).unwrap();
        assert_eq!(atlas.lines.len(), 1);
        assert_eq!(atlas.stations.len(), 2);
        assert!(RailAtlas::parse(br#"{"lines":[],"stations":[]}"#).is_err(), "an empty atlas is an error, not a blank map");
    }

    #[test]
    fn schedule_roundtrips_through_json() {
        let t = timetable();
        let json = serde_json::to_string(&t).unwrap();
        let back: SgrTimetable = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}

#[cfg(test)]
mod router_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Two ways chained at a shared node (39.0,-6.5) — Dar at the north way's start,
    /// Dodoma at the south way's end.
    fn chained_atlas() -> RailAtlas {
        let north = RailLine::new(vec![[39.0, -6.0], [39.0, -6.5]]);
        let south = RailLine::new(vec![[39.0, -6.5], [39.0, -7.0]]);
        RailAtlas {
            lines: vec![north, south],
            stations: vec![RailStop { name: "Dar Es Salaam".into(), lon: 39.0, lat: -6.0 }, RailStop { name: "Dodoma".into(), lon: 39.0, lat: -7.0 }],
        }
    }

    #[test]
    fn the_router_chains_connected_ways_into_one_path() {
        let atlas = chained_atlas();
        let mut router = RailRouter::new(&atlas);
        let path = router.slice(&atlas, "Dar Es Salaam", "Dodoma").expect("the chain connects");
        assert!(path.len() >= 3, "both ways ride the path: {path:?}");
        let (p, bearing) = point_at(&path, 0.5).unwrap();
        assert!((p[1] - (-6.5)).abs() < 1e-9, "the midpoint is the shared node: {p:?}");
        assert!((bearing - 180.0).abs() < 1.0, "headed south: {bearing}");
        // Cached: the second ask is the same path.
        assert_eq!(router.slice(&atlas, "Dar Es Salaam", "Dodoma"), Some(path));
    }
}
