//! The map's tile basemap: fetching (through the host's `Services::tile_fetch`), decoding, styling,
//! caching and drawing XYZ tiles under the art, in Web Mercator document space (`terrazon_map`).
//!
//! Tiles never touch the document's render: they draw as egui textures behind the art raster (and
//! so are not in exports), fetched bytes decode into egui textures here, and the canvas repaints
//! while fetches are in flight. The web build has no fetcher (`Services::tile_fetch` is `None`):
//! unloaded tiles then draw as a quiet placeholder grid.
//!
//! Only the tiles the view covers are ever asked for (Google-Maps style), at the zoom whose tiles
//! are ≈256 screen px. While a tile is on its way — or failed and waiting — its nearest ready
//! parent tile draws upscaled over its quad (the child's sub-rect of the parent's texture), so a
//! zoom-in or a fast pan reads as a map at once instead of a grid of dark holes; only when even
//! the world tile isn't here yet does the quiet placeholder grid show. QGIS-style styles
//! (`terrazon_map::TileStyle`) apply when a tile decodes: the
//! cache also keeps each tile's raw bytes, so a restyle re-decodes from bytes already here (no
//! refetch) into an entry keyed by the style's `stamp`, while the neutral style shares one texture
//! with every other neutral draw of the same tile. Tiles pinned as sub-layers of the base "Map"
//! layer ([`terrazon_map::TileLayer`]) draw the same way, each with its own style and opacity.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, Pos2, Rect, Shape, Stroke, TextureHandle, TextureOptions, pos2, vec2};
use terrazon_geom::Point;
use terrazon_map::{MvtGeom, TileId, TileKind, TileStyle, doc_to_lonlat, tiles_for_view_range};

use serde_json::json;

use crate::theme::Tokens;

/// One fetched tile, or its failure, from the host's fetcher.
#[derive(Debug)]
pub enum TileMsg {
    /// The tile's encoded bytes (PNG or JPEG, as the sources serve).
    Done { key: String, bytes: Vec<u8> },
    /// The fetch failed (offline, a bad source, rate limits); retried later.
    Failed { key: String },
    /// The ask never ran: the host's queue was full and dropped it (a fast pan floods it). The
    /// tile's entry goes so the next frame asks again — the re-ask lands on top of the queue and
    /// is served first.
    Dropped { key: String },
}

/// The channel a request's answer comes back on.
pub type TileTx = Sender<TileMsg>;

/// The host-side POST fetcher: the Google Map Tiles `createSession` handshake is a POST, which the
/// GET-only [`TileFetcher`] can't make. Given a cache key, URL, content type and body, get the
/// bytes and `tx.send` a [`TileMsg`] from any thread. Without it (the web) a session source waits.
pub trait TilePoster: Send + Sync {
    fn post(&self, key: String, url: String, content_type: String, body: String, tx: TileTx);
}

/// The host-side fetcher: given a cache key and URL, get the bytes and `tx.send` a [`TileMsg`]
/// (from any thread; the UI polls the other end every frame). The desktop app implements this
/// with `ureq` worker threads.
pub trait TileFetcher: Send + Sync {
    fn fetch(&self, key: String, url: String, tx: TileTx);
}

/// The tile disk cache's size, as the host's probe reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileCacheStats {
    /// Cached tiles (one file per fetched tile URL).
    pub tiles: u64,
    /// Their bytes on disk.
    pub bytes: u64,
}

/// The host's tile disk cache, read and cleared from the UI: the Tiles panel shows what the
/// fetcher kept on disk (a later session answers from there without the network) and offers to
/// clear it. Without a probe (the web) the panel hides the cache line.
pub trait TileCacheProbe: Send + Sync {
    /// The cache's size (`None` when this run keeps no cache — nothing cached yet, or a run
    /// without preferences).
    fn stats(&self) -> Option<TileCacheStats>;
    /// Empty the cache; the next fetch repopulates it.
    fn clear(&self);
}

/// The tile cache: requests out, textures in, one entry per (source, zoom, x, y, style stamp).
pub struct TileCache {
    pub rx: Receiver<TileMsg>,
    pub tx: TileTx,
    entries: HashMap<String, Entry>,
    /// Insertion order, for pruning.
    order: VecDeque<String>,
    /// Every fetched tile's raw bytes, keyed by the *unstyled* key (`{source}/{z}/{x}/{y}`): a
    /// restyle re-decodes from these instead of asking the server again (QGIS re-styles rasters
    /// locally, and a slider drag would otherwise hammer the provider with a refetch per pixel).
    raw: HashMap<String, Vec<u8>>,
    raw_order: VecDeque<String>,
    /// Frames since the cache was made (entries remember when they were last drawn).
    frame: u64,
    /// The context the textures were uploaded to (see `TerrazoneApp::adopt_context`).
    context: u64,
    /// Google Map Tiles sources whose createSession handshake is in flight (one POST per source,
    /// not per tile): their tiles wait instead of asking again every frame.
    session_pending: std::collections::HashSet<String>,
    /// Sources whose handshake failed: retry no sooner (a bad key shouldn't hammer Google).
    session_retry: HashMap<String, Instant>,
}

struct Entry {
    state: State,
}

