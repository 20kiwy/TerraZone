//! A native GeoJSON reader/writer (RFC 7946): features in and out of this crate's own data model,
//! with just `serde_json` underneath. Coordinates stay longitude/latitude (WGS84, degrees);
//! [`GeoFeature::to_mercator`] projects into Web Mercator metres through the crate's document
//! mapping, so imported geometry lands exactly where the tiles do.

use serde_json::{Value, json};

use crate::{WORLD_PT, doc_to_lonlat, lonlat_to_doc};

/// One position: longitude, latitude — degrees here, metres in the projected mirror types.
pub type Pos = (f64, f64);

/// A line or ring: positions in order.
pub type Ring = Vec<Pos>;

/// The WGS84 equatorial radius the Web Mercator projection's sphere uses, in metres.
pub const MERCATOR_RADIUS: f64 = 6_378_137.0;
/// The projected world's edge, 2π × [`MERCATOR_RADIUS`] metres: x spans −half..+half (east), y
/// +half..−half (north).
pub const WORLD_METERS: f64 = std::f64::consts::TAU * MERCATOR_RADIUS;

/// A GeoJSON property value (RFC 7946 leaves member values unconstrained): the JSON types, with
/// objects kept as ordered pairs so round-trips are stable.
#[derive(Clone, Debug, PartialEq)]
pub enum GeoValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<GeoValue>),
    /// An object as ordered key/value pairs (serde_json's map sorts keys; the order is stable).
    Object(Vec<(String, GeoValue)>),
}

impl GeoValue {
    /// The string, when this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The number, when this is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The bool, when this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// GeoJSON geometry, carrying longitude/latitude degrees. Positions keep their first two
/// dimensions (a third elevation is read and dropped); ring closure and orientation are the
/// file's own — the Shapefile reader normalizes its side.
#[derive(Clone, Debug, PartialEq)]
pub enum GeoGeometry {
    /// A `null` geometry (features may carry one).
    Null,
    Point(f64, f64),
    MultiPoint(Vec<Pos>),
    LineString(Ring),
    MultiLineString(Vec<Ring>),
    /// Rings in file order: the first is the exterior, the rest are holes.
    Polygon(Vec<Ring>),
    MultiPolygon(Vec<Vec<Ring>>),
    GeometryCollection(Vec<GeoGeometry>),
}

/// One GeoJSON feature: its properties (in file order) and its geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct GeoFeature {
    pub properties: Vec<(String, GeoValue)>,
    pub geometry: GeoGeometry,
}

impl GeoFeature {
    /// The first name-like property's value (`name`, then `NAME`, then `name_en`), trimmed and
    /// non-empty — a label for the Layers panel.
    pub fn label(&self) -> Option<&str> {
        ["name", "NAME", "name_en"]
            .iter()
            .find_map(|key| self.properties.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_str()).map(str::trim).filter(|s| !s.is_empty()))
    }

    /// The feature with its geometry projected to Web Mercator metres (properties pass through).
    pub fn to_mercator(&self) -> MercatorFeature {
        MercatorFeature { properties: self.properties.clone(), geometry: merc_geometry(&self.geometry) }
    }
}

/// A [`GeoFeature`] whose geometry is projected to Web Mercator metres (x east, y north) — for
/// anything that wants real-world lengths rather than document points.
#[derive(Clone, Debug, PartialEq)]
pub struct MercatorFeature {
    pub properties: Vec<(String, GeoValue)>,
    pub geometry: MercatorGeometry,
}

/// The same shapes as [`GeoGeometry`], coordinates in metres.
#[derive(Clone, Debug, PartialEq)]
pub enum MercatorGeometry {
    Null,
    Point(f64, f64),
    MultiPoint(Vec<Pos>),
    LineString(Ring),
    MultiLineString(Vec<Ring>),
    Polygon(Vec<Ring>),
    MultiPolygon(Vec<Vec<Ring>>),
    GeometryCollection(Vec<MercatorGeometry>),
}

