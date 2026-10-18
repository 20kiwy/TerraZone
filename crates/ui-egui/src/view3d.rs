//! The 3D canvas environment — the second canvas of the dual-canvas view. When the document's
//! view is in 3D mode ([`crate::View::mode3d`]) the canvas area renders this viewport instead of
//! the 2D map/paper path: a CPU-projected egui-painter scene with the 2D basemap's imagery tiles
//! draped FLAT on the datum (the elevation mesh is gone — the ground reads exactly like the 2D
//! map, in perspective), the terrain's contour lines with their heights as text, the document's
//! own drawing draped with its fills the way the 2D canvas paints them, the placed assets
//! standing on the ground, the placement tool's ghost, and the position aids: a ground grid and
//! the X/Y/Z axis gizmo at the scene's position, with a big cursor marker under the pointer.
//! The map layer itself stays flat 2D — this environment is the one 3D.
//!
//! The rendering is a port of the ArchCraft (effect) project's proven `view3d.rs` painter
//! viewport: the same Lambert shade (`ambient 0.44 + 0.62·lambert + fill`), the same light, the
//! same per-bucket back-to-front triangle sort into one egui mesh per bucket (Ground → Decal →
//! Standing), the same near-plane rejection and triangle budget.
//!
//! World frame (see `terrazon_scene3d::mercator`): metres, `x` east, `y` up, `z` south, the
//! origin at the scene centre — the DEM window's anchor, which follows the 2D view's centre.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, FontId, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, TextureOptions, Ui, pos2, vec2};
use glam::{Mat4, Vec2, Vec3, Vec4};

use terrazon_assets::{AssetKind, AssetPlacement};
use terrazon_color::Paint;
use terrazon_doc::NodeKind;
use terrazon_geom::{BezPath, PathEl, Point};
use terrazon_map::{TileId, WORLD_PT, doc_to_lonlat, lonlat_to_doc};
use terrazon_scene3d::dem::{DEM_ZOOM, DemGrid, DemLoader};
use terrazon_scene3d::mesh::Mesh3d;
use terrazon_scene3d::prim;
use terrazon_scene3d::terrain::{Contours, contours};
use terrazon_scene3d::{Mercator, OrbitCamera, Ray};

use crate::TerrazoneApp;
use crate::tiles::{TileFetcher, TileMsg, TileTx};

/// The fill light (shading matches the ArchCraft viewport the look is copied from).
const LIGHT: Vec3 = Vec3::new(0.45, 0.85, -0.35);
/// Painter rendering has no z-buffer: past this many triangles the far ones are dropped (with a
/// warning) instead of hanging the frame.
const TRI_BUDGET: usize = 400_000;
/// A calm light blue-grey sky.
const SKY: Color32 = Color32::from_rgb(0xc6, 0xd0, 0xd9);
/// Behind-the-camera rejection threshold (homogeneous w), as in ArchCraft's `Xform::proj`.
const NEAR_W: f32 = 1.0e-6;
/// Drag orbit speed (radians per pointer point).
const ORBIT_SPEED: f32 = 0.005;
/// One wheel notch dollies the camera by this factor.
const DOLLY: f32 = 1.2;
/// View-centre travel (ground metres) that rebuilds the DEM window around the new centre.
const DEM_MOVE_M: f64 = 1500.0;
/// The imagery ring around the scene centre: ±[`IMAGERY_RING`] tiles at the base zoom, wider
/// (and capped higher) as the drape refines under a closer camera.
const IMAGERY_RING: i64 = 2;
const IMAGERY_CAP: usize = 24;
/// How long a failed imagery tile waits before asking again (as the 2D tile cache does).
const IMAGERY_RETRY: Duration = Duration::from_secs(30);
/// The ground slab's half extent when no DEM window is known (metres).
const SLAB_HALF: f32 = 60_000.0;
/// Decals (contours, the document's draped drawing) and imagery sit this far above the datum
/// plane, so the painter's draw order (no z-buffer) keeps them on top of the slab under them.
const SKIN_LIFT: f32 = 0.35;

/// A true elevation at/below this is open water, not land: no land on Earth is deeper than
/// −1 km, while bathymetric DEMs (Terrarium carries GEBCO depths) read thousands of metres
/// down mid-ocean — an anchor there must datum to the sea surface, not the seafloor.
pub(crate) const OPEN_WATER_ELEV: f32 = -1000.0;

/// One triangle of the sorted painter pass.
#[derive(Clone)]
struct Tri {
    depth: f32,
    a: Pos2,
    b: Pos2,
    c: Pos2,
    color: Color32,
}

/// The world→screen projection (view·proj, one per frame), ported from ArchCraft's `Xform`.
struct Xform {
    vp: Mat4,
    rect: Rect,
}

impl Xform {
    /// Project a world point to screen; `None` behind the camera (w too small) or past the far
    /// plane.
    fn proj(&self, p: [f32; 3]) -> Option<Pos2> {
        self.proj_w(p).map(|(s, _)| s)
    }

    /// The same projection with the clip-space `w` (the view depth the affine-texture warp
    /// reads): the imagery drape subdivides by the depth range across a tile, so the ground
    /// imagery reads like the 2D map under any camera tilt instead of shearing.
    fn proj_w(&self, p: [f32; 3]) -> Option<(Pos2, f32)> {
        let v = self.vp * Vec4::new(p[0], p[1], p[2], 1.0);
        if v.w <= NEAR_W {
            return None;
        }
        let ndc = v.truncate() / v.w;
        if ndc.z > 1.0 || ndc.z < -1.0 {
            return None;
        }
        Some((
            pos2(self.rect.left() + (ndc.x * 0.5 + 0.5) * self.rect.width(), self.rect.top() + (1.0 - (ndc.y * 0.5 + 0.5)) * self.rect.height()),
            v.w,
        ))
    }

    /// Unproject a screen point onto the world's ground plane (y = 0): the ray through the
    /// point, cut where it meets the datum. `None` when the ray never comes down (the sky).
    /// The imagery ring reads this to cover the ground the tilted camera actually sees.
    fn ground_pick(&self, screen: Pos2) -> Option<[f32; 3]> {
        let ndc = |x: f32, y: f32| {
            Vec2::new((x - self.rect.left()) / self.rect.width() * 2.0 - 1.0, 1.0 - (y - self.rect.top()) / self.rect.height() * 2.0)
        };
        let n = ndc(screen.x, screen.y);
        let inv = self.vp.inverse();
        let unproject = |ndc_z: f32| {
            let v = inv * Vec4::new(n.x, n.y, ndc_z, 1.0);
            v.truncate() / v.w
        };
        let near = unproject(-1.0);
        let far = unproject(1.0);
        let dir = far - near;
        if dir.y.abs() < 1e-5 || dir.y > 0.0 {
            return None; // parallel to the ground or pointing up
        }
        let t = -near.y / dir.y;
        if t < 0.0 {
            return None;
        }
        let p = near + dir * t;
        Some([p.x, 0.0, p.z])
    }

    /// The Lambert shade: ambient + diffuse + a sky fill from above (the ArchCraft look).
    fn shade(color: [f32; 3], normal: Vec3) -> Color32 {
        let n = normal.normalize_or_zero();
        let n = if n.dot(-LIGHT) < 0.0 { -n } else { n };
        let lambert = n.dot(LIGHT).max(0.0);
        let fill = 0.12 * (0.5 + 0.5 * n.y);
        let k = (0.44 + 0.62 * lambert + fill).min(1.25);
        Color32::from_rgb((color[0] * k * 255.0).min(255.0) as u8, (color[1] * k * 255.0).min(255.0) as u8, (color[2] * k * 255.0).min(255.0) as u8)
    }
}

/// The 3D ground's imagery tiles: asks through the app's tile fetcher on a private channel (the
/// 2D map's [`crate::tiles::TileCache`] is untouched), textures keyed by tile key. The drape
/// refines as the camera closes (the dolly picks a finer zoom, like a 3D basemap), and while a
/// finer tile loads its ready ancestor stretches under it (the 2D cache's parent fallback).
pub struct Imagery {
    rx: Receiver<TileMsg>,
    tx: TileTx,
    /// Textures and in-flight marks per tile key (`{z}/{x}/{y}`); keeps the current ring's tiles
    /// and their ready ancestors.
    tiles: HashMap<String, ImageryTile>,
    /// The tiles the current ask covers, nearest first: what [`draw_imagery`] dresses.
    ring: Vec<TileId>,
    /// The tile ring the current asks cover (zoom, tile-x/y extents): re-asked when it
    /// changes.
    asked: Option<(u8, u32, u32, u32, u32)>,
    /// The egui context tag the textures were uploaded to (a new context starts the ring over).
    tag: u64,
    /// The pinned tile layers' own textures, keyed by tile-layer id (each pinned layer draws
    /// its one tile above the basemap, at its stored zoom — the Layers panel's order).
    pinned: HashMap<String, ImageryTile>,
}

struct ImageryTile {
    state: ImageryState,
}

enum ImageryState {
    Pending,
    Ready(TextureHandle),
    Failed(Instant),
}

/// The 3D environment's live state — DEM loader (for the contours and the readout's
/// elevations), the scene anchor, imagery and the placements' cached prim meshes. Held on
/// [`crate::CanvasCache::dem`] (never saved); built on the first 3D frame and kept across frames.
pub struct DemRuntime {
    /// The DEM window's loader (`None` while the host has no [`crate::Services::dem`]): it ticks
    /// every frame and stitches answered Terrarium tiles into the grid.
    pub loader: Option<DemLoader>,
    /// The DEM tiles stitched into the grid so far (the readout's `9/9`; re-stitch on change).
    ready_seen: usize,
    /// The scene centre (lon, lat) the whole world anchors at: the Mercator origin.
    pub anchor: (f64, f64),
    /// The DEM elevation at the anchor (true metres) — the datum the contours read against, so
    /// the site's contour ladder starts at grade.
    pub base: f32,
    /// The stitched elevation grid (until it lands the contours are empty).
    pub grid: Option<DemGrid>,
    /// Marching-squares contours over the grid, with the settings they were traced for.
    pub contours: Option<Contours>,
    contours_sig: (f32, f32),
    /// The imagery tile runtime.
    pub imagery: Imagery,
    /// The placements' prim meshes, cached per placement id (and the ghost, per kind).
    prims: HashMap<String, Mesh3d>,
    /// The camera was framed on the first grid; afterwards the user's camera moves are kept.
    pub framed: bool,
    /// The user drove the camera at least once (until then the first grid may recentre it).
    pub interacted: bool,
    /// The open-water datum hint fired once for this anchor (resets on re-anchor).
    water_hinted: bool,
}

impl DemRuntime {
    /// A runtime anchored at `(lon, lat)`, with a DEM loader when a fetcher exists.
    fn new(anchor: (f64, f64), fetcher: Option<&dyn terrazon_scene3d::DemFetcher>) -> Self {
        let loader = fetcher.map(|f| DemLoader::new(f, anchor.0, anchor.1, DEM_ZOOM));
        let imagery = Imagery::new();
        Self {
            loader,
            ready_seen: 0,
            anchor,
            base: 0.0,
            grid: None,
            contours: None,
            contours_sig: (0.0, 0.0),
            imagery,
            prims: HashMap::new(),
            framed: false,
            interacted: false,
            water_hinted: false,
        }
    }

    /// The contours for the current interval and datum, traced on demand (and only once per
    /// change — marching squares over the strided lattice is not free).
    fn contour_lines(&mut self, interval: f32, base: f32) -> Option<&Contours> {
        if self.grid.is_none() || interval <= 0.0 {
            return None;
        }
        let sig = (interval, base);
        if self.contours.is_none() || self.contours_sig != sig {
            let grid = self.grid.as_ref()?;
            self.contours = Some(contours(grid, base, interval, 5, 4));
            self.contours_sig = sig;
        }
        self.contours.as_ref()
    }

    /// The prim mesh of one placement, built once and cached by id.
    fn prim_of(&mut self, p: &AssetPlacement) -> Mesh3d {
        self.prims.entry(p.id.clone()).or_insert_with(|| build_prim(p.kind, p.scale)).clone()
    }

    /// The placement ghost's prim mesh, cached by kind.
    fn ghost_of(&mut self, kind: AssetKind) -> Mesh3d {
        let key = format!("ghost/{kind:?}");
        self.prims.entry(key).or_insert_with(|| build_prim(kind, 1.0)).clone()
    }

    /// A scene3d.set (or any terrain setting change): retrace the contours next frame.
    pub fn invalidate_mesh(&mut self) {
        self.contours = None;
    }

    /// A placement is gone: drop its cached prim mesh.
    pub fn forget_prim(&mut self, id: &str) {
        self.prims.remove(id);
    }
}

impl Imagery {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { rx, tx, tiles: HashMap::new(), ring: vec![], asked: None, tag: 0, pinned: HashMap::new() }
    }
}

/// The prim mesh of one catalogue asset at `scale` (metres), origin at its ground centre.
fn build_prim(kind: AssetKind, scale: f32) -> Mesh3d {
    match kind {
        AssetKind::House => prim::house(10.0 * scale, 8.0 * scale, 4.0 * scale),
        // A block building: the catalogue footprint, one colour, 3.5 m per storey — the scale
        // doubles as the storey count (1 → three storeys, 2 → six).
        AssetKind::Building => {
            let [w, d] = AssetKind::Building.size_m();
            let (hw, hd) = (w * 0.5 * scale, d * 0.5 * scale);
            let storeys = (scale * 3.0).round().max(1.0);
            let top = 3.5 * storeys;
            let wall = [0.72, 0.70, 0.66];
            terrazon_scene3d::mesh::box_between([-hw, 0.0, -hd], [hw, top, hd], wall)
        }
        AssetKind::Tree => prim::tree(8.0 * scale),
        AssetKind::Stadium => prim::stadium(105.0 * scale, 68.0 * scale),
        AssetKind::Pin => prim::pin(6.0 * scale),
    }
}

