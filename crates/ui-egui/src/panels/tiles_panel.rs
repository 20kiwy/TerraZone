//! The dock's Tiles panel: the document's map at hand, QGIS-style, in three tabs.
//!
//! - **Tiles**: every tile source — the built-ins (OpenStreetMap, Esri, OpenTopoMap, Google,
//!   Mapbox) and the user's connections — as a layer-style row: the eye on the far left shows or
//!   hides the tiles (an inactive source's eye makes it the basemap), the kind chip and the name
//!   follow, and the pencil and ⨯ sit on the right (⨯ only for a user connection). "Add
//!   Connection" makes a new one. Under it the pinned Tile Sub-layers: the eye, the lock, a
//!   pencil that opens the Edit tab and an ⨯ that unpins.
//! - **Style**: the sliders that style every tile as it draws, and the basemap's opacity, like a
//!   QGIS layer's properties.
//! - **Edit**: the tile editor the pencil opened — a pinned tile's data (name, source, z/x/y,
//!   opacity, style) or a connection's fields; a built-in shows read-only details and a way to
//!   duplicate it as an editable connection.

use egui::{Color32, Ui, vec2};
use serde_json::json;
use terrazon_map::{TileId, TileKind};

use crate::theme::{self, Tokens};
use crate::widgets::dim_label;
use crate::{TerrazoneApp, widgets};

/// The Tiles panel's tabs, in order.
const TABS: [&str; 3] = ["Tiles", "Style", "Edit"];

/// Which of the panel's three tabs shows. Kept across frames in the UI state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TilesPanelTab {
    /// The source list and the pinned tile sub-layers.
    #[default]
    Tiles,
    /// The basemap's style sliders.
    Style,
    /// The editor a row's pencil opened.
    Edit,
}

impl TilesPanelTab {
    fn at(self) -> usize {
        match self {
            Self::Tiles => 0,
            Self::Style => 1,
            Self::Edit => 2,
        }
    }

    fn from_at(i: usize) -> Self {
        match i {
            1 => Self::Style,
            2 => Self::Edit,
            _ => Self::Tiles,
        }
    }
}

/// What the Edit tab edits: a tile source (a user connection's fields, or a built-in's details)
/// or a pinned tile layer (its data). Set by a row's pencil; `None` shows the placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileEdit {
    Source(String),
    Layer(String),
}

pub fn show(app: &mut TerrazoneApp, ui: &mut Ui) {
    let Some(st) = app.session.active() else {
        dim_label(ui, tl!("Open a document to see its tiles."));
        return;
    };
    let map = st.doc.map.clone();

    // The panel's tabs; the dock header above carries the "Tiles" title.
    if let Some(i) = widgets::tab_bar(ui, &TABS, app.ui.tiles_tab.at()) {
        app.ui.tiles_tab = TilesPanelTab::from_at(i);
    }
    ui.add_space(4.0);
    match app.ui.tiles_tab {
        TilesPanelTab::Tiles => tiles_tab(app, ui, &map),
        TilesPanelTab::Style => style_tab(app, ui, &map),
        TilesPanelTab::Edit => edit_tab(app, ui, &map),
    }
}

/// The source list, the Add Connection form and the pinned tile sub-layers.
fn tiles_tab(app: &mut TerrazoneApp, ui: &mut Ui, map: &terrazon_map::MapSettings) {
    // Tiles on or off (the eye of the whole basemap).
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Basemap")).font(theme::semibold(12.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button(
                ui,
                if map.visible { "eye" } else { "eye-off" },
                if map.visible { tl!("Hide Tiles") } else { tl!("Show Tiles") },
                false,
                24.0,
            )
            .clicked()
            {
                let _ = app.run("map.showTiles", json!({ "on": !map.visible }));
            }
        });
    });
    ui.add_space(4.0);

    // One layer-style row per source: the eye far left, the kind chip, the name, then the pencil
    // and the ⨯. The active source's row is the highlighted one (its radio dot marks it); the eye
    // toggles the tiles or makes the row the basemap — a plain row click does nothing, QGIS-style.
    let cap = ui.available_width();
    for src in &map.sources {
        source_row(app, ui, map, src, cap);
        ui.add_space(1.0);
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    // Add Connection: a QGIS-style form — name, URL template, API key, attribution, zoom, kind.
    let open = app.ui.tiles_form;
    if widgets::icon_button(
        ui,
        if open { "chevron-down" } else { "plus" },
        if open { tl!("Hide Add Connection") } else { tl!("Add Connection") },
        false,
        24.0,
    )
    .clicked()
    {
        app.ui.tiles_form = !open;
    }
    if app.ui.tiles_form {
        add_connection(app, ui);
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    tile_sublayers(app, ui, map);

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
    cache_footer(app, ui);
}

/// The Tiles tab's cache footer: the tiles still downloading this second, then what the fetcher
/// kept on disk — the next session, a pan back, a restyle answer from there without the network —
/// with a Clear. Without a host probe (the web) only the download count shows.
fn cache_footer(app: &mut TerrazoneApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let pending = app.canvas.tiles.pending_count();
    if pending > 0 {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new(format!("{pending} {}", tl!("tiles loading"))).small());
        });
        ui.add_space(2.0);
    }
    let Some(probe) = app.services.tile_cache.clone() else { return };
    // The stats are a directory walk: take a fresh reading only when the line is missing, stale
    // past ten seconds, or right after a Clear — never every frame.
    let stale = app.ui.tile_cache_info.is_none_or(|(_, _, at)| at.elapsed() > std::time::Duration::from_secs(10));
    if stale {
        app.ui.tile_cache_info = probe.stats().map(|s| (s.tiles, s.bytes, std::time::Instant::now()));
    }
    let Some((tiles, bytes, _)) = app.ui.tile_cache_info else {
        return; // nothing cached yet (or a run without preferences): no line to show
    };
    ui.horizontal(|ui| {
        let (icon_r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
        crate::icons::paint(ui, "globe", icon_r, t.text_dim);
        ui.label(egui::RichText::new(format!("{} — {} {} · {}", tl!("Disk cache"), tiles, tl!("tiles on disk"), fmt_bytes(bytes))).small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button(ui, "trash-2", tl!("Clear cache"), false, 24.0).clicked() {
                probe.clear();
                app.ui.tile_cache_info = None;
                app.status(tl!("Tile cache cleared"));
            }
        });
    });
}

