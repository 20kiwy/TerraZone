//! TerraZone's map layer: Web Mercator (EPSG:3857) document mapping, XYZ tile sources, and the
//! native map-format readers (GeoJSON, Shapefile) plus WMS/WMTS/Google Map Tiles source helpers.
//!
//! A TerraZone document *is* a map: document points are Web Mercator world pixels at
//! [`REF_ZOOM`] — the whole world is [`WORLD_PT`] points square, x grows east from the antimeridian
//! and y grows south from the north's Mercator limit, so y points down like every document does.
//! Longitude/latitude (WGS84, degrees) convert through [`lonlat_to_doc`]/[`doc_to_lonlat`]; tiles
//! from any XYZ source ([`TileSource`]) place through [`TileId::rect_pt`].
//!
//! Pure math and data only: fetching, decoding and drawing tiles live above this crate (the app
//! fetches, the UI decodes and draws), so it compiles everywhere including wasm32.

use serde::{Deserialize, Serialize};

/// The zoom whose tile pixels are document points: one point = one tile pixel of zoom 4.
pub const REF_ZOOM: u8 = 4;
/// Document points across the whole Mercator world (256 × 2^[`REF_ZOOM`] = 4096).
pub const WORLD_PT: f64 = 256.0 * (1u32 << REF_ZOOM) as f64;
/// The Mercator latitude limit (±85.0511…°), where the projection reaches ±∞.
pub const LAT_LIMIT: f64 = 85.0511287798066;
/// The highest tile zoom any source may serve (street level).
pub const MAX_ZOOM: u8 = 22;

mod geojson;
mod google;
mod mvt;
mod shapefile;
mod wms;
mod wmts;

pub use geojson::{
    GeoFeature, GeoGeometry, GeoJsonError, GeoValue, MERCATOR_RADIUS, MercatorFeature, MercatorGeometry, Pos, Ring, WORLD_METERS, doc_pt_to_mercator,
    lonlat_to_mercator, mercator_to_doc_pt, mercator_to_lonlat, parse_geojson, write_geojson,
};
pub use google::{
    Aircraft, Airport, FlightRoute, GoogleError, MAX_CONTENT_URIS, TILE_HOST, Tileset, TilesetNode, adsbdb_flights_url, adsbdb_url,
    collect_content_uris, load_tileset, parse_aircraft, parse_flightroute, parse_session_response, session_request, tile_url as google_tile_url,
};
pub use mvt::{MvtGeom, decode_mvt};
pub use shapefile::{ShapeError, ShapeRecord, read_shapefile};
pub use wms::{WmsLayer, WmsParams, parse_capabilities_summary as parse_wms_capabilities, tile_bbox_meters, tile_url as wms_tile_url};
pub use wmts::{WmtsLayer, parse_capabilities_summary as parse_wmts_capabilities, tile_url as wmts_tile_url};

/// `lat` clamped into the Mercator range (out-of-range latitudes would run off the world square).
pub fn clamp_lat(lat: f64) -> f64 {
    if lat.is_finite() { lat.clamp(-LAT_LIMIT, LAT_LIMIT) } else { 0.0 }
}

/// Longitude/latitude (WGS84, degrees) → document points.
pub fn lonlat_to_doc(lon: f64, lat: f64) -> (f64, f64) {
    let lat = clamp_lat(lat);
    let lon = if lon.is_finite() { lon } else { 0.0 };
    let x = (lon + 180.0) / 360.0 * WORLD_PT;
    let y = (1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * WORLD_PT;
    (x, y)
}

/// Document points → longitude/latitude (WGS84, degrees), the latitude clamped into the Mercator
/// range.
pub fn doc_to_lonlat(x: f64, y: f64) -> (f64, f64) {
    let lon = x / WORLD_PT * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * y / WORLD_PT)).sinh().atan().to_degrees();
    (lon, clamp_lat(lat))
}

/// One XYZ tile: `z`oom, `x` column and `y` row (slippy map scheme, y 0 at the north).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// Is the tile on the world square at its zoom?
    pub fn valid(self) -> bool {
        self.z <= MAX_ZOOM && self.x < (1u32 << self.z) && self.y < (1u32 << self.z)
    }

    /// The tile's edge length in document points.
    pub fn edge_pt(self) -> f64 {
        WORLD_PT / (1u64 << self.z.min(30)) as f64
    }

    /// The tile's document-space rect as `(x0, y0, x1, y1)`.
    pub fn rect_pt(self) -> (f64, f64, f64, f64) {
        let e = self.edge_pt();
        (self.x as f64 * e, self.y as f64 * e, (self.x as f64 + 1.0) * e, (self.y as f64 + 1.0) * e)
    }

    /// The tile's URL from `source`'s template (`{z}`, `{x}`, `{y}`, `{s}`, `{r}` and `{key}`
    /// substituted; `{r}` goes, tiles are fetched at 1×; `{key}` is the source's API key —
    /// Mapbox's access token and friends).
    pub fn url(self, source: &TileSource) -> String {
        let s =
            source.subdomains.get(((self.x.wrapping_add(self.y)) % source.subdomains.len().max(1) as u32) as usize).map(String::as_str).unwrap_or("");
        source
            .template
            .replace("{z}", &self.z.to_string())
            .replace("{x}", &self.x.to_string())
            .replace("{y}", &self.y.to_string())
            .replace("{s}", s)
            .replace("{r}", "")
            .replace("{key}", &source.api_key)
    }
}

/// The tiles covering the document-space view `(x0, y0, x1, y1)` at `scale` screen pixels per
/// document point: the zoom whose tiles are closest to 256 screen pixels, and at most `max_tiles`
/// tiles (row-major) that intersect the view. `(zoom, tiles)`.
pub fn tiles_for_view(view: (f64, f64, f64, f64), scale: f64, max_zoom: u8, max_tiles: usize) -> (u8, Vec<TileId>) {
    tiles_for_view_range(view, scale, 0, max_zoom, max_tiles)
}

