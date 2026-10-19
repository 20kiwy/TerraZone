//! The fleet runtime: the poll loop, the marker cache and the 2D marker drawing. The document's
//! `fleet` state is the layer-engine truth (which feeds are signed in, their eyes, the demo —
//! with the "Fleet" group and its locked feed sub-layers in the tree); this module is the live
//! side of it, the way [`crate::tiles::TileCache`] is the live side of the tile sources: answers
//! land here, are parsed here, and the 2D map and the 3D environment both draw from this one
//! cache, gated by that one document state.
//!
//! The system-managed radars (Marine, Flight) live here too: while their session is pending the
//! runtime sends the vendor's bootstrap page a plain GET, and the `Set-Cookie` that comes back
//! lands in the document through `fleet.systemAuth` — no UI, no cookie the user ever types.
//! Bolt's token, by contrast, only ever comes from the user. Bolt's **vehicle artwork** is the
//! system's own work too: the poll names each class's CDN pin, the runtime downloads it once,
//! keeps the PNG in the settings vault and draws the vendor's own artwork over the vector
//! glyph. The SGR sweep's answers land in the document as a timetable the trains ride.

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Color32, Pos2, Rect, Shape, Stroke, pos2};
use h3o::CellIndex;
use terrazon_doc::{Document, NodeKind};
use terrazon_fleet as fleetlib;
use terrazon_fleet::{FeedStatus, Provider, Vehicle};

use crate::TerrazoneApp;

/// The embedded OSM rail atlas (Tanzania's railways and stations — see the fleet crate's
/// `assets/tz-rail.README.md`): parsed once per process, shared by the runtime and the draw.
/// The asset lives in the fleet crate and `cargo xtask rail` checks it, so the tooling and
/// the app read one file.
fn rail_atlas() -> &'static fleetlib::RailAtlas {
    fleetlib::rail::embedded_atlas()
}

/// The live fleet: answers in flight, the markers the feeds last answered with, and the demo.
pub struct FleetCache {
    pub rx: Receiver<fleetlib::FleetMsg>,
    tx: Sender<fleetlib::FleetMsg>,
    /// Requests in flight: answer key → the feed it belongs to.
    pending: HashMap<String, Provider>,
    /// The markers each feed last answered with (kept through an error — a lost poll must not
    /// blink the fleet away).
    pub markers: HashMap<Provider, Vec<Vehicle>>,
    /// The H3 cells of each feed's fetched items — the grid outlined under the icons.
    pub grid: HashMap<Provider, Vec<CellIndex>>,
    /// Each feed's live status (the doc holds the config; the answers land here).
    pub status: HashMap<Provider, FeedStatus>,
    /// The demo feed's markers, regenerated per poll tick around the view.
    pub demo_markers: Vec<Vehicle>,
    /// The demo grid: each demo marker's cell with the kind that named it (the cell takes the
    /// kind's type colour).
    pub demo_grid: Vec<(CellIndex, fleetlib::VehicleKind)>,
    /// The demo's clock: one step per poll, so the sample traffic drifts.
    demo_tick: u64,
    last_poll: Option<Instant>,
    poll_no: u64,
    /// The rail atlas the SGR trains ride (the embedded OSM network).
    pub atlas: fleetlib::RailAtlas,
    /// The router: the chained way paths between station pairs, computed once, cut per tick.
    pub rail_router: fleetlib::rail::RailRouter,
    /// Bolt's downloaded vehicle artwork, decode-ready: url → the decoded RGBA (the texture
    /// itself is made lazily in [`draw`], where the egui context lives).
    pub icon_images: HashMap<String, egui::ColorImage>,
    /// The textures made from [`Self::icon_images`] (one per url, cached by the painter).
    pub icons: HashMap<String, egui::TextureHandle>,
    icons_pending: BTreeSet<String>,
    icons_failed: BTreeSet<String>,
    /// The vault's artwork rows have been decoded into [`Self::icon_images`] once.
    icons_loaded: bool,
    /// The view box the demo last sampled (quantized centre + res): a pan into fresh cells
    /// regenerates the sample at once, the way the tiles do — the poll cadence only governs
    /// the drift.
    demo_view: Option<[i64; 3]>,
}

impl Default for FleetCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FleetCache {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            rx,
            tx,
            pending: HashMap::new(),
            markers: HashMap::new(),
            grid: HashMap::new(),
            status: HashMap::new(),
            demo_markers: Vec::new(),
            demo_grid: Vec::new(),
            demo_tick: 0,
            last_poll: None,
            poll_no: 0,
            atlas: rail_atlas().clone(),
            rail_router: fleetlib::rail::RailRouter::new(rail_atlas()),
            icon_images: HashMap::new(),
            icons: HashMap::new(),
            icons_pending: BTreeSet::new(),
            icons_failed: BTreeSet::new(),
            icons_loaded: false,
            demo_view: None,
        }
    }

    pub fn status_of(&self, provider: Provider) -> FeedStatus {
        self.status.get(&provider).cloned().unwrap_or(FeedStatus::Off)
    }

    pub fn markers_of(&self, provider: Provider) -> &[Vehicle] {
        self.markers.get(&provider).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn grid_of(&self, provider: Provider) -> &[CellIndex] {
        self.grid.get(&provider).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Is the next poll due (never polled yet, or the cadence has elapsed)?
    fn poll_due(&self, cadence: Duration) -> bool {
        self.last_poll.is_none_or(|t| Instant::now().saturating_duration_since(t) >= cadence)
    }
}

/// The view a poll covers: the lon/lat box and the map zoom level.
pub type FleetView = ((f64, f64, f64, f64), f64);

/// What one answer drain hands back: the rotated system sessions, the SGR sweep's absorbed
/// answers, and the artwork downloads ready for the vault.
type Drain = (Vec<(Provider, String)>, Vec<fleetlib::rail::Absorb>, Vec<(String, Vec<u8>)>);

/// What one sign-in step's answer asked for (applied after the answer drain — the loop owns
/// the cache borrow, the vault and the engine want the whole app).
enum OtpOutcome {
    /// The code went out; the editor moves to the 4-digit step.
    CodeSent,
    /// The step refused (bad phone, rejected code, unreachable vendor).
    Failed(String),
    /// The code checked out; the session is minted.
    SignedIn(fleetlib::BoltSession),
}

/// The vault's Bolt session, if it parses (a corrupt row is as good as none).
fn vault_session(app: &TerrazoneApp) -> Option<&str> {
    let secret = app.ui.fleet_vault.get("bolt", "session")?;
    fleetlib::BoltSession::parse(secret).map(|_| secret)
}

/// The device identity a sign-in rides: a stable per-installation uuid from the vault (made
/// once), placed at the map view so Bolt's market query matches where the user is looking.
pub fn bolt_device(app: &mut TerrazoneApp) -> fleetlib::Device {
    let uuid = match app.ui.fleet_vault.get("bolt", "device").map(String::from) {
        Some(u) => u,
        None => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
            let addr = &nanos as *const u64 as u64;
            let mut h = nanos ^ addr.rotate_left(17);
            h ^= h >> 33;
            h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
            h ^= h >> 33;
            let u = format!("tz-{h:016x}");
            app.ui.fleet_vault.put("bolt", "device", u.clone());
            u
        }
    };
    // Where the map is looking names the market (and the phone's country code when the
    // number was typed without one). No map view: the home view — Dar es Salaam.
    let (lat, lng) = app
        .view()
        .map(|v| {
            let (lng, lat) = terrazon_map::doc_to_lonlat(v.center.x, v.center.y);
            (lat, lng)
        })
        .unwrap_or((-6.79, 39.21));
    let country = fleetlib::phone_country(lat, lng).to_string();
    let timezone = fleetlib::timezone_for_country(&country).to_string();
    fleetlib::Device { uuid, lat, lng, country, timezone }
}