/// A byte count for people: "812 kB", "45.3 MB", "1.2 GB".
fn fmt_bytes(b: u64) -> String {
    let kb = 1024.0;
    let b = b as f64;
    if b >= kb * kb * kb {
        format!("{:.1} GB", b / (kb * kb * kb))
    } else if b >= kb * kb {
        format!("{:.1} MB", b / (kb * kb))
    } else if b >= kb {
        format!("{} kB", (b / kb).round() as u64)
    } else {
        format!("{b} B")
    }
}

/// The Tiles tab's source row: the eye far left, the kind chip, the name, the pencil and the ⨯.
fn source_row(app: &mut TerrazoneApp, ui: &mut Ui, map: &terrazon_map::MapSettings, src: &terrazon_map::TileSource, cap: f32) {
    let t = Tokens::get(ui.ctx());
    let active = src.id == map.active;
    let builtin = terrazon_map::builtin_id(&src.id);
    let (r, resp) = ui.allocate_exact_size(vec2(cap, 28.0), egui::Sense::click());
    ui.painter().rect_filled(
        r,
        4.0,
        if active {
            t.hover
        } else if resp.hovered() {
            t.panel_darker
        } else {
            Color32::TRANSPARENT
        },
    );
    let p = ui.painter_at(r);
    // The eye, on the far left: the active source's tiles on or off; an inactive source's eye
    // makes it the basemap (and switches the tiles back on).
    let eye_at = egui::Rect::from_center_size(egui::pos2(r.left() + 13.0, r.center().y), vec2(18.0, 18.0));
    let eye = ui.interact(eye_at, ui.id().with(("tiles-eye", &src.id)), egui::Sense::click());
    let shown = active && map.visible;
    crate::icons::paint(ui, if shown { "eye" } else { "eye-off" }, eye_at.shrink(3.0), if shown { t.icon } else { t.text_disabled });
    let run_eye = |app: &mut TerrazoneApp| {
        if active {
            let _ = app.run("map.showTiles", json!({ "on": !map.visible }));
        } else {
            let _ = app.run("map.setSource", json!({ "id": src.id }));
            if !map.visible {
                let _ = app.run("map.showTiles", json!({ "on": true }));
            }
        }
    };
    if eye.clicked() {
        run_eye(app);
    }
    eye.on_hover_text(if active { if map.visible { tl!("Hide Tiles") } else { tl!("Show Tiles") } } else { tl!("Make this the basemap") });
    // The kind chip and the name; the active row carries the radio dot.
    let (chip_col, chip_text) = kind_chip(&t, src.kind);
    p.text(egui::pos2(r.left() + 26.0, r.center().y), egui::Align2::LEFT_CENTER, chip_text, egui::FontId::proportional(13.0), chip_col);
    let (name_col, name_w) = if active { (t.text_strong, egui::FontId::proportional(12.5)) } else { (t.text_dim, egui::FontId::proportional(12.0)) };
    p.text(egui::pos2(r.left() + 44.0, r.center().y), egui::Align2::LEFT_CENTER, &src.name, name_w, name_col);
    if active {
        p.circle_filled(egui::pos2(r.right() - 62.0, r.center().y), 3.5, t.accent);
    }
    // The pencil: opens the Edit tab on this source (a built-in's shows its details and a
    // Duplicate action).
    let edit_at = egui::Rect::from_center_size(egui::pos2(r.right() - 40.0, r.center().y), vec2(18.0, 18.0));
    let edit = ui.interact(edit_at, ui.id().with(("tiles-edit", &src.id)), egui::Sense::click());
    let editing = matches!(&app.ui.tile_edit, Some(TileEdit::Source(id)) if id == &src.id);
    crate::icons::paint(ui, "pencil", edit_at.shrink(3.0), if editing { t.icon } else { t.text_dim });
    if edit.clicked() {
        app.ui.tiles_tab = TilesPanelTab::Edit;
        app.ui.tile_edit = Some(TileEdit::Source(src.id.clone()));
    }
    edit.on_hover_text(tl!("Edit tile source"));
    // The ⨯ removes a user connection; the built-ins stay.
    if !builtin {
        let rm_at = egui::Rect::from_center_size(egui::pos2(r.right() - 18.0, r.center().y), vec2(18.0, 18.0));
        let rm = ui.interact(rm_at, ui.id().with(("tiles-rm", &src.id)), egui::Sense::click());
        p.text(
            rm_at.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(13.0),
            if rm.hovered() { t.text_strong } else { t.text_dim },
        );
        if rm.clicked() {
            let _ = app.run("map.removeSource", json!({ "id": src.id }));
            if app.ui.tile_edit.as_ref() == Some(&TileEdit::Source(src.id.clone())) {
                app.ui.tile_edit = None;
            }
        }
        rm.on_hover_text(tl!("Remove connection"));
    }
    // A plain row click does nothing: the eye toggles, the pencil edits (QGIS semantics).
    resp.on_hover_text(format!("{}\n{}–z{}\n{}", src.name, src.min_zoom, src.max_zoom, src.attribution));
}

