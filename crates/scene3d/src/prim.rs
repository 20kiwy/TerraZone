//! Parametric asset meshes — the textured house, the stadium, trees and pins — copied from the
//! ArchCraft (effect) project's primitive builders and rebuilt in the world frame the 3D
//! environment draws: metres, `y` up, `z` south, the mesh's own origin at its ground centre so
//! a placement's lon/lat drops it straight onto the terrain.
//!
//! "Textured" here means per-face colors + UV walls: the house's walls carry brick-band UVs the
//! viewport resolves against its procedural plaster/brick textures, and every face carries its
//! own color (plaster, terracotta roof, glass, grass) so an untextured draw still reads.

use crate::mesh::{Mesh3d, MeshLayer};

/// The textured house: a plaster box with brick-band wall UVs, a terracotta gable roof, a door
/// and two windows, standing `w` × `d` metres with `wall_h` walls. Origin at the ground centre.
pub fn house(w: f32, d: f32, wall_h: f32) -> Mesh3d {
    let mut m = Mesh3d::new(MeshLayer::Standing);
    let (hw, hd) = (w * 0.5, d * 0.5);
    let plaster = [0.88, 0.82, 0.70];
    let roof = [0.72, 0.33, 0.22];
    let gable = [0.84, 0.78, 0.66];
    let door = [0.42, 0.28, 0.18];
    let glass = [0.55, 0.72, 0.78];
    let v = |m: &mut Mesh3d, p: [f32; 3], c: [f32; 3], uv: [f32; 2]| {
        m.positions.push(p);
        m.colors.push(c);
        m.uvs.push(uv);
        (m.positions.len() - 1) as u32
    };
    let uvw = |u: f32, vv: f32| [u, vv];

    // South wall (z = +hd, facing the default camera): UVs run u across, v up, brick bands.
    let zs = hd + 0.02;
    let sw0 = v(&mut m, [-hw, 0.0, zs], plaster, uvw(0.0, 0.0));
    let sw1 = v(&mut m, [hw, 0.0, zs], plaster, uvw(w / 3.0, 0.0));
    let sw2 = v(&mut m, [hw, wall_h, zs], plaster, uvw(w / 3.0, wall_h / 3.0));
    let sw3 = v(&mut m, [-hw, wall_h, zs], plaster, uvw(0.0, wall_h / 3.0));
    m.quad(sw0, sw1, sw2, sw3);
    // Door (centre-left, 1.0 × 2.0 m).
    let dw = (w / 8.0).max(0.5);
    let d0 = v(&mut m, [-dw, 0.0, hd + 0.04], door, uvw(0.0, 0.0));
    let d1 = v(&mut m, [dw, 0.0, hd + 0.04], door, uvw(0.1, 0.0));
    let d2 = v(&mut m, [dw, 2.0, hd + 0.04], door, uvw(0.1, 0.7));
    let d3 = v(&mut m, [-dw, 2.0, hd + 0.04], door, uvw(0.0, 0.7));
    m.quad(d0, d1, d2, d3);
    // Two windows on the upper wall.
    let ww = (w / 6.0).max(0.4);
    let wh = 0.8f32.min(wall_h * 0.4);
    let wy = wall_h * 0.55;
    for sign in [-1.0f32, 1.0] {
        let cx = sign * w * 0.28;
        let w0 = v(&mut m, [cx - ww, wy, hd + 0.04], glass, uvw(0.0, 0.0));
        let w1 = v(&mut m, [cx + ww, wy, hd + 0.04], glass, uvw(1.0, 0.0));
        let w2 = v(&mut m, [cx + ww, wy + wh, hd + 0.04], glass, uvw(1.0, 1.0));
        let w3 = v(&mut m, [cx - ww, wy + wh, hd + 0.04], glass, uvw(0.0, 1.0));
        m.quad(w0, w1, w2, w3);
    }
    m.texture = Some("house".into());

    // North wall (plain).
    let zn = -hd - 0.02;
    let nw0 = v(&mut m, [hw, 0.0, zn], plaster, uvw(0.0, 0.0));
    let nw1 = v(&mut m, [-hw, 0.0, zn], plaster, uvw(w / 3.0, 0.0));
    let nw2 = v(&mut m, [-hw, wall_h, zn], plaster, uvw(w / 3.0, wall_h / 3.0));
    let nw3 = v(&mut m, [hw, wall_h, zn], plaster, uvw(0.0, wall_h / 3.0));
    m.quad(nw0, nw1, nw2, nw3);

    // East + west walls.
    let xe = hw + 0.02;
    let ew0 = v(&mut m, [xe, 0.0, hd], plaster, uvw(0.0, 0.0));
    let ew1 = v(&mut m, [xe, 0.0, -hd], plaster, uvw(d / 3.0, 0.0));
    let ew2 = v(&mut m, [xe, wall_h, -hd], plaster, uvw(d / 3.0, wall_h / 3.0));
    let ew3 = v(&mut m, [xe, wall_h, hd], plaster, uvw(0.0, wall_h / 3.0));
    m.quad(ew0, ew1, ew2, ew3);
    let xw = -hw - 0.02;
    let ww0 = v(&mut m, [xw, 0.0, -hd], plaster, uvw(0.0, 0.0));
    let ww1 = v(&mut m, [xw, 0.0, hd], plaster, uvw(d / 3.0, 0.0));
    let ww2 = v(&mut m, [xw, wall_h, hd], plaster, uvw(d / 3.0, wall_h / 3.0));
    let ww3 = v(&mut m, [xw, wall_h, -hd], plaster, uvw(0.0, wall_h / 3.0));
    m.quad(ww0, ww1, ww2, ww3);

    // Gable roof: ridge along x at height wall_h + roof_h; two slopes + two gable ends.
    let rh = (w * 0.35).min(3.0);
    let ridge = wall_h + rh;
    let ov = 0.3f32.min(w * 0.1);
    let s0 = v(&mut m, [-hw - ov, wall_h, hd + ov], roof, uvw(0.0, 0.0));
    let s1 = v(&mut m, [hw + ov, wall_h, hd + ov], roof, uvw(1.0, 0.0));
    let s2 = v(&mut m, [hw + ov, ridge, 0.0], roof, uvw(1.0, 1.0));
    let s3 = v(&mut m, [-hw - ov, ridge, 0.0], roof, uvw(0.0, 1.0));
    m.quad(s0, s1, s2, s3);
    let n0 = v(&mut m, [hw + ov, wall_h, -hd - ov], roof, uvw(0.0, 0.0));
    let n1 = v(&mut m, [-hw - ov, wall_h, -hd - ov], roof, uvw(1.0, 0.0));
    let n2 = v(&mut m, [-hw - ov, ridge, 0.0], roof, uvw(1.0, 1.0));
    let n3 = v(&mut m, [hw + ov, ridge, 0.0], roof, uvw(0.0, 1.0));
    m.quad(n0, n1, n2, n3);
    // Gable ends (east + west triangles).
    let ge0 = v(&mut m, [xe + 0.02, wall_h, hd], gable, uvw(0.0, 0.0));
    let ge1 = v(&mut m, [xe + 0.02, wall_h, -hd], gable, uvw(1.0, 0.0));
    let ge2 = v(&mut m, [xe + 0.02, ridge, 0.0], gable, uvw(0.5, 1.0));
    m.tri(ge0, ge1, ge2);
    let gw0 = v(&mut m, [xw - 0.02, wall_h, -hd], gable, uvw(0.0, 0.0));
    let gw1 = v(&mut m, [xw - 0.02, wall_h, hd], gable, uvw(1.0, 0.0));
    let gw2 = v(&mut m, [xw - 0.02, ridge, 0.0], gable, uvw(0.5, 1.0));
    m.tri(gw0, gw1, gw2);
    m
}

