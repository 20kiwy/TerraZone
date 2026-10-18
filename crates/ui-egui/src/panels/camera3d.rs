//! The 3D Camera panel — the orbit camera, one panel of the four the 3D environment keeps
//! (Terrain / Scene 3D / Assets 3D / Camera 3D): the yaw and pitch, the distance, the presets and
//! the frame-the-terrain button.

use egui::{Ui, vec2};
/// The panel id (the icon column and the Window menu key it by).
pub const ID: &str = "camera3d";

use crate::theme::Tokens;
use crate::{TerrazoneApp, widgets};

pub fn show(app: &mut TerrazoneApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    widgets::subheader(ui, tl!("3D Camera"));
    let Some(_) = app.view() else {
        super::empty_state(ui, "rotate-3d", tl!("No document"), tl!("Open a map document to fly its 3D environment."));
        return;
    };
    let mut changed = false;
    let mut yaw_deg = app.ui.cam3d.yaw.to_degrees();
    let mut pitch_deg = app.ui.cam3d.pitch.to_degrees();
    // Distance on a log scale: a city block to a region.
    let mut dist_t = (app.ui.cam3d.dist).clamp(10.0, 60_000.0).ln() / 60_000f32.ln();
    ui.add_space(2.0);
    changed |= ui.add(egui::Slider::new(&mut yaw_deg, 0.0..=360.0).text(tl!("Orbit"))).changed();
    changed |= ui.add(egui::Slider::new(&mut pitch_deg, 3.0..=89.0).text(tl!("Pitch"))).changed();
    changed |= ui.add(egui::Slider::new(&mut dist_t, 0.0..=1.0).text(tl!("Distance"))).changed();
    if changed {
        app.ui.cam3d.yaw = yaw_deg.to_radians();
        app.ui.cam3d.pitch = pitch_deg.to_radians();
        app.ui.cam3d.dist = (dist_t * 60_000f32.ln()).exp().clamp(10.0, 60_000.0);
        if let Some(rt) = app.canvas.dem.as_mut() {
            rt.interacted = true;
        }
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new(tl!("Presets")).size(11.5).color(t.text));
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        if preset_button(ui, tl!("Aerial")) {
            set(&mut app.ui.cam3d, 0.62, 35.0, 2_400.0);
        }
        if preset_button(ui, tl!("Street")) {
            set(&mut app.ui.cam3d, 0.30, 6.0, 260.0);
        }
        if preset_button(ui, tl!("Top-down")) {
            let yaw = app.ui.cam3d.yaw;
            set(&mut app.ui.cam3d, yaw, 88.0, 4_000.0);
        }
        if preset_button(ui, tl!("Fit terrain"))
            && let Some(rt) = app.canvas.dem.as_ref()
            && let Some(g) = rt.grid.as_ref()
        {
            let half = g.w.min(g.h) as f32 * g.cell * 0.5;
            app.ui.cam3d.target = [0.0, 0.0, 0.0];
            app.ui.cam3d.dist = (half * 2.4).clamp(200.0, 60_000.0);
            app.ui.cam3d.pitch = 0.62;
            if let Some(rt) = app.canvas.dem.as_mut() {
                rt.interacted = true;
            }
        }
    });
    ui.add_space(6.0);
    widgets::dim_label(ui, tl!("Drag orbits on the canvas, Shift-drag pans, the wheel zooms, double-click recentres."));
}

fn preset_button(ui: &mut Ui, label: &str) -> bool {
    crate::widgets::secondary_button(ui, label).clicked()
}

fn set(cam: &mut terrazon_scene3d::OrbitCamera, yaw: f32, pitch_deg: f32, dist: f32) {
    cam.yaw = yaw;
    cam.pitch = pitch_deg.to_radians();
    cam.dist = dist;
}