/// The Add Connection form's body (kept out of `show` for reading's sake): the QGIS-style fields —
/// name, URL template, API key, attribution, min/max zoom, kind.
fn add_connection(app: &mut TerrazoneApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(6.0);
    let mut draft = app.ui.tiles_draft.clone();
    if connection_fields(ui, &mut draft) {
        app.ui.tiles_draft = draft.clone();
    }
    if !draft.template.is_empty() && !draft.template_ok() {
        template_hint(ui, &t, &draft);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(draft.template_ok() && !draft.name.trim().is_empty(), egui::Button::new(tl!("Connect"))).clicked() {
            let _ = app.run(
                "map.addSource",
                json!({
                    "name": draft.name.trim(),
                    "template": draft.template.trim(),
                    "apiKey": draft.api_key.trim(),
                    "attribution": draft.attribution.trim(),
                    "minZoom": draft.min_zoom,
                    "maxZoom": draft.max_zoom,
                    "kind": kind_str(draft.kind),
                }),
            );
            app.ui.tiles_form = false;
            app.ui.tiles_draft = TilesDraft::default();
        }
        if ui.small_button(tl!("Cancel")).clicked() {
            app.ui.tiles_form = false;
        }
    });
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(tl!(
            "Mapbox Streets, Satellite and the Mapbox vector streets ship with this app's access token; Google's endpoints need none."
        ))
        .size(11.0)
        .color(t.text_dim),
    );
}

/// The connection fields Add Connection and the Edit tab's source editor share: name, URL
/// template, API key, attribution, zoom range and the kind. True when anything changed this frame.
fn connection_fields(ui: &mut Ui, d: &mut TilesDraft) -> bool {
    let mut changed = false;
    egui::Grid::new("tiles-connection").num_columns(2).spacing([8.0, 5.0]).show(ui, |ui| {
        ui.label(tl!("Name"));
        changed |= ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(180.0).hint_text("My Tiles")).changed();
        ui.end_row();
        ui.label(tl!("URL"));
        changed |= ui
            .add(egui::TextEdit::singleline(&mut d.template).desired_width(180.0).hint_text("https://…/{z}/{x}/{y}.png"))
            .on_hover_text(tl!("An XYZ template: {z}, {x}, {y}; {s} subdomains and {key} where the API key goes. Vector tiles: the .mvt URL."))
            .changed();
        ui.end_row();
        ui.label(tl!("API Key"));
        changed |= ui
            .add(egui::TextEdit::singleline(&mut d.api_key).desired_width(180.0).hint_text(tl!("optional")))
            .on_hover_text(tl!("Put {key} in the URL where the key goes — a Mapbox access token, say."))
            .changed();
        ui.end_row();
        ui.label(tl!("Attribution"));
        changed |= ui.add(egui::TextEdit::singleline(&mut d.attribution).desired_width(180.0).hint_text(tl!("optional"))).changed();
        ui.end_row();
        ui.label(tl!("Zoom"));
        ui.horizontal(|ui| {
            changed |= ui.add(egui::DragValue::new(&mut d.min_zoom).range(0..=22).prefix("z")).changed();
            ui.label("–");
            changed |= ui.add(egui::DragValue::new(&mut d.max_zoom).range(0..=22).prefix("z")).changed();
        });
        ui.end_row();
        ui.label(tl!("Kind"));
        egui::ComboBox::from_id_salt("tiles-kind").selected_text(kind_str(d.kind)).width(150.0).show_ui(ui, |ui| {
            for (kind, label) in [
                (TileKind::Raster, "raster"),
                (TileKind::Vector, "vector"),
                (TileKind::Wms, "wms"),
                (TileKind::Wmts, "wmts"),
                (TileKind::GoogleSession, "googlesession"),
                (TileKind::Google3dTiles, "google3dtiles"),
            ] {
                if ui.selectable_label(d.kind == kind, label).clicked() && d.kind != kind {
                    d.kind = kind;
                    changed = true;
                }
            }
        });
        ui.end_row();
    });
    changed
}

