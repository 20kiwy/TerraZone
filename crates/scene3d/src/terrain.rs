//! Contour tracing over a [`DemGrid`] — the one terrain render the 3D environment still does
//! (the elevation mesh is gone: the ground drapes flat, and the relief reads from the contour
//! lines, each major line carrying its height as text along the path). Contours come from
//! marching squares over the DEM window. Everything lands in the world frame
//! [`mercator::Mercator`] projects to: `x` east, `z` south, `y` up, metres.

use crate::dem::DemGrid;

/// Marching-squares contours over the grid, site-relative metres (`v − base`).
#[derive(Clone, Debug, Default)]
pub struct Contours {
    /// Minor-interval segments `[x1, z1, x2, z2]` (world metres).
    pub minor: Vec<[f32; 4]>,
    /// Every-`major_every`-th segments (drawn stronger, and labelled).
    pub major: Vec<[f32; 4]>,
    /// The height labels: `(x, z)` world metres on a major line, and the line's elevation in
    /// TRUE site-relative metres (the text the line carries).
    pub labels: Vec<([f32; 2], f32)>,
    /// Metres between minor lines.
    pub interval: f32,
    /// Every Nth line is major.
    pub major_every: u32,
}

impl Contours {
    /// Nothing traced?
    pub fn is_empty(&self) -> bool {
        self.minor.is_empty() && self.major.is_empty()
    }

    /// Segment count (all levels).
    pub fn segment_count(&self) -> usize {
        self.minor.len() + self.major.len()
    }
}

