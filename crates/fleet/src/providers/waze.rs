//! The Waze provider's own API module: the Waze road network as the Waze Map Editor serves it
//! to its own web client — the anonymous `Features` read (the same request the editor's page
//! makes for its view: a bbox, the road types, the venue level that keeps it in the public
//! sandbox mode) — separated from the shared fetch plumbing the way the other vendors are.
//!
//! The answer carries far more than roads (venues, users, junctions, areas); this module
//! reads the two collections the roads layer draws — `segments` (the line geometry and its
//! attributes) and `streets` (the id → name table the segments point into) — and turns them
//! into [`WazeSeg`]s with everything the map needs: the class (Waze's own road-type enum,
//! mirrored from the editor), the name, the direction bits, the speeds, the closure flag.
//!
//! The road-type table is Waze's own numbering (read off the editor's bundle: `STREET:1,
//! PRIMARY_STREET:2, FREEWAY:3, RAMP:4, …, PARKING_LOT_ROAD:20, ALLEY:22`) — not the old
//! community legend that floated around for years. The draw-side palette (colours, widths,
//! dashes) rides here too, next to the legend it colours, the way the vendor paints its own
//! editor: freeways brick red over the pack, collectors yellow and orange, rail dashed grey.

use serde::Deserialize;

use crate::feed::ViewBox;

/// The editor's anonymous Features read (`row` = the rest-of-world instance — the one that
/// serves Tanzania; the query keeps `sandbox=true` and the venue level, which is what keeps
/// the endpoint answering without a session).
const FEATURES_URL: &str = "https://www.waze.com/row-Descartes/app/Features";
/// The road types the ask covers: every driving type plus rail, trail, ferry and the rest —
/// the full list the editor itself requests (comma-encoded for the query string).
const ROAD_TYPES: &str = "1,2,3,4,5,6,7,8,9,10,15,16,17,18,19,20,22";
/// The headers the read rides: the web client's own (the endpoint is public but expects the
/// editor's shape of traffic, the way the flight feed does).
pub fn request_headers() -> Vec<(String, String)> {
    vec![
        (
            "User-Agent".into(),
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36".into(),
        ),
        ("Accept".into(), "*/*".into()),
        ("Accept-Language".into(), "en-US,en;q=0.8".into()),
        ("Referer".into(), "https://www.waze.com/en/editor?env=row".into()),
    ]
}

/// Below this map zoom the roads layer does not ask: a country-wide bbox would pull the
/// whole corridor's network for a screen where no road is wider than a hair. The layer's
/// honest state under it is "zoom in".
pub const MIN_FETCH_ZOOM: f64 = 11.0;

/// The Features URL for a view: the bbox quantized outward onto a 0.01° grid (a pan inside
/// the same grid cell re-uses the answer instead of re-asking every poll tick) and the
/// editor's own query shape — `v=2&apiV2=true`, the road types, the venue level and the
/// `sandbox` flag that keeps the read anonymous.
pub fn features_url(view: ViewBox, zoom: f64) -> String {
    let (w, s, e, n) = view;
    const Q: f64 = 100.0; // the 0.01° grid the bbox snaps to, outward so it always covers
    let (w, s) = ((w * Q).floor() / Q, (s * Q).floor() / Q);
    let (e, n) = ((e * Q).ceil() / Q, (n * Q).ceil() / Q);
    let z = zoom.round().clamp(6.0, 19.0) as i32;
    let road_types = ROAD_TYPES.replace(',', "%2C");
    format!(
        "{FEATURES_URL}?bbox={w:.2},{s:.2},{e:.2},{n:.2}&language=en&v=2&apiV2=true&mapComments=true&\
roadTypes={road_types}&\
venueLevel=4&venueFilter=1%2C1%2C1%2C0&sandbox=true&zoomLevel={z}"
    )
}

