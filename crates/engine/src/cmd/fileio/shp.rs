//! Opening Shapefiles (`.shp`, the ESRI binary geometry format): points, multi-points, polylines
//! and polygons (2D; the Z and M variants' extra measures are skipped) placed onto the Web
//! Mercator world like GeoJSON. Only the `.shp` bytes are read here — the file dialog opens one
//! file — so zones take generated names ("Zone 1", …) to rename in the Layers panel. Coordinates
//! are read as longitude/latitude; files in a projected CRS still place (with a warning).

use terrazon_color::{Color, Paint};
use terrazon_doc::{Appearance, Document, Node, NodeKind};
use terrazon_geom::{FillRule, PathData, Point, SubPath};
use terrazon_map::{WORLD_PT, lonlat_to_doc};

use super::super::*;
use super::err;

/// How many shapes one import may make, and how many coordinate pairs it may read (guards against
/// hostile or runaway files).
const MAX_NODES: usize = 20_000;
const MAX_POINTS: usize = 2_000_000;
/// The `.shp` magic (file code) and the shape types this reads.
const FILE_CODE: u32 = 9994;
const SUPPORTED: &[(i32, &str)] = &[
    (0, "null"),
    (1, "point"),
    (3, "polyline"),
    (5, "polygon"),
    (8, "multipoint"),
    (11, "point Z"),
    (13, "polyline Z"),
    (15, "polygon Z"),
    (18, "multipoint Z"),
    (21, "point M"),
    (23, "polyline M"),
    (25, "polygon M"),
    (28, "multipoint M"),
];

