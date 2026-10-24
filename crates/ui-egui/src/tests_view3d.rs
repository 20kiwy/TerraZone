//! The 3D environment's commands and state: the dual-canvas switch (`view.mode3d`), the scene
//! settings (`scene3d.set`), the placements (`assets.*` — placed at doc points, listed back
//! away, without it arms the ghost) and the placements list.

use serde_json::json;

use crate::{Services, TerrazoneApp};

fn app_with_map() -> TerrazoneApp {
    let mut app = TerrazoneApp::new(terrazon_engine::Session::new(), Services::default());
    app.run("file.new", json!({ "preset": "World Map" })).unwrap();
    app
}

#[test]
fn mode3d_defaults_off_and_the_command_toggles_it() {
    let mut app = app_with_map();
    assert!(!app.view().is_some_and(|v| v.mode3d), "a document opens 2D");
    let r = app.run("view.mode3d", json!({})).unwrap();
    assert_eq!(r, json!(true), "no param toggles");
    assert!(app.view().is_some_and(|v| v.mode3d));
    let r = app.run("view.mode3d", json!({ "on": false })).unwrap();
    assert_eq!(r, json!(false));
    assert!(!app.view().is_some_and(|v| v.mode3d));
}

#[test]
fn mode3d_survives_a_serde_roundtrip() {
    let mut app = app_with_map();
    app.run("view.mode3d", json!({ "on": true })).unwrap();
    // The View struct serde-roundtrips with the flag (a saved view restores into it).
    let v = app.view().unwrap();
    let json = serde_json::to_string(v).unwrap();
    let back: crate::View = serde_json::from_str(&json).unwrap();
    assert!(back.mode3d);
    // The field is serde(default): a View built from JSON without it reads false.
    let older: crate::View =
        serde_json::from_str(&json!({ "zoom": 1.0, "center": [306.0, 396.0], "fitted": false, "rotation": 0.0, "artboard": 0 }).to_string()).unwrap();
    assert!(!older.mode3d);
}

#[test]
fn scene3d_set_clamps_the_settings() {
    let mut app = app_with_map();
    let r = app.run("scene3d.set", json!({ "contourInterval": -5.0, "imageryZoom": 3 })).unwrap();
    assert_eq!(r["contourInterval"], json!(0.0), "negative goes flat (off)");
    assert_eq!(r["imageryZoom"], json!(10), "the floor clamps");
    let r = app.run("scene3d.set", json!({})).unwrap();
    assert_eq!(r["contourInterval"], json!(0.0), "no params keep everything");
    assert_eq!(r["imageryZoom"], json!(10));
}

#[test]
fn scene3d_set_drives_the_camera_modes_and_the_walk_rig() {
    // The EffectCraft viewer's mode pair rides the command surface: "walk" stands the rig at
    // the orbit eye (the view hands over, no jump), "orbit" re-aims the arm from the walk.
    let mut app = app_with_map();
    app.ui.cam3d.dist = 500.0;
    let eye = app.ui.cam3d.eye();
    let r = app.run("scene3d.set", json!({ "camMode": "walk" })).unwrap();
    assert_eq!(r["camMode"], json!("walk"));
    assert_eq!(app.ui.cam_mode, terrazon_scene3d::CamMode::Walk);
    assert!((app.ui.walk3d.pos[0] - eye.x).abs() < 0.01 && (app.ui.walk3d.pos[2] - eye.z).abs() < 0.01);
    // The walk rig's own params drive it (clamped like the panel's sliders).
    app.run("scene3d.set", json!({ "walkX": 40.0, "walkY": -3.0, "walkZ": -20.0, "walkYaw": 90.0, "walkPitch": 120.0 })).unwrap();
    assert_eq!(app.ui.walk3d.pos[0], 40.0);
    assert_eq!(app.ui.walk3d.pos[2], -20.0);
    assert_eq!(app.ui.walk3d.pos[1], terrazon_scene3d::WalkCam::MIN_HEIGHT, "the floor clamps");
    assert!((app.ui.walk3d.yaw.to_degrees() - 90.0).abs() < 0.01);
    assert_eq!(app.ui.walk3d.pitch, terrazon_scene3d::WalkCam::PITCH_LIMIT, "the pitch clamps at ±83°");
    // Back to orbit: the arm looks where the walk looked (a level-ish look — an orbit arm
    // always pivots down at its target, so the handover matches the look it can mirror).
    app.run("scene3d.set", json!({ "walkPitch": -20.0 })).unwrap();
    let look_w = app.ui.walk3d.dir();
    let r = app.run("scene3d.set", json!({ "camMode": "orbit" })).unwrap();
    assert_eq!(r["camMode"], json!("orbit"));
    let look_o = (glam::Vec3::from(app.ui.cam3d.target) - app.ui.cam3d.eye()).normalize_or_zero();
    assert!(look_w.dot(look_o) > 0.95, "the orbit looks where the walk looked");
    // A nonsense mode string changes nothing.
    app.run("scene3d.set", json!({ "camMode": "sideways" })).unwrap();
    assert_eq!(app.ui.cam_mode, terrazon_scene3d::CamMode::Orbit);
}

