//! Google Map Tiles API (BYOK) and adsbdb: URL builders and response parsers, no HTTP — the host
//! performs the requests. The 2D path is the session handshake plus per-tile URLs; the 3D path
//! parses a Photorealistic 3D Tiles TileSet.json into a light tree (mesh rendering needs the
//! future 3D viewport; the 2D canvas uses the session's 2D tiles as the imagery fallback).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The Google Map Tiles API host (2D tiles, sessions and 3D TileSets all live under it).
pub const TILE_HOST: &str = "https://tile.googleapis.com";

/// Why a Google/adsbdb response didn't parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleError {
    /// The bytes aren't valid JSON (serde_json's message).
    NotJson(String),
    /// Valid JSON, but a field the format requires is missing (which one).
    MissingField(&'static str),
    /// Valid JSON, but a field has the wrong shape (serde_json's message).
    BadField(String),
}

impl std::fmt::Display for GoogleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson(e) => write!(f, "not valid JSON: {e}"),
            Self::MissingField(field) => write!(f, "missing field {field}"),
            Self::BadField(e) => write!(f, "unexpected field shape: {e}"),
        }
    }
}

impl std::error::Error for GoogleError {}

/// The 2D session handshake request: `(url, json_body)` — the host POSTs the body to the URL and
/// feeds the response to [`parse_session_response`]. The satellite map type is what TerraZone's
/// basemaps want.
pub fn session_request(key: &str) -> (String, String) {
    (format!("{TILE_HOST}/v1/createSession?key={key}"), r#"{"mapType":"satellite","language":"en-US","region":"US"}"#.to_string())
}

/// The session token from a createSession response: the `"session"` string field.
pub fn parse_session_response(bytes: &[u8]) -> Result<String, GoogleError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| GoogleError::NotJson(e.to_string()))?;
    v.get("session").and_then(Value::as_str).map(str::to_string).ok_or(GoogleError::MissingField("session"))
}

/// A 2D tile URL: the session token and the key travel in the query (the host appends nothing).
pub fn tile_url(session: &str, key: &str, z: u32, x: u32, y: u32) -> String {
    format!("{TILE_HOST}/v1/2dtiles/{z}/{x}/{y}?session={session}&key={key}")
}

// -- 3D Tiles --------------------------------------------------------------------------------

/// A Photorealistic 3D Tiles TileSet.json, parsed to the fields TerraZone uses.
///
/// The 2D canvas can't render meshes — that needs the future 3D viewport; until then a 3D tileset
/// source's imagery fallback is the session's 2D tiles.
#[derive(Clone, Debug, PartialEq)]
pub struct Tileset {
    /// The `asset` (or `assetVersion`) version string, "1.0"/"1.1" style.
    pub asset_version: String,
    pub root: TilesetNode,
}

/// One node of the tileset tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TilesetNode {
    /// The error (roughly, metres) this node's content resolves at; children refine it.
    pub geometric_error: f64,
    /// The content URI (a glTF/glb or another tileset), when the node has content.
    pub content: Option<String>,
    /// "ADD" or "REPLACE" — how children combine with this node.
    pub refine: Option<String>,
    /// The node's `boundingVolume.sphere`, `[center_x, center_y, center_z, radius]` metres.
    pub sphere: Option<[f64; 4]>,
    pub children: Vec<TilesetNode>,
}

// The raw serde shapes: camelCase wire names, 3D Tiles 1.0's `asset.version` and 1.1's flat
// `assetVersion` both accepted, everything unknown ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTileset {
    #[serde(default)]
    asset_version: String,
    asset: Option<RawAsset>,
    root: RawNode,
}

