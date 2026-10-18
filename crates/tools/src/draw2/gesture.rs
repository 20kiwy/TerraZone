//! Freehand gesture tools: Pencil (N), Paintbrush (B), Blob Brush (Shift+B), Eraser (Shift+E),
//! Knife, Smooth, Path Eraser and Join.
//!
//! Pencil and Paintbrush preview the fitted path live (`path.freehand` re-applied on the snapshot)
//! and commit on release; Alt on release (or ending near the start) closes the path, and starting
//! near an end of a selected open path continues it. The Pencil's new path starts and ends on
//! Smart Guides ([`DrawSnap`]: an anchor, a path, in line with the art), hovering too; the points
//! between follow the hand. The other tools collect the drag polyline (shown as an overlay) and run
//! one command on release.

use serde_json::{Value, json};
use terrazon_doc::NodeId;
use terrazon_geom::Point;

use super::{FEEDBACK, points_json, polyline};
use crate::guides::DrawSnap;
use crate::{Action, Cursor, Mods, Overlay, PointerEvent, PointerKind, Tool, ToolContext, ToolKey};

pub struct GestureTool {
    id: &'static str,
    points: Vec<Point>,
    active: bool,
    began: bool,
    /// Pencil/Paintbrush continuing a selected open path: (path, continue from its start).
    extend: Option<(NodeId, bool)>,
    /// Fit tolerance in points (Pencil/Paintbrush/Smooth "Fidelity").
    pub fidelity: f64,
    /// Brush / eraser diameter in points.
    pub size: f64,
    /// Fill new pencil/brush strokes.
    pub fill: bool,
    /// "Edit selected paths within N pixels".
    pub edit_within: f64,
    /// "Close paths when ends are within N pixels".
    pub close_within: f64,
    /// Smart Guides for the Pencil's ends.
    snap: DrawSnap,
}

