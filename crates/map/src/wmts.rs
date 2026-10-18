//! OGC WMTS (Web Map Service Tile Matrix Set) tile sources: KVP GetTile URLs, REST template
//! filling and a minimal GetCapabilities probe. URL building and string scanning only — the
//! host fetches.

use crate::wms::{ascii_lower, element_text, find_from, tag_boundary};

/// One `<Layer>` from a WMTS GetCapabilities document: everything its GetTile needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WmtsLayer {
    /// The layer's `<ows:Identifier>`.
    pub layer: String,
    /// The default (or chosen) `<Style>`'s identifier — usually "default".
    pub style: String,
    /// The `<TileMatrixSet>` the layer serves.
    pub matrix_set: String,
    /// The first `<Format>` ("image/png", …).
    pub format: String,
}

/// The GetTile URL for tile `(z, x, y)`. A `base_url` that already names `{TileMatrix}`-style
/// placeholders (any case) is a REST template: every placeholder — `{Layer}`, `{Style}`,
/// `{TileMatrixSet}`, `{TileMatrix}`, `{TileRow}`, `{TileCol}`, `{Format}` — is filled in place.
/// Anything else gets the KVP form: SERVICE=WMTS, REQUEST=GetTile, VERSION=1.0.0, LAYER, STYLE,
/// FORMAT, TILEMATRIXSET, then TILEMATRIX/TILEROW/TILECOL as the tile's z/y/x.
pub fn tile_url(base_url: &str, layer: &WmtsLayer, z: u32, x: u32, y: u32) -> String {
    if ascii_lower(base_url).contains("{tilematrix}") {
        let format = layer.format.strip_prefix("image/").unwrap_or(&layer.format);
        let url = replace_ci(base_url, "{layer}", &layer.layer);
        let url = replace_ci(&url, "{style}", &layer.style);
        let url = replace_ci(&url, "{tilematrixset}", &layer.matrix_set);
        let url = replace_ci(&url, "{tilematrix}", &z.to_string());
        let url = replace_ci(&url, "{tilerow}", &y.to_string());
        let url = replace_ci(&url, "{tilecol}", &x.to_string());
        return replace_ci(&url, "{format}", format);
    }
    let base = base_url.trim_end_matches(['?', '&']);
    let sep = if base.contains('?') { '&' } else { '?' };
    let WmtsLayer { layer, style, matrix_set, format } = layer;
    format!(
        "{base}{sep}SERVICE=WMTS&VERSION=1.0.0&REQUEST=GetTile&LAYER={layer}&STYLE={style}&FORMAT={format}\
         &TILEMATRIXSET={matrix_set}&TILEMATRIX={z}&TILEROW={y}&TILECOL={x}"
    )
}

/// Replace every case-insensitive `needle` with `repl` (REST templates arrive as `{TileMatrix}`
/// or `{tilematrix}` or anything between).
fn replace_ci(hay: &str, needle: &str, repl: &str) -> String {
    let lower = ascii_lower(hay);
    let mut out = String::with_capacity(hay.len());
    let mut at = 0usize;
    while let Some(p) = lower[at..].find(needle) {
        let p = at + p;
        out.push_str(&hay[at..p]);
        out.push_str(repl);
        at = p + needle.len();
    }
    out.push_str(&hay[at..]);
    out
}

/// A minimal GetCapabilities probe: every `<Layer>` in `<Contents>` with its `<ows:Identifier>`
/// (namespace prefix optional), the first `<Style>`'s identifier, `<TileMatrixSet>` and first
/// `<Format>`. Plain case-insensitive string scanning — no XML parser. Tile-matrix-set
/// definitions outside `<Layer>` don't report.
pub fn parse_capabilities_summary(xml: &str) -> Vec<WmtsLayer> {
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
            at = gt + 1;
            continue;
        }
        let seg_end = find_from(&lower, "<layer", gt + 1).unwrap_or(lower.len());
        let seg_end = find_from(&lower, "</layer", gt + 1).map_or(seg_end, |c| c.min(seg_end));
        let seg = gt + 1..seg_end;
        let ids = ["<ows:identifier", "<identifier"];
        let name = element_text(xml, &lower, seg.clone(), &ids).unwrap_or_default();
        if !name.is_empty() {
            let style = style_identifier(xml, &lower, seg.clone()).unwrap_or_default();
            let matrix_set = element_text(xml, &lower, seg.clone(), &["<tilematrixset"]).unwrap_or_default();
            let format = element_text(xml, &lower, seg, &["<format"]).unwrap_or_default();
            out.push(WmtsLayer { layer: name, style, matrix_set, format });
        }
        at = seg_end;
    }
    out
}

