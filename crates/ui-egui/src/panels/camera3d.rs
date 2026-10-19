//! The 3D Camera panel — the camera modes of the EffectCraft viewer, one panel of the four the
//! 3D environment keeps (Terrain / Scene 3D / Assets 3D / Camera 3D): Orbit or Walk, the orbit
//! arm's yaw/pitch/distance and presets, the walk rig's look and height, and the frame-the-
//! terrain button.

use egui::{Ui, vec2};
/// The panel id (the icon column and the Window menu key it by).
pub const ID: &str = "camera3d";

use terrazon_scene3d::CamMode;

use crate::theme::Tokens;
use crate::{TerrazoneApp, view3d, widgets};

pub fn show(app: &mut TerrazoneApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    widgets::subheader(ui, tl!("3D Camera"));
    let Some(_) = app.view() else {
        super::empty_state(ui, "rotate-3d", tl!("No document"), tl!("Open a map document to fly its 3D environment."));
        return;
    };
    // The mode pair (the EffectCraft viewer's): Orbit swings the arm, Walk drives the rig.
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        if mode_button(ui, tl!("Orbit"), app.ui.cam_mode == CamMode::Orbit) {
            view3d::set_cam_mode(app, CamMode::Orbit);
        }
        if mode_button(ui, tl!("Walk"), app.ui.cam_mode == CamMode::Walk) {
            view3d::set_cam_mode(app, CamMode::Walk);
        }
    });
    ui.add_space(6.0);
    match app.ui.cam_mode {
        CamMode::Orbit => orbit_controls(app, ui, &t),
        CamMode::Walk => walk_controls(app, ui, &t),
    }
    ui.add_space(6.0);
}

/// The orbit arm's controls: yaw, pitch, the log-scaled distance, the presets and the
/// frame-the-terrain button — as ever.
fn orbit_controls(app: &mut TerrazoneApp, ui: &mut Ui, t: &Tokens) {
    let mut changed = false;
    let mut yaw_deg = app.ui.cam3d.yaw.to_degrees();
    let mut pitch_deg = app.ui.cam3d.pitch.to_degrees();
    // Distance on a log scale: a city block to a region.
    let mut dist_t = (app.ui.cam3d.dist).clamp(10.0, 60_000.0).ln() / 60_000f32.ln();
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

/// The walk rig's controls (the EffectCraft camera): the look yaw/pitch, the eye height, and
/// the drive keys spelled out.
fn walk_controls(app: &mut TerrazoneApp, ui: &mut Ui, _t: &Tokens) {
    let mut changed = false;
    let mut yaw_deg = app.ui.walk3d.yaw.to_degrees();
    let mut pitch_deg = app.ui.walk3d.pitch.to_degrees();
    // Eye height on a log scale: street level to a flight level.
    let mut height_t = (app.ui.walk3d.pos[1]).clamp(0.4, 20_000.0).ln() / 20_000f32.ln();
    changed |= ui.add(egui::Slider::new(&mut yaw_deg, 0.0..=360.0).text(tl!("Look"))).changed();
    changed |= ui.add(egui::Slider::new(&mut pitch_deg, -83.0..=83.0).text(tl!("Pitch"))).changed();
    changed |= ui.add(egui::Slider::new(&mut height_t, 0.0..=1.0).text(tl!("Eye height"))).changed();
    if changed {
        app.ui.walk3d.yaw = yaw_deg.to_radians();
        app.ui.walk3d.pitch = pitch_deg.to_radians();
        app.ui.walk3d.pos[1] = (height_t * 20_000f32.ln()).exp().clamp(0.4, 20_000.0);
        if let Some(rt) = app.canvas.dem.as_mut() {
            rt.interacted = true;
        }
    }
    ui.add_space(6.0);
    widgets::dim_label(ui, tl!("W/A/S/D walks · Q/E sinks or lifts · Shift runs · the wheel lifts · drag with Space (or Alt) looks"));
}

fn preset_button(ui: &mut Ui, label: &str) -> bool {
    crate::widgets::secondary_button(ui, label).clicked()
}

/// One of the two mode buttons — a selectable chip that reads pressed while its mode is up.
fn mode_button(ui: &mut Ui, label: &str, active: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let fill = if active { t.selection.gamma_multiply(0.45) } else { egui::Color32::TRANSPARENT };
    ui.add(egui::Button::new(egui::RichText::new(label).size(12.5)).fill(fill).min_size(vec2(64.0, 22.0))).clicked()
}

fn set(cam: &mut terrazon_scene3d::OrbitCamera, yaw: f32, pitch_deg: f32, dist: f32) {
    cam.yaw = yaw;
    cam.pitch = pitch_deg.to_radians();
    cam.dist = dist;
}
