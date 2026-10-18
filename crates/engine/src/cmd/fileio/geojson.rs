//! Opening, placing and exporting GeoJSON (RFC 7946): WGS84 geometry (degrees; [lon, lat]) drawn
//! onto the Web Mercator world (`terrazon_map`), so every feature lands on the tile basemap and
//! behaves like any vector art. Polygon rings import with EvenOdd filling, so holes read back;
//! feature `name`/`title` properties become the Layers panel names ("edit zones and name well").

use serde_json::{Value, json};
use terrazon_color::{Color, Paint};
use terrazon_doc::{Appearance, Document, Node, NodeKind};
use terrazon_geom::{FillRule, PathData, Point, SubPath};
use terrazon_map::{WORLD_PT, lonlat_to_doc};

use super::super::*;
use super::err;

/// How many objects one import may make (a guard against hostile or runaway files).
const MAX_NODES: usize = 20_000;
/// How many coordinate pairs one import may read.
const MAX_POINTS: usize = 2_000_000;
/// How many points one ring or line may take.
const MAX_RING: usize = 100_000;

/// A fresh map document for imported data: the world artboard with tiles on.
fn map_document() -> Document {
    Document::new(WORLD_PT, WORLD_PT)
}

/// Import GeoJSON bytes → a document with one group of features, and import notes.
pub(super) fn import(bytes: &[u8]) -> Result<(Document, Vec<String>)> {
    let text = std::str::from_utf8(bytes).map_err(|_| err("a GeoJSON file is UTF-8 text (JSON), and this isn't"))?;
    let v: Value = serde_json::from_str(text).map_err(|e| err(format!("not valid JSON: {e}")))?;
    let mut doc = map_document();
    let layer = doc.layers[0].id;
    let mut budget = Budget::default();
    let mut made: Vec<Node> = Vec::new();
    let mut warnings = Vec::new();
    let name_of = |props: Option<&Value>, fallback: usize| -> String {
        props
            .and_then(|p| {
                ["name", "NAME", "Name", "title", "TITLE", "label", "id"]
                    .iter()
                    .find_map(|k| p.get(*k).and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
            })
            .unwrap_or_else(|| format!("Zone {fallback}"))
    };
    // Features of a FeatureCollection, a bare Feature, a GeometryCollection, or a bare geometry.
    let pairs: Vec<(Option<&Value>, &Value)> = match v.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => v
            .get("features")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|f| f.get("geometry").filter(|g| g.is_object()).map(|g| (f.get("properties"), g))).collect())
            .unwrap_or_default(),
        Some("Feature") => v.get("geometry").filter(|g| g.is_object()).map(|g| vec![(v.get("properties"), g)]).unwrap_or_default(),
        Some("GeometryCollection") => {
            v.get("geometries").and_then(Value::as_array).map(|a| a.iter().map(|g| (None as Option<&Value>, g)).collect()).unwrap_or_default()
        }
        Some(_) if v.is_object() => vec![(None as Option<&Value>, &v)],
        _ => Vec::new(),
    };
    for (i, (props, geom)) in pairs.iter().enumerate() {
        if made.len() >= MAX_NODES {
            warnings.push(format!("stopped at {MAX_NODES} objects: the file has more features than that"));
            break;
        }
        let label = name_of(*props, i + 1);
        let before = made.len();
        node_for_geometry(&mut doc, &mut budget, &label, geom, &mut made);
        if made.len() == before {
            warnings.push(format!("{label}: no mappable geometry (the type isn't Point, LineString or Polygon, or their Multi forms)"));
        }
    }
    if made.is_empty() {
        return Err(err("no mappable geometry in this file (its features have no Point, LineString or Polygon shapes)"));
    }
    if budget.non_geo > 0 {
        warnings.push(format!(
            "{} coordinate pairs were outside longitude/latitude (|lon| > 180 or |lat| > 90): the file may be in a projected CRS; those still placed as if longitude/latitude",
            budget.non_geo
        ));
    }
    // Point markers scale to the data's spread, so they read at the zoom the file is used at.
    let r = marker_radius(&made);
    for n in made.iter_mut() {
        scale_markers(n, r);
    }
    let id = doc.alloc_id();
    let mut group = Node::group(id, made.drain(..).map(std::sync::Arc::new).collect());
    group.name = Some("GeoJSON".into());
    doc.insert(Some(layer), usize::MAX, group).map_err(err)?;
    Ok((doc, warnings))
}