/// The scene Mercator: anchored at the runtime's scene centre (stable between DEM window
/// rebuilds so placements don't wander), or at the 2D view's centre before there is one.
fn scene_mercator(app: &TerrazoneApp) -> Mercator {
    let (lon, lat) = app.canvas.dem.as_ref().map_or(view_centre_lonlat(app), |r| r.anchor);
    Mercator::new(lat, lon)
}

/// The kind the Assets tool would place, when it is the tool holding the canvas (its option
/// names the kind; the tool's own options are the one truth).
fn assets_tool_kind(app: &TerrazoneApp) -> Option<AssetKind> {
    if app.session.tool_id() != "assets" {
        return None;
    }
    let opts = app.session.tool_options();
    let kind = opts.get("kind").and_then(|v| v.as_str()).unwrap_or("Tree");
    AssetKind::parse(kind)
}

/// A soft blob shadow under a standing prop: a dark ellipse decal offset opposite the sun (the
/// light comes from the north-west, so the shadow falls south-east), drawn on the terrain skin
/// the way the contours are. This is the baked shadow a real sun casts — the placement reads as
/// standing ON the ground, not pasted on it.
fn push_blob_shadow(bucket: &mut Vec<Tri>, drawn: &mut usize, xform: &Xform, at: [f32; 3], size_m: [f32; 2], m: &Mercator) {
    let (w, d) = (size_m[0], size_m[1]);
    let r = (w.max(d) * 0.5).max(1.0);
    // South-east offset (the sun's opposite), a quarter of the radius; the shadow stretches
    // slightly with height — kept simple: fixed quarter offset, same skin.
    let (ox, oz) = (r * 0.22, r * 0.30);
    let (lon, lat) = m.unproject(f64::from(at[0] + ox), f64::from(-(at[2] + oz)));
    let (cx, yn) = m.project(lon, lat);
    let c = [cx as f32, at[1] + 0.03, -(yn as f32)];
    let col = Color32::from_rgba_unmultiplied(0x1e, 0x24, 0x18, 88);
    let segs = 14;
    let squash = (d / w.max(0.01)).clamp(0.4, 1.0);
    let mut ring: Vec<Pos2> = Vec::with_capacity(segs + 1);
    for i in 0..=segs {
        let a = std::f32::consts::TAU * i as f32 / segs as f32;
        if let Some(p) = xform.proj([c[0] + r * a.cos(), c[1], c[2] + r * a.sin() * squash]) {
            ring.push(p);
        }
    }
    if ring.len() > 3 {
        let depth = (c[0] + c[2]).abs() * 0.001 + 0.02; // just above the skin, under the prop
        for i in 1..ring.len().saturating_sub(1) {
            if *drawn > TRI_BUDGET {
                return;
            }
            bucket.push(Tri { depth, a: ring[0], b: ring[i], c: ring[i + 1], color: col });
            *drawn += 1;
        }
    }
}

/// The 2D view centre in lon/lat — what the DEM window follows.
fn view_centre_lonlat(app: &TerrazoneApp) -> (f64, f64) {
    app.view().map_or((0.0, 0.0), |v| doc_to_lonlat(v.center.x, v.center.y))
}

/// Great-circle-ish ground distance (metres) between two lon/lat points, close range.
fn ground_distance(lon0: f64, lat0: f64, lon1: f64, lat1: f64) -> f64 {
    let k = lat0.to_radians().cos().max(0.01);
    let dx = (lon1 - lon0) * 111_320.0 * k;
    let dy = (lat1 - lat0) * 110_574.0;
    (dx * dx + dy * dy).sqrt()
}

/// The pick ray through a screen point on the canvas.
fn ray_at(cam: &OrbitCamera, rect: Rect, p: Pos2) -> Ray {
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    let ndc = [((p.x - rect.left()) / rect.width()) * 2.0 - 1.0, -(((p.y - rect.top()) / rect.height()) * 2.0 - 1.0)];
    Ray::from_ndc(cam, ndc, aspect)
}

/// The world point a screen position picks on the ground: the flat datum plane (the elevation
/// mesh is gone — the ground drapes flat, so the plane is the ground). `None` when the ray
/// never crosses it (looking at the sky).
fn ground_pick(app: &TerrazoneApp, cam: &OrbitCamera, rect: Rect, p: Pos2) -> Option<Vec3> {
    let _ = app;
    let ray = ray_at(cam, rect, p);
    ray.plane_y(0.0)
}

// ============================== Fleet markers ==============================

/// The flat hex base under a fleet marker (the H3 cell motif), one tint, decal layer. Sized by
/// the marine ladder's metres; origin at the ground centre; the heading does not rotate it (a
/// cell is a cell).
fn fleet_hex_prim(color: [f32; 3], span: f32) -> Mesh3d {
    let mut mesh = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Decal);
    mesh.z_bias = 0.04;
    let mut ring: Vec<u32> = Vec::with_capacity(terrazon_fleet::icons::HEX.len());
    for (ix, iy) in terrazon_fleet::icons::HEX {
        ring.push(mesh.vertex([ix * span, 0.05, -iy * span], color));
    }
    let n = ring.len();
    for i in 0..n.saturating_sub(2) {
        mesh.tri(ring[0], ring[i + 1], ring[i + 2]);
    }
    mesh
}

/// The standing block over the hex: the vehicle's own silhouette mass, sized by the marine
/// ladder (the same class the 2D icon uses), origin at the ground centre, nose north (−z) —
/// `draw_fleet` rotates it to the heading.
fn fleet_block_prim(kind: terrazon_fleet::VehicleKind, class: usize, color: [f32; 3]) -> Mesh3d {
    let l = terrazon_fleet::sizing::metres(class).max(3.0);
    let (w, len, h) = match kind {
        terrazon_fleet::VehicleKind::Car => (0.45 * l, l, 0.32 * l),
        terrazon_fleet::VehicleKind::Van => (0.50 * l, 1.10 * l, 0.50 * l),
        terrazon_fleet::VehicleKind::Motorcycle => (0.18 * l, 0.55 * l, 0.22 * l),
        terrazon_fleet::VehicleKind::Boat => (0.32 * l, 1.00 * l, 0.10 * l),
        terrazon_fleet::VehicleKind::Ship => (0.28 * l, 1.15 * l, 0.12 * l),
        terrazon_fleet::VehicleKind::Plane => (1.05 * l, 0.90 * l, 0.14 * l),
        terrazon_fleet::VehicleKind::PlaneHeavy => (1.25 * l, 0.95 * l, 0.16 * l),
        terrazon_fleet::VehicleKind::PlaneLight => (0.85 * l, 0.75 * l, 0.13 * l),
        terrazon_fleet::VehicleKind::Helicopter => (0.55 * l, 0.75 * l, 0.24 * l),
        terrazon_fleet::VehicleKind::Train => (0.42 * l, 1.30 * l, 0.40 * l),
    };
    let lighter = color.map(|c| (c * 1.18).min(1.0));
    let mut m = terrazon_scene3d::mesh::box_between([-w / 2.0, 0.06, -len / 2.0], [w / 2.0, 0.06 + h, len / 2.0], color);
    // A lighter cap so the kind reads from above (the roof).
    m.positions.extend([
        [-w / 2.0, 0.06 + h, -len / 2.0 + 0.15 * len],
        [w / 2.0, 0.06 + h, -len / 2.0 + 0.15 * len],
        [w / 2.0, 0.06 + h, len / 2.0 - 0.15 * len],
        [-w / 2.0, 0.06 + h, len / 2.0 - 0.15 * len],
    ]);
    m.colors.extend([lighter; 4]);
    let b = m.positions.len() as u32;
    m.quad(b - 4, b - 3, b - 2, b - 1);
    m.z_bias = 0.02;
    m
}

/// The fleet pass: the live feeds' markers (and the demo's) standing on the flat draped ground —
/// the same cache and the same document gating the 2D map draws, so both canvases show one fleet.
/// Flyers (the flight kinds) **project their altitude**: the vendor's own alt (feet → metres)
/// lifts the marker above the datum and a thin leader line drops to a ground ring, so an
/// aircraft reads at height the way the height map puts it there — not parked on the street.
fn draw_fleet(app: &mut TerrazoneApp, buckets: &mut [Vec<Tri>; 3], drawn: &mut usize, xform: &Xform) {
    if !crate::fleet::has_visible(app) {
        return;
    }
    let Some(doc) = app.session.active().map(|st| st.doc.clone()) else { return };
    let to_f32 = |c: egui::Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0];
    // Collect the jobs (vehicle, tint, alpha, hex) from the runtime cache before the runtime
    // borrow. The registry gates here too: a feed draws when its part rows' eyes say so, the
    // hex base rides the "H3 Cells" row the way the 2D map's cells do.
    let mut jobs: Vec<(terrazon_fleet::Vehicle, [f32; 3], f32, bool)> = Vec::new();
    {
        let cache = &app.canvas.fleet;
        for l in &doc.fleet.layers {
            if !l.visible || !l.auth.is_set() || !cache.status_of(l.provider).has_data() {
                continue;
            }
            let label = l.provider.label();
            if !crate::fleet::part_visible(&doc, label, crate::fleet::vehicles_part(l.provider)) {
                continue;
            }
            let hex_on = crate::fleet::part_visible(&doc, label, "H3 Cells");
            let [r, g, b] = l.provider.color();
            let tint = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0];
            for v in cache.markers_of(l.provider) {
                jobs.push((v.clone(), tint, 0.9, hex_on));
            }
        }
        if doc.fleet.demo && doc.fleet.demo_visible && crate::fleet::part_visible(&doc, "Demo", "Markers") {
            let hex_on = crate::fleet::part_visible(&doc, "Demo", "H3 Cells");
            for v in &cache.demo_markers {
                // The demo's kinds keep their type colours in 3D (the same mapping the 2D
                // map paints), so the demo fleet reads like the live one.
                jobs.push((v.clone(), to_f32(crate::fleet::kind_color(v.kind)), 0.85, hex_on));
            }
        }
    }
    let m = scene_mercator(app);
    // A frame-local prim cache: every marker of one kind/class/tint shares the meshes.
    let mut cache: HashMap<String, Mesh3d> = HashMap::new();
    for (v, color, alpha, hex_on) in jobs {
        let class = terrazon_fleet::sizing::class_of(v.kind, v.length_m);
        let span = terrazon_fleet::sizing::metres(class) * 1.15;
        let shade = ((color[0] * 255.0) as u8) as u16;
        let hex_key = format!("hex/{:?}/{class}/{shade}", v.kind);
        let block_key = format!("block/{:?}/{class}/{shade}", v.kind);
        let [x, z] = {
            let (wx, yn) = m.project(v.lon, v.lat);
            [wx as f32, -(yn as f32)]
        };
        let hex = cache.entry(hex_key).or_insert_with(|| fleet_hex_prim(color, span)).clone();
        let block = cache.entry(block_key).or_insert_with(|| fleet_block_prim(v.kind, class, color)).clone();
        // The hex doesn't rotate (a cell is a cell); the block turns to the heading: the
        // placements' clockwise-from-above convention, nose −z → east at 90°.
        let (s, c) = v.heading_deg.max(0.0).to_radians().sin_cos();
        let mut turned = block.clone();
        if v.heading_deg >= 0.0 {
            for p in turned.positions.iter_mut() {
                let (px, pz) = (p[0], p[2]);
                *p = [px * c - pz * s, p[1], px * s + pz * c];
            }
        }
        let _ = shade;
        // The altitude: flyers ride their reported metres (the vendor's own alt; the demo's
        // cruise), everything else sits on the datum. The ground ring stays below either way
        // — the marker's anchor on the map — dimmed once the body is up at height.
        let lift = if v.kind.flies() { v.alt_m.unwrap_or(0.0).clamp(0.0, 20_000.0) } else { 0.0 };
        let ground_alpha = if lift > span { alpha * 0.45 } else { alpha };
        if hex_on {
            push_mesh(buckets, drawn, &hex, [x, 0.0, z], 0.0, ground_alpha, xform);
        }
        push_mesh(buckets, drawn, &turned, [x, lift, z], 0.0, alpha, xform);
        if lift > span {
            // The plumb line: four triangles, built per flyer (cheaper than a cache keyed
            // by altitude).
            let leader = fleet_leader_prim(lift);
            push_mesh(buckets, drawn, &leader, [x, 0.0, z], 0.0, alpha * 0.55, xform);
        }
    }
}

/// The leader line under an elevated marker: a hair-thin crossed quad pair from the datum to
/// the marker's altitude, light grey — the plumb line the eye reads up to the aircraft and
/// back down to the ground ring it belongs to.
fn fleet_leader_prim(height: f32) -> Mesh3d {
    let mut mesh = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Decal);
    mesh.z_bias = 0.02;
    let color = [0.42, 0.44, 0.47];
    let w = 0.45f32;
    for (ax, az, bx, bz) in [(w, 0.0, -w, 0.0), (0.0, w, 0.0, -w)] {
        let a = mesh.vertex([ax, 0.0, az], color);
        let b = mesh.vertex([bx, 0.0, bz], color);
        let c = mesh.vertex([bx, height, bz], color);
        let d = mesh.vertex([ax, height, az], color);
        mesh.tri(a, b, c);
        mesh.tri(a, c, d);
    }
    mesh
}

/// Entering the 3D environment from the menu/switch: the status hint (the runtime itself starts
/// on the first 3D frame).
pub fn enter_3d(app: &mut TerrazoneApp) {
    enter_3d_status(app);
}

