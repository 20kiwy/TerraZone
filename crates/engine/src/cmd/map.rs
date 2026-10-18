//! The Map menu and Tile Sources dialog: the tile basemap a TerraZone document draws, and the
//! tiles pinned as sub-layers of its base layer. A TerraZone document *is* a map
//! ([`terrazon_map::MapSettings`], saved with the file), so these commands edit the document like
//! any other (one undo step each).

use serde_json::{Value, json};
use terrazon_color::{Color, Paint};
use terrazon_doc::{Appearance, Document, Node, NodeKind};
use terrazon_geom::{BezPath, FillRule, PathData};
use terrazon_map::{MAX_ZOOM, MapSettings, TileLayer, TileSource, TileStyle};

use super::layer::make_layer;
use super::*;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "map.setSource",
            "Tile Source",
            ["Map"],
            None,
            "{id: a source id (map.sources)} make that tile source the one the map draws; the built-ins: \"osm\", \"esri-light\", \"esri-dark\", \"esri-satellite\", \"opentopomap\", \"google-satellite\", \"google-hybrid\", \"google-streets\", \"google-terrain\", \"mapbox-streets\", \"mapbox-satellite\", \"mapbox-dark\", \"mapbox-vector\"",
            has_doc,
            set_source
        ),
        cmd!(
            "map.showTiles",
            "Show Tiles",
            ["Map"],
            None,
            "{on?: true|false (left out: toggle)} draw the map's tile basemap under the art (the Map menu's Show Tiles)",
            has_doc,
            show_tiles
        ),
        cmd!(
            "map.addSource",
            "Make Tile Source",
            ["Map"],
            None,
            "{name, template: an XYZ URL template with {z}, {x} and {y} (https; optional {s} for subdomains, {r} ignored, {key} where the API key goes; a WMS/WMTS service base URL or a Google TileSet.json URL for those kinds), attribution?: the line the map shows for it, minZoom?: 0–22 (default 0), maxZoom?: 0–22 (default 22), apiKey?: the key substituted into {key} (a Mapbox access token and friends), kind?: \"raster\" (default), \"vector\" (Mapbox Vector Tiles, drawn as linework), \"wms\", \"wmts\", \"googleSession\" (Google Map Tiles 2D, needs apiKey) or \"google3dTiles\", layers?: the WMS layers (comma-separated) or the WMTS layer, matrixSet?: the WMTS tile matrix set, styleName?: the WMS STYLES / WMTS style, format?: the image format (default image/png), subdomains?: [\"a\", \"b\", …] what {s} cycles through, id?: replace that source instead of adding} add a tile source connection to the document (Map › Tile Sources…, the Tiles panel) → {id}",
            has_doc,
            add_source
        ),
        cmd!(
            "map.removeSource",
            "Remove Tile Source",
            ["Map"],
            None,
            "{id} remove a user-added tile source from the document (the built-ins stay; the active one falls back to the first left)",
            has_doc,
            remove_source
        ),
        cmd!(
            "map.style",
            "Map Style",
            ["Map"],
            None,
            "{opacity?: 0–1, brightness?, contrast?, saturation?: −100–100, hue?: −180–180 degrees (each left out: keep the current one; none at all: just ask)} style the map's tile basemap, QGIS-style: the sliders apply to every tile as it draws → {opacity, brightness, contrast, saturation, hue}",
            has_doc,
            map_style
        ),
        cmd!(
            query "map.sources",
            "Tile Sources",
            [],
            None,
            "{} → {active, visible, sources: [{id, name, template, attribution, minZoom, maxZoom, subdomains, apiKey, kind, builtin}]}: the document's tile sources",
            has_doc,
            list_sources
        ),
        cmd!(
            query "map.tileLayers",
            "Tile Layers",
            [],
            None,
            "{} → {tiles: [{id, name, source, z, x, y, visible, locked, opacity, style: {brightness, contrast, saturation, hue}}]}: the tiles pinned as sub-layers of the map's base \"Map\" layer",
            has_doc,
            list_tile_layers
        ),
        cmd!(
            "map.addTileLayers",
            "Pin Tiles",
            [],
            None,
            "{tiles: [{source?: a source id (map.sources; left out or unknown: the active one), z, x, y: the tile, on the world square, name?: what the panels show (default \"z{z}/{x}/{y}\")}]} pin tiles as sub-layers of the map's base layer → {ids: the new tile layers' ids}; a tile already pinned (same source and z/x/y) is skipped, and at most 128 pins per call and 256 pinned in total (extras drop)",
            has_doc,
            add_tile_layers
        ),
        cmd!(
            "map.updateTileLayer",
            "Tile Layer Options",
            [],
            None,
            "{id, name?, source? (a source id that exists), z?, x?, y? (the new position must be on the world square), visible?, locked?, opacity?: 0–1, style?: {brightness?, contrast?, saturation?, hue?}} edit one pinned tile layer's data, like QGIS's layer properties → the updated tile layer (as map.tileLayers lists it)",
            has_doc,
            update_tile_layer
        ),
        cmd!(
            "map.removeTileLayers",
            "Remove Tile Layers",
            [],
            None,
            "{ids: [tile layer ids]} unpin the named tile layers (unknown ids are ignored) → {removed: how many went}",
            has_doc,
            remove_tile_layers
        ),
        cmd!(
            "map.sourceSession",
            "Tile Source Session",
            ["Map"],
            None,
            "{id, session} store a tile source's session token (the Google Map Tiles createSession answer the host's handshake fetched for `apiKey`; tile URLs then build from the pair) — the app does this by itself after the handshake → ok",
            has_doc,
            source_session
        ),
        cmd!(
            "map.importVectorData",
            "Import Vector Data",
            ["Map"],
            None,
            "{name, paths: [{d: SVG path data in document points (M/L/C/Z), closed?: bool (default false; closed paths fill EvenOdd, so rings read as outlines and holes), label?: the node's name}]} import GIS vector data as ONE new layer named `name` (a dataset = a layer, QGIS-style), placed above the current layer like layer.new makes one, filled with one path node per entry (compound paths keep their subpaths; nodes default to no fill and a 1 pt black stroke, like imported geometry) → {id: the new layer's id, nodes: how many path nodes were made}; an empty `paths` (or nothing drawable in it) is an error",
            has_doc,
            import_vector_data
        ),
    ]
}