/// The template hint when a draft's URL wouldn't pass the engine's check for its kind.
fn template_hint(ui: &mut Ui, t: &Tokens, d: &TilesDraft) {
    let text = match d.kind {
        TileKind::Raster | TileKind::Vector => tl!("The URL needs {z}, {x} and {y} in an https template."),
        _ => tl!("The URL needs to start with https."),
    };
    ui.label(egui::RichText::new(text).size(11.0).color(t.text_dim));
}

/// The kind as the engine's `map.addSource` names it.
fn kind_str(kind: TileKind) -> &'static str {
    match kind {
        TileKind::Raster => "raster",
        TileKind::Vector => "vector",
        TileKind::Wms => "wms",
        TileKind::Wmts => "wmts",
        TileKind::GoogleSession => "googlesession",
        TileKind::Google3dTiles => "google3dtiles",
    }
}

/// The kind chip's colour and glyph: a picture for raster tiles, contours for vector ones, an orb
/// for Google, a grid for the OGC services.
fn kind_chip(t: &Tokens, kind: TileKind) -> (Color32, &'static str) {
    match kind {
        TileKind::Raster => (t.text_dim, "▤"),
        TileKind::Vector => (t.accent, "⌁"),
        TileKind::Wms => (t.text_dim, "▨"),
        TileKind::Wmts => (t.text_dim, "▧"),
        TileKind::GoogleSession => (t.accent, "◎"),
        TileKind::Google3dTiles => (t.accent, "⬡"),
    }
}

/// A QGIS-style slider that drags through a local draft and commits once, on release (or on a
/// plain click): the caller gets `Some(final value)` exactly once per interaction.
fn live_slider(ui: &mut Ui, id: egui::Id, value: f64, range: std::ops::RangeInclusive<f64>, suffix: &str) -> Option<f64> {
    let draft: Option<f64> = ui.ctx().data(|d| d.get_temp(id));
    let mut v = draft.unwrap_or(value);
    let resp = ui.add(egui::Slider::new(&mut v, range).suffix(suffix));
    if resp.dragged() {
        ui.ctx().data_mut(|d| d.insert_temp(id, Some(v)));
        return None;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, None::<f64>));
    if draft.is_some() || resp.changed() { Some(v) } else { None }
}

/// A text field that commits on Enter (or on leaving with the text changed): the caller gets
/// `Some(new text)` once, and only for a non-empty, actually-different value.
fn text_field(ui: &mut Ui, id: egui::Id, value: &str) -> Option<String> {
    let mut s = ui.ctx().data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| value.to_string());
    let resp = ui.add(egui::TextEdit::singleline(&mut s).desired_width(140.0));
    let commit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if resp.has_focus() && resp.changed() {
        ui.ctx().data_mut(|d| d.insert_temp(id, s.clone()));
    }
    if !resp.has_focus() {
        ui.ctx().data_mut(|d| d.insert_temp(id, value.to_string()));
    }
    let out = (commit || (resp.changed() && !resp.has_focus())).then(|| s.trim().to_string()).filter(|t| !t.is_empty() && t != value);
    if out.is_some() {
        ui.ctx().data_mut(|d| d.insert_temp(id, value.to_string()));
    }
    out
}

/// The Style tab: the whole basemap's QGIS-style sliders — opacity, brightness, contrast,
/// saturation, hue — committed to `map.style` (one undo step per release, like every panel edit).
fn style_tab(app: &mut TerrazoneApp, ui: &mut Ui, map: &terrazon_map::MapSettings) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(tl!("Basemap Style")).font(theme::semibold(12.0)));
    let sliders: [(&str, &str, f64, std::ops::RangeInclusive<f64>, &str); 5] = [
        ("opacity", tl!("Opacity"), map.opacity * 100.0, 0.0..=100.0, "%"),
        ("brightness", tl!("Brightness"), map.style.brightness as f64, -100.0..=100.0, ""),
        ("contrast", tl!("Contrast"), map.style.contrast as f64, -100.0..=100.0, ""),
        ("saturation", tl!("Saturation"), map.style.saturation as f64, -100.0..=100.0, ""),
        ("hue", tl!("Hue"), map.style.hue as f64, -180.0..=180.0, "°"),
    ];
    let mut run = None;
    egui::Grid::new("tiles-basemap-style").num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
        for (k, label, value, range, suffix) in sliders {
            ui.label(egui::RichText::new(label).size(11.5));
            if let Some(v) = live_slider(ui, egui::Id::new(("basemap-style", k)), value, range, suffix) {
                run = Some(match k {
                    "opacity" => json!({ "opacity": (v / 100.0 * 1000.0).round() / 1000.0 }),
                    _ => json!({ k: v.round() }),
                });
            }
            ui.end_row();
        }
    });
    if let Some(p) = run {
        let _ = app.run("map.style", p);
    }
    let plain = map.style.is_neutral() && (map.opacity - 1.0).abs() < 1e-9;
    if !plain && ui.small_button(tl!("Reset")).clicked() {
        let _ = app.run("map.style", json!({ "opacity": 1.0, "brightness": 0, "contrast": 0, "saturation": 0, "hue": 0 }));
    }
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!("A pinned tile's own style lives on its Edit tab.")).size(11.0).color(t.text_dim));
}