/// Render the 3D canvas environment over `rect` (the whole canvas area). Called by
/// [`crate::canvas::show`] when the document's view is in 3D mode.
pub fn show_3d(app: &mut TerrazoneApp, ui: &mut egui::Ui, rect: Rect) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let resp = ui.interact(rect, egui::Id::new("canvas3d"), Sense::click_and_drag());
    let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
    handle_input(app, ui, &resp, rect, hover);
    // The eased dolly glides toward the wheel's target before the frame renders (the direct
    // writes — a scene3d.set camDist, the first framing — clear the target instead).
    if app.ui.cam3d.ease_dolly() {
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }
    // The frame's projection, built before the DEM/imagery step so the imagery ring covers
    // the ground this exact camera sees (a tilted view reaches toward the horizon). The
    // render path rebuilds it after the step (a re-anchor moves the camera target there).
    let cam_pre = app.ui.cam3d;
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    dem_step(app, ui.ctx(), Some(&Xform { vp: cam_pre.proj(aspect) * cam_pre.view(), rect }));

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, SKY);

    let interval = app.ui.terrain3d.contour_interval;
    let cam = app.ui.cam3d;
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    let xform = Xform { vp: cam.proj(aspect) * cam.view(), rect };

    // The pointer's ground pick feeds the status bar's coordinate readout (the same doc-point
    // hover the 2D canvas sets).
    app.hover_doc = hover.and_then(|h| ground_to_doc(app, rect, h));

    // The ground slab first (under everything): the flat ground the imagery drapes onto.
    draw_slab(&painter, &xform, SLAB_HALF, [0.55, 0.53, 0.48]);

    // The imagery drape (the 2D basemap under the 3D scene), flat on the datum: the ground
    // reads exactly like the 2D map, in perspective.
    draw_imagery(app, &painter, &xform);
    draw_loading_hint(app, &painter, &t, rect);

    // The ground grid (the position aid): metre lines around the scene's position, under the
    // contours and the art.
    draw_ground_grid(&painter, &xform, &cam);

    let mut buckets: [Vec<Tri>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut drawn = 0usize;

    // Contours (the Decal position: on the ground, under everything standing) as thin flat
    // quads at the datum, the major lines carrying their height as text along the path.
    let contour_data = {
        let base = app.canvas.dem.as_ref().map_or(0.0, |r| r.base);
        app.canvas.dem.as_mut().and_then(|rt| rt.contour_lines(interval, base)).map(|cs| (cs.clone(), base))
    };
    if let Some((cs, _base)) = contour_data.as_ref() {
        draw_contours(&mut buckets[1], &mut drawn, cs, &xform);
    }
    flush_bucket(&buckets[1], &painter);
    buckets[1].clear();
    if let Some((cs, base)) = contour_data.as_ref() {
        draw_contour_labels(&painter, &xform, cs, *base);
    }

    // The document's own drawing, draped on the flat ground: every visible path node's
    // topmost stroke and its fill — the fills painted the way the 2D canvas paints them.
    draw_doc_art(app, &painter, &xform);

    // The document's placed assets (Standing), each on its own ground point: the doc point
    // names the lon/lat (the same space every tool draws in), the ground is the datum.
    let placements = app.session.active().map(|st| st.doc.placements.placements.clone()).unwrap_or_default();
    if !placements.is_empty() {
        let m = scene_mercator(app);
        for p in &placements {
            let mesh = match app.canvas.dem.as_mut() {
                Some(rt) => rt.prim_of(p),
                None => build_prim(p.kind, p.scale),
            };
            let (lon, lat) = doc_to_lonlat(p.x, p.y);
            let (x, yn) = m.project(lon, lat);
            let at = [x as f32, 0.0, -(yn as f32)];
            push_placed(&mut buckets, &mut drawn, &mesh, p, at, &xform);
            push_blob_shadow(&mut buckets[1], &mut drawn, &xform, at, p.size_m(), &m);
        }
    }

    // The placement ghost: while the Assets tool holds the canvas, the asset it would place
    // draws translucent at the pointer's ground point.
    let ghost = assets_tool_kind(app).filter(|_| hover.is_some()).map(|kind| {
        hover.and_then(|h| ground_pick(app, &cam, rect, h)).map(|hit| {
            let mesh = match app.canvas.dem.as_mut() {
                Some(rt) => rt.ghost_of(kind),
                None => build_prim(kind, 1.0),
            };
            (kind, mesh, hit)
        })
    });
    if let Some(Some((kind, mesh, hit))) = ghost {
        push_mesh(&mut buckets, &mut drawn, &mesh, [hit.x, hit.y, hit.z], 0.0, 0.6, &xform);
        draw_ghost_ring(&painter, &xform, hit, kind);
    }

    // The fleet's live markers: the same feeds the 2D map draws, standing on the ground.
    draw_fleet(app, &mut buckets, &mut drawn, &xform);

    flush_bucket(&buckets[2], &painter);

    // The tools' overlays (previews, anchors, marquees) over everything — projected onto the
    // ground like the art, so drawing reads where it lands.
    let overlays = app.session.overlays(app.view_info());
    if !overlays.is_empty() {
        let drape = drape_ctx(app);
        let tol = drape_tol(app, rect);
        draw_overlays_3d(&painter, &xform, &drape, tol, &overlays);
    }

    // The position aids over everything: the X/Y/Z axis gizmo at the scene's position, and
    // the big cursor marker under the pointer's ground pick.
    draw_axis_gizmo(&painter, &xform, cam.target, cam.dist);
    if let Some(h) = hover
        && let Some(hit) = ground_pick(app, &cam, rect, h)
    {
        draw_hover_cursor(&painter, &xform, hit, cam.dist);
    }

    if drawn > TRI_BUDGET {
        let text = crate::i18n::fmt(tl!("Triangle budget reached — showing the closest {n} tris"), &[("n", &TRI_BUDGET.to_string())]);
        let font = FontId::proportional(11.0);
        let galley = painter.layout_no_wrap(text, font, Color32::from_rgb(140, 90, 20));
        let pos = pos2(rect.left() + 10.0, rect.top() + 44.0);
        painter.rect_filled(Rect::from_min_size(pos - vec2(3.0, 2.0), galley.size() + vec2(6.0, 4.0)), 3.0, Color32::from_black_alpha(96));
        painter.galley(pos, galley, Color32::from_rgb(140, 90, 20));
    }

    draw_readout(app, &painter, rect, hover);
    draw_canvas_switch(app, ui, rect);
}

/// Push one [`Mesh3d`] through the projection into the buckets: every triangle shaded, depth-
/// sorted (per bucket), near-plane rejected and degenerate-triangle skipped. `at` translates the
/// mesh's origin, `alpha` < 1 draws it translucent (the ghost).
fn push_mesh(buckets: &mut [Vec<Tri>; 3], drawn: &mut usize, mesh: &Mesh3d, at: [f32; 3], z_lift: f32, alpha: f32, xform: &Xform) {
    let bucket = mesh.layer as usize;
    let rot = 0.0f32; // placements' rotation is applied by `push_placed` before this via `at`
    let _ = rot;
    let world = |p: [f32; 3]| [p[0] + at[0], p[1] + at[1] + z_lift, p[2] + at[2]];
    let indices = if mesh.indices.is_empty() { (0..mesh.positions.len() as u32).collect::<Vec<u32>>() } else { mesh.indices.clone() };
    for tri in indices.as_chunks::<3>().0 {
        if *drawn > TRI_BUDGET {
            return;
        }
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (pa, pb, pc) = match (xform.proj(world(mesh.positions[i0])), xform.proj(world(mesh.positions[i1])), xform.proj(world(mesh.positions[i2])))
        {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        // Degenerate / sub-pixel triangles cost more than they show.
        let area = (pb.x - pa.x) * (pc.y - pa.y) - (pc.x - pa.x) * (pb.y - pa.y);
        if area.abs() < 0.15 {
            continue;
        }
        let a = world(mesh.positions[i0]);
        let b = world(mesh.positions[i1]);
        let c = world(mesh.positions[i2]);
        let base = mesh.colors.get(i0).copied().unwrap_or([0.7; 3]);
        let mut col = Xform::shade(base, normal_of(a, b, c));
        if alpha < 0.999 {
            col = col.gamma_multiply(alpha);
        }
        let cen = Vec3::from(a) + Vec3::from(b) + Vec3::from(c);
        let depth = (xform.vp.project_point3(cen / 3.0) - Vec3::ZERO).length() + mesh.z_bias * 0.05;
        buckets[bucket].push(Tri { depth, a: pa, b: pb, c: pc, color: col });
        *drawn += 1;
    }
}

/// Push a placement's prim mesh on the ground at its lon/lat, rotated about `y`.
fn push_placed(buckets: &mut [Vec<Tri>; 3], drawn: &mut usize, mesh: &Mesh3d, p: &AssetPlacement, at: [f32; 3], xform: &Xform) {
    let theta = p.rotation_deg.to_radians();
    let (s, c) = theta.sin_cos();
    let mut rot = mesh.clone();
    for v in rot.positions.iter_mut() {
        // Clockwise seen from above (the Placement doc's convention): east turns toward south.
        *v = [v[0] * c - v[2] * s + at[0], v[1] + at[1], v[0] * s + v[2] * c + at[2]];
    }
    push_mesh(buckets, drawn, &rot, [0.0, 0.0, 0.0], 0.0, 1.0, xform);
}

fn normal_of(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Vec3 {
    (Vec3::from(b) - Vec3::from(a)).cross(Vec3::from(c) - Vec3::from(a))
}

/// Flush one bucket: sort back-to-front by view depth, emit one egui mesh (the painter has no
/// z-buffer; this is the ArchCraft flush).
fn flush_bucket(bucket: &[Tri], painter: &egui::Painter) {
    let mut tris = bucket.to_vec();
    tris.sort_by(|p, q| q.depth.total_cmp(&p.depth));
    let mut mesh = egui::Mesh::default();
    for t in tris {
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(t.a, t.color);
        mesh.colored_vertex(t.b, t.color);
        mesh.colored_vertex(t.c, t.color);
        mesh.add_triangle(base, base + 1, base + 2);
    }
    if !mesh.is_empty() {
        painter.add(Shape::mesh(std::sync::Arc::new(mesh)));
    }
}

/// The flat ground slab at the datum (drawn first, under everything).
fn draw_slab(painter: &egui::Painter, xform: &Xform, half: f32, color: [f32; 3]) {
    let mut mesh = egui::Mesh::default();
    let corners = [(-half, -half), (half, -half), (half, half), (-half, half)];
    for (x, z) in corners {
        if let Some(p) = xform.proj([x, 0.0, z]) {
            mesh.colored_vertex(p, Xform::shade(color, Vec3::Y));
        }
    }
    if mesh.vertices.len() == 4 {
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(std::sync::Arc::new(mesh)));
    }
}

/// The DEM and imagery lifecycle, ticked once per 3D frame.
fn dem_step(app: &mut TerrazoneApp, ctx: &egui::Context, xform: Option<&Xform>) {
    let mut centre = view_centre_lonlat(app);
    let fetcher = app.services.dem.clone();
    if app.canvas.dem.is_none() {
        // Entering 3D from a wide map view: the ~40 km DEM window means nothing at a world-scale
        // view centre (a fresh World Map centres on (0°, 0°) — open ocean, and the user just sees
        // water). When the map view is wider than the window's scale and the document has drawn
        // content, anchor at the content's centre — the 2D view centres there too, so both
        // canvases show the same place. With no content the view centre stands: the datum clamp
        // keeps open water sane and the first grid's status hint explains the anchor.
        let wide = app.view().is_some_and(|v| crate::state::map_zoom_level(v.zoom) < 10.0);
        let content_centre = if wide {
            app.session
                .active()
                .filter(|st| st.doc.map.source().is_some())
                .and_then(|st| st.doc.art_bounds())
                .filter(|b| b.width() > 1.0 && b.height() > 1.0)
                .map(|b| b.center())
        } else {
            None
        };
        if let (Some(c), Some(v)) = (content_centre, app.view_mut()) {
            v.center = c;
            centre = view_centre_lonlat(app);
        }
        app.canvas.dem = Some(DemRuntime::new(centre, fetcher.as_deref()));
    }

    // The view centre wandered past the rebuild threshold: re-anchor the scene there (the camera
    // target rides along so the view doesn't jump) and re-ask the DEM window.
    let moved = app.canvas.dem.as_ref().is_some_and(|rt| {
        let (lon, lat) = rt.anchor;
        ground_distance(lon, lat, centre.0, centre.1) > DEM_MOVE_M
    });
    if moved {
        reanchor(app, fetcher.as_deref());
    }

    // 1. DEM answers: tick the loader, re-stitch when its ready count changed.
    let mut water_hint = false;
    let dem_busy = {
        let Some(rt) = app.canvas.dem.as_mut() else { return };
        if let Some(loader) = rt.loader.as_mut() {
            let _ = loader.tick();
            let ready = loader.ready_count();
            if ready != rt.ready_seen {
                rt.ready_seen = ready;
                if let Some(grid) = loader.stitch() {
                    // The datum is the surface the site stands on: the sample's own grade on
                    // land, the sea surface when the anchor is open water (bathymetric DEMs read
                    // the Gulf of Guinea at −5.5 km, which hung the whole scene kilometres over
                    // the camera and left the view a bare slab with a sliver of imagery).
                    let here = grid.sample(rt.anchor.0, rt.anchor.1);
                    let water = here.is_some_and(|e| e < OPEN_WATER_ELEV);
                    rt.base = match here {
                        Some(e) if !water => e,
                        Some(_) => 0.0,
                        None => grid.min_elev().max(0.0),
                    };
                    if water && !rt.water_hinted {
                        rt.water_hinted = true;
                        water_hint = true;
                    }
                    rt.contours = None;
                    rt.grid = Some(grid);
                }
            }
            loader.pending_count()
        } else {
            0
        }
    };
    if water_hint {
        app.status("3D anchors the map view centre — open water here; zoom the 2D map to an area to bring it into 3D".to_string());
    }
    // 2. Imagery: the basemap tiles the 3D ground covers, on the imagery runtime's own channel.
    //    The drape's zoom follows the camera: far it stays at the setting, close it refines, so
    //    dollying in brings sharper ground instead of stretching the same pixels.
    let base_zoom = app.ui.terrain3d.imagery_zoom;
    let cam_dist = app.ui.cam3d.dist;
    let (source, pinned) = app
        .session
        .active()
        .map(|st| {
            let src = st.doc.map.visible.then(|| st.doc.map.source().cloned()).flatten();
            let pins: Vec<terrazon_map::TileLayer> = st.doc.map.tile_layers.clone();
            (src, pins)
        })
        .unwrap_or((None, Vec::new()));
    let imagery_fetcher = app.services.tile_fetch.clone();
    let frame = ImageryFrame { source: source.as_ref(), base_zoom, cam_dist, xform };
    let imagery_busy = app.canvas.dem.as_mut().is_some_and(|rt| imagery_step(rt, frame, imagery_fetcher.as_deref(), ctx, &pinned));

    if dem_busy > 0 || imagery_busy {
        ctx.request_repaint_after(Duration::from_millis(120));
    }
}

/// Re-anchor the scene at the view's centre: a new DEM window, a new Mercator origin, the camera
/// target reprojected so the view stays where it was.
fn reanchor(app: &mut TerrazoneApp, fetcher: Option<&dyn terrazon_scene3d::DemFetcher>) {
    let centre = view_centre_lonlat(app);
    let Some(rt) = app.canvas.dem.as_mut() else { return };
    let old = Mercator::new(rt.anchor.1, rt.anchor.0);
    // Where the old camera target sat on the ground, in the new frame.
    let target = app.ui.cam3d.target;
    let (tlon, tlat) = old.unproject(f64::from(target[0]), f64::from(-target[2]));
    rt.anchor = centre;
    rt.ready_seen = 0;
    rt.grid = None;
    rt.contours = None;
    rt.base = 0.0;
    rt.water_hinted = false;
    rt.loader = fetcher.map(|f| DemLoader::new(f, centre.0, centre.1, DEM_ZOOM));
    let new = Mercator::new(centre.1, centre.0);
    let [nx, nz] = {
        let (x, yn) = new.project(tlon, tlat);
        [x as f32, -(yn as f32)]
    };
    app.ui.cam3d.target = [nx, target[1], nz];
    if let Some(rt) = app.canvas.dem.as_mut() {
        rt.imagery.asked = None;
    }
}

/// The drape's zoom for the camera's distance: the panel's setting at neighbourhood range, one
/// step finer inside ~600 m, two inside ~150 m (a street-level camera reads street-level
/// tiles); backed off coarser when far, so a high orbit doesn't fetch pixels it can't show.
/// Clamped into the source's own zoom range.
pub(crate) fn drape_zoom(base: u8, dist: f32, source: &terrazon_map::TileSource) -> u8 {
    let step = if dist > 8_000.0 {
        -2
    } else if dist > 2_500.0 {
        -1
    } else if dist > 600.0 {
        0
    } else if dist > 150.0 {
        1
    } else {
        2
    };
    let z = i32::from(base) + step;
    z.clamp(i32::from(source.min_zoom), i32::from(source.max_zoom)) as u8
}

/// The ring's radius and cap for a drape zoom `step` from the base: finer tiles cover less
/// ground, so a closer ring asks wider (still centre-out, still capped).
pub(crate) fn ring_shape(step: i32) -> (i64, usize) {
    match step {
        1 => (3, 36),
        2 => (4, 49),
        _ => (IMAGERY_RING, IMAGERY_CAP),
    }
}

/// What one imagery step reads from the frame: the drape's zoom inputs and the frame's own
/// projection — the ring covers the projected ground when a frame is present, and falls
/// back to the square ring around the anchor without one.
struct ImageryFrame<'a> {
    source: Option<&'a terrazon_map::TileSource>,
    base_zoom: u8,
    cam_dist: f32,
    xform: Option<&'a Xform>,
}