/// The stadium: a green pitch with white markings (ground decals), a ring running track, tiered
/// stands on four sides and an outer wall. Origin at the pitch's ground centre. A `pitch_w` ×
/// `pitch_d` of ~105 × 68 m with ~12 m stands matches a real one.
pub fn stadium(pitch_w: f32, pitch_d: f32) -> Mesh3d {
    let mut m = Mesh3d::new(MeshLayer::Standing);
    let grass = [0.30, 0.56, 0.28];
    let line = [0.92, 0.95, 0.92];
    let (pw, pd) = (pitch_w * 0.5, pitch_d * 0.5);
    // Ground decals sit 5 cm proud of the terrain slab (no z-fighting); `v3` takes any height.
    let p = |m: &mut Mesh3d, x: f32, z: f32, c: [f32; 3]| {
        m.positions.push([x, 0.05, z]);
        m.colors.push(c);
        (m.positions.len() - 1) as u32
    };
    let v3 = |m: &mut Mesh3d, x: f32, y: f32, z: f32, c: [f32; 3]| {
        m.positions.push([x, y, z]);
        m.colors.push(c);
        (m.positions.len() - 1) as u32
    };
    // Pitch slab: mowing stripes across x.
    let stripes = 8;
    for i in 0..stripes {
        let x0 = -pw + pitch_w * i as f32 / stripes as f32;
        let x1 = -pw + pitch_w * (i + 1) as f32 / stripes as f32;
        let c = if i % 2 == 0 { grass } else { [grass[0] * 1.12, grass[1] * 1.10, grass[2] * 1.08] };
        let a = p(&mut m, x0, pd, c);
        let b = p(&mut m, x1, pd, c);
        let cc = p(&mut m, x1, -pd, c);
        let d = p(&mut m, x0, -pd, c);
        m.quad(a, b, cc, d);
    }
    // Touchlines + halfway line (thin decal quads).
    let lw = 0.25f32;
    let line_quad = |m: &mut Mesh3d, x0: f32, z0: f32, x1: f32, z1: f32| {
        // A thin quad along the segment, `lw` wide, drawn as 4 ground verts.
        let (dx, dz) = (
            (z1 - z0) * 0.5 * lw / ((x1 - x0).abs() + (z1 - z0).abs()).max(1e-3),
            (x1 - x0) * 0.5 * lw / ((x1 - x0).abs() + (z1 - z0).abs()).max(1e-3),
        );
        let a = p(m, x0 + dz, z0 + dx, line);
        let b = p(m, x1 + dz, z1 + dx, line);
        let c = p(m, x1 - dz, z1 - dx, line);
        let d = p(m, x0 - dz, z0 - dx, line);
        m.quad(a, b, c, d);
    };
    line_quad(&mut m, -pw, pd, pw, pd);
    line_quad(&mut m, -pw, -pd, pw, -pd);
    line_quad(&mut m, -pw, pd, -pw, -pd);
    line_quad(&mut m, pw, pd, pw, -pd);
    line_quad(&mut m, -pw, 0.0, pw, 0.0);
    // Centre circle: 24 segments.
    let r = (pitch_w.min(pitch_d) * 0.14).max(3.0);
    let segs = 24;
    for i in 0..segs {
        let a0 = std::f32::consts::TAU * i as f32 / segs as f32;
        let a1 = std::f32::consts::TAU * (i + 1) as f32 / segs as f32;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let a = p(&mut m, r * c0, r * s0, line);
        let b = p(&mut m, r * c1, r * s1, line);
        let c = p(&mut m, (r + lw) * c1, (r + lw) * s1, line);
        let d = p(&mut m, (r + lw) * c0, (r + lw) * s0, line);
        m.quad(a, b, c, d);
    }

    // Track ring around the pitch (terracotta).
    let track_col = [0.62, 0.42, 0.34];
    let tw = pitch_w * 0.5 + 8.0;
    let td = pitch_d * 0.5 + 8.0;
    let track_w = 6.0f32;
    // South straight.
    let a = p(&mut m, -tw, td, track_col);
    let b = p(&mut m, tw, td, track_col);
    let c = p(&mut m, tw, td + track_w, track_col);
    let d = p(&mut m, -tw, td + track_w, track_col);
    m.quad(a, b, c, d);
    // North straight.
    let a = p(&mut m, tw, -td, track_col);
    let b = p(&mut m, -tw, -td, track_col);
    let c = p(&mut m, -tw, -td - track_w, track_col);
    let d = p(&mut m, tw, -td - track_w, track_col);
    m.quad(a, b, c, d);
    // East straight.
    let a = p(&mut m, tw, -td, track_col);
    let b = p(&mut m, tw, td, track_col);
    let c = p(&mut m, tw + track_w, td, track_col);
    let d = p(&mut m, tw + track_w, -td, track_col);
    m.quad(a, b, c, d);
    // West straight.
    let a = p(&mut m, -tw - track_w, td, track_col);
    let b = p(&mut m, -tw - track_w, -td, track_col);
    let c = p(&mut m, -tw, -td, track_col);
    let d = p(&mut m, -tw, td, track_col);
    m.quad(a, b, c, d);

    // Stands: four tiered grandstands rising away from the pitch — each tier a horizontal
    // step at its own height with a riser down to the step before (a clean stair profile).
    let concrete = [0.68, 0.67, 0.65];
    let seat = [0.30, 0.36, 0.55];
    let depth = 9.0f32;
    let tiers = 3;
    let span = |side: usize| match side {
        0 | 1 => (-tw, tw),
        _ => (-td, td),
    };
    // side 0 = south (z > 0), 1 = north, 2 = east, 3 = west. Returns the tier's front line
    // (a→b at depth r, facing the pitch) and back line (c→d at depth r + step).
    let front_back = |side: usize, r: f32, step: f32| match side {
        0 => {
            [[span(0).0, td + track_w + r], [span(0).1, td + track_w + r], [span(0).1, td + track_w + r + step], [span(0).0, td + track_w + r + step]]
        }
        1 => [
            [span(1).1, -td - track_w - r],
            [span(1).0, -td - track_w - r],
            [span(1).0, -td - track_w - r - step],
            [span(1).1, -td - track_w - r - step],
        ],
        2 => {
            [[tw + track_w + r, span(2).1], [tw + track_w + r, span(2).0], [tw + track_w + r + step, span(2).0], [tw + track_w + r + step, span(2).1]]
        }
        _ => [
            [-tw - track_w - r, span(3).0],
            [-tw - track_w - r, span(3).1],
            [-tw - track_w - r - step, span(3).1],
            [-tw - track_w - r - step, span(3).0],
        ],
    };
    for side in 0..4usize {
        let step = depth / tiers as f32;
        for t in 0..tiers {
            let y_lo = t as f32 * (4.0 / tiers as f32);
            let y_hi = (t + 1) as f32 * (4.0 / tiers as f32);
            let r = t as f32 * step;
            let col = if t % 2 == 0 { seat } else { [seat[0] * 1.15, seat[1] * 1.15, seat[2] * 1.15] };
            let [fa, fb, cd, da] = front_back(side, r, step);
            // Top of the step (horizontal, at y_hi).
            let t0 = v3(&mut m, fa[0], y_hi, fa[1], col);
            let t1 = v3(&mut m, fb[0], y_hi, fb[1], col);
            let t2 = v3(&mut m, cd[0], y_hi, cd[1], col);
            let t3 = v3(&mut m, da[0], y_hi, da[1], col);
            m.quad(t0, t1, t2, t3);
            // Riser down to the step before (concrete): a vertical quad at the front line.
            let r0 = v3(&mut m, fa[0], y_hi, fa[1], concrete);
            let r1 = v3(&mut m, fb[0], y_hi, fb[1], concrete);
            let r2 = v3(&mut m, fb[0], y_lo, fb[1], concrete);
            let r3 = v3(&mut m, fa[0], y_lo, fa[1], concrete);
            m.quad(r0, r1, r2, r3);
        }
    }

    // Outer wall ring (white, 3 m) around everything.
    let wall = [0.90, 0.90, 0.88];
    let wx0 = -tw - track_w - depth;
    let wx1 = tw + track_w + depth;
    let wz0 = -td - track_w - depth;
    let wz1 = td + track_w + depth;
    let corners = [(wx0, wz1), (wx1, wz1), (wx1, wz0), (wx0, wz0)];
    for i in 0..4 {
        let (x0, z0) = corners[i];
        let (x1, z1) = corners[(i + 1) % 4];
        let a = p(&mut m, x0, z0, wall);
        let b = p(&mut m, x1, z1, wall);
        let a2 = {
            m.positions.push([x0, 3.0, z0]);
            m.colors.push(wall);
            (m.positions.len() - 1) as u32
        };
        let b2 = {
            m.positions.push([x1, 3.0, z1]);
            m.colors.push(wall);
            (m.positions.len() - 1) as u32
        };
        m.quad(a, b, b2, a2);
    }
    m
}