fn map_of(d: &mut Document) -> &mut MapSettings {
    &mut d.map
}

fn set_source(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.setSource";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "give the source's id (see map.sources)"))?;
    let mut changed = false;
    s.edit("Tile Source", |d, _| {
        changed = map_of(d).set_active(id);
        if !changed && map_of(d).source().map(|s| s.id.as_str()) != Some(id) {
            return Err(bad(C, format!("no tile source called `{id}` (see map.sources)")));
        }
        Ok(())
    })?;
    ok()
}

fn show_tiles(s: &mut Session, p: &Value) -> Result<Value> {
    let mut on = false;
    s.edit("Show Tiles", |d, _| {
        let m = map_of(d);
        on = bool_or(p, "on", !m.visible);
        m.visible = on;
        Ok(())
    })?;
    Ok(json!({ "visible": on }))
}

/// A source id from a name: letters, digits and dashes, unique against the sources already there.
fn slug(name: &str, taken: &[String]) -> String {
    let mut base: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    base = base.trim_matches('-').to_string();
    if base.is_empty() {
        base = "source".into();
    }
    let mut id = base.clone();
    let mut n = 2;
    while taken.iter().any(|t| t == &id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

fn add_source(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.addSource";
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "give the source a name"))?.to_string();
    let template = str_param(p, "template")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| bad(C, "give the source's URL template (an XYZ template with {z}, {x} and {y})"))?
        .to_string();
    let max_zoom = p.get("maxZoom").and_then(Value::as_u64).unwrap_or(MAX_ZOOM as u64).clamp(0, MAX_ZOOM as u64) as u8;
    let min_zoom = p.get("minZoom").and_then(Value::as_u64).unwrap_or(0).clamp(0, MAX_ZOOM as u64) as u8;
    let api_key = str_param(p, "apiKey").map(str::trim).unwrap_or("").to_string();
    let kind = match str_param(p, "kind").map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("vector" | "mvt") => terrazon_map::TileKind::Vector,
        Some("wms") => terrazon_map::TileKind::Wms,
        Some("wmts") => terrazon_map::TileKind::Wmts,
        Some("googlesession" | "google-session") => terrazon_map::TileKind::GoogleSession,
        Some("google3dtiles" | "google-3d-tiles") => terrazon_map::TileKind::Google3dTiles,
        _ => terrazon_map::TileKind::Raster,
    };
    let subdomains: Vec<String> = p
        .get("subdomains")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(26).collect())
        .unwrap_or_default();
    let sub: Vec<&str> = subdomains.iter().map(String::as_str).collect();
    let mut source = TileSource::new("", &name, &template, str_param(p, "attribution").unwrap_or(""), max_zoom, &sub);
    source.min_zoom = min_zoom;
    source.api_key = api_key;
    source.kind = kind;
    source.layers = str_param(p, "layers").unwrap_or_default().to_string();
    source.matrix_set = str_param(p, "matrixSet").unwrap_or_default().to_string();
    source.style_name = str_param(p, "styleName").unwrap_or_default().to_string();
    source.format = str_param(p, "format").unwrap_or_default().to_string();
    if !source.plausible() {
        return Err(bad(C, "the template must be an https URL with {z}, {x} and {y} in it, like https://tile.example.org/{z}/{x}/{y}.png"));
    }
    let mut id = String::new();
    s.edit("Make Tile Source", |d, _| {
        let m = map_of(d);
        let id2 = match str_param(p, "id") {
            // Replace that source (same id): its settings update in place.
            Some(existing) if m.sources.iter().any(|s| s.id == existing && !terrazon_map::builtin_id(existing)) => existing.to_string(),
            _ => slug(&name, &m.sources.iter().map(|s| s.id.clone()).collect::<Vec<_>>()),
        };
        let src = TileSource { id: id2.clone(), ..source.clone() };
        id = id2;
        let made = m.upsert_source(src);
        m.set_active(&made);
        Ok(())
    })?;
    Ok(json!({ "id": id }))
}

fn remove_source(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.removeSource";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "give the source's id (see map.sources)"))?;
    let mut gone = false;
    s.edit("Remove Tile Source", |d, _| {
        gone = map_of(d).remove_source(id);
        if !gone {
            return Err(bad(C, format!("no removable tile source called `{id}` (the built-ins stay)")));
        }
        Ok(())
    })?;
    ok()
}

