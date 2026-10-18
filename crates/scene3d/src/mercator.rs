//! THE one Mercator projection for the 3D world — spherical Web Mercator rescaled to ground
//! metres at the scene centre (`1 world unit = 1 ground metre` at `(lat0, lon0)`).
//!
//! The 2D canvas maps lat/lon through document points (`terrazon_map::lonlat_to_doc`); the 3D
//! environment maps the same ellipsis-of-a-sphere math into metres around a centre so DEM
//! heights, imagery tiles, extruded buildings and placed assets all agree. World axes: `x`
//! east, `y` up, `z` south (north is `−z`) — the convention every module here shares.
//!
//! Ported from the ArchCraft (effect) project's `mapkit::mercator` (the same math the 2D
//! document's Web Mercator uses, in ground metres instead of document points).

/// Equatorial circumference, metres.
pub const EARTH_C: f64 = 40_075_016.685_578_6;

/// The ground-metre projection around one scene centre.
#[derive(Clone, Copy, Debug)]
pub struct Mercator {
    /// The centre latitude (degrees).
    pub lat0: f64,
    /// The centre longitude (degrees).
    pub lon0: f64,
    k: f64,
    mx0: f64,
    my0: f64,
}

impl Mercator {
    /// A projection around `(lat0, lon0)`.
    pub fn new(lat0: f64, lon0: f64) -> Self {
        let k = lat0.to_radians().cos();
        Self { lat0, lon0, k, mx0: lon_to_mx(lon0), my0: lat_to_my(lat0) }
    }

    /// lat/lon (degrees) → ground metres east/north of the centre.
    pub fn project(&self, lon: f64, lat: f64) -> (f64, f64) {
        ((lon_to_mx(lon) - self.mx0) * self.k, (lat_to_my(lat) - self.my0) * self.k)
    }

    /// Ground metres east/north → lat/lon (degrees).
    pub fn unproject(&self, x: f64, y: f64) -> (f64, f64) {
        let mx = x / self.k + self.mx0;
        let my = y / self.k + self.my0;
        let lon = mx / EARTH_C * 360.0;
        let phi = (my / (EARTH_C / (2.0 * std::f64::consts::PI))).sinh().atan();
        (lon, phi.to_degrees())
    }
}

fn lon_to_mx(lon: f64) -> f64 {
    lon / 360.0 * EARTH_C
}

fn lat_to_my(lat: f64) -> f64 {
    (lat.to_radians().tan().asinh()) * (EARTH_C / (2.0 * std::f64::consts::PI))
}

/// Raw Mercator y of a latitude (unit sphere — spans cancel in row math).
pub fn merc_y(lat_deg: f64) -> f64 {
    lat_deg.to_radians().tan().asinh()
}

/// Tile x/y for a lon/lat at zoom `z` (slippy scheme, y 0 at the north).
pub fn lonlat_to_tile(lon: f64, lat: f64, z: u32) -> (u32, u32) {
    let n = (1u64 << z) as f64;
    let x = ((lon + 180.0) / 360.0 * n).floor().max(0.0) as u32;
    let lat = lat.to_radians();
    let y = (((1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * n).floor().clamp(0.0, n - 1.0)) as u32;
    (x, y)
}

/// Ground metres per tile edge at a latitude / zoom.
pub fn tile_span_m(lat: f64, z: u32) -> f64 {
    EARTH_C * lat.to_radians().cos() / (1u64 << z) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn project_roundtrips_through_the_centre() {
        let m = Mercator::new(-6.78, 39.22);
        let (x, y) = m.project(39.22, -6.78);
        assert!(close(x, 0.0, 1e-6) && close(y, 0.0, 1e-6));
        for (lon, lat) in [(39.30, -6.80), (39.10, -6.70), (39.5, -7.1)] {
            let (x, y) = m.project(lon, lat);
            let (lo, la) = m.unproject(x, y);
            assert!(close(lo, lon, 1e-6) && close(la, lat, 1e-6), "{lon},{lat} → {lo},{la}");
        }
    }

    #[test]
    fn one_degree_of_longitude_is_the_metres_its_latitude_says() {
        // At the equator 1° lon = 111.32 km; at lat 60 it halves.
        let (eq, hi) = (Mercator::new(0.0, 0.0), Mercator::new(60.0, 0.0));
        let (x_eq, _) = eq.project(1.0, 0.0);
        let (x_hi, _) = hi.project(1.0, 60.0);
        assert!(close(x_eq, 111_319.49, 1.0));
        assert!(close(x_hi, x_eq * 0.5, 1.0));
    }

    #[test]
    fn lonlat_to_tile_matches_the_slippy_scheme() {
        // Sinza, Dar es Salaam at z13 — the tile the 2D canvas shows the same spot through.
        let (x, y) = lonlat_to_tile(39.2236, -6.7780, 13);
        assert_eq!((x, y), (4988, 4250));
        assert!(close(tile_span_m(-6.78, 13), 4857.78, 1.0));
    }
}