/// A conifer: trunk + two stacked cones. Origin at the ground centre, ~`h` tall.
pub fn tree(h: f32) -> Mesh3d {
    let mut m = Mesh3d::new(MeshLayer::Standing);
    let trunk_col = [0.42, 0.31, 0.22];
    let leaf = [0.22, 0.48, 0.26];
    let leaf_hi = [0.26, 0.55, 0.30];
    let r = (h * 0.22).max(0.4);
    // Trunk (square section is enough at scene scale).
    let tw = r * 0.35;
    for (x0, z0, x1, z1) in [(-tw, -tw, tw, -tw), (tw, -tw, tw, tw), (tw, tw, -tw, tw), (-tw, tw, -tw, -tw)] {
        let a = m.vertex([x0, 0.0, z0], trunk_col);
        let b = m.vertex([x1, 0.0, z1], trunk_col);
        let c = m.vertex([x1, h * 0.35, z1], trunk_col);
        let d = m.vertex([x0, h * 0.35, z0], trunk_col);
        m.quad(a, b, c, d);
    }
    // Two cones (8-sided): lower wide, upper narrow.
    for (tier, (base, tip, rad)) in [(0usize, (h * 0.25, h * 0.7, r)), (1usize, (h * 0.55, h, r * 0.65))] {
        let col = if tier == 0 { leaf } else { leaf_hi };
        let segs = 8;
        for i in 0..segs {
            let a0 = std::f32::consts::TAU * i as f32 / segs as f32;
            let a1 = std::f32::consts::TAU * (i + 1) as f32 / segs as f32;
            let (s0, c0) = a0.sin_cos();
            let (s1, c1) = a1.sin_cos();
            let b0 = m.vertex([rad * c0, base, rad * s0], col);
            let b1 = m.vertex([rad * c1, base, rad * s1], col);
            let t = m.vertex([0.0, tip, 0.0], col);
            m.tri(b0, b1, t);
        }
    }
    m
}