/// One style slider off the params, rounded to a whole step and clamped into its range (a script
/// may send 300 or −5.7; the sliders stop at ±100/±180).
fn style_field(p: &Value, key: &str, lo: i16, hi: i16) -> Option<i16> {
    p.get(key)?.as_f64().map(|v| v.round().clamp(f64::from(lo), f64::from(hi)) as i16)
}

/// The params' style fields over `base`: only the keys present change, each clamped into range.
fn style_over(p: &Value, base: TileStyle) -> TileStyle {
    let mut st = base;
    if let Some(v) = style_field(p, "brightness", -100, 100) {
        st.brightness = v;
    }
    if let Some(v) = style_field(p, "contrast", -100, 100) {
        st.contrast = v;
    }
    if let Some(v) = style_field(p, "saturation", -100, 100) {
        st.saturation = v;
    }
    if let Some(v) = style_field(p, "hue", -180, 180) {
        st.hue = v;
    }
    st
}

/// A style as the commands return it.
fn style_json(st: TileStyle) -> Value {
    json!({ "brightness": st.brightness, "contrast": st.contrast, "saturation": st.saturation, "hue": st.hue })
}

/// The basemap's style and opacity as `map.style` returns them.
fn style_json_full(m: &MapSettings) -> Value {
    let mut v = json!({ "opacity": m.opacity });
    if let (Some(o), Some(s)) = (v.as_object_mut(), style_json(m.style).as_object()) {
        for (k, val) in s {
            o.insert(k.clone(), val.clone());
        }
    }
    v
}

/// A pinned tile as the commands return it.
fn tile_json(t: &TileLayer) -> Value {
    json!({
        "id": t.id, "name": t.name, "source": t.source, "z": t.z, "x": t.x, "y": t.y,
        "visible": t.visible, "locked": t.locked, "opacity": t.opacity, "style": style_json(t.style),
    })
}

/// A tile coordinate off the params: a whole number ≥ 0 (a float that rounds is fine, 4.0 is 4).
fn tile_coord(p: &Value, key: &str, cmd: &str) -> Result<u32> {
    let v = p.get(key).and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("each tile needs a number `{key}`")))?;
    let n = v.round();
    if !v.is_finite() || n < 0.0 || n > f64::from(u32::MAX) {
        return Err(bad(cmd, format!("`{key}` must be a whole number 0–{}", u32::MAX)));
    }
    Ok(n as u32)
}

/// [`tile_coord`] for an update: absent stays absent ("don't move it"), present must be whole.
fn opt_tile_coord(p: &Value, key: &str, cmd: &str) -> Result<Option<u32>> {
    if p.get(key).is_none() {
        return Ok(None);
    }
    tile_coord(p, key, cmd).map(Some)
}

fn map_style(s: &mut Session, p: &Value) -> Result<Value> {
    // No style keys at all: a query — the current style, no undo step.
    if !["opacity", "brightness", "contrast", "saturation", "hue"].iter().any(|k| p.get(*k).is_some()) {
        let m = &s.doc()?.doc.map;
        return Ok(style_json_full(m));
    }
    let mut out = Value::Null;
    s.edit("Map Style", |d, _| {
        let m = map_of(d);
        if let Some(o) = p.get("opacity").and_then(Value::as_f64) {
            m.opacity = if o.is_finite() { o.clamp(0.0, 1.0) } else { 1.0 };
        }
        m.style = style_over(p, m.style);
        out = style_json_full(m);
        Ok(())
    })?;
    Ok(out)
}

fn list_tile_layers(s: &mut Session, _: &Value) -> Result<Value> {
    let st = s.doc()?;
    let tiles: Vec<Value> = st.doc.map.tile_layers.iter().map(tile_json).collect();
    Ok(json!({ "tiles": tiles }))
}

fn add_tile_layers(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.addTileLayers";
    /// Pins one call may make; more drop silently, like `tiles_for_view`'s cap.
    const MAX_PER_CALL: usize = 128;
    /// Pinned tiles one document holds; more drop silently.
    const MAX_TOTAL: usize = 256;

    /// One tile the call wants to pin, validated.
    struct Pin {
        source: Option<String>,
        z: u8,
        x: u32,
        y: u32,
        name: Option<String>,
    }

    let tiles = p.get("tiles").and_then(Value::as_array).ok_or_else(|| bad(C, "give a `tiles` array of {source?, z, x, y, name?}"))?;
    // Validate every tile first: one off the world square fails the whole call before anything pins.
    let mut wanted = Vec::with_capacity(tiles.len());
    for t in tiles {
        let z = tile_coord(t, "z", C)?;
        let x = tile_coord(t, "x", C)?;
        let y = tile_coord(t, "y", C)?;
        if z > u32::from(MAX_ZOOM) || x >= (1u32 << z) || y >= (1u32 << z) {
            return Err(bad(C, format!("tile z{z}/{x}/{y} is off the world square (zoom runs to {MAX_ZOOM})")));
        }
        wanted.push(Pin {
            source: t.get("source").and_then(Value::as_str).map(str::to_string),
            z: z as u8,
            x,
            y,
            name: t.get("name").and_then(Value::as_str).map(str::to_string),
        });
    }
    let mut ids: Vec<Value> = Vec::new();
    s.edit("Pin Tiles", |d, _| {
        let m = map_of(d);
        let active = m.active.clone();
        for w in wanted.iter().take(MAX_PER_CALL) {
            // An unknown (or absent) source id lands the tile on the active source.
            let source = match &w.source {
                Some(id) if m.sources.iter().any(|src| src.id == *id) => id.clone(),
                _ => active.clone(),
            };
            // Already pinned (from an earlier call or earlier in this one): skip, not a duplicate.
            if m.tile_layers.iter().any(|t| t.source == source && t.z == w.z && t.x == w.x && t.y == w.y) {
                continue;
            }
            if m.tile_layers.len() >= MAX_TOTAL {
                break;
            }
            let id = m.next_tile_layer_id();
            // A pin arrives locked: it lives inside the base "Map" layer (the Layers panel keeps
            // it under there, lock on) and is edited from the Tiles panel, QGIS-style. Unlocking
            // stays possible — `map.updateTileLayer` and the panels' lock cells still work.
            m.tile_layers.push(TileLayer { locked: true, ..TileLayer::new(id.clone(), source, w.z, w.x, w.y, w.name.as_deref()) });
            ids.push(id.into());
        }
        Ok(())
    })?;
    Ok(json!({ "ids": ids }))
}