/// Step one: send the 4-digit code to the phone in the draft. The request is the flow's; the
/// answer comes back through the cache channel and lands in [`tick`].
pub fn otp_send_code(app: &mut TerrazoneApp) {
    let Some(fetcher) = app.services.fleet.clone() else {
        app.ui.fleet_draft.error = Some("no network in this build — the demo feed shows the layer instead".into());
        return;
    };
    let phone = app.ui.fleet_draft.phone.trim().to_string();
    let device = bolt_device(app);
    let poll_no = app.canvas.fleet.poll_no.wrapping_add(1);
    app.canvas.fleet.poll_no = poll_no;
    let flow = fleetlib::flow_for(Provider::Bolt);
    match flow.otp_start(&phone, &device, poll_no) {
        Ok(req) => {
            // Bolt ties the pending session to the device: the same uuid must ride the confirm.
            let pending = fleetlib::PendingSession {
                session_id: device.uuid.clone(),
                device_id: device.uuid.clone(),
                phone: phone.clone(),
                channel: "sms".into(),
                created_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0),
            };
            app.ui.fleet_draft.pending = Some(pending);
            match req.method {
                fleetlib::AuthMethod::Post { body, content_type } => {
                    fetcher.post(req.key, req.url, req.headers, body, content_type, app.canvas.fleet.tx.clone());
                }
                fleetlib::AuthMethod::Get => {
                    fetcher.get(req.key, req.url, req.headers, app.canvas.fleet.tx.clone());
                }
            }
            app.ui.fleet_draft.error = None;
        }
        Err(e) => {
            app.ui.fleet_draft.error = Some(e);
        }
    }
}

/// Step two: verify the 4-digit code. A minted session lands in the vault (the settings keep
/// it) and in the document through `fleet.setAuth`.
pub fn otp_verify(app: &mut TerrazoneApp) {
    let Some(fetcher) = app.services.fleet.clone() else {
        app.ui.fleet_draft.error = Some("no network in this build".into());
        return;
    };
    let Some(pending) = app.ui.fleet_draft.pending.clone() else {
        app.ui.fleet_draft.error = Some("ask for a code first".into());
        return;
    };
    let device = bolt_device(app);
    let code = app.ui.fleet_draft.code.trim().to_string();
    let poll_no = app.canvas.fleet.poll_no.wrapping_add(1);
    app.canvas.fleet.poll_no = poll_no;
    let flow = fleetlib::flow_for(Provider::Bolt);
    match flow.otp_confirm(&pending, &code, &device, poll_no) {
        Ok(req) => match req.method {
            fleetlib::AuthMethod::Post { body, content_type } => {
                fetcher.post(req.key, req.url, req.headers, body, content_type, app.canvas.fleet.tx.clone());
            }
            fleetlib::AuthMethod::Get => {
                fetcher.get(req.key, req.url, req.headers, app.canvas.fleet.tx.clone());
            }
        },
        Err(e) => {
            app.ui.fleet_draft.error = Some(e);
        }
    }
}

/// Back to step one (a re-send, a wrong number — the draft clears its code).
pub fn otp_reset(app: &mut TerrazoneApp) {
    let draft = &mut app.ui.fleet_draft;
    draft.stage = crate::state::OtpStage::Phone;
    draft.code = String::new();
    draft.pending = None;
    draft.error = None;
}

/// Sign out: the document's row clears and the vault forgets the session (the settings do
/// not keep a credential the user signed out of).
pub fn sign_out_bolt(app: &mut TerrazoneApp) {
    let _ = app.run("fleet.clearAuth", serde_json::json!({ "provider": "Bolt" }));
    app.ui.fleet_vault.remove("bolt", "session");
    app.ui.fleet_draft = crate::state::FleetDraft::default();
}

/// The signed-in phone, for the row's label (the vault's session knows it).
pub fn bolt_signed_phone(app: &TerrazoneApp) -> Option<String> {
    let secret = vault_session(app)?;
    fleetlib::BoltSession::parse(secret).map(|s| s.phone)
}

