//! A native ESRI Shapefile reader, pure binary parsing with no dependencies: the `.shp` main file
//! (points, polylines, polygons, multi-points; the Z and M twins' extra dimensions are read and
//! dropped so geometry matches the 2D types) plus the `.dbf` dBASE III sidecar for attributes,
//! mapped onto the same [`GeoGeometry`]/[`GeoValue`] model the GeoJSON reader produces. Spec:
//! https://www.esri.com/content/dam/esrisites/site-assets/pdf/whitepaper-pdfs/shapefile.pdf —
//! coordinates are longitude/latitude (a projected CRS still reads, the caller warns).

use crate::geojson::{GeoGeometry, GeoValue};

/// The `.shp` file code (big-endian in the header's first word).
const FILE_CODE: u32 = 9994;

/// One shapefile record: its geometry and its `.dbf` attributes (empty without a sidecar).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeRecord {
    pub shape: GeoGeometry,
    pub properties: Vec<(String, GeoValue)>,
}

/// Why a shapefile didn't parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShapeError {
    /// Fewer than the 100 main-file header bytes.
    TooShort,
    /// The file code isn't 9994 (what was read).
    BadMagic(u32),
    /// The version field isn't 1000 (what was read).
    BadVersion(i32),
    /// A record or one of its declared arrays runs past the end of the buffer.
    Truncated,
    /// A record's shape type isn't one this reader knows (what was read).
    UnknownShapeType(i32),
    /// A record declares a negative part or point count.
    BadCounts,
    /// The `.dbf` sidecar is malformed (what's wrong with it).
    Dbf(&'static str),
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "not a Shapefile: shorter than the 100-byte main-file header"),
            Self::BadMagic(code) => write!(f, "not a Shapefile: file code {code}, not {FILE_CODE}"),
            Self::BadVersion(v) => write!(f, "unsupported Shapefile version {v} (want 1000)"),
            Self::Truncated => write!(f, "a Shapefile record runs past the end of the file"),
            Self::UnknownShapeType(t) => write!(f, "unknown Shapefile shape type {t}"),
            Self::BadCounts => write!(f, "a Shapefile record declares a negative part or point count"),
            Self::Dbf(why) => write!(f, "bad .dbf sidecar: {why}"),
        }
    }
}

impl std::error::Error for ShapeError {}

/// Read a `.shp` main file (and, optionally, its `.dbf` sidecar) into records. Each shape's
/// attributes come from the .dbf row at the same index: rows align by position, so a deleted
/// (.dbf tombstone) row leaves its shape with no properties rather than shifting the rest.
///
/// Geometry: points → [`GeoGeometry::Point`], polylines → `LineString` (one part) or
/// `MultiLineString` (several), polygons → `Polygon` with every ring kept — rings are closed and
/// oriented per the spec (exterior clockwise, holes counterclockwise), Z/M dropped.
pub fn read_shapefile(shp: &[u8], dbf: Option<&[u8]>) -> Result<Vec<ShapeRecord>, ShapeError> {
    if shp.len() < 100 {
        return Err(ShapeError::TooShort);
    }
    let code = u32_be(shp, 0).ok_or(ShapeError::TooShort)?;
    if code != FILE_CODE {
        return Err(ShapeError::BadMagic(code));
    }
    let version = i32_le(shp, 28).ok_or(ShapeError::TooShort)?;
    if version != 1000 {
        return Err(ShapeError::BadVersion(version));
    }
    // The declared file length (words 24..28) isn't enforced: parse records to the end of the
    // buffer, so a lying header costs nothing.
    let mut shapes = Vec::new();
    let mut at = 100usize;
    while at < shp.len() {
        let words = i32_be(shp, at + 4).ok_or(ShapeError::Truncated)?;
        if words < 0 {
            return Err(ShapeError::Truncated);
        }
        let content_len = words as usize * 2;
        let end = (at + 8).checked_add(content_len).ok_or(ShapeError::Truncated)?;
        let content = shp.get(at + 8..end).ok_or(ShapeError::Truncated)?;
        shapes.push(parse_shape(content)?);
        at = end;
    }
    // Attributes: .dbf row i pairs with shape i; a deleted row keeps its slot, empty.
    let attrs = dbf.map(read_dbf).transpose()?;
    Ok(shapes
        .into_iter()
        .enumerate()
        .map(|(i, shape)| ShapeRecord { shape, properties: attrs.as_ref().and_then(|a| a.get(i)).cloned().flatten().unwrap_or_default() })
        .collect())
}