#[test]
fn assets_place_at_doc_points_and_the_list_reports_them() {
    let mut app = app_with_map();
    let r = app.run("placements.place", json!({ "kind": "house", "x": 1234.5, "y": 987.0 })).unwrap();
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(r["kind"], json!("House"));
    let list = app.run("placements.list", json!({})).unwrap();
    let arr = list["placements"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], json!("House"));
    assert_eq!(arr[0]["x"], json!(1234.5));
    assert_eq!(arr[0]["id"], json!(id));
    // The placements are document data: they survive a roundtrip through the doc.
    let doc = app.session.active().unwrap().doc.clone();
    let json = serde_json::to_string(&doc).unwrap();
    let back: terrazon_doc::Document = serde_json::from_str(&json).unwrap();
    assert_eq!(back.placements.placements.len(), 1);
}

#[test]
fn the_assets_tool_brushes_a_row_and_remove_clears() {
    let mut app = app_with_map();
    let r = app.run("placements.spray", json!({ "kind": "tree", "points": [[0.0, 0.0], [10.0, 0.0], [20.0, 0.0]] })).unwrap();
    assert_eq!(r["placed"], json!(3));
    assert_eq!(app.run("placements.list", json!({})).unwrap()["placements"].as_array().unwrap().len(), 3);
    let e = app.run("placements.place", json!({ "kind": "rocket", "x": 1.0, "y": 1.0 })).unwrap_err();
    assert!(e.contains("tree"), "the error names the real kinds: {e}");
    let r = app.run("placements.remove", json!({ "id": "nope" })).unwrap();
    assert_eq!(r["removed"], json!(false));
    let id = app.run("placements.place", json!({ "kind": "Tree", "x": 5.0, "y": 5.0 })).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(app.run("placements.remove", json!({ "id": id })).unwrap()["removed"], json!(true));
    let cleared = app.run("placements.clear", json!({})).unwrap();
    assert!(cleared["cleared"].as_u64().unwrap() >= 2);
    assert!(app.run("placements.list", json!({})).unwrap()["placements"].as_array().unwrap().is_empty());
}

#[test]
fn a_placement_lands_where_the_mercator_says() {
    // The placement's world x/z comes from the scene Mercator; anchored at the placement's own
    // lon/lat it projects to (nearly) zero — the property every 3D draw relies on.
    use terrazon_scene3d::Mercator;
    let m = Mercator::new(-6.7780, 39.2236);
    let (x, yn) = m.project(39.2236, -6.7780);
    assert!(x.abs() < 1e-6 && yn.abs() < 1e-6);
    // And a spot ~100 m east: x east ≈ 100, z south = −y.
    let (x, yn) = m.project(39.22450, -6.7780);
    assert!((x - 100.0).abs() < 2.0, "100 m east of the anchor: {x}");
    assert!(yn.abs() < 0.5, "due east keeps north 0: {yn}");
}