/// One road segment as the layer draws it: the geometry in lon/lat pairs plus the attributes
/// the properties panel shows — Waze's road type, the street name (from the answer's street
/// table), the direction bits, the speeds, the closure and toll flags and the elevation
/// level (0 ground, negative tunnels, positive bridges).
#[derive(Clone, Debug, PartialEq)]
pub struct WazeSeg {
    /// The vendor's own segment id (stable — the properties panel names it, the picked-road
    /// highlight matches by it).
    pub id: i64,
    /// Waze's road type (see [`road_type_name`]).
    pub road_type: u8,
    /// The street name (`""` when Waze has none — most residential segments and every
    /// parking aisle carry no name).
    pub name: String,
    /// Drivable toward the segment's `to` node.
    pub fwd: bool,
    /// Drivable toward its `from` node.
    pub rev: bool,
    /// The vendor's own length figure in metres (the panel shows it; the draw uses geometry).
    pub length_m: f64,
    /// Elevation level: 0 ground, < 0 tunnel, > 0 bridge/overpass.
    pub level: i32,
    /// A live closure rides the segment (the panel flags it; the draw dashes it red).
    pub closures: bool,
    /// A priced (toll) segment.
    pub toll: bool,
    /// Forward speed limit, km/h, when the editor keeps one.
    pub fwd_speed: Option<u32>,
    /// Reverse speed limit, km/h, when the editor keeps one.
    pub rev_speed: Option<u32>,
    /// The line, lon/lat pairs (a MultiLineString flattens into the one polyline the map
    /// draws — a segment's rings are one road for this layer's purposes).
    pub pts: Vec<[f64; 2]>,
    /// The segment's own bbox `(min_lon, min_lat, max_lon, max_lat)` — the draw's cull box.
    pub bbox: [f64; 4],
}

/// The raw answer, only the collections this module reads (serde ignores the rest).
#[derive(Deserialize, Default)]
struct FeaturesAnswer {
    #[serde(default)]
    segments: SegmentsBlock,
    #[serde(default)]
    streets: StreetsBlock,
}

#[derive(Deserialize, Default)]
struct SegmentsBlock {
    #[serde(default)]
    objects: Vec<RawSeg>,
}