/// Parse one record's content: the little-endian shape type, then the shape.
fn parse_shape(content: &[u8]) -> Result<GeoGeometry, ShapeError> {
    let ty = i32_le(content, 0).ok_or(ShapeError::Truncated)?;
    match ty {
        0 => Ok(GeoGeometry::Null),
        // Point (1) and its Z (11) / M (21) twins: x, y — the z/m measures are dropped.
        1 | 11 | 21 => {
            let x = f64_le(content, 4).ok_or(ShapeError::Truncated)?;
            let y = f64_le(content, 12).ok_or(ShapeError::Truncated)?;
            Ok(GeoGeometry::Point(x, y))
        }
        // MultiPoint (8) and its Z (18) / M (28) twins.
        8 | 18 | 28 => multi_point(content),
        // PolyLine (3) / Polygon (5) and their Z (13/15) and M (23/25) twins — the parts/points
        // arrays read the same; Z/M arrays trail and are dropped with the rest of the content.
        3 | 13 | 23 => parts_shape(content, false),
        5 | 15 | 25 => parts_shape(content, true),
        other => Err(ShapeError::UnknownShapeType(other)),
    }
}

fn multi_point(content: &[u8]) -> Result<GeoGeometry, ShapeError> {
    let n = i32_le(content, 36).ok_or(ShapeError::Truncated)?;
    if n < 0 {
        return Err(ShapeError::BadCounts);
    }
    let at = 40usize;
    if content.len() < at + n as usize * 16 {
        return Err(ShapeError::Truncated);
    }
    let mut pts = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        pts.push((f64_le(content, at + i * 16).ok_or(ShapeError::Truncated)?, f64_le(content, at + i * 16 + 8).ok_or(ShapeError::Truncated)?));
    }
    Ok(GeoGeometry::MultiPoint(pts))
}

/// The bbox + parts + points layout a PolyLine (3), Polygon (5) and their Z/M twins share. For
/// polygons every part is a ring: closed and orientation-normalized (exterior clockwise, holes
/// counterclockwise), interior rings kept.
fn parts_shape(content: &[u8], polygon: bool) -> Result<GeoGeometry, ShapeError> {
    let nparts = i32_le(content, 36).ok_or(ShapeError::Truncated)?;
    let npts = i32_le(content, 40).ok_or(ShapeError::Truncated)?;
    if nparts < 0 || npts < 0 {
        return Err(ShapeError::BadCounts);
    }
    let parts_at = 44usize;
    let points_at = parts_at + nparts as usize * 4;
    if content.len() < points_at + npts as usize * 16 {
        return Err(ShapeError::Truncated);
    }
    let mut starts = Vec::with_capacity(nparts as usize);
    for i in 0..nparts as usize {
        starts.push(i32_le(content, parts_at + i * 4).ok_or(ShapeError::Truncated)?);
    }
    let mut pts = Vec::with_capacity(npts as usize);
    for i in 0..npts as usize {
        pts.push((
            f64_le(content, points_at + i * 16).ok_or(ShapeError::Truncated)?,
            f64_le(content, points_at + i * 16 + 8).ok_or(ShapeError::Truncated)?,
        ));
    }
    let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        // A corrupt part index (negative or past the points) drops just that part.
        let (Some(s), e) = (usize::try_from(start).ok(), starts.get(i + 1).copied().and_then(|v| usize::try_from(v).ok()).unwrap_or(npts as usize))
        else {
            continue;
        };
        if s > npts as usize || e > npts as usize || s >= e {
            continue;
        }
        let mut ring = pts[s..e].to_vec();
        if polygon {
            close_ring(&mut ring);
        }
        rings.push(ring);
    }
    if polygon {
        normalize_polygon(&mut rings);
    }
    Ok(match (polygon, rings.as_slice()) {
        (false, [ring]) => GeoGeometry::LineString(ring.clone()),
        (false, [_, _, ..]) => GeoGeometry::MultiLineString(rings.clone()),
        (true, [_, _, ..]) => GeoGeometry::Polygon(rings),
        _ => GeoGeometry::Null, // no (usable) parts at all
    })
}