/// Document points → Web Mercator metres. The document's y grows south, the projection's north,
/// so the flip happens here.
pub fn doc_pt_to_mercator(x: f64, y: f64) -> (f64, f64) {
    ((x / WORLD_PT - 0.5) * WORLD_METERS, (0.5 - y / WORLD_PT) * WORLD_METERS)
}

/// Web Mercator metres → document points.
pub fn mercator_to_doc_pt(x: f64, y: f64) -> (f64, f64) {
    (x / WORLD_METERS * WORLD_PT + WORLD_PT / 2.0, (0.5 - y / WORLD_METERS) * WORLD_PT)
}

/// Longitude/latitude (WGS84, degrees) → Web Mercator metres, through the crate's document
/// mapping (out-of-range latitudes clamp, exactly as [`crate::lonlat_to_doc`] clamps them).
pub fn lonlat_to_mercator(lon: f64, lat: f64) -> (f64, f64) {
    let (x, y) = lonlat_to_doc(lon, lat);
    doc_pt_to_mercator(x, y)
}

/// Web Mercator metres → longitude/latitude (WGS84, degrees).
pub fn mercator_to_lonlat(x: f64, y: f64) -> (f64, f64) {
    let (x, y) = mercator_to_doc_pt(x, y);
    doc_to_lonlat(x, y)
}

/// Why a document didn't parse as GeoJSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoJsonError {
    /// The text isn't valid JSON (serde_json's message).
    NotJson(String),
    /// The top-level JSON value isn't an object.
    NotAnObject,
    /// The value's "type" isn't a GeoJSON type this reader knows (what was read).
    BadType(String),
    /// A geometry's coordinates are missing or not numbers (which piece).
    BadCoordinates(&'static str),
}

impl std::fmt::Display for GeoJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson(e) => write!(f, "not valid JSON: {e}"),
            Self::NotAnObject => write!(f, "a GeoJSON document is a JSON object"),
            Self::BadType(t) => write!(f, "\"{t}\" is not a GeoJSON type this reader knows"),
            Self::BadCoordinates(what) => write!(f, "bad {what} coordinates"),
        }
    }
}

impl std::error::Error for GeoJsonError {}

/// Parse a GeoJSON document (RFC 7946) into its features. A `FeatureCollection`, a bare `Feature`
/// and a bare geometry all parse; a feature's `null` geometry becomes [`GeoGeometry::Null`]; a
/// top-level `GeometryCollection` becomes one feature with the collection geometry. Ring closure
/// and orientation are the file's own — nothing is rearranged, so a write of the parse is a
/// faithful round-trip.
pub fn parse_geojson(text: &str) -> Result<Vec<GeoFeature>, GeoJsonError> {
    // A UTF-8 byte-order mark and trailing whitespace are both tolerated (Windows tools save the
    // BOM).
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| GeoJsonError::NotJson(e.to_string()))?;
    let obj = v.as_object().ok_or(GeoJsonError::NotAnObject)?;
    match obj.get("type").and_then(Value::as_str).unwrap_or_default() {
        "FeatureCollection" => {
            let mut out = Vec::new();
            for f in obj.get("features").and_then(Value::as_array).into_iter().flatten() {
                out.push(parse_feature(f)?);
            }
            Ok(out)
        }
        "Feature" => Ok(vec![parse_feature(&v)?]),
        "Point" | "MultiPoint" | "LineString" | "MultiLineString" | "Polygon" | "MultiPolygon" | "GeometryCollection" => {
            Ok(vec![GeoFeature { properties: Vec::new(), geometry: parse_geometry(&v)? }])
        }
        other => Err(GeoJsonError::BadType(other.to_string())),
    }
}

/// Serialize features as a GeoJSON `FeatureCollection` string (compact JSON, properties in their
/// stored order).
pub fn write_geojson(features: &[GeoFeature]) -> String {
    let features = features
        .iter()
        .map(|f| {
            json!({
                "type": "Feature",
                "properties": f.properties.iter().map(|(k, v)| (k.clone(), value_json(v))).collect::<serde_json::Map<String, Value>>(),
                "geometry": geometry_json(&f.geometry),
            })
        })
        .collect::<Vec<_>>();
    json!({ "type": "FeatureCollection", "features": features }).to_string()
}

