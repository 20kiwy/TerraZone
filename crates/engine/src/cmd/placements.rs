//! The placed assets: the Assets tool's document side. A placement is data — the kind, a doc
//! point, a rotation and a scale — and the document's `placements` is the one truth both
//! canvases draw from (the vector glyph on the 2D map, the standing mesh in the 3D
//! environment). Like the fleet commands these are small, honest edits: one undo step per
//! gesture, and the queries read exactly what was stored.

use serde_json::{Value, json};

use super::*;
use super::{bad, has_doc, str_param};
use crate::{CommandSpec, Session};
use terrazon_assets::AssetKind;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "placements.place",
            "Place Asset",
            ["Map"],
            None,
            "{kind: tree|house|building|stadium|pin|camera, x, y, rotation?, scale?, pitch?, fov?} place one asset at a document point (the catalogue footprint × scale metres, rotated from north; a camera carries its aim: pitch below the horizon, horizontal fov, degrees) → {id, kind}",
            has_doc,
            place
        ),
        cmd!(
            "placements.spray",
            "Brush Assets",
            ["Map"],
            None,
            "{kind, points: [[x, y], …], rotation?, scale?, spacing?} brush a row of assets along the drag's points (the brush places one every spacing points; the tool computes them) → {placed}",
            has_doc,
            spray
        ),
        cmd!("placements.remove", "Remove Asset", ["Map"], None, "{id} remove one placed asset → {removed}", has_doc, remove),
        cmd!("placements.clear", "Clear Assets", ["Map"], None, "{} remove every placement → {cleared}", has_doc, clear),
        cmd!(
            "placements.list",
            "Placed Assets",
            ["Map"],
            None,
            "{} the document's placements → {placements: [{id, kind, x, y, rotation, scale, pitch, fov}]}",
            has_doc,
            list
        ),
        cmd!(
            "placements.update",
            "Update Asset",
            ["Map"],
            None,
            "{id, rotation?, scale?, pitch?, fov?} update one placement's aim and size: the rotation (degrees clockwise from north), the scale (the catalogue footprint ×), and for cameras the look pitch (degrees below the horizon) and horizontal fov (degrees) → {updated}",
            has_doc,
            update
        ),
    ]
}

fn kind_param(cmd: &str, p: &Value) -> Result<AssetKind> {
    let raw = str_param(p, "kind").ok_or_else(|| bad(cmd, "give the asset kind: tree, house, building, stadium, pin or camera"))?;
    AssetKind::parse(raw).ok_or_else(|| bad(cmd, format!("unknown asset kind {raw:?} — tree, house, building, stadium, pin or camera")))
}

fn rotation_of(p: &Value) -> f32 {
    p.get("rotation").and_then(Value::as_f64).map(|r| r as f32).unwrap_or(0.0)
}

fn scale_of(p: &Value) -> f32 {
    p.get("scale").and_then(Value::as_f64).map(|s| s as f32).unwrap_or(1.0).clamp(0.05, 50.0)
}

/// The camera's look pitch (degrees, negative down — clamped to a sane ±85°).
fn pitch_of(p: &Value) -> Option<f32> {
    p.get("pitch").and_then(Value::as_f64).map(|r| (r as f32).clamp(-85.0, 85.0))
}

/// The camera's horizontal field of view (degrees, 5–150).
fn fov_of(p: &Value) -> Option<f32> {
    p.get("fov").and_then(Value::as_f64).map(|r| (r as f32).clamp(5.0, 150.0))
}

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "placements.place";
    let kind = kind_param(C, p)?;
    let (Some(x), Some(y)) = (p.get("x").and_then(Value::as_f64), p.get("y").and_then(Value::as_f64)) else {
        return Err(bad(C, "give the document point: x and y"));
    };
    let (rotation, scale) = (rotation_of(p), scale_of(p));
    let (pitch, fov) = (pitch_of(p), fov_of(p));
    let mut out = (String::new(), kind.label().to_string());
    s.edit("Place Asset", |d, _| {
        let id = d.placements.place_with(kind, x, y, rotation, scale);
        if let Some(pl) = d.placements.placements.iter_mut().rev().find(|pl| pl.id == id) {
            if let Some(pitch) = pitch {
                pl.pitch_deg = pitch;
            }
            if let Some(fov) = fov {
                pl.fov_deg = fov;
            }
        }
        out = (id, kind.label().to_string());
        Ok(())
    })?;
    Ok(json!({ "id": out.0, "kind": out.1 }))
}

fn spray(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "placements.spray";
    let kind = kind_param(C, p)?;
    let points = p.get("points").and_then(Value::as_array).ok_or_else(|| bad(C, "give the drag's points: [[x, y], …]"))?;
    let pts: Vec<(f64, f64)> = points
        .iter()
        .filter_map(|q| {
            let a = q.as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
        })
        .collect();
    if pts.is_empty() {
        return Err(bad(C, "no points to place along"));
    }
    let (rotation, scale) = (rotation_of(p), scale_of(p));
    let mut placed = 0usize;
    s.edit("Brush Assets", |d, _| {
        for (x, y) in pts {
            d.placements.place_with(kind, x, y, rotation, scale);
            placed += 1;
        }
        Ok(())
    })?;
    Ok(json!({ "placed": placed, "kind": kind.label() }))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "placements.remove";
    let Some(id) = str_param(p, "id") else { return Err(bad(C, "give the placement id")) };
    let mut removed = false;
    s.edit("Remove Asset", |d, _| {
        removed = d.placements.remove(id);
        Ok(())
    })?;
    Ok(json!({ "removed": removed }))
}

fn clear(s: &mut Session, _p: &Value) -> Result<Value> {
    let mut cleared = 0usize;
    s.edit("Clear Assets", |d, _| {
        cleared = d.placements.placements.len();
        d.placements.clear();
        Ok(())
    })?;
    Ok(json!({ "cleared": cleared }))
}

fn list(s: &mut Session, _p: &Value) -> Result<Value> {
    let st = s.doc()?;
    Ok(json!({
        "placements": st
            .doc
            .placements
            .placements
            .iter()
            .map(|pl| json!({ "id": pl.id, "kind": pl.kind.label(), "x": pl.x, "y": pl.y, "rotation": pl.rotation_deg, "scale": pl.scale, "pitch": pl.pitch_deg, "fov": pl.fov_deg }))
            .collect::<Vec<_>>(),
    }))
}

/// Update one placement's aim and size: rotation, scale, and the camera's pitch/fov. Only the
/// given fields move; an unknown id is an error.
fn update(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "placements.update";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "give the placement id"))?.to_string();
    let rotation = p.get("rotation").and_then(Value::as_f64).map(|r| r as f32);
    let scale = p.get("scale").and_then(Value::as_f64).map(|s| (s as f32).clamp(0.05, 50.0));
    let (pitch, fov) = (pitch_of(p), fov_of(p));
    let mut updated = false;
    s.edit("Update Asset", |d, _| {
        let pl = d
            .placements
            .placements
            .iter_mut()
            .find(|pl| pl.id == id)
            .ok_or_else(|| bad(C, format!("no placement called `{id}` (see placements.list)")))?;
        if let Some(r) = rotation {
            pl.rotation_deg = r;
        }
        if let Some(sc) = scale {
            pl.scale = sc;
        }
        if let Some(pi) = pitch {
            pl.pitch_deg = pi;
        }
        if let Some(fo) = fov {
            pl.fov_deg = fo;
        }
        updated = true;
        Ok(())
    })?;
    Ok(json!({ "updated": updated }))
}
