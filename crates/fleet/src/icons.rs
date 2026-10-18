//! The fleet's vector icons, drawn in unit space (−1…1, nose pointing +Y = north) as closed
//! polygons — the same geometry the 2D canvas fills with the painter and the 3D environment
//! triangulates into a ground decal, so both canvases read one icon set designed once with the
//! drawing engine's own primitives.
//!
//! The outlines: a car (tapered nose, wide shoulders), a boxier van, a motorcycle's slim lens, a
//! ship (pointed bow, notched stern) and the plane everyone knows. [`HEX`] is the H3 hexagon the
//! 3D markers stand on.

/// Car / ride-hail marker.
pub const CAR: &[(f32, f32)] = &[
    (0.24, 1.00),
    (0.40, 0.60),
    (0.42, -0.10),
    (0.36, -0.55),
    (0.24, -0.92),
    (0.12, -1.00),
    (-0.12, -1.00),
    (-0.24, -0.92),
    (-0.36, -0.55),
    (-0.42, -0.10),
    (-0.40, 0.60),
    (-0.24, 1.00),
];

/// Van / delivery marker (boxier, longer roof).
pub const VAN: &[(f32, f32)] =
    &[(0.30, 1.00), (0.44, 0.80), (0.44, -0.85), (0.30, -1.00), (-0.30, -1.00), (-0.44, -0.85), (-0.44, 0.80), (-0.30, 1.00)];

/// Motorcycle / bike marker (the slim two-wheel lens).
pub const BIKE: &[(f32, f32)] =
    &[(0.10, 1.00), (0.16, 0.70), (0.16, -0.55), (0.10, -1.00), (-0.10, -1.00), (-0.16, -0.55), (-0.16, 0.70), (-0.10, 1.00)];

/// Vessel marker: pointed bow, squared stern with the aft-deck notch.
pub const SHIP: &[(f32, f32)] = &[
    (0.00, 1.00),
    (0.28, 0.55),
    (0.34, 0.10),
    (0.34, -0.70),
    (0.22, -0.86),
    (0.10, -0.86),
    (0.08, -0.72),
    (-0.08, -0.72),
    (-0.10, -0.86),
    (-0.22, -0.86),
    (-0.34, -0.70),
    (-0.34, 0.10),
    (-0.28, 0.55),
];

/// Aircraft marker.
pub const PLANE: &[(f32, f32)] = &[
    (0.00, 1.00),
    (0.10, 0.72),
    (0.12, 0.30),
    (0.72, -0.06),
    (0.72, -0.18),
    (0.14, -0.10),
    (0.12, -0.55),
    (0.30, -0.78),
    (0.30, -0.88),
    (0.06, -0.80),
    (0.04, -1.00),
    (-0.04, -1.00),
    (-0.06, -0.80),
    (-0.30, -0.88),
    (-0.30, -0.78),
    (-0.12, -0.55),
    (-0.14, -0.10),
    (-0.72, -0.18),
    (-0.72, -0.06),
    (-0.12, 0.30),
    (-0.10, 0.72),
];

/// Heavy / wide-body aircraft marker: the same planform with a fatter fuselage and two deck
/// rows of engines (a wide-body reads wider at the same size).
pub const PLANE_HEAVY: &[(f32, f32)] = &[
    (0.00, 1.00),
    (0.15, 0.72),
    (0.18, 0.30),
    (0.88, -0.02),
    (0.88, -0.20),
    (0.18, -0.08),
    (0.16, -0.52),
    (0.34, -0.76),
    (0.34, -0.88),
    (0.07, -0.78),
    (0.05, -1.00),
    (-0.05, -1.00),
    (-0.07, -0.78),
    (-0.34, -0.88),
    (-0.34, -0.76),
    (-0.16, -0.52),
    (-0.18, -0.08),
    (-0.88, -0.20),
    (-0.88, -0.02),
    (-0.18, 0.30),
    (-0.15, 0.72),
];

