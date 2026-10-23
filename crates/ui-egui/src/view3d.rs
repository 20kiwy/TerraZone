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
use terrazon_scene3d::{CamMode, CameraRig, Mercator, OrbitCamera, Ray, WalkCam, ray_plane_y};

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
/// The near plane the drape's quad clip cuts at — a hair in front of the rejection
/// threshold, so a clipped vertex's `w` rounds strictly above [`NEAR_W`] (the projection
/// below divides by it) and the screen math stays finite.
const CLIP_W: f32 = 1.0e-4;
/// Drag orbit speed (radians per pointer point).
const ORBIT_SPEED: f32 = 0.005;
/// One wheel notch dollies the camera by this factor.
const DOLLY: f32 = 1.2;
/// Walk mode (the EffectCraft camera): metres per second the WASD walk covers (the
/// reference's own walking pace).
const WALK_SPEED: f32 = 3.5;
/// Walk mode's Shift sprint multiplier — TerraZone's ground runs 60 km to the slab's edge, so
/// the walk needs a run to cross it.
const WALK_SPRINT: f32 = 8.0;
/// Walk mode's Q/E lift speed (metres per second).
const WALK_LIFT_SPEED: f32 = 4.0;
/// Walk mode's wheel lift (metres per scroll point — the reference's walk wheel, rescaled for
/// the metre world).
const WALK_WHEEL_LIFT: f32 = 0.05;
/// Walk mode's drag-look speed (radians per pointer point — the reference's own factor).
const WALK_LOOK_SPEED: f32 = 0.004;
/// The Web Mercator world's half extent (metres): the ground square the whole map covers
/// (±20 037 508 m). The grid, the slab and the imagery's footprint clamp to it — the 3D
/// ground IS the world's ground, so a zoomed-out camera sees the grid and the map run to the
/// world's own edges instead of stopping at a square patch around the anchor.
pub(crate) const WORLD_HALF_M: f64 = 20_037_508.342_789_244;
/// Target screen pixels between the endless grid's minor lines.
const GRID_TARGET_PX: f64 = 26.0;
/// The endless grid's per-direction line budget: at grazing angles the extent grows faster
/// than the centre spacing, so the step widens to stay inside it (the grid then still reaches
/// every edge of the viewport).
const GRID_MAX_LINES: f64 = 900.0;
/// The endless grid's minor lines draw only when they are at least this far apart on screen
/// (the EffectCraft viewer's draw_minor gate).
const GRID_MINOR_MIN_PX: f32 = 4.0;
/// View-centre travel (ground metres) that rebuilds the DEM window around the new centre.
const DEM_MOVE_M: f64 = 1500.0;
/// The imagery ring around the scene centre: ±[`IMAGERY_RING`] tiles at the base zoom, wider
/// (and capped higher) as the drape refines under a closer camera.
const IMAGERY_RING: i64 = 2;
const IMAGERY_CAP: usize = 24;
/// How long a failed imagery tile waits before asking again (as the 2D tile cache does).
const IMAGERY_RETRY: Duration = Duration::from_secs(30);
/// The ground slab's half extent fallback (metres) — the 3×3 slab patch sizes itself from
/// the frame's far reach; this floor keeps the patch sane before the first frame.
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
    /// The far plane the projection was built with (metres) — the horizon haze projects its
    /// band anchor along the eye's horizontal ray just inside it.
    far: f32,
}

impl Xform {
    /// The frame's projection for any [`CameraRig`] over `rect` — the orbit arm or the walk
    /// rig, whichever mode the environment is in.
    fn for_cam(rect: Rect, rig: &CameraRig) -> Self {
        let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
        let far = match rig {
            CameraRig::Orbit(c) => c.far_plane.max(c.dist * 8.0 + 400.0),
            CameraRig::Walk(_) => 120_000.0,
        };
        Self { vp: rig.proj(aspect) * rig.view(), rect, far }
    }

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
    /// The tiles the current ask covers, nearest first with each tile's view-depth ratio
    /// against the focus (near ground asks finer zooms than the far field — the tilted
    /// camera's LOD): what [`draw_imagery`] dresses.
    ring: Vec<(TileId, f64)>,
    /// The ask's fingerprint (the sorted ring's tile keys hashed): re-asked when it changes.
    asked: Option<u64>,
    /// The camera preview's own ring (the hovered camera's street-scale tiles, a small square
    /// around its ground point at its own zoom): the PIP renders sharp where the main frame's
    /// ring is coarse. Cleared when no camera hovers.
    preview_ring: Vec<TileId>,
    preview_asked: Option<(u8, u32, u32, u32, u32)>,
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

    /// The prim mesh of one placement, built once and cached by id + the build inputs (the
    /// scale, and the cameras' aim — an edited placement rebuilds, not a stale cache).
    fn prim_of(&mut self, p: &AssetPlacement) -> Mesh3d {
        let key = format!("{}|{:.3}|{:.1}|{:.1}", p.id, p.scale, p.pitch_deg, p.fov_deg);
        self.prims.entry(key).or_insert_with(|| build_prim(p.kind, p.scale)).clone()
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

    /// A placement is gone: drop its cached prim meshes (every build-input variant of it).
    pub fn forget_prim(&mut self, id: &str) {
        let prefix = format!("{id}|");
        self.prims.retain(|key, _| !key.starts_with(&prefix));
    }
}

impl Imagery {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { rx, tx, tiles: HashMap::new(), ring: vec![], asked: None, preview_ring: vec![], preview_asked: None, tag: 0, pinned: HashMap::new() }
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
        // A CCTV camera on its mast: the pole, the head box and the lens snout facing north
        // (−z — `push_placed` turns it to the placement's rotation, the same clockwise pass
        // the fleet rides). The view frustum it looks down is a separate mesh — it depends on
        // the placement's own pitch/fov (`camera_frustum_prim`), so it caches per placement.
        AssetKind::Camera => {
            let h = 6.0 * scale;
            let mut m = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Standing);
            merge_mesh(&mut m, &terrazon_scene3d::mesh::box_between([-0.09, 0.0, -0.09], [0.09, h, 0.09], [0.38, 0.40, 0.43]));
            merge_mesh(&mut m, &terrazon_scene3d::mesh::box_between([-0.28, h - 0.55, -0.42], [0.28, h, 0.10], [0.27, 0.29, 0.32]));
            merge_mesh(&mut m, &terrazon_scene3d::mesh::box_between([-0.12, h - 0.45, -0.58], [0.12, h - 0.18, -0.40], [0.14, 0.15, 0.17]));
            m
        }
    }
}

/// The scene Mercator: anchored at the runtime's scene centre (stable between DEM window
/// rebuilds so placements don't wander), or at the 2D view's centre before there is one.
fn scene_mercator(app: &TerrazoneApp) -> Mercator {
    let (lon, lat) = app.canvas.dem.as_ref().map_or(view_centre_lonlat(app), |r| r.anchor);
    Mercator::new(lat, lon)
}

/// The world position of one placement's ground point (the doc point through the scene
/// Mercator, on the datum).
fn placement_world(app: &TerrazoneApp, p: &AssetPlacement) -> [f32; 3] {
    let m = scene_mercator(app);
    let (lon, lat) = doc_to_lonlat(p.x, p.y);
    let (x, yn) = m.project(lon, lat);
    [x as f32, 0.0, -(yn as f32)]
}

/// The look rig of one camera placement: the mast-top eye standing on the datum (the flat
/// ground the placements read), looking along the placement's rotation (degrees clockwise
/// from north — the ground heading the 2D wedge draws) pitched below the horizon, through
/// the placement's own field of view. `aspect` widens the vertical fov out of the stored
/// horizontal one the way a render of that shape needs. This is the EffectCraft camera
/// object, one per placement: the preview renders the scene through it.
fn camera_rig(app: &TerrazoneApp, p: &AssetPlacement, aspect: f32) -> Option<CameraRig> {
    if !p.kind.is_camera() {
        return None;
    }
    let at = placement_world(app, p);
    let base = app.canvas.dem.as_ref().map_or(0.0, |r| r.base);
    // TerraZone's frame: x east, z south. Rotation clockwise from north: the ground heading
    // is (sin θ, −cos θ); the walk rig's yaw measures from +z (south), so yaw = θ + π.
    let th = p.rotation_deg.max(0.0).to_radians();
    let yaw = th + std::f32::consts::PI;
    let pitch = p.pitch_deg.clamp(-85.0, 85.0).to_radians();
    // The stored fov is horizontal: the vertical fov comes from the render's own aspect.
    let half_h = (p.fov_deg.clamp(5.0, 150.0) * 0.5).to_radians();
    let fov_y = 2.0 * (half_h.tan() / aspect.max(0.2)).atan();
    Some(CameraRig::Walk(WalkCam { pos: [at[0], base + p.eye_height_m(), at[2]], yaw, pitch, fov_y }))
}

/// The camera's view frustum as a decal mesh (apex at the mast-top eye, opening down to the
/// ground footprint the 2D wedge draws): four translucent side triangles over the ground
/// quad, the cone the 3D view reads the camera's aim from. Cached per placement+aim.
fn camera_frustum_prim(p: &AssetPlacement) -> Mesh3d {
    let half = (p.fov_deg.clamp(5.0, 150.0) * 0.5).to_radians();
    let pitch = p.pitch_deg.clamp(-85.0, 85.0).to_radians();
    let eye_h = p.eye_height_m();
    // The cone's edges sit at the centre ray's down-angle ± the half-fov: the steeper edge
    // lands NEAR, the shallower edge FAR; a ray at/above the horizon clamps to the far bound.
    let reach = |down: f32| if down <= 0.02 { CAM_FRUSTUM_FAR } else { (eye_h / down.tan()).clamp(0.5, CAM_FRUSTUM_FAR) };
    let down_c = -pitch; // radians below the horizon (positive = looking down)
    let near = reach(down_c + half);
    let far = reach(down_c - half).max(near + 0.5);
    let spread = half.tan();
    let th = p.rotation_deg.max(0.0).to_radians();
    // The ground heading (sin θ, −cos θ) and its right hand (cos θ, sin θ) — east at 90°.
    let (h_x, h_z) = (th.sin(), -th.cos());
    let (r_x, r_z) = (th.cos(), th.sin());
    let ground = |ahead: f32, side: f32| [h_x * ahead + r_x * side, 0.0, h_z * ahead + r_z * side];
    let apex = [0.0, eye_h, 0.0];
    let (na, nb) = (ground(near, -near * spread), ground(near, near * spread));
    let (fa, fb) = (ground(far, -far * spread), ground(far, far * spread));
    let mut m = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Decal);
    m.z_bias = 0.06;
    let color = [0.83, 0.54, 0.16];
    for tri in [[apex, na, nb], [apex, nb, fb], [apex, fb, fa], [apex, fa, na]] {
        let a = m.vertex(tri[0], color);
        let b = m.vertex(tri[1], color);
        let c = m.vertex(tri[2], color);
        m.tri(a, b, c);
    }
    // The ground footprint quad (the wedge's fill) under the sides.
    let a = m.vertex(na, color);
    let b = m.vertex(nb, color);
    let c = m.vertex(fb, color);
    let d = m.vertex(fa, color);
    m.tri(a, b, c);
    m.tri(a, c, d);
    m
}

/// The frustum's far clamp (metres): past this the cone is a smear on the ground.
const CAM_FRUSTUM_FAR: f32 = 120.0;

/// The camera placement the pointer hovers (its ground point projected within ~28 px of the
/// pointer): the preview renders the scene through it.
fn hover_camera(app: &TerrazoneApp, xform: &Xform, hover: Option<Pos2>) -> Option<AssetPlacement> {
    let h = hover?;
    let placements = app.session.active().map(|st| st.doc.placements.placements.clone()).unwrap_or_default();
    let mut best: Option<(f32, AssetPlacement)> = None;
    for p in placements.iter().filter(|p| p.kind.is_camera()) {
        let Some(s) = xform.proj(placement_world(app, p)) else { continue };
        let d = s.distance(h);
        if d <= 28.0 && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, p.clone()));
        }
    }
    best.map(|(_, p)| p)
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

/// The camera the frame renders through: the mode's rig (the orbit arm, or the walk rig the
/// EffectCraft viewer drives). Every pick, drape and aid of the 3D frame reads this, never
/// the mode directly.
fn rig_of(app: &TerrazoneApp) -> CameraRig {
    CameraRig::of(app.ui.cam_mode, app.ui.cam3d, app.ui.walk3d)
}

/// The pick ray through a screen point on the canvas.
fn ray_at(rig: &CameraRig, rect: Rect, p: Pos2) -> Ray {
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    let ndc = [((p.x - rect.left()) / rect.width()) * 2.0 - 1.0, -(((p.y - rect.top()) / rect.height()) * 2.0 - 1.0)];
    rig.ray_from_ndc(ndc, aspect)
}