/// Ask for and collect the imagery tiles around the scene centre. Returns whether any ask is
/// still in flight (so the frame keeps repainting until the ground is dressed).
fn imagery_step(
    rt: &mut DemRuntime,
    frame: ImageryFrame<'_>,
    fetcher: Option<&dyn TileFetcher>,
    ctx: &egui::Context,
    pinned: &[terrazon_map::TileLayer],
) -> bool {
    // Textures belong to the context they were uploaded to; a new one starts the ring over.
    let tag = crate::graphics::context_tag(ctx);
    if rt.imagery.tag != tag {
        rt.imagery.tiles.clear();
        rt.imagery.asked = None;
        rt.imagery.tag = tag;
    }
    let Some(source) = frame.source else { return false };
    let Some(fetcher) = fetcher else { return false };
    // The pinned tile layers: each asks its one tile from its own source when it hasn't one
    // pending yet (their textures key by tile-layer id, never colliding with the ring's).
    if !pinned.is_empty() {
        for tl in pinned {
            if !tl.visible || tl.opacity <= 0.0 {
                continue;
            }
            let tile = TileId::new(tl.z, tl.x, tl.y);
            if !tile.valid() || rt.imagery.pinned.contains_key(&tl.id) {
                continue;
            }
            rt.imagery.pinned.insert(tl.id.clone(), ImageryTile { state: ImageryState::Pending });
            let url = terrazon_map::build_tile_url(source, u32::from(tl.z), tl.x, tl.y);
            fetcher.fetch(format!("pinned/{}/{}", tl.id, tile_key(tile)), url, rt.imagery.tx.clone());
        }
    }
    let z = drape_zoom(frame.base_zoom, frame.cam_dist, source);
    let (cx, cy) = terrazon_scene3d::mercator::lonlat_to_tile(rt.anchor.0, rt.anchor.1, u32::from(z));
    // The ring the camera actually covers: from the frame's projection, the screen's corners
    // and edges unproject onto the datum — a tilted view reaches far toward the horizon, and
    // the ring covers that whole reach (the old fixed square showed a bare far field and
    // stretched its few tiles over it). Without a frame (no 3D canvas yet) the square ring
    // around the anchor stands in.
    let ring = imagery_ring_footprint(rt, frame.xform, z).unwrap_or_else(|| {
        let step = i32::from(z) - i32::from(frame.base_zoom);
        let (radius, cap) = ring_shape(step);
        imagery_ring(z, cx, cy, radius, cap)
    });
    // The ask's fingerprint: zoom, centre tile and the ring's extents (a tilt or pan that
    // changes the footprint re-asks even when the centre tile stays).
    let (min_x, max_x, min_y, max_y) =
        ring.iter().fold((u32::MAX, 0u32, u32::MAX, 0u32), |(x0, x1, y0, y1), t| (x0.min(t.x), x1.max(t.x), y0.min(t.y), y1.max(t.y)));
    if rt.imagery.asked != Some((z, min_x, max_x, min_y, max_y)) {
        // A new ring: keep the ring's tiles and their ancestors (they dress the ring while it
        // loads), forget the rest, ask the ring (centre out, capped).
        let mut keep = std::collections::HashSet::new();
        for t in &ring {
            keep.insert(tile_key(*t));
            for dz in 1..=2 {
                if let Some((anc, _, _)) = crate::tiles::ancestor_uv(*t, dz) {
                    keep.insert(tile_key(anc));
                }
            }
        }
        rt.imagery.tiles.retain(|key, _| keep.contains(key));
        rt.imagery.ring = ring.clone();
        rt.imagery.asked = Some((z, min_x, max_x, min_y, max_y));
        for t in &ring {
            let key = tile_key(*t);
            if let Some(ImageryTile { state: ImageryState::Failed(at) }) = rt.imagery.tiles.get(&key)
                && Instant::now() < *at
            {
                continue;
            }
            if !rt.imagery.tiles.contains_key(&key) {
                rt.imagery.tiles.insert(key.clone(), ImageryTile { state: ImageryState::Pending });
                let url = terrazon_map::build_tile_url(source, u32::from(t.z), t.x, t.y);
                fetcher.fetch(format!("{}/{}/{}/{}", source.id, t.z, t.x, t.y), url, rt.imagery.tx.clone());
            }
        }
    }
    // The answers: decode (neutral style — the 3D drape shows the map as it is), upload, keep.
    let mut answered = 0;
    while let Ok(msg) = rt.imagery.rx.try_recv() {
        answered += 1;
        if answered > 32 {
            break;
        }
        match msg {
            TileMsg::Done { key, bytes } => {
                // A pinned layer's answer keys by its tile-layer id.
                if let Some(rest) = key.strip_prefix("pinned/") {
                    let lid = rest.split('/').next().unwrap_or("").to_string();
                    let state = match image::load_from_memory(&bytes) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let size = [rgba.width() as usize, rgba.height() as usize];
                            let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                            ImageryState::Ready(ctx.load_texture(format!("3d-pinned-{key}"), image, TextureOptions::LINEAR))
                        }
                        Err(_) => ImageryState::Failed(Instant::now() + IMAGERY_RETRY),
                    };
                    rt.imagery.pinned.insert(lid, ImageryTile { state });
                    continue;
                }
                // The ask's key is `{source}/{z}/{x}/{y}`; the runtime stores by `{z}/{x}/{y}`.
                let tile_key = key.split('/').skip(1).collect::<Vec<_>>().join("/");
                let state = match image::load_from_memory(&bytes) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                        ImageryState::Ready(ctx.load_texture(format!("3d-tile-{key}"), image, TextureOptions::LINEAR))
                    }
                    Err(_) => ImageryState::Failed(Instant::now() + IMAGERY_RETRY),
                };
                rt.imagery.tiles.insert(tile_key, ImageryTile { state });
            }
            TileMsg::Failed { key } | TileMsg::Dropped { key } => {
                if let Some(rest) = key.strip_prefix("pinned/") {
                    let lid = rest.split('/').next().unwrap_or("").to_string();
                    rt.imagery.pinned.insert(lid, ImageryTile { state: ImageryState::Failed(Instant::now() + IMAGERY_RETRY) });
                    continue;
                }
                let tile_key = key.split('/').skip(1).collect::<Vec<_>>().join("/");
                rt.imagery.tiles.insert(tile_key, ImageryTile { state: ImageryState::Failed(Instant::now() + IMAGERY_RETRY) });
            }
        }
    }
    rt.imagery.tiles.values().any(|t| matches!(t.state, ImageryState::Pending))
        || rt.imagery.pinned.values().any(|t| matches!(t.state, ImageryState::Pending))
}

/// The tile ring around `(cx, cy)` at zoom `z`, nearest first, capped.
fn imagery_ring(z: u8, cx: u32, cy: u32, radius: i64, cap: usize) -> Vec<TileId> {
    let n = 1u64 << z;
    let mut ring: Vec<(i64, TileId)> = Vec::new();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = (i64::from(cx) + dx).rem_euclid(n as i64) as u32;
            let y = (i64::from(cy) + dy).clamp(0, n as i64 - 1) as u32;
            ring.push((dx * dx + dy * dy, TileId::new(z, x, y)));
        }
    }
    ring.sort_by_key(|(d, _)| *d);
    ring.truncate(cap);
    ring.into_iter().map(|(_, t)| t).collect()
}

/// How far from the anchor a footprint sample may land (metres): past this the imagery is
/// a smear anyway, and a near-horizon ray would otherwise run to the map's edge.
const FOOTPRINT_REACH_M: f32 = 30_000.0;
/// The footprint ring's cap (the widest tilted view needs more tiles than the square's 24,
/// still a fraction of what a full screen-quadtree would ask).
const FOOTPRINT_CAP: usize = 96;

/// The imagery ring from the frame's own ground footprint: the screen's corners and edge
/// midpoints unproject onto the datum (`Xform::ground_pick`), the reach clamps around the
/// anchor, and the tiles covering that ground box — one ring finer-padded — answer nearest
/// first, capped. `None` when the frame gives no ground (all sky, or no frame at all): the
/// caller falls back to the square ring.
fn imagery_ring_footprint(rt: &DemRuntime, xform: Option<&Xform>, z: u8) -> Option<Vec<TileId>> {
    let xf = xform?;
    let m = Mercator::new(rt.anchor.1, rt.anchor.0);
    let r = xf.rect;
    let samples = [
        Pos2::new(r.left(), r.top()),
        Pos2::new(r.center().x, r.top()),
        Pos2::new(r.right(), r.top()),
        Pos2::new(r.right(), r.center().y),
        Pos2::new(r.right(), r.bottom()),
        Pos2::new(r.center().x, r.bottom()),
        Pos2::new(r.left(), r.bottom()),
        Pos2::new(r.left(), r.center().y),
        r.center(),
    ];
    // Ground the samples, reach-clamped around the anchor (the world frame's origin).
    let ground: Vec<(f64, f64)> = samples
        .iter()
        .filter_map(|s| {
            xf.ground_pick(*s).map(|g| {
                let len = (g[0] * g[0] + g[2] * g[2]).sqrt();
                let clamped = if len > FOOTPRINT_REACH_M { [g[0] / len * FOOTPRINT_REACH_M, g[2] / len * FOOTPRINT_REACH_M] } else { [g[0], g[2]] };
                m.unproject(f64::from(clamped[0]), f64::from(-clamped[1]))
            })
        })
        .collect();
    if ground.len() < 3 {
        return None;
    }
    let (min_lon, max_lon, min_lat, max_lat) = ground
        .iter()
        .fold((f64::MAX, f64::MIN, f64::MAX, f64::MIN), |(x0, x1, y0, y1), (lon, lat)| (x0.min(*lon), x1.max(*lon), y0.min(*lat), y1.max(*lat)));
    // The tile box the ground covers, one tile padded; rows clamp into the web-mercator
    // band (y grows southward: the north edge names the small row, the south the big one).
    let n = 1u64 << z;
    let (tx_west, ty_north) = terrazon_scene3d::mercator::lonlat_to_tile(min_lon, max_lat.min(85.0), u32::from(z));
    let (tx_east, ty_south) = terrazon_scene3d::mercator::lonlat_to_tile(max_lon, min_lat.max(-85.0), u32::from(z));
    let tx0 = tx_west.min(tx_east).saturating_sub(1);
    let tx1 = tx_west.max(tx_east).saturating_add(1);
    let ty0 = ty_north.min(ty_south).saturating_sub(1);
    let ty1 = ty_north.max(ty_south).saturating_add(1);
    let (cx, cy) = terrazon_scene3d::mercator::lonlat_to_tile(rt.anchor.0, rt.anchor.1, u32::from(z));
    let last_row = n as u32 - 1;
    let mut ring: Vec<(i64, TileId)> = Vec::new();
    for ty in ty0..=ty1.min(last_row) {
        for tx in tx0..=tx1 {
            let x = (u64::from(tx) % n) as u32;
            let (dx, dy) = (i64::from(tx) - i64::from(cx), i64::from(ty) - i64::from(cy));
            ring.push((dx * dx + dy * dy, TileId::new(z, x, ty)));
        }
    }
    if ring.is_empty() {
        return None;
    }
    ring.sort_by_key(|(d, _)| *d);
    ring.truncate(FOOTPRINT_CAP);
    Some(ring.into_iter().map(|(_, t)| t).collect())
}