/// [`tiles_for_view`] with the source's minimum zoom honoured (a connection may say its tiles
/// start at z3, say): the picked zoom clamps into `min_zoom..=max_zoom`.
pub fn tiles_for_view_range(view: (f64, f64, f64, f64), scale: f64, min_zoom: u8, max_zoom: u8, max_tiles: usize) -> (u8, Vec<TileId>) {
    let (x0, y0, x1, y1) = (view.0.min(view.2), view.1.min(view.3), view.0.max(view.2), view.1.max(view.3));
    let min_zoom = min_zoom.min(MAX_ZOOM);
    let max_zoom = max_zoom.min(MAX_ZOOM).max(min_zoom);
    let z = if scale.is_finite() && scale > 0.0 {
        ((REF_ZOOM as f64 + scale.log2()).round() as i32).clamp(min_zoom as i32, max_zoom as i32) as u8
    } else {
        min_zoom
    };
    let edge = WORLD_PT / (1u64 << z) as f64;
    let n = 1u32 << z;
    let (tx0, tx1) = ((x0 / edge).floor().max(0.0) as u32, ((x1 / edge).floor() as u32).min(n.saturating_sub(1)));
    let (ty0, ty1) = ((y0 / edge).floor().max(0.0) as u32, ((y1 / edge).floor() as u32).min(n.saturating_sub(1)));
    let mut tiles = Vec::new();
    if tx0 <= tx1 && ty0 <= ty1 {
        for y in ty0..=ty1 {
            for x in tx0..=tx1 {
                tiles.push(TileId::new(z, x, y));
            }
        }
    }
    // Too many (a deep zoom out over an empty region): keep the middle rows, where the view's
    // centre is.
    if tiles.len() > max_tiles {
        let keep = max_tiles.max(1);
        let first = (tiles.len() - keep) / 2;
        tiles = tiles[first..first + keep].to_vec();
    }
    (z, tiles)
}

/// What a tile source serves: picture tiles, vector tiles, an OGC WMS/WMTS service, or the
/// Google Map Tiles API (2D session tiles or a 3D TileSet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TileKind {
    /// PNG/JPEG XYZ tiles.
    #[default]
    Raster,
    /// Mapbox Vector Tiles (.mvt protobuf), drawn as linework.
    Vector,
    /// An OGC WMS GetMap service: `template` is the base URL, `layers` names the layers.
    Wms,
    /// An OGC WMTS GetTile service: `layers` is the layer, `matrix_set`/`style_name`/`format` the
    /// tile matrix set, style and image format.
    Wmts,
    /// Google Map Tiles (BYOK): the host runs the createSession handshake with `api_key` and
    /// stores the token in `session`; tile URLs then build from the pair.
    #[serde(rename = "googleSession")]
    GoogleSession,
    /// Google Photorealistic 3D Tiles (BYOK): `template` is the TileSet.json URL the host loads
    /// (mesh rendering needs the future 3D viewport; the 2D canvas falls back to session tiles).
    #[serde(rename = "google3dTiles")]
    Google3dTiles,
}

/// One tile source: a URL template and its attribution (shown on the canvas, as the providers'
/// terms require).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TileSource {
    /// Stable id (`map.setSource` picks by it; the tile cache keys by it).
    pub id: String,
    /// The name the Tile Sources dialog and menus show.
    pub name: String,
    /// The URL template, with `{z}`, `{x}`, `{y}`, optional `{s}` (subdomains), `{r}` and `{key}`.
    pub template: String,
    /// The attribution line drawn on the map ("© OpenStreetMap contributors", …).
    pub attribution: String,
    /// The lowest zoom the source serves (0 unless the provider says otherwise).
    pub min_zoom: u8,
    /// The highest zoom the source serves.
    pub max_zoom: u8,
    /// The subdomains `{s}` cycles through (`a`, `b`, `c`…); empty means the template has no `{s}`.
    pub subdomains: Vec<String>,
    /// The API key put in the template's `{key}` (Mapbox's access token, Google's key…).
    #[serde(default)]
    pub api_key: String,
    /// Raster (default), vector, WMS, WMTS or a Google Map Tiles kind.
    #[serde(default)]
    pub kind: TileKind,
    /// The WMS layers (comma-separated) or the WMTS layer; empty for XYZ sources.
    #[serde(default)]
    pub layers: String,
    /// The WMTS tile matrix set.
    #[serde(default)]
    pub matrix_set: String,
    /// The WMS STYLES value / WMTS style identifier (usually empty or "default").
    #[serde(default)]
    pub style_name: String,
    /// The image format requested ("image/png", …).
    #[serde(default)]
    pub format: String,
    /// The Google Map Tiles session token (from createSession), paired with `api_key`.
    #[serde(default)]
    pub session: String,
}

impl TileSource {
    /// A new source.
    pub fn new(id: &str, name: &str, template: &str, attribution: &str, max_zoom: u8, subdomains: &[&str]) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            template: template.into(),
            attribution: attribution.into(),
            max_zoom,
            subdomains: subdomains.iter().map(|s| s.to_string()).collect(),
            ..Self::default()
        }
    }

    /// A new source with an API key and a kind (the Tiles panel's connections).
    pub fn with_key(id: &str, name: &str, template: &str, attribution: &str, max_zoom: u8, api_key: &str, kind: TileKind) -> Self {
        Self { api_key: api_key.into(), kind, ..Self::new(id, name, template, attribution, max_zoom, &[]) }
    }

    /// Does the source look usable (an http(s) URL, and an XYZ template for the kinds that fetch
    /// per-tile from one)?
    pub fn plausible(&self) -> bool {
        if !self.template.starts_with("http") {
            return false;
        }
        match self.kind {
            TileKind::Raster | TileKind::Vector => self.template.contains("{z}") && self.template.contains("{x}") && self.template.contains("{y}"),
            // 3D Tiles load a TileSet.json; WMS/WMTS carry their service base URL; the Google
            // kinds name the API.
            _ => true,
        }
    }
}

/// QGIS-style raster styling, applied when a tile decodes (raster) or as it draws (vector). Every
/// slider is 0 by default, so a plain basemap draws (and caches) exactly as the source served it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TileStyle {
    /// −100..=100; +100 lifts every channel by half the range (127.5).
    pub brightness: i16,
    /// −100..=100; scales around mid-grey 128, so −100 flattens to grey and +100 doubles the
    /// distance from it.
    pub contrast: i16,
    /// −100..=100; −100 greys each pixel to its luma, +100 pushes colours twice as far from it.
    pub saturation: i16,
    /// −180..=180 degrees, rotated in YIQ space (the classic NTSC hue rotation).
    pub hue: i16,
}