#[derive(Deserialize, Default)]
struct StreetsBlock {
    #[serde(default)]
    objects: Vec<RawStreet>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStreet {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSeg {
    id: i64,
    #[serde(default)]
    road_type: u8,
    #[serde(default)]
    fwd_direction: bool,
    #[serde(default)]
    rev_direction: bool,
    #[serde(default, rename = "primaryStreetID")]
    primary_street_id: Option<i64>,
    #[serde(default)]
    fwd_max_speed: Option<u32>,
    #[serde(default)]
    rev_max_speed: Option<u32>,
    #[serde(default)]
    length: f64,
    #[serde(default)]
    level: i32,
    #[serde(default)]
    has_closures: bool,
    #[serde(default, rename = "isPricedSegment")]
    is_priced: bool,
    #[serde(default)]
    geometry: Option<RawGeometry>,
}

#[derive(Deserialize)]
struct RawGeometry {
    #[serde(default)]
    coordinates: RawCoords,
}

#[derive(Deserialize, Default)]
#[serde(untagged)]
enum RawCoords {
    Line(Vec<[f64; 2]>),
    Multi(Vec<Vec<[f64; 2]>>),
    #[default]
    None,
}

/// Parse a Features answer into the layer's segments. A quiet area (zero segments) reads as
/// an honest empty, not an error; a non-JSON answer (a gate page, a rate limit) is an error
/// the row shows.
pub fn parse_features(bytes: &[u8]) -> Result<Vec<WazeSeg>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the answer is not UTF-8 text".to_string())?;
    let answer: FeaturesAnswer = serde_json::from_str(text).map_err(|e| format!("the answer is not the Features JSON: {e}"))?;
    let streets: std::collections::HashMap<i64, &str> = answer.streets.objects.iter().map(|s| (s.id, s.name.as_str())).collect();
    let mut out = Vec::with_capacity(answer.segments.objects.len());
    for raw in answer.segments.objects {
        let Some(geo) = raw.geometry else { continue };
        let pts: Vec<[f64; 2]> = match geo.coordinates {
            RawCoords::Line(line) => line,
            RawCoords::Multi(rings) => rings.into_iter().flatten().collect(),
            RawCoords::None => continue,
        };
        if pts.len() < 2 {
            continue;
        }
        let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for [lon, lat] in &pts {
            bbox[0] = bbox[0].min(*lon);
            bbox[1] = bbox[1].min(*lat);
            bbox[2] = bbox[2].max(*lon);
            bbox[3] = bbox[3].max(*lat);
        }
        out.push(WazeSeg {
            id: raw.id,
            road_type: raw.road_type,
            name: raw.primary_street_id.and_then(|id| streets.get(&id).copied()).unwrap_or("").to_string(),
            fwd: raw.fwd_direction,
            rev: raw.rev_direction,
            length_m: raw.length,
            level: raw.level,
            closures: raw.has_closures,
            toll: raw.is_priced,
            fwd_speed: raw.fwd_max_speed,
            rev_speed: raw.rev_max_speed,
            pts,
            bbox,
        });
    }
    // The draw paints big roads over small ones: sort by class rank so a freeway's casing
    // lands over the street it crosses, the way the vendor's own map stacks its types.
    out.sort_by_key(|s| road_type_rank(s.road_type));
    Ok(out)
}

/// The road type's name — Waze's own editor vocabulary (the enum read off the editor's
/// bundle; the numbers are the wire format's, and the data proves them: E11 rides 3, the
/// Dubai Metro rides 18, the parking aisles ride 20).
pub fn road_type_name(rt: u8) -> &'static str {
    match rt {
        1 => "Street",
        2 => "Primary Street",
        3 => "Freeway",
        4 => "Ramp",
        5 => "Walking Trail",
        6 => "Major Highway",
        7 => "Minor Highway",
        8 => "Off Road",
        9 => "Walkway",
        10 => "Pedestrian Boardwalk",
        15 => "Ferry",
        16 => "Stairway",
        17 => "Private Road",
        18 => "Railroad",
        19 => "Runway / Taxiway",
        20 => "Parking Lot Road",
        22 => "Alley",
        _ => "Road",
    }
}

/// The type's Waze-editor colour (the palette the vendor paints its own editor with — the
/// freeway's brick red and the major's orange are the ones everyone knows on sight).
pub fn road_type_color(rt: u8) -> [u8; 3] {
    match rt {
        3 => [0xc1, 0x56, 0x43],  // Freeway — the brick red
        6 => [0xf1, 0x9b, 0x53],  // Major Highway — orange
        7 => [0xff, 0xf0, 0x43],  // Minor Highway — yellow
        4 => [0xc9, 0x73, 0xd6],  // Ramp — lilac
        2 => [0xf6, 0xf1, 0x6f],  // Primary Street — pale yellow
        1 => [0xff, 0xff, 0xff],  // Street — white
        18 => [0x8a, 0x8a, 0x8a], // Railroad — grey
        20 => [0xb4, 0xb4, 0xb4], // Parking Lot Road — grey
        19 => [0xb3, 0xb3, 0xb3], // Runway / Taxiway — grey
        8 => [0xb0, 0x90, 0x6a],  // Off Road — dirt tan
        17 => [0xc9, 0xb7, 0x9c], // Private Road — sand
        22 => [0xd2, 0xb4, 0x8c], // Alley — tan
        15 => [0x5f, 0xbf, 0xae], // Ferry — teal
        5 => [0x7b, 0xc7, 0x75],  // Walking Trail — green
        9 => [0xa8, 0xc6, 0x86],  // Walkway — sage
        10 => [0xc9, 0xa6, 0x6b], // Pedestrian Boardwalk — tan
        16 => [0x3f, 0x91, 0x42], // Stairway — green
        _ => [0xcc, 0xcc, 0xcc],
    }
}