enum State {
    /// Asked for; the answer hasn't arrived.
    Pending,
    /// Decoded, styled and uploaded to the context (a raster tile's texture); `stamp` is the
    /// [`TileStyle::stamp`] the texture was decoded with.
    Ready { tex: TextureHandle, stamp: u64 },
    /// A vector tile's decoded linework, in document points (its style applies as it draws).
    Vector(Vec<MvtGeom>),
    /// The fetch failed; retry no sooner than `retry_at`.
    Failed { retry_at: Instant },
}

/// Cap on distinct tiles held (textures take memory; pruning keeps the newer half).
const CACHE_CAP: usize = 1024;
/// How long a failed tile waits before asking again.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// How long a refused Google handshake cools down before asking again (a bad key shouldn't
/// hammer Google once per frame — and Google counts the requests).
const SESSION_RETRY_AFTER: Duration = Duration::from_secs(300);
/// The cache-key prefix of a Google createSession handshake answer (the source id follows).
const SESSION_KEY: &str = "google-session/";
/// The most tiles one frame may consider.
const MAX_TILES: usize = 512;

impl Default for TileCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TileCache {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            rx,
            tx,
            entries: HashMap::new(),
            order: VecDeque::new(),
            raw: HashMap::new(),
            raw_order: VecDeque::new(),
            frame: 0,
            context: 0,
            session_pending: std::collections::HashSet::new(),
            session_retry: HashMap::new(),
        }
    }

    /// Drop everything when the graphics context changed (textures die with it; called from
    /// `adopt_context`). The raw bytes survive: they re-decode onto the new context on demand.
    pub(crate) fn recontext(&mut self, context: u64) {
        if self.context != context {
            self.entries.clear();
            self.order.clear();
            self.context = context;
        }
    }

    /// Drop the oldest half of the cache (textures and raw bytes alike) when it passes its cap.
    fn prune(&mut self) {
        if self.entries.len() > CACHE_CAP {
            let mut gone = 0;
            let drop = self.entries.len() - CACHE_CAP / 2;
            while gone < drop
                && let Some(key) = self.order.pop_front()
                && self.entries.remove(&key).is_some()
            {
                gone += 1;
            }
        }
        if self.raw.len() > CACHE_CAP {
            let mut gone = 0;
            let drop = self.raw.len() - CACHE_CAP / 2;
            while gone < drop
                && let Some(key) = self.raw_order.pop_front()
                && self.raw.remove(&key).is_some()
            {
                gone += 1;
            }
        }
    }

    /// How many asked-for tiles haven't answered yet (the Tiles panel's live download count).
    pub fn pending_count(&self) -> usize {
        self.entries.values().filter(|e| matches!(e.state, State::Pending)).count()
    }

    /// The MVT layer names of a decoded vector tile and how many geometries each holds, most
    /// numerous first — the Tiles panel's read-only "what's in this tile" data preview.
    pub fn vector_layers(&self, source: &str, tile: TileId) -> Vec<(String, usize)> {
        let plain = key_of(source, tile);
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (key, e) in &self.entries {
            // The entry key may carry a style stamp after the tile's z/x/y; the tile part is what
            // matters, and only the source prefix has to agree.
            if !key.starts_with(&plain) {
                continue;
            }
            if key.as_bytes().get(plain.len()).is_some_and(|b| *b != b'/' && *b != b'@') {
                continue;
            }
            if let State::Vector(geoms) = &e.state {
                for g in geoms {
                    *counts.entry(g.layer.clone()).or_default() += 1;
                }
            }
        }
        let mut out: Vec<(String, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// The unstyled cache key of a tile: `{source}/{z}/{x}/{y}`.
fn key_of(source: &str, t: TileId) -> String {
    format!("{source}/{}/{}/{}", t.z, t.x, t.y)
}

/// The cache key of a tile drawn in a style: the unstyled key, plus the style's stamp when the
/// style would change a pixel (neutral tiles share one entry whatever draws them). The stamp
/// segment is `s`-prefixed so it never reads as a tile coordinate.
fn key_for(source: &str, t: TileId, style: TileStyle) -> String {
    let key = key_of(source, t);
    if style.is_neutral() { key } else { format!("{key}/s{:x}", style.stamp()) }
}

/// The tile a cache key names: the key's last three numeric segments (`{z}/{x}/{y}`), past an
/// optional `s{stamp}` tail. A source id is a slug, so parsing from the back stays right for
/// numeric ones.
fn tile_of(key: &str) -> TileId {
    let mut segs: Vec<&str> = key.split('/').collect();
    if segs.len() > 3 && segs.last().is_some_and(|s| s.parse::<u32>().is_err()) {
        segs.pop();
    }
    if segs.len() < 3 {
        return TileId::new(0, 0, 0);
    }
    let n = segs.len();
    let (Some(z), Some(x), Some(y)) = (segs[n - 3].parse::<u8>().ok(), segs[n - 2].parse::<u32>().ok(), segs[n - 1].parse::<u32>().ok()) else {
        return TileId::new(0, 0, 0);
    };
    TileId::new(z, x, y)
}

/// The tile rect's screen quad (the flat map's projection).
fn tile_corners(tx0: f64, ty0: f64, tx1: f64, ty1: f64, xf: &crate::canvas::Xf) -> Vec<Pos2> {
    [Point::new(tx0, ty0), Point::new(tx1, ty0), Point::new(tx1, ty1), Point::new(tx0, ty1)].iter().map(|p| xf.to_screen(*p)).collect()
}

/// A raster tile's texture over its quad. `alpha` is the draw opacity (a pinned tile layer's own,
/// under the basemap's).
fn draw_tile_texture(painter: &egui::Painter, tex: egui::TextureId, tile: TileId, xf: &crate::canvas::Xf, alpha: f32) {
    draw_tile_texture_uv(painter, tex, tile, xf, alpha, pos2(0.0, 0.0), pos2(1.0, 1.0));
}

/// The same quad with a sub-rect of the texture: a parent tile upscaled over a still-loading
/// child ([`draw_tile_texture`] draws the whole texture, `uv0`..`uv1` picks the child's corner of
/// it).
fn draw_tile_texture_uv(painter: &egui::Painter, tex: egui::TextureId, tile: TileId, xf: &crate::canvas::Xf, alpha: f32, uv0: Pos2, uv1: Pos2) {
    let (tx0, ty0, tx1, ty1) = tile.rect_pt();
    let corners = tile_corners(tx0, ty0, tx1, ty1, xf);
    let tint = Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
    let (du, dv) = (uv1.x - uv0.x, uv1.y - uv0.y);
    let uv = |u: f32, v: f32| pos2(uv0.x + du * u, uv0.y + dv * v);
    let mut mesh = egui::Mesh::with_texture(tex);
    for (pos, uv) in corners.into_iter().zip([uv(0.0, 0.0), uv(1.0, 0.0), uv(1.0, 1.0), uv(0.0, 1.0)]) {
        mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: tint });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

/// A vector tile's linework: polygons in muted fills with their outlines, lines as hairlines —
/// map style, at any zoom (the strokes are screen pixels, like the art's map strokes). The tile's
/// style tints both (QGIS styles vector layers too), the alpha is the draw opacity.
fn draw_vector_tile(painter: &egui::Painter, geoms: &[MvtGeom], xf: &crate::canvas::Xf, t: &Tokens, style: TileStyle, alpha: f32) {
    let styled = |c: Color32| {
        let (mut r, mut g, mut b) = (c.r(), c.g(), c.b());
        style.apply_rgb(&mut r, &mut g, &mut b);
        Color32::from_rgb(r, g, b)
    };
    let fill = styled(t.panel_darker.lerp_to_gamma(Color32::from_rgb(148, 163, 184), 0.35));
    let line_c = styled(t.text_dim.lerp_to_gamma(Color32::from_rgb(203, 213, 225), 0.6));
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    for g in geoms {
        for ring in &g.rings {
            let pts: Vec<Pos2> = ring.iter().map(|(x, y)| xf.to_screen(Point::new(*x, *y))).collect();
            if pts.len() < 2 {
                continue;
            }
            if g.kind == 3 {
                painter.add(Shape::convex_polygon(
                    pts.clone(),
                    fill.gamma_multiply(a as f32 / 255.0),
                    Stroke::new(1.0, line_c.gamma_multiply(0.8 * a as f32 / 255.0)),
                ));
            } else {
                painter.add(Shape::line(pts, Stroke::new(1.0, line_c.gamma_multiply(a as f32 / 255.0))));
            }
        }
    }
}

/// What this frame does with one visible tile.
enum With {
    /// Paint its texture.
    Draw(TextureHandle),
    /// Paint its decoded vector linework.
    DrawVector(Vec<MvtGeom>),
    /// Decode the raw bytes already here into the wanted style, then paint it.
    Restyle,
    /// Ask the host for it.
    Ask,
    /// Show the placeholder (in flight, failed-and-waiting, or nothing fetches).
    Wait,
}

/// One draw call of the map: poll the fetcher, then every tile this frame wants — the basemap's
/// view tiles at their picked zoom, then the pinned tile layers on top of it — ready ones as
/// quads, the rest as placeholders, plus the source's attribution and the pointer's lat/long.
pub fn draw(app: &mut crate::TerrazoneApp, ui: &mut egui::Ui, rect: Rect, xf: &crate::canvas::Xf) {
    let Some(st) = app.session.active() else { return };
    if !st.doc.map.visible {
        return;
    }
    let Some(source) = st.doc.map.source().cloned() else { return };
    let basemap_style = st.doc.map.style;
    let pinned: Vec<terrazon_map::TileLayer> = st.doc.map.tile_layers.clone();
    let sources: std::collections::HashMap<String, terrazon_map::TileSource> = st.doc.map.sources.iter().map(|s| (s.id.clone(), s.clone())).collect();
    let doc_uid = st.uid;
    let map_opacity = st.doc.map.opacity as f32;
    // `st` borrows the session; everything it lent is cloned above so the fetch answers below can
    // run engine commands (the Google session store) through `app`.
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(rect);
    let t = Tokens::get(ui.ctx());

    // 1. What the fetcher answered since last frame: keep the raw bytes, decode into the style
    //    the key was asked for. A budget, not a drain: a pan back over cached tiles (the disk
    //    cache answers dozens at once) would otherwise decode forty PNGs inside one frame and
    //    hitch — the rest wait for the next frames, which the repaint below already schedules.
    app.canvas.tiles.frame += 1;
    let mut answered = 0;
    while let Ok(msg) = app.canvas.tiles.rx.try_recv() {
        answered += 1;
        if answered > 32 {
            break;
        }
        match msg {
            // The Google createSession handshake's answer (a POST the host made for the key):
            // store the token on its source (one undoable edit, it saves with the file) and the
            // tiles ask again with it. A refusal cools down before trying again.
            TileMsg::Done { key, bytes } if key.starts_with(SESSION_KEY) => {
                let id = &key[SESSION_KEY.len()..];
                app.canvas.tiles.session_pending.remove(id);
                match terrazon_map::parse_session_response(&bytes) {
                    Ok(session) => {
                        let _ = app.run("map.sourceSession", json!({ "id": id, "session": session }));
                    }
                    Err(_) => {
                        app.canvas.tiles.session_retry.insert(id.to_string(), Instant::now() + SESSION_RETRY_AFTER);
                        app.status(crate::i18n::msg("Google Map Tiles refused the key — check it in Map › Tile Sources…"));
                    }
                }
            }
            TileMsg::Failed { key } if key.starts_with(SESSION_KEY) => {
                let id = key.trim_start_matches(SESSION_KEY);
                app.canvas.tiles.session_pending.remove(id);
                app.canvas.tiles.session_retry.insert(id.to_string(), Instant::now() + SESSION_RETRY_AFTER);
            }
            TileMsg::Done { key, bytes } => {
                let plain = {
                    // The asked key's source segment: everything before the tile's z/x/y, past an
                    // `s{stamp}` tail when the tile was asked for styled.
                    let mut segs: Vec<&str> = key.split('/').collect();
                    if segs.len() > 3 && segs.last().is_some_and(|s| s.parse::<u32>().is_err()) {
                        segs.pop();
                    }
                    segs[..segs.len().saturating_sub(3)].join("/")
                };
                let kind = sources.get(&plain).map(|s| s.kind).unwrap_or_default();
                app.canvas.tiles.raw.insert(plain.clone(), bytes.clone());
                app.canvas.tiles.raw_order.push_back(plain);
                let state = match kind {
                    TileKind::Vector => match terrazon_map::decode_mvt(&bytes, tile_of(&key)) {
                        geoms if !geoms.is_empty() => State::Vector(geoms),
                        _ => State::Failed { retry_at: Instant::now() + RETRY_AFTER },
                    },
                    // WMS/WMTS/Google session tiles are raster PNG/JPEG like any XYZ source.
                    TileKind::Raster | TileKind::Wms | TileKind::Wmts | TileKind::GoogleSession => {
                        match decode_styled(&bytes, style_of_key(&key, basemap_style, &pinned)) {
                            Some(img) => {
                                let stamp = stamp_of_key(&key, basemap_style, &pinned);
                                let tex = ctx.load_texture(format!("tile-{key}"), img, TextureOptions::LINEAR);
                                State::Ready { tex, stamp }
                            }
                            None => State::Failed { retry_at: Instant::now() + RETRY_AFTER },
                        }
                    }
                    // 3D Tiles are never fetched per tile (the TileSet registers the source; mesh
                    // rendering needs the 3D viewport) — a defensive long wait if one ever lands.
                    TileKind::Google3dTiles => State::Failed { retry_at: Instant::now() + std::time::Duration::from_secs(3600) },
                };
                app.canvas.tiles.entries.insert(key.clone(), Entry { state });
                app.canvas.tiles.order.push_back(key);
            }
            TileMsg::Failed { key } => {
                let state = State::Failed { retry_at: Instant::now() + RETRY_AFTER };
                app.canvas.tiles.entries.insert(key.clone(), Entry { state });
            }
            // The ask was dropped by a full host queue: forget the tile so the next frame asks
            // again (a Google handshake drop re-opens its session attempt too).
            TileMsg::Dropped { key } => {
                if let Some(id) = key.strip_prefix(SESSION_KEY) {
                    app.canvas.tiles.session_pending.remove(id);
                }
                app.canvas.tiles.entries.remove(&key);
            }
        }
    }

    // 2. The document-space bounds of the view: what the basemap asks for and what a pinned tile
    //    must cross to draw.
    let ppp = ctx.pixels_per_point() as f64;
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    let doc: Vec<Point> = corners.iter().map(|p| xf.to_doc(*p)).collect();
    let x0 = doc.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let y0 = doc.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let x1 = doc.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
    let y1 = doc.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
    let view = (x0, y0, x1, y1);
    let scale = (xf.zoom * ppp).clamp(1.0e-6, 1.0e6);

    // 3. The basemap: the visible tiles at the zoom whose tiles are ≈256 screen px, styled by the
    //    map's own style at the basemap's opacity. The list is published for the panels and the
    //    Map menu ("Pin Visible Tiles").
    let (_, tiles) = tiles_for_view_range(view, scale, source.min_zoom, source.max_zoom, MAX_TILES);
    // The view's centre in document space; the ask order is centre-out — the middle of the screen
    // (where the eye is) fills first, the edges follow, Google-Maps order. Ready tiles draw the
    // same either way (they don't overlap), so only the requests change.
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let dist = |t: &TileId| {
        let (a, b, c, d) = t.rect_pt();
        let (mx, my) = ((a + c) / 2.0, (b + d) / 2.0);
        (((mx - cx) * (mx - cx) + (my - cy) * (my - cy)) as f32).sqrt()
    };
    let mut tiles = tiles;
    tiles.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
    app.canvas_view_tiles = Some((doc_uid, tiles.clone()));
    let fetcher = app.services.tile_fetch.clone();
    let now = Instant::now();
    let mut pending = 0usize;
    let basemap_alpha = map_opacity;
    for tile in tiles {
        if !tile.valid() {
            continue;
        }
        pending += draw_tile(app, &painter, &ctx, &t, xf, &source, tile, basemap_style, basemap_alpha, &fetcher, now);
    }

    // 4. The pinned tile layers, basemap over: each draws its one tile from its source, in its own
    //    style and opacity, whenever the tile crosses the view (the Layers panel lists them as
    //    sub-layers of the base "Map" layer).
    for tl in &pinned {
        if !tl.visible || tl.opacity <= 0.0 {
            continue;
        }
        let tile = TileId::new(tl.z, tl.x, tl.y);
        if !tile.valid() {
            continue;
        }
        let (tx0, ty0, tx1, ty1) = tile.rect_pt();
        if tx1 < x0 || tx0 > x1 || ty1 < y0 || ty0 > y1 {
            continue;
        }
        // Its own source when the document still has it; else the basemap's (a file pinned a
        // connection another copy of the file dropped).
        let src = sources.get(&tl.source).unwrap_or(&source);
        let src = if src.id == tl.source { src } else { &source };
        pending += draw_tile(app, &painter, &ctx, &t, xf, src, tile, tl.style, tl.opacity as f32, &fetcher, now);
    }

    // 5. Attribution (the providers' terms) and the pointer's latitude/longitude.
    let pad = 8.0;
    if !source.attribution.is_empty() {
        let font = egui::FontId::proportional(10.0);
        let galley = painter.layout_no_wrap(source.attribution.clone(), font, t.text_dim);
        let pos = pos2(rect.left() + pad, rect.bottom() - pad - galley.size().y);
        painter.rect_filled(Rect::from_min_size(pos - vec2(3.0, 2.0), galley.size() + vec2(6.0, 4.0)), 3.0, Color32::from_black_alpha(96));
        painter.galley(pos, galley, t.text_dim);
    }
    if let Some(hover) = app.hover_doc
        && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p))
    {
        let (lon, lat) = doc_to_lonlat(hover.x, hover.y);
        let text = coord_label(lat, lon, xf);
        let font = egui::FontId::proportional(10.0);
        let galley = painter.layout_no_wrap(text, font, t.text_dim);
        let pos = pos2(rect.right() - pad - galley.size().x, rect.bottom() - pad - galley.size().y);
        painter.rect_filled(Rect::from_min_size(pos - vec2(3.0, 2.0), galley.size() + vec2(6.0, 4.0)), 3.0, Color32::from_black_alpha(96));
        painter.galley(pos, galley, t.text_dim);
    }

    // 6. Tiles still coming: repaint so they pop in as they arrive.
    if pending > 0 {
        ctx.request_repaint_after(Duration::from_millis(80));
    }
    app.canvas.tiles.prune();
}

