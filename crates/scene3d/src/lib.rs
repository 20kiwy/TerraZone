//! TerraZone's 3D environment — the pieces every 3D surface shares, no egui inside.
//!
//! Copied from the ArchCraft (effect) project's proven 3D stack and adapted to TerraZone's
//! Mercator world: a WebGL-free egui-painter viewport draws what these modules build.
//!
//! * [`mercator`] — THE one Mercator projection for the 3D world: spherical Web Mercator
//!   rescaled to ground metres at a scene centre, so 1 world unit = 1 ground metre and every
//!   tool that knows lat/lon lands on the terrain (`x` east, `z` south, `y` up).
//! * [`dem`] — the DEM ("terrainium") load: AWS Terrarium elevation tiles (no key) fetched
//!   around the view, decoded (`e = R·256 + G + B/256 − 32768`), stitched into a [`dem::DemGrid`]
//!   with Mercator-correct rows and gap-filled; the host implements [`dem::DemFetcher`] with its
//!   own HTTP workers + disk cache.
//! * [`terrain`] — marching-squares contours over a [`dem::DemGrid`] (with each major line's
//!   height for its label): the one terrain render the 3D environment does — the ground itself
//!   drapes flat, and the relief reads from the contour lines.
//! * [`camera`] — the cameras and picking rays, framework-free (a port of ArchCraft's): the
//!   orbit arm, the EffectCraft viewer's first-person walk rig ([`camera::WalkCam`], WASD +
//!   drag look), the [`camera::CamMode`] pair and the [`camera::CameraRig`] that renders
//!   either of them.
//! * [`mesh`] — [`mesh::Mesh3d`] and its builders: textured ground quads, extruded building
//!   rings, boxes.
//! * [`prim`] — parametric asset meshes: the textured house, the stadium, trees, pins.
//!
//! Heights are TRUE metres; the ground drapes flat at the datum. The asset catalogue and the
//! document's placements live in `terrazon-assets`; this crate renders them.

use serde::{Deserialize, Serialize};

pub mod camera;
pub mod dem;
pub mod mercator;
pub mod mesh;
pub mod prim;
pub mod terrain;

pub use camera::{CamMode, CameraRig, OrbitCamera, Ray, WalkCam, ray_aabb, ray_plane_y};
pub use dem::{DEM_ZOOM, DemFetcher, DemGrid, DemMsg};
pub use mercator::{EARTH_C, Mercator, lonlat_to_tile, tile_span_m};
pub use mesh::{Mesh3d, MeshLayer};

/// The 3D environment's terrain settings — the contour interval (the only terrain render left:
/// the DEM elevation mesh is gone, the ground drapes flat) and the imagery zoom the drape
/// starts at. These are about RENDERING the ground, so they live with the renderer; the
/// placements themselves live in the document (`terrazon_assets::AssetsState`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerrainSettings {
    /// Contour interval in metres (0 = off).
    pub contour_interval: f32,
    /// The imagery zoom the drape starts from (the camera refines it).
    pub imagery_zoom: u8,
}

impl Default for TerrainSettings {
    fn default() -> Self {
        Self { contour_interval: 10.0, imagery_zoom: 16 }
    }
}