fn update_tile_layer(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.updateTileLayer";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "give the tile layer's `id` (see map.tileLayers)"))?.to_string();
    if let Some(style) = p.get("style")
        && !style.is_object()
    {
        return Err(bad(C, "`style` must be an object of {brightness?, contrast?, saturation?, hue?}"));
    }
    let z = opt_tile_coord(p, "z", C)?;
    let x = opt_tile_coord(p, "x", C)?;
    let y = opt_tile_coord(p, "y", C)?;
    let name = str_param(p, "name").map(str::to_string);
    let source = str_param(p, "source").map(str::to_string);
    let visible = p.get("visible").and_then(Value::as_bool);
    let locked = p.get("locked").and_then(Value::as_bool);
    let opacity = p.get("opacity").and_then(Value::as_f64).filter(|o| o.is_finite()).map(|o| o.clamp(0.0, 1.0));
    let mut updated = Value::Null;
    s.edit("Tile Layer Options", |d, _| {
        let m = map_of(d);
        // The source must exist before anything else moves (an error here undoes the whole edit).
        if let Some(src) = &source
            && !m.sources.iter().any(|src2| src2.id == *src)
        {
            return Err(bad(C, format!("no tile source called `{src}` (see map.sources)")));
        }
        let t = m.tile_layer_mut(&id).ok_or_else(|| bad(C, format!("no tile layer called `{id}` (see map.tileLayers)")))?;
        // The position validates as a whole: a lone new z with the old x/y usually lands off the square.
        if z.is_some() || x.is_some() || y.is_some() {
            let (nz, nx, ny) = (z.unwrap_or(u32::from(t.z)), x.unwrap_or(t.x), y.unwrap_or(t.y));
            if nz > u32::from(MAX_ZOOM) || nx >= (1u32 << nz) || ny >= (1u32 << nz) {
                return Err(bad(C, format!("tile z{nz}/{nx}/{ny} is off the world square (zoom runs to {MAX_ZOOM})")));
            }
            t.z = nz as u8;
            t.x = nx;
            t.y = ny;
        }
        if let Some(n) = &name {
            t.name = n.clone();
        }
        if let Some(src) = source {
            t.source = src;
        }
        if let Some(v) = visible {
            t.visible = v;
        }
        if let Some(l) = locked {
            t.locked = l;
        }
        if let Some(o) = opacity {
            t.opacity = o;
        }
        if let Some(style) = p.get("style") {
            t.style = style_over(style, t.style);
        }
        updated = tile_json(t);
        Ok(())
    })?;
    Ok(updated)
}