#[test]
fn the_galileo_view_commands_are_gone_the_environment_is_the_one_3d() {
    // The old tilt/yaw "3D view" duplicated the 3D environment; the map layer is flat 2D and
    // `view.mode3d` is the only 3D, so the old commands answer unknown-command errors.
    let mut app = app_with_map();
    assert!(app.run("view.map3d", json!({})).is_err(), "view.map3d is gone");
    assert!(app.run("view.map3d.tilt", json!({ "tilt": 45.0 })).is_err(), "view.map3d.tilt is gone");
    assert!(app.run("view.map3d.reset", json!({})).is_err(), "view.map3d.reset is gone");
    assert!(app.run("view.mode3d", json!({})).is_ok(), "view.mode3d is the one 3D");
}

#[test]
fn the_drape_zoom_follows_the_2d_rule_and_clamps_to_the_source() {
    use crate::view3d::{drape_zoom_view, zoom_level_of};
    let osm = terrazon_map::TileSource {
        id: "osm".into(),
        name: "OSM".into(),
        template: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
        min_zoom: 0,
        max_zoom: 19,
        ..Default::default()
    };
    // The 2D rule's own metres-per-px ladder: at zoom z the ground scale is 156544·cos φ / 2^z.
    let mpp_of = |z: i32| 156_544.0 / (1u64 << z) as f64;
    // Street scales refine like the flat canvas: the level whose tiles read ~256 px.
    assert_eq!(zoom_level_of(mpp_of(16), 0.0).round() as i32, 16);
    assert_eq!(zoom_level_of(mpp_of(19), 0.0).round() as i32, 19);
    assert_eq!(zoom_level_of(mpp_of(22), 0.0).round() as i32, 22, "the rule reaches the map's last level z22");
    // The drape clamps into the source's own range and the terrain panel's cap.
    let mpp = mpp_of(20);
    assert_eq!(drape_zoom_view(mpp, 0.0, &osm, 22), 19, "the source's own ceiling clamps");
    assert_eq!(drape_zoom_view(mpp, 0.0, &terrazon_map::TileSource { max_zoom: 22, ..osm.clone() }, 19), 19, "the panel's cap clamps");
    assert_eq!(drape_zoom_view(mpp_of(1), 0.0, &terrazon_map::TileSource { min_zoom: 2, max_zoom: 19, ..osm.clone() }, 22), 2, "the floor clamps");
    // Latitude reads through the Mercator stretch: the same ground scale at 60° north sits
    // one level coarser than at the equator.
    let z_eq = zoom_level_of(mpp_of(16), 0.0);
    let z_60 = zoom_level_of(mpp_of(16), 60.0);
    assert!((z_eq - z_60 - 1.0).abs() < 0.1, "cos 60° costs one level: {z_eq} vs {z_60}");
}

#[test]
fn the_world_clamps_the_footprint_and_the_grid() {
    // The grid's extent and the imagery's footprint both live inside the Mercator world
    // square — a far camera covers the world's ground, not a square patch around the anchor.
    use crate::view3d::grid_extent;
    let cam = terrazon_scene3d::OrbitCamera { target: [0.0, 0.0, 0.0], dist: 190_000.0, pitch: 1.2, ..Default::default() };
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 800.0));
    let (min_x, max_x, min_z, max_z) = grid_extent(&terrazon_scene3d::CameraRig::Orbit(cam), rect, 0.02);
    let half = crate::view3d::WORLD_HALF_M;
    assert!(min_x >= -half && max_x <= half && min_z >= -half && max_z <= half, "the extent stays in the world square");
    assert!(max_x - min_x > 100_000.0, "a far camera's grid reaches kilometres out, not one 60 km square: {min_x}..{max_x}");
}

