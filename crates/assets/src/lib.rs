//! TerraZone Assets: the placement catalogue and the document's placed assets.
//!
//! One catalogue, every canvas: an asset (a tree, a house, a stadium…) is placed by the
//! Assets tool — a click drops one, a drag brushes a row of them the way a brush lays paint —
//! and the same placement draws as a vector glyph on the 2D map and as a standing mesh in the
//! 3D environment. The catalogue is data, not files: every silhouette is generated in code, so
//! the crate stays network-free and tiny, and the placements live in the document (saved with
//! the `.terrazone` file, gated by the layer engine like everything else on the map).

use serde::{Deserialize, Serialize};

/// What can be placed. The catalogue is small and honest: six silhouettes that cover the
/// props a site plan needs, each with a real-world footprint — and the camera, the object
/// the 3D environment previews through (a CCTV-style camera on a mast, aimed by its own
/// yaw/pitch/fov).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    #[default]
    House,
    Building,
    Tree,
    Stadium,
    Pin,
    Camera,
}

impl AssetKind {
    pub const ALL: [AssetKind; 6] = [AssetKind::House, AssetKind::Building, AssetKind::Tree, AssetKind::Stadium, AssetKind::Pin, AssetKind::Camera];

    /// The name the tool and the readouts show.
    pub fn label(self) -> &'static str {
        match self {
            AssetKind::House => "House",
            AssetKind::Building => "Building",
            AssetKind::Tree => "Tree",
            AssetKind::Stadium => "Stadium",
            AssetKind::Pin => "Pin",
            AssetKind::Camera => "Camera",
        }
    }

    /// Parse from a command param / JSON (label or lowercase id, case-insensitive).
    pub fn parse(s: &str) -> Option<AssetKind> {
        let s = s.trim();
        AssetKind::ALL.into_iter().find(|k| k.label().eq_ignore_ascii_case(s) || format!("{k:?}").eq_ignore_ascii_case(s))
    }

    /// The real-world footprint `[width, depth]` in metres — the 2D glyph scales by it and the
    /// 3D mesh stands on it.
    pub fn size_m(self) -> [f32; 2] {
        match self {
            AssetKind::House => [10.0, 8.0],
            AssetKind::Building => [16.0, 12.0],
            AssetKind::Tree => [4.0, 4.0],
            AssetKind::Stadium => [140.0, 110.0],
            AssetKind::Pin => [1.0, 1.0],
            AssetKind::Camera => [3.0, 3.0],
        }
    }

    /// The camera kinds' aim defaults (a placement stores its own; `new` starts here): the
    /// look pitch below the horizon and the horizontal field of view, both degrees. The
    /// street-camera pair the CCTV samples ride.
    pub fn default_pitch_deg(self) -> f32 {
        match self {
            AssetKind::Camera => -35.0,
            _ => 0.0,
        }
    }

    pub fn default_fov_deg(self) -> f32 {
        match self {
            AssetKind::Camera => 60.0,
            _ => 0.0,
        }
    }

    /// Is this kind a camera the 3D environment previews through?
    pub fn is_camera(self) -> bool {
        matches!(self, AssetKind::Camera)
    }
}

impl std::fmt::Display for AssetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// One catalogue entry for the tool's picker: what it is, what icon names it, one line on it.
#[derive(Clone, Copy, Debug)]
pub struct AssetSpec {
    pub kind: AssetKind,
    pub label: &'static str,
    pub icon: &'static str,
    pub blurb: &'static str,
}

