//! Point placement tools: Array Point and Line Point.
//!
//! Both place small filled dots ([`DOT`] pt circles in the current fill and stroke) as one group,
//! through the engine's `shape.arrayPoint` and `shape.linePoint` commands, in document points —
//! so on a map document they are Mercator-correct wherever the view is zoomed, exactly like the
//! shape tools.
//!
//! Array Point drags a marquee that fills with an evenly spaced `rows` × `cols` array of dots (one
//! per cell; the cell size is the marquee divided by the counts, or the `spacing` option); a click
//! without a drag places the tool-options array centred on the click. Line Point drags a line that
//! gains `count` dots equally spaced along it, its ends included; a click without a drag draws
//! nothing (there is no line to space dots along yet). While dragging, ↑/↓ change the array's rows
//! (←/→ its columns) or the line's dot count.

use serde_json::{Value, json};
use terrazon_geom::Point;

use crate::guides::{DrawSnap, Leave};
use crate::shape::{drag_rect, space_moves};
use crate::{Action, Cursor, Mods, Overlay, PointerEvent, PointerKind, Tool, ToolContext, ToolKey};

/// Diameter of one point dot, in document points: the smallest sensible marker at any zoom.
pub const DOT: f64 = 2.5;

/// The smallest and largest row, column and dot count the tools take.
pub const MIN_CELLS: u32 = 1;
pub const MAX_CELLS: u32 = 64;
/// The Line Point tool's dots include both ends, so at least two.
pub const MIN_LINE_DOTS: u32 = 2;

/// The centres of a `rows` × `cols` array of dots filling the rectangle at `x`, `y` sized
/// `w` × `h`: one dot per cell of an even grid, at the cells' centres.
pub fn array_centres(x: f64, y: f64, w: f64, h: f64, rows: u32, cols: u32) -> Vec<Point> {
    let rows = rows.max(MIN_CELLS);
    let cols = cols.max(MIN_CELLS);
    let (cw, ch) = (w / f64::from(cols), h / f64::from(rows));
    let mut v = Vec::with_capacity(rows as usize * cols as usize);
    for j in 0..rows {
        for i in 0..cols {
            v.push(Point::new(x + (f64::from(i) + 0.5) * cw, y + (f64::from(j) + 0.5) * ch));
        }
    }
    v
}

/// `count` points equally spaced along `a → b`, its ends included (`count` < 2 gives the two ends).
pub fn line_points(a: Point, b: Point, count: u32) -> Vec<Point> {
    let n = count.max(MIN_LINE_DOTS);
    let step = (b - a) / f64::from(n - 1);
    (0..n).map(|i| a + step * f64::from(i)).collect()
}

/// How a drag-shaped gesture ended: [`Ended::Drag`] once a drag drew (preview on the way, commit
/// at the release) or [`Ended::Click`] for a plain press and release.
enum Ended {
    Drag { start: Point, end: Point },
    Click,
}

/// The parts the two tools share: Smart Guides on the press and the dragged point, Space held
/// moving the drag instead of sizing it, a 2 pt nudge before it counts as a drag.
#[derive(Default)]
struct Drag {
    start: Option<Point>,
    last: Point,
    mods: Mods,
    began: bool,
    snap: DrawSnap,
}

impl Drag {
    /// A press, drag and release. Returns the actions to run (the interaction's `Begin`) and, at
    /// the end of the gesture, how it ended.
    fn pointer(
        &mut self,
        cx: &ToolContext,
        ev: &PointerEvent,
        label: &str,
        leave: impl Fn(&ToolContext, Point, Mods) -> Option<Leave>,
    ) -> (Vec<Action>, Option<Ended>) {
        match ev.kind {
            PointerKind::Move => {
                self.snap.hover(cx, ev.pos, &[], None);
                (vec![], None)
            }
            PointerKind::Down => {
                let p = self.snap.press(cx, ev.pos, &[], None);
                self.start = Some(p);
                self.last = p;
                self.began = false;
                (vec![], None)
            }
            PointerKind::Drag => {
                let Some(mut s) = self.start else { return (vec![], None) };
                let pos = self.snap.drag(cx, ev.pos, leave(cx, s, ev.mods).as_ref());
                let ev = &PointerEvent { pos, ..*ev };
                space_moves(&mut s, &mut self.last, ev, self.began);
                self.start = Some(s);
                self.mods = ev.mods;
                let mut out = vec![];
                if !self.began {
                    if ev.pos.distance(s) < cx.tol(2.0) {
                        return (out, None);
                    }
                    self.began = true;
                    out.push(Action::Begin(label.into()));
                }
                (out, Some(Ended::Drag { start: s, end: ev.pos }))
            }
            PointerKind::Up => {
                let s = self.start.take();
                self.snap.clear();
                if std::mem::take(&mut self.began) {
                    (vec![], s.map(|s| Ended::Drag { start: s, end: self.last }))
                } else {
                    (vec![], Some(Ended::Click))
                }
            }
            _ => (vec![], None),
        }
    }
}

