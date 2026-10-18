//! A minimal Mapbox Vector Tile (MVT) decoder, no protobuf dependency: the tiles' geometry only
//! (names and attributes pass through where cheap), turned into Web Mercator document contours
//! ready to draw. Spec: https://protobuf.dev/programming-guides/encoding/ and
//! https://github.com/mapbox/vector-tile-spec.

use crate::{TileId, WORLD_PT};

/// One decoded geometry: its layer name, geometry kind and contours in document points.
#[derive(Clone, Debug, PartialEq)]
pub struct MvtGeom {
    /// The MVT layer's name ("road", "water", "building"…).
    pub layer: String,
    /// 1 point, 2 linestring, 3 polygon (from the spec's GeomType).
    pub kind: u32,
    /// Rings: open for lines, closed for polygons, in document points (y grows south).
    pub rings: Vec<Vec<(f64, f64)>>,
}

/// A varint reader over the tile bytes.
struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    fn done(&self) -> bool {
        self.i >= self.b.len()
    }

    /// The next varint, or `None` past the end (a truncated tile decodes to what it has).
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.b.get(self.i)?;
            self.i += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
    }

    fn skip(&mut self, wire: u64) -> Option<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => {
                self.i = self.i.checked_add(8)?;
                Some(())
            }
            2 => {
                let n = self.varint()? as usize;
                self.i = self.i.checked_add(n)?;
                Some(())
            }
            5 => {
                self.i = self.i.checked_add(4)?;
                Some(())
            }
            _ => None,
        }
    }

    /// A length-delimited field's bytes.
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.varint()? as usize;
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
}

/// Zigzag decode (MVT geometry parameters are zigzag-varint signed deltas).
fn zag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Decode an .mvt tile → its geometries in document points. A tile covers `tile`'s document rect;
/// its internal extent (usually 4096) scales from the tile's own field.
pub fn decode_mvt(bytes: &[u8], tile: TileId) -> Vec<MvtGeom> {
    let mut out = Vec::new();
    // Vector tiles come gzip-compressed (Mapbox serves .mvt gzipped); sniff the magic and unzip.
    let unzipped;
    let bytes: &[u8] = if bytes.len() > 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut v = Vec::new();
        if std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(bytes), &mut v).is_err() {
            return out;
        }
        unzipped = v;
        &unzipped
    } else {
        bytes
    };
    // A tile at the world's edge rounds its rect a hair wide: clamp the mapping into the world.
    let (rx0, ry0, rx1, ry1) = tile.rect_pt();
    let (tx0, ty0) = (rx0.max(0.0), ry0.max(0.0));
    let (tx1, ty1) = (rx1.min(WORLD_PT), ry1.min(WORLD_PT));
    let edge = (tx1 - tx0).max(1e-9);
    let _ = ty1;

    let mut top = Reader::new(bytes);
    while !top.done() {
        let (field, wire) = match top.varint() {
            Some(t) => (t >> 3, t & 7),
            None => break,
        };
        if field != 3 || wire != 2 {
            top.skip(wire);
            continue;
        }
        let Some(layer_bytes) = top.bytes() else { break };
        decode_layer(layer_bytes, tx0, ty0, edge, &mut out);
    }
    out
}

fn decode_layer(b: &[u8], tx0: f64, ty0: f64, edge: f64, out: &mut Vec<MvtGeom>) {
    let mut name = String::new();
    let mut extent = 4096.0f64;
    // Features and their tags pass through on the first walk; keys/values are interleaved with
    // features in the wire order, so collect the whole layer's fields first.
    let mut fields: Vec<(u64, u64, &[u8])> = Vec::new();
    let mut r = Reader::new(b);
    while !r.done() {
        let tag = match r.varint() {
            Some(t) => t,
            None => break,
        };
        let (field, wire) = (tag >> 3, tag & 7);
        match (field, wire) {
            (1, 2) => {
                if let Some(s) = r.bytes()
                    && let Ok(s) = std::str::from_utf8(s)
                {
                    name = s.to_string();
                }
            }
            (5, 0) => {
                if let Some(v) = r.varint()
                    && (1..=65536).contains(&v)
                {
                    extent = v as f64;
                }
            }
            (2, 2) | (3, 2) | (4, 2) => {
                if let Some(s) = r.bytes() {
                    fields.push((field, wire, s));
                }
            }
            _ => {
                r.skip(wire);
            }
        }
    }
    // The k/th key and v/th value of a feature's tags: keys are strings, values any wire type.
    // Values we keep as short strings (the layer name plus "road, water…" is all the drawing
    // needs; attributes are the document's business later).
    if name.is_empty() {
        name = "vector".into();
    }
    let scale = edge / extent;
    for (field, _, payload) in fields {
        if field != 2 {
            continue;
        }
        if let Some(g) = decode_feature(payload, &name, tx0, ty0, scale) {
            out.push(g);
        }
    }
}