/// The placement catalogue, in the order the tool lists it (the tree first — the prop this
/// exists for).
pub const CATALOG: &[AssetSpec] = &[
    AssetSpec { kind: AssetKind::Tree, label: "Tree", icon: "tree-pine", blurb: "A conifer, 8 m tall" },
    AssetSpec { kind: AssetKind::House, label: "House", icon: "home", blurb: "A pitched-roof house, 10×8 m" },
    AssetSpec { kind: AssetKind::Building, label: "Building", icon: "building-2", blurb: "A block building, 16×12 m" },
    AssetSpec { kind: AssetKind::Stadium, label: "Stadium", icon: "landmark", blurb: "A stadium bowl, 140×110 m" },
    AssetSpec { kind: AssetKind::Pin, label: "Pin", icon: "map-pin", blurb: "A map pin, 6 m tall" },
    AssetSpec { kind: AssetKind::Camera, label: "Camera", icon: "cctv", blurb: "A CCTV camera on a 6 m mast — the 3D view previews through it" },
];

/// One placed asset: the kind, where it stands (document points — the same space every tool
/// draws in, so a placement lands where the map is looking on both canvases), its rotation,
/// its scale (1 = the catalogue footprint) and its id ("a1", "a2", …). A camera adds its
/// aim: the look pitch below the horizon and the horizontal field of view, both degrees —
/// the preview rig and both canvases' view cone read them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetPlacement {
    pub id: String,
    pub kind: AssetKind,
    pub x: f64,
    pub y: f64,
    /// Degrees clockwise from north.
    #[serde(default)]
    pub rotation_deg: f32,
    #[serde(default = "one")]
    pub scale: f32,
    /// The camera's look pitch below the horizon (degrees, negative down).
    #[serde(default)]
    pub pitch_deg: f32,
    /// The camera's horizontal field of view (degrees).
    #[serde(default)]
    pub fov_deg: f32,
}

fn one() -> f32 {
    1.0
}

impl AssetPlacement {
    pub fn new(id: String, kind: AssetKind, x: f64, y: f64) -> Self {
        Self { id, kind, x, y, rotation_deg: 0.0, scale: 1.0, pitch_deg: kind.default_pitch_deg(), fov_deg: kind.default_fov_deg() }
    }

    /// The camera's mast-top eye height (metres): the 6 m mast times the placement's scale —
    /// the preview rig stands here and the 3D mast reaches here.
    pub fn eye_height_m(&self) -> f32 {
        6.0 * self.scale.max(0.05)
    }

    /// The world footprint at this placement's scale, `[width, depth]` in metres.
    pub fn size_m(&self) -> [f32; 2] {
        [self.kind.size_m()[0] * self.scale, self.kind.size_m()[1] * self.scale]
    }
}

/// The document's placed assets. Saved with the file when anything is placed — an empty
/// document carries none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssetsState {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placements: Vec<AssetPlacement>,
}

impl AssetsState {
    /// Place one asset; returns its id ("a{n}" — stable until a removal renumbers later ones).
    pub fn place(&mut self, kind: AssetKind, x: f64, y: f64) -> String {
        let id = format!("a{}", self.placements.len() + 1);
        self.placements.push(AssetPlacement::new(id.clone(), kind, x, y));
        id
    }

    /// Place one with explicit rotation and scale (the tool's brush passes these on).
    pub fn place_with(&mut self, kind: AssetKind, x: f64, y: f64, rotation_deg: f32, scale: f32) -> String {
        let id = self.place(kind, x, y);
        if let Some(p) = self.placements.iter_mut().rev().find(|p| p.id == id) {
            p.rotation_deg = rotation_deg;
            p.scale = scale.max(0.05);
        }
        id
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.placements.len();
        self.placements.retain(|p| p.id != id);
        self.placements.len() != before
    }

    pub fn clear(&mut self) {
        self.placements.clear();
    }

    /// Nothing placed: documents skip the block entirely (a plain map file carries nothing).
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }
}

/// One part of a kind's 2D glyph: a closed polygon in unit space (y-up, one unit = the
/// footprint's half-width), with its fill colour. The 2D map scales the parts by the
/// footprint and the view, the same silhouette every zoom.
#[derive(Clone, Copy, Debug)]
pub struct GlyphPart {
    pub pts: &'static [(f32, f32)],
    pub color: [u8; 3],
}