fn tile_key(t: TileId) -> String {
    format!("{}/{}/{}", t.z, t.x, t.y)
}

/// The imagery textures on the ground: the current ring, every ready tile draped FLAT on the
/// datum (the elevation mesh is gone — the ground reads like the 2D map); a tile still loading
/// stretches its nearest ready ancestor's texture over its own quad (the 2D cache's parent
/// fallback).
fn draw_imagery(app: &mut TerrazoneApp, painter: &egui::Painter, xform: &Xform) {
    let Some(rt) = app.canvas.dem.as_ref() else { return };
    if rt.imagery.ring.is_empty() {
        return;
    }
    let m = scene_mercator(app);
    for t in rt.imagery.ring.clone() {
        let state = rt.imagery.tiles.get(&tile_key(t)).map(|t| &t.state);
        match state {
            Some(ImageryState::Ready(tex)) => {
                draw_imagery_tile(painter, xform, &m, t, tex.id(), (pos2(0.0, 0.0), pos2(1.0, 1.0)), 1.0);
            }
            _ => {
                // The tile isn't in (yet): its nearest ready ancestor stretches over the quad.
                for dz in 1..=3 {
                    let Some((anc, uv0, uv1)) = crate::tiles::ancestor_uv(t, dz) else { break };
                    let anc_state = rt.imagery.tiles.get(&tile_key(anc)).map(|t| &t.state);
                    if let Some(ImageryState::Ready(tex)) = anc_state {
                        draw_imagery_tile(painter, xform, &m, t, tex.id(), (uv0, uv1), 1.0);
                        break;
                    }
                }
            }
        }
    }
    // The pinned tile layers, basemap over: each draws its one tile at its stored zoom, at its
    // own opacity, in tile_layers order — the same stack the 2D canvas paints.
    let pins = app.session.active().map(|st| st.doc.map.tile_layers.clone()).unwrap_or_default();
    if !pins.is_empty() {
        let rt = app.canvas.dem.as_ref();
        if let Some(rt) = rt {
            for tl in &pins {
                if !tl.visible || tl.opacity <= 0.0 {
                    continue;
                }
                let Some(ImageryTile { state: ImageryState::Ready(tex) }) = rt.imagery.pinned.get(&tl.id) else { continue };
                let tile = TileId::new(tl.z, tl.x, tl.y);
                if !tile.valid() {
                    continue;
                }
                draw_imagery_tile(painter, xform, &m, tile, tex.id(), (pos2(0.0, 0.0), pos2(1.0, 1.0)), tl.opacity as f32);
            }
        }
    }
}

/// One tile as a draped mesh: `n×n` sub-quads on the flat ground a hair above the slab,
/// textured with the tile's image (v down = south, like the tile's rows). `uv0`..`uv1`
/// pick a sub-rect of the texture (an ancestor stretched over a still-loading child).
///
/// `n` adapts to the camera's tilt: egui interpolates texture coordinates affinely in
/// screen space, so a coarse mesh shears when the tile spans a wide depth range (the
/// "distorted basemap" a tilted camera showed). Each tile reads its corner depths and
/// subdivides until every sub-quad's depth ratio is small — top-down keeps the cheap 8×8,
/// a grazing view refines toward 28×28 — so the draped imagery reads exactly like the 2D
/// map at any pitch, only the camera having changed.
fn draw_imagery_tile(painter: &egui::Painter, xform: &Xform, m: &Mercator, t: TileId, tex: egui::TextureId, uv: (Pos2, Pos2), alpha: f32) -> bool {
    let (tx0, ty0, tx1, ty1) = t.rect_pt();
    let (uv0, uv1) = uv;
    let (du, dv) = (uv1.x - uv0.x, uv1.y - uv0.y);
    let corner = |fx: f64, fy: f64| -> Option<(Pos2, f32)> {
        let (lon, lat) = doc_to_lonlat(tx0 + (tx1 - tx0) * fx, ty0 + (ty1 - ty0) * fy);
        let (x, yn) = m.project(lon, lat);
        xform.proj_w([x as f32, SKIN_LIFT, -(yn as f32)])
    };
    // The corners name the depth range (and the on-screen span) this tile covers. A corner
    // off-camera (behind the eye) leaves the range unknown — the ladder then stays coarse
    // and the sub-quad loop below still draws every visible cell of the tile.
    let cs = [corner(0.0, 0.0), corner(1.0, 0.0), corner(1.0, 1.0), corner(0.0, 1.0)];
    let depths: Vec<f32> = cs.iter().filter_map(|c| c.as_ref().map(|(_, w)| *w)).collect();
    let screen_span = {
        let pts: Vec<Pos2> = cs.iter().filter_map(|c| c.as_ref().map(|(p, _)| *p)).collect();
        let (min_x, max_x) = pts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
        let (min_y, max_y) = pts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
        (max_x - min_x).max(max_y - min_y)
    };
    // The sub-division ladder: depth ratio 1 (top-down) stays at 8; each extra ×1.15 of
    // depth range across the tile adds a rung, capped at 28 — and a tile too small on
    // screen never pays for refinement it can't show.
    let (w_min, w_max) = depths.iter().fold((f32::MAX, f32::MIN), |(lo, hi), w| (lo.min(*w), hi.max(*w)));
    let ratio = (w_max / w_min.max(NEAR_W)).max(1.0);
    let depth_rungs = (ratio.ln() / 1.15f32.ln()).ceil().max(0.0) as usize;
    let n = if screen_span < 220.0 || depths.len() < 4 { 8 } else { (8 + depth_rungs).clamp(8, 28) };
    let at = |i: usize, j: usize| -> Option<(Pos2, Pos2)> {
        let (fx, fy) = (i as f64 / n as f64, j as f64 / n as f64);
        let (lon, lat) = doc_to_lonlat(tx0 + (tx1 - tx0) * fx, ty0 + (ty1 - ty0) * fy);
        let (x, yn) = m.project(lon, lat);
        let p = xform.proj([x as f32, SKIN_LIFT, -(yn as f32)])?;
        Some((p, pos2(uv0.x + du * fx as f32, uv0.y + dv * fy as f32)))
    };
    let mut mesh = egui::Mesh::with_texture(tex);
    for j in 0..n {
        for i in 0..n {
            let (Some((a, ua)), Some((b, ub)), Some((c, uc)), Some((d, ud))) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)) else {
                continue;
            };
            let base_i = mesh.vertices.len() as u32;
            let tint = if alpha < 0.999 { Color32::WHITE.gamma_multiply(alpha) } else { Color32::WHITE };
            for (pos, uv) in [(a, ua), (b, ub), (c, uc), (d, ud)] {
                mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: tint });
            }
            mesh.add_triangle(base_i, base_i + 1, base_i + 2);
            mesh.add_triangle(base_i, base_i + 2, base_i + 3);
        }
    }
    let drew = !mesh.is_empty();
    if drew {
        painter.add(Shape::mesh(std::sync::Arc::new(mesh)));
    }
    drew
}

/// The look a draped path carries: its stroke (colour, width) and its fill (colour, tint) under it.
type DrapeStrokeFill = (([u8; 3], f32), Option<([u8; 3], f32)>);

/// The contours: thin flat quads on the datum (the ground is flat; the relief reads from the
/// lines themselves), minor thin and faint, major stronger — pushed into the Decal bucket.
fn draw_contours(bucket: &mut Vec<Tri>, drawn: &mut usize, cs: &Contours, xform: &Xform) {
    let minor = Color32::from_rgba_unmultiplied(0x6e, 0x59, 0x36, 175);
    let major = Color32::from_rgb(0x54, 0x40, 0x20);
    let half = 0.8f32;
    for (segs, col, is_major) in [(cs.minor.as_slice(), minor, false), (cs.major.as_slice(), major, true)] {
        if segs.len() > 60_000 && !is_major {
            continue; // an absurd trace: keep only the majors
        }
        for s in segs {
            if *drawn > TRI_BUDGET {
                return;
            }
            let (dx, dz) = (s[2] - s[0], s[3] - s[1]);
            let len = (dx * dx + dz * dz).sqrt().max(1e-6);
            let (nx, nz) = (-dz / len * half, dx / len * half);
            let y0 = SKIN_LIFT;
            let corners = [[s[0] + nx, s[1] + nz], [s[2] + nx, s[3] + nz], [s[2] - nx, s[3] - nz], [s[0] - nx, s[1] - nz]];
            let pts: Option<Vec<Pos2>> = corners.iter().map(|c| xform.proj([c[0], y0, c[1]])).collect();
            let Some(pts) = pts else { continue };
            let area = ((pts[1].x - pts[0].x) * (pts[2].y - pts[0].y) - (pts[2].x - pts[0].x) * (pts[1].y - pts[0].y)).abs();
            if area < 0.05 {
                continue;
            }
            let depth = (pts[0].x + pts[2].x).abs() * 0.001; // decal depth: tie-break only
            for tri in [[0usize, 1, 2], [0, 2, 3]] {
                bucket.push(Tri { depth, a: pts[tri[0]], b: pts[tri[1]], c: pts[tri[2]], color: col });
                *drawn += 1;
            }
        }
    }
}

/// The contour labels: each major line's height as small text along its path (the terrain's
/// only relief read) — the TRUE elevation (the site-relative level over the datum) — culled to
/// one label per ~110 screen px so the ladder stays legible.
fn draw_contour_labels(painter: &egui::Painter, xform: &Xform, cs: &Contours, base: f32) {
    if cs.labels.is_empty() {
        return;
    }
    let font = FontId::proportional(10.0);
    let ink = Color32::from_rgb(0x3d, 0x2f, 0x18);
    let mut placed: Vec<Pos2> = Vec::new();
    let min_gap_sq = 110.0f32 * 110.0;
    for (at, level) in &cs.labels {
        let [x, z] = *at;
        let Some(s) = xform.proj([x, SKIN_LIFT, z]) else { continue };
        if !xform.rect.contains(s) {
            continue;
        }
        if placed.iter().any(|p| p.distance_sq(s) < min_gap_sq) {
            continue;
        }
        let text = format!("{} m", (level + base).round() as i64);
        let galley = painter.layout_no_wrap(text, font.clone(), ink);
        let r = Rect::from_min_size(s + vec2(3.0, -15.0), galley.size() + vec2(5.0, 3.0));
        painter.rect_filled(r, 2.0, Color32::from_white_alpha(185));
        painter.galley(r.min + vec2(2.5, 1.5), galley, ink);
        placed.push(s);
    }
}

/// The ghost's ground ring (a circle on the datum around the hover point).
fn draw_ghost_ring(painter: &egui::Painter, xform: &Xform, hit: Vec3, kind: AssetKind) {
    let size = kind.size_m();
    let r = (size[0].max(size[1]) * 0.6).max(2.0);
    let col = Color32::from_rgb(0x2e, 0xa0, 0x4f);
    let segs = 32;
    let mut pts = Vec::with_capacity(segs + 1);
    for i in 0..=segs {
        let a = std::f32::consts::TAU * i as f32 / segs as f32;
        if let Some(p) = xform.proj([hit.x + r * a.cos(), hit.y + 0.05, hit.z + r * a.sin()]) {
            pts.push(p);
        }
    }
    if pts.len() > 2 {
        painter.add(Shape::line(pts, egui::Stroke::new(1.5, col)));
    }
}

/// The ground grid's line spacing for the camera's distance: a 1-2-5×10ⁿ metre ladder aiming
/// at about four cells across the visible half-width, so any dolly keeps a legible ladder.
fn grid_spacing(cam: &OrbitCamera) -> f32 {
    let raw = 2.0 * cam.dist * cam.fov_y.tan().max(0.01) / 4.0;
    let pow = 10.0f32.powf(raw.max(1e-3).log10().floor());
    let m = raw / pow;
    if m < 1.5 {
        pow
    } else if m < 3.5 {
        2.0 * pow
    } else if m < 7.5 {
        5.0 * pow
    } else {
        10.0 * pow
    }
}

/// The X/Y/Z grid lines on the map canvas: metre lines around the scene's position (the camera
/// target) on the flat ground, the two centre lines stronger — the position's coordinate frame,
/// seen well at every dolly.
fn draw_ground_grid(painter: &egui::Painter, xform: &Xform, cam: &OrbitCamera) {
    let target = cam.target;
    let s = grid_spacing(cam);
    let extent = (cam.dist * 2.5).max(s * 2.0);
    let n = ((extent / s).round() as i32).clamp(2, 28);
    let faint = Color32::from_black_alpha(44);
    let centre = Color32::from_black_alpha(100);
    let line = |a: [f32; 3], b: [f32; 3], stroke: egui::Stroke| {
        if let (Some(sa), Some(sb)) = (xform.proj(a), xform.proj(b)) {
            painter.line_segment([sa, sb], stroke);
        }
    };
    let z0 = target[2] - n as f32 * s;
    let z1 = target[2] + n as f32 * s;
    let x0 = target[0] - n as f32 * s;
    let x1 = target[0] + n as f32 * s;
    for i in -n..=n {
        let d = i as f32 * s;
        let is_centre = i == 0;
        let stroke = egui::Stroke::new(if is_centre { 1.4 } else { 1.0 }, if is_centre { centre } else { faint });
        line([target[0] + d, 0.02, z0], [target[0] + d, 0.02, z1], stroke);
        line([x0, 0.02, target[2] + d], [x1, 0.02, target[2] + d], stroke);
    }
}