/// Import `.shp` bytes → a document with one group of zones, and import notes.
pub(super) fn import(bytes: &[u8]) -> Result<(Document, Vec<String>)> {
    let mut p = Reader::new(bytes);
    if bytes.len() < 100 {
        return Err(err("not a Shapefile: shorter than its 100-byte header"));
    }
    let code = p.u32_be().ok_or_else(|| err("not a Shapefile: no file code"))?;
    if code != FILE_CODE {
        return Err(err(format!("not a Shapefile: its file code is {code}, not {FILE_CODE}")));
    }
    // Words 0..50 are the fixed header (file length, version, shape type, bounds); the records
    // follow at byte 100.
    let mut doc = Document::new(WORLD_PT, WORLD_PT);
    let layer = doc.layers[0].id;
    let mut nodes: Vec<Node> = Vec::new();
    let mut warnings = Vec::new();
    let mut non_geo = 0usize;
    let mut budget = 0usize;
    let mut skipped = std::collections::BTreeMap::new();
    let mut marker_pts: Vec<Point> = Vec::new();
    while nodes.len() < MAX_NODES {
        let Some(len) = p.record() else { break };
        budget += len;
        if budget > MAX_POINTS * 16 {
            break;
        }
        let Some(ty) = p.i32_le() else { break };
        let ty = ty as u32 as i32;
        if ty == 0 {
            continue;
        }
        let label = format!("Zone {}", nodes.len() + 1);
        let mut made: Vec<(SubPath, bool)> = Vec::new();
        match ty {
            1 | 11 | 21 => {
                let (Some(x), Some(y)) = (p.f64_le(), p.f64_le()) else { break };
                place_point(&mut made, &mut marker_pts, x, y, &mut non_geo);
            }
            8 | 18 | 28 => {
                let _box = p.f64_n(4);
                let Some(n) = p.i32_le().map(|n| n.unsigned_abs() as usize).filter(|n| *n <= MAX_POINTS) else { break };
                for _ in 0..n {
                    let (Some(x), Some(y)) = (p.f64_le(), p.f64_le()) else { break };
                    place_point(&mut made, &mut marker_pts, x, y, &mut non_geo);
                }
            }
            3 | 13 | 23 | 5 | 15 | 25 => {
                let _box = p.f64_n(4);
                let (Some(parts), Some(points)) = (p.i32_le(), p.i32_le()) else { break };
                let (parts, points) = (parts.unsigned_abs() as usize, points.unsigned_abs() as usize);
                if parts > 10_000 || points > MAX_POINTS {
                    warnings.push(format!("{label}: too large ({} parts, {} points), skipped", parts, points));
                    break;
                }
                let Some(starts) = p.u32_n(parts) else { break };
                let mut rings: Vec<Vec<Point>> = Vec::with_capacity(parts);
                for i in 0..parts {
                    let from = starts[i] as usize;
                    let to = starts.get(i + 1).copied().unwrap_or(points as u32) as usize;
                    if from >= to || to > points {
                        rings.push(Vec::new());
                        continue;
                    }
                    let mut ring = Vec::with_capacity((to - from).min(MAX_RING));
                    for _ in from..to {
                        let (Some(x), Some(y)) = (p.f64_le(), p.f64_le()) else { break };
                        if budget >= MAX_POINTS {
                            break;
                        }
                        budget += 1;
                        if !x.is_finite() || !y.is_finite() {
                            continue;
                        }
                        if !(-180.5..=180.5).contains(&x) || !(-90.5..=90.5).contains(&y) {
                            non_geo += 1;
                        }
                        let (px, py) = lonlat_to_doc(x, y);
                        ring.push(Point::new(px, py));
                    }
                    rings.push(ring);
                }
                let polygon = matches!(ty, 5 | 15 | 25);
                for ring in rings {
                    if ring.len() >= 2 {
                        made.push((SubPath::polyline(&ring, polygon), polygon));
                    }
                }
            }
            other => {
                if !SUPPORTED.iter().any(|(t, _)| *t == other) {
                    *skipped.entry(format!("shape type {other}")).or_insert(0usize) += 1;
                }
                break;
            }
        };
        if made.is_empty() {
            continue;
        }
        let polygon = made.iter().all(|(_, c)| *c);
        let id = doc.alloc_id();
        let mut n = Node::new(
            id,
            NodeKind::Path {
                path: PathData::new(made.into_iter().map(|(sp, _)| sp).collect()),
                rule: if polygon { FillRule::EvenOdd } else { FillRule::NonZero },
                live: None,
                clipping: false,
                guide: false,
            },
        );
        if polygon {
            n.appearance = Appearance::basic(Paint::solid(Color::rgb8(232, 168, 124)), Paint::solid(Color::rgb8(146, 74, 32)), 1.0);
        } else {
            n.appearance = Appearance::basic(Paint::None, Paint::solid(Color::rgb8(36, 99, 82)), 1.5);
        }
        n.name = Some(label);
        nodes.push(n);
    }
    if nodes.is_empty() {
        return Err(err("no shapes this reads in this Shapefile (it needs points, polylines or polygons; 2D or Z/M)"));
    }
    // Markers scale to the data's spread, as GeoJSON's do.
    let r = marker_radius(&nodes);
    for n in nodes.iter_mut() {
        scale_markers(n, r);
    }
    if non_geo > 0 {
        warnings.push(format!(
            "{non_geo} coordinates were outside longitude/latitude (|lon| > 180 or |lat| > 90): this Shapefile may be in a projected CRS; it still placed as if longitude/latitude"
        ));
    }
    for (ty, count) in skipped {
        warnings.push(format!("{count} unsupported shapes ({ty}) were skipped"));
    }
    let id = doc.alloc_id();
    let mut group = Node::group(id, nodes.drain(..).map(std::sync::Arc::new).collect());
    group.name = Some("Shapefile".into());
    doc.insert(Some(layer), usize::MAX, group).map_err(err)?;
    Ok((doc, warnings))
}

const MAX_RING: usize = 100_000;

/// Bounds-checked little/big-endian reads over the file's bytes.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }

    /// Take `n` bytes (None at the end of the file).
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }

    fn f64_le(&mut self) -> Option<f64> {
        self.take(8).and_then(|s| s.try_into().ok()).map(f64::from_le_bytes)
    }

    fn i32_le(&mut self) -> Option<i32> {
        self.take(4).and_then(|s| s.try_into().ok()).map(i32::from_le_bytes)
    }

    fn u32_be(&mut self) -> Option<u32> {
        self.take(4).and_then(|s| s.try_into().ok()).map(u32::from_be_bytes)
    }

    fn f64_n(&mut self, n: usize) -> Option<Vec<f64>> {
        (0..n).map(|_| self.f64_le()).collect()
    }

    fn u32_n(&mut self, n: usize) -> Option<Vec<u32>> {
        (0..n).map(|_| self.take(4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes)).collect()
    }

    /// One record header: skip to byte 100, then (content bytes) from the record number and
    /// content length (both big-endian, the length in 16-bit words).
    fn record(&mut self) -> Option<usize> {
        if self.at < 100 {
            self.at = 100;
        }
        let _number = self.u32_be()?;
        let words = self.u32_be()?;
        let bytes = words.checked_mul(2)?;
        if !(4..=(1 << 28)).contains(&bytes) {
            return None;
        }
        let content = bytes - 4;
        let end = self.at.checked_add(content as usize)?;
        if end > self.b.len() {
            return None;
        }
        Some(content as usize)
    }
}

