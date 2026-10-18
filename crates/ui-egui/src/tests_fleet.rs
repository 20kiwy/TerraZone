//! The fleet runtime's headless tests: the demo feed fills the cache and the layer gate reads
//! the tree, all with `Services::default()` (no fetcher — the demo is offline by design).

use crate::{Services, TerrazoneApp};
use serde_json::json;
use terrazon_fleet::{FeedStatus, Provider};

fn app_with_map() -> TerrazoneApp {
    let mut app = TerrazoneApp::new(terrazon_engine::Session::new(), Services::default());
    app.run("file.new", json!({ "preset": "World Map" })).unwrap();
    app
}

/// The visible view box a poll covers, the way the canvas computes it (around the view centre).
fn view_of(app: &TerrazoneApp) -> crate::fleet::FleetView {
    let v = app.view().unwrap();
    let (lon, lat) = terrazon_map::doc_to_lonlat(v.center.x, v.center.y);
    ((lon - 0.05, lat - 0.03, lon + 0.05, lat + 0.03), 16.0)
}

#[test]
fn the_demo_fills_the_cache_and_the_gate_opens() {
    let mut app = app_with_map();
    assert!(!crate::fleet::has_visible(&app), "nothing configured: nothing draws");
    app.run("fleet.demo", json!({ "on": true })).unwrap();
    // The demo's sync registers the whole tree — and the SGR atlas draws from its rows alone
    // (embedded data, no sweep needed): the gate is open already. The demo's own markers wait
    // for the tick.
    assert!(crate::fleet::has_visible(&app), "the registry is live: the SGR atlas rides its rows");
    assert!(app.canvas.fleet.demo_markers.is_empty(), "the demo's own markers wait for the first tick");
    let view = view_of(&app);
    crate::fleet::tick(&mut app, Some(view));
    let cache = &app.canvas.fleet;
    assert!(!cache.demo_markers.is_empty(), "the demo generated its sample traffic");
    assert!(crate::fleet::has_visible(&app), "demo on + markers: the gate opens");
    // The doc holds the config truth: demo on, the tree has the group — and the drawn parts
    // are registered rows under the feed.
    let doc = &app.session.active().unwrap().doc;
    assert!(doc.fleet.demo);
    assert!(doc.layers.iter().any(|n| n.name.as_deref() == Some("Fleet")));
    assert!(crate::fleet::part_visible(doc, "Demo", "Markers"), "the demo's markers are a registered row");
    assert!(crate::fleet::part_visible(doc, "Demo", "H3 Cells"), "the demo's cells are a registered row");
    // Toggling the demo's eye closes its rows' gate — the SGR rows keep their own eyes.
    app.run("fleet.toggle", json!({ "provider": "demo", "visible": false })).unwrap();
    let doc = &app.session.active().unwrap().doc;
    assert!(!crate::fleet::part_visible(doc, "Demo", "Markers"), "the eye says hidden");
    assert!(crate::fleet::part_visible(doc, "SGR Trains", "Rail Atlas"), "the SGR rows keep their own eyes");
    app.run("fleet.toggle", json!({ "provider": "demo", "visible": true })).unwrap();
    let doc = &app.session.active().unwrap().doc;
    assert!(crate::fleet::part_visible(doc, "Demo", "Markers"));
}

#[test]
fn the_part_rows_gate_their_own_draw() {
    let mut app = app_with_map();
    app.run("fleet.demo", json!({ "on": true })).unwrap();
    let view = view_of(&app);
    crate::fleet::tick(&mut app, Some(view));
    assert!(!app.canvas.fleet.demo_markers.is_empty(), "the demo generated its traffic");
    // The demo's Markers row hides: its markers stop drawing (the grid rides its own row).
    let part_id = |doc: &terrazon_doc::Document, feed: &str, part: &str| -> Option<terrazon_doc::NodeId> {
        let gi = doc.layers.iter().position(|n| n.name.as_deref() == Some("Fleet"))?;
        let terrazon_doc::NodeKind::Layer { children, .. } = &doc.layers[gi].kind else { return None };
        let feed = children.iter().find(|n| n.name.as_deref() == Some(feed))?;
        let terrazon_doc::NodeKind::Layer { children, .. } = &feed.kind else { return None };
        children.iter().find(|n| n.name.as_deref() == Some(part)).map(|n| n.id)
    };
    let doc = &app.session.active().unwrap().doc;
    let markers_row = part_id(doc, "Demo", "Markers").expect("the Markers row registered");
    app.run("layer.setProps", json!({ "ids": [markers_row.0], "visible": false })).unwrap();
    let doc = &app.session.active().unwrap().doc;
    assert!(!crate::fleet::part_visible(doc, "Demo", "Markers"), "the part's eye is the draw gate");
    assert!(crate::fleet::part_visible(doc, "Demo", "H3 Cells"), "the cells' row keeps its own eye");
    // The cells' row hides too: the demo draws nothing — the SGR atlas keeps its own rows.
    let doc = &app.session.active().unwrap().doc;
    let cells_row = part_id(doc, "Demo", "H3 Cells").expect("the H3 Cells row registered");
    app.run("layer.setProps", json!({ "ids": [cells_row.0], "visible": false })).unwrap();
    let doc = &app.session.active().unwrap().doc;
    assert!(!crate::fleet::part_visible(doc, "Demo", "H3 Cells"));
    assert!(crate::fleet::part_visible(doc, "SGR Trains", "Rail Atlas"), "every part row owns its eye");
}