/// The style one cache key was asked for: the basemap's unless the key is a pinned tile layer's
/// (theirs are `{source}/{z}/{x}/{y}/s{stamp}`, named by the layer's own style). Matching on the
/// stamp keeps a restyled decode of the same tile from being served the basemap's look.
fn style_of_key(key: &str, basemap: TileStyle, pinned: &[terrazon_map::TileLayer]) -> TileStyle {
    let stamp = stamp_of_tail(key, pinned);
    match stamp {
        Some(s) => pinned.iter().find(|tl| tl.style.stamp() == s).map(|tl| tl.style).unwrap_or(basemap),
        None => basemap,
    }
}

/// The stamp a cache key was asked under (see [`style_of_key`]).
fn stamp_of_key(key: &str, basemap: TileStyle, pinned: &[terrazon_map::TileLayer]) -> u64 {
    stamp_of_tail(key, pinned).unwrap_or_else(|| basemap.stamp())
}

/// The stamp in a key's `s{stamp}` tail, when it names one of `pinned`.
fn stamp_of_tail(key: &str, pinned: &[terrazon_map::TileLayer]) -> Option<u64> {
    let last = key.rsplit('/').next()?;
    let stamp = u64::from_str_radix(last.strip_prefix('s')?, 16).ok()?;
    pinned.iter().any(|tl| tl.style.stamp() == stamp && !tl.style.is_neutral()).then_some(stamp)
}

