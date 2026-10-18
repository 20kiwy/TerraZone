//! Point placement tools driven through the session (tools → actions → `shape.arrayPoint` /
//! `shape.linePoint` commands), and the commands themselves.

use serde_json::json;
use terrazon_doc::{NodeId, NodeKind};
use terrazon_geom::Point;
use terrazon_tools::{PointerEvent, PointerKind};

use super::*;
use crate::tooling::ViewInfo;

fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({"width": 800, "height": 600})).unwrap();
    s
}

fn view() -> ViewInfo {
    ViewInfo { smart_guides: false, ..Default::default() }
}

/// A gesture of `tool`: press at the first point, drag through the others, release at the last.
fn drag(s: &mut Session, tool: &str, pts: &[(f64, f64)]) {
    let v = view();
    s.select_tool(tool, v).unwrap();
    s.pointer(&PointerEvent::new(PointerKind::Down, pts[0].0, pts[0].1), v).unwrap();
    for &(x, y) in &pts[1..] {
        s.pointer(&PointerEvent::new(PointerKind::Drag, x, y), v).unwrap();
    }
    let l = pts[pts.len() - 1];
    s.pointer(&PointerEvent::new(PointerKind::Up, l.0, l.1), v).unwrap();
}

fn click(s: &mut Session, tool: &str, x: f64, y: f64) {
    drag(s, tool, &[(x, y)]);
}

/// The groups the point tools place, as (name, dot centres): their children are the dots, small
/// circles whose bounds centre is the dot's place.
fn dot_groups(s: &Session) -> Vec<(String, Vec<Point>)> {
    let mut v = vec![];
    s.doc().unwrap().doc.walk(|n| {
        if let NodeKind::Group { children, .. } = &n.kind
            && matches!(n.name.as_deref(), Some("Array Point" | "Line Point"))
        {
            let dots = children.iter().filter_map(|c| c.path_data().and_then(|pd| pd.bounds()).map(|b| b.center())).collect();
            v.push((n.name.clone().unwrap_or_default(), dots));
        }
    });
    v
}

fn dots(s: &Session) -> Vec<Point> {
    dot_groups(s).into_iter().flat_map(|(_, dots)| dots).collect()
}

fn near(a: Point, b: Point) -> bool {
    a.distance(b) < 1e-6
}

#[test]
fn array_point_places_two_by_three() {
    let mut s = session();
    let r = s.execute("shape.arrayPoint", &json!({"x": 10.0, "y": 20.0, "width": 90.0, "height": 60.0, "rows": 2, "columns": 3})).unwrap();
    assert_eq!(r["count"], json!(6));
    let gs = dot_groups(&s);
    assert_eq!(gs.len(), 1);
    assert_eq!(gs[0].0, "Array Point");
    let want = [(25.0, 35.0), (55.0, 35.0), (85.0, 35.0), (25.0, 65.0), (55.0, 65.0), (85.0, 65.0)];
    assert_eq!(gs[0].1.len(), want.len());
    for (p, (x, y)) in gs[0].1.iter().zip(want) {
        assert!(near(*p, Point::new(x, y)), "{p:?} != ({x}, {y})");
    }
    // The group is selected, and its dots are small filled circles in the current paint.
    let gid = NodeId(r["id"].as_u64().unwrap());
    assert_eq!(s.doc().unwrap().selection.objects, vec![gid]);
    let g = s.doc().unwrap().doc.node(gid).unwrap().clone();
    for c in g.children().unwrap() {
        let b = c.path_data().unwrap().bounds().unwrap();
        assert!((b.width() - terrazon_tools::points::DOT).abs() < 1e-9 && (b.height() - terrazon_tools::points::DOT).abs() < 1e-9);
        assert!(!c.appearance.fill_paint().is_none());
    }
}

#[test]
fn array_point_spacing_derives_the_marquee() {
    let mut s = session();
    // With `spacing` the rectangle is spacing×columns × spacing×rows from the top-left corner.
    s.execute("shape.arrayPoint", &json!({"x": 100.0, "y": 100.0, "spacing": 20.0, "rows": 2, "columns": 2})).unwrap();
    let mut got = dots(&s);
    got.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let want = [(110.0, 110.0), (110.0, 130.0), (130.0, 110.0), (130.0, 130.0)];
    assert_eq!(got.len(), want.len());
    for (p, (x, y)) in got.iter().zip(want) {
        assert!(near(*p, Point::new(x, y)), "{p:?}");
    }
    // `cols` is accepted where `columns` is.
    let r = s.execute("shape.arrayPoint", &json!({"x": 0.0, "y": 0.0, "spacing": 10.0, "rows": 1, "cols": 4})).unwrap();
    assert_eq!(r["count"], json!(4));
    assert_eq!(dot_groups(&s).len(), 2);
}