fn remove_tile_layers(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.removeTileLayers";
    let ids: Vec<String> = p
        .get("ids")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(C, "give an `ids` array of tile layer ids (see map.tileLayers)"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let mut removed = 0usize;
    s.edit("Remove Tile Layers", |d, _| {
        let m = map_of(d);
        let before = m.tile_layers.len();
        m.tile_layers.retain(|t| !ids.iter().any(|id| id == &t.id));
        removed = before - m.tile_layers.len();
        Ok(())
    })?;
    Ok(json!({ "removed": removed }))
}

/// Store a Google Map Tiles session token on the source `id` (`map.sourceSession`). The host's
/// createSession handshake (a POST the GET-only tile fetcher can't make itself) calls this through
/// the same engine-command path every other map setting goes through, so the document knows its
/// basemap's session like it knows its sources — and it saves with the file.
fn source_session(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.sourceSession";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "give the source's `id` (see map.sources)"))?.to_string();
    let session = str_param(p, "session").unwrap_or_default().to_string();
    s.edit("Tile Source Session", |d, _| {
        let m = map_of(d);
        let src = m.sources.iter_mut().find(|s| s.id == id).ok_or_else(|| bad(C, format!("no tile source called `{id}` (see map.sources)")))?;
        src.session = session;
        Ok(())
    })?;
    ok()
}

/// One imported path: its parsed geometry and how the node names and fills it.
struct Imported {
    path: PathData,
    closed: bool,
    label: Option<String>,
}

/// Import GIS vector data (`map.importVectorData`) as one new layer of path nodes: the paths'
/// `d` strings (SVG path data, document points) parse up front — one bad one fails the whole
/// import before anything is made — then the layer lands where `layer.new` would put it (above
/// the current layer, at its level; the top with nothing current) and becomes current. One undo
/// step for the whole dataset; nodes read like imported geometry: no fill, a 1 pt black stroke.
fn import_vector_data(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.importVectorData";
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "give the new layer a `name`"))?.to_string();
    let paths = p.get("paths").and_then(Value::as_array).ok_or_else(|| bad(C, "give a `paths` array of {d, closed?, label?}"))?;
    if paths.is_empty() {
        return Err(bad(C, "no paths to import: `paths` is empty"));
    }
    let mut wanted: Vec<Imported> = Vec::with_capacity(paths.len());
    for (i, path) in paths.iter().enumerate() {
        let Some(d) = path.get("d").and_then(Value::as_str) else {
            return Err(bad(C, format!("paths[{i}] has no `d` (SVG path data in document points)")));
        };
        let bp = BezPath::from_svg(d).map_err(|e| bad(C, format!("paths[{i}]: not valid path data: {e}")))?;
        let pd = PathData::from_bezpath(&bp);
        // A lone move (one anchor, no segments) draws nothing: skip it like empty data.
        if !pd.subpaths.iter().any(|sp| sp.segment_count() > 0) {
            continue;
        }
        wanted.push(Imported {
            path: pd,
            closed: path.get("closed").and_then(Value::as_bool).unwrap_or(false),
            label: path.get("label").and_then(Value::as_str).map(str::trim).filter(|l| !l.is_empty()).map(str::to_string),
        });
    }
    if wanted.is_empty() {
        return Err(bad(C, "nothing drawable in `paths`: every path parsed to no geometry"));
    }
    // Where the new layer goes (like `layer.new`): above the current layer, at its level.
    let cur = s.doc()?.current_layer();
    let nodes = wanted.len();
    let id = s.edit("Import Vector Data", |d, _| {
        let (parent, index) = match cur.and_then(|c| d.position(c)) {
            Some((parent, i, _)) => (parent, i + 1),
            None => (None, usize::MAX),
        };
        let layer_node = make_layer(d, Some(&name));
        let layer = d.insert(parent, index, layer_node)?;
        for w in wanted {
            let mut n = Node::new(
                d.alloc_id(),
                NodeKind::Path {
                    path: w.path,
                    rule: if w.closed { FillRule::EvenOdd } else { FillRule::NonZero },
                    live: None,
                    clipping: false,
                    guide: false,
                },
            );
            n.appearance = Appearance::basic(Paint::None, Paint::solid(Color::BLACK), 1.0);
            n.name = w.label;
            d.insert(Some(layer), usize::MAX, n)?;
        }
        Ok(layer)
    })?;
    let st = s.doc_mut()?;
    st.active_layer = Some(id);
    st.layer_rows = vec![id];
    Ok(json!({ "id": id.0, "nodes": nodes }))
}