/// The tiles the canvas view covers right now, at the zoom it shows them (the Map menu's "pin
/// what you see" comes from here). `None` without an open document drawing its basemap — or
/// before the canvas first drew (the canvas publishes the list every frame it draws tiles).
pub fn visible_tiles(app: &TerrazoneApp) -> Option<Vec<TileId>> {
    let st = app.session.active()?;
    if !st.doc.map.visible {
        return None;
    }
    let (uid, tiles) = app.canvas_view_tiles.as_ref()?;
    (uid == &st.uid).then(|| tiles.clone())
}

/// Tile Sub-layers: the pinned tiles as rows — the eye, the lock, the name, a pencil that opens
/// the Edit tab and an ⨯ that unpins. Click selects (Ctrl/Cmd adds), Delete unpins the picks.
fn tile_sublayers(app: &mut TerrazoneApp, ui: &mut Ui, map: &terrazon_map::MapSettings) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Tile Sub-layers")).font(theme::semibold(12.0)));
        ui.label(egui::RichText::new(crate::i18n::tn(map.tile_layers.len() as u64, "{n} tile", "{n} tiles")).size(11.0).color(t.text_dim));
    });
    if map.tile_layers.is_empty() {
        dim_label(ui, tl!("Pin tiles from the view above and they show here, and in the Layers panel under the Map layer."));
        return;
    }
    // Delete unpins the picked tile layers (the panel keeps the keyboard when a row was clicked).
    if !app.ui.tile_layers_sel.is_empty()
        && ui.ctx().memory(|m| m.focused().is_none())
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace))
    {
        let ids = json!({ "ids": app.ui.tile_layers_sel.clone() });
        let _ = app.run("map.removeTileLayers", ids);
        app.ui.tile_layers_sel.clear();
        app.ui.tile_layer_open = None;
        return;
    }
    let cap = ui.available_width();
    for tl in map.tile_layers.iter().rev() {
        let picked = app.ui.tile_layers_sel.iter().any(|id| id == &tl.id);
        let (r, resp) = ui.allocate_exact_size(vec2(cap, 26.0), egui::Sense::click());
        ui.painter().rect_filled(
            r,
            4.0,
            if picked {
                t.hover
            } else if resp.hovered() {
                t.panel_darker
            } else {
                Color32::TRANSPARENT
            },
        );
        let p = ui.painter_at(r);
        let left = r.left() + 6.0;
        // The source chip: a picture for raster tiles, contours for vector ones, an orb for Google.
        let kind = map.sources.iter().find(|s| s.id == tl.source).map(|s| s.kind).unwrap_or_default();
        let (chip, chip_text) = kind_chip(&t, kind);
        p.text(egui::pos2(left + 6.0, r.center().y), egui::Align2::LEFT_CENTER, chip_text, egui::FontId::proportional(12.0), chip);
        let name_col = if tl.visible { t.text } else { t.text_dim };
        p.text(egui::pos2(left + 22.0, r.center().y), egui::Align2::LEFT_CENTER, &tl.name, egui::FontId::proportional(12.0), name_col);
        // The eye, the lock, the pencil and the ⨯, right to left.
        let eye_at = egui::Rect::from_center_size(egui::pos2(r.right() - 82.0, r.center().y), vec2(18.0, 18.0));
        let lock_at = egui::Rect::from_center_size(egui::pos2(r.right() - 62.0, r.center().y), vec2(18.0, 18.0));
        let edit_at = egui::Rect::from_center_size(egui::pos2(r.right() - 42.0, r.center().y), vec2(18.0, 18.0));
        let rm_at = egui::Rect::from_center_size(egui::pos2(r.right() - 22.0, r.center().y), vec2(18.0, 18.0));
        let eye = ui.interact(eye_at, ui.id().with(("tile-eye", &tl.id)), egui::Sense::click());
        crate::icons::paint(ui, if tl.visible { "eye" } else { "eye-off" }, eye_at.shrink(3.0), if tl.visible { t.icon } else { t.text_disabled });
        if eye.clicked() {
            let _ = app.run("map.updateTileLayer", json!({ "id": tl.id, "visible": !tl.visible }));
        }
        eye.on_hover_text(if tl.visible { tl!("Hide Tile Layer") } else { tl!("Show Tile Layer") });
        let lock = ui.interact(lock_at, ui.id().with(("tile-lock", &tl.id)), egui::Sense::click());
        crate::icons::paint(ui, "lock", lock_at.shrink(3.0), if tl.locked { t.icon } else { t.text_disabled });
        if lock.clicked() {
            let _ = app.run("map.updateTileLayer", json!({ "id": tl.id, "locked": !tl.locked }));
        }
        lock.on_hover_text(if tl.locked { tl!("Unlock Tile Layer") } else { tl!("Lock Tile Layer") });
        // The pencil opens the Edit tab on this tile layer; the row being edited shows it lit.
        let edit = ui.interact(edit_at, ui.id().with(("tile-edit", &tl.id)), egui::Sense::click());
        let editing = matches!(&app.ui.tile_edit, Some(TileEdit::Layer(id)) if id == &tl.id);
        crate::icons::paint(ui, "pencil", edit_at.shrink(3.0), if editing { t.icon } else { t.text_dim });
        if edit.clicked() {
            app.ui.tiles_tab = TilesPanelTab::Edit;
            app.ui.tile_edit = Some(TileEdit::Layer(tl.id.clone()));
            app.ui.tile_layer_open = Some(tl.id.clone());
        }
        edit.on_hover_text(tl!("Edit tile data (name, source, position, opacity, style)"));
        let rm = ui.interact(rm_at, ui.id().with(("tile-rm", &tl.id)), egui::Sense::click());
        p.text(
            rm_at.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(13.0),
            if rm.hovered() { t.text_strong } else { t.text_dim },
        );
        if rm.clicked() {
            let _ = app.run("map.removeTileLayers", json!({ "ids": [tl.id] }));
            if app.ui.tile_edit.as_ref() == Some(&TileEdit::Layer(tl.id.clone())) {
                app.ui.tile_edit = None;
                app.ui.tile_layer_open = None;
            }
            app.ui.tile_layers_sel.retain(|id| id != &tl.id);
        }
        rm.on_hover_text(tl!("Unpin Tile"));
        // The row click picks; Ctrl/Cmd adds or removes from the picks.
        let mods = ui.input(|i| i.modifiers);
        if resp.clicked() {
            if mods.command || mods.ctrl {
                if picked {
                    app.ui.tile_layers_sel.retain(|id| id != &tl.id);
                } else {
                    app.ui.tile_layers_sel.push(tl.id.clone());
                }
            } else {
                app.ui.tile_layers_sel = vec![tl.id.clone()];
            }
        }
        resp.on_hover_text(format!("{}\n{} · z{}/{}/{} · {}%", tl.name, tl.source, tl.z, tl.x, tl.y, (tl.opacity * 100.0).round()));
        ui.add_space(1.0);
    }
}

