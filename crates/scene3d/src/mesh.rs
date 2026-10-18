//! [`Mesh3d`] — the renderable triangle soup the egui-painter viewport draws — and the builders
//! shared by terrain, imagery and buildings: textured ground quads, extruded footprint rings,
//! boxes. Positions are world metres (`x` east, `y` up, `z` south); colors are linear 0..1
//! floats; a mesh may carry UVs and name a texture slot the UI resolves (the house's brick /
//! plaster textures, the ground's imagery tiles).

/// Draw buckets, flushed back-to-front in this order (the painter has no z-buffer):
/// ground slabs first, then decal linework that lies ON the ground, then everything standing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum MeshLayer {
    /// Ground slabs: terrain, water, imagery underlays.
    #[default]
    Ground,
    /// Decals on the ground: roads, contours, pitch markings (drawn above Ground).
    Decal,
    /// Anything standing: walls, roofs, assets (drawn last, occluding decals).
    Standing,
}

/// One renderable mesh: positions + per-vertex colors, optional normals/UVs, optional texture.
#[derive(Clone, Debug, Default)]
pub struct Mesh3d {
    /// Vertices, world metres.
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals (empty → faces shade from their winding normal).
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex linear RGB 0..1.
    pub colors: Vec<[f32; 3]>,
    /// Per-vertex UV 0..1 (when the mesh names a texture).
    pub uvs: Vec<[f32; 2]>,
    /// Triangle indices (empty → every 3 positions are one triangle).
    pub indices: Vec<u32>,
    /// The layer this mesh draws in (the flush order).
    pub layer: MeshLayer,
    /// The texture slot the UI resolves (`None` → flat colors).
    pub texture: Option<String>,
    /// A small depth bias (metres) nudging coplanar decals like glPolygonOffset.
    pub z_bias: f32,
}

impl Mesh3d {
    /// An empty mesh in `layer`.
    pub fn new(layer: MeshLayer) -> Self {
        Self { layer, ..Self::default() }
    }

    /// Append a vertex, returning its index.
    pub fn vertex(&mut self, pos: [f32; 3], color: [f32; 3]) -> u32 {
        self.positions.push(pos);
        self.colors.push(color);
        (self.positions.len() - 1) as u32
    }

    /// Append a triangle by indices.
    pub fn tri(&mut self, a: u32, b: u32, c: u32) {
        self.indices.extend([a, b, c]);
    }

    /// Append a quad (a-b-c-d counter-clockwise seen from outside) as two triangles.
    pub fn quad(&mut self, a: u32, b: u32, c: u32, d: u32) {
        self.tri(a, b, c);
        self.tri(a, c, d);
    }

    /// Append a textured quad with UVs per corner (same winding rule). The mesh's `texture`
    /// names the slot the UI resolves.
    pub fn tex_quad(&mut self, ps: [[f32; 3]; 4], uvs: [[f32; 2]; 4], colors: [[f32; 3]; 4]) {
        let base = self.positions.len() as u32;
        for (p, c) in ps.into_iter().zip(colors) {
            self.positions.push(p);
            self.colors.push(c);
        }
        self.uvs.extend(uvs);
        self.quad(base, base + 1, base + 2, base + 3);
    }

    /// Triangle count the draw loop will issue.
    pub fn tri_count(&self) -> usize {
        if self.indices.is_empty() { self.positions.len() / 3 } else { self.indices.len() / 3 }
    }

    /// The world-space bounding box (min, max); `None` when empty.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for p in &self.positions {
            for a in 0..3 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
            any = true;
        }
        any.then_some((min, max))
    }
}

