//! OGC WMS (Web Map Service) tile sources: the GetMap URL whose BBOX is an XYZ tile's
//! Web-Mercator bbox, and a minimal GetCapabilities probe. URL building and string scanning
//! only — the host fetches.

use crate::{TileId, doc_pt_to_mercator};

/// The GetMap parameters a WMS tile source carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WmsParams {
    /// Comma-separated layer names (the LAYERS value).
    pub layers: String,
    /// The STYLES value (usually empty, sometimes "default").
    pub styles_param: String,
    /// The protocol version — "1.3.1" (default) or "1.1.1".
    pub version: String,
    /// The request CRS/SRS — EPSG:3857 for tile basemaps.
    pub crs: String,
    /// The image format ("image/png", "image/jpeg"…).
    pub format: String,
    /// Ask for transparency.
    pub transparent: bool,
}

impl Default for WmsParams {
    fn default() -> Self {
        Self {
            layers: String::new(),
            styles_param: String::new(),
            version: "1.3.1".into(),
            crs: "EPSG:3857".into(),
            format: "image/png".into(),
            transparent: false,
        }
    }
}

/// One named layer from a GetCapabilities document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WmsLayer {
    pub name: String,
    pub title: String,
}

/// An XYZ tile's Web-Mercator bbox, `(min_x, min_y, max_x, max_y)` metres — what a GetMap for
/// that tile asks for. Computed from the crate's own tile placement.
pub fn tile_bbox_meters(z: u32, x: u32, y: u32) -> (f64, f64, f64, f64) {
    let (dx0, dy0, dx1, dy1) = TileId::new(z.min(u8::MAX as u32) as u8, x, y).rect_pt();
    let (min_x, _) = doc_pt_to_mercator(dx0, 0.0);
    let (max_x, _) = doc_pt_to_mercator(dx1, 0.0);
    let (_, max_y) = doc_pt_to_mercator(0.0, dy0); // doc y grows south; the tile's top edge is north
    let (_, min_y) = doc_pt_to_mercator(0.0, dy1);
    (min_x, min_y, max_x, max_y)
}

/// The KVP GetMap URL for tile `(z, x, y)`: `base_url` (with or without a `?` already in it)
/// plus SERVICE=WMS, REQUEST=GetMap and the tile's Web-Mercator BBOX at 256×256. WMS 1.1.1 asks
/// with SRS=, 1.3.x with CRS= — EPSG:3857's axes stay easting/northing either way, so the BBOX
/// is `min_x,min_y,max_x,max_y` in both.
pub fn tile_url(base_url: &str, params: &WmsParams, z: u32, x: u32, y: u32) -> String {
    let (min_x, min_y, max_x, max_y) = tile_bbox_meters(z, x, y);
    let axis = if params.version.starts_with("1.1") { "SRS" } else { "CRS" };
    let base = base_url.trim_end_matches(['?', '&']);
    let sep = if base.contains('?') { '&' } else { '?' };
    let transparent = if params.transparent { "TRUE" } else { "FALSE" };
    let WmsParams { layers, styles_param, version, crs, format, .. } = params;
    format!(
        "{base}{sep}SERVICE=WMS&VERSION={version}&REQUEST=GetMap&LAYERS={layers}&STYLES={styles_param}&{axis}={crs}\
         &BBOX={min_x},{min_y},{max_x},{max_y}&WIDTH=256&HEIGHT=256&FORMAT={format}&TRANSPARENT={transparent}"
    )
}

/// A minimal GetCapabilities probe: every `<Layer>` that carries a `<Name>` (group layers don't),
/// with its `<Title>`. Plain case-insensitive string scanning — no XML parser. Names are taken
/// from before any nested `<Layer>`, so parent and children all report.
pub fn parse_capabilities_summary(xml: &str) -> Vec<WmsLayer> {
    let lower = ascii_lower(xml);
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(start) = find_from(&lower, "<layer", at) {
        if !tag_boundary(lower.as_bytes(), start + 6) {
            at = start + 6;
            continue;
        }
        let Some(gt) = lower[start..].find('>').map(|p| start + p) else { break };
        if gt > 0 && lower.as_bytes()[gt - 1] == b'/' {
            at = gt + 1; // self-closing layer
            continue;
        }
        // The layer's own Name/Title precede any nested <Layer>; bound the scan by the next
        // layer's open tag or this layer's close, whichever comes first.
        let seg_end = find_from(&lower, "<layer", gt + 1).unwrap_or(lower.len());
        let seg_end = find_from(&lower, "</layer", gt + 1).map_or(seg_end, |c| c.min(seg_end));
        let seg = gt + 1..seg_end;
        let name = element_text(xml, &lower, seg.clone(), &["<name"]).unwrap_or_default();
        let title = element_text(xml, &lower, seg, &["<title"]).unwrap_or_default();
        if !name.is_empty() {
            out.push(WmsLayer { name, title });
        }
        at = seg_end;
    }
    out
}

// -- Small scanning helpers shared with wmts.rs ----------------------------------------------

/// The text with only ASCII letters lowered: byte positions keep matching the original, so a
/// position found here indexes `xml` too.
pub(crate) fn ascii_lower(s: &str) -> String {
    s.chars().map(|c| c.to_ascii_lowercase()).collect()
}

/// The first position `needle` occurs at in `hay` at or after `from`.
pub(crate) fn find_from(hay: &str, needle: &str, from: usize) -> Option<usize> {
    hay.get(from..)?.find(needle).map(|p| p + from)
}