fn parse_feature(f: &Value) -> Result<GeoFeature, GeoJsonError> {
    let obj = f.as_object().ok_or_else(|| GeoJsonError::BadType("feature".into()))?;
    let properties = match obj.get("properties") {
        Some(Value::Object(p)) => p.iter().map(|(k, v)| (k.clone(), value_from_json(v))).collect(),
        _ => Vec::new(),
    };
    let geometry = match obj.get("geometry") {
        None | Some(Value::Null) => GeoGeometry::Null,
        Some(g) => parse_geometry(g)?,
    };
    Ok(GeoFeature { properties, geometry })
}

fn parse_geometry(g: &Value) -> Result<GeoGeometry, GeoJsonError> {
    let obj = g.as_object().ok_or_else(|| GeoJsonError::BadType("geometry".into()))?;
    let ty = obj.get("type").and_then(Value::as_str).unwrap_or_default();
    let coords = || obj.get("coordinates").ok_or(GeoJsonError::BadCoordinates("geometry"));
    match ty {
        "Point" => {
            let (x, y) = position(coords()?)?;
            Ok(GeoGeometry::Point(x, y))
        }
        "MultiPoint" => Ok(GeoGeometry::MultiPoint(positions(coords()?)?)),
        "LineString" => Ok(GeoGeometry::LineString(positions(coords()?)?)),
        "MultiLineString" => Ok(GeoGeometry::MultiLineString(rings(coords()?)?)),
        "Polygon" => Ok(GeoGeometry::Polygon(rings(coords()?)?)),
        "MultiPolygon" => Ok(GeoGeometry::MultiPolygon(
            coords()?.as_array().ok_or(GeoJsonError::BadCoordinates("polygon array"))?.iter().map(rings).collect::<Result<Vec<_>, _>>()?,
        )),
        "GeometryCollection" => Ok(GeoGeometry::GeometryCollection(
            obj.get("geometries")
                .and_then(Value::as_array)
                .ok_or(GeoJsonError::BadCoordinates("geometries"))?
                .iter()
                .map(parse_geometry)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        other => Err(GeoJsonError::BadType(other.to_string())),
    }
}

/// One `[lon, lat, …]` position: the first two numbers, extra dimensions dropped.
fn position(v: &Value) -> Result<Pos, GeoJsonError> {
    let a = v.as_array().ok_or(GeoJsonError::BadCoordinates("position"))?;
    match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(GeoJsonError::BadCoordinates("position")),
    }
}

fn positions(v: &Value) -> Result<Ring, GeoJsonError> {
    v.as_array().ok_or(GeoJsonError::BadCoordinates("position array"))?.iter().map(position).collect()
}

fn rings(v: &Value) -> Result<Vec<Ring>, GeoJsonError> {
    v.as_array().ok_or(GeoJsonError::BadCoordinates("ring array"))?.iter().map(positions).collect()
}

fn value_from_json(v: &Value) -> GeoValue {
    match v {
        Value::Null => GeoValue::Null,
        Value::Bool(b) => GeoValue::Bool(*b),
        Value::Number(n) => n.as_f64().map(GeoValue::Number).unwrap_or(GeoValue::Null),
        Value::String(s) => GeoValue::String(s.clone()),
        Value::Array(a) => GeoValue::Array(a.iter().map(value_from_json).collect()),
        Value::Object(o) => GeoValue::Object(o.iter().map(|(k, v)| (k.clone(), value_from_json(v))).collect()),
    }
}

fn value_json(v: &GeoValue) -> Value {
    match v {
        GeoValue::Null => Value::Null,
        GeoValue::Bool(b) => Value::Bool(*b),
        GeoValue::Number(n) => serde_json::Number::from_f64(*n).map(Value::Number).unwrap_or(Value::Null),
        GeoValue::String(s) => Value::String(s.clone()),
        GeoValue::Array(a) => Value::Array(a.iter().map(value_json).collect()),
        GeoValue::Object(o) => o.iter().map(|(k, v)| (k.clone(), value_json(v))).collect::<serde_json::Map<String, Value>>().into(),
    }
}

fn ring_json(r: &Ring) -> Value {
    r.iter().map(|p| json!([p.0, p.1])).collect::<Vec<Value>>().into()
}

fn geometry_json(g: &GeoGeometry) -> Value {
    match g {
        GeoGeometry::Null => Value::Null,
        GeoGeometry::Point(x, y) => json!({ "type": "Point", "coordinates": [x, y] }),
        GeoGeometry::MultiPoint(ps) => json!({ "type": "MultiPoint", "coordinates": ring_json(ps) }),
        GeoGeometry::LineString(r) => json!({ "type": "LineString", "coordinates": ring_json(r) }),
        GeoGeometry::MultiLineString(rs) => json!({ "type": "MultiLineString", "coordinates": rs.iter().map(ring_json).collect::<Vec<_>>() }),
        GeoGeometry::Polygon(rs) => json!({ "type": "Polygon", "coordinates": rs.iter().map(ring_json).collect::<Vec<_>>() }),
        GeoGeometry::MultiPolygon(ps) => {
            json!({ "type": "MultiPolygon", "coordinates": ps.iter().map(|rs| rs.iter().map(ring_json).collect::<Vec<_>>()).collect::<Vec<_>>() })
        }
        GeoGeometry::GeometryCollection(gs) => {
            json!({ "type": "GeometryCollection", "geometries": gs.iter().map(geometry_json).collect::<Vec<_>>() })
        }
    }
}

fn merc_geometry(g: &GeoGeometry) -> MercatorGeometry {
    match g {
        GeoGeometry::Null => MercatorGeometry::Null,
        GeoGeometry::Point(x, y) => {
            let (mx, my) = lonlat_to_mercator(*x, *y);
            MercatorGeometry::Point(mx, my)
        }
        GeoGeometry::MultiPoint(ps) => MercatorGeometry::MultiPoint(ps.iter().map(|p| lonlat_to_mercator(p.0, p.1)).collect()),
        GeoGeometry::LineString(r) => MercatorGeometry::LineString(r.iter().map(|p| lonlat_to_mercator(p.0, p.1)).collect()),
        GeoGeometry::MultiLineString(rs) => {
            MercatorGeometry::MultiLineString(rs.iter().map(|r| r.iter().map(|p| lonlat_to_mercator(p.0, p.1)).collect()).collect())
        }
        GeoGeometry::Polygon(rs) => MercatorGeometry::Polygon(rs.iter().map(|r| r.iter().map(|p| lonlat_to_mercator(p.0, p.1)).collect()).collect()),
        GeoGeometry::MultiPolygon(ps) => MercatorGeometry::MultiPolygon(
            ps.iter().map(|rs| rs.iter().map(|r| r.iter().map(|p| lonlat_to_mercator(p.0, p.1)).collect()).collect()).collect(),
        ),
        GeoGeometry::GeometryCollection(gs) => MercatorGeometry::GeometryCollection(gs.iter().map(merc_geometry).collect()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Every geometry type, `null` geometry, `null` properties and every GeoValue kind at once.
    const SAMPLE: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"A","rank":2,"hot":true,"tags":["x","y"],"meta":{"k":null}},
         "geometry":{"type":"Point","coordinates":[1.5,2.5,7]}},
        {"type":"Feature","properties":null,"geometry":null},
        {"type":"Feature","properties":{"NAME":"B"},"geometry":{"type":"MultiPoint","coordinates":[[0,0],[10,20]]}},
        {"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[0,0],[1,1],[2,0]]}},
        {"type":"Feature","properties":{},"geometry":{"type":"MultiLineString","coordinates":[[[0,0],[1,1]],[[2,2],[3,3]]]}},
        {"type":"Feature","properties":{"name_en":"C"},
         "geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[1,1],[1,2],[2,2],[1,1]]]}},
        {"type":"Feature","properties":{},"geometry":{"type":"MultiPolygon","coordinates":[[[[0,0],[1,0],[1,1],[0,0]]]]}},
        {"type":"Feature","properties":{},
         "geometry":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[3,4]},{"type":"LineString","coordinates":[[5,5],[6,6]]}]}}
    ]}"#;

    #[test]
    fn parses_every_geometry_type() {
        let fs = parse_geojson(SAMPLE).unwrap();
        assert_eq!(fs.len(), 8);
        assert_eq!(fs[0].geometry, GeoGeometry::Point(1.5, 2.5), "the elevation third is dropped");
        assert_eq!(fs[1].geometry, GeoGeometry::Null, "null geometry tolerated");
        assert_eq!(fs[2].geometry, GeoGeometry::MultiPoint(vec![(0.0, 0.0), (10.0, 20.0)]));
        assert_eq!(fs[3].geometry, GeoGeometry::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)]));
        assert_eq!(fs[4].geometry, GeoGeometry::MultiLineString(vec![vec![(0.0, 0.0), (1.0, 1.0)], vec![(2.0, 2.0), (3.0, 3.0)]]));
        assert_eq!(
            fs[5].geometry,
            GeoGeometry::Polygon(vec![
                vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
                vec![(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (1.0, 1.0)]
            ]),
            "holes kept as-is"
        );
        assert_eq!(fs[6].geometry, GeoGeometry::MultiPolygon(vec![vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]]]));
        match &fs[7].geometry {
            GeoGeometry::GeometryCollection(gs) => {
                assert_eq!(gs.len(), 2);
                assert_eq!(gs[0], GeoGeometry::Point(3.0, 4.0));
                assert_eq!(gs[1], GeoGeometry::LineString(vec![(5.0, 5.0), (6.0, 6.0)]));
            }
            other => panic!("not a GeometryCollection: {other:?}"),
        }
    }

    #[test]
    fn parses_every_property_kind() {
        let fs = parse_geojson(SAMPLE).unwrap();
        assert_eq!(
            fs[0].properties,
            vec![
                // serde_json's map sorts keys; the pair order is stable (here alphabetical).
                ("hot".into(), GeoValue::Bool(true)),
                ("meta".into(), GeoValue::Object(vec![("k".into(), GeoValue::Null)])),
                ("name".into(), GeoValue::String("A".into())),
                ("rank".into(), GeoValue::Number(2.0)),
                ("tags".into(), GeoValue::Array(vec![GeoValue::String("x".into()), GeoValue::String("y".into())])),
            ]
        );
        assert!(fs[1].properties.is_empty(), "null properties become none");
    }

    #[test]
    fn labels_pick_the_first_name_like_property() {
        let fs = parse_geojson(SAMPLE).unwrap();
        assert_eq!(fs[0].label(), Some("A"));
        assert_eq!(fs[2].label(), Some("B"), "NAME counts");
        assert_eq!(fs[5].label(), Some("C"), "name_en counts");
        assert_eq!(fs[1].label(), None, "no properties, no label");
        // Empty and whitespace names don't count.
        let bare: GeoFeature = parse_geojson(r#"{"type":"Feature","properties":{"name":"  "},"geometry":null}"#).unwrap().remove(0);
        assert_eq!(bare.label(), None);
    }

    #[test]
    fn write_then_parse_is_the_same_features() {
        let fs = parse_geojson(SAMPLE).unwrap();
        let text = write_geojson(&fs);
        assert!(text.contains(r#""type":"FeatureCollection""#) && text.contains(r#""features":"#), "{text}");
        assert_eq!(parse_geojson(&text).unwrap(), fs, "the writer's output re-parses to the same features");
    }

    #[test]
    fn tolerates_bom_trailing_whitespace_and_bare_documents() {
        parse_geojson(&format!("\u{feff}{SAMPLE}  \n")).unwrap();
        // A bare Feature and a bare geometry parse as one feature each.
        let f = parse_geojson(r#" {"type":"Feature","properties":{"name":"F"},"geometry":{"type":"Point","coordinates":[1,2]}} "#).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].geometry, GeoGeometry::Point(1.0, 2.0));
        let g = parse_geojson(r#"{"type":"Point","coordinates":[9,8]}"#).unwrap();
        assert_eq!(g.len(), 1);
        assert!(g[0].properties.is_empty(), "a bare geometry has no properties");
        assert!(parse_geojson(r#"{"type":"FeatureCollection","features":[]}"#).unwrap().is_empty());
        // A missing "features" member tolerates as an empty collection.
        assert!(parse_geojson(r#"{"type":"FeatureCollection"}"#).unwrap().is_empty());
    }

    #[test]
    fn bad_documents_error() {
        assert_eq!(parse_geojson("[1,2]"), Err(GeoJsonError::NotAnObject));
        assert_eq!(parse_geojson("nope"), Err(GeoJsonError::NotJson("expected ident at line 1 column 2".into())));
        assert_eq!(parse_geojson(r#"{"type":"Nope"}"#), Err(GeoJsonError::BadType("Nope".into())));
        assert_eq!(parse_geojson(r#"{"type":"Point"}"#), Err(GeoJsonError::BadCoordinates("geometry")));
        assert_eq!(parse_geojson(r#"{"type":"Point","coordinates":[1]}"#), Err(GeoJsonError::BadCoordinates("position")));
        // A missing geometry member tolerates as null; a non-object geometry is the error.
        assert_eq!(parse_geojson(r#"{"type":"Feature"}"#).unwrap(), vec![GeoFeature { properties: Vec::new(), geometry: GeoGeometry::Null }]);
        assert_eq!(parse_geojson(r#"{"type":"Feature","geometry":5}"#), Err(GeoJsonError::BadType("geometry".into())));
    }

    #[test]
    fn mercator_matches_the_document_math() {
        // The projection's corners are the world's corners.
        let (x, y) = lonlat_to_mercator(0.0, 0.0);
        assert!((x.abs() + y.abs()) < 1e-6, "{x} {y}");
        let (x, y) = lonlat_to_mercator(-180.0, 0.0);
        assert!((x - -WORLD_METERS / 2.0).abs() < 1e-6 && y.abs() < 1e-6, "{x} {y}");
        let (_, y) = lonlat_to_mercator(0.0, crate::LAT_LIMIT);
        assert!((y - WORLD_METERS / 2.0).abs() < 1e-6, "{y}");
        // Round trips through the doc-point math.
        for (lon, lat) in [(-0.1276, 51.5072), (23.5, -45.9), (179.0, 80.0)] {
            let (mx, my) = lonlat_to_mercator(lon, lat);
            let (lon2, lat2) = mercator_to_lonlat(mx, my);
            assert!((lon - lon2).abs() < 1e-9 && (lat - lat2).abs() < 1e-9, "{lon} {lat} -> {mx} {my} -> {lon2} {lat2}");
        }
        // Doc points sit half a world from the projection's metres.
        let (dx, dy) = mercator_to_doc_pt(-WORLD_METERS / 2.0, WORLD_METERS / 2.0);
        assert!((dx - 0.0).abs() < 1e-9 && (dy - 0.0).abs() < 1e-9, "{dx} {dy}");
    }

    #[test]
    fn to_mercator_projects_every_shape() {
        let fs = parse_geojson(SAMPLE).unwrap();
        let m = fs[5].to_mercator();
        assert_eq!(m.properties, fs[5].properties, "properties pass through");
        match m.geometry {
            MercatorGeometry::Polygon(rs) => {
                assert_eq!(rs.len(), 2, "both rings project");
                assert!(rs.iter().all(|r| r.iter().all(|p| p.0.abs() <= WORLD_METERS / 2.0 && p.1.abs() <= WORLD_METERS / 2.0)));
            }
            other => panic!("not a projected polygon: {other:?}"),
        }
        let (mx, my) = lonlat_to_mercator(1.5, 2.5);
        assert_eq!(fs[0].to_mercator().geometry, MercatorGeometry::Point(mx, my));
        assert_eq!(fs[1].to_mercator().geometry, MercatorGeometry::Null);
    }
}