#[test]
fn statuses_start_off_and_the_demo_tick_drifts() {
    let mut app = app_with_map();
    app.run("fleet.demo", json!({ "on": true })).unwrap();
    let status = app.canvas.fleet.status_of(Provider::Bolt);
    assert_eq!(status, FeedStatus::Off, "no auth: Off");
    let view = view_of(&app);
    crate::fleet::tick(&mut app, Some(view));
    let a = app.canvas.fleet.demo_markers.clone();
    assert!(!a.is_empty());
    // Advance the cadence and tick again: the same vehicles drift (ids stable). `fleet.poll`
    // marks the state poll-due, so the next tick runs at once.
    let _ = app.run("fleet.poll", json!({}));
    let view = view_of(&app);
    crate::fleet::tick(&mut app, Some(view));
    let b = app.canvas.fleet.demo_markers.clone();
    let ids_a: std::collections::BTreeSet<_> = a.iter().map(|v| v.id.clone()).collect();
    let ids_b: std::collections::BTreeSet<_> = b.iter().map(|v| v.id.clone()).collect();
    assert_eq!(ids_a, ids_b, "the same fleet, moved");
    assert!(a != b, "the positions drifted");
}

#[test]
fn signing_in_marks_connecting_and_polls_the_cells() {
    // No fetcher in Services::default(), so the ask can't leave — the status wiring is what the
    // test pins: Off → Connecting on sign-in, never Ready without an answer.
    let mut app = app_with_map();
    let _ = app.run("fleet.setAuth", json!({ "provider": "bolt", "token": "tok" }));
    let doc = &app.session.active().unwrap().doc;
    assert!(doc.fleet.layer(Provider::Bolt).unwrap().auth.is_set());
    assert!(app.canvas.fleet.markers_of(Provider::Bolt).is_empty(), "no answer yet: bolt has nothing to draw");
    // The sync registered the tree — the SGR atlas rides its rows alone (embedded data).
    assert!(crate::fleet::part_visible(doc, "SGR Trains", "Rail Atlas"));
    let bolt_node = doc.layers.iter().find(|n| n.name.as_deref() == Some("Fleet")).is_some();
    assert!(bolt_node, "the Fleet group exists once a feed signs in");
}

/// A scripted fleet fetcher: answers each ask with the vendor-shaped bytes the sweep and the
/// Bolt poll expect (no network), recording the URLs it was asked for.
#[derive(Default)]
struct ScriptedFleet {
    asks: std::sync::Mutex<Vec<String>>,
}

impl ScriptedFleet {
    fn asked_for(&self, needle: &str) -> bool {
        self.asks.lock().unwrap().iter().any(|u| u.contains(needle))
    }
}

const STATIONS_ANSWER: &[u8] =
    br#"{"code":200,"ok":true,"message":"GNRL100 - Success.","data":[{"id":1,"title":"Dar Es Salaam"},{"id":2,"title":"Pugu"},{"id":12,"title":"Dodoma"}]}"#;