impl GestureTool {
    pub fn new(id: &str) -> Self {
        let id: &'static str = match id {
            "paintbrush" => "paintbrush",
            "blobBrush" => "blobBrush",
            "eraser" => "eraser",
            "knife" => "knife",
            "smooth" => "smooth",
            "pathEraser" => "pathEraser",
            "join" => "join",
            _ => "pencil",
        };
        let fidelity = if id == "smooth" { 2.5 } else { 1.5 };
        Self {
            id,
            points: vec![],
            active: false,
            began: false,
            extend: None,
            fidelity,
            size: 10.0,
            fill: false,
            edit_within: 12.0,
            close_within: 15.0,
            snap: DrawSnap::default(),
        }
    }

    /// Do the ends of its strokes snap (the Pencil)?
    fn snaps(&self) -> bool {
        self.id == "pencil"
    }

    fn draws_path(&self) -> bool {
        matches!(self.id, "pencil" | "paintbrush")
    }

    fn label(&self) -> &'static str {
        match self.id {
            "paintbrush" => "Paintbrush",
            "blobBrush" => "Blob Brush",
            "eraser" => "Eraser",
            "knife" => "Knife",
            "smooth" => "Smooth",
            "pathEraser" => "Path Eraser",
            "join" => "Join",
            _ => "Pencil",
        }
    }

    /// The selected open path whose end is near `p`.
    fn find_extend(&self, cx: &ToolContext, p: Point) -> Option<(NodeId, bool)> {
        let tol = cx.tol(self.edit_within);
        for id in &cx.selection.objects {
            let Some(pd) = cx.doc.node(*id).and_then(|n| n.path_data()) else { continue };
            if !cx.doc.is_editable(*id) {
                continue;
            }
            let Some(sp) = pd.subpaths.iter().rev().find(|s| !s.closed && !s.anchors.is_empty()) else { continue };
            let (first, last) = (sp.anchors[0].p, sp.anchors[sp.anchors.len() - 1].p);
            if p.distance(last) <= tol {
                return Some((*id, false));
            }
            if p.distance(first) <= tol {
                return Some((*id, true));
            }
        }
        None
    }

    /// Parameters of `path.freehand` for the current stroke. The fit tolerance rides with the
    /// view: the option's points are screen pixels, so they are sent as document points divided
    /// by the zoom. Fixed points would collapse the stroke to a line at a map's street level
    /// (1.5 pt is ~98,000 screen px at 65536×) and over-fit it to the device's jitter at the
    /// whole-world 0.002×.
    pub fn freehand_params(&self, cx: &ToolContext, closed: bool) -> Value {
        let mut v = json!({
            "points": points_json(&self.points),
            "fidelity": self.fidelity / cx.zoom.max(1e-6),
            "closed": closed,
            "style": if self.id == "paintbrush" { "brush" } else { "pencil" },
            "fill": self.fill,
        });
        if let Some((id, start)) = self.extend {
            v["extend"] = json!({"id": id.0, "end": if start { "start" } else { "end" }});
        }
        v
    }

    /// The brush diameter in document points. On a map document strokes draw in screen pixels
    /// (a 1 px line is one device pixel at every zoom), so the size option is screen pixels too
    /// and rides the zoom down: 0.1 px at 65536× is ~1.5e-6 pt, thin enough to trace a street —
    /// elsewhere it is document points, as the option's pt says.
    fn brush_size(&self, cx: &ToolContext) -> f64 {
        if cx.doc.map.visible { self.size / cx.zoom.max(1e-6) } else { self.size }
    }

    fn finish(&mut self, cx: &ToolContext, m: Mods) -> Vec<Action> {
        let pts = &self.points;
        let has_sel = !cx.selection.is_empty();
        let tol = cx.tol(self.edit_within);
        match self.id {
            "pencil" | "paintbrush" => {
                if !self.began {
                    return vec![];
                }
                let near = pts.len() > 2 && pts[0].distance(pts[pts.len() - 1]) <= cx.tol(self.close_within);
                let closed = m.alt || (self.extend.is_none() && near);
                vec![Action::Preview("path.freehand".into(), self.freehand_params(cx, closed)), Action::Commit]
            }
            "blobBrush" => vec![Action::Exec("path.blob".into(), json!({"points": points_json(pts), "size": self.brush_size(cx)}))],
            "eraser" => vec![Action::Exec("path.eraseRegion".into(), json!({"points": points_json(pts), "size": self.brush_size(cx)}))],
            "knife" if pts.len() >= 2 => vec![Action::Exec("path.knife".into(), json!({"points": points_json(pts)}))],
            "smooth" if has_sel && pts.len() >= 2 => {
                vec![Action::Exec("path.smoothRegion".into(), json!({"points": points_json(pts), "radius": tol, "fidelity": self.fidelity}))]
            }
            "pathEraser" if has_sel => vec![Action::Exec("path.eraseSegments".into(), json!({"points": points_json(pts), "width": cx.tol(8.0)}))],
            "join" if pts.len() >= 2 => vec![Action::Exec("path.joinScrub".into(), json!({"points": points_json(pts), "tolerance": tol}))],
            _ => vec![],
        }
    }
}