/// The X/Y/Z axis gizmo at the scene's position (the camera target): east/up/south arrows in
/// the console colours, labelled — the position's own frame, big enough to read at any dolly.
fn draw_axis_gizmo(painter: &egui::Painter, xform: &Xform, target: [f32; 3], dist: f32) {
    let len = (dist * 0.16).max(2.0);
    let base = [target[0], target[1] + 0.05, target[2]];
    let axes: [([f32; 3], Color32, &str); 3] = [
        ([1.0, 0.0, 0.0], Color32::from_rgb(0xd4, 0x4c, 0x47), "X"),
        ([0.0, 1.0, 0.0], Color32::from_rgb(0x4c, 0xa6, 0x4c), "Y"),
        ([0.0, 0.0, 1.0], Color32::from_rgb(0x47, 0x6e, 0xd4), "Z"),
    ];
    let Some(origin) = xform.proj(base) else { return };
    for (dir, color, name) in axes {
        let tip_w = [base[0] + dir[0] * len, base[1] + dir[1] * len, base[2] + dir[2] * len];
        let Some(tip) = xform.proj(tip_w) else { continue };
        painter.line_segment([origin, tip], egui::Stroke::new(2.5, color));
        // The arrowhead: two short barbs across the axis direction, in screen space.
        let d = (tip - origin).normalized();
        let side = egui::Vec2::new(-d.y, d.x) * 5.0;
        let back = tip - d * 9.0;
        painter.line_segment([tip, back + side], egui::Stroke::new(2.0, color));
        painter.line_segment([tip, back - side], egui::Stroke::new(2.0, color));
        let font = FontId::proportional(11.0);
        let galley = painter.layout_no_wrap(name.to_string(), font, color);
        painter.galley(tip + vec2(4.0, -galley.size().y - 2.0), galley, color);
    }
}

/// The pointer's position marker: a big ground ring with crosshair ticks and a small Z stub,
/// white over black so it reads on any imagery — where the tools will draw, seen well.
fn draw_hover_cursor(painter: &egui::Painter, xform: &Xform, hit: Vec3, dist: f32) {
    let Some(c) = xform.proj([hit.x, SKIN_LIFT, hit.z]) else { return };
    let ring = 13.0f32;
    // The Z stub up from the ground (the third dimension the pick sits at).
    let stub = (dist * 0.05).max(1.5);
    if let Some(top) = xform.proj([hit.x, SKIN_LIFT + stub, hit.z]) {
        painter.line_segment([c, top], egui::Stroke::new(2.0, Color32::from_black_alpha(150)));
        painter.line_segment([c, top], egui::Stroke::new(1.0, Color32::from_white_alpha(220)));
    }
    // The ring: black under white, so it reads on any ground.
    painter.circle_stroke(c, ring, egui::Stroke::new(4.0, Color32::from_black_alpha(130)));
    painter.circle_stroke(c, ring, egui::Stroke::new(2.0, Color32::from_white_alpha(235)));
    // The crosshair ticks.
    for d in [egui::vec2(1.0, 0.0), egui::vec2(-1.0, 0.0), egui::vec2(0.0, 1.0), egui::vec2(0.0, -1.0)] {
        let a = c + d * (ring - 4.0);
        let b = c + d * (ring + 6.0);
        painter.line_segment([a, b], egui::Stroke::new(3.5, Color32::from_black_alpha(130)));
        painter.line_segment([a, b], egui::Stroke::new(2.0, Color32::from_white_alpha(235)));
    }
    painter.circle_filled(c, 2.5, Color32::from_white_alpha(240));
    painter.circle_stroke(c, 2.5, egui::Stroke::new(1.0, Color32::from_black_alpha(160)));
}

/// The pointer readout (top-left): the picked ground point's lon/lat (+ elevation where the DEM
/// covers it) over the terrain status line.
fn draw_readout(app: &TerrazoneApp, painter: &egui::Painter, rect: Rect, hover: Option<Pos2>) {
    let pad = 8.0;
    let font = FontId::proportional(10.0);
    let mut lines: Vec<String> = Vec::new();
    if let Some(hp) = hover
        && let Some(hit) = ground_pick(app, &app.ui.cam3d, rect, hp)
    {
        let m = scene_mercator(app);
        let (lon, lat) = m.unproject(f64::from(hit.x), f64::from(-hit.z));
        let mut line1 = format!("{:.5}°{}, {:.5}°{}", lat.abs(), if lat < 0.0 { 'S' } else { 'N' }, lon.abs(), if lon < 0.0 { 'W' } else { 'E' });
        if let Some(rt) = app.canvas.dem.as_ref()
            && let Some(e) = rt.grid.as_ref().and_then(|g| g.sample(lon, lat))
        {
            line1.push_str(&format!(" · {} m", e.round() as i64));
        }
        lines.push(line1);
    }
    lines.push(terrain_status(app));
    let galley = painter.layout_no_wrap(lines.join("\n"), font, Color32::WHITE);
    let pos = pos2(rect.left() + pad, rect.top() + pad);
    painter.rect_filled(Rect::from_min_size(pos - vec2(3.0, 2.0), galley.size() + vec2(6.0, 4.0)), 3.0, Color32::from_black_alpha(150));
    painter.galley(pos, galley, Color32::WHITE);
}

/// The terrain status line: `Terrain 9/9 · 768×768` (the contour source), `Terrain loading 4/9…`
/// or the no-fetcher hint.
fn terrain_status(app: &TerrazoneApp) -> String {
    let Some(rt) = app.canvas.dem.as_ref() else { return tl!("no DEM fetcher").to_string() };
    let Some(loader) = rt.loader.as_ref() else {
        return format!("{} — {}", tl!("Terrain"), tl!("no DEM fetcher"));
    };
    if let Some(g) = rt.grid.as_ref() {
        format!("{} {}/{} · {}×{}", tl!("Terrain"), loader.ready_count(), 9, g.w, g.h)
    } else if loader.pending_count() > 0 {
        format!("{} {}/{}…", tl!("Terrain loading"), loader.ready_count(), 9)
    } else {
        format!("{} {}/{}", tl!("Terrain"), loader.ready_count(), 9)
    }
}

/// The dual-canvas switch (top-right): `2D` and `3D` toggle the view's mode.
fn draw_canvas_switch(app: &mut TerrazoneApp, ui: &mut egui::Ui, rect: Rect) {
    let active_fill = crate::theme::Tokens::get(ui.ctx()).selection;
    let size = vec2(36.0, 22.0);
    let top = pos2(rect.right() - size.x * 2.0 - 12.0, rect.top() + 8.0);
    let on_2d = ui
        .put(
            Rect::from_min_size(top, size),
            egui::Button::new("2D").fill(if app.view().is_some_and(|v| !v.mode3d) { active_fill } else { Color32::from_black_alpha(110) }),
        )
        .clicked();
    let on_3d = ui
        .put(
            Rect::from_min_size(top + vec2(size.x + 4.0, 0.0), size),
            egui::Button::new("3D").fill(if app.view().is_some_and(|v| v.mode3d) { active_fill } else { Color32::from_black_alpha(110) }),
        )
        .clicked();
    let to = if on_2d {
        Some(false)
    } else if on_3d {
        Some(true)
    } else {
        None
    };
    if let Some(on) = to
        && app.view().is_some_and(|v| v.mode3d != on)
    {
        if let Some(v) = app.view_mut() {
            v.mode3d = on;
        }
        if on {
            enter_3d_status(app);
        }
    }
}

/// The status hint shown when the 3D environment is entered.
fn enter_3d_status(app: &mut TerrazoneApp) {
    app.status(tl!("3D environment — drag to orbit · Shift-drag to pan · wheel to zoom"));
}

/// The "terrain is on its way" line under the readout while nothing has stitched yet.
fn draw_loading_hint(app: &TerrazoneApp, painter: &egui::Painter, t: &crate::theme::Tokens, rect: Rect) {
    let Some(rt) = app.canvas.dem.as_ref() else { return };
    let Some(loader) = rt.loader.as_ref() else { return };
    if rt.grid.is_some() || loader.ready_count() > 0 {
        return;
    }
    let text = format!("{}…", tl!("Terrain loading"));
    let font = FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(text, font, t.text_dim);
    let pos = pos2(rect.center().x - galley.size().x * 0.5, rect.center().y - galley.size().y * 0.5);
    painter.rect_filled(Rect::from_min_size(pos - vec2(4.0, 3.0), galley.size() + vec2(8.0, 6.0)), 4.0, Color32::from_black_alpha(110));
    painter.galley(pos, galley, t.text_dim);
}

/// Input on the 3D canvas. With a drawing/editing tool up (the default), the pointer picks the
/// ground and the document point goes to the tool through the same dispatch the 2D canvas uses —
/// every path tool draws mercatorized on the terrain. The camera keeps its own gestures: drag
/// orbits, Shift-drag pans, the wheel dollies, double-click recentres — and with a navigation
/// tool (Hand/Zoom/Rotate View), Space held, or the placement tool up, the whole pointer belongs
/// to the camera, exactly as before.
fn handle_input(app: &mut TerrazoneApp, ui: &mut egui::Ui, resp: &egui::Response, rect: Rect, hover: Option<Pos2>) {
    let m = ui.input(|i| i.modifiers);
    let delta = ui.input(|i| i.pointer.delta());
    let tool = app.session.tool_id().to_string();
    let space = ui.input(|i| i.key_down(egui::Key::Space));
    let nav_tool = matches!(tool.as_str(), "hand" | "zoom" | "rotateView");
    let cam_mode = nav_tool || space;
    // The wheel: dolly (zoom toward the target) whatever the tool. `zoom_delta` covers Cmd-wheel
    // and pinch; the plain wheel arrives as scroll, zooming in (camera closer) scrolling up,
    // like the 2D canvas.
    let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y));
    let factor = if (zoom - 1.0).abs() > 1.0e-6 {
        1.0 / zoom
    } else if scroll.abs() > 0.01 {
        if scroll > 0.0 { 1.0 / DOLLY } else { DOLLY }
    } else {
        1.0
    };
    let mut moved = false;
    if (factor - 1.0).abs() > 1.0e-6 {
        app.ui.cam3d.dolly_by(factor);
        moved = true;
    }
    if cam_mode {
        if resp.dragged_by(egui::PointerButton::Primary) {
            if m.shift {
                app.ui.cam3d.pan(delta.x, delta.y);
            } else {
                // Yaw increases dragging right; dragging up raises the pitch (toward top-down).
                app.ui.cam3d.orbit(delta.x * ORBIT_SPEED, -delta.y * ORBIT_SPEED);
            }
            moved = true;
        }
        if resp.dragged_by(egui::PointerButton::Middle) {
            app.ui.cam3d.pan(delta.x, delta.y);
            moved = true;
        }
        let dbl = resp.double_clicked();
        if dbl && let Some(p) = hover {
            recentre(app, rect, p);
            moved = true;
        }
    } else {
        // Drawing / editing. Alt-drag still orbits (the camera's escape hatch with a tool up);
        // the middle button pans; the primary button belongs to the tool.
        if m.alt && resp.dragged_by(egui::PointerButton::Primary) {
            app.ui.cam3d.orbit(delta.x * ORBIT_SPEED, -delta.y * ORBIT_SPEED);
            moved = true;
        } else if resp.dragged_by(egui::PointerButton::Middle) {
            app.ui.cam3d.pan(delta.x, delta.y);
            moved = true;
        } else {
            dispatch_tool(app, ui, resp, rect, hover);
        }
    }
    if moved && let Some(rt) = app.canvas.dem.as_mut() {
        rt.interacted = true;
    }
}

/// A stroke draws through the camera it pressed under (frozen at Down): its every point picks
/// the ground through that same camera, so an Alt-orbit mid-stroke can't displace the line —
/// the 2D canvas's frozen-Xf rule, in 3D.
fn press_cam_id() -> egui::Id {
    egui::Id::new("canvas3d-press-cam")
}

/// Route the pointer to the active tool: the ground pick turns the screen position into the
/// document point the tool sees (the ground's lon/lat, through the document's Mercator).
fn dispatch_tool(app: &mut TerrazoneApp, ui: &mut egui::Ui, resp: &egui::Response, rect: Rect, hover: Option<Pos2>) {
    let pointer = ui.input(|i| i.pointer.clone());
    let m = ui.input(|i| i.modifiers);
    let view = app.view_info();
    let pick = |app: &TerrazoneApp, p: Pos2| ground_to_doc(app, rect, p).map(|d| (d, p));
    let at = hover.or_else(|| pointer.interact_pos().filter(|p| rect.contains(*p)));
    let frozen = |app: &TerrazoneApp, ui: &Ui, p: Pos2| -> Option<Point> {
        let cam: OrbitCamera = ui.data(|d| d.get_temp(press_cam_id()))?;
        let hit = ground_pick(app, &cam, rect, p)?;
        let m = scene_mercator(app);
        let (lon, lat) = m.unproject(f64::from(hit.x), f64::from(-hit.z));
        let (x, y) = lonlat_to_doc(lon, lat);
        Some(Point::new(x, y))
    };
    if pointer.primary_pressed()
        && resp.hovered()
        && let Some(p) = at
        && let Some(doc) = pick(app, p)
    {
        ui.data_mut(|d| d.insert_temp(press_cam_id(), app.ui.cam3d));
        let ev = terrazon_tools::PointerEvent {
            kind: terrazon_tools::PointerKind::Down,
            pos: doc.0,
            mods: crate::canvas::mods(m, false),
            pressure: crate::canvas::pen_pressure(ui, true),
        };
        crate::canvas::dispatch(app, &ev, view);
    } else if pointer.primary_down() && ui.data(|d| d.get_temp::<OrbitCamera>(press_cam_id()).is_some()) {
        // Held: drag (and tick time-driven tools), through the frozen camera.
        if pointer.delta() != egui::Vec2::ZERO
            && let Some(p) = at
            && let Some(pos) = frozen(app, ui, p)
        {
            let ev = terrazon_tools::PointerEvent {
                kind: terrazon_tools::PointerKind::Drag,
                pos,
                mods: crate::canvas::mods(m, false),
                pressure: crate::canvas::pen_pressure(ui, false),
            };
            crate::canvas::dispatch(app, &ev, view);
        }
        if app.session.tool_wants_ticks() {
            let dt = f64::from(ui.input(|i| i.unstable_dt).min(0.25));
            let r = app.session.tool_tick(dt, view);
            crate::canvas::apply_requests(app, r);
        }
    } else if !pointer.primary_down() && ui.data(|d| d.get_temp::<OrbitCamera>(press_cam_id()).is_some()) {
        // Released: the tool's Up, through the frozen camera, then the press memory goes.
        let p = at.unwrap_or(rect.center());
        if let Some(pos) = frozen(app, ui, p) {
            let ev = terrazon_tools::PointerEvent {
                kind: terrazon_tools::PointerKind::Up,
                pos,
                mods: crate::canvas::mods(m, false),
                pressure: crate::canvas::pen_pressure(ui, false),
            };
            crate::canvas::dispatch(app, &ev, view);
        }
        ui.data_mut(|d| d.remove::<OrbitCamera>(press_cam_id()));
    } else if let Some(p) = at
        && pointer.is_moving()
    {
        // Hovering: the tool's Move (the pen's rubber band, shape-builder hints…).
        if let Some(pos) = pick(app, p).map(|d| d.0) {
            let ev =
                terrazon_tools::PointerEvent { kind: terrazon_tools::PointerKind::Move, pos, mods: crate::canvas::mods(m, false), pressure: 1.0 };
            crate::canvas::dispatch(app, &ev, view);
        }
    }
    if resp.double_clicked()
        && let Some(p) = at
        && let Some(pos) = pick(app, p).map(|d| d.0)
    {
        let ev =
            terrazon_tools::PointerEvent { kind: terrazon_tools::PointerKind::DoubleClick, pos, mods: crate::canvas::mods(m, false), pressure: 1.0 };
        crate::canvas::dispatch(app, &ev, view);
    }
    if pointer.is_moving() || resp.dragged() {
        ui.ctx().request_repaint();
    }
}