/// Trace contours at `interval` metres (major every `major_every`), on a lattice strided
/// `stride` cells — run once at load, culled per frame. Major lines get a height label at
/// their segment midpoints (the draw culls them by screen spacing).
pub fn contours(grid: &DemGrid, base: f32, interval: f32, major_every: u32, stride: usize) -> Contours {
    let mut out = Contours { minor: vec![], major: vec![], labels: vec![], interval, major_every: major_every.max(1) };
    if interval <= 0.0 || grid.w < 2 || grid.h < 2 {
        return out;
    }
    let s = stride.max(1);
    // Lattice points (clamped to the grid edge) and their world coords. The DEM window is
    // a lat/lon rect drawn in Mercator metres: the cell size in world x is uniform per column,
    // so a uniform lattice in grid space is close enough for contour polylines.
    let npx = (grid.w - 1).div_ceil(s);
    let npy = (grid.h - 1).div_ceil(s);
    let wx = |sx: usize| (sx as f32 + 0.5 - grid.w as f32 / 2.0) * grid.cell;
    let wz = |sy: usize| -((sy as f32 + 0.5 - grid.h as f32 / 2.0) * grid.cell);
    let h = |ix: usize, iy: usize| -> f32 { grid.data[(iy.min(grid.h - 1)) * grid.w + ix.min(grid.w - 1)] - base };
    let lerp = |ha: f32, wa: (f32, f32), hb: f32, wb: (f32, f32), l: f32| -> (f32, f32) {
        let t = ((l - ha) / (hb - ha)).clamp(0.0, 1.0);
        (wa.0 + t * (wb.0 - wa.0), wa.1 + t * (wb.1 - wa.1))
    };
    for jy in 0..npy {
        let sy0 = (jy * s).min(grid.h - 1);
        let sy1 = ((jy + 1) * s).min(grid.h - 1);
        if sy1 == sy0 {
            continue;
        }
        for jx in 0..npx {
            let sx0 = (jx * s).min(grid.w - 1);
            let sx1 = ((jx + 1) * s).min(grid.w - 1);
            if sx1 == sx0 {
                continue;
            }
            // Cell corners (a=top-left, b=top-right, c=bottom-right, d=bottom-left), metres
            // relative to the datum.
            let (ha, hb, hc, hd) = (h(sx0, sy0), h(sx1, sy0), h(sx1, sy1), h(sx0, sy1));
            let (pa, pb, pc, pd) = ((wx(sx0), wz(sy0)), (wx(sx1), wz(sy0)), (wx(sx1), wz(sy1)), (wx(sx0), wz(sy1)));
            let mn = ha.min(hb).min(hc).min(hd);
            let mx = ha.max(hb).max(hc).max(hd);
            if !mn.is_finite() || !mx.is_finite() {
                continue;
            }
            // Levels crossing this cell (snapped to the interval ladder).
            let l0 = (mn / interval).floor() as i64 + 1;
            let l1 = (mx / interval).ceil() as i64;
            for l in l0..l1 {
                let level = l as f32 * interval;
                // Edges crossed, marching-squares style (below/above per corner).
                let mut pts: Vec<(f32, f32)> = Vec::with_capacity(4);
                if (ha < level) != (hb < level) {
                    pts.push(lerp(ha, pa, hb, pb, level));
                }
                if (hb < level) != (hc < level) {
                    pts.push(lerp(hb, pb, hc, pc, level));
                }
                if (hc < level) != (hd < level) {
                    pts.push(lerp(hc, pc, hd, pd, level));
                }
                if (hd < level) != (ha < level) {
                    pts.push(lerp(hd, pd, ha, pa, level));
                }
                if pts.len() >= 2 {
                    let seg = [pts[0].0, pts[0].1, pts[1].0, pts[1].1];
                    if l.rem_euclid(out.major_every as i64) == 0 {
                        // The major line carries its height: a label at the segment midpoint.
                        out.labels.push(([(seg[0] + seg[2]) * 0.5, (seg[1] + seg[3]) * 0.5], level));
                        out.major.push(seg);
                    } else {
                        out.minor.push(seg);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dem::DemGrid;
    use crate::mercator::merc_y;

    fn flat_grid() -> DemGrid {
        DemGrid {
            w: 4,
            h: 4,
            cell: 50.0,
            lat0: -6.7,
            lon0: 39.2,
            lon_span: 0.02,
            merc_north: merc_y(-6.7),
            merc_span: merc_y(-6.7) - merc_y(-6.72),
            data: vec![30.0; 16],
        }
    }

    #[test]
    fn contours_land_on_the_interval_ladder() {
        let mut g = flat_grid();
        // A ramp 5..65 m across the rows (off the 20 m ladder so no level runs along a corner):
        // contours at 20/40/60 each cross once.
        for r in 0..4 {
            for c in 0..4 {
                g.data[r * 4 + c] = 20.0 * r as f32 + 5.0;
            }
        }
        let cs = contours(&g, 0.0, 20.0, 2, 1);
        assert!(!cs.is_empty());
        // The ramp runs across the rows, so each level crosses every lattice column: 3 segments
        // per level (20/40/60) that join into one polyline when drawn.
        assert_eq!(cs.major.len() + cs.minor.len(), 9, "{:?} + {:?}", cs.major, cs.minor);
        assert_eq!(cs.major.len(), 3, "every 2nd (40) is major: {:?}", cs.major);
        // Each level's segments sit at one z (its crossing height): they join end to end.
        let zs: std::collections::HashSet<i32> = cs.major.iter().map(|s| s[1].round() as i32).collect();
        assert_eq!(zs.len(), 1, "the major level's segments share one height: {zs:?}");
    }

    #[test]
    fn major_lines_carry_their_height_as_labels() {
        let mut g = flat_grid();
        for r in 0..4 {
            for c in 0..4 {
                g.data[r * 4 + c] = 20.0 * r as f32 + 5.0;
            }
        }
        let cs = contours(&g, 0.0, 20.0, 2, 1);
        // One label per major segment, at the segment's midpoint, reading the line's level.
        assert_eq!(cs.labels.len(), cs.major.len(), "a label rides every major segment");
        for (at, level) in &cs.labels {
            assert!((*level - 40.0).abs() < 1e-3, "the ramp's only major level is 40 m: {level}");
            let on_a_line = cs.major.iter().any(|s| (at[0] - (s[0] + s[2]) * 0.5).abs() < 1e-3 && (at[1] - (s[1] + s[3]) * 0.5).abs() < 1e-3);
            assert!(on_a_line, "the label sits on its line's midpoint: {at:?}");
        }
        // Interval 0 (off) traces nothing at all.
        assert!(contours(&g, 0.0, 0.0, 2, 1).is_empty(), "the contour switch off traces nothing");
    }
}