impl Tool for GestureTool {
    fn id(&self) -> &'static str {
        self.id
    }
    fn busy(&self) -> bool {
        self.active
    }
    /// The Blob Brush and Eraser take [ and ] for their brush size (see `key`) ahead of any
    /// shortcut bound to them.
    fn claims_key(&self, _cx: &ToolContext, key: ToolKey) -> bool {
        matches!(self.id, "blobBrush" | "eraser") && matches!(key, ToolKey::BracketLeft | ToolKey::BracketRight)
    }
    fn pointer(&mut self, cx: &ToolContext, ev: &PointerEvent) -> Vec<Action> {
        let p = ev.pos;
        match ev.kind {
            PointerKind::Move if self.snaps() => {
                self.snap.hover(cx, p, &[], None);
                vec![]
            }
            PointerKind::Down => {
                self.active = true;
                self.began = false;
                self.extend = if self.draws_path() { self.find_extend(cx, p) } else { None };
                // A path continued starts at its end; a new one where it snaps.
                let start = if self.snaps() && self.extend.is_none() { self.snap.press(cx, p, &[], None) } else { p };
                self.points = vec![start];
                vec![]
            }
            PointerKind::Drag => {
                if !self.active {
                    return vec![];
                }
                if self.snaps() {
                    // Where the stroke would end: shown, taken on release.
                    self.snap.drag(cx, p, None);
                }
                if self.points.last().is_some_and(|l| l.distance(p) < cx.tol(1.0)) {
                    return vec![];
                }
                self.points.push(p);
                if !self.draws_path() {
                    return vec![];
                }
                let mut out = vec![];
                if !self.began {
                    self.began = true;
                    out.push(Action::Begin(self.label().into()));
                }
                out.push(Action::Preview("path.freehand".into(), self.freehand_params(cx, false)));
                out
            }
            PointerKind::Up => {
                if !self.active {
                    return vec![];
                }
                let end = if self.snaps() { self.snap.drag(cx, p, None) } else { p };
                self.snap.clear();
                if self.points.last().is_some_and(|l| l.distance(p) >= cx.tol(1.0)) {
                    self.points.push(end);
                } else if self.snaps()
                    && self.points.len() > 1
                    && let Some(last) = self.points.last_mut()
                {
                    // The last step was where the button went up: it moves where that snaps.
                    *last = end;
                }
                let out = self.finish(cx, ev.mods);
                self.active = false;
                self.began = false;
                self.points.clear();
                self.extend = None;
                out
            }
            _ => vec![],
        }
    }
    fn key(&mut self, _cx: &ToolContext, key: ToolKey, _m: Mods) -> Vec<Action> {
        match key {
            ToolKey::Escape if self.active => {
                let began = self.began;
                self.snap.clear();
                self.active = false;
                self.began = false;
                self.points.clear();
                if began { vec![Action::Cancel] } else { vec![] }
            }
            ToolKey::BracketLeft if matches!(self.id, "blobBrush" | "eraser") => {
                // Stepping down into the sub-point range: a map seen at street level needs a
                // 0.1 pt brush, so the step is a tenth below 1 pt (a whole-point step there would
                // jump straight past it) and 1 pt from 1.0 up. Tenths are kept exact so repeated
                // presses don't drift off the 0.1 floor by floating point.
                let step = if self.size <= 1.0 { 0.1 } else { 1.0 };
                self.size = (self.size - step).max(0.1);
                if self.size < 1.0 {
                    self.size = (self.size * 10.0).round() / 10.0;
                }
                vec![]
            }
            ToolKey::BracketRight if matches!(self.id, "blobBrush" | "eraser") => {
                let step = if self.size < 1.0 { 0.1 } else { 1.0 };
                self.size = (self.size + step).min(1000.0);
                if self.size < 1.0 {
                    self.size = (self.size * 10.0).round() / 10.0;
                }
                vec![]
            }
            _ => vec![],
        }
    }
    fn deactivate(&mut self, _cx: &ToolContext) -> Vec<Action> {
        let began = self.began;
        self.snap.clear();
        self.active = false;
        self.began = false;
        self.points.clear();
        if began { vec![Action::Commit] } else { vec![] }
    }
    fn overlays(&self, cx: &ToolContext) -> Vec<Overlay> {
        let mut o = self.snap.guides().to_vec();
        if !self.active || self.points.len() < 2 || self.began {
            return o;
        }
        let width = match self.id {
            "blobBrush" | "eraser" => {
                // The stroke draws as wide as the brush ring: on a map document the size is
                // screen pixels, elsewhere document points at the view's zoom.
                (if cx.doc.map.visible { self.size } else { self.size * cx.zoom }) as f32
            }
            _ => 1.0,
        };
        let color = if self.id == "eraser" { [0x80, 0x80, 0x80] } else { FEEDBACK };
        o.push(Overlay::Path { path: polyline(&self.points), color, width, dashed: self.id == "knife" });
        o
    }
    fn cursor(&self, _cx: &ToolContext, _p: Point, _m: Mods) -> Cursor {
        Cursor::Crosshair
    }
    fn options(&self) -> Value {
        match self.id {
            "pencil" | "paintbrush" => {
                json!({"fidelity": self.fidelity, "fill": self.fill, "editWithin": self.edit_within, "closeWithin": self.close_within})
            }
            "smooth" => json!({"fidelity": self.fidelity}),
            "blobBrush" | "eraser" => json!({"size": self.size}),
            _ => Value::Null,
        }
    }
    fn set_option(&mut self, key: &str, v: &Value) {
        match key {
            "fidelity" => self.fidelity = v.as_f64().unwrap_or(self.fidelity).clamp(0.05, 100.0),
            "size" => self.size = v.as_f64().unwrap_or(self.size).clamp(0.1, 1000.0),
            "fill" => self.fill = v.as_bool().unwrap_or(self.fill),
            "editWithin" => self.edit_within = v.as_f64().unwrap_or(self.edit_within).clamp(0.0, 100.0),
            "closeWithin" => self.close_within = v.as_f64().unwrap_or(self.close_within).clamp(0.0, 100.0),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use terrazon_doc::Selection;

    fn drag(t: &mut GestureTool, cx: &ToolContext, pts: &[(f64, f64)], m: Mods) -> Vec<Vec<Action>> {
        let mut v = vec![t.pointer(cx, &PointerEvent::new(PointerKind::Down, pts[0].0, pts[0].1))];
        for &(x, y) in &pts[1..] {
            v.push(t.pointer(cx, &PointerEvent::new(PointerKind::Drag, x, y)));
        }
        let l = pts[pts.len() - 1];
        v.push(t.pointer(cx, &PointerEvent::new(PointerKind::Up, l.0, l.1).with_mods(m)));
        v
    }

    /// [`cx`] with the view's zoom (screen px per document point): a map document reaches from
    /// 0.002 (the whole world) to 65536 (street level), and the tools must work across that.
    fn cx_zoom<'a>(d: &'a terrazon_doc::Document, s: &'a terrazon_doc::Selection, p: &'a crate::PaintDefaults, zoom: f64) -> ToolContext<'a> {
        let mut c = cx(d, s, p);
        c.zoom = zoom;
        c
    }

    /// The Pencil's new path starts and ends on Smart Guides (#506), the points between where
    /// the hand went.
    #[test]
    fn pencil_ends_snap_to_smart_guides() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let cx = cx(&d, &s, &p);
        let mut t = GestureTool::new("pencil");
        let acts = drag(&mut t, &cx, &[(102.0, 98.0), (151.0, 300.0), (250.0, 320.0), (198.0, 203.0)], Mods::default());
        let Some(Action::Preview(_, v)) = acts.last().and_then(|a| a.first()) else { panic!("{acts:?}") };
        assert_eq!(v["points"], json!([[100.0, 100.0], [151.0, 300.0], [250.0, 320.0], [200.0, 200.0]]));
        // The Paintbrush follows the hand all the way.
        let mut t = GestureTool::new("paintbrush");
        let acts = drag(&mut t, &cx, &[(102.0, 98.0), (151.0, 300.0)], Mods::default());
        let Some(Action::Preview(_, v)) = acts.last().and_then(|a| a.first()) else { panic!("{acts:?}") };
        assert_eq!(v["points"][0], json!([102.0, 98.0]));
    }

    #[test]
    fn pencil_previews_and_commits() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let cx = cx(&d, &s, &p);
        let mut t = GestureTool::new("pencil");
        let acts = drag(&mut t, &cx, &[(0.0, 0.0), (10.0, 5.0), (20.0, 0.0)], Mods::default());
        assert!(acts[0].is_empty());
        assert_eq!(acts[1][0], Action::Begin("Pencil".into()));
        assert!(matches!(&acts[1][1], Action::Preview(c, v) if c == "path.freehand" && v["points"].as_array().unwrap().len() == 2));
        let last = acts.last().unwrap();
        assert!(matches!(&last[0], Action::Preview(_, v) if v["closed"] == false && v["style"] == "pencil"));
        assert_eq!(last[1], Action::Commit);
        assert!(!t.busy());
    }

    #[test]
    fn pencil_alt_closes_and_continues_selected_path() {
        let (d, id) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let cx = cx(&d, &s, &p);
        let mut t = GestureTool::new("paintbrush");
        let acts = drag(&mut t, &cx, &[(0.0, 0.0), (10.0, 5.0), (20.0, 0.0)], Mods { alt: true, ..Default::default() });
        assert!(matches!(&acts.last().unwrap()[0], Action::Preview(_, v) if v["closed"] == true && v["style"] == "brush"));
        // A selected closed rect is not continued; an open path would be (see engine tests).
        let mut s2 = Selection::default();
        s2.set([id]);
        let cx2 = crate::testutil::cx(&d, &s2, &p);
        let acts = drag(&mut t, &cx2, &[(100.0, 100.0), (90.0, 90.0)], Mods::default());
        assert!(matches!(&acts[1][1], Action::Preview(_, v) if v.get("extend").is_none()));
    }

    #[test]
    fn eraser_and_knife_exec_on_release() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let cx = cx(&d, &s, &p);
        let mut t = GestureTool::new("eraser");
        let acts = drag(&mut t, &cx, &[(50.0, 150.0), (250.0, 150.0)], Mods::default());
        assert!(acts[1].is_empty());
        assert!(matches!(&acts[2][0], Action::Exec(c, v) if c == "path.eraseRegion" && v["size"] == 10.0));
        let mut k = GestureTool::new("knife");
        k.pointer(&cx, &PointerEvent::new(PointerKind::Down, 50.0, 150.0));
        k.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 150.0, 150.0));
        assert_eq!(k.overlays(&cx).len(), 1);
        let a = k.pointer(&cx, &PointerEvent::new(PointerKind::Up, 250.0, 150.0));
        assert!(matches!(&a[0], Action::Exec(c, v) if c == "path.knife" && v["points"].as_array().unwrap().len() == 3));
    }

    #[test]
    fn smooth_needs_selection_and_brackets_resize() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let cx = cx(&d, &s, &p);
        let mut t = GestureTool::new("smooth");
        let acts = drag(&mut t, &cx, &[(50.0, 150.0), (250.0, 150.0)], Mods::default());
        assert!(acts.iter().all(|a| a.is_empty()));
        let mut b = GestureTool::new("blobBrush");
        b.key(&cx, ToolKey::BracketRight, Mods::default());
        assert_eq!(b.options()["size"], 11.0);
    }

    /// The Blob Brush / Eraser's bracket keys reach 0.1 pt (a map at street level needs a
    /// sub-point brush): tenths below 1 pt, whole points from 1.0 up, floored at the same 0.1 the
    /// size option clamps to.
    #[test]
    fn blob_brush_brackets_step_into_sub_point_sizes() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let cx = cx(&d, &s, &p);
        let mut b = GestureTool::new("blobBrush");
        // Nine whole-point steps down reach 1.0; the tenth steps by a tenth, into the sub-point
        // range the old 1.0-floor keys could never enter.
        for _ in 0..9 {
            b.key(&cx, ToolKey::BracketLeft, Mods::default());
        }
        assert_eq!(b.options()["size"], 1.0);
        b.key(&cx, ToolKey::BracketLeft, Mods::default());
        assert_eq!(b.options()["size"], 0.9);
        // The floor is the same 0.1 `set_option("size")` clamps to, however often [ repeats.
        for _ in 0..20 {
            b.key(&cx, ToolKey::BracketLeft, Mods::default());
        }
        assert_eq!(b.options()["size"], 0.1);
        // Back up by tenths to 1.0, then whole points again.
        for _ in 0..9 {
            b.key(&cx, ToolKey::BracketRight, Mods::default());
        }
        assert_eq!(b.options()["size"], 1.0);
        b.key(&cx, ToolKey::BracketRight, Mods::default());
        assert_eq!(b.options()["size"], 2.0);
        // The Eraser shares the keys.
        let mut e = GestureTool::new("eraser");
        e.set_option("size", &json!(0.5));
        e.key(&cx, ToolKey::BracketLeft, Mods::default());
        assert_eq!(e.options()["size"], 0.4);
    }

    /// [ and ] reach the tool ahead of the shortcuts: the Blob Brush and Eraser claim them, the
    /// Pencil (which has no brush) doesn't.
    #[test]
    fn blob_brush_claims_the_bracket_keys() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let cx = cx(&d, &s, &p);
        let b = GestureTool::new("blobBrush");
        assert!(b.claims_key(&cx, ToolKey::BracketLeft) && b.claims_key(&cx, ToolKey::BracketRight));
        assert!(!GestureTool::new("pencil").claims_key(&cx, ToolKey::BracketLeft));
        // The Symbolism tools' brush keys reach them too.
        let t = crate::create("symbolSprayer");
        assert!(t.claims_key(&cx, ToolKey::BracketLeft));
    }

    /// On a map document (where strokes draw in screen pixels) the Blob Brush's size option is
    /// screen pixels: it rides the zoom down, so 0.1 px at 65536× is ~1.5e-6 pt — a stroke thin
    /// enough to trace a street. Elsewhere it stays document points, and mid-drag the stroke
    /// draws as wide as the brush ring shows.
    #[test]
    fn blob_brush_size_rides_the_zoom_on_a_map() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let street = cx_zoom(&d, &s, &p, 65536.0);
        let mut b = GestureTool::new("blobBrush");
        b.set_option("size", &json!(0.1));
        let acts = drag(&mut b, &street, &[(10.0, 10.0), (20.0, 10.0)], Mods::default());
        let Some(Action::Exec(_, v)) = acts.last().and_then(|a| a.first()) else { panic!("{acts:?}") };
        assert!((v["size"].as_f64().unwrap() - 0.1 / 65536.0).abs() < 1e-15, "{}", v["size"]);
        // Mid-drag the stroke overlay is the option itself (screen pixels on a map).
        let mut b = GestureTool::new("blobBrush");
        b.set_option("size", &json!(0.1));
        b.pointer(&street, &PointerEvent::new(PointerKind::Down, 10.0, 10.0));
        b.pointer(&street, &PointerEvent::new(PointerKind::Drag, 15.0, 10.0));
        b.pointer(&street, &PointerEvent::new(PointerKind::Drag, 20.0, 10.0));
        let overlays = b.overlays(&street);
        let Some(Overlay::Path { width, .. }) = overlays.last() else { panic!("no stroke overlay") };
        assert_eq!(*width, 0.1);
        // A plain document keeps document points: 0.1 pt at 2× is still 0.1 pt (the overlay 0.2 px).
        let (d, _) = doc_with_rect();
        let mut d = d;
        d.map.visible = false;
        let cx = cx_zoom(&d, &s, &p, 2.0);
        let mut b = GestureTool::new("blobBrush");
        b.set_option("size", &json!(0.1));
        let acts = drag(&mut b, &cx, &[(10.0, 10.0), (20.0, 10.0)], Mods::default());
        let Some(Action::Exec(_, v)) = acts.last().and_then(|a| a.first()) else { panic!("{acts:?}") };
        assert_eq!(v["size"], 0.1);
        b.pointer(&cx, &PointerEvent::new(PointerKind::Down, 10.0, 10.0));
        b.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 15.0, 10.0));
        b.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 20.0, 10.0));
        let overlays = b.overlays(&cx);
        let Some(Overlay::Path { width, .. }) = overlays.last() else { panic!("no stroke overlay") };
        assert_eq!(*width, 0.2);
    }

    /// The freehand fit tolerance is a screen size: at 1× zoom it is the option's own points
    /// (unchanged), and it shrinks with the zoom so a stroke fits the hand the same at any zoom —
    /// a fixed 1.5 pt at 65536× is ~98,000 screen px and collapses the stroke to a line.
    #[test]
    fn freehand_fidelity_tracks_the_screen() {
        let (d, _) = doc_with_rect();
        let s = Selection::default();
        let p = paint();
        let mut t = GestureTool::new("paintbrush");
        let cx1 = cx(&d, &s, &p);
        let acts = drag(&mut t, &cx1, &[(0.0, 0.0), (10.0, 5.0)], Mods::default());
        let Some(Action::Preview(_, v)) = acts[1].last() else { panic!("{acts:?}") };
        assert_eq!(v["fidelity"], 1.5);
        let cx8 = cx_zoom(&d, &s, &p, 8.0);
        let mut t = GestureTool::new("paintbrush");
        let acts = drag(&mut t, &cx8, &[(0.0, 0.0), (10.0, 5.0)], Mods::default());
        // The live preview on the drag and the committed preview on the release both ride with
        // the zoom (8× → an eighth of the points).
        let Some(Action::Preview(_, v)) = acts[1].last() else { panic!("{acts:?}") };
        assert_eq!(v["fidelity"], 1.5 / 8.0);
        let Some(Action::Preview(_, v)) = acts[2].first() else { panic!("{acts:?}") };
        assert_eq!(v["fidelity"], 1.5 / 8.0);
    }
}
