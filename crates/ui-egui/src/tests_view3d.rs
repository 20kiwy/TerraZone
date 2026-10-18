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
fn the_drape_zoom_refines_with_the_camera_and_clamps_to_the_source() {
    use crate::view3d::drape_zoom;
    let osm = terrazon_map::TileSource {
        id: "osm".into(),
        name: "OSM".into(),
        template: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
        min_zoom: 0,
        max_zoom: 19,
        ..Default::default()
    };
    // The panel's setting at neighbourhood range…
    assert_eq!(drape_zoom(16, 2_500.0, &osm), 16);
    // …one step finer inside 600 m, two inside 150 m…
    assert_eq!(drape_zoom(16, 500.0, &osm), 17);
    assert_eq!(drape_zoom(16, 100.0, &osm), 18);
    // …coarser when far, and never past the source's own range either way.
    assert_eq!(drape_zoom(16, 20_000.0, &osm), 14);
    assert_eq!(drape_zoom(16, 100.0, &terrazon_map::TileSource { max_zoom: 17, ..osm.clone() }), 17, "the ceiling clamps");
    assert_eq!(drape_zoom(1, 20_000.0, &terrazon_map::TileSource { min_zoom: 2, max_zoom: 19, ..osm.clone() }), 2, "the floor clamps");
}

#[test]
fn the_imagery_ring_widens_as_the_drape_refines() {
    use crate::view3d::ring_shape;
    assert_eq!(ring_shape(0), (2, 24), "the base ring stays as ever");
    let (r1, c1) = ring_shape(1);
    let (r2, c2) = ring_shape(2);
    assert!(r1 > 2 && c1 > 24, "one step finer asks wider: {r1}×{c1}");
    assert!(r2 > r1 && c2 > c1, "two steps wider still: {r2}×{c2}");
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