const TRIPS_ANSWER: &[u8] = br#"{"code":200,"ok":true,"data":[{"tripId":153,"tripName":"DAR -- DOM | EXPRESS","tripsNo":"0011904","startTime":"06:00:00","endTime":"09:50:00","boardingStationId":1,"boardingStationName":"Dar Es Salaam","landingStationId":12,"landingStationName":"Dodoma"}]}"#;
const BOLT_POLL_ANSWER: &[u8] = br#"{"code":0,"message":"OK","data":{"vehicles":{
    "taxi":{"3428":[{"id":"v1","lat":59.439,"lng":24.754,"bearing":10.0,"icon_id":"3428"}]},
    "icons":{"taxi":{"3428":{"icon_url":"https://images.bolt.eu/test/pin.png"}}},
    "category_details":{"taxi":{"3428":{"name":"Comfort","group":"comfort"}}}}}}"#;

impl terrazon_fleet::FleetFetch for ScriptedFleet {
    fn get(&self, key: String, url: String, _headers: Vec<(String, String)>, tx: std::sync::mpsc::Sender<terrazon_fleet::FleetMsg>) {
        self.asks.lock().unwrap().push(url.clone());
        let bytes: Vec<u8> = if url.contains("GetTripAssignedStations") {
            STATIONS_ANSWER.to_vec()
        } else if url.starts_with("https://images.bolt.eu/test/pin.png") {
            // A real 2×3 red PNG: the icon pipeline decodes it for the map.
            let img = image::RgbaImage::from_pixel(2, 3, image::Rgba([255, 0, 0, 255]));
            let mut png = Vec::new();
            image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
            png
        } else {
            TRIPS_ANSWER.to_vec()
        };
        let _ = tx.send(terrazon_fleet::FleetMsg::Done { key, bytes, set_cookie: None });
    }

    fn post(
        &self,
        key: String,
        url: String,
        _headers: Vec<(String, String)>,
        _body: String,
        _content_type: String,
        tx: std::sync::mpsc::Sender<terrazon_fleet::FleetMsg>,
    ) {
        self.asks.lock().unwrap().push(url.clone());
        let bytes = if url.contains("mobility/search/poll") { BOLT_POLL_ANSWER.to_vec() } else { TRIPS_ANSWER.to_vec() };
        let _ = tx.send(terrazon_fleet::FleetMsg::Done { key, bytes, set_cookie: None });
    }
}

#[test]
fn the_sgr_sweep_builds_the_timetable_and_bolt_downloads_its_icons() {
    let scripted = std::sync::Arc::new(ScriptedFleet::default());
    let mut app = TerrazoneApp::new(terrazon_engine::Session::new(), Services { fleet: Some(scripted.clone()), ..Services::default() });
    app.run("file.new", json!({ "preset": "World Map" })).unwrap();
    app.run("fleet.syncTree", json!({})).unwrap();
    // Bolt signs in with a stored session (the shape the OTP flow mints).
    let session = terrazon_fleet::BoltSession {
        session_id: "dev".into(),
        device_id: "dev".into(),
        auth_username: "uid_7".into(),
        phone: "+255700000000".into(),
        created_ms: 0,
    };
    app.run("fleet.setAuth", json!({ "provider": "Bolt", "token": session.to_secret() })).unwrap();

    let view = view_of(&app);
    // Tick 1: the first poll — the sweep's station read and Bolt's vehicle poll go out.
    crate::fleet::tick(&mut app, Some(view));
    assert!(scripted.asked_for("GetTripAssignedStations"), "the sweep starts with the stations");
    assert!(scripted.asked_for("mobility/search/poll"), "bolt polls the vehicles");
    // Tick 2: the answers drain — the stations land as the timetable's base, Bolt's markers
    // (and their artwork ask) follow.
    crate::fleet::tick(&mut app, Some(view));
    {
        let l = app.session.active().unwrap().doc.fleet.layer(Provider::Sgr).unwrap().clone();
        let tt = l.schedule.expect("the stations answer lands the timetable");
        assert_eq!(tt.stations.len(), 3);
        assert_eq!(l.status, FeedStatus::Ready);
    }
    assert_eq!(app.canvas.fleet.status_of(Provider::Sgr), FeedStatus::Ready);
    assert_eq!(app.canvas.fleet.markers_of(Provider::Bolt).len(), 1, "the poll's vehicle parsed");
    // Tick 3 (forced): the sweep's next step (Dar es Salaam → Dodoma) rides poll 2.
    app.run("fleet.poll", json!({})).unwrap();
    crate::fleet::tick(&mut app, Some(view));
    assert!(scripted.asked_for("SearchTrip"), "the sweep searches the trip pairs");
    assert!(scripted.asked_for("images.bolt.eu/test/pin.png"), "the vehicle's artwork is on its way");
    // Tick 4: the trip answer merges into the train; the artwork decodes.
    crate::fleet::tick(&mut app, Some(view));
    let doc = app.session.active().unwrap().doc.clone();
    let l = doc.fleet.layer(Provider::Sgr).unwrap();
    let tt = l.schedule.as_ref().unwrap();
    assert_eq!(tt.trains.len(), 1, "the trip merged under its tripsNo");
    assert_eq!(tt.trains[0].stops.len(), 2, "Dar es Salaam and Dodoma in running order");
    assert_eq!(tt.trains[0].stops[0].name, "Dar Es Salaam");
    assert!(app.canvas.fleet.icon_images.contains_key("https://images.bolt.eu/test/pin.png"), "the artwork decoded");
    // The sources query reports the timetable.
    let q = app.run("fleet.sources", json!({})).unwrap();
    let sgr = q["layers"].as_array().unwrap().iter().find(|l| l["provider"] == "SGR Trains").unwrap().clone();
    assert_eq!(sgr["trains"], 1);
    assert_eq!(sgr["status"], "ready");
}