/// How far up the pyramid a parent may sit and still read as the tile (beyond this the stretch
/// blurs past recognition; the placeholder's quiet grid reads better).
const MAX_ANCESTOR_DZ: u32 = 6;

/// The ancestor `dz` levels above `tile` (its corner of the parent's square, as UVs 0..1 over the
/// parent's texture). `None` past the top of the pyramid.
pub(crate) fn ancestor_uv(tile: TileId, dz: u32) -> Option<(TileId, Pos2, Pos2)> {
    if dz == 0 || dz > MAX_ANCESTOR_DZ {
        return None;
    }
    let az = i32::from(tile.z) - dz as i32;
    if az < 0 {
        return None;
    }
    let span = 1u32 << dz;
    let anc = TileId::new(az as u8, tile.x >> dz, tile.y >> dz);
    let d = 1.0 / span as f32;
    let (u, v) = ((tile.x & (span - 1)) as f32 * d, (tile.y & (span - 1)) as f32 * d);
    Some((anc, pos2(u, v), pos2(u + d, v + d)))
}

/// The nearest ready raster ancestor of `tile`: an upscaled parent keeps the map readable while
/// the tile itself fetches (Google-Maps order), instead of a dark hole per tile. Vector tiles
/// have no texture to stretch — their entries never match.
fn ready_ancestor(app: &crate::TerrazoneApp, source_id: &str, tile: TileId, style: TileStyle) -> Option<(TextureHandle, Pos2, Pos2)> {
    for dz in 1..=MAX_ANCESTOR_DZ {
        let Some((anc, uv0, uv1)) = ancestor_uv(tile, dz) else { break };
        if let Some(Entry { state: State::Ready { tex, stamp }, .. }) = app.canvas.tiles.entries.get(&key_for(source_id, anc, style))
            && *stamp == style.stamp()
        {
            return Some((tex.clone(), uv0, uv1));
        }
    }
    None
}

