//! The Assets tool: the catalogue's props placed like the path tools place points — a click
//! drops one asset at the pointer, a drag **brushes** a row of them (one every so-many
//! metres, the way a brush lays paint), each placement its own document data through the
//! `assets.*` commands. The 2D map draws the placements as vector glyphs; the 3D environment
//! stands the same placements on the terrain as meshes — one list, both canvases.

use serde_json::{Value, json};

use terrazon_assets::AssetKind;
use terrazon_geom::Point;

use crate::{Action, Cursor, Mods, Overlay, PointerEvent, PointerKind, Tool, ToolContext, ToolKey};

/// The tool id (the registry, the catalog and the control bar key by it).
pub const ID: &str = "assets";

/// Web-mercator ground metres → document points: the whole world spans [`terrazon_map::WORLD_PT`]
/// points, so a metre is that span over the equator's circumference. Latitude stretches east-west
/// distances in Mercator by 1/cos φ — at Dar es Salaam that is 0.7 %, far below what a brush
/// spacing can see.
fn doc_per_metre() -> f64 {
    terrazon_map::WORLD_PT / 40_075_016.686
}

/// One asset every ~80 % of its footprint: a row of trees reads as a planting, not a grid.
const SPACING_FACTOR: f64 = 0.8;

#[derive(Default)]
pub struct AssetsTool {
    /// The kind the next placement places (the control bar's picker sets it).
    kind: AssetKind,
    /// The placement scale (the catalogue footprint × scale metres; the picker sets it).
    scale: f32,
    /// The brush drag's points so far (a click is a drag with one).
    drag: Option<Vec<Point>>,
    /// The last point a placement landed at (the spacing measures from it).
    last: Option<Point>,
    /// The hover point (the ghost in 3D reads the tool's option, not this; the dot previews 2D).
    hover: Option<Point>,
}

impl AssetsTool {
    fn spacing_doc(&self) -> f64 {
        let [w, _] = self.kind.size_m();
        (w as f64 * self.scale as f64 * doc_per_metre() * SPACING_FACTOR).max(2.0)
    }
}

impl Tool for AssetsTool {
    fn id(&self) -> &'static str {
        ID
    }

    fn busy(&self) -> bool {
        self.drag.is_some()
    }

    fn pointer(&mut self, _cx: &ToolContext, ev: &PointerEvent) -> Vec<Action> {
        match ev.kind {
            PointerKind::Down => {
                // One undo group for the whole gesture: a click places one, a drag brushes a row.
                self.drag = Some(vec![ev.pos]);
                self.last = Some(ev.pos);
                vec![Action::Begin("Brush Assets".into())]
            }
            PointerKind::Drag => {
                let spacing = self.spacing_doc();
                let Some(pts) = self.drag.as_mut() else { return vec![] };
                // Walk from the last placement toward the pointer, dropping one every spacing —
                // a fast drag still lays an even row (no gaps, no clumps).
                while let Some(last) = self.last {
                    let dx = ev.pos.x - last.x;
                    let dy = ev.pos.y - last.y;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist < spacing {
                        break;
                    }
                    let step = spacing / dist;
                    let next = Point::new(last.x + dx * step, last.y + dy * step);
                    pts.push(next);
                    self.last = Some(next);
                }
                vec![]
            }
            PointerKind::Up => {
                let Some(pts) = self.drag.take() else { return vec![] };
                self.last = None;
                if pts.is_empty() {
                    return vec![Action::Commit];
                }
                let (kind, scale) = (self.kind, self.scale);
                let points: Vec<Value> = pts.iter().map(|p| json!([p.x, p.y])).collect();
                // One placement rides the same command a row does — the spray with one point
                // IS the click-place, so both undo as one step and read the same in the panel.
                vec![Action::Exec("placements.spray".into(), json!({ "kind": kind.label(), "points": points, "scale": scale })), Action::Commit]
            }
            _ => vec![],
        }
    }

    fn key(&mut self, _cx: &ToolContext, key: ToolKey, _mods: Mods) -> Vec<Action> {
        // Escape cancels a drag mid-stroke (nothing placed, the undo group closes empty).
        if matches!(key, ToolKey::Escape) && self.drag.take().is_some() {
            self.last = None;
            return vec![Action::Cancel];
        }
        vec![]
    }

    fn overlays(&self, _cx: &ToolContext) -> Vec<Overlay> {
        // The row being brushed (or the hover point), as anchor dots — the placement preview.
        let dot = |p: &Point| Overlay::Anchor { p: *p, color: [0x2e, 0xa0, 0x4f], filled: true, size: 3.0 };
        match self.drag.as_ref() {
            Some(pts) => pts.iter().map(dot).collect(),
            None => self.hover.iter().map(dot).collect(),
        }
    }

    fn cursor(&self, _cx: &ToolContext, _p: Point, _mods: Mods) -> Cursor {
        Cursor::Crosshair
    }

    fn options(&self) -> serde_json::Value {
        json!({ "kind": self.kind.label(), "scale": self.scale })
    }

    fn set_option(&mut self, key: &str, value: &serde_json::Value) {
        match key {
            "kind" => {
                if let Some(s) = value.as_str()
                    && let Some(k) = AssetKind::parse(s)
                {
                    self.kind = k;
                }
            }
            "scale" => {
                if let Some(v) = value.as_f64() {
                    self.scale = (v as f32).clamp(0.05, 50.0);
                }
            }
            _ => {}
        }
    }
}