/// A polygon ring is closed: the first point repeats at the end.
fn close_ring(ring: &mut Vec<(f64, f64)>) {
    if ring.len() > 2
        && let (Some(&first), Some(&last)) = (ring.first(), ring.last())
        && first != last
    {
        ring.push(first);
    }
}

/// The signed shoelace area: positive winds counterclockwise (y grows north here).
fn signed_area(ring: &[(f64, f64)]) -> f64 {
    let mut a = 0.0;
    for i in 0..ring.len() {
        let j = (i + 1) % ring.len();
        a += ring[i].0 * ring[j].1 - ring[j].0 * ring[i].1;
    }
    a / 2.0
}

/// The spec's winding: the first (exterior) ring clockwise, holes counterclockwise.
fn normalize_polygon(rings: &mut [Vec<(f64, f64)>]) {
    for (i, ring) in rings.iter_mut().enumerate() {
        let area = signed_area(ring);
        let exterior = i == 0;
        if (exterior && area > 0.0) || (!exterior && area < 0.0) {
            ring.reverse();
        }
    }
}

/// One .dbf row's decoded fields (a deleted row reads as `None`).
type DbfRow = Vec<(String, GeoValue)>;

/// Read a dBASE III `.dbf`: field descriptors from the header, then fixed-width records behind a
/// one-byte deletion flag. Rows keep their index (a deleted row is `None`), so shapes pair by
/// position.
fn read_dbf(dbf: &[u8]) -> Result<Vec<Option<DbfRow>>, ShapeError> {
    if dbf.len() < 33 {
        return Err(ShapeError::Dbf("shorter than a dBASE III header"));
    }
    let n_records = u32_le(dbf, 4).ok_or(ShapeError::Dbf("no record count"))? as usize;
    let header_len = u16_le(dbf, 8).ok_or(ShapeError::Dbf("no header length"))? as usize;
    let record_len = u16_le(dbf, 10).ok_or(ShapeError::Dbf("no record length"))? as usize;
    if header_len < 33 || header_len > dbf.len() || record_len == 0 {
        return Err(ShapeError::Dbf("nonsensical header lengths"));
    }
    // 32-byte field descriptors until the 0x0D terminator.
    let mut fields: Vec<(String, u8, usize)> = Vec::new();
    let mut at = 32usize;
    while at + 32 <= header_len && dbf[at] != 0x0d {
        let name_bytes = dbf.get(at..at + 11).ok_or(ShapeError::Dbf("short field descriptor"))?;
        let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
        let name = String::from_utf8_lossy(&name_bytes[..end]).trim().to_string();
        if !name.is_empty() {
            fields.push((name, dbf[at + 11], dbf[at + 16] as usize));
        }
        at += 32;
    }
    let mut rows = Vec::with_capacity(n_records);
    let mut at = header_len;
    for _ in 0..n_records {
        let Some(row) = dbf.get(at..at + record_len) else { break }; // a short tail ends the file
        at += record_len;
        let deleted = row[0] == b'*';
        let mut props = Vec::with_capacity(fields.len());
        let mut fat = 1usize;
        for (name, ftype, flen) in &fields {
            let value = row.get(fat..fat + *flen).map_or(GeoValue::Null, |cell| decode_field(*ftype, cell));
            props.push((name.clone(), value));
            fat += flen;
        }
        rows.push(if deleted { None } else { Some(props) });
    }
    Ok(rows)
}

/// One dBASE field's bytes → a [`GeoValue`], by the field's type letter.
fn decode_field(ftype: u8, cell: &[u8]) -> GeoValue {
    let s = String::from_utf8_lossy(cell).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
    match ftype {
        b'C' | b'D' if s.is_empty() => GeoValue::Null, // character / date (yyyymmdd)
        b'C' | b'D' => GeoValue::String(s),
        b'N' | b'F' => {
            if s.is_empty() {
                return GeoValue::Null;
            }
            s.parse::<f64>().map(GeoValue::Number).unwrap_or_else(|_| GeoValue::String(s))
        }
        b'L' => match s.bytes().next() {
            Some(b'T' | b't' | b'Y' | b'y') => GeoValue::Bool(true),
            Some(b'F' | b'f' | b'N' | b'n') => GeoValue::Bool(false),
            _ => GeoValue::Null,
        },
        // Memo pointers (M) point into the .dbt this reader doesn't see; anything else is unknown.
        _ => GeoValue::Null,
    }
}

fn u32_be(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes(s.try_into().ok()?))
}

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(s.try_into().ok()?))
}