/// A bundled sample's features as one named group node (the New Document templates' data): the
/// "GeoJSON" group of an import, renamed to `label`.
pub(crate) fn sample_group(bytes: &[u8], label: &str) -> Result<Node> {
    let (doc, _) = import(bytes)?;
    let layer = doc.layers.first().ok_or_else(|| err("the sample came out empty"))?;
    let group = layer.children().and_then(|c| c.last()).ok_or_else(|| err("the sample came out empty"))?;
    let mut n = (**group).clone();
    n.name = Some(label.to_string());
    Ok(n)
}

/// Read-progress guards shared by one import.
#[derive(Default)]
struct Budget {
    points: usize,
    non_geo: usize,
    capped: bool,
}

impl Budget {
    /// Take one coordinate pair (lon, lat) → document points, counting guards.
    fn take(&mut self, lon: f64, lat: f64) -> Option<Point> {
        self.points += 1;
        if self.points > MAX_POINTS || self.capped || !lon.is_finite() || !lat.is_finite() {
            return None;
        }
        if !(-180.5..=180.5).contains(&lon) || !(-90.5..=90.5).contains(&lat) {
            self.non_geo += 1;
        }
        let (x, y) = lonlat_to_doc(lon, lat);
        Some(Point::new(x, y))
    }
}

/// Append the node one geometry makes (a Multi form becomes one multi-subpath node, so a zone
/// stays one object to name and style).
fn node_for_geometry(doc: &mut Document, budget: &mut Budget, label: &str, g: &Value, out: &mut Vec<Node>) {
    let Some(coords) = g.get("coordinates").filter(|c| c.is_array()) else { return };
    let ty = g.get("type").and_then(Value::as_str).unwrap_or("");
    let subpaths: Vec<SubPath> = match ty {
        "Point" => point_ring(budget, coords, 1.0).into_iter().collect(),
        "MultiPoint" => coords.as_array().map(|a| a.iter().filter_map(|c| point_ring(budget, c, 1.0)).collect()).unwrap_or_default(),
        "LineString" => line(budget, coords).into_iter().collect(),
        "MultiLineString" => coords.as_array().map(|a| a.iter().filter_map(|c| line(budget, c)).collect()).unwrap_or_default(),
        "Polygon" => polygon_rings(budget, coords),
        "MultiPolygon" => coords.as_array().map(|a| a.iter().flat_map(|poly| polygon_rings(budget, poly)).collect()).unwrap_or_default(),
        _ => Vec::new(),
    };
    if subpaths.is_empty() {
        return;
    }
    let closed = matches!(ty, "Polygon" | "MultiPolygon" | "Point" | "MultiPoint");
    let id = doc.alloc_id();
    let mut n = Node::new(
        id,
        NodeKind::Path {
            path: PathData::new(subpaths),
            rule: if closed { FillRule::EvenOdd } else { FillRule::NonZero },
            live: None,
            clipping: false,
            guide: false,
        },
    );
    // Zones read as map art: filled, outlined areas; outlined routes; markers.
    if ty == "LineString" || ty == "MultiLineString" {
        n.appearance = Appearance::basic(Paint::None, Paint::solid(Color::rgb8(36, 99, 82)), 1.5);
    } else if ty == "Point" || ty == "MultiPoint" {
        n.appearance = Appearance::basic(Paint::solid(Color::rgb8(196, 106, 58)), Paint::solid(Color::rgb8(146, 74, 32)), 1.0);
    } else {
        n.appearance = Appearance::basic(Paint::solid(Color::rgb8(232, 168, 124)), Paint::solid(Color::rgb8(146, 74, 32)), 1.0);
    }
    n.name = Some(label.to_string());
    out.push(n);
}