/// Draw `tile`'s nearest ready parent over its quad, or the quiet placeholder when even the
/// world tile isn't here yet.
#[allow(clippy::too_many_arguments)]
fn draw_loading(
    app: &crate::TerrazoneApp,
    painter: &egui::Painter,
    xf: &crate::canvas::Xf,
    t: &Tokens,
    source_id: &str,
    tile: TileId,
    style: TileStyle,
    alpha: f32,
) {
    if let Some((tex, uv0, uv1)) = ready_ancestor(app, source_id, tile, style) {
        draw_tile_texture_uv(painter, tex.id(), tile, xf, alpha, uv0, uv1);
    } else {
        draw_placeholder(painter, &tile_corners_of(tile, xf), t);
    }
}

/// Ask for, decode, style or draw one tile of `source`, in `style` at `alpha`. Returns 1 while the
/// tile isn't on screen yet (so the caller keeps repainting), 0 when it drew or gave up for now.
#[allow(clippy::too_many_arguments)]
fn draw_tile(
    app: &mut crate::TerrazoneApp,
    painter: &egui::Painter,
    ctx: &egui::Context,
    t: &Tokens,
    xf: &crate::canvas::Xf,
    source: &terrazon_map::TileSource,
    tile: TileId,
    style: TileStyle,
    alpha: f32,
    fetcher: &Option<std::sync::Arc<dyn TileFetcher>>,
    now: Instant,
) -> usize {
    let key = key_for(&source.id, tile, style);
    let with = {
        match app.canvas.tiles.entries.get(&key) {
            Some(Entry { state: State::Ready { tex, stamp }, .. }) if *stamp == style.stamp() => With::Draw(tex.clone()),
            Some(Entry { state: State::Vector(geoms), .. }) => With::DrawVector(geoms.clone()),
            Some(Entry { state: State::Pending, .. }) => With::Wait,
            Some(Entry { state: State::Failed { retry_at }, .. }) if now < *retry_at => With::Wait,
            // A texture under another style, or nothing: the raw bytes (kept for every fetched
            // tile) re-decode into the wanted style with no refetch.
            _ if app.canvas.tiles.raw.contains_key(&key_of(&source.id, tile)) => With::Restyle,
            _ => With::Ask,
        }
    };
    match with {
        With::Draw(tex) => {
            draw_tile_texture(painter, tex.id(), tile, xf, alpha);
            0
        }
        With::DrawVector(geoms) => {
            draw_vector_tile(painter, &geoms, xf, t, style, alpha);
            0
        }
        With::Restyle => {
            // Decode the raw bytes into the wanted style, draw what came out this very frame (no
            // one-frame gap while a style slider drags), and keep it under the style's key.
            let plain = key_of(&source.id, tile);
            let bytes = app.canvas.tiles.raw.get(&plain).cloned().unwrap_or_default();
            if source.kind == TileKind::Vector {
                match terrazon_map::decode_mvt(&bytes, tile) {
                    geoms if !geoms.is_empty() => {
                        app.canvas.tiles.entries.insert(key.clone(), Entry { state: State::Vector(geoms.clone()) });
                        app.canvas.tiles.order.push_back(key);
                        draw_vector_tile(painter, &geoms, xf, t, style, alpha);
                    }
                    _ => {
                        // Undecodable bytes (a truncated response, say): wait like a failed fetch,
                        // the next Ask past the retry window tries the server again.
                        app.canvas.tiles.entries.insert(key, Entry { state: State::Failed { retry_at: now + RETRY_AFTER } });
                        draw_placeholder(painter, &tile_corners_of(tile, xf), t);
                    }
                }
            } else if let Some(img) = decode_styled(&bytes, style) {
                let tex = ctx.load_texture(format!("tile-{key}"), img, TextureOptions::LINEAR);
                app.canvas.tiles.entries.insert(key.clone(), Entry { state: State::Ready { tex: tex.clone(), stamp: style.stamp() } });
                app.canvas.tiles.order.push_back(key);
                draw_tile_texture(painter, tex.id(), tile, xf, alpha);
            } else {
                app.canvas.tiles.entries.insert(key, Entry { state: State::Failed { retry_at: now + RETRY_AFTER } });
                draw_placeholder(painter, &tile_corners_of(tile, xf), t);
            }
            0
        }
        With::Ask => {
            // 3D Tiles have no per-tile 2D URL: the TileSet registers the source, the 2D canvas
            // draws the session 2D tiles as imagery (the panel says what the 3D kind is for).
            if source.kind == TileKind::Google3dTiles {
                return 1;
            }
            // A Google session source with no session yet: run the createSession handshake once
            // per source, then the tiles ask again with the stored token.
            if source.kind == TileKind::GoogleSession && source.session.is_empty() {
                return google_handshake(app, source, now);
            }
            if let Some(f) = fetcher {
                app.canvas.tiles.entries.insert(key.clone(), Entry { state: State::Pending });
                app.canvas.tiles.order.push_back(key.clone());
                f.fetch(key, terrazon_map::build_tile_url(source, u32::from(tile.z), tile.x, tile.y), app.canvas.tiles.tx.clone());
            }
            // The ask is away; what shows now is the parent's picture (see `draw_loading`).
            draw_loading(app, painter, xf, t, &source.id, tile, style, alpha);
            1
        }
        With::Wait => {
            // In flight or failed-and-waiting: the nearest ready parent keeps the ground visible
            // (a zoom-in reads instantly instead of a grid of dark holes).
            draw_loading(app, painter, xf, t, &source.id, tile, style, alpha);
            1
        }
    }
}