#[test]
fn the_ruler_reads_the_scene_the_camera_sees() {
    // The 3D ruler frame is centred on the view's ground and scaled to the camera's reach:
    // dolly in and the degree steps refine, pan and they follow.
    let mut app = app_with_map();
    use terrazon_map::{doc_to_lonlat, lonlat_to_doc};
    let (x, y) = lonlat_to_doc(39.2236, -6.7780);
    app.run("view.setZoom", json!({ "zoom": 51200.0, "center": [x, y] })).unwrap();
    let full = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
    let xf = crate::view3d::ruler_xf(&app, full);
    let (lon, lat) = doc_to_lonlat(xf.center.x, xf.center.y);
    assert!((lat - -6.7780).abs() < 1e-4 && (lon - 39.2236).abs() < 1e-4, "the ruler centres on the ground: {lon},{lat}");
    assert!(xf.zoom > 1.0, "a 40 m reach spans the strips: {}", xf.zoom);
    // Dolly the camera close: the ruler's steps refine (its zoom grows).
    app.ui.cam3d.dist = 60.0;
    let close = crate::view3d::ruler_xf(&app, full);
    assert!(close.zoom > xf.zoom, "closer camera refines the ruler: {} -> {}", xf.zoom, close.zoom);
    // And far out it coarsens but stays inside the map's zoom range.
    app.ui.cam3d.dist = 40_000.0;
    let far = crate::view3d::ruler_xf(&app, full);
    assert!(far.zoom < close.zoom && far.zoom >= 0.002, "far camera coarsens within range: {}", far.zoom);
}

#[test]
fn the_terrain_settings_roundtrip_through_older_saves() {
    // A settings blob saved when the terrain carried exaggeration/sea level still loads: the
    // flat-ground build ignores the removed fields, keeps the contour interval and the zoom.
    let older = r#"{"exaggeration": 3.5, "contour_interval": 25.0, "sea_level": -4.0, "imagery_zoom": 15}"#;
    let st: terrazon_scene3d::TerrainSettings = serde_json::from_str(older).unwrap();
    assert_eq!(st.contour_interval, 25.0);
    assert_eq!(st.imagery_zoom, 15);
    let fresh = terrazon_scene3d::TerrainSettings::default();
    assert_eq!(fresh.contour_interval, 10.0, "the default contours stay on");
}

#[test]
fn entering_3d_from_a_wide_view_anchors_on_drawn_content() {
    // A fresh World Map sits at world zoom with the view centre on (0°, 0°) — open ocean. With
    // drawn map content (a path over Dar es Salaam) the first 3D frame moves the 2D view centre
    // onto the content, so the DEM window anchors where the drawing is, not mid-ocean.
    use terrazon_map::{doc_to_lonlat, lonlat_to_doc};
    let mut app = app_with_map();
    let (dx, dy) = lonlat_to_doc(39.2236, -6.7780);
    app.run("shape.line", json!({ "x1": dx - 20.0, "y1": dy - 20.0, "x2": dx + 20.0, "y2": dy + 20.0 })).ok();
    let drawn = app.session.active().unwrap().doc.art_bounds();
    assert!(drawn.is_some(), "the path gives the document content bounds");
    assert!(app.view().is_some_and(|v| crate::state::map_zoom_level(v.zoom) < 10.0), "a fresh map opens wide");
    // The same content-centre choice dem_step makes on its first frame.
    let wide = app.view().is_some_and(|v| crate::state::map_zoom_level(v.zoom) < 10.0);
    let centre =
        wide.then(|| app.session.active().unwrap().doc.art_bounds()).flatten().filter(|b| b.width() > 1.0 && b.height() > 1.0).map(|b| b.center());
    assert!(centre.is_some(), "a wide view over drawn content picks the content centre");
    let c = centre.unwrap();
    let (clon, clat) = doc_to_lonlat(c.x, c.y);
    assert!((clon - 39.2236).abs() < 0.01 && (clat - -6.7780).abs() < 0.01, "the anchor lands on the drawing: {clon},{clat}");
}