/// The per-frame step: drain answers, poll when due, refresh the demo. `view` is the map view
/// the markers cover (the document's own view, wherever it is drawn).
pub fn tick(app: &mut TerrazoneApp, view: Option<FleetView>) {
    // Frame-one housekeeping: a document the engine just created or opened picks its Fleet group
    // up here (background sync — no undo step), the same way the Map layer is always there. And
    // a session the user kept in the settings vault re-attaches itself: Bolt's sign-in outlives
    // documents, so a fresh one is live again without anyone typing anything.
    if app.session.active().is_some_and(|st| !st.doc.layers.iter().any(|n| n.name.as_deref() == Some("Fleet"))) {
        let _ = app.run("fleet.syncTree", serde_json::json!({}));
    }
    // The deployment's own trust: a username + device pair in the environment (or a ready
    // Basic header) signs the Bolt feed in automatically — the Basic code is generated, the
    // vault keeps the pair, and the fleet is live before anyone types anything. A
    // trust-minted session re-mints on every launch (the vendor ties a poll's
    // `rh_session_id` to the sign-in and refuses a reused one); a session the vendor itself
    // minted (a real phone sign-in kept in the vault) outranks the trust and stays.
    if !app.ui.fleet_trust_tried
        && let Some(trust) = fleetlib::BoltTrust::from_env()
    {
        app.ui.fleet_trust_tried = true;
        let stored_trust = vault_session(app).and_then(fleetlib::BoltSession::parse).is_some_and(|s| s.session_id.starts_with("trust-"));
        if vault_session(app).is_none() || stored_trust {
            app.ui.fleet_vault.put("bolt", "session", trust.session_secret());
            app.ui.fleet_vault.put("bolt", "device", trust.device_secret());
            app.status(crate::i18n::fmt(tl!("Bolt signed in from the deployment's own trust ({uid})"), &[("uid", &trust.auth_username)]));
        }
    }
    if app.session.active().is_some_and(|st| st.doc.fleet.layer(Provider::Bolt).is_none_or(|l| !l.auth.is_set()))
        && vault_session(app).is_some()
        && let Some(secret) = vault_session(app).map(String::from)
    {
        let _ = app.run("fleet.setAuth", serde_json::json!({ "provider": "Bolt", "token": secret }));
    }
    // Bolt's artwork kept in the settings vault decodes once per session (the downloads
    // outlive the process — the pins are on the map before the first poll answers).
    if !app.canvas.fleet.icons_loaded {
        app.canvas.fleet.icons_loaded = true;
        for (url, b64) in app.ui.fleet_vault.prefixed("bolt", "icon:") {
            use base64::Engine as _;
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
                && let Some(img) = decode_png(&bytes)
            {
                app.canvas.fleet.icon_images.insert(url, img);
            }
        }
    }
    let Some(doc) = app.session.active().map(|st| st.doc.clone()) else { return };
    let grid_zoom = view.map_or(8.0, |(_, z)| z);

    // The SGR trains ride their own times: the timetable the sweep built is projected along
    // the rail atlas every tick (the markers move as the clock does — that is the whole point
    // of a schedule feed). No grid: trains ride a line, not hex cells.
    if let Some(l) = doc.fleet.layer(Provider::Sgr)
        && l.visible
        && let Some(tt) = &l.schedule
    {
        let now = fleetlib::rail::now_minutes_eat(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
        );
        let markers = {
            let cache = &mut app.canvas.fleet;
            tt.project_routed(&cache.atlas, &mut cache.rail_router, now)
        };
        #[cfg(test)]
        eprintln!("DEBUG tick projected {} markers (now {now}, trains {})", markers.len(), tt.trains.len());
        app.canvas.fleet.grid.insert(Provider::Sgr, Vec::new());
        app.canvas.fleet.markers.insert(Provider::Sgr, markers);
        // The schedule is live: the sweep's data answers set this too, but a timetable that
        // came with the document (a saved map file) reads ready on its own.
        app.canvas.fleet.status.insert(Provider::Sgr, FeedStatus::Ready);
    }

    // 1. Answers: parse and store. A data failure marks the feed Error but keeps its markers; a
    //    bootstrap failure just keeps *Connecting* — the system retries with the poll cadence.
    //    The sign-in answers (Bolt's otp-start / otp-confirm) land in the vault + document; the
    //    SGR sweep's answers merge into the document's timetable; Bolt's artwork PNGs decode
    //    into the icon cache (and persist to the settings vault).
    let mut otp_outcomes: Vec<OtpOutcome> = Vec::new();
    let (sessions, sgr_answers, icon_saved): Drain = {
        let cache = &mut app.canvas.fleet;
        let mut sessions: Vec<(Provider, String)> = Vec::new();
        let mut sgr_answers: Vec<fleetlib::rail::Absorb> = Vec::new();
        let mut icon_saved: Vec<(String, Vec<u8>)> = Vec::new();
        while let Ok(msg) = cache.rx.try_recv() {
            match msg {
                fleetlib::FleetMsg::Done { key, bytes, set_cookie } => {
                    // The sign-in steps answer to the draft, not to a feed.
                    if key.starts_with("bolt/otp-start") {
                        if let Some(e) = fleetlib::bolt_start_answer(&bytes).err() {
                            otp_outcomes.push(OtpOutcome::Failed(e));
                        } else {
                            otp_outcomes.push(OtpOutcome::CodeSent);
                        }
                        continue;
                    }
                    if key.starts_with("bolt/otp-confirm") {
                        let Some(pending) = app.ui.fleet_draft.pending.clone() else { continue };
                        match fleetlib::bolt_confirm_answer(&pending, &bytes) {
                            Ok(session) => otp_outcomes.push(OtpOutcome::SignedIn(session)),
                            Err(e) => otp_outcomes.push(OtpOutcome::Failed(e)),
                        }
                        continue;
                    }
                    // Bolt's vehicle artwork: the PNG decodes into the cache, the bytes ride
                    // to the vault (the settings keep it for next launch).
                    if let Some(url) = key.strip_prefix("icon/") {
                        let url = url.to_string();
                        cache.icons_pending.remove(&url);
                        match decode_png(&bytes) {
                            Some(img) => {
                                icon_saved.push((url.clone(), bytes));
                                cache.icon_images.insert(url, img);
                            }
                            None => {
                                cache.icons_failed.insert(url);
                            }
                        }
                        continue;
                    }
                    let bootstrap = key.contains("-bootstrap");
                    let Some(provider) = cache.pending.remove(&key) else { continue };
                    if bootstrap {
                        // The handshake answered: the system owns the session from here.
                        if let Some(c) = set_cookie.filter(|c| !c.trim().is_empty()) {
                            sessions.push((provider, c));
                        }
                        continue;
                    }
                    // The SGR sweep's answers are schedules, not vehicles: they fingerprint
                    // into the document's timetable.
                    if provider == Provider::Sgr {
                        match fleetlib::rail::absorb_answer(&bytes) {
                            Ok(answer) => sgr_answers.push(answer),
                            Err(e) => {
                                cache.status.insert(provider, FeedStatus::Error(e));
                            }
                        }
                        continue;
                    }
                    // A data answer can carry a fresh cookie too (vendors rotate sessions).
                    if provider.bootstrap_url().is_some()
                        && let Some(c) = set_cookie.filter(|c| !c.trim().is_empty())
                    {
                        sessions.push((provider, c));
                    }
                    match fleetlib::feed::parse(provider, &bytes) {
                        Ok(vehicles) => {
                            // The flight vendor answers anonymous callers with its global
                            // capped cloud (the bbox rides the query, the server ignores
                            // it) — the layer keeps the view's share: only the aircraft
                            // inside the box this ask covered (a little over, so a pan
                            // doesn't blink the edges).
                            let flight_box = view.map(|((w, s, e, n), _)| (w - 0.15, s - 0.15, e + 0.15, n + 0.15));
                            let vehicles = match (provider, flight_box) {
                                (Provider::Flight, Some((w, s, e, n))) => {
                                    vehicles.into_iter().filter(|v| v.lon >= w && v.lon <= e && v.lat >= s && v.lat <= n).collect()
                                }
                                _ => vehicles,
                            };
                            cache.grid.insert(provider, fleetlib::feed::grid_cells(&vehicles, fleetlib::h3grid::res_for_zoom(grid_zoom)));
                            cache.markers.insert(provider, vehicles);
                            cache.status.insert(provider, FeedStatus::Ready);
                        }
                        Err(e) => {
                            cache.status.insert(provider, FeedStatus::Error(e));
                        }
                    }
                }
                fleetlib::FleetMsg::Failed { key, reason } => {
                    if key.starts_with("bolt/otp-") {
                        otp_outcomes.push(OtpOutcome::Failed(reason));
                        continue;
                    }
                    if let Some(url) = key.strip_prefix("icon/") {
                        cache.icons_pending.remove(url);
                        cache.icons_failed.insert(url.to_string());
                        continue;
                    }
                    let bootstrap = key.contains("-bootstrap");
                    let Some(provider) = cache.pending.remove(&key) else { continue };
                    if !bootstrap {
                        cache.status.insert(provider, FeedStatus::Error(reason));
                    }
                }
            }
        }
        // The sweep's answers and the saved artwork go back to the caller (they need the
        // whole app; the cache borrow ends here).
        (sessions, sgr_answers, icon_saved)
    };
    // The sweep's answers merge into the document's timetable (one background step per
    // answer — the sweep is slow, the user never performs it).
    if !sgr_answers.is_empty() {
        let date =
            fleetlib::rail::today_eat(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0));
        let mut timetable =
            app.session.active().and_then(|st| st.doc.fleet.layer(Provider::Sgr)).and_then(|l| l.schedule.clone()).unwrap_or_default();
        for answer in &sgr_answers {
            timetable.absorb(&date, answer);
        }
        if let Ok(body) = serde_json::to_value(&timetable) {
            let _ = app.run("fleet.sgrSchedule", body);
        }
        app.canvas.fleet.status.insert(Provider::Sgr, FeedStatus::Ready);
    }
    // The downloaded artwork persists (base64 PNG rows in the settings vault).
    for (url, bytes) in icon_saved {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        app.ui.fleet_vault.put("bolt", &format!("icon:{url}"), b64);
    }
    // The system's sessions land in the document through the engine command (one journal step
    // per session — the handshake is rare, the user never performs it).
    for (provider, cookie) in &sessions {
        let _ = app.run("fleet.systemAuth", serde_json::json!({ "provider": provider.label(), "cookie": cookie }));
    }
    // The sign-in outcomes land where the user is looking: the draft walks its two steps, a
    // minted session goes to the vault (the settings keep it) and into the document.
    for outcome in otp_outcomes {
        match outcome {
            OtpOutcome::CodeSent => {
                let draft = &mut app.ui.fleet_draft;
                draft.stage = crate::state::OtpStage::Code;
                draft.error = None;
                app.status(tl!("The code is on its way — type the 4 digits Bolt sent"));
            }
            OtpOutcome::Failed(e) => {
                let draft = &mut app.ui.fleet_draft;
                draft.error = Some(e);
                draft.stage = crate::state::OtpStage::Phone;
                draft.pending = None;
            }
            OtpOutcome::SignedIn(session) => {
                let secret = session.to_secret();
                let phone = session.phone.clone();
                app.ui.fleet_vault.put("bolt", "session", secret.clone());
                let draft = &mut app.ui.fleet_draft;
                draft.phone = phone;
                draft.code = String::new();
                draft.pending = None;
                draft.stage = crate::state::OtpStage::Phone;
                draft.error = None;
                let _ = app.run("fleet.setAuth", serde_json::json!({ "provider": "Bolt", "token": secret }));
                app.status(tl!("Bolt verified — the session is saved in the settings"));
            }
        }
    }

    // 2. Poll when due (the `fleet.poll` command sets `poll_due` to ask for one now). The demo
    //    needs no fetcher — it regenerates offline, and it follows the view the way the tiles
    //    do: a pan into fresh cells resamples at once, the poll cadence only drifts it.
    let cadence = Duration::from_secs(u64::from(doc.fleet.poll_secs.max(5)));
    let due = doc.fleet.poll_due || app.canvas.fleet.poll_due(cadence);
    let wants = doc.fleet.layers.iter().any(|l| l.wants_fetch() || l.needs_bootstrap()) || doc.fleet.demo;
    let Some(((west, south, east, north), zoom)) = view else { return };
    let res = fleetlib::h3grid::res_for_zoom(zoom);
    let span = fleetlib::h3grid::cell_span_deg(res).max(1e-4);
    let q = |v: f64| (v / span).round() as i64;
    let demo_key = [q((west + east) * 0.5), q((south + north) * 0.5), i64::from(u8::from(res))];
    let demo_moved = doc.fleet.demo && app.canvas.fleet.demo_view != Some(demo_key);
    if doc.fleet.demo && (demo_moved || (due && wants)) {
        let markers = fleetlib::feed::demo_markers((west, south, east, north), zoom, app.canvas.fleet.demo_tick);
        let mut grid: Vec<(CellIndex, fleetlib::VehicleKind)> = Vec::new();
        for v in &markers {
            if let Ok(ll) = h3o::LatLng::new(v.lat, v.lon) {
                let cell = ll.to_cell(res);
                if !grid.iter().any(|(c, _)| *c == cell) {
                    grid.push((cell, v.kind));
                }
            }
        }
        let cache = &mut app.canvas.fleet;
        cache.demo_view = Some(demo_key);
        cache.demo_grid = grid;
        cache.demo_markers = markers;
        cache.demo_tick = cache.demo_tick.wrapping_add(1);
    }
    if !due || !wants {
        return;
    }
    let cache = &mut app.canvas.fleet;
    cache.last_poll = Some(Instant::now());
    cache.poll_no = cache.poll_no.wrapping_add(1);
    if let Some(fetcher) = app.services.fleet.clone() {
        let (requests, skipped) = fleetlib::feed::build_requests(&doc.fleet, (west, south, east, north), zoom, cache.poll_no);
        // A feed that could not ask (an unreadable session, say) says so — "sign in again"
        // beats a silent row that only looks empty.
        for (provider, reason) in skipped {
            cache.status.insert(provider, FeedStatus::Error(reason));
        }
        for req in requests {
            if cache.pending.contains_key(&req.key) {
                continue;
            }
            match &req.body {
                Some((body, content_type)) => {
                    fetcher.post(req.key.clone(), req.url.clone(), req.headers.clone(), body.clone(), content_type.clone(), cache.tx.clone());
                }
                None => {
                    fetcher.get(req.key.clone(), req.url.clone(), req.headers.clone(), cache.tx.clone());
                }
            }
            cache.pending.insert(req.key, req.provider);
            let entry = cache.status.entry(req.provider).or_insert(FeedStatus::Connecting);
            if !entry.has_data() {
                *entry = FeedStatus::Connecting;
            }
        }
        // The system's own handshake: the managed radars still waiting for their session get a
        // plain page load, whose Set-Cookie fills the credential above on the answer's return.
        for l in &doc.fleet.layers {
            if !l.needs_bootstrap() {
                continue;
            }
            let Some(req) = fleetlib::feed::build_bootstrap(l.provider, cache.poll_no) else { continue };
            if cache.pending.contains_key(&req.key) {
                continue;
            }
            fetcher.get(req.key.clone(), req.url.clone(), req.headers.clone(), cache.tx.clone());
            cache.pending.insert(req.key, req.provider);
            cache.status.insert(req.provider, FeedStatus::Connecting);
        }
        // Bolt's vehicle artwork: any CDN pin the markers name that isn't on file gets one
        // GET (once per url — the answer decodes into the cache and rides to the vault).
        let wanted: BTreeSet<String> = cache.markers_of(Provider::Bolt).iter().filter_map(|v| v.icon_url.clone()).filter(|u| !u.is_empty()).collect();
        for url in wanted {
            if cache.icon_images.contains_key(&url)
                || cache.icons.contains_key(&url)
                || cache.icons_pending.contains(&url)
                || cache.icons_failed.contains(&url)
            {
                continue;
            }
            cache.icons_pending.insert(url.clone());
            fetcher.get(format!("icon/{url}"), url, vec![("User-Agent".into(), "TerraZone/0.22.0 (Bolt vehicle artwork)".into())], cache.tx.clone());
        }
    }
}

