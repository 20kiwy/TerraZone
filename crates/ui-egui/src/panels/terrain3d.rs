//! The 3D Terrain panel — the DEM environment's settings: the contour interval (the one
//! terrain render left — the ground drapes flat, the relief reads from the contour lines and
//! their height labels) and the imagery zoom, with the DEM window's live status.

use egui::Ui;
use serde_json::json;

/// The panel id (the icon column and the Window menu key it by).
pub const ID: &str = "terrain3d";

use crate::theme::Tokens;
use crate::{TerrazoneApp, widgets};

/// One DEM window: a `DEM_TILES²` block of Terrarium tiles around the scene centre.
const DEM_TILES: usize = terrazon_scene3d::dem::DEM_TILES as usize * terrazon_scene3d::dem::DEM_TILES as usize;

pub fn show(app: &mut TerrazoneApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    widgets::subheader(ui, tl!("3D Terrain"));
    let Some(_) = app.view() else {
        super::empty_state(ui, "globe", tl!("No document"), tl!("Open a map document to shape its 3D terrain."));
        return;
    };

    // Live DEM status (the same numbers the 3D viewport's readout shows).
    match app.canvas.dem.as_ref() {
        None => {
            widgets::dim_label(ui, tl!("The terrain starts when the 3D environment opens."));
        }
        Some(rt) => {
            match (rt.loader.as_ref(), rt.grid.as_ref()) {
                (Some(loader), Some(grid)) => {
                    let done = loader.ready_count() == DEM_TILES;
                    let line = if done {
                        format!("{} {}/{} · {}×{} · {:.0} m/px", tl!("Terrain"), loader.ready_count(), DEM_TILES, grid.w, grid.h, grid.cell)
                    } else {
                        format!("{} {}/{}…", tl!("Terrain loading"), loader.ready_count(), DEM_TILES)
                    };
                    ui.label(egui::RichText::new(line).size(11.5).color(t.text_dim));
                }
                (Some(loader), None) => {
                    ui.label(
                        egui::RichText::new(format!("{} {}/{}…", tl!("Terrain loading"), loader.ready_count(), DEM_TILES))
                            .size(11.5)
                            .color(t.text_dim),
                    );
                    widgets::dim_label(ui, tl!("The elevation tiles (AWS Terrarium, no key) download once and cache on disk."));
                }
                (None, _) => {
                    widgets::dim_label(ui, tl!("no DEM fetcher"));
                }
            }
            ui.add_space(6.0);
        }
    }

    let mut interval = app.ui.terrain3d.contour_interval;
    let mut changed = ui.add(egui::Slider::new(&mut interval, 0.0..=100.0).text(tl!("Contours (m)"))).changed();
    let mut zoom = i64::from(app.ui.terrain3d.imagery_zoom);
    changed |= ui.add(egui::Slider::new(&mut zoom, 10..=22).text(tl!("Imagery zoom"))).changed();

    if changed {
        let _ = app.run("scene3d.set", json!({ "contourInterval": interval, "imageryZoom": zoom }));
    }
    ui.add_space(6.0);
    widgets::dim_label(ui, tl!("The ground drapes the basemap flat; contours (0 = off) draw the relief with their heights."));
}