/// The Array Point tool: a marquee drag fills with an evenly spaced rows × columns array of dots;
/// a click places the tool-options array centred on the click.
pub struct ArrayPointTool {
    drag: Drag,
    pub rows: u32,
    pub cols: u32,
    /// Cell size of a click's array, in document points (a drag derives it from the marquee).
    pub spacing: f64,
}

impl ArrayPointTool {
    pub fn new() -> Self {
        Self { drag: Drag::default(), rows: 3, cols: 3, spacing: 10.0 }
    }

    /// The command for a marquee from `start` to `p`: the rows × columns array filling it, the
    /// cell size derived from the marquee.
    pub fn command(&self, start: Point, p: Point, m: Mods) -> (String, Value) {
        let r = drag_rect(start, p, m);
        ("shape.arrayPoint".into(), json!({"x": r.x0, "y": r.y0, "width": r.width(), "height": r.height(), "rows": self.rows, "columns": self.cols}))
    }

    /// The command for a click at `p`: the tool-options array (`rows` × `cols` dots `spacing`
    /// apart) centred on it.
    pub fn click_command(&self, p: Point) -> (String, Value) {
        let (w, h) = (self.spacing * f64::from(self.cols), self.spacing * f64::from(self.rows));
        (
            "shape.arrayPoint".into(),
            json!({
                "x": p.x - w / 2.0, "y": p.y - h / 2.0,
                "width": w, "height": h,
                "rows": self.rows, "columns": self.cols, "spacing": self.spacing,
            }),
        )
    }
}

impl Default for ArrayPointTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ArrayPointTool {
    fn id(&self) -> &'static str {
        "arrayPoint"
    }
    fn busy(&self) -> bool {
        self.drag.start.is_some()
    }
    fn pointer(&mut self, cx: &ToolContext, ev: &PointerEvent) -> Vec<Action> {
        let (mut out, ended) = self.drag.pointer(cx, ev, "Array Point", |_cx, s, m| Some(Leave::diagonal(s, m.shift)));
        match ended {
            Some(Ended::Drag { .. }) if ev.kind == PointerKind::Up => out.push(Action::Commit),
            Some(Ended::Drag { start, end }) => {
                let (c, v) = self.command(start, end, self.drag.mods);
                out.push(Action::Preview(c, v));
            }
            // A click places the tool-options array around the click at once (its own undo step).
            Some(Ended::Click) => {
                let (c, v) = self.click_command(self.drag.last);
                out.push(Action::Exec(c, v));
            }
            None => {}
        }
        out
    }
    fn key(&mut self, _cx: &ToolContext, key: ToolKey, _mods: Mods) -> Vec<Action> {
        if key == ToolKey::Escape {
            self.drag.start = None;
            self.drag.began = false;
            self.drag.snap.clear();
            return vec![Action::Cancel];
        }
        let Some(s) = self.drag.start.filter(|_| self.drag.began) else { return vec![] };
        let changed = match key {
            ToolKey::Up => {
                self.rows = (self.rows + 1).min(MAX_CELLS);
                true
            }
            ToolKey::Down => {
                self.rows = self.rows.saturating_sub(1).max(MIN_CELLS);
                true
            }
            ToolKey::Right => {
                self.cols = (self.cols + 1).min(MAX_CELLS);
                true
            }
            ToolKey::Left => {
                self.cols = self.cols.saturating_sub(1).max(MIN_CELLS);
                true
            }
            _ => false,
        };
        if changed {
            let (c, v) = self.command(s, self.drag.last, self.drag.mods);
            vec![Action::Preview(c, v)]
        } else {
            vec![]
        }
    }
    fn overlays(&self, cx: &ToolContext) -> Vec<Overlay> {
        let mut o = self.drag.snap.guides().to_vec();
        if let Some(s) = self.drag.start.filter(|_| self.drag.began && cx.measurement_labels) {
            let r = drag_rect(s, self.drag.last, self.drag.mods);
            o.push(Overlay::Measure { p: self.drag.last, text: cx.size_label(r.width(), r.height()) });
        }
        o
    }
    fn cursor(&self, _cx: &ToolContext, _p: Point, _m: Mods) -> Cursor {
        Cursor::Crosshair
    }
    fn options(&self) -> Value {
        json!({"rows": self.rows, "cols": self.cols, "spacing": self.spacing})
    }
    fn set_option(&mut self, key: &str, v: &Value) {
        match key {
            "rows" => self.rows = v.as_u64().unwrap_or(3).clamp(u64::from(MIN_CELLS), u64::from(MAX_CELLS)) as u32,
            "cols" | "columns" => self.cols = v.as_u64().unwrap_or(3).clamp(u64::from(MIN_CELLS), u64::from(MAX_CELLS)) as u32,
            "spacing" => self.spacing = v.as_f64().unwrap_or(10.0).clamp(0.1, 100_000.0),
            _ => {}
        }
    }
}