/// Decode a PNG answer into an egui-ready RGBA image (None when it isn't a decodable PNG).
fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// The Fleet group's own row visibility by node name (the doc's tree is the eye truth).
fn row_visible(doc: &Document, name: &str) -> bool {
    let Some(gi) = doc.layers.iter().position(|n| n.name.as_deref() == Some("Fleet")) else { return false };
    let group = &doc.layers[gi];
    if !group.visible {
        return false;
    }
    let NodeKind::Layer { children, .. } = &group.kind else { return false };
    children.iter().any(|n| n.name.as_deref() == Some(name) && n.visible)
}

/// The part row that carries a feed's vehicles ("Trains" rides the SGR's rails).
pub(crate) fn vehicles_part(p: Provider) -> &'static str {
    match p {
        Provider::Bolt => "Vehicles",
        Provider::Marine => "Vessels",
        Provider::Flight => "Aircraft",
        Provider::Sgr => "Trains",
    }
}

/// A part row's eye, down the doc tree (Fleet group → feed row → part row). A row missing
/// (the sync hasn't run yet, an old file) reads visible — the sync owns the rows' existence,
/// the eyes are the user's control.
pub(crate) fn part_visible(doc: &Document, feed: &str, part: &str) -> bool {
    let Some(gi) = doc.layers.iter().position(|n| n.name.as_deref() == Some("Fleet")) else { return false };
    if !doc.layers[gi].visible {
        return false;
    }
    let NodeKind::Layer { children, .. } = &doc.layers[gi].kind else { return false };
    let Some(feed_node) = children.iter().find(|n| n.name.as_deref() == Some(feed)) else { return false };
    if !feed_node.visible {
        return false;
    }
    match &feed_node.kind {
        NodeKind::Layer { children: parts, .. } => parts.iter().find(|n| n.name.as_deref() == Some(part)).is_none_or(|p| p.visible),
        _ => true,
    }
}