#[derive(Deserialize)]
struct RawAsset {
    #[serde(default)]
    version: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawNode {
    geometric_error: f64,
    content: Option<RawContent>,
    refine: Option<String>,
    bounding_volume: Option<RawBoundingVolume>,
    children: Vec<RawNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawContent {
    #[serde(alias = "url", default)]
    uri: Option<String>,
    bounding_volume: Option<RawBoundingVolume>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBoundingVolume {
    sphere: Option<Vec<f64>>,
}

/// Parse a TileSet.json (`root.json` and its `content` tilesets alike).
pub fn load_tileset(bytes: &[u8]) -> Result<Tileset, GoogleError> {
    let raw: RawTileset = serde_json::from_slice(bytes).map_err(|e| match e.classify() {
        serde_json::error::Category::Data => GoogleError::BadField(e.to_string()),
        _ => GoogleError::NotJson(e.to_string()),
    })?;
    let asset_version = if raw.asset_version.is_empty() { raw.asset.map(|a| a.version).unwrap_or_default() } else { raw.asset_version };
    Ok(Tileset { asset_version, root: node_from(raw.root) })
}

fn node_from(raw: RawNode) -> TilesetNode {
    let RawNode { geometric_error, content, refine, bounding_volume, children } = raw;
    let sphere = bounding_volume
        .and_then(|b| b.sphere)
        .or_else(|| content.as_ref().and_then(|c| c.bounding_volume.as_ref()).and_then(|b| b.sphere.clone()))
        .and_then(sphere4);
    TilesetNode {
        geometric_error,
        content: content.as_ref().and_then(|c| c.uri.clone()),
        refine,
        sphere,
        children: children.into_iter().map(node_from).collect(),
    }
}

/// A sphere is exactly four numbers; anything else isn't one.
fn sphere4(v: Vec<f64>) -> Option<[f64; 4]> {
    <[f64; 4]>::try_from(v).ok()
}

/// How many content URIs one tileset may yield (a runaway tree stops here).
pub const MAX_CONTENT_URIS: usize = 1024;

/// Every content URI in the tileset, depth-first, capped at [`MAX_CONTENT_URIS`].
pub fn collect_content_uris(tileset: &Tileset) -> Vec<String> {
    fn walk(node: &TilesetNode, out: &mut Vec<String>) {
        if out.len() >= MAX_CONTENT_URIS {
            return;
        }
        if let Some(uri) = &node.content {
            out.push(uri.clone());
        }
        for child in &node.children {
            walk(child, out);
            if out.len() >= MAX_CONTENT_URIS {
                return;
            }
        }
    }
    let mut out = Vec::new();
    walk(&tileset.root, &mut out);
    out
}

// -- adsbdb -----------------------------------------------------------------------------------

/// The aircraft lookup URL for an ICAO 24-bit hex address (no "0x", case-insensitive).
pub fn adsbdb_url(icao_hex: &str) -> String {
    format!("https://api.adsbdb.com/v0/aircraft/{icao_hex}")
}

/// The callsign (flightroute) lookup URL.
pub fn adsbdb_flights_url(callsign: &str) -> String {
    format!("https://api.adsbdb.com/v0/callsign/{callsign}")
}

/// One aircraft from adsbdb, modelled loosely: whatever fields the response carries land here,
/// aliases cover the flat and `airframe`-nested shapes the API has used.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Aircraft {
    /// adsbdb names it `n_number`; `registration` and camelCase arrive from other shapers.
    #[serde(rename = "n_number", alias = "registration", alias = "nNumber")]
    pub registration: Option<String>,
    #[serde(alias = "manufacturer", alias = "mfr", alias = "mfrModel", alias = "mfr_model")]
    pub manufacturer: Option<String>,
    #[serde(alias = "type", alias = "typeName", alias = "faaType", alias = "typeAirCraft")]
    pub type_name: Option<String>,
    #[serde(alias = "icaoType", alias = "icao_type")]
    pub icao_type: Option<String>,
    #[serde(alias = "description", alias = "desc")]
    pub description: Option<String>,
}

/// One flightroute from adsbdb: the callsign with its origin and destination airports.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FlightRoute {
    #[serde(rename = "callsign", alias = "callSign")]
    pub callsign: String,
    pub origin: Option<Airport>,
    pub destination: Option<Airport>,
}

/// One airport: the fields TerraZone labels with; everything else ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Airport {
    #[serde(rename = "iata_code", alias = "iata", alias = "iataCode")]
    pub iata: Option<String>,
    #[serde(rename = "icao_code", alias = "icao", alias = "icaoCode")]
    pub icao: Option<String>,
    pub name: Option<String>,
    #[serde(rename = "country_iso_name", alias = "countryIsoName", alias = "countryIso")]
    pub country_iso_name: Option<String>,
    pub municipality: Option<String>,
}