fn list_sources(s: &mut Session, _: &Value) -> Result<Value> {
    let st = s.doc()?;
    let m = &st.doc.map;
    let sources: Vec<Value> = m
        .sources
        .iter()
        .map(|src| {
            json!({
                "id": src.id,
                "name": src.name,
                "template": src.template,
                "attribution": src.attribution,
                "minZoom": src.min_zoom,
                "maxZoom": src.max_zoom,
                "subdomains": src.subdomains,
                "apiKey": src.api_key,
                "kind": match src.kind {
                    terrazon_map::TileKind::Raster => "raster",
                    terrazon_map::TileKind::Vector => "vector",
                    terrazon_map::TileKind::Wms => "wms",
                    terrazon_map::TileKind::Wmts => "wmts",
                    terrazon_map::TileKind::GoogleSession => "googleSession",
                    terrazon_map::TileKind::Google3dTiles => "google3dTiles",
                },
                "builtin": terrazon_map::builtin_id(&src.id),
            })
        })
        .collect();
    Ok(json!({ "active": m.active, "visible": m.visible, "sources": sources }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use crate::Session;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", &json!({ "width": 4096.0, "height": 4096.0 })).unwrap();
        s
    }

    #[test]
    fn source_round_trip() {
        let mut s = session();
        let r = s.execute("map.sources", &json!({})).unwrap();
        assert_eq!(r["active"], "osm");
        assert_eq!(r["visible"], true);
        assert!(r["sources"].as_array().unwrap().len() >= 5);

        s.execute("map.setSource", &json!({ "id": "esri-dark" })).unwrap();
        let r = s.execute("map.sources", &json!({})).unwrap();
        assert_eq!(r["active"], "esri-dark");
        assert!(s.execute("map.setSource", &json!({ "id": "nope" })).is_err());

        s.execute("map.showTiles", &json!({ "on": false })).unwrap();
        assert_eq!(s.execute("map.sources", &json!({})).unwrap()["visible"], false);
        s.execute("map.showTiles", &json!({})).unwrap();
        assert_eq!(s.execute("map.sources", &json!({})).unwrap()["visible"], true);
    }

    #[test]
    fn add_and_remove() {
        let mut s = session();
        let r = s
            .execute(
                "map.addSource",
                &json!({ "name": "My Tiles", "template": "https://tiles.example.org/{z}/{x}/{y}.png", "attribution": "© me", "maxZoom": 12 }),
            )
            .unwrap();
        let id = r["id"].as_str().unwrap().to_string();
        assert_eq!(id, "my-tiles");
        let r = s.execute("map.sources", &json!({})).unwrap();
        assert_eq!(r["active"], id.as_str());
        // Another of the same name gets a fresh id.
        let r2 = s.execute("map.addSource", &json!({ "name": "My Tiles", "template": "https://tiles2.example.org/{z}/{x}/{y}.png" })).unwrap();
        assert_ne!(r2["id"], id);
        // A bad template is refused.
        assert!(s.execute("map.addSource", &json!({ "name": "Bad", "template": "https://no-placeholders" })).is_err());
        // A built-in can't be replaced by id or removed.
        assert!(s.execute("map.removeSource", &json!({ "id": "osm" })).is_err());
        s.execute("map.removeSource", &json!({ "id": id })).unwrap();
        assert!(s.execute("map.removeSource", &json!({ "id": id })).is_err());
    }

    #[test]
    fn undo_steps() {
        let mut s = session();
        s.execute("map.setSource", &json!({ "id": "esri-light" })).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.execute("map.sources", &json!({})).unwrap()["active"], "osm");
    }

    #[test]
    fn sources_query_still_lists_the_builtins() {
        let mut s = session();
        let r = s.execute("map.sources", &json!({})).unwrap();
        assert_eq!(r["active"], "osm");
        assert!(r["sources"].as_array().unwrap().iter().any(|src| src["id"] == "osm" && src["builtin"] == true));
    }

    #[test]
    fn basemap_style_sets_clamps_queries_and_undoes() {
        let mut s = session();
        // No keys: a query, and no edit.
        assert_eq!(s.execute("map.style", &json!({})).unwrap(), json!({ "opacity": 1.0, "brightness": 0, "contrast": 0, "saturation": 0, "hue": 0 }));
        s.execute("map.style", &json!({ "brightness": 20, "hue": 500, "contrast": -5.7, "opacity": 0.4 })).unwrap();
        let st = s.execute("map.style", &json!({})).unwrap();
        assert_eq!(st["opacity"], 0.4);
        assert_eq!(st["brightness"], 20);
        assert_eq!(st["contrast"], -6);
        assert_eq!(st["saturation"], 0);
        assert_eq!(st["hue"], 180, "each field rounds and clamps into range; the others keep their value");
        // One undo step takes the whole set back.
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.execute("map.style", &json!({})).unwrap()["brightness"], 0);
    }

    #[test]
    fn pinning_names_and_defaults_tiles() {
        let mut s = session();
        let r = s
            .execute(
                "map.addTileLayers",
                &json!({ "tiles": [{ "z": 4, "x": 8, "y": 8 }, { "source": "esri-dark", "z": 3, "x": 2, "y": 5, "name": "My Tile" }] }),
            )
            .unwrap();
        assert_eq!(r["ids"], json!(["tile-1", "tile-2"]));
        let tiles = s.execute("map.tileLayers", &json!({})).unwrap()["tiles"].as_array().unwrap().to_vec();
        assert_eq!(tiles.len(), 2);
        assert_eq!(
            tiles[0],
            json!({
                "id": "tile-1", "name": "z4/8/8", "source": "osm", "z": 4, "x": 8, "y": 8,
                "visible": true, "locked": true, "opacity": 1.0,
                "style": { "brightness": 0, "contrast": 0, "saturation": 0, "hue": 0 },
            }),
            "an unnamed tile is named after its coordinates and lands on the active source"
        );
        assert_eq!(tiles[1]["name"], "My Tile");
        assert_eq!(tiles[1]["source"], "esri-dark");
        // An unknown source id falls back to the active one, not an error.
        let r = s.execute("map.addTileLayers", &json!({ "tiles": [{ "source": "nope", "z": 1, "x": 0, "y": 0 }] })).unwrap();
        assert_eq!(r["ids"], json!(["tile-3"]));
        assert_eq!(s.execute("map.tileLayers", &json!({})).unwrap()["tiles"][2]["source"], "osm");
    }

    #[test]
    fn pinning_updates_queries_and_removes_in_one_undo_step_each() {
        let mut s = session();
        s.execute("map.addTileLayers", &json!({ "tiles": [{ "z": 4, "x": 8, "y": 8 }] })).unwrap();
        let r = s
            .execute(
                "map.updateTileLayer",
                &json!({ "id": "tile-1", "name": "Harbour", "visible": false, "locked": true, "opacity": 1.5, "style": { "saturation": -100, "brightness": 5 } }),
            )
            .unwrap();
        assert_eq!(
            r,
            json!({
                "id": "tile-1", "name": "Harbour", "source": "osm", "z": 4, "x": 8, "y": 8,
                "visible": false, "locked": true, "opacity": 1.0,
                "style": { "brightness": 5, "contrast": 0, "saturation": -100, "hue": 0 },
            }),
            "opacity clamps into 0–1 and the style fields keep what the params left out"
        );
        s.execute("edit.undo", &json!({})).unwrap();
        let tiles = s.execute("map.tileLayers", &json!({})).unwrap()["tiles"].as_array().unwrap().to_vec();
        assert_eq!(tiles[0]["opacity"], 1.0, "the update was one undoable edit");
        assert_eq!(tiles[0]["style"]["saturation"], 0);
        // The query lists what a removal is about to take away.
        let r = s.execute("map.removeTileLayers", &json!({ "ids": ["tile-1", "nope"] })).unwrap();
        assert_eq!(r["removed"], 1, "unknown ids are ignored");
        assert!(s.execute("map.tileLayers", &json!({})).unwrap()["tiles"].as_array().unwrap().is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.execute("map.tileLayers", &json!({})).unwrap()["tiles"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn tile_layers_can_move_between_sources_and_positions() {
        let mut s = session();
        s.execute("map.addTileLayers", &json!({ "tiles": [{ "z": 4, "x": 8, "y": 8 }] })).unwrap();
        s.execute("map.updateTileLayer", &json!({ "id": "tile-1", "source": "esri-dark", "z": 3, "x": 1, "y": 2 })).unwrap();
        let t = s.execute("map.tileLayers", &json!({})).unwrap()["tiles"][0].clone();
        assert_eq!(t["source"], "esri-dark");
        assert_eq!(t["z"], 3);
        assert_eq!(t["x"], 1);
        assert_eq!(t["y"], 2);
        // The new position validates as a whole: the old x/y don't survive a smaller zoom
        // (z1 fits 2 columns and rows, so the old y 2 drops off the world and the call is refused).
        assert!(s.execute("map.updateTileLayer", &json!({ "id": "tile-1", "z": 1 })).is_err());
        assert!(s.execute("map.updateTileLayer", &json!({ "id": "tile-1", "source": "nope" })).is_err());
        assert!(s.execute("map.updateTileLayer", &json!({ "id": "tile-9" })).is_err(), "no such tile layer");
        // Nothing moved: every refused update undid itself.
        let t = s.execute("map.tileLayers", &json!({})).unwrap()["tiles"][0].clone();
        assert_eq!(t["source"], "esri-dark");
        assert_eq!(t["z"], 3);
        assert_eq!(t["x"], 1);
        assert_eq!(t["y"], 2);
    }

    #[test]
    fn pinning_skips_duplicates_and_caps_the_count() {
        let mut s = session();
        let row = |xs: std::ops::Range<u32>, z: u32| -> Vec<serde_json::Value> { xs.map(|x| json!({ "z": z, "x": x, "y": 0 })).collect() };
        s.execute("map.addTileLayers", &json!({ "tiles": row(0..8, 4) })).unwrap();
        // The same source and z/x/y, again: nothing pins.
        let r = s.execute("map.addTileLayers", &json!({ "tiles": [{ "z": 4, "x": 0, "y": 0 }] })).unwrap();
        assert_eq!(r["ids"], json!([]));
        assert_eq!(s.execute("map.tileLayers", &json!({})).unwrap()["tiles"].as_array().unwrap().len(), 8);
        // 200 tiles in one call: the first 128 pin.
        let r = s.execute("map.addTileLayers", &json!({ "tiles": row(0..200, 8) })).unwrap();
        assert_eq!(r["ids"].as_array().unwrap().len(), 128);
        // 8 + 128 = 136 pinned; the rest of row y0 (x 128–255) takes it to the 256 cap, so 120 pin.
        let r = s.execute("map.addTileLayers", &json!({ "tiles": row(128..256, 8) })).unwrap();
        assert_eq!(r["ids"].as_array().unwrap().len(), 120);
        let tiles = s.execute("map.tileLayers", &json!({})).unwrap()["tiles"].as_array().unwrap().to_vec();
        assert_eq!(tiles.len(), 256);
        assert_eq!(tiles[255]["id"], "tile-256", "ids keep counting through the caps");
    }

    #[test]
    fn pinning_refuses_bad_tiles_and_params() {
        let mut s = session();
        assert!(s.execute("map.addTileLayers", &json!({})).is_err(), "no tiles array");
        assert!(s.execute("map.addTileLayers", &json!({ "tiles": [{ "z": 4 }] })).is_err(), "x and y missing");
        assert!(s.execute("map.addTileLayers", &json!({ "tiles": [{ "z": 2, "x": 4, "y": 0 }] })).is_err(), "x 4 is off a z2 world");
        assert!(s.execute("map.addTileLayers", &json!({ "tiles": [{ "z": 23, "x": 0, "y": 0 }] })).is_err(), "zoom runs to 22");
        assert!(s.execute("map.removeTileLayers", &json!({})).is_err(), "no ids array");
        // A refused call pins nothing.
        assert!(s.execute("map.tileLayers", &json!({})).unwrap()["tiles"].as_array().unwrap().is_empty());
    }

    fn import(s: &mut Session, paths: serde_json::Value) -> Result<serde_json::Value, crate::EngineError> {
        s.execute("map.importVectorData", &json!({ "name": "Imported data", "paths": paths }))
    }

    #[test]
    fn import_makes_one_layer_of_named_nodes() {
        let mut s = session();
        let layers_before = s.doc().unwrap().doc.layers.len();
        let r = import(
            &mut s,
            json!([
                { "d": "M 10 10 L 20 10 L 20 20 Z M 12 12 L 18 12 L 18 18 Z", "closed": true, "label": "Ring with hole" },
                { "d": "M 0 0 L 5 5", "label": "Ferry" },
                { "d": "M 30 30 L 31 30 L 31 31 Z" },
            ]),
        )
        .unwrap();
        let id = r["id"].as_u64().unwrap();
        assert_eq!(r["nodes"], 3);
        let d = &s.doc().unwrap().doc;
        // The new layer sits above the base "Map" layer and is named for the dataset.
        assert_eq!(d.layers.len(), layers_before + 1);
        let layer = d.layers.last().unwrap();
        assert_eq!(layer.id.0, id);
        assert_eq!(layer.name.as_deref(), Some("Imported data"));
        let nodes: Vec<_> = layer.children().unwrap().to_vec();
        assert_eq!(nodes.len(), 3);
        // The label names the node; an unlabelled one keeps none.
        assert_eq!(nodes[0].name.as_deref(), Some("Ring with hole"));
        assert_eq!(nodes[1].name.as_deref(), Some("Ferry"));
        assert_eq!(nodes[2].name, None);
        // Closed paths fill EvenOdd (holes read); open ones are NonZero. Imported geometry reads
        // as linework: no fill, a black stroke.
        use terrazon_geom::FillRule;
        assert!(matches!(&nodes[0].kind, terrazon_doc::NodeKind::Path { rule: FillRule::EvenOdd, path, .. } if path.subpaths.len() == 2));
        assert!(matches!(&nodes[1].kind, terrazon_doc::NodeKind::Path { rule: FillRule::NonZero, .. }));
        assert!(nodes[0].appearance.fill_paint().is_none(), "imported geometry fills nothing");
        assert!(!nodes[0].appearance.stroke_paint().is_none(), "and reads as a black outline");
        // The layer became current, like layer.new does.
        assert_eq!(s.doc().unwrap().active_layer, Some(terrazon_doc::NodeId(id)));
    }

    #[test]
    fn import_is_one_undo_step_and_undo_restores() {
        let mut s = session();
        let undo = s.doc().unwrap().history.undo.len();
        import(&mut s, json!([{ "d": "M 0 0 L 10 10" }, { "d": "M 5 5 L 9 9 Z", "closed": true }])).unwrap();
        assert_eq!(s.doc().unwrap().history.undo.len(), undo + 1, "the whole dataset is one undo step");
        s.execute("edit.undo", &json!({})).unwrap();
        let d = &s.doc().unwrap().doc;
        assert_eq!(d.layers.len(), 1, "back to the base Map layer alone");
        assert_eq!(d.layers[0].children().unwrap().len(), 0);
        assert_eq!(s.doc().unwrap().history.redo.len(), 1);
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().doc.layers.len(), 2);
    }

    #[test]
    fn import_places_the_layer_above_the_current_layer() {
        let mut s = session();
        // With the base layer current (the default), the data lands just above it.
        import(&mut s, json!([{ "d": "M 0 0 L 1 1" }])).unwrap();
        assert_eq!(s.doc().unwrap().doc.layers[0].name.as_deref(), Some("Map"));
        assert_eq!(s.doc().unwrap().doc.layers[1].name.as_deref(), Some("Imported data"));
        // A second import goes above the first (the current one), at the top level.
        s.execute("map.importVectorData", &json!({ "name": "Second", "paths": [{ "d": "M 0 0 L 1 1" }] })).unwrap();
        let names: Vec<_> = s.doc().unwrap().doc.layers.iter().filter_map(|l| l.name.clone()).collect();
        assert_eq!(names, ["Map", "Imported data", "Second"]);
    }

    #[test]
    fn import_refuses_empty_and_bad_paths() {
        let mut s = session();
        assert!(import(&mut s, json!([])).is_err(), "no paths at all");
        assert!(import(&mut s, json!([{}])).is_err(), "no `d`");
        assert!(import(&mut s, json!([{ "d": "M 0 0 X 9" }])).is_err(), "not path data");
        assert!(import(&mut s, json!([{ "d": "M 0 0" }])).is_err(), "a lone move draws nothing");
        assert!(import(&mut s, json!([{ "d": "M 0 0 L 1 1" }])).is_ok(), "the real thing works");
        // Every refused call left the document alone.
        assert_eq!(s.doc().unwrap().doc.layers.len(), 2, "the base layer plus the one good import");
        assert!(s.execute("map.importVectorData", &json!({ "paths": [{ "d": "M 0 0 L 1 1" }] })).is_err(), "no name");
    }

    #[test]
    fn the_session_token_stores_on_its_source() {
        let mut s = session();
        assert!(s.execute("map.sourceSession", &json!({ "id": "nope", "session": "x" })).is_err(), "no such source");
        assert!(s.execute("map.sourceSession", &json!({})).is_err(), "no id given");
        s.execute("map.sourceSession", &json!({ "id": "google-map-tiles", "session": "tok-1" })).unwrap();
        let src = |s: &Session| s.doc().unwrap().doc.map.sources.iter().find(|src| src.id == "google-map-tiles").unwrap().session.clone();
        assert_eq!(src(&s), "tok-1");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(src(&s), "", "the store is one undoable edit");
    }
}