/// The Line Point tool: a line drag places `count` dots equally spaced along it, ends included.
pub struct LinePointTool {
    drag: Drag,
    pub count: u32,
}

impl LinePointTool {
    pub fn new() -> Self {
        Self { drag: Drag::default(), count: 5 }
    }

    /// The command for a drag from `start` to `p` (Shift keeps it to 45° steps, Alt draws it
    /// mirrored through the start, as the Line Segment tool does).
    pub fn command(&self, start: Point, p: Point, m: Mods) -> (String, Value) {
        let (a, b) = if m.alt { (start - (p - start), p) } else { (start, p) };
        ("shape.linePoint".into(), json!({"x1": a.x, "y1": a.y, "x2": b.x, "y2": b.y, "count": self.count}))
    }
}

impl Default for LinePointTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for LinePointTool {
    fn id(&self) -> &'static str {
        "linePoint"
    }
    fn busy(&self) -> bool {
        self.drag.start.is_some()
    }
    fn pointer(&mut self, cx: &ToolContext, ev: &PointerEvent) -> Vec<Action> {
        let (mut out, ended) = self.drag.pointer(cx, ev, "Line Point", |cx, s, m| Some(Leave::segment(cx, s, m.shift)));
        match ended {
            Some(Ended::Drag { .. }) if ev.kind == PointerKind::Up => out.push(Action::Commit),
            Some(Ended::Drag { start, end }) => {
                let (c, v) = self.command(start, end, self.drag.mods);
                out.push(Action::Preview(c, v));
            }
            // A click has no line to space the dots along: nothing to place.
            None | Some(Ended::Click) => {}
        }
        out
    }
    fn key(&mut self, _cx: &ToolContext, key: ToolKey, _mods: Mods) -> Vec<Action> {
        if key == ToolKey::Escape {
            self.drag.start = None;
            self.drag.began = false;
            self.drag.snap.clear();
            return vec![Action::Cancel];
        }
        let Some(s) = self.drag.start.filter(|_| self.drag.began) else { return vec![] };
        let changed = match key {
            ToolKey::Up => {
                self.count = (self.count + 1).min(MAX_CELLS);
                true
            }
            ToolKey::Down => {
                self.count = self.count.saturating_sub(1).max(MIN_LINE_DOTS);
                true
            }
            _ => false,
        };
        if changed {
            let (c, v) = self.command(s, self.drag.last, self.drag.mods);
            vec![Action::Preview(c, v)]
        } else {
            vec![]
        }
    }
    fn overlays(&self, cx: &ToolContext) -> Vec<Overlay> {
        let mut o = self.drag.snap.guides().to_vec();
        if let Some(s) = self.drag.start.filter(|_| self.drag.began && cx.measurement_labels) {
            o.push(Overlay::Measure { p: self.drag.last, text: cx.size_label((self.drag.last - s).x.abs(), (self.drag.last - s).y.abs()) });
        }
        o
    }
    fn cursor(&self, _cx: &ToolContext, _p: Point, _m: Mods) -> Cursor {
        Cursor::Crosshair
    }
    fn options(&self) -> Value {
        json!({"count": self.count})
    }
    fn set_option(&mut self, key: &str, v: &Value) {
        if key == "count" {
            self.count = v.as_u64().unwrap_or(5).clamp(u64::from(MIN_LINE_DOTS), u64::from(MAX_CELLS)) as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use terrazon_doc::Selection;

    /// A document, a paint proxy and a context without Smart Guides, so drags land exactly where
    /// the test puts them.
    macro_rules! plain {
        ($d:ident, $s:ident, $p:ident, $cx:ident) => {
            let ($d, _) = doc_with_rect();
            let $s = Selection::default();
            let $p = paint();
            let mut $cx = cx(&$d, &$s, &$p);
            $cx.smart_guides = false;
        };
    }

    #[test]
    fn array_geometry_two_by_three() {
        let pts = array_centres(10.0, 20.0, 90.0, 60.0, 2, 3);
        assert_eq!(pts.len(), 6);
        // Cell size 90/3 × 60/2 = 30 × 30, dots at the cells' centres.
        let want = [(25.0, 35.0), (55.0, 35.0), (85.0, 35.0), (25.0, 65.0), (55.0, 65.0), (85.0, 65.0)];
        for (p, (x, y)) in pts.iter().zip(want) {
            assert_eq!(*p, Point::new(x, y));
        }
        // A degenerate one-cell array is one dot at the middle; a zero-width marquee keeps its
        // rows on the one line.
        assert_eq!(array_centres(0.0, 0.0, 10.0, 10.0, 1, 1), vec![Point::new(5.0, 5.0)]);
        assert_eq!(array_centres(0.0, 0.0, 0.0, 20.0, 2, 1), vec![Point::new(0.0, 5.0), Point::new(0.0, 15.0)]);
    }

    #[test]
    fn line_geometry_count_five() {
        let pts = line_points(Point::new(0.0, 0.0), Point::new(100.0, 0.0), 5);
        assert_eq!(pts.len(), 5);
        for (p, x) in pts.iter().zip([0.0, 25.0, 50.0, 75.0, 100.0]) {
            assert_eq!(*p, Point::new(x, 0.0));
        }
        // Ends included, whatever the direction; two is the floor.
        assert_eq!(
            line_points(Point::new(30.0, 60.0), Point::new(0.0, 0.0), 4),
            vec![Point::new(30.0, 60.0), Point::new(20.0, 40.0), Point::new(10.0, 20.0), Point::new(0.0, 0.0)]
        );
        assert_eq!(line_points(Point::new(1.0, 1.0), Point::new(9.0, 1.0), 1), vec![Point::new(1.0, 1.0), Point::new(9.0, 1.0)]);
    }

    #[test]
    fn array_drag_previews_and_commits() {
        plain!(_d, _s, _p, cx);
        let mut t = ArrayPointTool::new();
        t.pointer(&cx, &PointerEvent::new(PointerKind::Down, 10.0, 10.0));
        let a = t.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 100.0, 70.0));
        assert_eq!(a[0], Action::Begin("Array Point".into()));
        assert_eq!(
            a[1],
            Action::Preview("shape.arrayPoint".into(), json!({"x": 10.0, "y": 10.0, "width": 90.0, "height": 60.0, "rows": 3, "columns": 3}))
        );
        // Shift makes the marquee square, Alt draws it from its centre (as the Rectangle tool's).
        let a = t.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 130.0, 90.0).with_mods(Mods { shift: true, ..Default::default() }));
        assert!(matches!(&a[..], [Action::Preview(_, v)] if v["width"] == 120.0 && v["height"] == 120.0), "{a:?}");
        assert_eq!(t.pointer(&cx, &PointerEvent::new(PointerKind::Up, 130.0, 90.0)), vec![Action::Commit]);
        assert!(!t.busy());
    }

    #[test]
    fn array_click_places_the_options_array() {
        plain!(_d, _s, _p, cx);
        let mut t = ArrayPointTool::new();
        t.set_option("rows", &json!(2));
        t.set_option("cols", &json!(2));
        t.set_option("spacing", &json!(20.0));
        t.pointer(&cx, &PointerEvent::new(PointerKind::Down, 100.0, 100.0));
        let a = t.pointer(&cx, &PointerEvent::new(PointerKind::Up, 100.0, 100.0));
        assert_eq!(
            a,
            vec![Action::Exec(
                "shape.arrayPoint".into(),
                json!({"x": 80.0, "y": 80.0, "width": 40.0, "height": 40.0, "rows": 2, "columns": 2, "spacing": 20.0})
            )]
        );
    }

    #[test]
    fn array_arrows_and_escape() {
        plain!(_d, _s, _p, cx);
        let mut t = ArrayPointTool::new();
        t.pointer(&cx, &PointerEvent::new(PointerKind::Down, 10.0, 10.0));
        t.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 110.0, 60.0));
        let a = t.key(&cx, ToolKey::Up, Mods::default());
        assert!(matches!(&a[0], Action::Preview(_, v) if v["rows"] == 4));
        let a = t.key(&cx, ToolKey::Right, Mods::default());
        assert!(matches!(&a[0], Action::Preview(_, v) if v["columns"] == 4));
        let a = t.key(&cx, ToolKey::Down, Mods::default());
        assert!(matches!(&a[0], Action::Preview(_, v) if v["rows"] == 3));
        let a = t.key(&cx, ToolKey::Left, Mods::default());
        assert!(matches!(&a[0], Action::Preview(_, v) if v["columns"] == 3));
        // Arrows before the drag begins do nothing (there is nothing to resize yet).
        let mut t2 = ArrayPointTool::new();
        t2.pointer(&cx, &PointerEvent::new(PointerKind::Down, 10.0, 10.0));
        assert!(t2.key(&cx, ToolKey::Up, Mods::default()).is_empty());
        assert_eq!(t.key(&cx, ToolKey::Escape, Mods::default()), vec![Action::Cancel]);
        assert!(!t.busy());
    }

    #[test]
    fn array_options_clamp() {
        let mut t = ArrayPointTool::new();
        t.set_option("rows", &json!(0));
        t.set_option("cols", &json!(999));
        t.set_option("spacing", &json!(-5.0));
        assert_eq!(t.options(), json!({"rows": 1, "cols": 64, "spacing": 0.1}));
        // `columns` is accepted as the engine names it.
        t.set_option("columns", &json!(5));
        assert_eq!(t.options()["cols"], json!(5));
    }

    #[test]
    fn line_drag_previews_and_commits() {
        plain!(_d, _s, _p, cx);
        let mut t = LinePointTool::new();
        t.pointer(&cx, &PointerEvent::new(PointerKind::Down, 100.0, 100.0));
        let a = t.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 300.0, 100.0));
        assert_eq!(a[0], Action::Begin("Line Point".into()));
        assert_eq!(a[1], Action::Preview("shape.linePoint".into(), json!({"x1": 100.0, "y1": 100.0, "x2": 300.0, "y2": 100.0, "count": 5})));
        assert_eq!(t.pointer(&cx, &PointerEvent::new(PointerKind::Up, 300.0, 100.0)), vec![Action::Commit]);
        // Shift keeps the drag to 45° steps (the Line Segment tool's constraint).
        let mut t = LinePointTool::new();
        let shift = Mods { shift: true, ..Default::default() };
        t.pointer(&cx, &PointerEvent::new(PointerKind::Down, 100.0, 100.0));
        let a = t.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 200.0, 200.0).with_mods(shift));
        assert!(matches!(&a[1], Action::Preview(_, v) if v["x2"] == 200.0 && v["y2"] == 200.0), "{a:?}");
        // Alt mirrors the line through the start.
        let a = t.command(Point::new(100.0, 100.0), Point::new(150.0, 120.0), Mods { alt: true, ..Default::default() });
        assert_eq!(a.1, json!({"x1": 50.0, "y1": 80.0, "x2": 150.0, "y2": 120.0, "count": 5}));
    }

    #[test]
    fn line_click_draws_nothing() {
        plain!(_d, _s, _p, cx);
        let mut t = LinePointTool::new();
        t.pointer(&cx, &PointerEvent::new(PointerKind::Down, 100.0, 100.0));
        assert_eq!(t.pointer(&cx, &PointerEvent::new(PointerKind::Up, 100.0, 100.0)), vec![]);
        assert!(!t.busy());
    }

    #[test]
    fn line_count_clamps_and_arrows() {
        let mut t = LinePointTool::new();
        t.set_option("count", &json!(1));
        assert_eq!(t.options()["count"], json!(2));
        t.set_option("count", &json!(100));
        assert_eq!(t.options()["count"], json!(64));
        plain!(_d, _s, _p, cx);
        t.pointer(&cx, &PointerEvent::new(PointerKind::Down, 0.0, 0.0));
        t.pointer(&cx, &PointerEvent::new(PointerKind::Drag, 100.0, 0.0));
        let a = t.key(&cx, ToolKey::Up, Mods::default());
        assert!(matches!(&a[0], Action::Preview(_, v) if v["count"] == 64));
        let a = t.key(&cx, ToolKey::Down, Mods::default());
        assert!(matches!(&a[0], Action::Preview(_, v) if v["count"] == 63));
    }

    #[test]
    fn tools_are_created_by_id() {
        assert_eq!(crate::create("arrayPoint").id(), "arrayPoint");
        assert_eq!(crate::create("linePoint").id(), "linePoint");
        assert_eq!(crate::tool_info("arrayPoint").map(|t| t.label), Some("Array Point Tool"));
        assert_eq!(crate::tool_info("linePoint").map(|t| t.label), Some("Line Point Tool"));
    }
}