/// Does anything draw right now? (Cheap gate before the painter work.) Every drawn artifact
/// is registered as a row under its feed — the answer reads the registry: a feed draws when
/// its eyes (feed row and part rows) say so and its data is there.
pub fn has_visible(app: &TerrazoneApp) -> bool {
    let Some(doc) = app.session.active().map(|st| &st.doc) else { return false };
    let cache = &app.canvas.fleet;
    for l in &doc.fleet.layers {
        if !l.visible || !row_visible(doc, l.provider.label()) {
            continue;
        }
        let label = l.provider.label();
        let transport = if l.provider == Provider::Sgr {
            // The schedule feed's data truth is the cache's status (the sweep answered) — no
            // auth. Its atlas and stations are embedded data: they draw from the eyes alone.
            cache.status_of(l.provider).has_data()
        } else {
            l.auth.is_set() && cache.status_of(l.provider).has_data()
        };
        if transport && !cache.markers_of(l.provider).is_empty() && part_visible(doc, label, vehicles_part(l.provider)) {
            return true;
        }
        if transport && l.provider != Provider::Sgr && !cache.grid_of(l.provider).is_empty() && part_visible(doc, label, "H3 Cells") {
            return true;
        }
        if l.provider == Provider::Sgr && part_visible(doc, label, "Rail Atlas") {
            return true;
        }
    }
    if doc.fleet.demo && doc.fleet.demo_visible && row_visible(doc, "Demo") {
        if !cache.demo_markers.is_empty() && part_visible(doc, "Demo", "Markers") {
            return true;
        }
        if !cache.demo_grid.is_empty() && part_visible(doc, "Demo", "H3 Cells") {
            return true;
        }
    }
    false
}