impl TileStyle {
    /// Nothing would change: tiles can be drawn and cached unstyled.
    pub fn is_neutral(self) -> bool {
        self.brightness == 0 && self.contrast == 0 && self.saturation == 0 && self.hue == 0
    }

    /// Every field back into its range (a hand-typed or scripted value may sit outside).
    pub fn clamped(self) -> Self {
        Self {
            brightness: self.brightness.clamp(-100, 100),
            contrast: self.contrast.clamp(-100, 100),
            saturation: self.saturation.clamp(-100, 100),
            hue: self.hue.clamp(-180, 180),
        }
    }

    /// A stable, cheap signature of the four fields (FNV-1a over their little-endian bytes): the
    /// tile cache keys styled tiles by it, so restyling orphans the old copies without a version
    /// counter or building strings.
    pub fn stamp(self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for v in [self.brightness, self.contrast, self.saturation, self.hue] {
            for b in v.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        }
        h
    }

    /// Style one pixel in place. The passes run in display order — brightness, contrast,
    /// saturation, hue — each clamped back into 0..=255, so any combination of extremes stays sane.
    pub fn apply_rgb(&self, r: &mut u8, g: &mut u8, b: &mut u8) {
        let (mut red, mut green, mut blue) = (f64::from(*r), f64::from(*g), f64::from(*b));
        // Brightness: ±100 spans half the channel, keeping both ends reachable but not clipped flat.
        let shift = f64::from(self.brightness) / 100.0 * 127.5;
        if shift != 0.0 {
            red = (red + shift).clamp(0.0, 255.0);
            green = (green + shift).clamp(0.0, 255.0);
            blue = (blue + shift).clamp(0.0, 255.0);
        }
        // Contrast: 128 is mid-grey; ±100 takes the scale factor from 0 (flat grey) to 2.
        let k = 1.0 + f64::from(self.contrast) / 100.0;
        if k != 1.0 {
            red = (128.0 + (red - 128.0) * k).clamp(0.0, 255.0);
            green = (128.0 + (green - 128.0) * k).clamp(0.0, 255.0);
            blue = (128.0 + (blue - 128.0) * k).clamp(0.0, 255.0);
        }
        // Saturation: lerp toward the Rec.601 luma; the factor tops out at 2, where colours sit
        // twice as far from their grey as they started.
        let s = (1.0 + f64::from(self.saturation) / 100.0).clamp(0.0, 2.0);
        if s != 1.0 {
            let y = 0.299 * red + 0.587 * green + 0.114 * blue;
            red = (y + (red - y) * s).clamp(0.0, 255.0);
            green = (y + (green - y) * s).clamp(0.0, 255.0);
            blue = (y + (blue - y) * s).clamp(0.0, 255.0);
        }
        // Hue: rotate the chroma in YIQ space. At 0° the NTSC matrices invert each other only to a
        // rounding error, so 0° is skipped — a neutral style must be a pixel-for-pixel identity.
        if self.hue != 0 {
            let (sin, cos) = f64::from(self.hue).to_radians().sin_cos();
            let y = 0.299 * red + 0.587 * green + 0.114 * blue;
            let i = 0.596 * red - 0.274 * green - 0.322 * blue;
            let q = 0.211 * red - 0.523 * green + 0.312 * blue;
            let (i, q) = (i * cos + q * sin, -i * sin + q * cos);
            red = (y + 0.956 * i + 0.621 * q).clamp(0.0, 255.0);
            green = (y - 0.272 * i - 0.647 * q).clamp(0.0, 255.0);
            blue = (y - 1.106 * i + 1.703 * q).clamp(0.0, 255.0);
        }
        *r = red.round() as u8;
        *g = green.round() as u8;
        *b = blue.round() as u8;
    }

    /// Style a whole RGBA8 buffer (stride 4); alpha is left alone.
    pub fn style_rgba(&self, rgba: &mut [u8]) {
        for [r, g, b, _] in rgba.as_chunks_mut::<4>().0 {
            self.apply_rgb(r, g, b);
        }
    }
}

/// One pinned tile: a sub-layer of the map's base layer ("Map"), seen and edited in the Tiles and
/// Layers panels like a QGIS layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TileLayer {
    /// Stable unique id ("tile-1", …), what commands and the panels pick the layer by.
    pub id: String,
    /// The name the panels show; a pinned tile defaults to "z{z}/{x}/{y}".
    pub name: String,
    /// The [`TileSource::id`] the tile came from.
    pub source: String,
    /// The tile: zoom, column and row (see [`TileId`]).
    pub z: u8,
    /// The tile column.
    pub x: u32,
    /// The tile row.
    pub y: u32,
    /// Drawn under the art.
    pub visible: bool,
    /// The panels refuse edits.
    pub locked: bool,
    /// 0..=1.
    pub opacity: f64,
    /// QGIS-style raster styling.
    pub style: TileStyle,
}

impl Default for TileLayer {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            source: String::new(),
            z: 0,
            x: 0,
            y: 0,
            visible: true,
            locked: false,
            opacity: 1.0,
            style: TileStyle::default(),
        }
    }
}

impl TileLayer {
    /// A pinned tile of `source`, named "z{z}/{x}/{y}" unless `name` gives it one.
    pub fn new(id: String, source: String, z: u8, x: u32, y: u32, name: Option<&str>) -> Self {
        Self {
            id,
            name: name.filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("z{z}/{x}/{y}")),
            source,
            z,
            x,
            y,
            ..Self::default()
        }
    }
}

/// The document's map setup: whether tiles show, which source is active and every source it may
/// use (the built-ins plus the user's own), the basemap's own style and opacity, and the tiles
/// pinned as sub-layers of the base layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MapSettings {
    /// Tiles drawn under the art.
    pub visible: bool,
    /// The active source's [`TileSource::id`].
    pub active: String,
    /// Every source this document may use: the built-ins plus the user's own.
    pub sources: Vec<TileSource>,
    /// The basemap's own QGIS-style raster style (the Map layer's appearance).
    pub style: TileStyle,
    /// The basemap's draw opacity, 0..=1 (QGIS-style layer opacity for the whole basemap).
    pub opacity: f64,
    /// Tiles pinned as sub-layers of the base layer, in panel order.
    pub tile_layers: Vec<TileLayer>,
}