#[test]
fn line_point_places_five_on_the_line() {
    let mut s = session();
    let r = s.execute("shape.linePoint", &json!({"x1": 100.0, "y1": 100.0, "x2": 300.0, "y2": 100.0, "count": 5})).unwrap();
    assert_eq!(r["count"], json!(5));
    let gs = dot_groups(&s);
    assert_eq!(gs.len(), 1);
    assert_eq!(gs[0].0, "Line Point");
    // Equally spaced, ends included.
    for (p, x) in gs[0].1.iter().zip([100.0, 150.0, 200.0, 250.0, 300.0]) {
        assert!(near(*p, Point::new(x, 100.0)), "{p:?}");
    }
    // Diagonally too, and the default count is 5.
    s.execute("shape.linePoint", &json!({"x1": 0.0, "y1": 0.0, "x2": 30.0, "y2": 60.0})).unwrap();
    let g = &dot_groups(&s)[1].1;
    assert_eq!(g.len(), 5);
    for (p, (x, y)) in g.iter().zip([(0.0, 0.0), (7.5, 15.0), (15.0, 30.0), (22.5, 45.0), (30.0, 60.0)]) {
        assert!(near(*p, Point::new(x, y)), "{p:?}");
    }
}

#[test]
fn array_and_line_point_params_clamp() {
    let mut s = session();
    // Rows and columns land in 1..=64, the line's count in 2..=64.
    s.execute("shape.arrayPoint", &json!({"x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0, "rows": 0, "columns": 999})).unwrap();
    assert_eq!(dots(&s).len(), 64);
    s.execute("shape.linePoint", &json!({"x1": 0.0, "y1": 0.0, "x2": 10.0, "y2": 0.0, "count": 1})).unwrap();
    assert_eq!(dots(&s).len(), 66);
    s.execute("shape.linePoint", &json!({"x1": 0.0, "y1": 0.0, "x2": 10.0, "y2": 0.0, "count": 100})).unwrap();
    assert_eq!(dots(&s).len(), 130);
    // Without a marquee or a spacing there is nothing to place the array on.
    let e = s.execute("shape.arrayPoint", &json!({"x": 0.0, "y": 0.0})).unwrap_err();
    assert!(e.to_string().contains("spacing"), "{e}");
    let e = s.execute("shape.arrayPoint", &json!({"x": 0.0, "y": 0.0, "spacing": -1.0})).unwrap_err();
    assert!(e.to_string().contains("positive"), "{e}");
}

#[test]
fn array_point_tool_drag_is_one_undo_step() {
    let mut s = session();
    s.select_tool("arrayPoint", view()).unwrap();
    s.set_tool_option("rows", &json!(2));
    s.set_tool_option("cols", &json!(3));
    let undo = s.doc().unwrap().history.undo.len();
    drag(&mut s, "arrayPoint", &[(10.0, 10.0), (100.0, 70.0)]);
    // A 2 × 3 array fills the marquee, as one undo step.
    assert_eq!(dots(&s).len(), 6);
    assert_eq!(s.doc().unwrap().history.undo.len(), undo + 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(dots(&s).is_empty());
}

#[test]
fn line_point_tool_drag_is_one_undo_step() {
    let mut s = session();
    let undo = s.doc().unwrap().history.undo.len();
    drag(&mut s, "linePoint", &[(100.0, 100.0), (300.0, 100.0)]);
    assert_eq!(dots(&s).len(), 5);
    assert_eq!(s.doc().unwrap().history.undo.len(), undo + 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(dots(&s).is_empty());
}

#[test]
fn array_point_click_uses_the_tool_options() {
    let mut s = session();
    s.select_tool("arrayPoint", view()).unwrap();
    s.set_tool_option("rows", &json!(2));
    s.set_tool_option("cols", &json!(2));
    s.set_tool_option("spacing", &json!(20.0));
    let undo = s.doc().unwrap().history.undo.len();
    click(&mut s, "arrayPoint", 200.0, 200.0);
    let mut got = dots(&s);
    got.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let want = [(190.0, 190.0), (190.0, 210.0), (210.0, 190.0), (210.0, 210.0)];
    assert_eq!(got.len(), want.len());
    for (p, (x, y)) in got.iter().zip(want) {
        assert!(near(*p, Point::new(x, y)), "{p:?}");
    }
    // A click is its own undo step.
    assert_eq!(s.doc().unwrap().history.undo.len(), undo + 1);
}

#[test]
fn line_point_click_draws_nothing() {
    let mut s = session();
    let undo = s.doc().unwrap().history.undo.len();
    click(&mut s, "linePoint", 200.0, 200.0);
    assert!(dots(&s).is_empty());
    assert_eq!(s.doc().unwrap().history.undo.len(), undo);
}