/// Is any feed live right now — fetching, waiting on an answer, riding a schedule, or the
/// demo drifting? The app's frame loop reads this to keep a 100 ms repaint cadence of its
/// own: the poll round, the markers and the SGR trains move while the mouse sits still, and
/// a fetch never waits for an input event to make its round (the bug the user saw: the map
/// only refetched when a mouse move happened to drive a frame).
pub fn is_live(app: &TerrazoneApp) -> bool {
    let Some(doc) = app.session.active().map(|st| &st.doc) else { return false };
    let cache = &app.canvas.fleet;
    if doc.fleet.demo && doc.fleet.demo_visible && row_visible(doc, "Demo") {
        return true;
    }
    doc.fleet.layers.iter().any(|l| {
        if !l.visible || !row_visible(doc, l.provider.label()) {
            return false;
        }
        if l.needs_bootstrap() || l.wants_fetch() {
            return true;
        }
        l.provider == Provider::Sgr && l.schedule.is_some() || !cache.markers_of(l.provider).is_empty()
    })
}

/// The 2D fleet: the H3 grid of the fetched items outlined under one vector icon per vehicle,
/// sized by the marine ladder, tinted per feed (the demo takes each kind's type colour),
/// over the basemap and under the art. The SGR row draws its own world: the OSM rail network
/// highlighted under the trains riding it, stations named, every train placed by its own
/// times.
pub fn draw(app: &mut TerrazoneApp, painter: &egui::Painter, rect: Rect, xf: &crate::canvas::Xf) {
    if !has_visible(app) {
        return;
    }
    // The vendor's artwork: the decoded PNGs become textures here, where the context lives.
    if !app.canvas.fleet.icon_images.is_empty() {
        let pending: Vec<(String, egui::ColorImage)> = app
            .canvas
            .fleet
            .icon_images
            .iter()
            .filter(|(url, _)| !app.canvas.fleet.icons.contains_key(*url))
            .map(|(url, img)| (url.clone(), img.clone()))
            .collect();
        for (url, img) in pending {
            let tex = painter.ctx().load_texture(format!("fleet-icon:{url}"), img, egui::TextureOptions::LINEAR);
            app.canvas.fleet.icons.insert(url, tex);
        }
    }
    let Some(doc) = app.session.active().map(|st| &st.doc) else { return };
    let cache = &app.canvas.fleet;
    // The zoom's marker scale: the same curve every fleet overlay reads, so the whole layer
    // breathes with the zoom instead of the icons standing still while the map moves.
    let zoom_scale = marker_scale(frame_level(xf));
    let mut shapes: Vec<Shape> = Vec::new();
    for l in &doc.fleet.layers {
        let label = l.provider.label();
        if !l.visible || !row_visible(doc, label) {
            continue;
        }
        let [r, g, b] = l.provider.color();
        let color = Color32::from_rgb(r, g, b);
        if l.provider == Provider::Sgr {
            // The SGR world: the embedded rail atlas and its stations draw from the eyes
            // alone (the app's own OSM extract — no sweep needed), the trains ride the
            // sweep's timetable, each artifact on its own registered row.
            if part_visible(doc, label, "Rail Atlas") {
                draw_rail(&mut shapes, &cache.atlas, xf, rect, color, zoom_scale);
            }
            if part_visible(doc, label, "Stations") {
                draw_stations(&mut shapes, painter, &cache.atlas, xf, rect, color, zoom_scale);
            }
            if part_visible(doc, label, "Trains") {
                draw_markers(&mut shapes, cache.markers_of(l.provider), xf, rect, color, &cache.icons, zoom_scale);
            }
            continue;
        }
        if !l.auth.is_set() || !cache.status_of(l.provider).has_data() {
            continue;
        }
        if part_visible(doc, label, "H3 Cells") {
            draw_grid(&mut shapes, cache.grid_of(l.provider), xf, rect, color);
        }
        if part_visible(doc, label, vehicles_part(l.provider)) {
            draw_markers(&mut shapes, cache.markers_of(l.provider), xf, rect, color, &cache.icons, zoom_scale);
        }
    }
    if doc.fleet.demo && doc.fleet.demo_visible && row_visible(doc, "Demo") {
        // The demo shows the whole system: every kind in its type colour, cells included —
        // each on its own registered row.
        if part_visible(doc, "Demo", "H3 Cells") {
            draw_kind_grid(&mut shapes, &cache.demo_grid, xf, rect);
        }
        if part_visible(doc, "Demo", "Markers") {
            draw_kind_markers(&mut shapes, &cache.demo_markers, xf, rect, &cache.icons, zoom_scale);
        }
    }
    painter.extend(shapes);
}

/// The view's geographic box (the corners' lon/lat extents) — the rail atlas's cull box.
fn view_box(xf: &crate::canvas::Xf, rect: Rect) -> (f64, f64, f64, f64) {
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    let (mut min_lon, mut min_lat, mut max_lon, mut max_lat) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for c in corners {
        let d = xf.to_doc(c);
        let (lon, lat) = terrazon_map::doc_to_lonlat(d.x, d.y);
        min_lon = min_lon.min(lon);
        max_lon = max_lon.max(lon);
        min_lat = min_lat.min(lat);
        max_lat = max_lat.max(lat);
    }
    (min_lon, min_lat, max_lon, max_lat)
}

/// The rail network highlight: every OSM railway line whose box touches the view, stroked in
/// the SGR tint — the "railway data from the OSM fetch" the trains ride. The stroke breathes
/// with the zoom (a hair at world view, fuller at street level).
fn draw_rail(out: &mut Vec<Shape>, atlas: &fleetlib::RailAtlas, xf: &crate::canvas::Xf, rect: Rect, color: Color32, zoom_scale: f32) {
    let (w, s, e, n) = view_box(xf, rect);
    let width = 0.9 + 1.1 * zoom_scale;
    let stroke = Stroke::new(width, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 170));
    for line in &atlas.lines {
        let [min_lon, min_lat, max_lon, max_lat] = line.bbox;
        if max_lon < w || min_lon > e || max_lat < s || min_lat > n {
            continue;
        }
        let pts: Vec<Pos2> = line.pts.iter().map(|p| to_screen(p[0], p[1], xf)).collect();
        if pts.len() < 2 {
            continue;
        }
        out.push(Shape::Path(egui::epaint::PathShape { points: pts, closed: false, fill: Color32::TRANSPARENT, stroke: stroke.into() }));
    }
}