impl Default for MapSettings {
    fn default() -> Self {
        Self { visible: true, active: "osm".into(), sources: builtin_sources(), style: TileStyle::default(), opacity: 1.0, tile_layers: Vec::new() }
    }
}

impl MapSettings {
    /// The active source, when it exists.
    pub fn source(&self) -> Option<&TileSource> {
        self.sources.iter().find(|s| s.id == self.active).or_else(|| self.sources.first())
    }

    /// Make `id` the active source (a no-op when there is no such source) → did it change?
    pub fn set_active(&mut self, id: &str) -> bool {
        if self.sources.iter().any(|s| s.id == id) && self.active != id {
            self.active = id.to_string();
            true
        } else {
            false
        }
    }

    /// Add (or replace, by id) a source → its id.
    pub fn upsert_source(&mut self, source: TileSource) -> String {
        let id = source.id.clone();
        match self.sources.iter_mut().find(|s| s.id == id) {
            Some(s) => *s = source,
            None => self.sources.push(source),
        }
        id
    }

    /// Remove the source `id` (built-ins can't go) → did it go?
    pub fn remove_source(&mut self, id: &str) -> bool {
        if builtin_id(id) {
            return false;
        }
        let gone = self.sources.iter().any(|s| s.id == id);
        self.sources.retain(|s| s.id != id);
        if gone && self.active == id {
            self.active = self.sources.first().map(|s| s.id.clone()).unwrap_or_else(|| "osm".into());
        }
        gone
    }

    /// The pinned tile layer `id`, when it is there.
    pub fn tile_layer(&self, id: &str) -> Option<&TileLayer> {
        self.tile_layers.iter().find(|t| t.id == id)
    }

    /// The pinned tile layer `id`, mutable.
    pub fn tile_layer_mut(&mut self, id: &str) -> Option<&mut TileLayer> {
        self.tile_layers.iter_mut().find(|t| t.id == id)
    }

    /// Is `(z, x, y)` a real tile on the world square (see [`TileId::valid`])?
    pub fn valid_tile(z: u8, x: u32, y: u32) -> bool {
        TileId::new(z, x, y).valid()
    }

    /// The next free pinned-tile id: "tile-N", N one past the highest "tile-N" already there.
    /// A hand-edited file may carry gaps or non-numeric ids — those parse as nothing.
    pub fn next_tile_layer_id(&self) -> String {
        let top = self.tile_layers.iter().filter_map(|t| t.id.strip_prefix("tile-")).filter_map(|n| n.parse::<u64>().ok()).max().unwrap_or(0);
        format!("tile-{}", top + 1)
    }
}