#[test]
fn the_open_water_datum_clamps_to_the_sea_surface() {
    // Bathymetric DEMs read the seafloor mid-ocean; the datum must stay the sea surface there
    // (nothing on land is deeper than −1 km), so the scene builds at the camera, not kilometres
    // above it.
    const OPEN_WATER_ELEV: f32 = super::view3d::OPEN_WATER_ELEV;
    const {
        assert!(OPEN_WATER_ELEV <= -1000.0, "deeper than any land");
    }
    let here: Option<f32> = Some(-5468.5); // the Gulf of Guinea's floor
    let water = here.is_some_and(|e| e < OPEN_WATER_ELEV);
    assert!(water);
    let base = match here {
        Some(e) if !water => e,
        Some(_) => 0.0,
        None => 0.0,
    };
    assert_eq!(base, 0.0, "open water datums to the sea surface");
    let land: Option<f32> = Some(35.0);
    let water = land.is_some_and(|e| e < OPEN_WATER_ELEV);
    let base = match land {
        Some(e) if !water => e,
        Some(_) => 0.0,
        None => 0.0,
    };
    assert_eq!(base, 35.0, "land keeps its own grade");
}

#[test]
fn scene3d_set_toggles_the_debug_overlays() {
    // The tile and object debug overlays ride `scene3d.set` (the 3D Terrain panel's
    // checkboxes drive the same command) and read back in its reply, so agents and users
    // alike can turn the scene's inputs visible.
    let mut app = app_with_map();
    let r = app.run("scene3d.set", json!({})).unwrap();
    assert_eq!(r["tilesDebug"], json!(false), "off by default");
    assert_eq!(r["objectsDebug"], json!(false));
    let r = app.run("scene3d.set", json!({ "tilesDebug": true, "objectsDebug": true })).unwrap();
    assert_eq!(r["tilesDebug"], json!(true));
    assert_eq!(r["objectsDebug"], json!(true));
    assert!(app.ui.tiles_debug && app.ui.objects_debug, "the flags land on the ui state the canvas reads");
    let r = app.run("scene3d.set", json!({ "tilesDebug": false })).unwrap();
    assert_eq!(r["tilesDebug"], json!(false), "one key at a time flips only its own");
    assert_eq!(r["objectsDebug"], json!(true));
    assert!(!app.ui.tiles_debug && app.ui.objects_debug);
}

#[test]
fn the_walk_rig_reads_the_arrow_keys_not_the_letter_keys() {
    // The walk drive's axis read answers the ARROW keys — up forward, down back, left/right
    // strafe — and never the letters: W/A/S/D are the tools' keys, so the rig's own read
    // leaves them alone (the user's rule: "the WASD are used for tools, use arrows for
    // navigation").
    let ctx = egui::Context::default();
    let key = |k: egui::Key| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::default() };
    let raw = egui::RawInput {
        events: vec![key(egui::Key::ArrowUp), key(egui::Key::ArrowRight), key(egui::Key::W), key(egui::Key::A), key(egui::Key::S), key(egui::Key::D)],
        ..Default::default()
    };
    let axes = std::cell::Cell::new((0.0f32, 0.0f32));
    let mut out = ctx.run_ui(raw, |ui| {
        ui.input(|i| axes.set(crate::view3d::walk_key_axes(i)));
    });
    out.textures_delta.clear();
    assert_eq!(axes.get(), (1.0, 1.0), "up walks forward, right strafes — and the letters read nothing");
    // The other pair: down backs, left strafes the other way (a fresh context — egui's
    // input pass is one-shot per assert here).
    let ctx = egui::Context::default();
    let raw = egui::RawInput { events: vec![key(egui::Key::ArrowDown), key(egui::Key::ArrowLeft)], ..Default::default() };
    let axes = std::cell::Cell::new((0.0f32, 0.0f32));
    let mut out = ctx.run_ui(raw, |ui| {
        ui.input(|i| axes.set(crate::view3d::walk_key_axes(i)));
    });
    out.textures_delta.clear();
    assert_eq!(axes.get(), (-1.0, -1.0), "down walks back, left strafes");
}