/// The named stations: a small platform block per stop with its name once the zoom reads
/// street names (the same read the map's own labels ride). The block scales with the zoom
/// like the markers do.
fn draw_stations(
    out: &mut Vec<Shape>,
    painter: &egui::Painter,
    atlas: &fleetlib::RailAtlas,
    xf: &crate::canvas::Xf,
    rect: Rect,
    color: Color32,
    zoom_scale: f32,
) {
    let (w, s, e, n) = view_box(xf, rect);
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 220);
    let block = (3.0 + 4.0 * zoom_scale).max(3.5);
    for st in &atlas.stations {
        if st.lon < w || st.lon > e || st.lat < s || st.lat > n {
            continue;
        }
        let p = to_screen(st.lon, st.lat, xf);
        if !rect.expand(20.0).contains(p) {
            continue;
        }
        out.push(Shape::rect_filled(Rect::from_center_size(p, egui::vec2(block, block)), 1.5, fill));
        out.push(Shape::rect_stroke(
            Rect::from_center_size(p, egui::vec2(block, block)),
            1.5,
            Stroke::new(1.0, Color32::from_black_alpha(160)),
            egui::StrokeKind::Outside,
        ));
        if xf.zoom >= 10.0 {
            let font = egui::FontId::proportional(9.0 + 2.5 * zoom_scale);
            let galley = painter.ctx().fonts_mut(|f| f.layout_no_wrap(st.name.clone(), font, Color32::from_black_alpha(225)));
            out.push(Shape::Text(egui::epaint::TextShape::new(p + egui::vec2(6.0, -6.0), galley, Color32::from_white_alpha(180))));
        }
    }
}

/// The type colour a vehicle kind draws in (the demo's stand-in for its feed's tint). The
/// 3D environment reads the same mapping, so both canvases show one fleet in one palette.
pub(crate) fn kind_color(kind: fleetlib::VehicleKind) -> Color32 {
    let [r, g, b] = match kind {
        fleetlib::VehicleKind::Plane | fleetlib::VehicleKind::PlaneHeavy | fleetlib::VehicleKind::PlaneLight | fleetlib::VehicleKind::Helicopter => {
            fleetlib::Provider::Flight.color()
        }
        fleetlib::VehicleKind::Boat | fleetlib::VehicleKind::Ship => fleetlib::Provider::Marine.color(),
        fleetlib::VehicleKind::Train => fleetlib::Provider::Sgr.color(),
        fleetlib::VehicleKind::Car | fleetlib::VehicleKind::Van | fleetlib::VehicleKind::Motorcycle => fleetlib::Provider::Bolt.color(),
    };
    Color32::from_rgb(r, g, b)
}

fn to_screen(lon: f64, lat: f64, xf: &crate::canvas::Xf) -> Pos2 {
    let (dx, dy) = terrazon_map::lonlat_to_doc(lon, lat);
    let p = xf.to_screen(terrazon_geom::Point::new(dx, dy));
    pos2(p.x, p.y)
}

/// One cell's hexagon ring in screen space (None when far off the canvas).
fn cell_ring(cell: CellIndex, xf: &crate::canvas::Xf, rect: Rect) -> Option<Vec<Pos2>> {
    let pts: Vec<Pos2> = fleetlib::h3grid::cell_boundary(cell).iter().map(|(lon, lat)| to_screen(*lon, *lat, xf)).collect();
    let first = pts.first()?;
    let mut bounds = egui::Rect::from_min_size(*first, egui::Vec2::ZERO);
    for p in &pts {
        bounds.extend_with(*p);
    }
    if !rect.expand(200.0).intersects(bounds) {
        return None;
    }
    Some(pts)
}

/// The H3 grid: each fetched item's cell outlined in its feed's colour, a faint fill so the
/// tiles read as one layer over the basemap.
fn draw_grid(out: &mut Vec<Shape>, cells: &[CellIndex], xf: &crate::canvas::Xf, rect: Rect, color: Color32) {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24);
    let stroke = Stroke::new(1.2, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 130));
    for cell in cells {
        let Some(pts) = cell_ring(*cell, xf, rect) else { continue };
        out.push(Shape::Path(egui::epaint::PathShape { points: pts, closed: true, fill, stroke: stroke.into() }));
    }
}

/// The demo's grid: each cell takes the type colour of the kind that named it.
fn draw_kind_grid(out: &mut Vec<Shape>, grid: &[(CellIndex, fleetlib::VehicleKind)], xf: &crate::canvas::Xf, rect: Rect) {
    for (cell, kind) in grid {
        let Some(pts) = cell_ring(*cell, xf, rect) else { continue };
        let color = kind_color(*kind);
        let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24);
        let stroke = Stroke::new(1.2, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 130));
        out.push(Shape::Path(egui::epaint::PathShape { points: pts, closed: true, fill, stroke: stroke.into() }));
    }
}

fn draw_markers(
    out: &mut Vec<Shape>,
    markers: &[Vehicle],
    xf: &crate::canvas::Xf,
    rect: Rect,
    color: Color32,
    icons: &HashMap<String, egui::TextureHandle>,
    zoom_scale: f32,
) {
    for v in markers {
        let mut centre = to_screen(v.lon, v.lat, xf);
        // Scheduled events stack at their origin station by definition (five departures from
        // Dar es Salaam draw five marks on the same point) — a deterministic spread fans them
        // out round the station so the count reads: same id, same offset, every frame.
        if v.scheduled {
            let mut h: u32 = 0x811c_9dc5;
            for b in v.id.bytes() {
                h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
            }
            let ring = (h % 8) as f32;
            let step = fleetlib::sizing::px(fleetlib::sizing::class_of(v.kind, v.length_m)) * 1.1 * zoom_scale;
            let (dx, dy) = match ring as u32 {
                0 => (0.0, 0.0), // the first wait sits on the station itself
                n => {
                    let a = (n as f32) * std::f32::consts::TAU / 7.0;
                    (a.sin() * 2.2 * step, -a.cos() * 2.2 * step)
                }
            };
            centre += egui::vec2(dx, dy);
        }
        if !rect.expand(60.0).contains(centre) {
            continue;
        }
        icon_shape(out, v, centre, color, icons, zoom_scale);
    }
}