/// The Edit tab: the editor a row's pencil opened, or the placeholder.
fn edit_tab(app: &mut TerrazoneApp, ui: &mut Ui, map: &terrazon_map::MapSettings) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Edit")).font(theme::semibold(12.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button(tl!("Done")).clicked() {
                app.ui.tiles_tab = TilesPanelTab::Tiles;
                app.ui.tile_edit = None;
                app.ui.tile_layer_open = None;
            }
        });
    });
    ui.add_space(2.0);
    match app.ui.tile_edit.clone() {
        Some(TileEdit::Layer(id)) => {
            let Some(tl) = map.tile_layers.iter().find(|t| t.id == id).cloned() else {
                dim_label(ui, tl!("Nothing selected — press a row's pencil to edit it here."));
                return;
            };
            if let Some(run) = tile_editor(app, ui, map, &tl) {
                let _ = app.run("map.updateTileLayer", run);
            }
            ui.add_space(6.0);
            // Unpinning is what the ⨯ does on the row; here it respects the lock.
            let unpin = ui.add_enabled(!tl.locked, egui::Button::new(tl!("Unpin Tile"))).on_hover_text(if tl.locked {
                tl!("Locked — unlock the tile first.")
            } else {
                tl!("Unpin Tile")
            });
            if unpin.clicked() {
                let _ = app.run("map.removeTileLayers", json!({ "ids": [tl.id] }));
                app.ui.tile_edit = None;
                app.ui.tile_layer_open = None;
                app.ui.tiles_tab = TilesPanelTab::Tiles;
            }
        }
        Some(TileEdit::Source(id)) => {
            let Some(src) = map.sources.iter().find(|s| s.id == id).cloned() else {
                dim_label(ui, tl!("Nothing selected — press a row's pencil to edit it here."));
                return;
            };
            if terrazon_map::builtin_id(&src.id) {
                builtin_source_info(ui, &src);
                ui.add_space(6.0);
                if ui.button(tl!("Duplicate as Connection")).clicked() {
                    let mut draft = TilesDraft::of_source(&src);
                    draft.name = format!("{} (copy)", src.name);
                    app.ui.tiles_draft = draft;
                    app.ui.tiles_form = true;
                    app.ui.tile_edit = None;
                    app.ui.tiles_tab = TilesPanelTab::Tiles;
                }
            } else {
                source_editor(app, ui, &src);
            }
        }
        None => {
            dim_label(ui, tl!("Nothing selected — press a row's pencil to edit it here."));
        }
    }
}

