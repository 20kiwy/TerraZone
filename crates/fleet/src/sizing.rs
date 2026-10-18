//! The marker sizing ladder — the **marine sizing**: vendors report vessel lengths, and the
//! marker scale reads that length through one shared ladder. Everything in the fleet sizes from
//! it, so a Bolt van and a 25 m trawler read at the same visual weight, and a super-tanker
//! dominates a bike the way it dominates the road.

use crate::VehicleKind;

/// The number of size classes.
pub const CLASSES: usize = 7;

/// Marker size in screen points per class (small dinghy … super-tanker).
pub const PX: [f32; CLASSES] = [7.0, 9.0, 11.0, 14.0, 18.0, 23.0, 30.0];

/// Marker footprint in world metres per class (the 3D prim uses this).
pub const METRES: [f32; CLASSES] = [4.0, 6.0, 8.0, 12.0, 18.0, 26.0, 40.0];

/// The class a reported length falls in: the marine ladder's bins
/// (≤7, ≤14, ≤21, ≤35, ≤60, ≤120, beyond).
pub fn class_for_length(length_m: f32) -> usize {
    let l = if length_m.is_finite() && length_m > 0.0 { length_m } else { 0.0 };
    if l <= 7.0 {
        0
    } else if l <= 14.0 {
        1
    } else if l <= 21.0 {
        2
    } else if l <= 35.0 {
        3
    } else if l <= 60.0 {
        4
    } else if l <= 120.0 {
        5
    } else {
        6
    }
}

/// The class a vehicle without a reported length gets: its kind's base size on the same ladder.
pub fn class_for_kind(kind: VehicleKind) -> usize {
    match kind {
        VehicleKind::Motorcycle => 0,
        VehicleKind::Car => 2,
        VehicleKind::Van => 3,
        VehicleKind::Boat => 2,
        VehicleKind::Ship => 4,
        VehicleKind::Plane => 3,
        VehicleKind::PlaneHeavy => 5,
        VehicleKind::PlaneLight => 1,
        VehicleKind::Helicopter => 2,
        VehicleKind::Train => 4,
    }
}

/// The class a vehicle draws at: its own length when the vendor reports one, else its kind.
pub fn class_of(kind: VehicleKind, length_m: Option<f32>) -> usize {
    match length_m {
        Some(l) => class_for_length(l),
        None => class_for_kind(kind),
    }
}

/// Marker size in points.
pub fn px(class: usize) -> f32 {
    PX[class.min(CLASSES - 1)]
}

/// Marker footprint in metres (3D).
pub fn metres(class: usize) -> f32 {
    METRES[class.min(CLASSES - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marine_bins_match_the_ladder() {
        assert_eq!(class_for_length(5.0), 0, "dinghy");
        assert_eq!(class_for_length(7.0), 0);
        assert_eq!(class_for_length(7.5), 1, "pilot boat");
        assert_eq!(class_for_length(18.0), 2);
        assert_eq!(class_for_length(25.0), 3, "trawler");
        assert_eq!(class_for_length(55.0), 4, "coaster");
        assert_eq!(class_for_length(110.0), 5);
        assert_eq!(class_for_length(300.0), 6, "super-tanker");
        assert_eq!(class_for_length(-3.0), 0, "junk lengths clamp to the bottom bin");
        assert_eq!(class_for_length(f32::NAN), 0);
    }

    #[test]
    fn kinds_land_on_the_same_ladder() {
        assert_eq!(class_for_kind(VehicleKind::Motorcycle), 0);
        assert_eq!(class_for_kind(VehicleKind::Car), 2);
        assert_eq!(class_for_kind(VehicleKind::Ship), 4);
        // Reported length wins over the kind's base.
        assert_eq!(class_of(VehicleKind::Ship, Some(3.0)), 0, "a reported jet-ski is small");
        assert_eq!(class_of(VehicleKind::Car, None), 2);
        // The ladders rise with the class (const arrays: check the values individually).
        let [p0, _, _, p3, _, _, p6] = PX;
        assert!(p0 < p3 && p3 < p6, "{p0} < {p3} < {p6}");
        let [m0, _, _, m3, _, _, m6] = METRES;
        assert!(m0 < m3 && m3 < m6, "{m0} < {m3} < {m6}");
    }
}