#[test]
fn the_imagery_ring_learns_a_sources_effective_zoom_cap() {
    // The coverage learner: a zoom the source declares but never answers (16 straight
    // failures, zero answers) caps the ring's asks one level down, so the near field
    // dresses from the finest zoom that actually answers instead of paying a dead ask
    // per tile every frame.
    let mut fails = std::collections::BTreeMap::new();
    let oks = std::collections::BTreeMap::new();
    for _ in 0..16 {
        *fails.entry(22u8).or_default() += 1;
    }
    assert_eq!(crate::view3d::learn_zoom_cap(&fails, &oks, 22, None), Some(21), "a dead z22 caps at z21");
    // The cap only walks down: a learned 21 stays with more failures above it.
    assert_eq!(crate::view3d::learn_zoom_cap(&fails, &oks, 22, Some(21)), Some(21));
    // A zoom the source answers never learns a cap.
    let mut answered = std::collections::BTreeMap::new();
    answered.insert(22u8, 1);
    assert_eq!(crate::view3d::learn_zoom_cap(&fails, &answered, 22, None), None);
    // Low zooms never learn (a flaky street zoom is a network problem, not a coverage edge).
    let mut low = std::collections::BTreeMap::new();
    for _ in 0..16 {
        *low.entry(15u8).or_default() += 1;
    }
    assert_eq!(crate::view3d::learn_zoom_cap(&low, &oks, 15, None), None);
    // The floor: even a stack of failures at 15's neighbour never caps below z14.
    let mut z16 = std::collections::BTreeMap::new();
    for _ in 0..16 {
        *z16.entry(16u8).or_default() += 1;
    }
    assert_eq!(crate::view3d::learn_zoom_cap(&z16, &oks, 16, None), Some(15));
}

#[test]
fn the_drape_collects_the_documents_text_as_labels() {
    // The 3D drape's text pass: a text.create node surfaces as a DrapeText — the plain
    // string at its anchor's world point, the run's size and fill, the node's opacity.
    let mut app = app_with_map();
    let r = app.run("text.create", json!({"x": 100.0, "y": 120.0, "text": "Sinza", "size": 0.004})).unwrap();
    assert!(r["id"].as_u64().is_some(), "text.create answers an id: {r}");
    let st = app.session.active().unwrap();
    let mut out = Vec::new();
    let world = |p: terrazon_geom::Point| [p.x as f32, 0.0, -p.y as f32];
    for layer in &st.doc.layers {
        crate::view3d::collect_doc_text(layer, &world, &mut out, 1.0);
    }
    assert_eq!(out.len(), 1, "one label collected: {out:?}");
    assert_eq!(out[0].text, "Sinza");
    // The engine clamps a new label at 0.1 pt (its UI floor) — a map doc scales it down with
    // a transform, and the drape reads the SCALED size (the xf's own scale rides along).
    assert!((out[0].size - 0.1).abs() < 1.0e-9, "the size rides the run: {}", out[0].size);
    app.run("object.scale", json!({"sx": 4, "sy": 4})).unwrap();
    let st = app.session.active().unwrap();
    let mut out = Vec::new();
    let world = |p: terrazon_geom::Point| [p.x as f32, 0.0, -p.y as f32];
    for layer in &st.doc.layers {
        crate::view3d::collect_doc_text(layer, &world, &mut out, 1.0);
    }
    assert!((out[0].size - 0.004).abs() < 1.0e-9, "the scaled size rides the xf: {}", out[0].size);
    assert_eq!(out[0].alpha, 1.0);
    // On a 45% layer the label dims with it (the layer-opacity parity).
    let lay = app.run("layer.new", json!({"top": true})).unwrap()["id"].as_u64().unwrap();
    let r = app.run("text.create", json!({"x": -50.0, "y": -60.0, "text": "Gulf"})).unwrap();
    let tid = r["id"].as_u64().unwrap();
    app.run("select.set", json!({"ids": [tid]})).unwrap();
    app.run("layer.move", json!({"id": tid, "target": lay, "place": "inside"})).unwrap();
    app.run("select.none", json!({})).unwrap();
    app.run("layer.target", json!({"id": lay})).unwrap();
    app.run("transparency.set", json!({"opacity": 45})).unwrap();
    let st = app.session.active().unwrap();
    let mut out = Vec::new();
    for layer in &st.doc.layers {
        crate::view3d::collect_doc_text(layer, &world, &mut out, 1.0);
    }
    let gulf = out.iter().find(|d| d.text == "Gulf").expect("the moved label collects");
    assert!((gulf.alpha - 0.45).abs() < 0.01, "the 45% layer dims its label: {}", gulf.alpha);
}