/// The 2D glyph: the same silhouette language the 3D meshes speak (a tree is a trunk and two
/// stacked canopies there; it is the same here, flattened).
pub fn glyph(kind: AssetKind) -> Vec<GlyphPart> {
    match kind {
        AssetKind::Tree => vec![
            GlyphPart { pts: TRUNK, color: [0x6b, 0x4f, 0x35] },
            GlyphPart { pts: CANOPY_LOW, color: [0x2a, 0x66, 0x38] },
            GlyphPart { pts: CANOPY_TOP, color: [0x35, 0x7d, 0x44] },
        ],
        AssetKind::House => vec![GlyphPart { pts: HOUSE_BODY, color: [0xc9, 0xb8, 0xa6] }, GlyphPart { pts: HOUSE_ROOF, color: [0x8c, 0x4a, 0x3a] }],
        AssetKind::Building => vec![GlyphPart { pts: BLOCK, color: [0x9a, 0x9c, 0xa1] }, GlyphPart { pts: BLOCK_TOP, color: [0xb8, 0xba, 0xbf] }],
        AssetKind::Stadium => vec![GlyphPart { pts: BOWL, color: [0xb6, 0xb2, 0xa8] }, GlyphPart { pts: PITCH, color: [0x4e, 0x8a, 0x4e] }],
        AssetKind::Pin => vec![GlyphPart { pts: PIN, color: [0xd6, 0x45, 0x45] }],
        AssetKind::Camera => vec![
            GlyphPart { pts: CAMERA_MAST, color: [0x6b, 0x6e, 0x73] },
            GlyphPart { pts: CAMERA_BODY, color: [0x4a, 0x4d, 0x52] },
            GlyphPart { pts: CAMERA_LENS, color: [0x2a, 0x2d, 0x31] },
        ],
    }
}

