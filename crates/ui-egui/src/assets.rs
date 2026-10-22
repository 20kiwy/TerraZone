//! The 2D placed assets: one vector glyph per placement, drawn the way the fleet markers are —
//! over the basemap, under the fleet and the art. The glyph is the catalogue's silhouette (the
//! same shapes the 3D meshes speak), scaled by the placement's real-world footprint: a tree is
//! four metres wide whether the map is at street level or at world view.

use egui::{Color32, Pos2, Rect, Shape, pos2};
use terrazon_geom::Point;

use crate::TerrazoneApp;

/// The 2D glyph's smallest on-screen size (px) at street zooms: below it the footprint's true
/// scale would hide the placement entirely. The floor reads the fleet's zoom curve — far views
/// scale it down so a wide map doesn't read a wall of glyphs (a 6 px floor keeps them findable).
const GLYPH_FLOOR: f64 = 16.0;
/// The floor's lower bound at the widest zooms.
const GLYPH_FLOOR_MIN: f64 = 6.0;

/// Ground metres → document points (the Mercator span; see the Assets tool's note on latitude).
fn doc_per_metre() -> f64 {
    terrazon_map::WORLD_PT / 40_075_016.686
}

/// The placements' glyphs. Nothing here gates on the fleet machinery — the placements are
/// document data; if anything is placed it draws.
pub fn draw(app: &TerrazoneApp, painter: &egui::Painter, rect: Rect, xf: &crate::canvas::Xf) {
    let Some(doc) = app.session.active().map(|st| &st.doc) else { return };
    if doc.placements.is_empty() {
        return;
    }
    let mut shapes: Vec<Shape> = Vec::new();
    // The zoom's scale: the same curve the fleet's markers read, so every overlay breathes
    // with the zoom together.
    let level = crate::state::map_zoom_level(xf.zoom);
    let floor = (GLYPH_FLOOR * crate::fleet::marker_scale(level) as f64).max(GLYPH_FLOOR_MIN);
    for p in &doc.placements.placements {
        let centre_doc = Point::new(p.x, p.y);
        let centre = xf.to_screen(centre_doc);
        // The footprint in screen pixels: metres → doc points → screen. Below the icon floor
        // the GLYPH holds the floor (the fleet's sizing ladder does the same): a 4 m tree is
        // 1.7 px at z16 — true to the world, but impossible to place by eye; the floor keeps
        // the placement readable at every zoom, growing to true scale as the map zooms in.
        let [w, d] = p.size_m();
        let w_px = w as f64 * doc_per_metre() * xf.zoom;
        let d_px = d as f64 * doc_per_metre() * xf.zoom;
        let size_px = w_px.max(d_px).max(floor);
        if !rect.expand(size_px.max(40.0) as f32).contains(centre) {
            continue;
        }
        let scale_px = size_px;
        // A camera draws its view wedge UNDER the glyph: the ground footprint of the cone it
        // looks down (the horizontal fov, opening along the look rotation), filled translucent
        // — the same wedge the 3D environment stands as its frustum.
        if p.kind.is_camera() {
            shapes.extend(camera_wedge(p, centre, doc_per_metre() * xf.zoom));
        }
        // Rotation is degrees clockwise from north: the glyph is unit-space y-up, the screen
        // y-down, so north is −y and clockwise turns east first.
        let rad = (p.rotation_deg as f64).to_radians();
        let (sin, cos) = rad.sin_cos();
        for part in terrazon_assets::glyph(p.kind) {
            let pts: Vec<Pos2> = part
                .pts
                .iter()
                .map(|(gx, gy)| {
                    // Unit space: x spans the footprint (±0.5), y the height from the ground
                    // up — one unit = the floored on-screen size.
                    let ux = (*gx) as f64 * scale_px;
                    let uy = (*gy) as f64 * scale_px;
                    let (rx, ry) = (ux * cos - uy * sin, ux * sin + uy * cos);
                    pos2(centre.x + rx as f32, centre.y - ry as f32)
                })
                .collect();
            let [r, g, b] = part.color;
            shapes.push(Shape::Path(egui::epaint::PathShape {
                points: pts,
                closed: true,
                fill: Color32::from_rgba_unmultiplied(r, g, b, 235),
                stroke: egui::Stroke::new(0.8, Color32::from_black_alpha(70)).into(),
            }));
        }
    }
    painter.extend(shapes);
}

/// A camera placement's view wedge on the 2D map: the ground footprint of the cone the camera
/// looks down from its mast-top eye — the near and far edges at the fov's own reaches, the
/// sides opening at `tan(fov/2)` (a flat-ground frustum footprint, the same wedge the 3D
/// environment stands). `m_px` is screen px per ground metre at this zoom; the wedge follows
/// the aim honestly (looking down lands close, looking near the horizon runs far) and clamps
/// so a near-horizon aim doesn't draw a wedge kilometres long.
fn camera_wedge(p: &terrazon_assets::AssetPlacement, centre: egui::Pos2, m_px: f64) -> Vec<Shape> {
    if m_px <= 0.0 {
        return vec![];
    }
    let half = f64::from(p.fov_deg.max(4.0) * 0.5).to_radians();
    let pitch = f64::from(p.pitch_deg.clamp(-85.0, 85.0)).to_radians();
    let eye = f64::from(p.eye_height_m());
    // The ground reach of a ray pitched `down` below the horizon: eye / tan(down); a horizon
    // ray (tan ≤ ~0) clamps to the wedge's far bound. The cone's edges sit at the centre
    // ray's down-angle ± the half-fov: the steeper edge lands NEAR, the shallower edge FAR —
    // a ray at or above the horizon clamps (the footprint runs to the wedge's far bound).
    let reach = |down: f64| if down <= 0.02 { WEDGE_FAR_M } else { (eye / down.tan()).clamp(0.5, WEDGE_FAR_M) };
    let down_c = -pitch; // radians below the horizon (positive = looking down)
    let near = reach(down_c + half);
    let far = reach(down_c - half).max(near + 0.5);
    let spread = half.tan();
    let dir = f64::from(p.rotation_deg.max(0.0)).to_radians();
    let ground = |ahead_m: f64, side_m: f64| -> egui::Pos2 {
        // Ground space: ahead along the look, side across it — metres to screen px. The
        // rotation is degrees clockwise from north: the heading's screen direction is
        // (sin θ, −cos θ), its right hand (cos θ, sin θ) — east at 90°, matching the 3D
        // environment's aim (the placements' clockwise-from-above convention).
        let (sin, cos) = dir.sin_cos();
        let sx = (ahead_m * sin + side_m * cos) * m_px;
        let sy = (-ahead_m * cos + side_m * sin) * m_px;
        egui::pos2((centre.x as f64 + sx) as f32, (centre.y as f64 + sy) as f32)
    };
    let a = ground(near, -near * spread);
    let b = ground(far, -far * spread);
    let c = ground(far, far * spread);
    let d = ground(near, near * spread);
    let fill = Color32::from_rgba_unmultiplied(0xd4, 0x8a, 0x2a, 46);
    let stroke = egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xd4, 0x8a, 0x2a, 150));
    vec![Shape::Path(egui::epaint::PathShape { points: vec![a, b, c, d], closed: true, fill, stroke: stroke.into() })]
}

/// The wedge's far clamp (metres): past this the cone is a smear on the map.
const WEDGE_FAR_M: f64 = 120.0;