/// A marker circle of `r` document points about a coordinate (a 12-gon of corners, closed; scaled
/// to the data's spread once the whole import is in).
fn point_ring(budget: &mut Budget, coords: &Value, r: f64) -> Option<SubPath> {
    let c = coords.as_array()?;
    let lon = c.first()?.as_f64()?;
    let lat = c.get(1)?.as_f64()?;
    let p = budget.take(lon, lat)?;
    Some(circle_at(p, r))
}

fn circle_at(p: Point, r: f64) -> SubPath {
    let mut pts = Vec::with_capacity(12);
    for i in 0..12 {
        let a = i as f64 * std::f64::consts::TAU / 12.0;
        pts.push(Point::new(p.x + r * a.cos(), p.y + r * a.sin()));
    }
    SubPath::polyline(&pts, true)
}

/// Scale every round 12-corner ring (a marker) of a node's path about its centre to `r`.
fn scale_markers(n: &mut Node, r: f64) {
    let NodeKind::Path { path, .. } = &mut n.kind else { return };
    for sp in path.subpaths.iter_mut() {
        if sp.anchors.len() != 12 || !sp.closed {
            continue;
        }
        let c = Point::new(sp.anchors.iter().map(|a| a.p.x).sum::<f64>() / 12.0, sp.anchors.iter().map(|a| a.p.y).sum::<f64>() / 12.0);
        let r0 = (sp.anchors[0].p.x - c.x).hypot(sp.anchors[0].p.y - c.y);
        let even = r0 > 0.0 && sp.anchors.iter().all(|a| ((a.p.x - c.x).hypot(a.p.y - c.y) - r0).abs() < r0 * 0.05);
        if !even {
            continue;
        }
        for a in sp.anchors.iter_mut() {
            let dx = a.p.x - c.x;
            let dy = a.p.y - c.y;
            a.p = Point::new(c.x + dx * r / r0, c.y + dy * r / r0);
            a.h_in = a.p;
            a.h_out = a.p;
        }
    }
}

/// A marker size that reads for this import: 2% of the imported art's spread, at least ½ pt.
fn marker_radius(made: &[Node]) -> f64 {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for n in made {
        let NodeKind::Path { path, .. } = &n.kind else { continue };
        for sp in &path.subpaths {
            for a in &sp.anchors {
                x0 = x0.min(a.p.x);
                y0 = y0.min(a.p.y);
                x1 = x1.max(a.p.x);
                y1 = y1.max(a.p.y);
            }
        }
    }
    let diag = (x1 - x0).hypot(y1 - y0);
    if diag.is_finite() && diag > 0.0 { (diag * 0.02).max(0.5) } else { 0.5 }
}

/// An open line as a subpath.
fn line(budget: &mut Budget, coords: &Value) -> Option<SubPath> {
    let a = coords.as_array()?;
    let mut pts = Vec::with_capacity(a.len().min(MAX_RING));
    for c in a {
        let (Some(lon), Some(lat)) = (c.get(0).and_then(Value::as_f64), c.get(1).and_then(Value::as_f64)) else { continue };
        match budget.take(lon, lat) {
            Some(p) if pts.len() < MAX_RING => pts.push(p),
            _ => {}
        }
    }
    (pts.len() >= 2).then(|| SubPath::polyline(&pts, false))
}