/// The identifier inside the first `<Style>` element of the range.
fn style_identifier(xml: &str, lower: &str, range: std::ops::Range<usize>) -> Option<String> {
    let mut at = range.start;
    while let Some(p) = find_from(lower, "<style", at) {
        if p >= range.end {
            break;
        }
        if !tag_boundary(lower.as_bytes(), p + 6) {
            at = p + 6;
            continue;
        }
        let Some(gt) = lower[p + 6..].find('>').map(|q| p + 6 + q) else { break };
        if gt > 0 && lower.as_bytes()[gt - 1] == b'/' {
            return None; // self-closing <Style/>
        }
        let close = find_from(lower, "</style", gt + 1).map_or(range.end, |c| c.min(range.end));
        return element_text(xml, lower, gt + 1..close, &["<ows:identifier", "<identifier"]);
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn layer() -> WmtsLayer {
        WmtsLayer { layer: "world".into(), style: "default".into(), matrix_set: "GoogleMapsCompatible".into(), format: "image/png".into() }
    }

    #[test]
    fn kvp_gettile_url() {
        let url = tile_url("https://example.com/wmts", &layer(), 7, 3, 5);
        assert_eq!(
            url,
            "https://example.com/wmts?SERVICE=WMTS&VERSION=1.0.0&REQUEST=GetTile&LAYER=world&STYLE=default\
             &FORMAT=image/png&TILEMATRIXSET=GoogleMapsCompatible&TILEMATRIX=7&TILEROW=5&TILECOL=3"
        );
        // An existing query joins with &.
        let url = tile_url("https://example.com/wmts?token=t", &layer(), 0, 0, 0);
        assert!(url.starts_with("https://example.com/wmts?token=t&SERVICE=WMTS"), "{url}");
        assert!(url.contains("TILEMATRIX=0&TILEROW=0&TILECOL=0"), "{url}");
    }

    #[test]
    fn rest_template_fills_case_insensitively() {
        let base = "https://maps.example.com/wmts/1.0.0/{Layer}/default/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.png";
        let url = tile_url(base, &layer(), 4, 2, 1);
        assert_eq!(url, "https://maps.example.com/wmts/1.0.0/world/default/GoogleMapsCompatible/4/1/2.png");
        // Lowercase placeholders fill too, and {Format} loses its image/ prefix.
        let base = "https://m.example.com/{layer}/{style}/{tilematrixset}/{tilematrix}/{tilerow}/{tilecol}.{format}";
        let url = tile_url(base, &layer(), 9, 8, 7);
        assert_eq!(url, "https://m.example.com/world/default/GoogleMapsCompatible/9/7/8.png");
    }

    #[test]
    fn capabilities_scan_layers() {
        let xml = r#"<Capabilities xmlns:ows="http://www.opengis.net/ows/1.1"><Contents>
            <Layer><ows:Title>Imagery</ows:Title><ows:Identifier>world</ows:Identifier>
                <Style isDefault="true"><ows:Identifier>default</ows:Identifier></Style>
                <Format>image/png</Format><Format>image/jpeg</Format>
                <TileMatrixSetLink><TileMatrixSet>GoogleMapsCompatible</TileMatrixSet></TileMatrixSetLink>
                <TileMatrixSetLimits><TileMatrixSetLimits-never/></TileMatrixSetLimits>
            </Layer>
            <Layer><ows:Identifier>bare</ows:Identifier></Layer>
            <TileMatrixSet><ows:Identifier>GoogleMapsCompatible</ows:Identifier>
                <TileMatrix><ows:Identifier>0</ows:Identifier></TileMatrix>
            </TileMatrixSet>
        </Contents></Capabilities>"#;
        let layers = parse_capabilities_summary(xml);
        assert_eq!(
            layers,
            vec![
                WmtsLayer { layer: "world".into(), style: "default".into(), matrix_set: "GoogleMapsCompatible".into(), format: "image/png".into() },
                WmtsLayer { layer: "bare".into(), style: String::new(), matrix_set: String::new(), format: String::new() },
            ],
            "two layers; the TileMatrixSet definition outside <Layer> is not a layer"
        );
    }

    #[test]
    fn capabilities_scan_tolerates_no_namespace_prefix() {
        let xml = "<Layer><Identifier>plain</Identifier><Style><Identifier>sty</Identifier></Style>\
                   <Format>image/jpeg</Format><TileMatrixSet>EGM95</TileMatrixSet></Layer>";
        assert_eq!(
            parse_capabilities_summary(xml),
            vec![WmtsLayer { layer: "plain".into(), style: "sty".into(), matrix_set: "EGM95".into(), format: "image/jpeg".into() }]
        );
    }
}