/// A built-in source's read-only details: the facts, and the way to an editable copy.
fn builtin_source_info(ui: &mut Ui, src: &terrazon_map::TileSource) {
    let t = Tokens::get(ui.ctx());
    egui::Grid::new("tiles-builtin-source").num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
        ui.label(egui::RichText::new(tl!("Name")).size(11.5));
        ui.label(&src.name);
        ui.end_row();
        ui.label(egui::RichText::new(tl!("Kind")).size(11.5));
        ui.label(kind_str(src.kind));
        ui.end_row();
        ui.label(egui::RichText::new(tl!("Zoom")).size(11.5));
        ui.label(format!("z{}–z{}", src.min_zoom, src.max_zoom));
        ui.end_row();
        ui.label(egui::RichText::new(tl!("Attribution")).size(11.5));
        ui.label(&src.attribution);
        ui.end_row();
        ui.label(egui::RichText::new(tl!("URL")).size(11.5));
        ui.label(egui::RichText::new(&src.template).size(11.0).color(t.text_dim));
        ui.end_row();
    });
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Built-ins can't be changed. Duplicate it as a connection to edit a copy.")).size(11.0).color(t.text_dim));
}

/// A user connection's editor: the same fields as Add Connection, prefilled from the source,
/// saved in place with `map.addSource` (the engine replaces the source with the same id).
fn source_editor(app: &mut TerrazoneApp, ui: &mut Ui, src: &terrazon_map::TileSource) {
    let t = Tokens::get(ui.ctx());
    let key = egui::Id::new(("tiles-source-edit", &src.id));
    let mut draft: TilesDraft = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_else(|| TilesDraft::of_source(src));
    ui.add_space(4.0);
    if connection_fields(ui, &mut draft) {
        ui.ctx().data_mut(|d| d.insert_temp(key, draft.clone()));
    }
    if !draft.template.is_empty() && !draft.template_ok() {
        template_hint(ui, &t, &draft);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let ok = draft.template_ok() && !draft.name.trim().is_empty();
        if ui.add_enabled(ok, egui::Button::new(tl!("Save"))).clicked() {
            let _ = app.run(
                "map.addSource",
                json!({
                    "id": src.id,
                    "name": draft.name.trim(),
                    "template": draft.template.trim(),
                    "apiKey": draft.api_key.trim(),
                    "attribution": draft.attribution.trim(),
                    "minZoom": draft.min_zoom,
                    "maxZoom": draft.max_zoom,
                    "kind": kind_str(draft.kind),
                }),
            );
            ui.ctx().data_mut(|d| d.remove::<TilesDraft>(key));
            app.ui.tile_edit = None;
            app.ui.tiles_tab = TilesPanelTab::Tiles;
        }
        if ui.small_button(tl!("Cancel")).clicked() {
            ui.ctx().data_mut(|d| d.remove::<TilesDraft>(key));
            app.ui.tile_edit = None;
            app.ui.tiles_tab = TilesPanelTab::Tiles;
        }
    });
}