/// The built-in tile sources: OSM and CARTO, Esri, OpenTopoMap, Google and Mapbox (the Mapbox
/// ones need the app's Mapbox access token, put in the templates' `{key}`).
pub fn builtin_sources() -> Vec<TileSource> {
    const MAPBOX_TOKEN: &str = "pk.eyJ1IjoiYm9sdHVzZXIiLCJhIjoiY21kZDBzbmE4MDM4czJqcXV1OHo1YzY2ZSJ9.922Yb_-Fs0xY48TZ0BwJ0Q";
    vec![
        TileSource::new("osm", "OpenStreetMap", "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "© OpenStreetMap contributors", 19, &[]),
        TileSource::new(
            "esri-light",
            "Light (Esri)",
            "https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/World_Light_Gray_Base/MapServer/tile/{z}/{y}/{x}",
            "© Esri, Maxar, Earthstar Geographics",
            16,
            &[],
        ),
        TileSource::new(
            "esri-dark",
            "Dark (Esri)",
            "https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/World_Dark_Gray_Base/MapServer/tile/{z}/{y}/{x}",
            "© Esri, Maxar, Earthstar Geographics",
            16,
            &[],
        ),
        TileSource::new(
            "esri-satellite",
            "Satellite (Esri)",
            "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
            "Imagery © Esri, Maxar, Earthstar Geographics",
            19,
            &[],
        ),
        TileSource::new(
            "opentopomap",
            "Topographic (OpenTopoMap)",
            "https://{s}.tile.opentopomap.org/{z}/{x}/{y}.png",
            "© OpenStreetMap contributors, SRTM | style: © OpenTopoMap (CC-BY-SA)",
            17,
            &["a", "b", "c"],
        ),
        // Google's XYZ endpoints (the same tiles the web map serves; community-documented).
        TileSource::new("google-satellite", "Google Satellite", "https://mt1.google.com/vt/lyrs=s&x={x}&y={y}&z={z}", "Imagery © Google", 20, &[]),
        TileSource::new("google-hybrid", "Google Hybrid", "https://mt1.google.com/vt/lyrs=y&x={x}&y={y}&z={z}", "Imagery © Google", 20, &[]),
        TileSource::new("google-streets", "Google Streets", "https://mt1.google.com/vt/lyrs=m&x={x}&y={y}&z={z}", "© Google", 20, &[]),
        TileSource::new("google-terrain", "Google Terrain", "https://mt1.google.com/vt/lyrs=p&x={x}&y={y}&z={z}", "© Google", 20, &[]),
        // Mapbox Styles API raster tiles (the styles render server-side) — `{key}` is the token.
        TileSource::with_key(
            "mapbox-streets",
            "Mapbox Streets",
            "https://api.mapbox.com/styles/v1/mapbox/streets-v12/tiles/{z}/{x}/{y}?access_token={key}",
            "© Mapbox © OpenStreetMap",
            22,
            MAPBOX_TOKEN,
            TileKind::Raster,
        ),
        TileSource::with_key(
            "mapbox-satellite",
            "Mapbox Satellite",
            "https://api.mapbox.com/styles/v1/mapbox/satellite-v9/tiles/{z}/{x}/{y}?access_token={key}",
            "© Mapbox © Maxar",
            22,
            MAPBOX_TOKEN,
            TileKind::Raster,
        ),
        TileSource::with_key(
            "mapbox-dark",
            "Mapbox Dark",
            "https://api.mapbox.com/styles/v1/mapbox/dark-v11/tiles/{z}/{x}/{y}?access_token={key}",
            "© Mapbox © OpenStreetMap",
            22,
            MAPBOX_TOKEN,
            TileKind::Raster,
        ),
        // Mapbox's vector tileset: drawn as linework by the built-in MVT decoder.
        TileSource::with_key(
            "mapbox-vector",
            "Mapbox Vector (Streets v8)",
            "https://api.mapbox.com/v4/mapbox.mapbox-streets-v8/{z}/{x}/{y}.mvt?access_token={key}",
            "© Mapbox © OpenStreetMap",
            14,
            MAPBOX_TOKEN,
            TileKind::Vector,
        ),
        // Esri's imagery service endpoint (the same World_Imagery tileset, served for apps).
        TileSource::new(
            "esri-world-imagery",
            "Esri World Imagery",
            "https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
            "Esri, Maxar, Earthstar Geographics",
            19,
            &[],
        ),
        // Google Map Tiles (BYOK): the host runs the session handshake with the user's key, then
        // tiles build from the session + key pair.
        TileSource::with_key(
            "google-map-tiles",
            "Google Map Tiles (2D, key)",
            "https://tile.googleapis.com/v1/2dtiles/{z}/{x}/{y}",
            "Imagery © Google",
            22,
            "",
            TileKind::GoogleSession,
        ),
        // Google Photorealistic 3D Tiles (BYOK): the template is the TileSet.json the host loads;
        // imagery falls back to the 2D session tiles until the 3D viewport exists.
        TileSource::with_key(
            "google-3d-tiles",
            "Google Photorealistic 3D Tiles (key)",
            "https://tile.googleapis.com/v1/3dtiles/root.json",
            "Imagery © Google",
            MAX_ZOOM,
            "",
            TileKind::Google3dTiles,
        ),
        // NOAA nowCOAST: a plain WMS whose GetMap URLs build from the tile's Mercator bbox.
        TileSource {
            kind: TileKind::Wms,
            layers: "conus_base_reflectivity".into(),
            format: "image/png".into(),
            ..TileSource::new(
                "noaa-nowcoast-radar",
                "NOAA nowCOAST radar",
                "https://nowcoast.noaa.gov/geoserver/observations/weather_radar/ows",
                "NOAA nowCOAST",
                12,
                &[],
            )
        },
    ]
}

/// The built-in source ids (built-ins can't be removed by the user).
pub fn builtin_id(id: &str) -> bool {
    matches!(
        id,
        "osm"
            | "esri-light"
            | "esri-dark"
            | "esri-satellite"
            | "opentopomap"
            | "google-satellite"
            | "google-hybrid"
            | "google-streets"
            | "google-terrain"
            | "mapbox-streets"
            | "mapbox-satellite"
            | "mapbox-dark"
            | "mapbox-vector"
            | "esri-world-imagery"
            | "google-map-tiles"
            | "google-3d-tiles"
            | "noaa-nowcoast-radar"
    )
}

/// The URL the host fetches for tile `(z, x, y)` of `source`: XYZ template substitution for
/// raster and vector tiles (see [`TileId::url`]), a WMS GetMap whose BBOX is the tile's
/// Web-Mercator bbox for WMS, a KVP GetTile (or REST template fill) for WMTS, and the Google 2D
/// session URL for `GoogleSession`. A `Google3dTiles` source has no per-tile URL — its template
/// names the TileSet.json the host loads — so the template's placeholders just substitute.
pub fn build_tile_url(source: &TileSource, z: u32, x: u32, y: u32) -> String {
    match source.kind {
        TileKind::Raster | TileKind::Vector | TileKind::Google3dTiles => TileId::new(z.min(u8::MAX as u32) as u8, x, y).url(source),
        TileKind::Wms => wms::tile_url(
            &source.template,
            &WmsParams {
                layers: source.layers.clone(),
                styles_param: source.style_name.clone(),
                format: if source.format.is_empty() { "image/png".into() } else { source.format.clone() },
                ..WmsParams::default()
            },
            z,
            x,
            y,
        ),
        TileKind::Wmts => wmts::tile_url(
            &source.template,
            &WmtsLayer {
                layer: source.layers.clone(),
                style: source.style_name.clone(),
                matrix_set: source.matrix_set.clone(),
                format: if source.format.is_empty() { "image/png".into() } else { source.format.clone() },
            },
            z,
            x,
            y,
        ),
        TileKind::GoogleSession => google::tile_url(&source.session, &source.api_key, z, x, y),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn world_layout() {
        assert_eq!(WORLD_PT, 4096.0);
        // London against the standard slippy-map math (z4: tile 7, row 5; a tile pixel is a point).
        let (x, y) = lonlat_to_doc(-0.1276, 51.5072);
        assert!(close(x, 2046.548), "{x}");
        assert!(close(y, 1362.028), "{y}");
        // Round trip.
        let (lon, lat) = doc_to_lonlat(x, y);
        assert!(close(lon, -0.1276) && close(lat, 51.5072), "{lon} {lat}");
        // The corners: west antimeridian + north limit, east antimeridian + south limit.
        let (x0, y0) = lonlat_to_doc(-180.0, LAT_LIMIT);
        assert!(close(x0, 0.0) && close(y0, 0.0), "{x0} {y0}");
        let (x1, y1) = lonlat_to_doc(180.0, -LAT_LIMIT);
        assert!(close(x1, WORLD_PT) && close(y1, WORLD_PT), "{x1} {y1}");
        // Out-of-range latitude clamps, not runs away (a hair below 0 is the Mercator limit's
        // last-digit rounding).
        let (_, y) = lonlat_to_doc(0.0, 89.9);
        assert!((-1e-9..WORLD_PT).contains(&y), "{y}");
        // Off-world points read back at the latitude limits (y grows south, so above the world is
        // the north limit).
        assert!(doc_to_lonlat(WORLD_PT * 1.5, -WORLD_PT).1 >= LAT_LIMIT - 1e-9);
        assert!(doc_to_lonlat(WORLD_PT * 1.5, WORLD_PT * 2.0).1 <= -LAT_LIMIT + 1e-9);
    }

    #[test]
    fn tile_placement() {
        let t = TileId::new(4, 8, 8);
        assert!(close(t.edge_pt(), 256.0));
        assert_eq!(t.rect_pt(), (2048.0, 2048.0, 2304.0, 2304.0));
        assert!(!TileId::new(2, 4, 0).valid());
        assert!(TileId::new(19, (1 << 19) - 1, (1 << 19) - 1).valid());
        // The equator/prime-meridian tile at any zoom starts at the world's middle.
        let mid = 1u32 << 2; // z3: 8 tiles, the middle one is 4
        let (x0, y0, _, _) = TileId::new(3, mid, mid).rect_pt();
        assert!(close(x0, WORLD_PT / 2.0) && close(y0, WORLD_PT / 2.0));
    }

    #[test]
    fn view_tiles() {
        // At 1 screen px per point, zoom-4 tiles are 256 px on screen.
        let (z, tiles) = tiles_for_view((0.0, 0.0, WORLD_PT, WORLD_PT), 1.0, 19, 64);
        assert_eq!(z, 4);
        assert!(!tiles.is_empty() && tiles.len() <= 64);
        assert!(tiles.iter().all(|t| t.z == 4));
        // Zoomed far in, the tile zoom rises.
        let (z, _) = tiles_for_view((1000.0, 1000.0, 1010.0, 1010.0), 4096.0, 19, 1024);
        assert_eq!(z, 16);
        // A source's cap holds.
        let (z, _) = tiles_for_view((1000.0, 1000.0, 1010.0, 1010.0), 4096.0, 12, 1024);
        assert_eq!(z, 12);
        // Degenerate scale is safe.
        let (z, tiles) = tiles_for_view((0.0, 0.0, WORLD_PT, WORLD_PT), 0.0, 19, 16);
        assert_eq!(z, 0);
        assert!(tiles.len() <= 16);
    }

    #[test]
    fn urls() {
        let osm = TileSource::new("osm", "OpenStreetMap", "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "© OpenStreetMap contributors", 19, &[]);
        assert_eq!(TileId::new(4, 8, 8).url(&osm), "https://tile.openstreetmap.org/4/8/8.png");
        let carto =
            TileSource::new("carto", "CARTO", "https://{s}.basemaps.cartocdn.com/light_all/{z}/{x}/{y}{r}.png", "© CARTO", 20, &["a", "b", "c", "d"]);
        assert_eq!(TileId::new(2, 1, 1).url(&carto), "https://c.basemaps.cartocdn.com/light_all/2/1/1.png");
        // Esri's {y}/{x} order goes through the same substitution.
        let esri = TileSource::new(
            "esri",
            "Esri",
            "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
            "© Esri",
            19,
            &[],
        );
        assert_eq!(TileId::new(3, 5, 2).url(&esri), "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/3/2/5");
    }

    #[test]
    fn settings_sources() {
        let mut m = MapSettings::default();
        assert_eq!(m.active, "osm");
        assert_eq!(m.source().map(|s| s.id.as_str()), Some("osm"));
        assert!(m.set_active("esri-dark"));
        assert!(!m.set_active("nope"));
        assert_eq!(m.source().map(|s| s.id.as_str()), Some("esri-dark"));
        m.upsert_source(TileSource::new("mine", "Mine", "https://tiles.example/{z}/{x}/{y}.png", "© me", 12, &[]));
        m.set_active("mine");
        assert!(m.remove_source("mine"));
        assert!(!m.remove_source("mine"));
        // A built-in can't go away.
        assert!(!m.remove_source("osm"));
        // Removing the active one falls back.
        m.upsert_source(TileSource::new("x", "X", "https://x/{z}/{x}/{y}.png", "© x", 8, &[]));
        m.set_active("x");
        assert!(m.remove_source("x"));
        assert_eq!(m.active, "osm");
        // Serde round-trips.
        let json = serde_json::to_string(&m).unwrap();
        let back: MapSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // An old document without `map` gets the default.
        let old: MapSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old, MapSettings::default());
    }

    #[test]
    fn style_and_tile_layers_round_trip_through_serde() {
        let mut m = MapSettings { style: TileStyle { brightness: 10, contrast: -20, saturation: 100, hue: -90 }, ..MapSettings::default() };
        m.tile_layers = vec![
            TileLayer::new("tile-1".into(), "osm".into(), 4, 8, 8, None),
            TileLayer {
                visible: false,
                locked: true,
                opacity: 0.5,
                style: TileStyle { saturation: -100, ..TileStyle::default() },
                ..TileLayer::new("tile-2".into(), "esri-dark".into(), 3, 2, 5, Some("My Tile"))
            },
        ];
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"tileLayers\"") && json.contains("\"saturation\":-100"), "{json}");
        let back: MapSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // A document saved before tiles could be pinned loads as today: the defaults.
        let old: MapSettings = serde_json::from_str(r#"{"visible":false,"active":"osm","sources":[]}"#).unwrap();
        assert_eq!(old.style, TileStyle::default());
        assert!(old.tile_layers.is_empty());
    }

    #[test]
    fn tile_layer_ids_names_and_validity() {
        let mut m = MapSettings::default();
        assert!(MapSettings::valid_tile(0, 0, 0));
        assert!(MapSettings::valid_tile(MAX_ZOOM, (1 << MAX_ZOOM) - 1, (1 << MAX_ZOOM) - 1));
        assert!(!MapSettings::valid_tile(3, 8, 0), "z3 has 8 tiles per row, so x 8 is off");
        assert!(!MapSettings::valid_tile(MAX_ZOOM + 1, 0, 0));
        assert_eq!(m.next_tile_layer_id(), "tile-1", "an empty list starts at 1");
        let t = TileLayer::new("tile-1".into(), "osm".into(), 4, 8, 8, None);
        assert_eq!(t.name, "z4/8/8", "a pinned tile is named after its coordinates");
        m.tile_layers.push(t);
        m.tile_layers.push(TileLayer::new("tile-9".into(), "osm".into(), 2, 1, 1, Some("")));
        assert_eq!(m.tile_layers[1].name, "z2/1/1", "an empty name falls back to the coordinates");
        assert_eq!(m.tile_layer("tile-1").map(|t| t.z), Some(4));
        assert!(m.tile_layer_mut("nope").is_none());
        // One past the highest numeric suffix, junk ids notwithstanding.
        m.tile_layers.push(TileLayer { id: "weird".into(), ..TileLayer::default() });
        assert_eq!(m.next_tile_layer_id(), "tile-10");
    }

    #[test]
    fn brightness_lifts_and_clamps() {
        let st = TileStyle { brightness: 100, ..TileStyle::default() };
        let (mut r, mut g, mut b) = (10u8, 127u8, 250u8);
        st.apply_rgb(&mut r, &mut g, &mut b);
        assert_eq!((r, g, b), (138, 255, 255), "+127.5 lifts 10 to 138 and pins both whites");
        let st = TileStyle { brightness: -100, ..TileStyle::default() };
        let (mut r, mut g, mut b) = (10u8, 127u8, 250u8);
        st.apply_rgb(&mut r, &mut g, &mut b);
        assert_eq!((r, g, b), (0, 0, 123), "−127.5 pins both blacks");
    }

    #[test]
    fn contrast_scales_around_mid_grey() {
        // 128 stays put; ±100 doubles the distance from it, or flattens it away.
        let st = TileStyle { contrast: 100, ..TileStyle::default() };
        let (mut r, mut g, mut b) = (128u8, 138u8, 20u8);
        st.apply_rgb(&mut r, &mut g, &mut b);
        assert_eq!((r, g, b), (128, 148, 0));
        let st = TileStyle { contrast: -100, ..TileStyle::default() };
        let (mut r, mut g, mut b) = (200u8, 5u8, 128u8);
        st.apply_rgb(&mut r, &mut g, &mut b);
        assert_eq!((r, g, b), (128, 128, 128));
    }

    #[test]
    fn full_negative_saturation_greys_to_luma() {
        let st = TileStyle { saturation: -100, ..TileStyle::default() };
        let (mut r, mut g, mut b) = (255u8, 0u8, 0u8);
        st.apply_rgb(&mut r, &mut g, &mut b);
        assert_eq!((r, g, b), (76, 76, 76), "every channel lands on the Rec.601 luma 0.299·255");
        // Over-saturation pushes the same red twice as far from its luma (out to the rails here).
        let st = TileStyle { saturation: 100, ..TileStyle::default() };
        let (mut r, mut g, mut b) = (200u8, 120u8, 80u8);
        st.apply_rgb(&mut r, &mut g, &mut b);
        assert_eq!((r, g, b), (255, 101, 21));
    }

    #[test]
    fn hue_180_flips_red_toward_cyan() {
        let st = TileStyle { hue: 180, ..TileStyle::default() };
        let (mut r, mut g, mut b) = (255u8, 0u8, 0u8);
        st.apply_rgb(&mut r, &mut g, &mut b);
        assert_eq!((r, g, b), (0, 152, 153), "a YIQ half-turn of pure red");
    }

    #[test]
    fn neutral_style_is_identity() {
        assert!(TileStyle::default().is_neutral());
        assert!(!TileStyle { hue: 1, ..TileStyle::default() }.is_neutral());
        let st = TileStyle::default();
        let mut px = vec![0, 1, 254, 255, 128, 64, 32, 16, 255, 255, 0, 0];
        let want = px.clone();
        st.style_rgba(&mut px);
        assert_eq!(px, want, "alpha included: 9 of every 12 bytes untouched");
        // An out-of-range style clamps back into range before it does anything.
        assert_eq!(
            TileStyle { brightness: 900, contrast: -900, saturation: 900, hue: 900 }.clamped(),
            TileStyle { brightness: 100, contrast: -100, saturation: 100, hue: 180 }
        );
    }

    #[test]
    fn stamp_identifies_the_style() {
        let a = TileStyle { brightness: 10, contrast: -20, saturation: 100, hue: -90 };
        assert_eq!(a.stamp(), a.stamp(), "stable across calls");
        assert_eq!(a.clamped().stamp(), a.stamp(), "clamping already-in-range fields changes nothing");
        for st in
            [TileStyle { brightness: 11, ..a }, TileStyle { contrast: -19, ..a }, TileStyle { saturation: 99, ..a }, TileStyle { hue: -89, ..a }]
        {
            assert_ne!(st.stamp(), a.stamp(), "{st:?} must not share the default style's stamp");
        }
        assert_eq!(TileStyle::default().stamp(), TileStyle::default().clamped().stamp());
    }

    #[test]
    fn new_kinds_round_trip_and_old_files_load() {
        // A 0.10.2-era TileSource carries none of the new fields: it loads unchanged, raster kind.
        let old: TileSource = serde_json::from_str(
            r#"{"id":"osm","name":"OpenStreetMap","template":"https://tile.openstreetmap.org/{z}/{x}/{y}.png","attribution":"© OpenStreetMap contributors","minZoom":0,"maxZoom":19,"subdomains":[]}"#,
        )
        .unwrap();
        assert_eq!(old.kind, TileKind::Raster);
        assert!(old.layers.is_empty() && old.matrix_set.is_empty() && old.style_name.is_empty() && old.format.is_empty() && old.session.is_empty());
        assert_eq!(
            old,
            TileSource::new("osm", "OpenStreetMap", "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "© OpenStreetMap contributors", 19, &[])
        );
        // A Google session source survives a round trip through the new fields, camelCase stable.
        let g = TileSource {
            session: "s-abc123".into(),
            api_key: "k".into(),
            kind: TileKind::GoogleSession,
            ..TileSource::new(
                "google-map-tiles",
                "Google Map Tiles (2D, key)",
                "https://tile.googleapis.com/v1/2dtiles/{z}/{x}/{y}",
                "Imagery © Google",
                22,
                &[],
            )
        };
        let json = serde_json::to_string(&g).unwrap();
        assert!(json.contains("\"kind\":\"googleSession\"") && json.contains("\"session\":\"s-abc123\""), "{json}");
        assert_eq!(serde_json::from_str::<TileSource>(&json).unwrap(), g);
        // The other new kinds serialize under the names the docs will carry.
        assert_eq!(serde_json::to_string(&TileKind::Wms).unwrap(), "\"wms\"");
        assert_eq!(serde_json::to_string(&TileKind::Wmts).unwrap(), "\"wmts\"");
        assert_eq!(serde_json::to_string(&TileKind::Google3dTiles).unwrap(), "\"google3dTiles\"");
        // An old MapSettings with a bare source list still loads (the container defaults hold).
        let old_map: MapSettings = serde_json::from_str(r#"{"visible":false,"active":"osm","sources":[{"id":"osm","name":"OpenStreetMap","template":"https://tile.openstreetmap.org/{z}/{x}/{y}.png","attribution":"© OpenStreetMap contributors","maxZoom":19}]}"#).unwrap();
        assert_eq!(old_map.sources[0].kind, TileKind::Raster);
        assert_eq!(old_map.sources[0].session, "");
    }

    #[test]
    fn build_tile_url_dispatches_by_kind() {
        // Raster/Vector: the XYZ template substitution TileId::url does.
        let osm = TileSource::new("osm", "OpenStreetMap", "https://tile.openstreetmap.org/{z}/{x}/{y}.png", "© OpenStreetMap contributors", 19, &[]);
        assert_eq!(build_tile_url(&osm, 3, 5, 2), "https://tile.openstreetmap.org/3/5/2.png");
        let mvt = TileSource::with_key(
            "mvt",
            "MVT",
            "https://api.mapbox.com/v4/t/{z}/{x}/{y}.mvt?access_token={key}",
            "© Mapbox",
            14,
            "TOK",
            TileKind::Vector,
        );
        assert_eq!(build_tile_url(&mvt, 1, 0, 1), "https://api.mapbox.com/v4/t/1/0/1.mvt?access_token=TOK");
        // WMS: the GetMap URL carries the tile's hand-computable Mercator bbox (z1/x1/y0 is the
        // north-east quarter of the projected world).
        let wms_src = TileSource {
            kind: TileKind::Wms,
            layers: "conus_base_reflectivity".into(),
            format: "image/png".into(),
            ..TileSource::new(
                "noaa",
                "NOAA nowCOAST radar",
                "https://nowcoast.noaa.gov/geoserver/observations/weather_radar/ows",
                "NOAA nowCOAST",
                12,
                &[],
            )
        };
        let url = build_tile_url(&wms_src, 1, 1, 0);
        let half = WORLD_METERS / 2.0;
        assert_eq!(
            url,
            format!(
                "https://nowcoast.noaa.gov/geoserver/observations/weather_radar/ows?SERVICE=WMS&VERSION=1.3.1&REQUEST=GetMap\
                 &LAYERS=conus_base_reflectivity&STYLES=&CRS=EPSG:3857&BBOX=0,0,{half},{half}&WIDTH=256&HEIGHT=256&FORMAT=image/png&TRANSPARENT=FALSE"
            )
        );
        // WMTS: KVP GetTile from the source's layer fields.
        let wmts_src = TileSource {
            kind: TileKind::Wmts,
            layers: "world".into(),
            style_name: "default".into(),
            matrix_set: "GoogleMapsCompatible".into(),
            format: "image/jpeg".into(),
            ..TileSource::new("wmts", "WMTS", "https://example.com/wmts", "© t", 12, &[])
        };
        assert_eq!(
            build_tile_url(&wmts_src, 7, 3, 5),
            "https://example.com/wmts?SERVICE=WMTS&VERSION=1.0.0&REQUEST=GetTile&LAYER=world&STYLE=default\
             &FORMAT=image/jpeg&TILEMATRIXSET=GoogleMapsCompatible&TILEMATRIX=7&TILEROW=5&TILECOL=3"
        );
        // A WMTS REST template fills in place, case-insensitively.
        let rest = TileSource {
            kind: TileKind::Wmts,
            layers: "w".into(),
            style_name: "d".into(),
            matrix_set: "g".into(),
            format: "image/png".into(),
            ..TileSource::new(
                "wmts-rest",
                "WMTS REST",
                "https://m.example.com/{Layer}/{Style}/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.png",
                "© t",
                12,
                &[],
            )
        };
        assert_eq!(build_tile_url(&rest, 4, 2, 1), "https://m.example.com/w/d/g/4/1/2.png");
        // Google session: the 2D tile URL from the session token + key.
        let goog = TileSource {
            kind: TileKind::GoogleSession,
            session: "S1".into(),
            api_key: "K1".into(),
            ..TileSource::new(
                "google-map-tiles",
                "Google Map Tiles (2D, key)",
                "https://tile.googleapis.com/v1/2dtiles/{z}/{x}/{y}",
                "Imagery © Google",
                22,
                &[],
            )
        };
        assert_eq!(build_tile_url(&goog, 4, 2, 3), "https://tile.googleapis.com/v1/2dtiles/4/2/3?session=S1&key=K1");
        // Google 3D Tiles: the template is the TileSet.json — no per-tile placeholders, no change.
        let t3d = TileSource {
            kind: TileKind::Google3dTiles,
            api_key: "K2".into(),
            ..TileSource::new(
                "google-3d-tiles",
                "Google Photorealistic 3D Tiles (key)",
                "https://tile.googleapis.com/v1/3dtiles/root.json",
                "Imagery © Google",
                MAX_ZOOM,
                &[],
            )
        };
        assert_eq!(build_tile_url(&t3d, 9, 4, 5), "https://tile.googleapis.com/v1/3dtiles/root.json");
    }

    #[test]
    fn presets_cover_the_new_kinds() {
        let sources = builtin_sources();
        let by_id = |id: &str| sources.iter().find(|s| s.id == id).unwrap_or_else(|| panic!("no preset {id}"));
        let imagery = by_id("esri-world-imagery");
        assert_eq!(imagery.max_zoom, 19);
        assert_eq!(imagery.attribution, "Esri, Maxar, Earthstar Geographics");
        assert!(imagery.plausible());
        assert_eq!(by_id("google-map-tiles").kind, TileKind::GoogleSession);
        assert_eq!(by_id("google-3d-tiles").template, "https://tile.googleapis.com/v1/3dtiles/root.json");
        let noaa = by_id("noaa-nowcoast-radar");
        assert_eq!(noaa.kind, TileKind::Wms);
        assert_eq!(noaa.layers, "conus_base_reflectivity");
        assert_eq!(noaa.format, "image/png");
        assert_eq!(noaa.max_zoom, 12);
        assert_eq!(noaa.attribution, "NOAA nowCOAST");
        assert!(noaa.plausible());
        // Every new preset is a built-in (can't be removed by the user).
        for id in ["esri-world-imagery", "google-map-tiles", "google-3d-tiles", "noaa-nowcoast-radar"] {
            assert!(builtin_id(id));
        }
    }
}
