//! Map › Tile Sources…: switch the tile basemap, turn tiles on or off, make a new tile source (an
//! XYZ URL template) and remove the ones added before. OK applies the new source; picking a source
//! applies at once.
//!
//! Fields: `active` (the picked source id, applied when picked), `show` (tiles on), `name`,
//! `template`, `attribution` (the new source, applied by OK).

use serde_json::{Value, json};

use super::swatch_options::{grid, label};
use super::{DialogSpec, run_and_close};
use crate::TerrazoneApp;
use crate::state::Dialog;

/// The dialog kind of Tile Sources.
pub const KIND: &str = "mapSources";

pub(super) const SPEC: DialogSpec = DialogSpec { heading: |_| tl!("Tile Sources").into(), body, confirm, min_width: 420.0, ..DialogSpec::FORM };

/// Open Tile Sources on the document's map.
pub fn open(app: &mut TerrazoneApp) -> Result<Value, String> {
    let (active, visible) =
        app.session.active().map(|st| (st.doc.map.active.clone(), st.doc.map.visible)).ok_or_else(|| "no document open".to_string())?;
    app.ui.dialog = Some(Dialog::new(KIND, json!({ "active": active, "show": visible })));
    Ok(Value::Null)
}

fn body(app: &mut TerrazoneApp, ui: &mut egui::Ui, d: &mut Dialog) -> bool {
    let Some(st) = app.session.active() else { return false };
    let map = st.doc.map.clone();
    // Tiles on or off.
    let mut show = d.bool("show") || (d.fields.get("show").is_none() && map.visible);
    if ui.checkbox(&mut show, tl!("Show tiles under the art")).changed() {
        d.fields.insert("show".into(), json!(show));
        let _ = app.run("map.showTiles", json!({ "on": show }));
    }
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    // The sources: picking one switches at once. A user source has a Remove button.
    let active = d.str("active");
    for src in &map.sources {
        ui.horizontal(|ui| {
            if ui.radio(src.id == active, &src.name).on_hover_text(format!("{}\n{}", src.template, src.attribution)).clicked() {
                d.fields.insert("active".into(), json!(src.id));
                let _ = app.run("map.setSource", json!({ "id": src.id }));
            }
            let builtin = terrazon_map::builtin_id(&src.id);
            if !builtin
                && ui.small_button(tl!("Remove")).clicked()
                && app.run("map.removeSource", json!({ "id": src.id })).is_ok()
                && d.str("active") == src.id
            {
                d.fields.insert("active".into(), json!(map.sources.first().map(|s| s.id.clone()).unwrap_or_default()));
            }
        });
        ui.label(egui::RichText::new(&src.attribution).size(10.0).color(crate::theme::Tokens::get(ui.ctx()).text_dim));
        ui.add_space(2.0);
    }
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    // Make a new source: name + an XYZ template (plus its attribution), OK adds it.
    ui.label(egui::RichText::new(tl!("Make Tile Source")).strong());
    ui.add_space(2.0);
    let t = crate::theme::Tokens::get(ui.ctx());
    grid(ui, |ui| {
        label(ui, tl!("Name:"));
        let mut name = d.str("name");
        if ui.add(egui::TextEdit::singleline(&mut name).desired_width(220.0)).changed() {
            d.fields.insert("name".into(), json!(name));
        }
        ui.end_row();
        label(ui, tl!("URL template:"));
        let mut template = d.str("template");
        let resp = ui.add(egui::TextEdit::singleline(&mut template).desired_width(220.0).hint_text("https://…/{z}/{x}/{y}.png"));
        if resp.changed() {
            d.fields.insert("template".into(), json!(template));
        }
        ui.end_row();
        label(ui, tl!("Attribution:"));
        let mut attribution = d.str("attribution");
        if ui.add(egui::TextEdit::singleline(&mut attribution).desired_width(220.0)).changed() {
            d.fields.insert("attribution".into(), json!(attribution));
        }
        ui.end_row();
    });
    if !d.str("template").is_empty() && !terrazon_map::TileSource::new("", "", d.str("template").trim(), "", 19, &[]).plausible() {
        ui.label(egui::RichText::new(tl!("The template needs {z}, {x} and {y} in an https URL.")).size(11.0).color(t.text_dim));
    }
    false
}

fn confirm(app: &mut TerrazoneApp, d: &Dialog) -> Result<Value, String> {
    let template = d.str("template").trim().to_string();
    if template.is_empty() {
        // Nothing to add: the dialog was for switching (already applied as picked).
        app.ui.dialog = None;
        return Ok(Value::Null);
    }
    let mut params = json!({ "name": d.str("name").trim(), "template": template });
    if !d.str("attribution").trim().is_empty() {
        params["attribution"] = json!(d.str("attribution").trim());
    }
    run_and_close(app, "map.addSource", params)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use terrazon_engine::Session;

    #[test]
    fn opens_switches_and_makes_a_source() {
        let mut app = TerrazoneApp::new(Session::new(), Default::default());
        app.run("file.new", json!({})).unwrap();
        open(&mut app).unwrap();
        assert_eq!(app.ui.dialog.as_ref().unwrap().str("active"), "osm");
        // Switch by field + confirm with no template just closes.
        app.ui.dialog.as_mut().unwrap().fields.insert("active".into(), json!("esri-dark"));
        let d = app.ui.dialog.as_ref().unwrap().clone();
        super::confirm(&mut app, &d).unwrap();
        assert!(app.ui.dialog.is_none());
        assert_eq!(app.session.active().unwrap().doc.map.active, "osm"); // picking applies in the body; the field alone doesn't
        // OK with a template makes the source and activates it.
        open(&mut app).unwrap();
        {
            let d = app.ui.dialog.as_mut().unwrap();
            d.fields.insert("name".into(), json!("My Tiles"));
            d.fields.insert("template".into(), json!("https://tiles.example.org/{z}/{x}/{y}.png"));
            d.fields.insert("attribution".into(), json!("© me"));
        }
        let d = app.ui.dialog.as_ref().unwrap().clone();
        super::confirm(&mut app, &d).unwrap();
        let st = app.session.active().unwrap();
        assert!(st.doc.map.sources.iter().any(|s| s.id == "my-tiles"));
        assert_eq!(st.doc.map.active, "my-tiles");
    }
}