/// An axis-aligned box from `min` to `max`, one color, standing mesh. Used by buildings'
/// generic blocks and small props.
pub fn box_between(min: [f32; 3], max: [f32; 3], color: [f32; 3]) -> Mesh3d {
    let mut m = Mesh3d::new(MeshLayer::Standing);
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    // 8 corners.
    let idx = |m: &mut Mesh3d, x: f32, y: f32, z: f32| m.vertex([x, y, z], color);
    let p000 = idx(&mut m, x0, y0, z0);
    let p100 = idx(&mut m, x1, y0, z0);
    let p110 = idx(&mut m, x1, y1, z0);
    let p010 = idx(&mut m, x0, y1, z0);
    let p001 = idx(&mut m, x0, y0, z1);
    let p101 = idx(&mut m, x1, y0, z1);
    let p111 = idx(&mut m, x1, y1, z1);
    let p011 = idx(&mut m, x0, y1, z1);
    // South face (z0, seen from +z).
    m.quad(p000, p100, p110, p010);
    // North face (z1, seen from −z).
    m.quad(p101, p001, p011, p111);
    // East face (x1, seen from +x).
    m.quad(p100, p101, p111, p110);
    // West face.
    m.quad(p001, p000, p010, p011);
    // Top.
    m.quad(p010, p110, p111, p011);
    // Bottom (rarely seen; keeps the mesh closed).
    m.quad(p000, p001, p101, p100);
    m
}

/// Extrude one closed footprint `ring` (world metres, `y` ignored on input) from `base_h` to
/// `top_h`, walls in `color`, flat roof slightly darker. The ring triangulates as a fan from
/// its centroid — fine for the small convex-ish footprints MVT buildings carry.
pub fn extrude_ring(ring: &[[f32; 2]], base_h: f32, top_h: f32, color: [f32; 3]) -> Mesh3d {
    let mut m = Mesh3d::new(MeshLayer::Standing);
    if ring.len() < 3 || top_h <= base_h {
        return m;
    }
    let mut roof = [0.0f32; 2];
    for p in ring {
        roof[0] += p[0];
        roof[1] += p[1];
    }
    roof[0] /= ring.len() as f32;
    roof[1] /= ring.len() as f32;
    let n = ring.len();
    let darker = [color[0] * 0.86, color[1] * 0.86, color[2] * 0.86];
    // Walls.
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let v0 = m.vertex([a[0], base_h, a[1]], color);
        let v1 = m.vertex([b[0], base_h, b[1]], color);
        let v2 = m.vertex([b[0], top_h, b[1]], color);
        let v3 = m.vertex([a[0], top_h, a[1]], color);
        m.quad(v0, v1, v2, v3);
    }
    // Roof fan.
    let center = m.vertex([roof[0], top_h, roof[1]], darker);
    let top_start = m.positions.len() as u32;
    for p in ring {
        m.vertex([p[0], top_h, p[1]], darker);
    }
    for i in 0..n {
        let a = top_start + i as u32;
        let b = top_start + ((i + 1) % n) as u32;
        m.tri(center, a, b);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_bounds_and_counts_match_its_corners() {
        let m = box_between([0.0, 0.0, 0.0], [2.0, 4.0, 6.0], [0.5, 0.4, 0.3]);
        assert_eq!(m.positions.len(), 8);
        assert_eq!(m.tri_count(), 12, "six quads × two tris");
        let (min, max) = m.bounds().unwrap();
        assert_eq!(min, [0.0, 0.0, 0.0]);
        assert_eq!(max, [2.0, 4.0, 6.0]);
        assert_eq!(m.layer, MeshLayer::Standing);
    }

    #[test]
    fn an_extruded_ring_gets_walls_and_a_roof_fan() {
        let ring = [[0.0, 0.0], [10.0, 0.0], [10.0, 8.0], [0.0, 8.0]];
        let m = extrude_ring(&ring, 0.0, 12.0, [0.7, 0.65, 0.6]);
        // 4 wall quads (16 verts) + roof fan center + 4 top corners.
        assert_eq!(m.positions.len(), 16 + 5);
        assert_eq!(m.tri_count(), 8 + 4);
        let (min, max) = m.bounds().unwrap();
        assert!((max[1] - 12.0).abs() < 1e-4);
        assert!((min[1] - 0.0).abs() < 1e-4);
    }

    #[test]
    fn degenerate_rings_extrude_to_nothing() {
        assert_eq!(extrude_ring(&[[0.0, 0.0]], 0.0, 5.0, [1.0; 3]).tri_count(), 0);
        assert_eq!(extrude_ring(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]], 5.0, 5.0, [1.0; 3]).tri_count(), 0);
    }
}