/// Double-click: the camera target recentres on the clicked ground point.
fn recentre(app: &mut TerrazoneApp, rect: Rect, p: Pos2) {
    let cam = app.ui.cam3d;
    let Some(hit) = ground_pick(app, &cam, rect, p) else { return };
    app.ui.cam3d.target = [hit.x, 0.0, hit.z];
}

/// A click with the placement tool active: place the kind at the picked ground point (its
/// elevation is sampled right away when the DEM covers it; the tool stays active).
/// The document point a screen position picks on the ground: the picked world point's lon/lat
/// through the document's Mercator — the same doc coordinates the 2D canvas draws with, so a
/// stroke drawn here lands exactly where the flat map puts it.
fn ground_to_doc(app: &TerrazoneApp, rect: Rect, p: Pos2) -> Option<Point> {
    let hit = ground_pick(app, &app.ui.cam3d, rect, p)?;
    let m = scene_mercator(app);
    let (lon, lat) = m.unproject(f64::from(hit.x), f64::from(-hit.z));
    let (x, y) = lonlat_to_doc(lon, lat);
    Some(Point::new(x, y))
}

/// The flat frame the 3D environment's rulers read (the canvas draws the strips over the 3D
/// view): centred on the scene anchor — the ground the DEM window and imagery sit on — and
/// scaled so the ruler spans about the ground the camera sees. The perspective has no flat
/// projection, so the strips read the target's neighbourhood: pan and dolly the camera and the
/// degrees follow it.
pub fn ruler_xf(app: &TerrazoneApp, full: Rect) -> crate::canvas::Xf {
    let (lon, lat) = app.canvas.dem.as_ref().map_or_else(|| view_centre_lonlat(app), |rt| rt.anchor);
    let aspect = f64::from(full.width() / full.height().max(1.0)).max(0.01);
    let metres = 2.0 * f64::from(app.ui.cam3d.dist) * f64::from(app.ui.cam3d.fov_y.tan().max(0.01)) * aspect;
    // Ground metres per document point at this latitude (a degree of longitude shrinks by cos φ;
    // the Mercator y stretch cancels — the ruler reads degrees through the flat projection).
    let m_per_pt = 360.0 / WORLD_PT * 111_320.0 * lat.to_radians().cos().max(0.05);
    let zoom = (f64::from(full.width()) * m_per_pt / metres).clamp(0.002, 65_536.0);
    let (x, y) = lonlat_to_doc(lon, lat);
    crate::canvas::Xf { rect: full, zoom, center: Point::new(x, y), rot: 0.0 }
}

/// Everything a document point needs to become a world point on the flat draped ground.
struct DrapeCtx {
    m: Mercator,
}

impl DrapeCtx {
    /// Document point → world point on the draped ground (flat at the datum).
    fn world(&self, p: Point) -> [f32; 3] {
        let (lon, lat) = doc_to_lonlat(p.x, p.y);
        let (x, yn) = self.m.project(lon, lat);
        [x as f32, SKIN_LIFT, -(yn as f32)]
    }
    fn project(&self, xform: &Xform, p: Point) -> Option<Pos2> {
        xform.proj(self.world(p))
    }
}

/// The scene's drape context for the active document.
fn drape_ctx(app: &TerrazoneApp) -> DrapeCtx {
    DrapeCtx { m: scene_mercator(app) }
}

/// The flattening tolerance (document points) that puts a draped point every ~1.2 screen px:
/// ground metres per screen px at the camera's target depth, through the Mercator's metres per
/// document point.
fn drape_tol(app: &TerrazoneApp, rect: Rect) -> f64 {
    let metres_per_px = 2.0 * f64::from(app.ui.cam3d.dist) * f64::from(app.ui.cam3d.fov_y.tan().max(0.01)) / f64::from(rect.height().max(1.0));
    let m_per_doc_pt = 360.0 / WORLD_PT * 111_320.0 * scene_mercator(app).lat0.to_radians().cos().max(0.05);
    (metres_per_px * 1.2 / m_per_doc_pt).max(1.0e-9)
}

/// One draped polyline set: world-space subpath runs with a screen-space look. `closed` says
/// the path's geometry closes — a filled path's runs become filled polygons (under the stroke),
/// the way the 2D canvas paints fills under strokes.
struct DrapeLine {
    runs: Vec<Vec<[f32; 3]>>,
    look: DrapeLook,
    closed: bool,
}

/// The look of a draped line: colour, opacity, stroke width, and the fill a closed
/// path paints under its stroke (`None` = stroke only). `tint` is the paint's own
/// translucency (the 2D fill's alpha), so the fill reads like the 2D canvas's.
struct DrapeLook {
    rgb: [u8; 3],
    alpha: f32,
    width: f32,
    fill: Option<([u8; 3], f32)>,
}

/// The document's own drawing, draped on the flat ground: every visible path node's topmost
/// stroke as ground lines, and its topmost solid fill under them — every closed run of the
/// node paints (a MultiPolygon's islands fill like the 2D canvas's even-odd does for outer
/// rings), at the fill's own tint × the node's opacity, exactly the alpha the 2D canvas uses.
/// Text, images and effects stay 2D-only — this is the drawing, simply showing where it was
/// drawn, on the same ground the imagery lies on.
fn draw_doc_art(app: &TerrazoneApp, painter: &egui::Painter, xform: &Xform) {
    let Some(st) = app.session.active() else { return };
    let drape = drape_ctx(app);
    let tol = drape_tol(app, xform.rect);
    let mut lines: Vec<DrapeLine> = Vec::new();
    let mut budget = 60_000usize;
    let world = |p: Point| drape.world(p);
    for layer in &st.doc.layers {
        collect_doc_paths(layer, &world, &mut lines, &mut budget, tol);
        if budget == 0 {
            break;
        }
    }
    for l in &lines {
        // A closed path paints its fill first (each closed run on the ground — islands
        // included), the stroke over it — the same under-over the 2D canvas draws.
        if l.closed
            && let Some((fill, fill_alpha)) = l.look.fill
        {
            let fcol =
                Color32::from_rgba_unmultiplied(fill[0], fill[1], fill[2], (l.look.alpha * fill_alpha * 255.0).round().clamp(0.0, 255.0) as u8);
            for run in &l.runs {
                let pts: Option<Vec<Pos2>> = run.iter().map(|p| xform.proj(*p)).collect();
                if let Some(pts) = pts
                    && pts.len() >= 3
                {
                    painter.add(Shape::Path(egui::epaint::PathShape { points: pts, closed: true, fill: fcol, stroke: egui::Stroke::NONE.into() }));
                }
            }
        }
        let color = Color32::from_rgba_unmultiplied(l.look.rgb[0], l.look.rgb[1], l.look.rgb[2], (l.look.alpha * 255.0).round() as u8);
        for run in &l.runs {
            let mut cur: Vec<Pos2> = Vec::with_capacity(run.len());
            for p in run {
                match xform.proj(*p) {
                    Some(s) => cur.push(s),
                    None => {
                        if cur.len() >= 2 {
                            painter.add(Shape::line(cur.clone(), Stroke::new(l.look.width, color)));
                        }
                        cur.clear();
                    }
                }
            }
            if cur.len() >= 2 {
                painter.add(Shape::line(cur, Stroke::new(l.look.width, color)));
            }
        }
    }
}

/// Collect the drapeable paths of one node subtree (visible paths with a stroke or fill; layers,
/// groups, compound paths and blends walk through — path data lives in document coordinates).
fn collect_doc_paths(node: &terrazon_doc::Node, world: &dyn Fn(Point) -> [f32; 3], out: &mut Vec<DrapeLine>, budget: &mut usize, tol: f64) {
    if !node.visible || *budget == 0 {
        return;
    }
    match &node.kind {
        NodeKind::Path { path, guide, clipping, .. } if !guide && !clipping => {
            if let Some((stroke, fill)) = path_look(node) {
                drape_bezpath(
                    &path.to_bezpath(),
                    tol,
                    world,
                    DrapeLook { rgb: stroke.0, alpha: node.opacity.clamp(0.0, 1.0), width: stroke.1, fill },
                    out,
                    budget,
                );
            }
        }
        NodeKind::Layer { children, .. }
        | NodeKind::Group { children, .. }
        | NodeKind::Compound { children, .. }
        | NodeKind::Blend { children, .. } => {
            for c in children {
                collect_doc_paths(c, world, out, budget, tol);
                if *budget == 0 {
                    break;
                }
            }
        }
        _ => {}
    }
}

/// The drape look of one path node: its topmost stroke (solid paint) — else its topmost fill
/// read as an outline — plus its topmost fill for the closed-polygon underlay. `None` where
/// neither drapes (no paint, a pattern).
fn path_look(node: &terrazon_doc::Node) -> Option<DrapeStrokeFill> {
    let stroke = node.appearance.stroke().and_then(|s| paint_rgba(&s.paint).map(|(rgb, _)| (rgb, (s.width as f32).clamp(0.75, 8.0))));
    let fill = node.appearance.fill().and_then(|f| paint_rgba(&f.paint));
    match (stroke, fill) {
        (None, None) => None,
        (stroke, fill) => {
            // No stroke: the fill reads as the outline (the previous single-line drape).
            let stroke = stroke.or_else(|| fill.as_ref().map(|(rgb, _)| (*rgb, 1.0)));
            Some((stroke?, fill))
        }
    }
}

/// A paint's drape colour and alpha: a solid's own tint, a gradient's mid colour and opacity
/// (the drape's honest average of the ramp); `None` for no paint and patterns.
fn paint_rgba(p: &Paint) -> Option<([u8; 3], f32)> {
    match p {
        Paint::Solid { color, tint, .. } => {
            let [r, g, b, _] = color.to_rgba8(1.0);
            Some(([r, g, b], tint.clamp(0.0, 1.0)))
        }
        Paint::Gradient(gp) => {
            let (c, a) = gp.gradient.sample(0.5);
            let [r, g, b, _] = c.to_rgba8(1.0);
            Some(([r, g, b], a.clamp(0.0, 1.0)))
        }
        _ => None,
    }
}

/// Flatten a document path and drape it: each flattened point becomes a world point through
/// `world`, subpaths split at `MoveTo` (closed ones joined back to their first point).
fn drape_bezpath(bp: &BezPath, tol: f64, world: &dyn Fn(Point) -> [f32; 3], look: DrapeLook, out: &mut Vec<DrapeLine>, budget: &mut usize) {
    let mut runs: Vec<Vec<[f32; 3]>> = Vec::new();
    let mut cur: Vec<[f32; 3]> = Vec::new();
    let mut closed = false;
    crate::canvas::kurbo_flatten(bp, tol, &mut |el| match el {
        PathEl::MoveTo(p) => {
            if cur.len() >= 2 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            cur.push(world(Point::new(p.x, p.y)));
        }
        PathEl::LineTo(p) => {
            if *budget > 0 {
                *budget -= 1;
                cur.push(world(Point::new(p.x, p.y)));
            }
        }
        PathEl::ClosePath => {
            closed = true;
            if let Some(f) = cur.first().copied() {
                cur.push(f);
            }
        }
        _ => {}
    });
    if cur.len() >= 2 {
        runs.push(cur);
    }
    if !runs.is_empty() {
        out.push(DrapeLine { runs, look, closed });
    }
}