/// The Google Map Tiles createSession handshake: one POST per source (the GET-only tile fetcher
/// can't make it — the host answers through `Services::tile_post_fetch`); the token stores on the
/// source through `map.sourceSession` when the answer lands, and the tiles ask again with it.
/// While it is in flight, and for a while after a failure, the tiles just wait.
fn google_handshake(app: &mut crate::TerrazoneApp, source: &terrazon_map::TileSource, now: Instant) -> usize {
    if source.api_key.is_empty() {
        // No key, no handshake: the sources dialog is where the key goes.
        return 1;
    }
    if let Some(until) = app.canvas.tiles.session_retry.get(&source.id)
        && now < *until
    {
        return 1;
    }
    if app.canvas.tiles.session_pending.contains(&source.id) {
        return 1;
    }
    let Some(poster) = app.services.tile_post_fetch.clone() else {
        return 1;
    };
    let (url, body) = terrazon_map::session_request(&source.api_key);
    app.canvas.tiles.session_pending.insert(source.id.clone());
    poster.post(format!("{SESSION_KEY}{}", source.id), url, "application/json".into(), body, app.canvas.tiles.tx.clone());
    1
}

/// The tile rect's screen quad (see [`tile_corners`]).
fn tile_corners_of(tile: TileId, xf: &crate::canvas::Xf) -> Vec<Pos2> {
    let (tx0, ty0, tx1, ty1) = tile.rect_pt();
    tile_corners(tx0, ty0, tx1, ty1, xf)
}