/// The draw order: big roads last (over the pack) — freeways top, collectors above locals,
/// the greys and greens at the bottom.
pub fn road_type_rank(rt: u8) -> u8 {
    match rt {
        3 => 12,     // Freeway
        6 => 10,     // Major Highway
        4 => 9,      // Ramp
        7 => 8,      // Minor Highway
        2 => 7,      // Primary Street
        18 => 6,     // Railroad
        15 => 4,     // Ferry
        8 | 17 => 3, // Off Road, Private Road
        1 => 2,      // Street
        _ => 1,      // trails, walkways, parking, alleys, the rest
    }
}

/// Whether the type draws dashed (the non-driving ways read dashed, the vendor's own way).
pub fn road_type_dashed(rt: u8) -> bool {
    matches!(rt, 5 | 9 | 10 | 15 | 16 | 18 | 19)
}

/// The type's stroke width at zoom scale 1 (the draw scales it with the zoom like the rest
/// of the fleet overlays) — the big classes read wide, the footways read hair-thin.
pub fn road_type_width(rt: u8) -> f32 {
    match rt {
        3 => 2.8,  // Freeway
        6 => 2.3,  // Major Highway
        7 => 1.9,  // Minor Highway
        4 => 1.7,  // Ramp
        2 => 1.7,  // Primary Street
        1 => 1.3,  // Street
        18 => 1.6, // Railroad
        15 => 1.8, // Ferry
        19 => 2.0, // Runway / Taxiway
        _ => 1.1,
    }
}