/// A map pin: a thin cone stem with a ball head. Origin at the ground point.
pub fn pin(h: f32) -> Mesh3d {
    let mut m = Mesh3d::new(MeshLayer::Standing);
    let col = [0.85, 0.30, 0.24];
    let segs = 8;
    let r = h * 0.10;
    for i in 0..segs {
        let a0 = std::f32::consts::TAU * i as f32 / segs as f32;
        let a1 = std::f32::consts::TAU * (i + 1) as f32 / segs as f32;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let b0 = m.vertex([r * c0, 0.0, r * s0], col);
        let b1 = m.vertex([r * c1, 0.0, r * s1], col);
        let t = m.vertex([0.0, h * 0.75, 0.0], col);
        m.tri(b0, b1, t);
    }
    // Head: 8×4 quarter-shell ball atop the stem.
    let cy = h * 0.75;
    let rr = h * 0.28;
    for j in 0..4 {
        let th0 = std::f32::consts::FRAC_PI_2 * j as f32 / 4.0;
        let th1 = std::f32::consts::FRAC_PI_2 * (j + 1) as f32 / 4.0;
        for i in 0..segs {
            let a0 = std::f32::consts::TAU * i as f32 / segs as f32;
            let a1 = std::f32::consts::TAU * (i + 1) as f32 / segs as f32;
            let at = |th: f32, a: f32| -> [f32; 3] {
                let (s, c) = a.sin_cos();
                let (st, ct) = th.sin_cos();
                [rr * st * c, cy + rr * ct, rr * st * s]
            };
            let p00 = m.vertex(at(th0, a0), col);
            let p10 = m.vertex(at(th0, a1), col);
            let p11 = m.vertex(at(th1, a1), col);
            let p01 = m.vertex(at(th1, a0), col);
            m.quad(p00, p10, p11, p01);
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_house_stands_its_wall_and_ridge_heights_with_wall_uvs() {
        let m = house(10.0, 8.0, 4.0);
        assert_eq!(m.texture.as_deref(), Some("house"));
        assert!(!m.uvs.is_empty(), "walls carry brick-band UVs");
        let (_, max) = m.bounds().unwrap();
        // Ridge = wall 4 m + roof min(0.35·10, 3) = 3 m → 7 m; the overhang adds none upward.
        assert!(max[1] > 6.9 && max[1] < 7.3, "ridge at {}: {:?}", max[1], max);
        assert_eq!(m.layer, MeshLayer::Standing);
    }

    #[test]
    fn the_stadium_carries_a_pitch_and_a_bowl() {
        let m = stadium(105.0, 68.0);
        assert!(m.positions.len() > 200, "{} verts", m.positions.len());
        let (min, max) = m.bounds().unwrap();
        assert!(max[0] > 60.0 && min[0] < -60.0, "the bowl surrounds the pitch: {min:?}..{max:?}");
        assert!(max[1] > 3.0, "stands rise: {max:?}");
        // Ground decals sit at 5 cm; the first stand riser starts at the ground (y 0).
        assert!(min[1] >= 0.0 && min[1] <= 0.06, "nothing sinks: {min:?}");
    }

    #[test]
    fn a_tree_and_a_pin_rise_from_the_ground() {
        let t = tree(8.0);
        let (min, max) = t.bounds().unwrap();
        assert!(min[1].abs() < 1e-4 && (max[1] - 8.0).abs() < 1e-4);
        let p = pin(3.0);
        let (_, pmax) = p.bounds().unwrap();
        assert!((pmax[1] - (3.0 * 0.75 + 3.0 * 0.28)).abs() < 0.05, "ball tops the stem: {pmax:?}");
    }
}