/// The tile layer's QGIS-style editor: name, source, z/x/y, opacity and the style sliders, plus a
/// read-only peek at what a vector tile holds (its MVT layers, most numerous first). Returns the
/// params of an update to run, if any field committed this frame.
fn tile_editor(app: &mut TerrazoneApp, ui: &mut Ui, map: &terrazon_map::MapSettings, tl: &terrazon_map::TileLayer) -> Option<serde_json::Value> {
    let t = Tokens::get(ui.ctx());
    let mut run: Option<serde_json::Value> = None;
    let commit = |run: &mut Option<serde_json::Value>, p: serde_json::Value| {
        *run = Some(match run.take() {
            Some(prev) => {
                let mut o = prev;
                for (k, v) in p.as_object().into_iter().flatten() {
                    o[k] = v.clone();
                }
                o
            }
            None => {
                let mut o = serde_json::Map::new();
                o.insert("id".into(), json!(tl.id));
                for (k, v) in p.as_object().into_iter().flatten() {
                    o[k] = v.clone();
                }
                serde_json::Value::Object(o)
            }
        });
    };
    ui.indent(egui::Id::new(("tile-editor", &tl.id)), |ui| {
        egui::Grid::new(("tile-editor-grid", &tl.id)).num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
            ui.label(egui::RichText::new(tl!("Name")).size(11.5));
            if let Some(name) = text_field(ui, egui::Id::new(("tile-name", &tl.id)), &tl.name) {
                commit(&mut run, json!({ "name": name }));
            }
            ui.end_row();
            ui.label(egui::RichText::new(tl!("Source")).size(11.5));
            let current = map.sources.iter().find(|s| s.id == tl.source).map(|s| s.name.clone()).unwrap_or_else(|| tl.source.clone());
            egui::ComboBox::from_id_salt(("tile-source", &tl.id)).selected_text(current).width(150.0).show_ui(ui, |ui| {
                for s in &map.sources {
                    if ui.selectable_label(s.id == tl.source, &s.name).clicked() && s.id != tl.source {
                        commit(&mut run, json!({ "source": s.id }));
                    }
                }
            });
            ui.end_row();
            ui.label(egui::RichText::new(tl!("Tile")).size(11.5));
            ui.horizontal(|ui| {
                let (mut z, mut x, mut y) = (tl.z as i32, tl.x as i32, tl.y as i32);
                if ui.add(egui::DragValue::new(&mut z).range(0..=22).prefix("z")).changed() {
                    commit(&mut run, json!({ "z": z }));
                }
                if ui.add(egui::DragValue::new(&mut x).prefix("x")).changed() {
                    commit(&mut run, json!({ "x": x }));
                }
                if ui.add(egui::DragValue::new(&mut y).prefix("y")).changed() {
                    commit(&mut run, json!({ "y": y }));
                }
            });
            ui.end_row();
            ui.label(egui::RichText::new(tl!("Opacity")).size(11.5));
            if let Some(v) = live_slider(ui, egui::Id::new(("tile-opacity", &tl.id)), tl.opacity * 100.0, 0.0..=100.0, "%") {
                commit(&mut run, json!({ "opacity": (v / 100.0 * 1000.0).round() / 1000.0 }));
            }
            ui.end_row();
        });
        ui.add_space(2.0);
        ui.label(egui::RichText::new(tl!("Style")).size(11.5).color(t.text_dim));
        let sliders: [(&str, &str, f64, std::ops::RangeInclusive<f64>, &str); 4] = [
            ("brightness", tl!("Brightness"), tl.style.brightness as f64, -100.0..=100.0, ""),
            ("contrast", tl!("Contrast"), tl.style.contrast as f64, -100.0..=100.0, ""),
            ("saturation", tl!("Saturation"), tl.style.saturation as f64, -100.0..=100.0, ""),
            ("hue", tl!("Hue"), tl.style.hue as f64, -180.0..=180.0, "°"),
        ];
        egui::Grid::new(("tile-editor-style", &tl.id)).num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
            for (k, label, value, range, suffix) in sliders {
                ui.label(egui::RichText::new(label).size(11.5));
                if let Some(v) = live_slider(ui, egui::Id::new(("tile-style", &tl.id, k)), value, range, suffix) {
                    commit(&mut run, json!({ "style": { k: v.round() } }));
                }
                ui.end_row();
            }
        });
        if kind_of(map, tl) == TileKind::Vector {
            let ls = app.canvas.tiles.vector_layers(&tl.source, TileId::new(tl.z, tl.x, tl.y));
            if !ls.is_empty() {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(tl!("Data")).size(11.5).color(t.text_dim));
                let text = ls.iter().take(6).map(|(name, n)| format!("{name} ({n})")).collect::<Vec<_>>().join(" · ");
                ui.label(egui::RichText::new(text).size(11.0).color(t.text_dim));
            }
        }
        if kind_of(map, tl) == TileKind::Google3dTiles {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(tl!(
                    "3D Tiles: the meshes draw in a 3D viewport; until then the 2D canvas shows Google's session 2D tiles as the imagery."
                ))
                .size(11.0)
                .color(t.text_dim),
            );
        }
    });
    run
}

/// The tile layer's source kind, for the editor's data preview (raster tiles have none).
fn kind_of(map: &terrazon_map::MapSettings, tl: &terrazon_map::TileLayer) -> TileKind {
    map.sources.iter().find(|s| s.id == tl.source).map(|s| s.kind).unwrap_or_default()
}

/// A connection draft, kept while the Add Connection form is open — and while the Edit tab edits a
/// user connection (in egui temp data, keyed by the source id).
#[derive(Clone, Debug, Default)]
pub struct TilesDraft {
    pub name: String,
    pub template: String,
    pub api_key: String,
    pub attribution: String,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub kind: TileKind,
}

impl TilesDraft {
    /// The draft of a source's fields, as the Edit tab's editor and Duplicate as Connection
    /// prefill them.
    pub fn of_source(src: &terrazon_map::TileSource) -> Self {
        Self {
            name: src.name.clone(),
            template: src.template.clone(),
            api_key: src.api_key.clone(),
            attribution: src.attribution.clone(),
            min_zoom: src.min_zoom,
            max_zoom: src.max_zoom,
            kind: src.kind,
        }
    }

    /// The template passes the engine's check for the draft's kind: an XYZ template ({z}, {x},
    /// {y}) for raster and vector tiles, an https URL for the service kinds.
    pub fn template_ok(&self) -> bool {
        if !self.template.starts_with("http") {
            return false;
        }
        match self.kind {
            TileKind::Raster | TileKind::Vector => self.template.contains("{z}") && self.template.contains("{x}") && self.template.contains("{y}"),
            _ => true,
        }
    }
}