/// One point shape/multipoint member: a marker circle (scaled once the import is in).
fn place_point(made: &mut Vec<(SubPath, bool)>, markers: &mut Vec<Point>, x: f64, y: f64, non_geo: &mut usize) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    if !(-180.5..=180.5).contains(&x) || !(-90.5..=90.5).contains(&y) {
        *non_geo += 1;
    }
    let (px, py) = lonlat_to_doc(x, y);
    let c = Point::new(px, py);
    markers.push(c);
    let mut pts = Vec::with_capacity(12);
    for i in 0..12 {
        let a = i as f64 * std::f64::consts::TAU / 12.0;
        pts.push(Point::new(c.x + a.cos(), c.y + a.sin()));
    }
    made.push((SubPath::polyline(&pts, true), false));
}

/// A marker size that reads for this import: 2% of the imported art's spread, at least ½ pt.
fn marker_radius(nodes: &[Node]) -> f64 {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for n in nodes {
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

/// Scale every round 12-corner ring (a marker) about its centre to `r`.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A minimal .shp: header + one polygon record (a triangle round London) + one point record.
    fn fixture() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&FILE_CODE.to_be_bytes()); // file code
        b.extend_from_slice(&[0u8; 20]); // unused
        let file_words = (100u32 + (8 + 4 + 32 + 8 + 8 + 4 + 4 + 8 * 3)) / 2; // header + polygon record
        b.extend_from_slice(&file_words.to_be_bytes()); // file length in words
        b.extend_from_slice(&1000i32.to_le_bytes()); // version
        b.extend_from_slice(&5i32.to_le_bytes()); // shape type: polygon
        for v in [-0.2f64, 51.4, 0.1, 51.6] {
            b.extend_from_slice(&v.to_le_bytes()); // bbox
        }
        for v in [0.0f64, 0.0, 0.0, 0.0] {
            b.extend_from_slice(&v.to_le_bytes()); // bbox Z and M: the header's last 32 bytes
        }
        // Record 1: polygon, one part, three points.
        let content_words = ((4 + 32 + 4 + 4 + 4 + 8 * 6) / 2) as u32;
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&content_words.to_be_bytes());
        b.extend_from_slice(&5i32.to_le_bytes());
        for v in [-0.2f64, 51.4, 0.1, 51.6] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&1i32.to_le_bytes()); // num parts
        b.extend_from_slice(&3i32.to_le_bytes()); // num points
        b.extend_from_slice(&0u32.to_le_bytes()); // part 0 at 0
        for (x, y) in [(-0.2f64, 51.4f64), (0.1, 51.4), (0.1, 51.6)] {
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&y.to_le_bytes());
        }
        // Record 2: point at Paris.
        b.extend_from_slice(&2u32.to_be_bytes());
        b.extend_from_slice(&10u32.to_be_bytes()); // (4 + 16) / 2 words
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&2.35f64.to_le_bytes());
        b.extend_from_slice(&48.85f64.to_le_bytes());
        b
    }

    #[test]
    fn import_shapes() {
        let (doc, warnings) = import(&fixture()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let group = doc.layers[0].children().unwrap();
        assert_eq!(group.len(), 1);
        let zones = group[0].children().unwrap();
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0].name.as_deref(), Some("Zone 1"));
        assert!(matches!(&zones[0].kind, NodeKind::Path { rule: FillRule::EvenOdd, .. }));
        // The polygon ring round London: its document points are on the world square, near
        // London's tile position (zoom 4: x ≈ 2045.8, y ≈ 1364.3).
        let NodeKind::Path { path, .. } = &zones[0].kind else { panic!("not a path") };
        let first = &path.subpaths[0].anchors[0].p;
        assert!((first.x - 2045.8).abs() < 8.0 && (first.y - 1364.3).abs() < 16.0, "{first:?}");
        // The point took a marker sized to the data.
        assert_eq!(zones[1].name.as_deref(), Some("Zone 2"));
    }

    #[test]
    fn rejects_junk() {
        assert!(import(b"short").is_err());
        let mut not_shp = fixture();
        not_shp[0..4].copy_from_slice(&1234u32.to_be_bytes());
        assert!(import(&not_shp).is_err());
    }
}