fn u16_le(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn i32_le(b: &[u8], at: usize) -> Option<i32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(i32::from_le_bytes(s.try_into().ok()?))
}

fn i32_be(b: &[u8], at: usize) -> Option<i32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(i32::from_be_bytes(s.try_into().ok()?))
}

fn f64_le(b: &[u8], at: usize) -> Option<f64> {
    let s = b.get(at..at.checked_add(8)?)?;
    Some(f64::from_bits(u64::from_le_bytes(s.try_into().ok()?)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    // -- Fixture builders: the exact Shapefile/dBASE byte layouts, hand-crafted. --------------

    fn le32(v: i32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn be32(v: i32) -> [u8; 4] {
        v.to_be_bytes()
    }

    fn le64(v: f64) -> [u8; 8] {
        v.to_le_bytes()
    }

    /// The 100-byte main-file header: code 9994, five unused words, the file length in 16-bit
    /// words (big-endian), version 1000, the shape type, then bbox + Z/M extents (f64 ×8, zeros).
    fn header(shape_type: i32, words: i32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend(be32(9994));
        h.extend_from_slice(&[0; 20]);
        h.extend(be32(words));
        h.extend(le32(1000));
        h.extend(le32(shape_type));
        h.extend_from_slice(&[0; 64]);
        h
    }

    /// Write the file length (bytes / 2, in words) into the header's big-endian slot.
    fn seal(shp: &mut [u8]) {
        let words = shp.len() as i32 / 2;
        shp[24..28].copy_from_slice(&be32(words));
    }

    /// One record: number (big-endian), content length in 16-bit words (big-endian), content.
    fn record(num: i32, content: &[u8]) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend(be32(num));
        r.extend(be32(content.len() as i32 / 2));
        r.extend_from_slice(content);
        r
    }

    /// A Point record's content: type, x, y (20 bytes).
    fn point(x: f64, y: f64) -> Vec<u8> {
        let mut c = le32(1).to_vec();
        c.extend(le64(x));
        c.extend(le64(y));
        c
    }

    /// The parts/points content a PolyLine (3) / Polygon (5) carries: type, bbox, part count,
    /// point count, part start indexes, points.
    fn parts(ty: i32, rings: &[&[(f64, f64)]]) -> Vec<u8> {
        let npts: usize = rings.iter().map(|r| r.len()).sum();
        let mut c = Vec::new();
        c.extend(le32(ty));
        for v in [0.0, 0.0, 10.0, 10.0] {
            c.extend(le64(v));
        }
        c.extend(le32(rings.len() as i32));
        c.extend(le32(npts as i32));
        let mut start = 0i32;
        for r in rings {
            c.extend(le32(start));
            start += r.len() as i32;
        }
        for r in rings {
            for &(x, y) in *r {
                c.extend(le64(x));
                c.extend(le64(y));
            }
        }
        c
    }

    /// A dBASE III sidecar: fields NAME C10, POP N8.0, SET D8, OK L1; three records, the second a
    /// deleted tombstone, the third carrying empties and an unknown logical.
    fn dbf() -> Vec<u8> {
        let fields: [(&[u8; 11], u8, u8, u8); 4] = [
            (b"NAME\0\0\0\0\0\0\0", b'C', 10, 0),
            (b"POP\0\0\0\0\0\0\0\0", b'N', 8, 0),
            (b"SET\0\0\0\0\0\0\0\0", b'D', 8, 0),
            (b"OK\0\0\0\0\0\0\0\0\0", b'L', 1, 0),
        ];
        let header_len = 32 + fields.len() * 32 + 1;
        let record_len = 1 + 10 + 8 + 8 + 1;
        let mut d = Vec::new();
        d.push(0x03); // dBASE III, no memo file
        d.extend_from_slice(&[0x64, 0x01, 0x01]); // last edit yymmdd
        d.extend(3u32.to_le_bytes());
        d.extend((header_len as u16).to_le_bytes());
        d.extend((record_len as u16).to_le_bytes());
        d.extend_from_slice(&[0; 20]); // reserved
        for (name, ty, len, dec) in fields {
            d.extend_from_slice(name);
            d.push(ty);
            d.extend_from_slice(&[0; 4]); // field data address, unused
            d.push(len);
            d.push(dec);
            d.extend_from_slice(&[0; 14]); // reserved
        }
        d.push(0x0d); // header terminator
        for (flag, cells) in [
            (0x20u8, ["Alpha     ", "     123", "20240101", "T"]),
            (b'*', ["Beta      ", "      99", "20240102", "F"]),
            (0x20, ["Gamma C-3 ", "        ", "        ", "?"]),
        ] {
            d.push(flag);
            for cell in cells {
                d.extend_from_slice(cell.as_bytes());
            }
        }
        d
    }

    fn polygon_shp() -> Vec<u8> {
        let mut shp = header(5, 0);
        // An exterior ring wound counterclockwise (against the spec) and a hole wound clockwise
        // (as the spec has it) — the reader normalizes both.
        let content = parts(5, &[&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)], &[(0.2, 0.2), (0.2, 0.3), (0.3, 0.2)]]);
        shp.extend(record(1, &content));
        seal(&mut shp);
        shp
    }

    // -- Tests --------------------------------------------------------------------------------

    #[test]
    fn point_and_z_m_variants() {
        let mut shp = header(1, 0);
        shp.extend(record(1, &point(8.5, -3.25)));
        // PointZ (11): x, y, z, m — the Z/M read and dropped, geometry is the plain 2D point.
        let mut z = le32(11).to_vec();
        z.extend(le64(4.5));
        z.extend(le64(6.75));
        z.extend(le64(100.0));
        z.extend(le64(7.0));
        shp.extend(record(2, &z));
        // PointM (21): x, y, m.
        let mut m = le32(21).to_vec();
        m.extend(le64(-1.0));
        m.extend(le64(2.0));
        m.extend(le64(9.0));
        shp.extend(record(3, &m));
        seal(&mut shp);
        let records = read_shapefile(&shp, None).unwrap();
        assert_eq!(
            records.iter().map(|r| r.shape.clone()).collect::<Vec<_>>(),
            vec![GeoGeometry::Point(8.5, -3.25), GeoGeometry::Point(4.5, 6.75), GeoGeometry::Point(-1.0, 2.0)]
        );
        assert!(records.iter().all(|r| r.properties.is_empty()), "no sidecar, no properties");
    }

    #[test]
    fn multipoint() {
        let mut shp = header(8, 0);
        let mut c = le32(8).to_vec();
        for v in [0.0, 0.0, 10.0, 10.0] {
            c.extend(le64(v));
        }
        c.extend(le32(2));
        c.extend(le64(1.0));
        c.extend(le64(2.0));
        c.extend(le64(3.0));
        c.extend(le64(4.0));
        shp.extend(record(1, &c));
        seal(&mut shp);
        let records = read_shapefile(&shp, None).unwrap();
        assert_eq!(records[0].shape, GeoGeometry::MultiPoint(vec![(1.0, 2.0), (3.0, 4.0)]));
    }

    #[test]
    fn polyline_one_part_is_a_linestring_many_are_multi() {
        let mut shp = header(3, 0);
        shp.extend(record(1, &parts(3, &[&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]])));
        shp.extend(record(2, &parts(3, &[&[(0.0, 0.0), (1.0, 1.0)], &[(5.0, 5.0), (6.0, 6.0)]])));
        seal(&mut shp);
        let records = read_shapefile(&shp, None).unwrap();
        assert_eq!(records[0].shape, GeoGeometry::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]));
        assert_eq!(records[1].shape, GeoGeometry::MultiLineString(vec![vec![(0.0, 0.0), (1.0, 1.0)], vec![(5.0, 5.0), (6.0, 6.0)]]));
    }

    #[test]
    fn polygon_rings_close_normalize_and_keep_holes() {
        let records = read_shapefile(&polygon_shp(), None).unwrap();
        let GeoGeometry::Polygon(rings) = &records[0].shape else { panic!("not a polygon") };
        assert_eq!(rings.len(), 2, "the interior ring is kept");
        for ring in rings {
            assert_eq!(ring.first(), ring.last(), "{ring:?} must be closed");
        }
        assert!(signed_area(&rings[0]) <= 0.0, "the exterior ring winds clockwise: {:?}", rings[0]);
        assert!(signed_area(&rings[1]) >= 0.0, "holes wind counterclockwise: {:?}", rings[1]);
        // The flipped exterior is the same triangle, backwards: (0,0),(0,1),(1,0),(0,0).
        assert_eq!(rings[0], vec![(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (0.0, 0.0)]);
    }

    #[test]
    fn null_shape_keeps_the_record() {
        let mut shp = header(0, 0);
        shp.extend(record(1, &le32(0)));
        shp.extend(record(2, &point(1.0, 1.0)));
        seal(&mut shp);
        let records = read_shapefile(&shp, None).unwrap();
        assert_eq!(records[0].shape, GeoGeometry::Null);
        assert_eq!(records[1].shape, GeoGeometry::Point(1.0, 1.0));
    }

    #[test]
    fn dbf_properties_pair_by_index_and_deleted_rows_skip() {
        let records = read_shapefile(&polygon_shp(), Some(&dbf())).unwrap();
        assert_eq!(records.len(), 1, "the .shp drives the record count");
        // One shape, three dbf rows: the first row's properties land on it.
        assert_eq!(
            records[0].properties,
            vec![
                ("NAME".into(), GeoValue::String("Alpha".into())),
                ("POP".into(), GeoValue::Number(123.0)),
                ("SET".into(), GeoValue::String("20240101".into())),
                ("OK".into(), GeoValue::Bool(true)),
            ]
        );
    }

    #[test]
    fn dbf_rows_align_through_tombstones() {
        // Three point records against the three-row dbf (middle row deleted): deleted rows keep
        // their slot so shapes pair by position, and their values leak nowhere.
        let mut shp = header(1, 0);
        shp.extend(record(1, &point(1.0, 1.0)));
        shp.extend(record(2, &point(2.0, 2.0)));
        shp.extend(record(3, &point(3.0, 3.0)));
        seal(&mut shp);
        let records = read_shapefile(&shp, Some(&dbf())).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].properties[0].1, GeoValue::String("Alpha".into()));
        assert!(records[1].properties.is_empty(), "the deleted row's shape carries no properties");
        assert!(records.iter().all(|r| !r.properties.iter().any(|(k, v)| k == "NAME" && v == &GeoValue::String("Beta".into()))));
        let third = &records[2].properties;
        assert_eq!(third[0].1, GeoValue::String("Gamma C-3".into()), "character fields trim their padding");
        assert_eq!(third[1].1, GeoValue::Null, "an empty number is no value");
        assert_eq!(third[2].1, GeoValue::Null, "an empty date is no value");
        assert_eq!(third[3].1, GeoValue::Null, "an unknown logical is no value");
    }

    #[test]
    fn a_short_dbf_leaves_the_rest_without_properties() {
        // Exactly the .dbf header, no records: shapes read, attributes don't.
        let records = read_shapefile(&polygon_shp(), Some(&dbf()[..161])).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].properties.is_empty(), "no complete row before the buffer ends");
    }

    #[test]
    fn bad_files_error() {
        assert_eq!(read_shapefile(&[], None), Err(ShapeError::TooShort));
        assert_eq!(read_shapefile(&[0; 99], None), Err(ShapeError::TooShort));
        let mut bad_magic = header(1, 50);
        bad_magic[0..4].copy_from_slice(&be32(1234));
        assert_eq!(read_shapefile(&bad_magic, None), Err(ShapeError::BadMagic(1234)));
        let mut bad_version = header(1, 50);
        bad_version[28..32].copy_from_slice(&le32(999));
        assert_eq!(read_shapefile(&bad_version, None), Err(ShapeError::BadVersion(999)));
        // A record header promising more content than the buffer holds.
        let mut truncated = header(1, 50);
        truncated.extend(be32(1));
        truncated.extend(be32(10));
        truncated.extend(le32(1));
        assert_eq!(read_shapefile(&truncated, None), Err(ShapeError::Truncated));
        // An unknown shape type (31, multi-patch).
        let mut unknown = header(0, 0);
        unknown.extend(record(1, &le32(31)));
        assert_eq!(read_shapefile(&unknown, None), Err(ShapeError::UnknownShapeType(31)));
        // A negative point count.
        let mut counts = header(3, 0);
        let mut c = le32(3).to_vec();
        c.extend_from_slice(&[0; 32]);
        c.extend(le32(1));
        c.extend(le32(-1));
        counts.extend(record(1, &c));
        assert_eq!(read_shapefile(&counts, None), Err(ShapeError::BadCounts));
        // A .dbf that isn't one.
        assert_eq!(read_shapefile(&polygon_shp(), Some(&[0; 5])), Err(ShapeError::Dbf("shorter than a dBASE III header")));
    }
}