/// The demo's markers, each kind in its type colour.
fn draw_kind_markers(
    out: &mut Vec<Shape>,
    markers: &[Vehicle],
    xf: &crate::canvas::Xf,
    rect: Rect,
    icons: &HashMap<String, egui::TextureHandle>,
    zoom_scale: f32,
) {
    for v in markers {
        let centre = to_screen(v.lon, v.lat, xf);
        if !rect.expand(60.0).contains(centre) {
            continue;
        }
        icon_shape(out, v, centre, kind_color(v.kind), icons, zoom_scale);
    }
}

/// The zoom-adaptive marker scale: the fleet's ladder reads full size at street zooms and
/// eases down as the map zooms out, so a far view (z10 and out) reads the map instead of a wall
/// of icons. 1.0 from z15 up (the ladder's own domain), ~0.5 at z10, ~0.38 at the widest. The
/// assets' glyph floor reads the same curve, so every overlay breathes with the zoom together.
pub(crate) fn marker_scale(level: f64) -> f32 {
    let k = ((level - 8.0) / 7.0).clamp(0.0, 1.0) as f32;
    0.38 + 0.62 * k
}

/// The zoom level this canvas frame is at (the map's own Web-Mercator level).
fn frame_level(xf: &crate::canvas::Xf) -> f64 {
    crate::state::map_zoom_level(xf.zoom)
}

/// The ceiling any single marker reaches on screen (points): the ladder breathes with the
/// zoom, but even at street zoom no pin or glyph grows past this — a marker names a point,
/// it does not cover the block it sits in.
const ICON_MAX: f32 = 28.0;

/// One vehicle's icon: the kind's outline rotated to its heading (nose lands on
/// `(sin h, −cos h)`), sized by the marine ladder under the zoom's scale, over a white halo.
/// The vendor's own artwork (Bolt's downloaded pin) rides the same bearing: the pin is
/// anchored tip-down at the point and swung round to the vehicle's heading, over a soft
/// ground shadow, with a heading needle over it so the direction reads even where the
/// artwork itself is round.
fn icon_shape(out: &mut Vec<Shape>, v: &Vehicle, centre: Pos2, color: Color32, icons: &HashMap<String, egui::TextureHandle>, zoom_scale: f32) {
    // The vendor's own artwork first (Bolt's downloaded pin, anchored tip-down at the point).
    if let Some(url) = &v.icon_url
        && let Some(tex) = icons.get(url)
    {
        let (w, h) = (tex.size()[0] as f32, tex.size()[1] as f32);
        // The floor keeps the pin seen at every zoom ("at least seen well"); the cap keeps a
        // marker a marker (it names a point, it does not cover the block).
        let target = (fleetlib::sizing::px(fleetlib::sizing::class_of(v.kind, v.length_m)) * 1.9 * zoom_scale).clamp(14.0, ICON_MAX);
        let scale = target / h.max(1.0);
        let (pw, ph) = (w * scale, h * scale);
        // The ground shadow: a soft ellipse under the tip so the pin sits ON the map.
        out.push(Shape::ellipse_filled(centre + egui::vec2(0.0, ph * 0.06), egui::vec2(ph * 0.34, ph * 0.12), Color32::from_black_alpha(60)));
        let known = v.heading_deg.is_finite() && v.heading_deg >= 0.0;
        let rad = if known { v.heading_deg.to_radians() } else { 0.0 };
        let (sin, cos) = rad.sin_cos();
        // The pin's corners in body frame (tip at the origin, body above), swung round the
        // tip by the bearing (screen-clockwise: heading 90 points the body east).
        let corner = |dx: f32, dy: f32| pos2(centre.x + dx * cos - dy * sin, centre.y + dx * sin + dy * cos);
        let mut mesh = egui::Mesh::with_texture(tex.id());
        let mut quad: [u32; 4] = [0; 4];
        for (i, ((dx, dy), uv)) in [
            ((-pw * 0.5, -ph), pos2(0.0, 0.0)),
            ((pw * 0.5, -ph), pos2(1.0, 0.0)),
            ((pw * 0.5, 0.0), pos2(1.0, 1.0)),
            ((-pw * 0.5, 0.0), pos2(0.0, 1.0)),
        ]
        .into_iter()
        .enumerate()
        {
            quad[i] = mesh.vertices.len() as u32;
            mesh.vertices.push(egui::epaint::Vertex { pos: corner(dx, dy), uv, color: Color32::WHITE });
        }
        let [tl, tr, br, bl] = quad;
        mesh.add_triangle(tl, tr, br);
        mesh.add_triangle(tl, br, bl);
        out.push(Shape::Mesh(std::sync::Arc::new(mesh)));
        // The heading needle: a short dart from the pin's middle along the bearing, with the
        // feed's colour under a white edge — the direction reads even on round artwork.
        if known {
            let from = centre + egui::vec2(sin * ph * 0.12, -cos * ph * 0.12);
            let to = centre + egui::vec2(sin * ph * 0.46, -cos * ph * 0.46);
            out.push(Shape::line_segment([from, to], Stroke::new(2.6, Color32::from_black_alpha(90))));
            out.push(Shape::line_segment([from, to], Stroke::new(1.4, color)));
            out.push(Shape::circle_filled(to, 2.2, color));
            out.push(Shape::circle_filled(to, 1.1, Color32::WHITE));
        }
        return;
    }
    let class = fleetlib::sizing::class_of(v.kind, v.length_m);
    let size = (fleetlib::sizing::px(class) * 1.3 * zoom_scale).min(ICON_MAX);
    let rad = v.heading_deg.to_radians();
    let (sin, cos) = rad.sin_cos();
    let pts: Vec<Pos2> =
        v.kind.icon().iter().map(|(ix, iy)| pos2(centre.x + (ix * cos + iy * sin) * size, centre.y + (ix * sin - iy * cos) * size)).collect();
    // A scheduled event (the SGR's trips still waiting for their departure) draws dimmed —
    // clearly data, clearly not running yet; the live markers carry the full colour.
    let (r, g, b) = (color.r(), color.g(), color.b());
    let (fill, halo, edge) = if v.scheduled {
        (Color32::from_rgba_unmultiplied(r, g, b, 115), Color32::from_white_alpha(90), Stroke::new(1.0, Color32::from_black_alpha(110)))
    } else {
        (color, Color32::from_white_alpha(150), Stroke::new(1.0, Color32::from_black_alpha(190)))
    };
    out.push(Shape::circle_filled(centre, size * 0.85, halo));
    out.push(Shape::Path(egui::epaint::PathShape { points: pts, closed: true, fill, stroke: edge.into() }));
}