#[test]
fn the_sgr_trains_ride_the_timetable_at_the_scheduled_times() {
    // A schedule whose train is running NOW (whatever wall-clock time the test runs at):
    // the first stop 40 minutes ago, the terminal 40 minutes from now, so the train is
    // halfway along the Dar es Salaam → Dodoma slice of the real atlas.
    let now = terrazon_fleet::rail::now_minutes_eat(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
    );
    let timetable = json!({
        "date": terrazon_fleet::rail::today_eat(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
        ),
        "stations": [[1, "Dar Es Salaam"], [12, "Dodoma"]],
        "trains": [{
            "tripsNo": "0011999",
            "name": "TEST RUNNER",
            "direction": 1,
            "stops": [
                {"stationId": 1, "name": "Dar Es Salaam", "timeMin": (now + 1440 - 40) % 1440},
                {"stationId": 12, "name": "Dodoma", "timeMin": (now + 40) % 1440},
            ],
        }],
    });
    let mut app = app_with_map();
    app.run("fleet.sgrSchedule", timetable).unwrap();
    let view = view_of(&app);
    crate::fleet::tick(&mut app, Some(view));
    let markers = app.canvas.fleet.markers_of(Provider::Sgr);
    assert_eq!(markers.len(), 1, "the train is on the line: {markers:?}");
    let m = &markers[0];
    assert_eq!(m.kind, terrazon_fleet::VehicleKind::Train);
    // Dar es Salaam sits at (39.24, -6.82), Dodoma at (35.74, -6.17): halfway by TIME, placed
    // on the rail path — somewhere strictly between, on land the atlas covers.
    let dar = (39.24_f64, -6.82_f64);
    let (lon, lat) = (m.lon, m.lat);
    assert!(((lon - dar.0).abs() < 3.6) && ((lat - dar.1).abs() < 0.7), "the train rides between the terminals: ({lon}, {lat})");
    assert!(m.heading_deg >= 0.0, "the heading rides the rail path");
    assert!(crate::fleet::has_visible(&app), "a live timetable with a running train draws");
}

/// The zoom-adaptive marker scale: full ladder at street zooms, eased down when far — a far
/// view reads the map instead of a wall of icons, and every overlay shares the one curve.
#[test]
fn marker_scale_eases_down_when_far_and_back_at_street_level() {
    let at = |lvl: f64| crate::fleet::marker_scale(lvl);
    assert!((at(15.0) - 1.0).abs() < 1e-6, "z15 and up is the ladder's full size: {}", at(15.0));
    assert!((at(19.0) - 1.0).abs() < 1e-6, "close zooms never grow past it: {}", at(19.0));
    let z10 = at(10.0);
    assert!(z10 < 0.62 && z10 > 0.4, "z10 reads about half the ladder: {z10}");
    let z8 = at(8.0);
    assert!((z8 - 0.38).abs() < 1e-6, "the widest view bottoms out at the floor: {z8}");
    assert!(at(4.0) == z8, "clamped below the floor of the curve: {}", at(4.0));
    assert!(at(12.0) > z10 && at(12.0) < 1.0, "the ease climbs through the middle: {}", at(12.0));
    // A far-view car (class 2, 11 px at 1.45×) lands near the map's own street symbols.
    let px = terrazon_fleet::sizing::px(2) * 1.45 * z10;
    assert!(px < 10.0, "a car at z10 is a map symbol, not a billboard: {px}");
}