/// Light aircraft marker: the slim GA planform (short wings, single tail).
pub const PLANE_LIGHT: &[(f32, f32)] = &[
    (0.00, 1.00),
    (0.09, 0.66),
    (0.10, 0.34),
    (0.58, -0.02),
    (0.58, -0.16),
    (0.11, -0.06),
    (0.09, -0.50),
    (0.18, -0.72),
    (0.18, -0.84),
    (0.05, -0.80),
    (0.04, -1.00),
    (-0.04, -1.00),
    (-0.05, -0.80),
    (-0.18, -0.84),
    (-0.18, -0.72),
    (-0.09, -0.50),
    (-0.11, -0.06),
    (-0.58, -0.16),
    (-0.58, -0.02),
    (-0.10, 0.34),
    (-0.09, 0.66),
];

/// Helicopter marker: the rotor-up planform — the rotor bar across the top, the cabin below
/// it, the tail boom running south. Drawn pointing north like the rest (the heading still
/// rotates it; rotorcraft often have no track, then it sits).
pub const HELI: &[(f32, f32)] = &[
    (-0.90, 0.82),
    (-0.10, 0.82),
    (-0.08, 0.52),
    (-0.24, 0.40),
    (-0.24, 0.00),
    (-0.08, -0.08),
    (-0.05, -0.82),
    (-0.13, -0.94),
    (-0.13, -1.00),
    (0.13, -1.00),
    (0.13, -0.94),
    (0.05, -0.82),
    (0.08, -0.08),
    (0.24, 0.00),
    (0.24, 0.40),
    (0.08, 0.52),
    (0.10, 0.82),
    (0.90, 0.82),
    (0.90, 1.00),
    (-0.90, 1.00),
];

/// Train marker: the front view of a multiple unit — the roof with the windshield notch, the
/// body and the skirt, drawn pointing north (+Y) so the heading rotation reads the same.
pub const TRAIN: &[(f32, f32)] = &[
    (-0.40, 1.00),
    (-0.24, 1.00),
    (-0.24, 0.42),
    (0.24, 0.42),
    (0.24, 1.00),
    (0.40, 1.00),
    (0.40, -0.55),
    (0.26, -0.88),
    (0.26, -1.00),
    (-0.26, -1.00),
    (-0.26, -0.88),
    (-0.40, -0.55),
];

/// The H3 hexagon (pointy-top), the fleet markers' base plate in 3D.
pub const HEX: &[(f32, f32)] = &[(0.0, 1.0), (-0.866, 0.5), (-0.866, -0.5), (0.0, -1.0), (0.866, -0.5), (0.866, 0.5)];

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_area(poly: &[(f32, f32)]) -> f32 {
        let mut a = 0.0;
        for i in 0..poly.len() {
            let (x1, y1) = poly[i];
            let (x2, y2) = poly[(i + 1) % poly.len()];
            a += x1 * y2 - x2 * y1;
        }
        a / 2.0
    }

    #[test]
    fn every_outline_is_closed_convexish_and_north_up() {
        for (name, poly) in [
            ("car", CAR),
            ("van", VAN),
            ("bike", BIKE),
            ("ship", SHIP),
            ("plane", PLANE),
            ("plane-heavy", PLANE_HEAVY),
            ("plane-light", PLANE_LIGHT),
            ("heli", HELI),
            ("train", TRAIN),
            ("hex", HEX),
        ] {
            assert!(poly.len() >= 6, "{name} too few points");
            let area = signed_area(poly);
            assert!(area.abs() > 0.5, "{name} covers too little: {area}");
            let ys: Vec<f32> = poly.iter().map(|p| p.1).collect();
            let max_y = ys.iter().cloned().fold(f32::MIN, f32::max);
            assert!((max_y - 1.0).abs() < 0.35, "{name} nose must reach up: {max_y}");
            for (x, y) in poly {
                assert!(x.abs() <= 1.0 && y.abs() <= 1.0, "{name} out of unit space: ({x},{y})");
            }
        }
    }

    #[test]
    fn hex_is_the_h3_hexagon() {
        assert_eq!(HEX.len(), 6);
        assert!((signed_area(HEX) - 2.598).abs() < 0.01, "unit hexagon area 3√3/2");
    }
}