/// Parse an aircraft response (`response.aircraft`); a nested `airframe` object fills fields the
/// flat shape left out.
pub fn parse_aircraft(bytes: &[u8]) -> Result<Aircraft, GoogleError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| GoogleError::NotJson(e.to_string()))?;
    let aircraft =
        v.get("response").and_then(|r| r.get("aircraft")).filter(|a| !a.is_null()).ok_or(GoogleError::MissingField("response.aircraft"))?;
    let mut a: Aircraft = serde_json::from_value(aircraft.clone()).map_err(|e| GoogleError::BadField(e.to_string()))?;
    if let Some(sub) = aircraft.get("airframe").and_then(Value::as_object)
        && let Ok(sub) = serde_json::from_value::<Aircraft>(Value::Object(sub.clone()))
    {
        a.manufacturer = a.manufacturer.take().or(sub.manufacturer);
        a.type_name = a.type_name.take().or(sub.type_name);
        a.icao_type = a.icao_type.take().or(sub.icao_type);
        a.description = a.description.take().or(sub.description);
    }
    Ok(a)
}

/// Parse a callsign response (`response.flightroute`, falling back to the bare `response.callsign`
/// when the route has no airports to speak of).
pub fn parse_flightroute(bytes: &[u8]) -> Result<FlightRoute, GoogleError> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| GoogleError::NotJson(e.to_string()))?;
    let resp = v.get("response").ok_or(GoogleError::MissingField("response"))?;
    let mut route: FlightRoute = resp
        .get("flightroute")
        .cloned()
        .map(|r| serde_json::from_value::<FlightRoute>(r).map_err(|e| GoogleError::BadField(e.to_string())))
        .transpose()?
        .unwrap_or_default();
    if route.callsign.is_empty() {
        route.callsign = resp.get("callsign").and_then(Value::as_str).unwrap_or_default().to_string();
    }
    Ok(route)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn session_request_shape() {
        let (url, body) = session_request("KEY-1");
        assert_eq!(url, "https://tile.googleapis.com/v1/createSession?key=KEY-1");
        assert_eq!(body, r#"{"mapType":"satellite","language":"en-US","region":"US"}"#);
    }

    #[test]
    fn session_response_parses() {
        assert_eq!(parse_session_response(br#"{"session":"ChAIEhaA","expiry":"2024-01-01T00:00:00Z"}"#).unwrap(), "ChAIEhaA");
        assert_eq!(parse_session_response(br#"{"nope":1}"#), Err(GoogleError::MissingField("session")));
        assert_eq!(parse_session_response(br#"{"session":42}"#), Err(GoogleError::MissingField("session")));
        assert!(matches!(parse_session_response(b"not json"), Err(GoogleError::NotJson(_))));
    }

    #[test]
    fn two_d_tile_urls() {
        assert_eq!(tile_url("S", "K", 4, 2, 3), "https://tile.googleapis.com/v1/2dtiles/4/2/3?session=S&key=K");
        assert_eq!(tile_url("S", "K", 0, 0, 0), "https://tile.googleapis.com/v1/2dtiles/0/0/0?session=S&key=K");
    }

    #[test]
    fn tileset_parses_google_shape() {
        // Google's Photorealistic 3D Tiles root.json shape: asset.version, a bounding sphere on
        // the root, content URIs and refining children.
        let bytes = br#"{"asset":{"version":"1.1"},"geometricError":9999,
            "root":{"geometricError":500,"refine":"ADD","boundingVolume":{"sphere":[1,2,3,4]},
                "content":{"uri":"mesh/0.glb"},
                "children":[{"geometricError":10,"content":{"uri":"mesh/1.glb"}},
                            {"geometricError":5,"boundingVolume":{"sphere":[5,6,7,8]},"content":{"url":"mesh/2.glb"}}]}}"#;
        let ts = load_tileset(bytes).unwrap();
        assert_eq!(ts.asset_version, "1.1");
        assert_eq!(ts.root.geometric_error, 500.0);
        assert_eq!(ts.root.refine.as_deref(), Some("ADD"));
        assert_eq!(ts.root.sphere, Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(ts.root.content.as_deref(), Some("mesh/0.glb"));
        assert_eq!(ts.root.children.len(), 2);
        assert_eq!(ts.root.children[1].sphere, Some([5.0, 6.0, 7.0, 8.0]));
        assert_eq!(ts.root.children[1].content.as_deref(), Some("mesh/2.glb"), "the 1.0 'url' alias reads too");
        // The flat 1.1-style assetVersion works as well.
        let flat = load_tileset(br#"{"assetVersion":"1.0.0","root":{"geometricError":1}}"#).unwrap();
        assert_eq!(flat.asset_version, "1.0.0");
        assert!(flat.root.children.is_empty() && flat.root.content.is_none());
        // Content URIs come out depth-first.
        assert_eq!(collect_content_uris(&ts), vec!["mesh/0.glb", "mesh/1.glb", "mesh/2.glb"]);
    }

    #[test]
    fn content_uris_cap_at_1024() {
        let root = TilesetNode {
            children: (0..2000).map(|i| TilesetNode { content: Some(format!("m/{i}")), ..TilesetNode::default() }).collect(),
            ..TilesetNode::default()
        };
        let uris = collect_content_uris(&Tileset { asset_version: String::new(), root });
        assert_eq!(uris.len(), MAX_CONTENT_URIS);
        assert_eq!(uris[0], "m/0");
    }

    #[test]
    fn tileset_errors() {
        assert!(matches!(load_tileset(b"nope"), Err(GoogleError::NotJson(_))));
        assert!(matches!(load_tileset(br#"{"asset":{"version":"1.0"}}"#), Err(GoogleError::BadField(_))), "no root");
    }

    #[test]
    fn adsbdb_aircraft_parses_flat_and_nested() {
        assert_eq!(adsbdb_url("ae1727"), "https://api.adsbdb.com/v0/aircraft/ae1727");
        // The flat shape.
        let flat = br#"{"response":{"aircraft":{"n_number":"N940QW","manufacturer":"Piper","type":"PA-28-181","icao_type":"P28A","description":"Fixed wing single engine"}}}"#;
        let a = parse_aircraft(flat).unwrap();
        assert_eq!(
            a,
            Aircraft {
                registration: Some("N940QW".into()),
                manufacturer: Some("Piper".into()),
                type_name: Some("PA-28-181".into()),
                icao_type: Some("P28A".into()),
                description: Some("Fixed wing single engine".into()),
            }
        );
        // The airframe-nested shape: the nested object fills what the flat shape lacks.
        let nested = br#"{"response":{"aircraft":{"n_number":"N11696","airframe":{"mfr":"Cessna","icao_type":"C172","name":"Skyhawk","description":"Landplane, single engine, piston"}}}}"#;
        let a = parse_aircraft(nested).unwrap();
        assert_eq!(a.registration.as_deref(), Some("N11696"));
        assert_eq!(a.manufacturer.as_deref(), Some("Cessna"), "the airframe's mfr fills manufacturer");
        assert_eq!(a.icao_type.as_deref(), Some("C172"));
        assert_eq!(a.description.as_deref(), Some("Landplane, single engine, piston"));
        // A not-found response errors, not panics.
        assert_eq!(parse_aircraft(br#"{"response":{"not_found":true}}"#), Err(GoogleError::MissingField("response.aircraft")));
        assert_eq!(parse_aircraft(br#"{"response":{"aircraft":null}}"#), Err(GoogleError::MissingField("response.aircraft")));
        assert!(matches!(parse_aircraft(b""), Err(GoogleError::NotJson(_))));
    }

    #[test]
    fn adsbdb_flightroute_parses() {
        assert_eq!(adsbdb_flights_url("BAW123"), "https://api.adsbdb.com/v0/callsign/BAW123");
        let bytes = br#"{"response":{"callsign":"BAW123","flightroute":{"callsign":"BAW123",
            "origin":{"iata_code":"LHR","icao_code":"EGLL","name":"London Heathrow Airport","country_iso_name":"GB","municipality":"London"},
            "destination":{"iata_code":"JFK","icao_code":"KJFK","name":"John F Kennedy International Airport","country_iso_name":"US","municipality":"New York"}}}}"#;
        let fr = parse_flightroute(bytes).unwrap();
        assert_eq!(fr.callsign, "BAW123");
        assert_eq!(
            fr.origin,
            Some(Airport {
                iata: Some("LHR".into()),
                icao: Some("EGLL".into()),
                name: Some("London Heathrow Airport".into()),
                country_iso_name: Some("GB".into()),
                municipality: Some("London".into()),
            })
        );
        assert_eq!(fr.destination.and_then(|a| a.icao), Some("KJFK".into()));
        // No flightroute: the bare callsign survives, airports stay empty.
        let bare = parse_flightroute(br#"{"response":{"callsign":"ZZZ00"}}"#).unwrap();
        assert_eq!(bare, FlightRoute { callsign: "ZZZ00".into(), origin: None, destination: None });
        assert_eq!(parse_flightroute(br#"{"nope":1}"#), Err(GoogleError::MissingField("response")));
    }
}