/// The big road classes that get a dark casing under their colour at street zooms (the
/// vendor's own depth trick: the casing separates the road from the tile's grey streets).
pub fn road_type_cased(rt: u8) -> bool {
    matches!(rt, 3 | 4 | 6 | 7 | 2)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A trimmed piece of the real Kinondoni (Dar es Salaam) answer — one named two-way
    /// street, one unnamed one-way pair of bits, the street table the names point into.
    fn fixture() -> Vec<u8> {
        r#"{
          "segments": { "roadTypes": [1,2], "objects": [
            { "id": 384189158, "roadType": 2, "fwdDirection": true, "revDirection": true,
              "primaryStreetID": 42206414, "fwdMaxSpeed": null, "revMaxSpeed": 50,
              "length": 108, "level": 0, "hasClosures": false, "isPricedSegment": false,
              "geometry": { "type": "LineString", "coordinates": [[39.2242, -6.7740], [39.2250, -6.7734]] } },
            { "id": 384189159, "roadType": 1, "fwdDirection": true, "revDirection": false,
              "primaryStreetID": 8239092, "fwdMaxSpeed": 30, "revMaxSpeed": null,
              "length": 42, "level": -1, "hasClosures": true, "isPricedSegment": true,
              "geometry": { "type": "LineString", "coordinates": [[39.22, -6.78], [39.221, -6.781], [39.222, -6.782]] } },
            { "id": 384189160, "roadType": 7, "fwdDirection": true, "revDirection": true,
              "primaryStreetID": 42206084, "length": 300, "level": 0, "hasClosures": false, "isPricedSegment": false,
              "geometry": { "type": "MultiLineString", "coordinates": [[[39.21, -6.77]], [[39.212, -6.771], [39.214, -6.772]]] } }
          ]},
          "streets": { "objects": [
            { "id": 42206414, "name": "Juma Ikangaa Rd" },
            { "id": 8239092, "name": "" },
            { "id": 42206084, "name": "Silvesta St" }
          ]}
        }"#
        .as_bytes()
        .to_vec()
    }

    #[test]
    fn parses_the_real_shapes() {
        let segs = parse_features(&fixture()).unwrap();
        assert_eq!(segs.len(), 3, "every segment with a drawable line rides");
        let named = segs.iter().find(|s| s.id == 384189158).unwrap();
        assert_eq!(named.name, "Juma Ikangaa Rd");
        assert_eq!(named.road_type, 2);
        assert!(named.fwd && named.rev);
        assert_eq!(named.fwd_speed, None);
        assert_eq!(named.rev_speed, Some(50));
        assert_eq!(named.pts.len(), 2);
        assert!((named.bbox[0] - 39.2242).abs() < 1e-9 && (named.bbox[2] - 39.2250).abs() < 1e-9);
        let tunnel = segs.iter().find(|s| s.id == 384189159).unwrap();
        assert_eq!(tunnel.level, -1, "tunnels ride negative levels");
        assert!(tunnel.closures && tunnel.toll);
        assert!(!tunnel.rev, "the one-way bit rode through");
        assert_eq!(tunnel.name, "", "an empty street name stays empty");
        let multi = segs.iter().find(|s| s.id == 384189160).unwrap();
        assert_eq!(multi.pts.len(), 3, "the MultiLineString flattens into the one polyline the map draws");
        assert_eq!(multi.name, "Silvesta St");
    }

    #[test]
    fn the_legend_is_wazes_own() {
        // The numbers the Dubai/Dar data proved (E11 on 3, the Metro on 18, parking on 20).
        assert_eq!(road_type_name(3), "Freeway");
        assert_eq!(road_type_name(4), "Ramp");
        assert_eq!(road_type_name(6), "Major Highway");
        assert_eq!(road_type_name(7), "Minor Highway");
        assert_eq!(road_type_name(18), "Railroad");
        assert_eq!(road_type_name(20), "Parking Lot Road");
        assert_eq!(road_type_name(22), "Alley");
        // Draw order: the freeway over everything, the footways at the bottom.
        assert!(road_type_rank(3) > road_type_rank(1));
        assert!(road_type_rank(1) > road_type_rank(5));
        // The big classes case, the footways dash.
        assert!(road_type_cased(3) && road_type_cased(7));
        assert!(!road_type_cased(1));
        assert!(road_type_dashed(18) && road_type_dashed(5));
        assert!(!road_type_dashed(3));
    }

    #[test]
    fn the_url_is_quantized_and_editor_shaped() {
        let url = features_url((39.21491645, -6.78358375, 39.22856354, -6.77257619), 16.4);
        assert!(url.starts_with("https://www.waze.com/row-Descartes/app/Features?bbox=39.21,-6.79,39.23,-6.77&"), "{url}");
        assert!(url.contains("apiV2=true"), "the v2 API flag rides");
        assert!(url.contains("sandbox=true"), "the sandbox flag keeps the read anonymous");
        assert!(url.contains("venueLevel=4"), "the venue level rides (its absence flips the endpoint to the logged-in mode)");
        assert!(url.contains("zoomLevel=16"), "the zoom rounds");
        // A pan inside the same 0.01° cell reuses the exact URL (the runtime's skip check).
        let again = features_url((39.2150, -6.7850, 39.2250, -6.7750), 16.2);
        assert_eq!(url, again, "the same cell, the same URL");
        // The zoom clamps into the editor's sane band.
        assert!(features_url((39.2, -6.8, 39.23, -6.77), 2.0).contains("zoomLevel=6"));
        assert!(features_url((39.2, -6.8, 39.23, -6.77), 25.0).contains("zoomLevel=19"));
    }

    #[test]
    fn a_gate_page_is_an_error_and_a_quiet_area_is_empty() {
        assert!(parse_features(b"<!DOCTYPE html>").is_err(), "a gate page reads as the error it is");
        let quiet = parse_features(br#"{"segments":{"objects":[]},"streets":{"objects":[]}}"#).unwrap();
        assert!(quiet.is_empty(), "a quiet box is an honest empty");
    }
}