/// A polygon's rings (first the shell, then holes), as closed subpaths.
fn polygon_rings(budget: &mut Budget, coords: &Value) -> Vec<SubPath> {
    coords
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|ring| {
                    line(budget, ring).map(|mut sp| {
                        sp.closed = true;
                        sp
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Export the document's path art as a GeoJSON FeatureCollection: every closed subpath a Polygon
/// ring, every open one a LineString, round markers their centre Point. Names become `name`
/// properties; anything that isn't path art (text, images, effects) is skipped.
pub(super) fn encode(doc: &Document) -> Result<Vec<u8>> {
    const C: &str = "document.exportAs";
    let mut names = std::collections::HashSet::new();
    let mut features: Vec<Value> = Vec::new();
    let mut walk = |n: &Node, features: &mut Vec<Value>, names: &mut std::collections::HashSet<String>| {
        let NodeKind::Path { path, .. } = &n.kind else { return };
        let name = n.name.clone().unwrap_or_else(|| "Zone".into());
        let mut base = name.clone();
        let mut k = 2;
        while !names.insert(base.clone()) {
            base = format!("{name} {k}");
            k += 1;
        }
        let mut polys: Vec<Value> = Vec::new();
        let mut lines: Vec<Value> = Vec::new();
        let mut points: Vec<Value> = Vec::new();
        let mut single = 0usize;
        for sp in &path.subpaths {
            let pts = flatten(sp);
            if pts.len() < 2 {
                continue;
            }
            single += 1;
            let ring: Vec<Value> = pts
                .iter()
                .map(|p| {
                    let (lon, lat) = terrazon_map::doc_to_lonlat(p.x, p.y);
                    json!([round7(lon), round7(lat)])
                })
                .collect();
            if sp.closed {
                // A small, even 12-corner ring is a marker: its centre exports as a Point.
                if pts.len() == 12
                    && let Some(c) = marker_centre(&pts)
                {
                    let (lon, lat) = terrazon_map::doc_to_lonlat(c.x, c.y);
                    points.push(json!([round7(lon), round7(lat)]));
                    continue;
                }
                polys.push(json!(ring));
            } else {
                lines.push(json!(ring));
            }
        }
        if single == 0 {
            return;
        }
        let geometry = if !polys.is_empty() {
            if polys.len() == 1 {
                json!({ "type": "Polygon", "coordinates": polys[0] })
            } else {
                json!({ "type": "MultiPolygon", "coordinates": polys })
            }
        } else if !lines.is_empty() {
            if lines.len() == 1 {
                json!({ "type": "LineString", "coordinates": lines[0] })
            } else {
                json!({ "type": "MultiLineString", "coordinates": lines })
            }
        } else if points.len() == 1 {
            json!({ "type": "Point", "coordinates": points[0] })
        } else {
            json!({ "type": "MultiPoint", "coordinates": points })
        };
        features.push(json!({ "type": "Feature", "properties": { "name": base }, "geometry": geometry }));
    };
    fn walk_all(
        n: &Node,
        features: &mut Vec<Value>,
        names: &mut std::collections::HashSet<String>,
        walk: &mut impl FnMut(&Node, &mut Vec<Value>, &mut std::collections::HashSet<String>),
    ) {
        walk(n, features, names);
        if let Some(children) = n.children() {
            for c in children {
                if c.visible {
                    walk_all(c, features, names, walk);
                }
            }
        }
    }
    for layer in &doc.layers {
        if layer.visible {
            walk_all(layer, &mut features, &mut names, &mut walk);
        }
    }
    if features.is_empty() {
        return Err(bad(C, "nothing to export: the document has no path art (text, images and effects don't export to GeoJSON)"));
    }
    let mut out = json!({ "type": "FeatureCollection", "features": features });
    if let Some(o) = out.as_object_mut() {
        o.insert("generator".into(), json!(format!("TerraZone {}", env!("CARGO_PKG_VERSION"))));
    }
    serde_json::to_vec_pretty(&out).map_err(|e| bad(C, format!("can't encode: {e}")))
}

/// Cubic samples per handled segment when flattening for export.
const FLATTEN_SAMPLES: usize = 12;

/// A subpath as a polyline: handled segments sampled (imported GeoJSON has corners; drawn art
/// curves).
fn flatten(sp: &SubPath) -> Vec<Point> {
    let a = &sp.anchors;
    if a.len() < 2 {
        return a.iter().map(|an| an.p).collect();
    }
    let mut pts: Vec<Point> = Vec::with_capacity(a.len() * (FLATTEN_SAMPLES + 1));
    let n = if sp.closed { a.len() } else { a.len() - 1 };
    for i in 0..n {
        let j = (i + 1) % a.len();
        let (p0, p1) = (a[i].p, a[j].p);
        let c1 = if a[i].h_out == p0 { p0 } else { a[i].h_out };
        let c2 = if a[j].h_in == p1 { p1 } else { a[j].h_in };
        pts.push(p0);
        if !(c1 == p0 && c2 == p1) {
            for k in 1..FLATTEN_SAMPLES {
                let t = k as f64 / FLATTEN_SAMPLES as f64;
                pts.push(cubic(p0, c1, c2, p1, t));
            }
        }
    }
    if !sp.closed {
        pts.push(a[a.len() - 1].p);
    }
    pts
}

fn cubic(p0: Point, c1: Point, c2: Point, p1: Point, t: f64) -> Point {
    let u = 1.0 - t;
    Point::new(
        u * u * u * p0.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * p1.x,
        u * u * u * p0.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * p1.y,
    )
}

/// The centre of a small, even 12-corner ring (a marker), in document points.
fn marker_centre(pts: &[Point]) -> Option<Point> {
    let c = Point::new(pts.iter().map(|p| p.x).sum::<f64>() / 12.0, pts.iter().map(|p| p.y).sum::<f64>() / 12.0);
    let r0 = (pts[0].x - c.x).hypot(pts[0].y - c.y);
    if r0 <= 0.0 || r0 > 2.0 {
        return None;
    }
    pts.iter().all(|p| ((p.x - c.x).hypot(p.y - c.y) - r0).abs() < r0 * 0.05).then_some(c)
}

/// GeoJSON positions keep 7 decimals (about a centimetre).
fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const FC: &str = r#"{
        "type": "FeatureCollection",
        "features": [
            { "type": "Feature", "properties": { "name": "Harbour", "kind": "water" },
              "geometry": { "type": "Polygon", "coordinates": [[[4.9, 52.37], [4.95, 52.37], [4.95, 52.4], [4.9, 52.4], [4.9, 52.37]]] } },
            { "type": "Feature", "properties": { "name": "Ferry" },
              "geometry": { "type": "LineString", "coordinates": [[4.9, 52.37], [4.95, 52.4], [5.0, 52.42]] } },
            { "type": "Feature", "properties": {},
              "geometry": { "type": "Point", "coordinates": [4.9, 52.37] } }
        ]
    }"#;

    #[test]
    fn import_features() {
        let (doc, warnings) = import(FC.as_bytes()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        // One group with three named features on Layer 1.
        let kids = doc.layers[0].children().unwrap();
        assert_eq!(kids.len(), 1);
        let group = &kids[0];
        assert_eq!(group.name.as_deref(), Some("GeoJSON"));
        let features = group.children().unwrap();
        assert_eq!(features.len(), 3);
        assert_eq!(features[0].name.as_deref(), Some("Harbour"));
        assert!(matches!(&features[0].kind, NodeKind::Path { rule: FillRule::EvenOdd, .. }));
        assert_eq!(features[1].name.as_deref(), Some("Ferry"));
        // The unnamed point took a generated name.
        assert_eq!(features[2].name.as_deref(), Some("Zone 3"));
        // All inside the world square.
        let b = doc.art_bounds().unwrap();
        assert!(b.x0 >= -1.0 && b.x1 <= WORLD_PT + 1.0 && b.y0 >= -1.0 && b.y1 <= WORLD_PT + 1.0, "{b:?}");
    }

    #[test]
    fn round_trip() {
        let (doc, _) = import(FC.as_bytes()).unwrap();
        let bytes = encode(&doc).unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "FeatureCollection");
        let names: Vec<&str> = v["features"].as_array().unwrap().iter().filter_map(|f| f["properties"]["name"].as_str()).collect();
        assert_eq!(names, ["Harbour", "Ferry", "Zone 3"]);
        assert_eq!(v["features"][0]["geometry"]["type"], "Polygon");
        assert_eq!(v["features"][1]["geometry"]["type"], "LineString");
        assert_eq!(v["features"][2]["geometry"]["type"], "Point");
        // The geometry is back in lon/lat (a Polygon's first position).
        let first = v["features"][0]["geometry"]["coordinates"][0].as_array().unwrap();
        let (lon, lat) = (first[0].as_f64().unwrap(), first[1].as_f64().unwrap());
        assert!((lon - 4.9).abs() < 1e-4 && (lat - 52.37).abs() < 1e-3, "{lon} {lat}");
    }

    #[test]
    fn rejects_junk() {
        assert!(import(b"not json").is_err());
        assert!(import(br#"{"type": "FeatureCollection", "features": []}"#).is_err());
        assert!(import(br#"{"package": "x"}"#).is_err());
    }
}