fn decode_feature(b: &[u8], layer: &str, tx0: f64, ty0: f64, scale: f64) -> Option<MvtGeom> {
    let mut kind = 0u32;
    let mut geom: Option<&[u8]> = None;
    let mut r = Reader::new(b);
    while !r.done() {
        let tag = r.varint()?;
        let (field, wire) = (tag >> 3, tag & 7);
        match (field, wire) {
            (3, 0) => kind = r.varint()? as u32,
            (4, 2) => geom = Some(r.bytes()?),
            _ => r.skip(wire)?,
        }
    }
    let cmds = geom?;
    if !(1..=3).contains(&kind) {
        return None; // points and collections don't draw as contours
    }
    let mut g = Reader::new(cmds);
    let mut x = 0i64;
    let mut y = 0i64;
    let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut cur: Vec<(f64, f64)> = Vec::new();
    while !g.done() {
        let header = g.varint()?;
        let (id, count) = (header & 7, (header >> 3) as usize);
        match id {
            1 | 2 => {
                for _ in 0..count {
                    let dx = zag(g.varint()?);
                    let dy = zag(g.varint()?);
                    x += dx;
                    y += dy;
                    if id == 1 && !cur.is_empty() {
                        rings.push(std::mem::take(&mut cur));
                    }
                    cur.push((tx0 + x as f64 * scale, ty0 + y as f64 * scale));
                }
            }
            7 => {
                if cur.len() > 2 {
                    // Close the ring (the first and last point share coordinates; keep one).
                    if let Some(first) = cur.first().copied() {
                        cur.push(first);
                    }
                    rings.push(std::mem::take(&mut cur));
                }
            }
            _ => break, // unknown command: the rest can't be trusted
        }
    }
    if !cur.is_empty() {
        rings.push(cur);
    }
    let rings: Vec<Vec<(f64, f64)>> = rings.into_iter().filter(|r| r.len() > 1).collect();
    if rings.is_empty() {
        return None;
    }
    Some(MvtGeom { layer: layer.to_string(), kind, rings })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A hand-encoded MVT: one layer "road" (extent 4096) with one linestring from (0,0) to
    /// (4096,4096) — tile-local, i.e. the tile's diagonal.
    fn sample_linestring_mvt() -> Vec<u8> {
        let mut f = Vec::new();
        // Feature: field 3 (type) varint 2; field 4 (geometry) bytes.
        let mut cmds = Vec::new();
        // MoveTo(1): header (1 & 7) | 1 << 3 = 9; params zigzag 0,0.
        cmds.extend([9u8, 0, 0]);
        // LineTo(1): header 2 | 1 << 3 = 10; params zigzag(+4096)=8192, zigzag(+4096)=8192 → two varints.
        fn varint(v: u64, out: &mut Vec<u8>) {
            let mut v = v;
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(b);
                    break;
                }
                out.push(b | 0x80);
            }
        }
        varint(10, &mut cmds);
        varint(8192, &mut cmds);
        varint(8192, &mut cmds);
        f.push(3 << 3);
        f.push(2);
        f.push((4 << 3) | 2);
        varint(cmds.len() as u64, &mut f);
        f.extend(&cmds);
        // Layer: field 1 name "road"; field 2 feature; field 5 extent 4096.
        let mut layer = Vec::new();
        layer.push((1 << 3) | 2);
        varint(4, &mut layer);
        layer.extend(b"road");
        layer.push((2 << 3) | 2);
        varint(f.len() as u64, &mut layer);
        layer.extend(&f);
        layer.push(5 << 3);
        layer.push(0x80 | 0x20); // 4096 = 0x1000 → varint 0x80 0x20
        // Tile: field 3 layer.
        let mut tile = Vec::new();
        tile.push((3 << 3) | 2);
        varint(layer.len() as u64, &mut tile);
        tile.extend(&layer);
        tile
    }

    #[test]
    fn decodes_a_linestring() {
        let tile = TileId::new(4, 8, 8);
        let geoms = decode_mvt(&sample_linestring_mvt(), tile);
        assert_eq!(geoms.len(), 1);
        assert_eq!(geoms[0].layer, "road");
        assert_eq!(geoms[0].kind, 2);
        let (tx0, ty0, tx1, _) = tile.rect_pt();
        let ring = &geoms[0].rings[0];
        assert_eq!(ring.len(), 2);
        assert!((ring[0].0 - tx0).abs() < 1e-6 && (ring[0].1 - ty0).abs() < 1e-6);
        assert!((ring[1].0 - tx1).abs() < 1e-6);
    }

    #[test]
    fn junk_decodes_empty() {
        assert!(decode_mvt(b"", TileId::new(0, 0, 0)).is_empty());
        assert!(decode_mvt(&[0xff, 0xff, 0xff], TileId::new(0, 0, 0)).is_empty());
    }

    #[test]
    fn decodes_gzipped_tiles() {
        // Mapbox serves .mvt gzipped: the magic (1f 8b) sniffs and unzips first.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &sample_linestring_mvt()).unwrap();
        let gzipped = gz.finish().unwrap();
        let tile = TileId::new(4, 8, 8);
        let geoms = decode_mvt(&gzipped, tile);
        assert_eq!(geoms.len(), 1);
        assert_eq!(geoms[0].layer, "road");
    }
}