/// The world point a screen position picks on the ground: the flat datum plane (the elevation
/// mesh is gone — the ground drapes flat, so the plane is the ground). `None` when the ray
/// never crosses it (looking at the sky).
fn ground_pick(app: &TerrazoneApp, rig: &CameraRig, rect: Rect, p: Pos2) -> Option<Vec3> {
    let _ = app;
    let ray = ray_at(rig, rect, p);
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
    // The ground vehicles stand on the draped terrain (the anchor's elevation), not the
    // datum — a fleet on a plateau reads on the ground the imagery paints.
    let base = app.canvas.dem.as_ref().map_or(0.0, |r| r.base);
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
        // The SGR's trains ride their own four-car set (the livery body) instead of the
        // generic kind block — the id names them, the way the 2D map's scheduled fan does.
        let is_sgr_train = v.kind == terrazon_fleet::VehicleKind::Train && v.id.starts_with("sgr-");
        let block = if is_sgr_train {
            cache.entry("sgr-train-set".into()).or_insert_with(sgr_train_prim).clone()
        } else {
            cache.entry(block_key).or_insert_with(|| fleet_block_prim(v.kind, class, color)).clone()
        };
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
        let lift = if v.kind.flies() { v.alt_m.unwrap_or(0.0).clamp(0.0, 20_000.0) } else { base };
        let ground_alpha = if lift > span { alpha * 0.45 } else { alpha };
        if hex_on {
            push_mesh(buckets, drawn, &hex, [x, base, z], 0.0, ground_alpha, xform);
        }
        push_mesh(buckets, drawn, &turned, [x, lift, z], 0.0, alpha, xform);
        if lift > span {
            // The plumb line: four triangles, built per flyer (cheaper than a cache keyed
            // by altitude).
            let leader = fleet_leader_prim(lift - base);
            push_mesh(buckets, drawn, &leader, [x, base, z], 0.0, alpha * 0.55, xform);
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

/// Append `other`'s vertices and indices onto `m` (the prim builders compose).
fn merge_mesh(m: &mut Mesh3d, other: &Mesh3d) {
    let base = m.positions.len() as u32;
    m.positions.extend_from_slice(&other.positions);
    m.colors.extend_from_slice(&other.colors);
    if other.indices.is_empty() {
        let n = other.positions.len() as u32;
        for i in 0..n {
            m.indices.push(base + i);
        }
    } else {
        m.indices.extend(other.indices.iter().map(|i| base + i));
    }
}

/// The SGR train set as the 3D body: a four-car electric multiple unit in the TRC's own
/// livery reading — cream body, the green window band, a darker roof — one mesh, nose at
/// −z, origin at the ground centre (the same convention `draw_fleet` rotates by).
fn sgr_train_prim() -> Mesh3d {
    let body = [0.91, 0.90, 0.86];
    let band = [0.13, 0.44, 0.27];
    let roof = [0.62, 0.63, 0.62];
    let glass = [0.22, 0.28, 0.30];
    let mut m = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Standing);
    let (w, len, h) = (2.9_f32, 17.0_f32, 3.5_f32);
    for car in 0..4usize {
        let z0 = -76.0 / 2.0 + car as f32 * 19.0;
        let z1 = z0 + len;
        // The car body.
        let b = terrazon_scene3d::mesh::box_between([-w / 2.0, 0.35, z0], [w / 2.0, 0.35 + h, z1], body);
        merge_mesh(&mut m, &b);
        // The roof: a slightly narrower slab over the body.
        let r = terrazon_scene3d::mesh::box_between([-w / 2.0 - 0.12, 0.35 + h, z0 + 0.4], [w / 2.0 + 0.12, 0.35 + h + 0.32, z1 - 0.4], roof);
        merge_mesh(&mut m, &r);
        // The green band along both flanks (the livery stripe), a hair off the skin.
        for side in [-1.0_f32, 1.0] {
            let s = terrazon_scene3d::mesh::box_between([side * (w / 2.0 + 0.02), 1.5, z0 + 0.6], [side * (w / 2.0 + 0.06), 2.4, z1 - 0.6], band);
            merge_mesh(&mut m, &s);
            // The window glass above the band.
            let g = terrazon_scene3d::mesh::box_between([side * (w / 2.0 + 0.02), 2.55, z0 + 1.6], [side * (w / 2.0 + 0.06), 3.1, z1 - 1.6], glass);
            merge_mesh(&mut m, &g);
        }
        // The nose cab (front car only): a shorter, taller face reads the driver's end.
        if car == 0 {
            let n = terrazon_scene3d::mesh::box_between([-w / 2.0 + 0.2, 0.35, z1 - 0.01], [w / 2.0 - 0.2, 0.35 + h - 0.6, z1 + 0.5], band);
            merge_mesh(&mut m, &n);
        }
    }
    m
}

/// A ground ribbon along a world-space centreline: one quad per segment, `half_w` metres to
/// each side at height `y` — the way the ballast, the rails and the contact wire lay down.
fn ribbon_between(out: &mut Mesh3d, pts: &[[f32; 2]], half_w: f32, y: f32, color: [f32; 3]) {
    for w in pts.windows(2) {
        let (dx, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
        let len = (dx * dx + dz * dz).sqrt();
        if len < 0.05 {
            continue;
        }
        let (nx, nz) = (-dz / len * half_w, dx / len * half_w);
        let a = out.vertex([w[0][0] - nx, y, w[0][1] - nz], color);
        let b = out.vertex([w[0][0] + nx, y, w[0][1] + nz], color);
        let c = out.vertex([w[1][0] + nx, y, w[1][1] + nz], color);
        let d = out.vertex([w[1][0] - nx, y, w[1][1] - nz], color);
        out.tri(a, b, c);
        out.tri(a, c, d);
    }
}

/// The SGR's railway in 3D: the atlas's SGR-class ways lay down as a dark ballast ribbon
/// with twin rails on it (street-close), the electrified ways raise their catenary — posts
/// every ~45 m with the wire they carry — and the whole pass gates on the same "Rail Atlas"
/// row the 2D map's highlight rides. The old TAZARA and Central Line stay atlas data only:
/// this canvas is the SGR's view.
fn draw_sgr_rail(app: &mut TerrazoneApp, buckets: &mut [Vec<Tri>; 3], drawn: &mut usize, xform: &Xform) {
    let Some(doc) = app.session.active().map(|st| st.doc.clone()) else { return };
    let Some(l) = doc.fleet.layer(terrazon_fleet::Provider::Sgr) else { return };
    let live = l.schedule.is_some() || !app.canvas.fleet.markers_of(terrazon_fleet::Provider::Sgr).is_empty();
    if !l.visible || !live || !crate::fleet::part_visible(&doc, terrazon_fleet::Provider::Sgr.label(), "Rail Atlas") {
        return;
    }
    let m = scene_mercator(app);
    let (clon, clat) = view_centre_lonlat(app);
    let far = match rig_of(app) {
        CameraRig::Orbit(c) => c.dist,
        CameraRig::Walk(w) => (w.pos[1] * 6.0).max(30.0),
    };
    // Level of detail by eye distance: the corridor reads from any height (the ribbon),
    // the street furniture only when the eye is close enough to see it.
    let detailed = far < 22_000.0;
    let view_r = (far as f64 * 6.0).clamp(12_000.0, 120_000.0);
    let atlas = &app.canvas.fleet.atlas;
    // The draped ground stands at the anchor's terrain elevation — the ribbon, the poles
    // and the wire ride that base, not the datum, or they bury under the imagery.
    let base = app.canvas.dem.as_ref().map_or(0.0, |r| r.base);
    let mut ballast = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Decal);
    ballast.z_bias = 0.03;
    let mut rails = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Decal);
    rails.z_bias = 0.045;
    let mut furn = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Decal);
    furn.z_bias = 0.035;
    let ballast_c = [0.30, 0.285, 0.26];
    let rail_c = [0.80, 0.78, 0.74];
    let post_c = [0.55, 0.42, 0.20];
    let wire_c = [0.32, 0.30, 0.27];
    let deg_to_m_lat = 111_320.0_f32;
    let deg_to_m_lon = (111_320.0 * clat.to_radians().cos()) as f32;
    let mut poles = 0usize;
    for line in &atlas.lines {
        if line.cls != terrazon_fleet::rail::RailClass::Sgr || !line.is_operational() {
            continue;
        }
        // Cull: the way's bbox against the view's ground radius (cheap, in degrees).
        let [mn_lo, mn_la, mx_lo, mx_la] = line.bbox;
        let (dlo, dla) = ((view_r / deg_to_m_lon as f64), (view_r / deg_to_m_lat as f64));
        let near = !(mx_lo < clon - dlo || mn_lo > clon + dlo || mx_la < clat - dla || mn_la > clat + dla);
        if !near {
            continue;
        }
        let world: Vec<[f32; 2]> = line
            .pts
            .iter()
            .map(|p| {
                let (x, yn) = m.project(p[0], p[1]);
                [x as f32, -(yn as f32)]
            })
            .collect();
        ribbon_between(&mut ballast, &world, 2.6, 0.05, ballast_c);
        if detailed {
            // Twin rails at the standard gauge's half-width.
            for side in [-0.72_f32, 0.72] {
                let shifted: Vec<[f32; 2]> = world
                    .windows(2)
                    .flat_map(|w| {
                        let (dx, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
                        let len = (dx * dx + dz * dz).sqrt().max(1e-4);
                        let (nx, nz) = (-dz / len * side, dx / len * side);
                        [[w[0][0] + nx, w[0][1] + nz], [w[1][0] + nx, w[1][1] + nz]]
                    })
                    .collect();
                ribbon_between(&mut rails, &shifted, 0.14, 0.09, rail_c);
            }
        }
        if line.electrified && detailed {
            // The contact line: posts off one side every ~45 m, each with its arm reaching
            // over the track, and the wire they carry riding the centreline at 5.5 m.
            let mut acc = 22.5_f32;
            for w in world.windows(2) {
                let (dx, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
                let seg_m = ((dx * deg_to_m_lon).powi(2) + (dz * deg_to_m_lat).powi(2)).sqrt();
                if seg_m < 0.05 {
                    continue;
                }
                let (ux, uz) = (dx / seg_m, dz / seg_m);
                // The side normal (metres): perpendicular in world space.
                let (px_n, pz_n) = (-uz, ux);
                let mut d = acc;
                while d < seg_m && poles < 600 {
                    let mx = w[0][0] + ux * d;
                    let mz = w[0][1] + uz * d;
                    // To world metres for the pole geometry.
                    let pole_x = mx + px_n * 3.4;
                    let pole_z = mz + pz_n * 3.4;
                    let pole = terrazon_scene3d::mesh::box_between([pole_x - 0.11, 0.0, pole_z - 0.11], [pole_x + 0.11, 5.9, pole_z + 0.11], post_c);
                    merge_mesh(&mut furn, &pole);
                    let arm = terrazon_scene3d::mesh::box_between(
                        [pole_x - px_n * 1.2 - 0.05, 5.45, pole_z - pz_n * 1.2 - 0.05],
                        [pole_x + 0.11, 5.62, pole_z + 0.11],
                        post_c,
                    );
                    merge_mesh(&mut furn, &arm);
                    poles += 1;
                    d += 45.0;
                }
                acc = d - seg_m;
            }
            if far < 9_000.0 {
                // The wire itself: a hair ribbon riding the centreline at 5.5 m, the height
                // the arms reach over the track.
                let mut wire_m = Mesh3d::new(terrazon_scene3d::mesh::MeshLayer::Decal);
                wire_m.z_bias = 0.05;
                ribbon_between(&mut wire_m, &world, 0.05, 5.5, wire_c);
                push_mesh(buckets, drawn, &wire_m, [0.0, base, 0.0], 0.0, 0.9, xform);
            }
        }
    }
    // The ribbons were built in world coordinates: push them at the terrain base.
    push_mesh(buckets, drawn, &ballast, [0.0, base, 0.0], 0.0, 1.0, xform);
    push_mesh(buckets, drawn, &rails, [0.0, base, 0.0], 0.0, 1.0, xform);
    if poles > 0 {
        push_mesh(buckets, drawn, &furn, [0.0, base, 0.0], 0.0, 1.0, xform);
    }
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
    // The drape's ground scale rides along: metres per screen px at the focus (the 2D rule's
    // input), plus the hovered camera's own street-scale ring for the preview.
    {
        let rig0 = rig_of(app);
        let xf0 = Xform::for_cam(rect, &rig0);
        let mpp0 = metres_per_px(app, rect);
        let preview = hover_camera(app, &xf0, hover).map(|cam| {
            let (lon, lat) = doc_to_lonlat(cam.x, cam.y);
            let aspect = 320.0 / 200.0;
            let half_h = (cam.fov_deg.clamp(5.0, 150.0) * 0.5).to_radians();
            let fov_y = 2.0 * (half_h.tan() / aspect).atan();
            let pmpp = 2.0 * f64::from(cam.eye_height_m()) * f64::from((fov_y * 0.5).tan()) / 200.0;
            (lon, lat, pmpp)
        });
        dem_step(app, ui.ctx(), Some(&xf0), mpp0, preview);
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, SKY);

    let interval = app.ui.terrain3d.contour_interval;
    let rig = rig_of(app);
    let xform = Xform::for_cam(rect, &rig);
    // The position aids' scale: the orbit arm's distance, or the walk rig's height off the
    // ground (a walked camera keeps its aids readable at street height).
    let aid_dist = match rig {
        CameraRig::Orbit(c) => c.dist,
        CameraRig::Walk(w) => (w.pos[1] * 3.0).max(12.0),
    };

    // The pointer's ground pick feeds the status bar's coordinate readout (the same doc-point
    // hover the 2D canvas sets).
    app.hover_doc = hover.and_then(|h| ground_to_doc(app, rect, h));
    // The camera placement under the pointer (if any): its view frustum runs brighter and the
    // preview at the canvas's bottom-right renders the scene through it.
    let hover_cam = hover_camera(app, &xform, hover);

    // The ground slab first (under everything): the flat ground the imagery drapes onto.
    let focus = match rig {
        CameraRig::Orbit(c) => [c.target[0], c.target[2]],
        CameraRig::Walk(w) => [w.pos[0], w.pos[2]],
    };
    let slab_half = (match rig {
        CameraRig::Orbit(c) => c.dist * 4.0,
        CameraRig::Walk(w) => w.pos[1] * 40.0,
    })
    .clamp(2_000.0, WORLD_HALF_M as f32);
    draw_slab(&painter, &xform, focus, slab_half, [0.55, 0.53, 0.48]);

    // The map's ground grid — the position aid — draws UNDER the imagery now (the same order
    // the 2D canvas reads): the tile map paints OVER the grid, so a draped map never shows
    // grid lines on top of it. The Grid layer's eye (`doc.map.grid_visible`, the Layers
    // panel's bottom row) turns the grid on; hiding the tile stack (the Tiles group's eye)
    // reveals it across the full world. A paper document keeps its grid too — there is no
    // imagery to cover it.
    let grid_on = app.session.active().is_none_or(|st| st.doc.map.grid_visible);
    if grid_on {
        draw_ground_grid(&painter, &xform, rect, &rig);
    }

    // The imagery drape (the 2D basemap under the 3D scene), flat on the datum: the ground
    // reads exactly like the 2D map, in perspective — over the grid, under everything else.
    draw_imagery(app, &painter, &xform);
    draw_loading_hint(app, &painter, &t, rect);

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
            let at = placement_world(app, p);
            push_placed(&mut buckets, &mut drawn, &mesh, p, at, &xform);
            let pbase = app.canvas.dem.as_ref().map_or(0.0, |r| r.base);
            push_blob_shadow(&mut buckets[1], &mut drawn, &xform, [at[0], pbase, at[2]], p.size_m(), &m);
            // A camera stands its view frustum too — the translucent cone from the mast-top
            // eye down to the ground footprint, the aim the 2D map draws as its wedge. The
            // hovered camera's cone runs brighter (it is the one the preview renders through).
            if p.kind.is_camera() {
                let hovered = hover_cam.as_ref().is_some_and(|hc| hc.id == p.id);
                let key = format!("camfrustum/{}/{}", p.id, p.rotation_deg as i32);
                let frustum = match app.canvas.dem.as_mut() {
                    Some(rt) => rt.prims.entry(key).or_insert_with(|| camera_frustum_prim(p)).clone(),
                    None => camera_frustum_prim(p),
                };
                push_mesh(&mut buckets, &mut drawn, &frustum, [at[0], pbase, at[2]], 0.0, if hovered { 0.62 } else { 0.32 }, &xform);
            }
        }
    }

    // The placement ghost: while the Assets tool holds the canvas, the asset it would place
    // draws translucent at the pointer's ground point.
    let ghost = assets_tool_kind(app).filter(|_| hover.is_some()).map(|kind| {
        hover.and_then(|h| ground_pick(app, &rig, rect, h)).map(|hit| {
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

    // The SGR's railway itself: the ballast ribbon, the twin rails and the electric
    // corridor's catenary — the standard gauge line under its own trains (the same atlas
    // the 2D map highlights, the same eyes).
    draw_sgr_rail(app, &mut buckets, &mut drawn, &xform);

    // The fleet's live markers: the same feeds the 2D map draws, standing on the ground.
    draw_fleet(app, &mut buckets, &mut drawn, &xform);

    // The SGR pass and the fleet's own decals (the hex bases, the blob shadows) ride the
    // Decal bucket — flush it here, above the contours' earlier pass, before the standing
    // bucket draws over both.
    flush_bucket(&buckets[1], &painter);
    buckets[1].clear();

    flush_bucket(&buckets[2], &painter);

    // The tools' overlays (previews, anchors, marquees) over everything — projected onto the
    // ground like the art, so drawing reads where it lands.
    let overlays = app.session.overlays(app.view_info());
    if !overlays.is_empty() {
        let drape = drape_ctx(app);
        let tol = drape_tol(app, rect);
        draw_overlays_3d(&painter, &xform, &drape, tol, &overlays);
    }

    // The position aids over everything: the X/Y/Z axis gizmo at the scene's position (under
    // the walked camera in walk mode), and the big cursor marker under the pointer's pick.
    let gizmo_at = match rig {
        CameraRig::Orbit(c) => c.target,
        CameraRig::Walk(w) => [w.pos[0], 0.0, w.pos[2]],
    };
    draw_axis_gizmo(&painter, &xform, gizmo_at, aid_dist);
    if let Some(h) = hover
        && let Some(hit) = ground_pick(app, &rig, rect, h)
    {
        draw_hover_cursor(&painter, &xform, hit, aid_dist);
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
    // The debug overlays, over everything but the canvas switch: the imagery ring's own
    // boundaries and zoom labels, the placements' own labels.
    if app.ui.tiles_debug {
        draw_tile_debug(app, &painter, &xform);
    }
    if app.ui.objects_debug {
        draw_object_debug(app, &painter, &xform);
    }
    // The camera preview: the hovered camera's own scene, bottom-right, over everything.
    if let Some(cam) = hover_cam.as_ref() {
        draw_camera_pip(app, ui, rect, cam, grid_on);
    }
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

/// The flat ground slab at the datum (drawn first, under everything): a 3×3 patch of quads
/// centred on the camera's focus, half the frame's far reach — every cell projects
/// independently, so the ground never vanishes because one far corner left the frustum (the
/// old single world-sized quad did exactly that the moment the camera tilted). The imagery
/// drapes over it; where no tile is ready yet, this is the ground that shows.
fn draw_slab(painter: &egui::Painter, xform: &Xform, focus: [f32; 2], half: f32, color: [f32; 3]) {
    let col = Xform::shade(color, Vec3::Y);
    let mut mesh = egui::Mesh::default();
    let mut grid: [[Option<Pos2>; 3]; 3] = Default::default();
    for (ix, x) in [focus[0] - half, focus[0], focus[0] + half].into_iter().enumerate() {
        for (iz, z) in [focus[1] - half, focus[1], focus[1] + half].into_iter().enumerate() {
            grid[ix][iz] = xform.proj([x, 0.0, z]);
        }
    }
    for ix in 0..2 {
        for iz in 0..2 {
            let (Some(a), Some(b), Some(c), Some(d)) = (grid[ix][iz], grid[ix + 1][iz], grid[ix + 1][iz + 1], grid[ix][iz + 1]) else { continue };
            let base = mesh.vertices.len() as u32;
            for p in [a, b, c, d] {
                mesh.colored_vertex(p, col);
            }
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base, base + 2, base + 3);
        }
    }
    if !mesh.is_empty() {
        painter.add(Shape::mesh(std::sync::Arc::new(mesh)));
    }
}

/// The DEM and imagery lifecycle, ticked once per 3D frame. `mpp` is the camera's ground
/// metres per screen px at the focus (the drape's zoom input, the 2D rule's scale).
fn dem_step(app: &mut TerrazoneApp, ctx: &egui::Context, xform: Option<&Xform>, mpp: f64, preview: Option<(f64, f64, f64)>) {
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
    //    The drape's zoom follows the camera's ground scale (the 2D map's own rule — mpp →
    //    level, to z22 where the source serves it), so dollying in brings sharper ground and
    //    dollying out coarsens it, exactly like the flat canvas.
    let source_zoom_cap = app.ui.terrain3d.imagery_zoom;
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
    let frame = ImageryFrame { source: source.as_ref(), mpp, xform };
    let imagery_busy =
        app.canvas.dem.as_mut().is_some_and(|rt| imagery_step(rt, frame, imagery_fetcher.as_deref(), ctx, &pinned, source_zoom_cap, preview));

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
    // How far the scene travels: a pan re-anchors around the camera (the view must not jump);
    // a far jump — the user panned the 2D map while the 3D runtime sat idle, then came back —
    // should land where the 2D map now looks, so the target snaps onto the new anchor's own
    // ground instead of carrying the old view kilometres across the world.
    let jump = ground_distance(rt.anchor.0, rt.anchor.1, centre.0, centre.1);
    let far_jump = jump > DEM_MOVE_M * 10.0;
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
    app.ui.cam3d.target = if far_jump {
        [0.0, target[1], 0.0]
    } else {
        let [nx, nz] = {
            let (x, yn) = new.project(tlon, tlat);
            [x as f32, -(yn as f32)]
        };
        [nx, target[1], nz]
    };
    if far_jump
        && let Some(walk) = match app.ui.cam_mode {
            CamMode::Walk => Some(&mut app.ui.walk3d),
            CamMode::Orbit => None,
        }
    {
        // A walked camera rides to the new anchor too (its position IS the view).
        walk.pos = [0.0, walk.pos[1], 0.0];
    }
    if let Some(rt) = app.canvas.dem.as_mut() {
        rt.imagery.asked = None;
    }
}

/// The drape's zoom for the camera's ground scale — the 2D map's own rule read through the
/// camera: the Web-Mercator level whose tiles are ~256 screen px at the focus's metres-per-px
/// ([`zoom_level_of`]), so the drape refines and coarsens exactly like the 2D canvas does as
/// the camera dollies — down to the map's last level z22 for the sources that serve it.
/// Clamped into the source's own zoom range and the terrain panel's max setting.
pub(crate) fn drape_zoom_view(mpp: f64, lat: f64, source: &terrazon_map::TileSource, max_zoom: u8) -> u8 {
    let z = zoom_level_of(mpp, lat).round() as i32;
    z.clamp(i32::from(source.min_zoom), i32::from(source.max_zoom).min(i32::from(max_zoom)).min(i32::from(terrazon_map::MAX_ZOOM))) as u8
}

/// What one imagery step reads from the frame: the drape's ground scale (metres per screen px
/// at the focus) and the frame's own projection — the ring covers the projected ground when a
/// frame is present, and falls back to the square ring around the anchor without one.
struct ImageryFrame<'a> {
    source: Option<&'a terrazon_map::TileSource>,
    mpp: f64,
    xform: Option<&'a Xform>,
}

/// Ask for one imagery tile if it isn't in the runtime yet (and isn't cooling down from a
/// recent failure) — the one ask the main ring, the preview ring and the preview's ancestor
/// fallbacks all go through.
fn ensure_imagery_tile(rt: &mut DemRuntime, fetcher: &dyn TileFetcher, source: &terrazon_map::TileSource, t: TileId) {
    let key = tile_key(t);
    if let Some(ImageryTile { state: ImageryState::Failed(at) }) = rt.imagery.tiles.get(&key)
        && Instant::now() < *at
    {
        return;
    }
    if !rt.imagery.tiles.contains_key(&key) {
        rt.imagery.tiles.insert(key.clone(), ImageryTile { state: ImageryState::Pending });
        let url = terrazon_map::build_tile_url(source, u32::from(t.z), t.x, t.y);
        fetcher.fetch(format!("{}/{}/{}/{}", source.id, t.z, t.x, t.y), url, rt.imagery.tx.clone());
    }
}

/// Ask for and collect the imagery tiles around the scene centre. Returns whether any ask is
/// still in flight (so the frame keeps repainting until the ground is dressed).
fn imagery_step(
    rt: &mut DemRuntime,
    frame: ImageryFrame<'_>,
    fetcher: Option<&dyn TileFetcher>,
    ctx: &egui::Context,
    pinned: &[terrazon_map::TileLayer],
    max_zoom: u8,
    preview: Option<(f64, f64, f64)>,
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
    // The LOD ring: every tile at the zoom its own ground distance needs (near fine, far
    // coarse), so a tilted camera's close ground reads sharp instead of stretching the
    // focus's zoom over it. Without a frame (no 3D canvas yet) the square ring around the
    // anchor at the focus's zoom stands in.
    let focus_z = drape_zoom_view(frame.mpp, rt.anchor.1, source, max_zoom);
    let (cx, cy) = terrazon_scene3d::mercator::lonlat_to_tile(rt.anchor.0, rt.anchor.1, u32::from(focus_z));
    let ring = imagery_ring_footprint(rt.anchor, frame.xform, source, max_zoom, frame.mpp)
        .unwrap_or_else(|| imagery_ring(focus_z, cx, cy, IMAGERY_RING, IMAGERY_CAP).into_iter().map(|t| (t, 1.0)).collect());
    // The ask's fingerprint: the ring's tile keys hashed in sorted order (a tilt or pan that
    // changes any tile re-asks; an unchanged set never does).
    let mut keys: Vec<String> = ring.iter().map(|(t, _)| tile_key(*t)).collect();
    keys.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for k in &keys {
        std::hash::Hash::hash(k, &mut hasher);
    }
    let print = std::hash::Hasher::finish(&hasher);
    if rt.imagery.asked != Some(print) {
        // A new ring: keep the ring's tiles and their ancestors (they dress the ring while it
        // loads), the camera preview's own tiles too, forget the rest, ask the ring (nearest
        // first, capped).
        let mut keep = std::collections::HashSet::new();
        for (t, _) in &ring {
            keep.insert(tile_key(*t));
            for dz in 1..=2 {
                if let Some((anc, _, _)) = crate::tiles::ancestor_uv(*t, dz) {
                    keep.insert(tile_key(anc));
                }
            }
        }
        for t in &rt.imagery.preview_ring {
            keep.insert(tile_key(*t));
            for dz in 1..=2 {
                if let Some((anc, _, _)) = crate::tiles::ancestor_uv(*t, dz) {
                    keep.insert(tile_key(anc));
                }
            }
        }
        rt.imagery.tiles.retain(|key, _| keep.contains(key));
        rt.imagery.ring = ring.clone();
        rt.imagery.asked = Some(print);
        for (t, _) in &ring {
            ensure_imagery_tile(rt, fetcher, source, *t);
        }
    }
    // The camera preview's own ring: the hovered camera renders at street scale through its
    // own rig — a small square of tiles around its ground point at the PIP's own zoom, so the
    // preview reads sharp where the main frame's ring is coarse. No hover: cleared, and the
    // next main re-ask forgets the preview's tiles.
    match preview {
        Some((plon, plat, pmpp)) => {
            // The preview's ground footprint spans the aim's reach (~70 m deep): the zoom
            // caps at z20 so a ring this size actually COVERS it (finer tiles would need
            // thousands), while the tiles still land near the PIP's own pixels-per-metre.
            let pz = drape_zoom_view(pmpp, plat, source, max_zoom).min(20);
            let (pcx, pcy) = terrazon_scene3d::mercator::lonlat_to_tile(plon, plat, u32::from(pz));
            let pring = imagery_ring(pz, pcx, pcy, 4, 100);
            let (min_x, max_x, min_y, max_y) =
                pring.iter().fold((u32::MAX, 0u32, u32::MAX, 0u32), |(x0, x1, y0, y1), t| (x0.min(t.x), x1.max(t.x), y0.min(t.y), y1.max(t.y)));
            if rt.imagery.preview_asked != Some((pz, min_x, max_x, min_y, max_y)) {
                rt.imagery.preview_ring = pring.clone();
                rt.imagery.preview_asked = Some((pz, min_x, max_x, min_y, max_y));
                for t in &pring {
                    ensure_imagery_tile(rt, fetcher, source, *t);
                    // The draw's fallback chain: a tile that never lands (a zoom the source
                    // doesn't serve here) stretches its ready ancestor — ask those too.
                    for dz in 1..=3 {
                        if let Some((anc, _, _)) = crate::tiles::ancestor_uv(*t, dz) {
                            ensure_imagery_tile(rt, fetcher, source, anc);
                        }
                    }
                }
            }
        }
        None => {
            rt.imagery.preview_ring.clear();
            rt.imagery.preview_asked = None;
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
        || rt.imagery.preview_ring.iter().any(|t| matches!(rt.imagery.tiles.get(&tile_key(*t)).map(|t| &t.state), Some(ImageryState::Pending)))
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

/// The footprint ring's cap (the widest tilted view needs more tiles than the square's 24,
/// and the LOD's mixed zooms add a handful more — still a fraction of what a full
/// screen-quadtree would ask).
const FOOTPRINT_CAP: usize = 140;

/// The imagery ring from the frame's own ground footprint, LODed per distance: a grid of
/// screen samples grounds onto the datum (`Xform::ground_pick`, each clamped into the WEB
/// MERCATOR WORLD SQUARE), every sample's view depth beside the focus's names the ground
/// scale the screen sees THERE — a tilted camera's near ground sits much closer than the
/// focus, so it asks the finer zoom it needs while the far field reads coarser tiles (one
/// zoom for the whole view starved the near ground, which is why an angled camera's imagery
/// read like a smear) — and the tile covering the sample at its own zoom answers, nearest
/// first, capped. `None` when the frame gives no ground (all sky, or no frame at all): the
/// caller falls back to the square ring.
fn imagery_ring_footprint(anchor: (f64, f64), xform: Option<&Xform>, source: &terrazon_map::TileSource, max_zoom: u8, focus_mpp: f64) -> Option<Vec<(TileId, f64)>> {
    let xf = xform?;
    let m = Mercator::new(anchor.1, anchor.0);
    let r = xf.rect;
    // The focus's own view depth — the anchor's ground point, the same ground `focus_mpp`
    // was measured at. Every sample's depth divides against it.
    let (fax, fay) = m.project(anchor.0, anchor.1);
    let w_focus = xf.proj_w([fax as f32, SKIN_LIFT, -(fay as f32)]).map(|(_, w)| w).unwrap_or(1.0).max(NEAR_W);
    // The sample grid: dense enough that a near tile (the screen's finest, ~256 px) never
    // slips between samples.
    const COLS: usize = 6;
    const ROWS: usize = 9;
    let mut picks: HashMap<TileId, f64> = HashMap::new();
    for row in 0..=ROWS {
        for col in 0..=COLS {
            let sx = r.left() + r.width() * col as f32 / COLS as f32;
            let sy = r.top() + r.height() * row as f32 / ROWS as f32;
            let Some(g) = xf.ground_pick(pos2(sx, sy)) else { continue };
            // World-clamp (the box never leaves the map's own ground).
            let len = f64::from((g[0] * g[0] + g[2] * g[2]).sqrt());
            let (gx, gz) = if len > WORLD_HALF_M {
                let k = WORLD_HALF_M / len;
                (f64::from(g[0]) * k, f64::from(g[2]) * k)
            } else {
                (f64::from(g[0]), f64::from(g[2]))
            };
            let (lon, lat) = m.unproject(gx, -gz);
            // The ground scale the screen sees at this sample, beside the focus's: the
            // view depths ride the same projection, so their ratio is the mpp ratio.
            let w = xf.proj_w([g[0], SKIN_LIFT, g[2]]).map(|(_, w)| w).unwrap_or(w_focus).max(NEAR_W);
            let ratio = (f64::from(w) / f64::from(w_focus)).clamp(1.0 / 64.0, 64.0);
            let z = drape_zoom_view(focus_mpp * ratio, lat, source, max_zoom);
            let (tx, ty) = terrazon_scene3d::mercator::lonlat_to_tile(lon, lat, u32::from(z));
            let e = picks.entry(TileId::new(z, tx, ty)).or_insert(f64::INFINITY);
            *e = e.min(ratio);
        }
    }
    // The anchor's own tile at the focus's zoom: the centre never slips through the grid.
    let fz = drape_zoom_view(focus_mpp, anchor.1, source, max_zoom);
    let (acx, acy) = terrazon_scene3d::mercator::lonlat_to_tile(anchor.0, anchor.1, u32::from(fz));
    picks.entry(TileId::new(fz, acx, acy)).or_insert(1.0);
    if picks.is_empty() {
        return None;
    }
    let mut ring: Vec<(TileId, f64)> = picks.into_iter().collect();
    ring.sort_by(|a, b| a.1.total_cmp(&b.1));
    ring.truncate(FOOTPRINT_CAP);
    Some(ring)
}

fn tile_key(t: TileId) -> String {
    format!("{}/{}/{}", t.z, t.x, t.y)
}

/// The imagery textures on the ground: the current ring, every ready tile draped FLAT on the
/// datum (the elevation mesh is gone — the ground reads like the 2D map); a tile still loading
/// stretches its nearest ready ancestor's texture over its own quad (the 2D cache's parent
/// fallback). The ring's tiles draw FARTHEST first so a finer near tile paints over the
/// coarser far tile that contains it.
fn draw_imagery(app: &mut TerrazoneApp, painter: &egui::Painter, xform: &Xform) {
    let ring = app.canvas.dem.as_ref().map(|rt| rt.imagery.ring.clone()).unwrap_or_default();
    draw_imagery_ring(app, painter, xform, &ring, true);
}

/// The imagery textures on the ground for one ring: every ready tile draped FLAT on the datum
/// (the elevation mesh is gone — the ground reads like the 2D map); a tile still loading
/// stretches its nearest ready ancestor's texture over its own quad (the 2D cache's parent
/// fallback). The tiles draw FARTHEST FIRST (the ring's depth ratio, descending): the LOD's
/// finer near tiles land over the coarser far tiles that hold them. The main frame dresses
/// the drape's ring; the camera preview dresses its own ([`DemRuntime::imagery`]'s
/// `preview_ring`) — same tiles, same fallback.
fn draw_imagery_ring(app: &mut TerrazoneApp, painter: &egui::Painter, xform: &Xform, ring: &[(TileId, f64)], with_pinned: bool) {
    let Some(rt) = app.canvas.dem.as_ref() else { return };
    if ring.is_empty() {
        return;
    }
    let mut order: Vec<(TileId, f64)> = ring.to_vec();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    let m = scene_mercator(app);
    for (t, _) in &order {
        let state = rt.imagery.tiles.get(&tile_key(*t)).map(|t| &t.state);
        match state {
            Some(ImageryState::Ready(tex)) => {
                draw_imagery_tile(painter, xform, &m, *t, tex.id(), (pos2(0.0, 0.0), pos2(1.0, 1.0)), 1.0);
            }
            _ => {
                // The tile isn't in (yet): its nearest ready ancestor stretches over the quad.
                for dz in 1..=3 {
                    let Some((anc, uv0, uv1)) = crate::tiles::ancestor_uv(*t, dz) else { break };
                    let anc_state = rt.imagery.tiles.get(&tile_key(anc)).map(|t| &t.state);
                    if let Some(ImageryState::Ready(tex)) = anc_state {
                        draw_imagery_tile(painter, xform, &m, *t, tex.id(), (uv0, uv1), 1.0);
                        break;
                    }
                }
            }
        }
    }
    // The pinned tile layers, basemap over: each draws its one tile at its stored zoom, at its
    // own opacity, in tile_layers order — the same stack the 2D canvas paints. They ride the
    // MAIN ring's pass only (the preview shows the plain ground).
    if !with_pinned {
        return;
    }
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

/// One vertex of a clipped textured quad: the clip-space position and its texture coordinate.
type ClipVert = (glam::Vec4, Pos2);

/// Clip a textured quad against the near plane (clip `w` above [`CLIP_W`]): Sutherland–Hodgman
/// across the four edges, the UVs riding the interpolation, so a ground cell that straddles
/// the eye's plane still paints its visible part instead of vanishing — the bare hole with
/// the straight edge a grazing camera used to cut out of the drape's near field.
fn clip_quad_near(quad: [ClipVert; 4]) -> Vec<ClipVert> {
    let mut out: Vec<ClipVert> = Vec::with_capacity(8);
    for i in 0..4 {
        let a = quad[i];
        let b = quad[(i + 1) % 4];
        let a_in = a.0.w > CLIP_W;
        let b_in = b.0.w > CLIP_W;
        if a_in {
            out.push(a);
        }
        if a_in != b_in {
            let t = (CLIP_W - a.0.w) / (b.0.w - a.0.w);
            out.push((a.0 + (b.0 - a.0) * t, pos2(a.1.x + (b.1.x - a.1.x) * t, a.1.y + (b.1.y - a.1.y) * t)));
        }
    }
    out
}

/// One tile as a draped mesh: `nx×nz` sub-quads on the flat ground a hair above the slab,
/// textured with the tile's image (v down = south, like the tile's rows). `uv0`..`uv1`
/// pick a sub-rect of the texture (an ancestor stretched over a still-loading child).
///
/// The grid subdivides PER AXIS, and the axes are independent: egui interpolates texture
/// coordinates affinely in screen space, so a coarse mesh shears when a sub-quad spans a
/// wide depth range — and under a tilted camera the depth varies along the VIEW's ground
/// direction, not along the tile's own x/z equally. Each axis reads its own depth ratio
/// (the two mid-edges across it, the corners' clip `w` CLAMPED into the near plane so a
/// tile running under the eye refines hard instead of collapsing to the cheap ladder —
/// that collapse was the angled camera's smear) and its own screen span, and refines until
/// every sub-quad's depth ratio is small and its screen cells stay small — top-down keeps
/// the cheap 8×8, a grazing view refines the compressed axis hard (up to 64) while the
/// lateral axis stays cheap. Each sub-quad then clips against the near plane (its visible
/// part paints; the part behind the eye never did), and a cell wholly off the viewport
/// skips. The draped imagery reads exactly like the 2D map at any pitch, only the camera
/// having changed.
fn draw_imagery_tile(painter: &egui::Painter, xform: &Xform, m: &Mercator, t: TileId, tex: egui::TextureId, uv: (Pos2, Pos2), alpha: f32) -> bool {
    let (tx0, ty0, tx1, ty1) = t.rect_pt();
    let (uv0, uv1) = uv;
    let (du, dv) = (uv1.x - uv0.x, uv1.y - uv0.y);
    // The world point a tile fraction names (the ground plane, a hair above the slab).
    let world = |fx: f64, fy: f64| -> [f32; 3] {
        let (lon, lat) = doc_to_lonlat(tx0 + (tx1 - tx0) * fx, ty0 + (ty1 - ty0) * fy);
        let (x, yn) = m.project(lon, lat);
        [x as f32, SKIN_LIFT, -(yn as f32)]
    };
    // The clip-space vertex a tile fraction names: the position the near-plane clip reads,
    // the UV the texture samples.
    let clip_at = |fx: f64, fy: f64| -> ClipVert {
        let w = world(fx, fy);
        let v = xform.vp * glam::Vec4::new(w[0], w[1], w[2], 1.0);
        (v, pos2(uv0.x + du * fx as f32, uv0.y + dv * fy as f32))
    };
    // The view depth a tile fraction sits at, clamped into the near plane: the ladder's
    // input everywhere (a corner behind the eye clamps tiny, the ratio across the axis
    // grows huge, the ladder refines that axis hard — no cheap collapse).
    let w_at = |fx: f64, fy: f64| -> f32 { clip_at(fx, fy).0.w.max(NEAR_W) };
    let (w00, w10, w11, w01) = (w_at(0.0, 0.0), w_at(1.0, 0.0), w_at(1.0, 1.0), w_at(0.0, 1.0));
    // The tile's on-screen span, from the corners that project (the rest clipped away). A
    // tile wholly behind the eye paints nothing at all — one check, no 4096-cell sweep.
    let mut xs = [0.0f32; 4];
    let mut ys = [0.0f32; 4];
    let mut any = false;
    for (i, (fx, fy)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
        let v = clip_at(fx, fy).0;
        if v.w > NEAR_W {
            let ndc = v.truncate() / v.w;
            xs[i] = xform.rect.left() + (ndc.x * 0.5 + 0.5) * xform.rect.width();
            ys[i] = xform.rect.top() + (1.0 - (ndc.y * 0.5 + 0.5)) * xform.rect.height();
            any = true;
        }
    }
    if !any {
        return false;
    }
    let screen_span = {
        let (min_x, max_x) = (xs.iter().cloned().fold(f32::MAX, f32::min), xs.iter().cloned().fold(f32::MIN, f32::max));
        let (min_y, max_y) = (ys.iter().cloned().fold(f32::MAX, f32::min), ys.iter().cloned().fold(f32::MIN, f32::max));
        (max_x - min_x).max(max_y - min_y)
    };
    // Per-axis subdivision: the depth ratio across the axis (its two mid-edges), the axis's
    // screen span, the tighter of the two ladders wins; capped and floored.
    const DEPTH_RUNG: f32 = 1.10; // per-sub-quad depth ratio the affine warp barely reads
    const CELL_PX: f32 = 16.0; // sub-quad screen cells stay under this
    const MAX_RUNGS: usize = 64;
    let axis_rungs = |ratio: f32, span_px: f32| -> usize {
        let depth = (ratio.max(1.0).ln() / DEPTH_RUNG.ln()).ceil() as usize;
        let px = (span_px / CELL_PX).ceil() as usize;
        (depth.max(px)).clamp(8, MAX_RUNGS)
    };
    let (nx, nz) = if screen_span < 220.0 {
        (8usize, 8usize)
    } else {
        // The x axis spans corners (0,0)→(1,0) and (0,1)→(1,1): its depth ratio compares the
        // x=0 edge against the x=1 edge; the z axis the z=0 edge against the z=1 edge.
        let ratio_x = w00.max(w01) / w10.min(w11);
        let ratio_z = w00.max(w10) / w01.min(w11);
        let (rx, rz) = (ratio_x.max(1.0 / ratio_x), ratio_z.max(1.0 / ratio_z));
        (axis_rungs(rx, screen_span * 0.5), axis_rungs(rz, screen_span * 0.5))
    };
    let mut mesh = egui::Mesh::with_texture(tex);
    let tint = if alpha < 0.999 { Color32::WHITE.gamma_multiply(alpha) } else { Color32::WHITE };
    // The grid's clip vertices, projected once (each point feeds up to four cells — the
    // per-cell re-projection this replaces was four Mercator projects per cell).
    let mut grid: Vec<ClipVert> = Vec::with_capacity((nx + 1) * (nz + 1));
    for j in 0..=nz {
        for i in 0..=nx {
            grid.push(clip_at(i as f64 / nx as f64, j as f64 / nz as f64));
        }
    }
    let at = |i: usize, j: usize| grid[j * (nx + 1) + i];
    for j in 0..nz {
        for i in 0..nx {
            let poly = clip_quad_near([at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)]);
            if poly.len() < 3 {
                continue;
            }
            // The clipped hull's screen points (every vertex survives the near plane here).
            let pts: Vec<(Pos2, Pos2)> = poly
                .iter()
                .map(|(v, uv)| {
                    let ndc = v.truncate() / v.w;
                    (pos2(xform.rect.left() + (ndc.x * 0.5 + 0.5) * xform.rect.width(), xform.rect.top() + (1.0 - (ndc.y * 0.5 + 0.5)) * xform.rect.height()), *uv)
                })
                .collect();
            // A sub-quad wholly off the viewport paints nothing: skip it (the higher
            // refinements would otherwise pay for pixels nobody sees).
            let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for (p, _) in &pts {
                lo_x = lo_x.min(p.x);
                hi_x = hi_x.max(p.x);
                lo_y = lo_y.min(p.y);
                hi_y = hi_y.max(p.y);
            }
            if hi_x < xform.rect.left() || lo_x > xform.rect.right() || hi_y < xform.rect.top() || lo_y > xform.rect.bottom() {
                continue;
            }
            let base = mesh.vertices.len() as u32;
            for (p, uv) in &pts {
                mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv: *uv, color: tint });
            }
            for k in 1..pts.len().saturating_sub(1) {
                mesh.add_triangle(base, base + k as u32, base + k as u32 + 1);
            }
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

/// The 1/2/5×10ⁿ step the endless grid's ladder picks for a raw target spacing.
fn nice_step(s: f64) -> f64 {
    let mag = 10f64.powf(s.max(1e-9).log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if m * mag >= s {
            return m * mag;
        }
    }
    10.0 * mag
}

/// The ground rectangle (min_x, max_x, min_z, max_z) the endless grid must cover for one
/// camera over one viewport: the screen's corners (and centre) un-projected onto the ground
/// plane, horizon-missed rays marched out to the frame's far plane, every hit clamped into
/// the Web Mercator world square — the grid covers the whole world and nothing past it.
/// Pure, so the endlessness is testable without a painter.
pub(crate) fn grid_extent(rig: &CameraRig, rect: Rect, y: f32) -> (f64, f64, f64, f64) {
    let corners = [
        pos2(rect.left(), rect.top()),
        pos2(rect.right(), rect.top()),
        pos2(rect.left(), rect.bottom()),
        pos2(rect.right(), rect.bottom()),
        rect.center(),
    ];
    let focus_ground: [f64; 2] = match rig {
        CameraRig::Orbit(c) => [f64::from(c.target[0]), f64::from(c.target[2])],
        CameraRig::Walk(c) => [f64::from(c.pos[0]), f64::from(c.pos[2])],
    };
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    // The march clamp: the rig's own far plane — the reach the frame actually renders. A ray
    // that meets the ground past it draws nothing, so the extent never needs to cover it; a
    // HIGH camera's far plane reaches kilometres out, which is exactly what a zoomed-out
    // grid must cover (the old fixed 60 km ring is what drew the grid as one square patch
    // under a far camera).
    let far_m = match rig {
        CameraRig::Orbit(c) => c.far_plane.max(c.dist * 8.0 + 400.0),
        CameraRig::Walk(_) => 120_000.0,
    } as f64;
    let mut min_x = f64::MAX;
    let mut max_x = f64::MIN;
    let mut min_z = f64::MAX;
    let mut max_z = f64::MIN;
    let mut hits = 0usize;
    // The world clamp (metres): every extent hit lands inside the Mercator world square — a
    // zoomed-out camera sees the grid run to the world's edges, not past them.
    let clamp_w = |v: f64| v.clamp(-WORLD_HALF_M, WORLD_HALF_M);
    for c in corners {
        let ndc = [((c.x - rect.left()) / rect.width()) * 2.0 - 1.0, -(((c.y - rect.top()) / rect.height()) * 2.0 - 1.0)];
        let ray = rig.ray_from_ndc(ndc, aspect);
        // On-plane hits clamp to the far plane (the rendered reach) so the extent stays
        // finite but still covers everything on screen; rays at/above the horizon march out
        // along their ground heading the same way.
        let hit = match ray_plane_y(&ray, y).map(|p| (p, (p - ray.origin).length())) {
            Some((_p, t)) => {
                let t = t.min(far_m as f32);
                Some([clamp_w(f64::from(ray.origin.x + ray.dir.x * t)), clamp_w(f64::from(ray.origin.z + ray.dir.z * t))])
            }
            None => {
                let (dx, dz) = (f64::from(ray.dir.x), f64::from(ray.dir.z));
                let len = (dx * dx + dz * dz).sqrt();
                if len < 1e-9 {
                    None
                } else {
                    Some([clamp_w(f64::from(ray.origin.x) + dx / len * far_m), clamp_w(f64::from(ray.origin.z) + dz / len * far_m)])
                }
            }
        };
        if let Some([x, z]) = hit {
            hits += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_z = min_z.min(z);
            max_z = max_z.max(z);
        }
    }
    if hits < 3 {
        // Degenerate — fall back to a generous box around the focus so some grid always shows.
        let far = 400.0f64;
        (min_x, max_x) = (focus_ground[0] - far, focus_ground[0] + far);
        (min_z, max_z) = (focus_ground[1] - far, focus_ground[1] + far);
    }
    // The whole extent lives inside the world square (the degenerate fallback clamped too).
    min_x = min_x.max(-WORLD_HALF_M);
    max_x = max_x.min(WORLD_HALF_M);
    min_z = min_z.max(-WORLD_HALF_M);
    max_z = max_z.min(WORLD_HALF_M);
    if min_x > max_x || min_z > max_z {
        // The focus itself sat past the world square: clamp the box onto the edge.
        let (fx, fz) = (focus_ground[0].clamp(-WORLD_HALF_M, WORLD_HALF_M), focus_ground[1].clamp(-WORLD_HALF_M, WORLD_HALF_M));
        (min_x, max_x) = (fx - 1.0, fx + 1.0);
        (min_z, max_z) = (fz - 1.0, fz + 1.0);
    }
    (min_x, max_x, min_z, max_z)
}

/// The viewport-filling "endless" ground grid — the EffectCraft viewer's grid (ported by way of
/// ArchCraft's `view3d.rs`): its extent comes from un-projecting the screen corners onto the
/// ground plane ([`grid_extent`]), and rays that miss the ground (at/above the horizon) march
/// out along their ground heading to a far clamp ring, so the grid always covers the whole
/// viewport and never visibly ends. The step adapts to zoom — the 1-2-5 ladder aiming at
/// [`GRID_TARGET_PX`] between minor lines, widened to stay inside the line budget at grazing
/// angles, minor lines dropped under [`GRID_MINOR_MIN_PX`] — a minor/major/super hierarchy
/// reads depth the way the reference's does, and every segment clips against the camera
/// frustum (four side planes + a near plane through the eye) so no endpoint behind the lens
/// mirrors across the screen. A haze band at the horizon melts the far line ends into the sky.
/// Drawn under the contours, the art and the fleet (the frame paints those after), it only
/// ever veils itself.
fn draw_ground_grid(painter: &egui::Painter, xform: &Xform, rect: Rect, rig: &CameraRig) {
    let y = 0.02f32;
    let (min_x, max_x, min_z, max_z) = grid_extent(rig, rect, y);
    let eye = rig.eye();

    // -- adaptive step: metres-per-pixel at the focus, ~[`GRID_TARGET_PX`] px between minor
    // lines, never past the line budget -------------------------------------------------------
    let span = (max_x - min_x).max(max_z - min_z).max(1.0);
    let focus = match rig {
        CameraRig::Orbit(c) => [c.target[0], c.target[2]],
        CameraRig::Walk(c) => [c.pos[0], c.pos[2]],
    };
    let m_per_px = xform
        .proj([focus[0], y, focus[1]])
        .zip(xform.proj([focus[0] + 1.0, y, focus[1]]))
        .map(|(a, b)| 1.0 / (b.x - a.x).abs().max(1e-3))
        .unwrap_or(1.0) as f64;
    let target = (GRID_TARGET_PX * m_per_px.max(1e-6)).max(span / GRID_MAX_LINES);
    let step = nice_step(target);
    let major_every = 5usize;
    let sub = step / major_every as f64;

    let minor_alpha = 34u8;
    let major_alpha = 84u8;
    let super_alpha = 140u8;
    let grid_col = |a: u8| Color32::from_black_alpha(a);

    let x0 = (min_x / sub).floor() * sub;
    let z0 = (min_z / sub).floor() * sub;
    let nx = (((max_x - x0) / sub).ceil().max(0.0) as usize).min(2400);
    let nz = (((max_z - z0) / sub).ceil().max(0.0) as usize).min(2400);

    // -- frustum clip: grid segments clip against the camera frustum (four Gribb–Hartmann
    // side planes + a near plane through the eye) so no endpoint ever lands behind the lens —
    // a behind-camera endpoint mirrors across the screen and tears the grid -------------------
    let vp = xform.vp.to_cols_array();
    let row = |r: usize| [f64::from(vp[r]), f64::from(vp[4 + r]), f64::from(vp[8 + r]), f64::from(vp[12 + r])];
    let (r0, r1, r3) = (row(0), row(1), row(3));
    let mut planes: [[f64; 4]; 5] = [
        [r3[0] + r0[0], r3[1] + r0[1], r3[2] + r0[2], r3[3] + r0[3]], // left
        [r3[0] - r0[0], r3[1] - r0[1], r3[2] - r0[2], r3[3] - r0[3]], // right
        [r3[0] + r1[0], r3[1] + r1[1], r3[2] + r1[2], r3[3] + r1[3]], // bottom
        [r3[0] - r1[0], r3[1] - r1[1], r3[2] - r1[2], r3[3] - r1[3]], // top
        [0.0, 1.0, 0.0, 0.0],                                         // near plane, set below
    ];
    // Near plane: dot(p - eye, forward) >= 0.3 m.
    let fwd3 = match rig {
        CameraRig::Orbit(c) => {
            let e = c.eye();
            Vec3::new(c.target[0] - e.x, c.target[1] - e.y, c.target[2] - e.z)
        }
        CameraRig::Walk(c) => c.dir(),
    };
    let fl = fwd3.length().max(1e-9) as f64;
    let f3 = [f64::from(fwd3.x) / fl, f64::from(fwd3.y) / fl, f64::from(fwd3.z) / fl];
    planes[4] = [f3[0], f3[1], f3[2], -(f3[0] * f64::from(eye.x) + f3[1] * f64::from(eye.y) + f3[2] * f64::from(eye.z)) + 0.3];
    let clip = |a: [f64; 2], b: [f64; 2]| -> Option<(Pos2, Pos2)> {
        let mut from = [a[0], f64::from(y), a[1]];
        let mut to = [b[0], f64::from(y), b[1]];
        for pl in planes.iter() {
            let df = pl[0] * from[0] + pl[1] * from[1] + pl[2] * from[2] + pl[3];
            let dt = pl[0] * to[0] + pl[1] * to[1] + pl[2] * to[2] + pl[3];
            if df < 0.0 && dt < 0.0 {
                return None;
            }
            if df < 0.0 {
                let t = df / (df - dt);
                from = [from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t, from[2] + (to[2] - from[2]) * t];
            } else if dt < 0.0 {
                let t = dt / (dt - df);
                to = [to[0] + (from[0] - to[0]) * t, to[1] + (from[1] - to[1]) * t, to[2] + (from[2] - to[2]) * t];
            }
        }
        let sa = xform.proj([from[0] as f32, y, from[2] as f32])?;
        let sb = xform.proj([to[0] as f32, y, to[2] as f32])?;
        Some((sa, sb))
    };

    let mut lines_minor: Vec<(Pos2, Pos2)> = Vec::new();
    let mut lines_major: Vec<(Pos2, Pos2)> = Vec::new();
    let mut lines_super: Vec<(Pos2, Pos2)> = Vec::new();
    // Major-ness comes from the absolute world coordinate (multiples of `sub`), so the grid is
    // stable while the camera pans.
    let classify = |v: f64| -> (bool, bool) {
        let i = (v / sub).round() as i64;
        let maj = i.rem_euclid(major_every as i64) == 0;
        let sup = i.rem_euclid((major_every * 5) as i64) == 0;
        (maj, sup)
    };
    for i in 0..=nx {
        let x = x0 + i as f64 * sub;
        let (maj, sup) = classify(x);
        if let Some((a, b)) = clip([x, z0], [x, max_z]) {
            if sup {
                lines_super.push((a, b));
            } else if maj {
                lines_major.push((a, b));
            } else {
                lines_minor.push((a, b));
            }
        }
    }
    for j in 0..=nz {
        let z = z0 + j as f64 * sub;
        let (maj, sup) = classify(z);
        if let Some((a, b)) = clip([x0, z], [max_x, z]) {
            if sup {
                lines_super.push((a, b));
            } else if maj {
                lines_major.push((a, b));
            } else {
                lines_minor.push((a, b));
            }
        }
    }
    // Minor lines only when they are ≥ [`GRID_MINOR_MIN_PX`] apart (the EffectCraft draw_minor).
    let minor_px = (step / major_every as f64 / m_per_px.max(1e-6)) as f32;
    if minor_px >= GRID_MINOR_MIN_PX {
        painter.extend(lines_minor.into_iter().map(|(a, b)| Shape::line_segment([a, b], egui::Stroke::new(1.0, grid_col(minor_alpha)))));
    }
    painter.extend(lines_major.into_iter().map(|(a, b)| Shape::line_segment([a, b], egui::Stroke::new(1.0, grid_col(major_alpha)))));
    painter.extend(lines_super.into_iter().map(|(a, b)| Shape::line_segment([a, b], egui::Stroke::new(1.0, grid_col(super_alpha)))));

    // The axis lines through the world origin (the scene anchor) across the visible range —
    // the gizmo's own red X and blue Z, stronger than the grid.
    if let Some((a, b)) = clip([x0, 0.0], [max_x, 0.0]) {
        painter.line_segment([a, b], egui::Stroke::new(1.4, Color32::from_rgb(0xd4, 0x4c, 0x47)));
    }
    if let Some((a, b)) = clip([0.0, z0], [0.0, max_z]) {
        painter.line_segment([a, b], egui::Stroke::new(1.4, Color32::from_rgb(0x47, 0x6e, 0xd4)));
    }

    // -- horizon haze: the far clamp ring converges at the horizon; a soft gradient there
    // melts the line ends into the sky instead of stopping mid-screen, and the strip above
    // the horizon is cleaned to pure sky. Only while the horizon is actually on screen (a
    // pitched-down orbit never sees it).
    let fwd = rig.forward_xz();
    let l = (fwd[0] * fwd[0] + fwd[1] * fwd[1]).sqrt();
    if l > 1e-6 {
        // Any point on the horizontal ray out of the eye projects onto the horizon line; the
        // distance only has to stay inside the far plane for the projection to answer.
        let d = xform.far * 0.9;
        if let Some(hp) = xform.proj([eye.x + (fwd[0] / l) * d, eye.y, eye.z + (fwd[1] / l) * d])
            && rect.top() <= hp.y
            && hp.y <= rect.bottom()
        {
            let bg = SKY;
            let sky_h = (hp.y - rect.top()).clamp(0.0, rect.height());
            if sky_h > 0.0 {
                painter.rect_filled(Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(rect.right(), rect.top() + sky_h)), 0.0, bg);
            }
            // The haze band fading out below the horizon.
            let band_top = hp.y.max(rect.top());
            let band_h = (rect.height() * 0.30).min(rect.bottom() - band_top);
            if band_h > 0.0 {
                let mut mesh = egui::Mesh::default();
                let yb = band_top + band_h;
                let (xl, xr) = (rect.left(), rect.right());
                let c0 = bg;
                let c1 = Color32::from_rgba_unmultiplied(SKY.r(), SKY.g(), SKY.b(), 0);
                mesh.colored_vertex(pos2(xl, band_top), c0);
                mesh.colored_vertex(pos2(xr, band_top), c0);
                mesh.colored_vertex(pos2(xl, yb), c1);
                mesh.colored_vertex(pos2(xr, yb), c1);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(2, 1, 3);
                painter.add(Shape::mesh(std::sync::Arc::new(mesh)));
            }
        }
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
/// covers it), its X/Y/Z on the env grid (world metres — where a point would land), the view's
/// own Web-Mercator zoom level (the same levels the 2D map reads, to z22), then the terrain
/// status line.
fn draw_readout(app: &TerrazoneApp, painter: &egui::Painter, rect: Rect, hover: Option<Pos2>) {
    let pad = 8.0;
    let font = FontId::proportional(10.0);
    let mut lines: Vec<String> = Vec::new();
    if let Some(hp) = hover
        && let Some(hit) = ground_pick(app, &rig_of(app), rect, hp)
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
        // The env-grid position of the pick: X east, Y up (the datum plane), Z south — the
        // numbers a point placed here keeps.
        lines.push(format!("X {:+.1} · Y {:+.1} · Z {:+.1} m", hit.x, hit.y, hit.z));
        // The view's own zoom level, the 2D map's rule read through the camera: the level the
        // 2D canvas would show at this ground scale (the drape's tiles follow it).
        let level = view_zoom_level(app, rect);
        lines.push(format!("z{:.1}", level));
    }
    lines.push(terrain_status(app));
    // The debug overlays' stats ride the readout: the imagery ring's own numbers (the
    // per-distance LOD made legible — a tilted camera reads a ZOOM RANGE, not one level)
    // and the placements' count.
    if app.ui.tiles_debug {
        lines.push(tile_ring_stats(app));
    }
    if app.ui.objects_debug {
        lines.push(object_stats(app));
    }
    let galley = painter.layout_no_wrap(lines.join("\n"), font, Color32::WHITE);
    let pos = pos2(rect.left() + pad, rect.top() + pad);
    painter.rect_filled(Rect::from_min_size(pos - vec2(3.0, 2.0), galley.size() + vec2(6.0, 4.0)), 3.0, Color32::from_black_alpha(150));
    painter.galley(pos, galley, Color32::WHITE);
}

/// The imagery ring's stats line: how many tiles the frame's footprint asked, the zoom range
/// they span (a tilted camera's LOD mix — near ground fine, far field coarse — reads here as
/// a RANGE, never one level) and how many are still on their way.
fn tile_ring_stats(app: &TerrazoneApp) -> String {
    let Some(rt) = app.canvas.dem.as_ref() else { return crate::i18n::fmt(tl!("Tiles {n} · z{a}–z{b}"), &[("n", "0"), ("a", "0"), ("b", "0")]) };
    let n = rt.imagery.ring.len();
    let (zmin, zmax) = rt.imagery.ring.iter().fold((u8::MAX, 0u8), |(a, b), (t, _)| (a.min(t.z), b.max(t.z)));
    let pending = rt
        .imagery
        .ring
        .iter()
        .filter(|(t, _)| matches!(rt.imagery.tiles.get(&tile_key(*t)).map(|s| &s.state), Some(ImageryState::Pending)))
        .count();
    if pending > 0 {
        crate::i18n::fmt(
            tl!("Tiles {n} · z{a}–z{b} · {c} loading"),
            &[("n", &n.to_string()), ("a", &zmin.to_string()), ("b", &zmax.to_string()), ("c", &pending.to_string())],
        )
    } else {
        crate::i18n::fmt(tl!("Tiles {n} · z{a}–z{b}"), &[("n", &n.to_string()), ("a", &zmin.to_string()), ("b", &zmax.to_string())])
    }
}

/// The placements' stats line: how many objects stand in the scene and how many of them are
/// cameras (the ones with an aim and a preview).
fn object_stats(app: &TerrazoneApp) -> String {
    let (n, cams) = app
        .session
        .active()
        .map(|st| {
            let n = st.doc.placements.placements.len();
            let cams = st.doc.placements.placements.iter().filter(|p| p.kind.is_camera()).count();
            (n, cams)
        })
        .unwrap_or((0, 0));
    crate::i18n::fmt(tl!("Objects {n} · cameras {c}"), &[("n", &n.to_string()), ("c", &cams.to_string())])
}

/// The debug overlay's colour for a tile's zoom: one hue per band, coarse far ground green
/// through to the finest near ground violet, so a tilted view's LOD mix reads at a glance.
fn zoom_band_color(z: u8) -> Color32 {
    match z {
        0..=13 => Color32::from_rgb(0x74, 0xc0, 0x6e),
        14..=16 => Color32::from_rgb(0xd9, 0xc1, 0x48),
        17..=18 => Color32::from_rgb(0xe8, 0x8b, 0x3a),
        19..=20 => Color32::from_rgb(0xe0, 0x54, 0x4c),
        _ => Color32::from_rgb(0xb0, 0x6e, 0xdc),
    }
}

/// The tile debug overlay: every ring tile's boundary drawn on the ground, coloured by its
/// zoom band ([`zoom_band_color`]; a tile still loading draws dim), its `z` labelled at the
/// centre when the tile is big enough on screen — the drape's per-distance LOD made visible.
fn draw_tile_debug(app: &TerrazoneApp, painter: &egui::Painter, xform: &Xform) {
    let Some(rt) = app.canvas.dem.as_ref() else { return };
    let ring = rt.imagery.ring.clone();
    if ring.is_empty() {
        return;
    }
    let m = scene_mercator(app);
    let font = FontId::proportional(9.0);
    for (t, _) in &ring {
        let pending = matches!(rt.imagery.tiles.get(&tile_key(*t)).map(|s| &s.state), Some(ImageryState::Pending));
        let col = if pending { Color32::from_white_alpha(70) } else { zoom_band_color(t.z) };
        let stroke = Stroke::new(1.0, col);
        // The tile's corners on the ground (doc pts → lon/lat → world → screen).
        let (tx0, ty0, tx1, ty1) = t.rect_pt();
        let corner = |x: f64, y: f64| -> Option<Pos2> {
            let (lon, lat) = doc_to_lonlat(x, y);
            let (wx, wy) = m.project(lon, lat);
            xform.proj([wx as f32, SKIN_LIFT, -(wy as f32)])
        };
        let cs = [corner(tx0, ty0), corner(tx1, ty0), corner(tx1, ty1), corner(tx0, ty1)];
        for i in 0..4 {
            if let (Some(a), Some(b)) = (cs[i], cs[(i + 1) % 4]) {
                painter.add(Shape::line_segment([a, b], stroke));
            }
        }
        // The zoom label at the tile's centre, when the tile is big enough on screen to
        // carry one (far tiles would smear into label soup).
        let Some(centre) = corner((tx0 + tx1) / 2.0, (ty0 + ty1) / 2.0) else { continue };
        let span = cs.iter().flatten().map(|p| p.distance(centre)).fold(0.0f32, f32::max);
        if span < 26.0 {
            continue;
        }
        let text = format!("z{}", t.z);
        let galley = painter.layout_no_wrap(text, font.clone(), col);
        let pos = centre - galley.size() * 0.5;
        painter.rect_filled(Rect::from_min_size(pos - vec2(2.0, 1.0), galley.size() + vec2(4.0, 2.0)), 2.0, Color32::from_black_alpha(130));
        painter.galley(pos, galley, col);
    }
}

/// The object debug overlay: every placed asset wears `#id kind` over its ground point (a
/// camera adds its aim — rotation, pitch, fov, the values `placements.update` re-aims with)
/// above a small ground cross — the scene's standing objects made countable.
fn draw_object_debug(app: &TerrazoneApp, painter: &egui::Painter, xform: &Xform) {
    let Some(st) = app.session.active() else { return };
    if st.doc.placements.placements.is_empty() {
        return;
    }
    let font = FontId::proportional(9.0);
    for p in &st.doc.placements.placements {
        let at = placement_world(app, p);
        let Some(sp) = xform.proj(at) else { continue };
        let cross = Stroke::new(1.2, Color32::from_rgb(0x2e, 0xc2, 0x7e));
        painter.add(Shape::line_segment([sp - vec2(5.0, 0.0), sp + vec2(5.0, 0.0)], cross));
        painter.add(Shape::line_segment([sp - vec2(0.0, 5.0), sp + vec2(0.0, 5.0)], cross));
        let mut text = format!("#{} {}", p.id, p.kind.label());
        if p.kind.is_camera() {
            text.push_str(&format!(" · rot {:.0}° pitch {:.0}° fov {:.0}°", p.rotation_deg, p.pitch_deg, p.fov_deg));
        }
        let galley = painter.layout_no_wrap(text, font.clone(), Color32::WHITE);
        let pos = pos2(sp.x - galley.size().x * 0.5, sp.y - galley.size().y - 8.0);
        painter.rect_filled(Rect::from_min_size(pos - vec2(2.0, 1.0), galley.size() + vec2(4.0, 2.0)), 2.0, Color32::from_black_alpha(140));
        painter.galley(pos, galley, Color32::WHITE);
    }
}

/// The 3D view's Web-Mercator zoom level — the 2D map's own rule read through the camera:
/// metres per screen px at the orbit target (or the walk rig's height), turned into the
/// level the 2D canvas would show at that ground scale. Clamped to the map's [`terrazon_map::MAX_ZOOM`].
pub(crate) fn view_zoom_level(app: &TerrazoneApp, rect: Rect) -> f64 {
    let mpp = metres_per_px(app, rect);
    let lat = view_centre_lonlat(app).1;
    zoom_level_of(mpp, lat)
}

/// The zoom level for a ground metres-per-screen-px at a latitude: the 2D rule
/// (`z = REF_ZOOM + log2(scale)`) with the scale solved from the Mercator's ground metres
/// per document point — `scale = m_per_pt·cos φ / mpp`, one rule, both canvases.
pub(crate) fn zoom_level_of(mpp: f64, lat: f64) -> f64 {
    let m_per_pt = 360.0 / terrazon_map::WORLD_PT * 111_320.0 * lat.to_radians().cos().max(0.05);
    let scale = m_per_pt / mpp.max(1e-9);
    (terrazon_map::REF_ZOOM as f64 + scale.log2()).clamp(0.0, f64::from(terrazon_map::MAX_ZOOM))
}

/// Ground metres per screen px at the camera's focus (the orbit target, or the walk rig's
/// height-driven stand-in — the drape's own distance input).
fn metres_per_px(app: &TerrazoneApp, rect: Rect) -> f64 {
    let rig = rig_of(app);
    let dist = match rig {
        CameraRig::Orbit(c) => f64::from(c.dist),
        CameraRig::Walk(w) => f64::from(w.pos[1] * 2.5),
    };
    let fov = match rig {
        CameraRig::Orbit(c) => c.fov_y,
        CameraRig::Walk(w) => w.fov_y,
    };
    2.0 * dist * f64::from(fov.tan().max(0.01)) / f64::from(rect.height().max(1.0))
}

/// The camera preview (the render view the EffectCraft tool drives): the hovered camera's
/// own scene, rendered through its rig into a picture-in-picture at the canvas's bottom-right
/// — the same passes the main frame runs (slab, grid, imagery, the doc's draped art, the
/// fleet), one smaller projection standing where the camera stands, looking where it looks.
/// The frame, the label (the id and its aim) and the ground ring ride around it; hover
/// another camera to look through that one.
fn draw_camera_pip(app: &mut TerrazoneApp, ui: &mut egui::Ui, full: Rect, cam: &AssetPlacement, grid_on: bool) {
    let (w, h) = (320.0f32, 200.0f32);
    let margin = 12.0;
    let rect = Rect::from_min_size(pos2(full.right() - w - margin, full.bottom() - h - margin), vec2(w, h));
    if !full.contains(rect.min) || !full.contains(rect.max) {
        return;
    }
    let aspect = rect.width() / rect.height().max(1.0);
    let Some(rig) = camera_rig(app, cam, aspect) else { return };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, SKY);

    // The camera's own frame: the same ground passes, one smaller projection.
    let xform = Xform::for_cam(rect, &rig);
    let focus = match rig {
        CameraRig::Walk(w2) => [w2.pos[0], w2.pos[2]],
        _ => [0.0, 0.0],
    };
    let slab_half = match rig {
        CameraRig::Walk(w2) => (w2.pos[1] * 40.0).clamp(2_000.0, WORLD_HALF_M as f32),
        _ => SLAB_HALF,
    };
    draw_slab(&painter, &xform, focus, slab_half, [0.55, 0.53, 0.48]);
    if grid_on {
        draw_ground_grid(&painter, &xform, rect, &rig);
    }
    // The preview dresses its own street-scale ring (the main frame's ring is coarse at the
    // aerial distances the main camera sits at). One zoom for the whole preview: every tile
    // rides at ratio 1.0, so the draw's far-first order keeps their own relative order.
    let preview_ring = app
        .canvas
        .dem
        .as_ref()
        .map(|rt| rt.imagery.preview_ring.iter().map(|t| (*t, 1.0)).collect::<Vec<_>>())
        .unwrap_or_default();
    draw_imagery_ring(app, &painter, &xform, &preview_ring, false);
    draw_doc_art(app, &painter, &xform);
    let mut buckets: [Vec<Tri>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut drawn = 0usize;
    draw_fleet(app, &mut buckets, &mut drawn, &xform);
    flush_bucket(&buckets[1], &painter);
    flush_bucket(&buckets[2], &painter);

    // The frame and the label: what this is, which camera, its aim.
    let stroke = egui::Stroke::new(2.0, Color32::from_black_alpha(160));
    painter.rect_stroke(rect.shrink(1.0), 2.0, stroke, egui::StrokeKind::Middle);
    let label = format!("{} {} · {}° / {}°", tl!("Camera preview"), cam.id, cam.fov_deg.round() as i32, -cam.pitch_deg.round() as i32);
    let font = FontId::proportional(10.0);
    let galley = painter.layout_no_wrap(label, font, Color32::WHITE);
    let pos = pos2(rect.left() + 8.0, rect.top() + 6.0);
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
    app.status(tl!("3D environment — drag to orbit · Shift-drag to pan · wheel to zoom · the Camera panel switches to Walk (WASD)"));
}

/// Switch the 3D environment's camera mode — the EffectCraft viewer's pair. The switch hands
/// the view over instead of jumping: walking from an orbit stands the walk rig at the orbit
/// eye looking the orbit's way; returning re-aims the orbit from where the walk stood, so both
/// directions keep the scene on screen.
pub fn set_cam_mode(app: &mut TerrazoneApp, mode: CamMode) {
    if app.ui.cam_mode == mode {
        return;
    }
    match mode {
        CamMode::Walk => {
            let w = WalkCam::from_orbit(&app.ui.cam3d);
            app.ui.walk3d = w;
            app.status(tl!("Walk mode — W/A/S/D to move · Space-drag to look · Q/E down/up · Shift runs · the wheel lifts"));
        }
        CamMode::Orbit => {
            let c = OrbitCamera::from_walk(&app.ui.walk3d);
            app.ui.cam3d = c;
            app.status(tl!("Orbit camera — drag to orbit · Shift-drag to pan · wheel to zoom · double-click recentres"));
        }
    }
    app.ui.cam_mode = mode;
    if let Some(rt) = app.canvas.dem.as_mut() {
        rt.interacted = true;
    }
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
/// to the camera, exactly as before. In Walk mode (the EffectCraft camera) the same camera
/// gestures look instead of orbit, and the keyboard drives the rig: W/A/S/D walks forward,
/// left, back, right, Q/E sinks or lifts, Shift sprints, the wheel raises or lowers.
fn handle_input(app: &mut TerrazoneApp, ui: &mut egui::Ui, resp: &egui::Response, rect: Rect, hover: Option<Pos2>) {
    let m = ui.input(|i| i.modifiers);
    let delta = ui.input(|i| i.pointer.delta());
    let tool = app.session.tool_id().to_string();
    let space = ui.input(|i| i.key_down(egui::Key::Space));
    let nav_tool = matches!(tool.as_str(), "hand" | "zoom" | "rotateView");
    let cam_gesture = nav_tool || space;
    let walking = app.ui.cam_mode == CamMode::Walk;
    // The wheel: dolly (zoom toward the target) whatever the tool — the walk rig lifts instead
    // (the reference's walk wheel). `zoom_delta` covers Cmd-wheel and pinch; the plain wheel
    // arrives as scroll, zooming in (camera closer) scrolling up, like the 2D canvas.
    let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y));
    let mut moved = false;
    if walking {
        if scroll.abs() > 0.01 {
            app.ui.walk3d.lift(scroll * WALK_WHEEL_LIFT);
            moved = true;
        } else if (zoom - 1.0).abs() > 1.0e-6 {
            app.ui.walk3d.lift((zoom - 1.0) * 120.0);
            moved = true;
        }
    } else {
        let factor = if (zoom - 1.0).abs() > 1.0e-6 {
            1.0 / zoom
        } else if scroll.abs() > 0.01 {
            if scroll > 0.0 { 1.0 / DOLLY } else { DOLLY }
        } else {
            1.0
        };
        if (factor - 1.0).abs() > 1.0e-6 {
            app.ui.cam3d.dolly_by(factor);
            moved = true;
        }
    }
    if cam_gesture {
        if resp.dragged_by(egui::PointerButton::Primary) {
            if walking {
                // Drag looks (the reference's grab-the-world: dragging down looks up).
                app.ui.walk3d.look(delta.x * WALK_LOOK_SPEED, delta.y * WALK_LOOK_SPEED);
            } else if m.shift {
                app.ui.cam3d.pan(delta.x, delta.y);
            } else {
                // Yaw increases dragging right; dragging up raises the pitch (toward top-down).
                app.ui.cam3d.orbit(delta.x * ORBIT_SPEED, -delta.y * ORBIT_SPEED);
            }
            moved = true;
        }
        if resp.dragged_by(egui::PointerButton::Middle) {
            if walking {
                app.ui.walk3d.look(delta.x * WALK_LOOK_SPEED, delta.y * WALK_LOOK_SPEED);
            } else {
                app.ui.cam3d.pan(delta.x, delta.y);
            }
            moved = true;
        }
        let dbl = resp.double_clicked();
        if dbl && let Some(p) = hover {
            recentre(app, rect, p);
            moved = true;
        }
    } else {
        // Drawing / editing. Alt-drag still orbits — or looks, in walk mode — (the camera's
        // escape hatch with a tool up); the middle button pans; the primary button belongs to
        // the tool.
        if m.alt && resp.dragged_by(egui::PointerButton::Primary) {
            if walking {
                app.ui.walk3d.look(delta.x * WALK_LOOK_SPEED, delta.y * WALK_LOOK_SPEED);
            } else {
                app.ui.cam3d.orbit(delta.x * ORBIT_SPEED, -delta.y * ORBIT_SPEED);
            }
            moved = true;
        } else if resp.dragged_by(egui::PointerButton::Middle) {
            if walking {
                app.ui.walk3d.look(delta.x * WALK_LOOK_SPEED, delta.y * WALK_LOOK_SPEED);
            } else {
                app.ui.cam3d.pan(delta.x, delta.y);
            }
            moved = true;
        } else {
            dispatch_tool(app, ui, resp, rect, hover);
        }
    }
    // The walk keys: the EffectCraft camera's drive. They read while the canvas is under the
    // pointer (the wheel's own gate), so typing into a panel never walks the camera.
    if walking && resp.hovered() {
        let dt = ui.input(|i| i.unstable_dt).min(0.25);
        let (mut fwd, mut side) = (0.0f32, 0.0f32);
        ui.input(|i| {
            if i.key_down(egui::Key::W) {
                fwd += 1.0;
            }
            if i.key_down(egui::Key::S) {
                fwd -= 1.0;
            }
            if i.key_down(egui::Key::D) {
                side += 1.0;
            }
            if i.key_down(egui::Key::A) {
                side -= 1.0;
            }
        });
        if fwd != 0.0 || side != 0.0 {
            let sprint = if m.shift { WALK_SPRINT } else { 1.0 };
            app.ui.walk3d.walk(fwd, side, WALK_SPEED * sprint * dt);
            moved = true;
        }
        let mut sink = false;
        let mut rise = false;
        ui.input(|i| {
            sink = i.key_down(egui::Key::Q);
            rise = i.key_down(egui::Key::E);
        });
        if sink || rise {
            app.ui.walk3d.lift((rise as i32 - sink as i32) as f32 * WALK_LIFT_SPEED * dt);
            moved = true;
        }
        if moved {
            ui.ctx().request_repaint();
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
        let cam: CameraRig = ui.data(|d| d.get_temp(press_cam_id()))?;
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
        ui.data_mut(|d| d.insert_temp(press_cam_id(), rig_of(app)));
        let ev = terrazon_tools::PointerEvent {
            kind: terrazon_tools::PointerKind::Down,
            pos: doc.0,
            mods: crate::canvas::mods(m, false),
            pressure: crate::canvas::pen_pressure(ui, true),
        };
        crate::canvas::dispatch(app, &ev, view);
    } else if pointer.primary_down() && ui.data(|d| d.get_temp::<CameraRig>(press_cam_id()).is_some()) {
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
    } else if !pointer.primary_down() && ui.data(|d| d.get_temp::<CameraRig>(press_cam_id()).is_some()) {
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
        ui.data_mut(|d| d.remove::<CameraRig>(press_cam_id()));
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

/// Double-click: the camera target recentres on the clicked ground point (the orbit arm's
/// gesture — the walk rig moves with its own keys, so a walk double-click stays a no-op).
fn recentre(app: &mut TerrazoneApp, rect: Rect, p: Pos2) {
    if app.ui.cam_mode != CamMode::Orbit {
        return;
    }
    let cam = app.ui.cam3d;
    let Some(hit) = ground_pick(app, &CameraRig::Orbit(cam), rect, p) else { return };
    app.ui.cam3d.target = [hit.x, 0.0, hit.z];
}

/// A click with the placement tool active: place the kind at the picked ground point (its
/// elevation is sampled right away when the DEM covers it; the tool stays active).
/// The document point a screen position picks on the ground: the picked world point's lon/lat
/// through the document's Mercator — the same doc coordinates the 2D canvas draws with, so a
/// stroke drawn here lands exactly where the flat map puts it.
fn ground_to_doc(app: &TerrazoneApp, rect: Rect, p: Pos2) -> Option<Point> {
    let hit = ground_pick(app, &rig_of(app), rect, p)?;
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
    let xform = Xform { vp: cam.proj(aspect) * cam.view(), rect: inner, far: cam.far_plane.max(cam.dist * 8.0 + 400.0) };

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

    /// The endless grid's ladder: the step keeps the 1-2-5×10ⁿ mantissa whatever raw target it
    /// is handed, and grows with the target.
    #[test]
    fn the_endless_grid_step_climbs_the_one_two_five_ladder() {
        for (target, want) in [(0.7, 1.0), (1.7, 2.0), (4.2, 5.0), (7.9, 10.0), (31.0, 50.0), (140.0, 200.0)] {
            let s = nice_step(target);
            assert!((s - want).abs() < 1e-9, "nice_step({target}) = {s}, want {want}");
        }
        assert!(nice_step(4.0) < nice_step(40.0) && nice_step(40.0) < nice_step(400.0));
    }

    /// The endless grid's extent never visibly ends: a camera looking level (every corner ray
    /// above the horizon) still covers kilometres of ground ahead — the horizon rays march to
    /// the far ring instead of shrinking the grid to a finite patch.
    #[test]
    fn the_endless_grid_covers_the_ground_even_looking_level() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0));
        // A walk rig at eye height looking level (pitch 0): every corner ray misses the ground.
        let walk = CameraRig::Walk(WalkCam { pos: [0.0, 1.6, 0.0], yaw: std::f32::consts::PI, pitch: 0.0, fov_y: 65.0f32.to_radians() });
        let (min_x, max_x, min_z, max_z) = grid_extent(&walk, rect, 0.0);
        assert!(min_z < -40_000.0, "looking north (−z) the extent reaches kilometres ahead: {min_z}");
        assert!((max_z - min_z) > 40_000.0 && (max_x - min_x) > 10_000.0, "the coverage is vast, not a patch: x {min_x}..{max_x} z {min_z}..{max_z}");
        // The extent stays finite (the far plane clamps it) so the line budget holds.
        assert!(max_x.abs() <= 130_000.0 && max_z.abs() <= 130_000.0, "the walk rig's far plane bounds the march: x {max_x} z {max_z}");
        // And a normal pitched-down orbit covers its own view.
        let orbit = CameraRig::Orbit(OrbitCamera { target: [0.0, 0.0, 0.0], dist: 900.0, yaw: 0.6, pitch: 0.62, ..Default::default() });
        let (o0, o1, o2, o3) = grid_extent(&orbit, rect, 0.0);
        assert!(o1 > o0 && o3 > o2, "the orbit's extent is a real rectangle");
        assert!(o1 - o0 > 200.0, "a 900 m orbit sees hundreds of metres across: {}", o1 - o0);
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
        let hit = ground_pick(&app, &CameraRig::Orbit(cam), rect, centre).expect("a pitched ray crosses the datum");
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
        assert!(ground_pick(&app, &CameraRig::Orbit(cam), rect, pos2(400.0, 40.0)).is_none(), "the sky picks nothing");
    }

    /// Switching camera modes hands the view over: orbit → walk stands the rig at the eye
    /// looking the orbit's way; walk → orbit re-aims the arm from where the walk stood. And
    /// the mode round-trips through the command surface.
    #[test]
    fn switching_camera_modes_hands_the_view_over() {
        let mut app = app_with_map();
        app.run("view.mode3d", serde_json::json!({ "on": true })).unwrap();
        app.ui.cam3d.dist = 400.0;
        let eye = app.ui.cam3d.eye();
        crate::view3d::set_cam_mode(&mut app, CamMode::Walk);
        assert_eq!(app.ui.cam_mode, CamMode::Walk);
        assert!((app.ui.walk3d.pos[0] - eye.x).abs() < 0.01 && (app.ui.walk3d.pos[2] - eye.z).abs() < 0.01, "the walk stands where the orbit was");
        assert!(app.ui.walk3d.pos[1] >= 0.4, "the eye height never sinks under the datum");
        // The walk drives; switching back aims the orbit down the walk's own look.
        app.ui.walk3d.walk(1.0, 0.0, 30.0);
        app.ui.walk3d.look(0.4, -0.1);
        let look_w = app.ui.walk3d.dir();
        crate::view3d::set_cam_mode(&mut app, CamMode::Orbit);
        assert_eq!(app.ui.cam_mode, CamMode::Orbit);
        let look_o = (glam::Vec3::from(app.ui.cam3d.target) - app.ui.cam3d.eye()).normalize_or_zero();
        assert!(look_w.dot(look_o) > 0.95, "the orbit looks where the walk looked: {look_w:?} vs {look_o:?}");
        // Same mode twice is a no-op (the handoff never resets a standing camera).
        let before = app.ui.cam3d;
        crate::view3d::set_cam_mode(&mut app, CamMode::Orbit);
        assert_eq!(app.ui.cam3d, before);
    }

    /// A tilted camera asks each ground distance its own zoom: the near field (much closer
    /// than the orbit focus) reads finer tiles than the far field, every zoom inside the
    /// source's range — one zoom for the whole view was the angled camera's smear.
    #[test]
    fn the_tilted_drape_asks_each_distance_its_own_zoom() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0));
        let cam = terrazon_scene3d::OrbitCamera {
            target: [0.0, 0.0, 0.0],
            dist: 1500.0,
            pitch: 20_f32.to_radians(),
            fov_y: 60_f32.to_radians(),
            ..Default::default()
        };
        let xf = Xform::for_cam(rect, &terrazon_scene3d::CameraRig::Orbit(cam));
        let anchor = (39.2236_f64, -6.7780_f64);
        let src = terrazon_map::TileSource {
            id: "test".into(),
            name: "Test".into(),
            template: "https://example.test/{z}/{x}/{y}.png".into(),
            min_zoom: 0,
            max_zoom: 22,
            ..Default::default()
        };
        let mpp = 2.0 * f64::from(cam.dist) * f64::from(cam.fov_y.tan()) / f64::from(rect.height());
        let ring = imagery_ring_footprint(anchor, Some(&xf), &src, 22, mpp).expect("a tilted camera sees ground");
        assert!(!ring.is_empty());
        let zooms: std::collections::BTreeSet<u8> = ring.iter().map(|(t, _)| t.z).collect();
        assert!(zooms.len() >= 2, "a tilted view asks more than one zoom: {zooms:?}");
        let z_focus = drape_zoom_view(mpp, anchor.1, &src, 22);
        let z_max = *zooms.last().unwrap();
        assert!(z_max >= z_focus + 1, "the near ground asks finer than the focus serves: {z_max} vs {z_focus}");
        assert!(*zooms.first().unwrap() < z_max, "the far field stays coarser than the near");
        for (t, _) in &ring {
            assert!(t.z >= src.min_zoom && t.z <= src.max_zoom, "every zoom inside the source: {}", t.z);
        }
        // Nearest first: the ring's ratios ascend with the tiles' zooms (near = fine).
        for w in ring.windows(2) {
            assert!(w[0].1 <= w[1].1, "the ring sorts nearest first: {:?}", w);
        }
    }

    /// A ground cell straddling the eye's plane clips to its visible part: every surviving
    /// vertex sits in front of the near plane, the UVs ride the cut — the drape's near field
    /// keeps painting instead of cutting a bare hole at the camera's feet.
    #[test]
    fn a_ground_cell_straddling_the_eye_still_paints() {
        let quad = [
            (glam::Vec4::new(-1.0, -1.0, 0.5, -2.0), egui::pos2(0.0, 0.0)), // behind the eye
            (glam::Vec4::new(1.0, -1.0, 0.5, 2.0), egui::pos2(1.0, 0.0)),
            (glam::Vec4::new(1.0, 1.0, 0.5, 4.0), egui::pos2(1.0, 1.0)),
            (glam::Vec4::new(-1.0, 1.0, 0.5, 6.0), egui::pos2(0.0, 1.0)),
        ];
        let out = clip_quad_near(quad);
        assert!(out.len() >= 3, "the visible part survives the cut: {}", out.len());
        for (v, uv) in &out {
            assert!(v.w > NEAR_W, "every survivor sits in front: {}", v.w);
            assert!((-0.001..=1.001).contains(&uv.x) && (-0.001..=1.001).contains(&uv.y), "the uvs stay on the tile: {uv:?}");
        }
        // A quad wholly behind the eye clips to nothing at all.
        let gone = clip_quad_near([
            (glam::Vec4::new(-1.0, -1.0, 0.5, -4.0), egui::pos2(0.0, 0.0)),
            (glam::Vec4::new(1.0, -1.0, 0.5, -2.0), egui::pos2(1.0, 0.0)),
            (glam::Vec4::new(1.0, 1.0, 0.5, -6.0), egui::pos2(1.0, 1.0)),
            (glam::Vec4::new(-1.0, 1.0, 0.5, -8.0), egui::pos2(0.0, 1.0)),
        ]);
        assert!(gone.is_empty(), "a fully-behind cell paints nothing: {}", gone.len());
    }
}