/// Does the tag name end at `at` (the byte is a boundary, not a longer name)? Rejects
/// `<TileMatrixSet` matching inside `<TileMatrixSetLink`, for instance.
pub(crate) fn tag_boundary(hay: &[u8], at: usize) -> bool {
    matches!(hay.get(at), None | Some(b'>' | b'/' | b' ' | b'\t' | b'\n' | b'\r'))
}

/// The text of the first element whose open tag matches one of `opens` (case-insensitively, tag
/// name bounded), within `xml[range]`. The close tag is the open tag's name re-prefixed with `/`.
pub(crate) fn element_text(xml: &str, lower: &str, range: std::ops::Range<usize>, opens: &[&str]) -> Option<String> {
    for open in opens {
        if open.len() < 2 || !open.starts_with('<') {
            continue;
        }
        let close = format!("</{}", &open[1..]);
        let mut at = range.start;
        while let Some(p) = find_from(lower, open, at) {
            if p >= range.end {
                break;
            }
            let after = p + open.len();
            if !tag_boundary(lower.as_bytes(), after) {
                at = after;
                continue;
            }
            let Some(gt) = lower[after..].find('>').map(|p| after + p) else { break };
            if gt > 0 && lower.as_bytes()[gt - 1] == b'/' {
                at = gt + 1;
                continue; // self-closing: no content
            }
            let Some(cp) = find_close(lower, &close, gt + 1) else {
                at = gt + 1;
                continue;
            };
            return Some(xml[gt + 1..cp].trim().to_string());
        }
    }
    None
}

/// The close tag's position, with the same name-boundary rule as opens.
fn find_close(lower: &str, close: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(p) = find_from(lower, close, at) {
        if tag_boundary(lower.as_bytes(), p + close.len()) {
            return Some(p);
        }
        at = p + close.len();
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::WORLD_METERS;

    #[test]
    fn bbox_matches_hand_computed_mercator() {
        let half = WORLD_METERS / 2.0;
        // z1/x1/y0 covers the north-east quarter of the projected world.
        assert_eq!(tile_bbox_meters(1, 1, 0), (0.0, 0.0, half, half));
        // z2/x1/y1 is the quarter tile just west of the prime meridian, straddling the equator.
        let (min_x, min_y, max_x, max_y) = tile_bbox_meters(2, 1, 1);
        assert!((min_x - -half / 2.0).abs() < 1e-6, "{min_x}");
        assert!((min_y - 0.0).abs() < 1e-6, "{min_y}");
        assert!((max_x - 0.0).abs() < 1e-6, "{max_x}");
        assert!((max_y - half / 2.0).abs() < 1e-6, "{max_y}");
        // z0 is the whole world square.
        assert_eq!(tile_bbox_meters(0, 0, 0), (-half, -half, half, half));
    }

    #[test]
    fn getmap_url_1_3_1() {
        let params = WmsParams { layers: "roads".into(), ..WmsParams::default() };
        let url = tile_url("https://example.com/wms", &params, 1, 1, 0);
        let half = WORLD_METERS / 2.0;
        assert_eq!(
            url,
            format!(
                "https://example.com/wms?SERVICE=WMS&VERSION=1.3.1&REQUEST=GetMap&LAYERS=roads&STYLES=&CRS=EPSG:3857\
                 &BBOX=0,0,{half},{half}&WIDTH=256&HEIGHT=256&FORMAT=image/png&TRANSPARENT=FALSE"
            )
        );
    }

    #[test]
    fn getmap_url_1_1_1_uses_srs() {
        let params = WmsParams { version: "1.1.1".into(), transparent: true, ..WmsParams::default() };
        let url = tile_url("https://example.com/wms?foo=bar", &params, 0, 0, 0);
        assert!(url.contains("SRS=EPSG:3857"), "{url}");
        assert!(!url.contains("CRS="), "{url}");
        assert!(url.contains("TRANSPARENT=TRUE"), "{url}");
        // The base's own query survives; joining goes through &.
        assert!(url.starts_with("https://example.com/wms?foo=bar&SERVICE=WMS"), "{url}");
        // A trailing & (or ?) doesn't double up.
        let url = tile_url("https://example.com/wms?foo=bar&", &params, 0, 0, 0);
        assert!(url.starts_with("https://example.com/wms?foo=bar&SERVICE=WMS"), "{url}");
        assert!(!url.contains("&&"), "{url}");
    }

    #[test]
    fn capabilities_scan_names_and_titles() {
        let xml = r#"<?xml version="1.0"?><WMS_Capabilities version="1.3.0"><Capability><Layer>
            <Title>The root has no Name</Title>
            <Layer><Name>roads</Name><Title>Roads</Title></Layer>
            <Layer queryable="1"><Name>radar</Name><Title>Base Reflectivity</Title>
                <Layer><Name>radar_child</Name><Title>Nested child</Title></Layer>
            </Layer>
            <Layer><Title>A nameless group</Title></Layer>
            <Layers><Name>not a layer tag</Name></Layers>
        </Layer></Capability></WMS_Capabilities>"#;
        let layers = parse_capabilities_summary(xml);
        assert_eq!(
            layers,
            vec![
                WmsLayer { name: "roads".into(), title: "Roads".into() },
                WmsLayer { name: "radar".into(), title: "Base Reflectivity".into() },
                WmsLayer { name: "radar_child".into(), title: "Nested child".into() },
            ]
        );
    }

    #[test]
    fn capabilities_scan_is_case_insensitive() {
        let xml = "<layer><NAME>base</NAME><Title>Base</Title></layer>";
        assert_eq!(parse_capabilities_summary(xml), vec![WmsLayer { name: "base".into(), title: "Base".into() }]);
        assert!(parse_capabilities_summary("no layers here").is_empty());
    }
}