/// The unloaded-tile look: the pasteboard's colour, a shade darker, with its grid line.
fn draw_placeholder(painter: &egui::Painter, q: &[Pos2], t: &Tokens) {
    let fill = t.pasteboard.lerp_to_gamma(Color32::BLACK, 0.12);
    let line = egui::Stroke::new(1.0, t.pasteboard.lerp_to_gamma(Color32::BLACK, 0.3));
    if q.len() >= 4 {
        painter.add(Shape::convex_polygon(q.to_vec(), fill, line));
    }
}

/// Decode a tile's bytes (PNG or JPEG, what every source here serves) into an egui image, styled
/// by `style` (a neutral style is the pixel-for-pixel original).
fn decode_styled(bytes: &[u8], style: TileStyle) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let mut raw = rgba.into_raw();
    if !style.is_neutral() {
        style.style_rgba(&mut raw);
    }
    Some(ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &raw))
}

/// The pointer's coordinates as people read them — "6.7923°S, 39.2081°E": hemisphere letters, and
/// the decimals of the rulers' own degree step plus two, so the readout is always finer than the
/// graticule and the values change smoothly as the pointer moves over the map.
pub(crate) fn coord_label(lat: f64, lon: f64, xf: &crate::canvas::Xf) -> String {
    let (lon_step, lat_step) = crate::canvas::mercator_steps(xf);
    coord_label_steps(lat, lon, lon_step, lat_step)
}