/// The tools' overlays projected onto the terrain: previews and marquees drape like the art,
/// anchors, handles and labels sit at their projected points — drawing reads where it lands.
fn draw_overlays_3d(painter: &egui::Painter, xform: &Xform, d: &DrapeCtx, tol: f64, overlays: &[terrazon_tools::Overlay]) {
    use terrazon_tools::Overlay;
    let c3 = |rgb: &[u8; 3]| Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let world = |p: Point| d.world(p);
    let mut tmp: Vec<DrapeLine> = Vec::new();
    for o in overlays {
        match o {
            Overlay::Marquee(r) => {
                let corners =
                    [Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Point::new(r.x1, r.y1), Point::new(r.x0, r.y1), Point::new(r.x0, r.y0)];
                let pts: Option<Vec<Pos2>> = corners.iter().map(|p| d.project(xform, *p)).collect();
                if let Some(pts) = pts {
                    painter.add(Shape::line(pts, Stroke::new(1.0, Color32::from_white_alpha(230))));
                }
            }
            Overlay::Path { path, color, width, .. } => {
                let mut budget = 8_000usize;
                drape_bezpath(
                    path,
                    tol,
                    &world,
                    DrapeLook { rgb: *color, alpha: 1.0, width: width.clamp(0.75, 6.0), fill: None },
                    &mut tmp,
                    &mut budget,
                );
            }
            Overlay::Line { a, b, color, .. } => {
                if let (Some(sa), Some(sb)) = (d.project(xform, *a), d.project(xform, *b)) {
                    painter.line_segment([sa, sb], Stroke::new(1.0, c3(color)));
                }
            }
            Overlay::GridLine { a, b, color } => {
                if let (Some(sa), Some(sb)) = (d.project(xform, *a), d.project(xform, *b)) {
                    painter.line_segment([sa, sb], Stroke::new(1.0, Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3])));
                }
            }
            Overlay::Anchor { p, color, filled, size } => {
                if let Some(s) = d.project(xform, *p) {
                    let r = size.max(2.0);
                    let rect = Rect::from_center_size(s, vec2(r * 2.0, r * 2.0));
                    if *filled {
                        painter.rect_filled(rect, 1.0, c3(color));
                    } else {
                        painter.rect_filled(rect, 1.0, Color32::WHITE);
                        painter.rect_stroke(rect, 1.0, Stroke::new(1.0, c3(color)), egui::StrokeKind::Inside);
                    }
                }
            }
            Overlay::Handle { p, color } => {
                if let Some(s) = d.project(xform, *p) {
                    painter.circle_filled(s, 2.5, c3(color));
                }
            }
            Overlay::Label { p, text, color } => {
                if let Some(s) = d.project(xform, *p) {
                    painter.text(s + vec2(8.0, -14.0), egui::Align2::LEFT_TOP, text, FontId::proportional(11.0), c3(color));
                }
            }
            Overlay::Measure { p, text } => {
                if let Some(s) = d.project(xform, *p) {
                    let galley = painter.layout_no_wrap(text.clone(), FontId::proportional(11.0), Color32::WHITE);
                    let r = Rect::from_min_size(s + vec2(14.0, 14.0), galley.size() + vec2(12.0, 8.0));
                    painter.rect_filled(r, 3.0, Color32::from_black_alpha(170));
                    painter.galley(r.min + vec2(6.0, 4.0), galley, Color32::WHITE);
                }
            }
            Overlay::Highlight { quad, color } => {
                let pts: Option<Vec<Pos2>> = quad.iter().map(|p| d.project(xform, *p)).collect();
                if let Some(pts) = pts {
                    painter.add(Shape::convex_polygon(pts, Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]), Stroke::NONE));
                }
            }
            Overlay::Swatch { p, color, selected } => {
                if let Some(s) = d.project(xform, *p) {
                    painter.circle_filled(s, 6.0, Color32::WHITE);
                    painter.circle_filled(s, 5.0, Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]));
                    painter.circle_stroke(s, 6.0, Stroke::new(1.0, Color32::from_gray(32)));
                    if *selected {
                        painter.circle_stroke(s, 8.0, Stroke::new(2.0, Color32::from_rgb(0x2e, 0xa0, 0x4f)));
                    }
                }
            }
        }
    }
    for l in &tmp {
        let color = Color32::from_rgba_unmultiplied(l.look.rgb[0], l.look.rgb[1], l.look.rgb[2], (l.look.alpha * 255.0).round() as u8);
        for run in &l.runs {
            let pts: Option<Vec<Pos2>> = run.iter().map(|p| xform.proj(*p)).collect();
            if let Some(pts) = pts {
                painter.add(Shape::line(pts, Stroke::new(l.look.width, color)));
            }
        }
    }
}

/// The Welcome screen's 3D vignette: the textured house and the stadium on a lit ground patch —
/// the same painter path the 3D canvas uses (Lambert, depth-sorted buckets), a fixed aerial
/// camera. Drawn inside `rect` as a rounded card.
pub fn vignette(ui: &mut egui::Ui, rect: Rect) {
    let painter = ui.painter_at(rect);
    let t = crate::theme::Tokens::get(ui.ctx());
    painter.rect_filled(rect, 8.0, Color32::from_rgb(0x2c, 0x35, 0x3d));
    let inner = rect.shrink(6.0);

    static HOUSE: std::sync::OnceLock<Mesh3d> = std::sync::OnceLock::new();
    static STADIUM: std::sync::OnceLock<Mesh3d> = std::sync::OnceLock::new();
    let house = HOUSE.get_or_init(|| prim::house(16.0, 12.0, 6.5));
    let stadium = STADIUM.get_or_init(|| prim::stadium(64.0, 42.0));

    // A fixed aerial camera over the little park: house in front, stadium behind.
    let cam = OrbitCamera {
        target: [-10.0, 2.0, 4.0],
        dist: 112.0,
        yaw: 0.52,
        pitch: 0.44,
        fov_y: 42.0f32.to_radians(),
        far_plane: 2_000.0,
        target_dist: None,
    };
    let aspect = (inner.width() / inner.height().max(1.0)).max(0.01);
    let xform = Xform { vp: cam.proj(aspect) * cam.view(), rect: inner };

    // Sky gradient card top → the ground reads against it.
    painter.rect_filled(inner, 4.0, SKY);
    let mut buckets: [Vec<Tri>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut drawn = 0usize;
    // The lawn the two sit on (a soft green, Lambert-shaded flat).
    let mut lawn = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Ground);
    let green = [0.42, 0.58, 0.34];
    let a = lawn.vertex([-90.0, 0.0, -60.0], green);
    let b = lawn.vertex([90.0, 0.0, -60.0], green);
    let c = lawn.vertex([90.0, 0.0, 60.0], green);
    let d = lawn.vertex([-90.0, 0.0, 60.0], green);
    lawn.quad(a, b, c, d);
    push_mesh(&mut buckets, &mut drawn, &lawn, [0.0, 0.0, 0.0], 0.0, 1.0, &xform);
    push_mesh(&mut buckets, &mut drawn, stadium, [22.0, 0.0, -6.0], 0.0, 1.0, &xform);
    flush_bucket(&buckets[0], &painter);
    buckets[0].clear();
    // House (Standing, textured with the procedural plaster/brick pass below).
    push_mesh(&mut buckets, &mut drawn, house, [-38.0, 0.0, 16.0], 0.0, 1.0, &xform);
    flush_bucket(&buckets[2], &painter);
    draw_vignette_house_texture(ui, &xform, [-38.0, 0.0, 16.0]);
    // The caption plate.
    let caption = tl!("The 3D environment — the basemap draped flat, contour lines and placed assets, in their own panels.").to_string();
    let galley = painter.layout_no_wrap(caption, FontId::proportional(10.5), Color32::WHITE);
    let pos = pos2(inner.left() + 8.0, inner.bottom() - galley.size().y - 8.0);
    painter.rect_filled(Rect::from_min_size(pos - vec2(4.0, 3.0), galley.size() + vec2(8.0, 6.0)), 3.0, Color32::from_black_alpha(120));
    painter.galley(pos, galley, Color32::WHITE);
    let _ = t;
}

/// The house texture: a tiny procedural plaster/brick tile, uploaded once per context (the tag
/// guards a graphics-context swap).
fn vignette_house_texture(ctx: &egui::Context, tag: u64) -> Option<egui::TextureHandle> {
    static TEX: std::sync::Mutex<Option<(u64, egui::TextureHandle)>> = std::sync::Mutex::new(None);
    let mut slot = TEX.lock().ok()?;
    if let Some((t, tex)) = slot.as_ref()
        && *t == tag
    {
        return Some(tex.clone());
    }
    // 64×64: cream plaster with two brick courses near the bottom (the wall UV's v > 0.7).
    let mut img = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let course = (y / 8) % 2;
            let brick = (x + course * 8) % 24 < 21 && y >= 44;
            let (r, g, b) = if brick {
                (0xb6, 0x8a, 0x68)
            } else if y >= 44 {
                (0xc9, 0xa2, 0x82)
            } else {
                (0xe2, 0xd6, 0xb8)
            };
            // A grain of noise so flat walls still read.
            let n = ((x.wrapping_mul(73) ^ y.wrapping_mul(151)) % 7) as i16 - 3;
            let cl = |v: i16| v.clamp(0, 255) as u8;
            img.extend_from_slice(&[cl(r + n), cl(g + n), cl(b + n), 255]);
        }
    }
    let image = ColorImage::from_rgba_unmultiplied([64, 64], &img);
    let tex = ctx.load_texture("vignette-house", image, TextureOptions::LINEAR);
    *slot = Some((tag, tex.clone()));
    Some(tex)
}

/// The house's walls as a textured mesh: the prim mesh carries UVs (u across the wall, v up with
/// brick bands low); every triangle is Lambert-shaded into the vertex color over the texture.
fn draw_vignette_house_texture(ui: &mut egui::Ui, xform: &Xform, at: [f32; 3]) {
    let tag = crate::graphics::context_tag(ui.ctx());
    let Some(tex) = vignette_house_texture(ui.ctx(), tag) else { return };
    let house = prim::house(16.0, 12.0, 6.5);
    let indices = if house.indices.is_empty() { (0..house.positions.len() as u32).collect::<Vec<u32>>() } else { house.indices.clone() };
    let mut mesh = egui::Mesh::with_texture(tex.id());
    let world = |i: usize| [house.positions[i][0] + at[0], house.positions[i][1] + at[1], house.positions[i][2] + at[2]];
    for tri in indices.as_chunks::<3>().0 {
        let (pa, pb, pc) = match (xform.proj(world(tri[0] as usize)), xform.proj(world(tri[1] as usize)), xform.proj(world(tri[2] as usize))) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => continue,
        };
        let area = (pb.x - pa.x) * (pc.y - pa.y) - (pc.x - pa.x) * (pb.y - pa.y);
        if area.abs() < 0.15 {
            continue;
        }
        let n = normal_of(world(tri[0] as usize), world(tri[1] as usize), world(tri[2] as usize));
        // White under the Lambert shade: the shade IS the tint over the texture.
        let shaded = Xform::shade([1.0, 1.0, 1.0], n);
        let base = mesh.vertices.len() as u32;
        for (i, p) in [(tri[0] as usize, pa), (tri[1] as usize, pb), (tri[2] as usize, pc)] {
            let uv = house.uvs.get(i).copied().unwrap_or([0.0, 0.0]);
            mesh.vertices.push(egui::epaint::Vertex { pos: p, uv: pos2(uv[0], uv[1]), color: shaded });
        }
        mesh.add_triangle(base, base + 1, base + 2);
    }
    if !mesh.is_empty() {
        ui.painter_at(xform.rect).add(Shape::mesh(std::sync::Arc::new(mesh)));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::Services;

    fn app_with_map() -> TerrazoneApp {
        let mut app = TerrazoneApp::new(terrazon_engine::Session::new(), Services::default());
        app.run("file.new", serde_json::json!({ "preset": "World Map" })).unwrap();
        app
    }

    /// The grid spacing follows the 1-2-5×10ⁿ ladder: three camera distances read three
    /// clean steps (each within 4× of the visible half-width over 4).
    #[test]
    fn the_ground_grid_spacing_climbs_the_one_two_five_ladder() {
        let s_close = grid_spacing(&OrbitCamera { dist: 120.0, ..Default::default() });
        let s_mid = grid_spacing(&OrbitCamera { dist: 1_200.0, ..Default::default() });
        let s_far = grid_spacing(&OrbitCamera { dist: 12_000.0, ..Default::default() });
        assert!(s_close < s_mid && s_mid < s_far, "spacing grows with the dolly: {s_close} {s_mid} {s_far}");
        for s in [s_close, s_mid, s_far] {
            let mantissa = s / 10.0f32.powf(s.log10().floor());
            assert!([1.0, 2.0, 5.0, 10.0].iter().any(|k| (mantissa - k).abs() < 1e-4), "1-2-5 ladder: {s} → {mantissa}");
        }
    }

    /// The pointer picks the flat datum plane — the ground drapes flat, so the pick and the
    /// drawing land exactly where the eye sees, on the same plane the imagery drapes onto.
    #[test]
    fn the_pointer_picks_the_flat_datum_plane() {
        let mut app = app_with_map();
        let anchor = (39.2236f64, -6.7780f64);
        app.canvas.dem = Some(DemRuntime::new(anchor, None));
        // Yaw 0: the eye sits south of the target, looking north at it.
        let cam = OrbitCamera { target: [0.0, 0.0, 0.0], dist: 300.0, yaw: 0.0, pitch: 0.6, ..Default::default() };
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let centre = pos2(400.0, 300.0);
        let hit = ground_pick(&app, &cam, rect, centre).expect("a pitched ray crosses the datum");
        assert!(hit.y.abs() < 1e-4, "the pick rides the datum plane: {hit:?}");
        // And its lon/lat round-trips through the scene Mercator (the doc point tools draw with).
        let m = scene_mercator(&app);
        let (lon, lat) = m.unproject(f64::from(hit.x), f64::from(-hit.z));
        let (back_x, back_y) = m.project(lon, lat);
        assert!(((back_x - f64::from(hit.x)).abs() < 1e-3) && ((back_y - f64::from(-hit.z)).abs() < 1e-3));
    }

    /// A look up at the sky picks nothing.
    #[test]
    fn a_sky_ray_picks_nothing() {
        let mut app = app_with_map();
        app.canvas.dem = Some(DemRuntime::new((39.2236, -6.7780), None));
        // Pitch above the horizon: the ray never comes down to the plane.
        let cam = OrbitCamera { target: [0.0, 0.0, 0.0], dist: 300.0, yaw: 0.0, pitch: 0.05, ..Default::default() };
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        assert!(ground_pick(&app, &cam, rect, pos2(400.0, 40.0)).is_none(), "the sky picks nothing");
    }
}