const TRUNK: &[(f32, f32)] = &[(-0.06, 0.0), (0.06, 0.0), (0.06, 0.55), (-0.06, 0.55)];
const CANOPY_LOW: &[(f32, f32)] = &[(-0.5, 0.3), (0.5, 0.3), (0.22, 0.95), (-0.22, 0.95)];
const CANOPY_TOP: &[(f32, f32)] = &[(-0.3, 0.72), (0.3, 0.72), (0.0, 1.25)];
const HOUSE_BODY: &[(f32, f32)] = &[(-0.5, 0.0), (0.5, 0.0), (0.5, 0.62), (-0.5, 0.62)];
const HOUSE_ROOF: &[(f32, f32)] = &[(-0.58, 0.62), (0.58, 0.62), (0.0, 1.05)];
const BLOCK: &[(f32, f32)] = &[(-0.5, 0.0), (0.5, 0.0), (0.5, 0.9), (-0.5, 0.9)];
const BLOCK_TOP: &[(f32, f32)] = &[(-0.42, 0.9), (0.42, 0.9), (0.42, 1.0), (-0.42, 1.0)];
const BOWL: &[(f32, f32)] = &[(-0.62, -0.5), (0.62, -0.5), (0.68, 0.0), (0.62, 0.5), (-0.62, 0.5), (-0.68, 0.0)];
const PITCH: &[(f32, f32)] = &[(-0.42, -0.32), (0.42, -0.32), (0.42, 0.32), (-0.42, 0.32)];
const PIN: &[(f32, f32)] = &[(0.0, 1.0), (0.28, 0.55), (0.28, 0.2), (0.0, 0.0), (-0.28, 0.2), (-0.28, 0.55)];
/// The camera glyph rides high (a mast-top body): the body box with a centred lens — the
/// 2D glyph carries no facing of its own; the view wedge (ui-egui/src/assets.rs) draws the
/// aim, the same wedge the 3D environment stands as its frustum.
const CAMERA_BODY: &[(f32, f32)] = &[(-0.34, 0.62), (0.34, 0.62), (0.34, 0.94), (-0.34, 0.94)];
const CAMERA_LENS: &[(f32, f32)] = &[(-0.16, 0.70), (0.16, 0.70), (0.16, 0.86), (-0.16, 0.86)];
/// The mast: drawn as the body's stalk so the glyph reads as a camera on a pole.
const CAMERA_MAST: &[(f32, f32)] = &[(-0.06, 0.0), (0.06, 0.0), (0.06, 0.62), (-0.06, 0.62)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_roundtrip_labels_and_parse() {
        for k in AssetKind::ALL {
            assert_eq!(AssetKind::parse(k.label()), Some(k));
            assert_eq!(AssetKind::parse(&format!("{k:?}").to_lowercase()), Some(k));
        }
        assert_eq!(AssetKind::parse("TREE"), Some(AssetKind::Tree));
        assert_eq!(AssetKind::parse("nope"), None);
        assert_eq!(AssetKind::Tree.to_string(), "Tree");
    }

    #[test]
    fn the_catalogue_lists_every_kind_tree_first() {
        assert_eq!(CATALOG.len(), AssetKind::ALL.len());
        assert_eq!(CATALOG[0].kind, AssetKind::Tree, "the tree leads — the prop the brush is for");
        let mut kinds: Vec<AssetKind> = CATALOG.iter().map(|s| s.kind).collect();
        kinds.sort_by_key(|k| format!("{k:?}"));
        let mut all: Vec<AssetKind> = AssetKind::ALL.to_vec();
        all.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(kinds, all);
        assert!(CATALOG.iter().all(|s| !s.label.is_empty() && !s.icon.is_empty() && !s.blurb.is_empty()));
    }

    #[test]
    fn footprints_are_real_world_metres() {
        assert_eq!(AssetKind::Stadium.size_m(), [140.0, 110.0]);
        assert_eq!(AssetKind::Tree.size_m(), [4.0, 4.0]);
        assert!(AssetKind::ALL.into_iter().all(|k| k.size_m()[0] > 0.0 && k.size_m()[1] > 0.0));
    }

    #[test]
    fn placements_place_remove_and_roundtrip() {
        let mut st = AssetsState::default();
        assert!(st.is_empty());
        let a = st.place(AssetKind::Tree, 100.0, 200.0);
        let b = st.place_with(AssetKind::House, 300.0, 400.0, 45.0, 2.0);
        assert_eq!(st.placements.len(), 2);
        assert_ne!(a, b);
        let house = st.placements.iter().find(|p| p.id == b).unwrap();
        assert_eq!(house.rotation_deg, 45.0);
        assert_eq!(house.scale, 2.0);
        assert_eq!(house.size_m()[0], 20.0, "the footprint scales");
        assert!(st.remove(&a));
        assert!(!st.remove(&a), "a second removal of the same id says so");
        assert_eq!(st.placements.len(), 1);
        // The state roundtrips through JSON (what the document save writes).
        let json = serde_json::to_string(&st).unwrap();
        let back: AssetsState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, st);
        // An empty state serializes to nothing but a pair of braces — skip-if-empty.
        let empty = serde_json::to_string(&AssetsState::default()).unwrap();
        assert_eq!(empty, "{}");
        st.clear();
        assert!(st.is_empty());
    }

    #[test]
    fn every_kind_has_a_glyph_with_ground_origin() {
        for k in AssetKind::ALL {
            let parts = glyph(k);
            assert!(!parts.is_empty(), "{k} draws something");
            assert!(parts.iter().all(|p| !p.pts.is_empty()));
            // The glyph as a whole touches the ground line (y = 0) — it stands, not floats.
            assert!(parts.iter().any(|p| p.pts.iter().any(|(_, y)| *y <= 0.01)), "{k:?} stands on the ground");
        }
    }
}