/// The readout for a step pair already picked — the status bar rides the steps the rulers drew
/// this frame ([`crate::CanvasCache::ruler_steps`]), 2D and 3D alike.
pub(crate) fn coord_label_steps(lat: f64, lon: f64, lon_step: f64, lat_step: f64) -> String {
    // Two decimals finer than the graticule's labels (which the step's own decimals now name
    // exactly — a 2.5 rung needs one more than its magnitude), clamped so an absurd step can't
    // flood the readout.
    let dec = |step: f64| (crate::canvas::step_decimals(step) + 2).min(6);
    format!(
        "{:.*}°{}, {:.*}°{}",
        dec(lat_step),
        lat.abs(),
        if lat < 0.0 { 'S' } else { 'N' },
        dec(lon_step),
        lon.abs(),
        if lon < 0.0 { 'W' } else { 'E' }
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A view over the map's document space, `zoom` screen points per document point.
    fn xf(zoom: f64) -> crate::canvas::Xf {
        crate::canvas::Xf {
            rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
            zoom,
            center: Point::new(0.0, 0.0),
            rot: 0.0,
        }
    }

    #[test]
    fn coord_label_reads_lat_then_lon_with_hemispheres() {
        let s = coord_label(-6.79235, 39.20814, &xf(1.0));
        assert!(s.contains("°S,"), "south of the equator: {s}");
        assert!(s.ends_with("°E"), "east of the meridian: {s}");
        let n = coord_label(41.5, -70.0, &xf(1.0));
        assert!(n.contains("°N,") && n.ends_with("°W"), "{n}");
    }

    #[test]
    fn coord_label_zooms_into_finer_decimals() {
        // World zoom: a coarse readout; street zoom: the same pointer read far finer — the
        // decimals follow the rulers' degree step, plus two.
        fn lat(s: &str) -> &str {
            s.split(',').next().unwrap_or_default().trim()
        }
        let far = coord_label(-6.79235, 39.20814, &xf(1.0));
        let near = coord_label(-6.79235, 39.20814, &xf(100_000.0));
        assert!(lat(&near).len() > lat(&far).len(), "closer zoom reads finer: {far} vs {near}");
    }

    #[test]
    fn decode_rejects_junk() {
        assert!(decode_styled(b"not an image", TileStyle::default()).is_none());
        assert!(decode_styled(&[], TileStyle::default()).is_none());
    }

    #[test]
    fn ancestor_uv_names_the_parent_and_the_childs_sub_rect() {
        // z3 tile (4, 6): its z2 parent is (2, 3), and the tile is the parent's top-left quarter.
        let (anc, uv0, uv1) = ancestor_uv(TileId::new(3, 4, 6), 1).unwrap();
        assert_eq!(anc, TileId::new(2, 2, 3));
        assert_eq!(uv0, pos2(0.0, 0.0));
        assert_eq!(uv1, pos2(0.5, 0.5));
        // Two levels up: z3 (5, 7) sits in z1 tile (1, 1), its quarter-of-a-quarter lower right.
        let (anc, uv0, uv1) = ancestor_uv(TileId::new(3, 5, 7), 2).unwrap();
        assert_eq!(anc, TileId::new(1, 1, 1));
        assert_eq!(uv0, pos2(0.25, 0.75));
        assert_eq!(uv1, pos2(0.5, 1.0));
        // The world tile has no parent, and zero or out-of-range depths ask nothing.
        assert!(ancestor_uv(TileId::new(0, 0, 0), 1).is_none());
        assert!(ancestor_uv(TileId::new(4, 8, 8), 0).is_none());
        assert!(ancestor_uv(TileId::new(4, 8, 8), MAX_ANCESTOR_DZ + 1).is_none());
    }

    #[test]
    fn keys_carry_the_style_stamp_only_when_styled() {
        let t = TileId::new(4, 8, 8);
        assert_eq!(key_for("osm", t, TileStyle::default()), "osm/4/8/8");
        let styled = TileStyle { saturation: 40, ..TileStyle::default() };
        assert_eq!(key_for("osm", t, styled), format!("osm/4/8/8/s{:x}", styled.stamp()));
    }

    #[test]
    fn tile_of_reads_the_tail_of_a_key() {
        assert_eq!(tile_of("osm/4/8/8"), TileId::new(4, 8, 8));
        assert_eq!(tile_of("osm/4/8/8/1a2b"), TileId::new(4, 8, 8));
        // A numeric source id is not mistaken for the tile.
        assert_eq!(tile_of("123/4/8/8"), TileId::new(4, 8, 8));
        assert_eq!(tile_of("osm"), TileId::new(0, 0, 0));
    }

    #[test]
    fn vector_layers_counts_by_layer_name() {
        let mut c = TileCache::new();
        let road =
            |n: usize| (0..n).map(|_| MvtGeom { layer: "road".into(), kind: 2, rings: vec![vec![(0.0, 0.0), (1.0, 1.0)]] }).collect::<Vec<MvtGeom>>();
        let water = vec![MvtGeom { layer: "water".into(), kind: 3, rings: vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]] }];
        c.entries.insert("mvtsrc/4/8/8".into(), Entry { state: State::Vector(road(2)) });
        c.entries.insert("mvtsrc/4/8/8/ff".into(), Entry { state: State::Vector(water) });
        c.entries.insert("osm/4/8/8".into(), Entry { state: State::Pending });
        let ls = c.vector_layers("mvtsrc", TileId::new(4, 8, 8));
        assert_eq!(ls, vec![("road".to_string(), 2), ("water".to_string(), 1)]);
        assert!(c.vector_layers("osm", TileId::new(4, 8, 8)).is_empty());
    }

    #[test]
    fn style_of_key_picks_the_pinned_layers_style() {
        let pinned_style = TileStyle { brightness: 30, ..TileStyle::default() };
        let pinned = vec![terrazon_map::TileLayer { id: "tile-1".into(), style: pinned_style, ..terrazon_map::TileLayer::default() }];
        let key = key_for("osm", TileId::new(4, 8, 8), pinned_style);
        assert_eq!(style_of_key(&key, TileStyle::default(), &pinned), pinned_style);
        assert_eq!(stamp_of_key(&key, TileStyle::default(), &pinned), pinned_style.stamp());
        // The basemap's key reads the basemap style back.
        let plain = key_for("osm", TileId::new(4, 8, 8), TileStyle::default());
        assert_eq!(style_of_key(&plain, TileStyle::default(), &pinned), TileStyle::default());
    }
}
