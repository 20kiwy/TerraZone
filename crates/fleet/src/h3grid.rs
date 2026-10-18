//! H3 grid helpers: the view covers H3 cells at a resolution the map zoom picks, and each cell
//! names where its marker sits. Resolution 3 covers whole countries, 9 city blocks — the ladder
//! keeps the per-poll cell count sane at every zoom.

use h3o::{CellIndex, Resolution};

/// The resolution a map zoom level asks for: res 3 at world views ("from 3"), one rung per ~2.5
/// zoom levels, capped at 9 so a street zoom can't flood the feed with thousands of cells.
pub fn res_for_zoom(map_zoom: f64) -> Resolution {
    const LADDER: [u8; 7] = [3, 4, 5, 6, 7, 8, 9];
    let step = ((map_zoom - 4.0) / 2.5).round();
    let i = step.clamp(0.0, (LADDER.len() - 1) as f64) as usize;
    Resolution::try_from(LADDER[i]).unwrap_or(Resolution::Three)
}

/// The approximate east-west edge length of one cell at `res`, in degrees of longitude at the
/// equator (used to step the covering sampler, and to keep demo traffic inside its cell).
pub fn cell_span_deg(res: Resolution) -> f64 {
    // Mean hexagon edge ~ edge_length_km(res); 1° of longitude at the equator ≈ 111.32 km.
    let km = match res {
        Resolution::Zero => 1107.7,
        Resolution::One => 418.7,
        Resolution::Two => 158.2,
        Resolution::Three => 59.8,
        Resolution::Four => 22.6,
        Resolution::Five => 8.5,
        Resolution::Six => 3.2,
        Resolution::Seven => 1.2,
        Resolution::Eight => 0.46,
        _ => 0.17,
    };
    km / 111.32
}

/// The H3 cells covering a lon/lat box (degrees, west/south/east/north), sampled on a grid one
/// cell-edge apart and deduped. Capped: if a resolution would cover more than `cap` cells the
/// caller asked for too fine a grid at too wide a view — the sampler coarsens nothing, the zoom
/// ladder (`res_for_zoom`) is what keeps the count sane; past the cap the set truncates.
pub fn cells_covering(west: f64, south: f64, east: f64, north: f64, res: Resolution, cap: usize) -> Vec<CellIndex> {
    let (west, east) = if west <= east { (west, east) } else { (east, west) };
    let (south, north) = if south <= north { (south, north) } else { (north, south) };
    // Mercator latitude limits (the map can't show beyond these anyway).
    let south = south.clamp(-85.0, 85.0);
    let north = north.clamp(-85.0, 85.0);
    let step = (cell_span_deg(res) * 1.5).max(1e-4);
    let cols = (((east - west) / step).ceil() as usize).saturating_add(1).min(256);
    let rows = (((north - south) / (step * 0.87)).ceil() as usize).saturating_add(1).min(256);
    let mut seen: Vec<CellIndex> = Vec::new();
    for r in 0..rows {
        let lat = if rows == 1 { (south + north) / 2.0 } else { south + (north - south) * r as f64 / (rows - 1) as f64 };
        for c in 0..cols {
            let lon = if cols == 1 { (west + east) / 2.0 } else { west + (east - west) * c as f64 / (cols - 1) as f64 };
            let Some(cell) = h3o::LatLng::new(lat, lon).ok().map(|ll| ll.to_cell(res)) else { continue };
            if !seen.contains(&cell) {
                seen.push(cell);
                if seen.len() >= cap {
                    return seen;
                }
            }
        }
    }
    seen
}

/// A cell's centre in degrees `(lon, lat)`.
pub fn cell_center(cell: CellIndex) -> (f64, f64) {
    let ll = h3o::LatLng::from(cell);
    (ll.lng(), ll.lat())
}

/// A cell's boundary ring in degrees, `(lon, lat)` pairs, counter-clockwise, not closed (the
/// polygon shapes close themselves) — the ring the map outlines for the grid.
pub fn cell_boundary(cell: CellIndex) -> Vec<(f64, f64)> {
    cell.boundary().iter().map(|ll| (ll.lng(), ll.lat())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latlng(lon: f64, lat: f64) -> h3o::LatLng {
        h3o::LatLng::new(lat, lon).unwrap()
    }

    #[test]
    fn zoom_ladder_runs_from_three_to_nine() {
        assert_eq!(res_for_zoom(0.0), Resolution::Three);
        assert_eq!(res_for_zoom(4.0), Resolution::Three, "z4 is 100% — still a wide view");
        assert_eq!(res_for_zoom(13.0), Resolution::Seven);
        assert_eq!(res_for_zoom(19.0), Resolution::Nine);
        assert_eq!(res_for_zoom(30.0), Resolution::Nine, "capped");
    }

    #[test]
    fn covering_finds_the_centre_cell_and_stays_bounded() {
        // A Sinza-sized box (about a neighbourhood) at res 8 covers a handful of cells.
        let res = Resolution::Eight;
        let cells = cells_covering(39.20, -6.79, 39.24, -6.76, res, 512);
        assert!(!cells.is_empty());
        assert!(cells.len() < 60, "{} cells for a small box", cells.len());
        let centre = latlng(39.223, -6.778).to_cell(res);
        assert!(cells.contains(&centre), "the centre's own cell must be in the covering");
        // A world box truncates at the cap instead of exploding.
        let world = cells_covering(-180.0, -85.0, 180.0, 85.0, Resolution::Nine, 64);
        assert_eq!(world.len(), 64);
    }

    #[test]
    fn centers_land_inside_their_cell() {
        let cell = latlng(39.223, -6.778).to_cell(Resolution::Seven);
        let (lon, lat) = cell_center(cell);
        let round = latlng(lon, lat).to_cell(Resolution::Seven);
        assert_eq!(round, cell);
    }

    #[test]
    fn boundary_rings_are_hexagons_around_the_center() {
        for cell in cells_covering(39.20, -6.79, 39.24, -6.76, Resolution::Eight, 8) {
            let ring = cell_boundary(cell);
            assert!((5..=6).contains(&ring.len()), "{} verts", ring.len());
            let (clon, clat) = cell_center(cell);
            let lons: Vec<f64> = ring.iter().map(|(lon, _)| *lon).collect();
            let lats: Vec<f64> = ring.iter().map(|(_, lat)| *lat).collect();
            let (min_lon, max_lon) = (lons.iter().cloned().fold(f64::MAX, f64::min), lons.iter().cloned().fold(f64::MIN, f64::max));
            let (min_lat, max_lat) = (lats.iter().cloned().fold(f64::MAX, f64::min), lats.iter().cloned().fold(f64::MIN, f64::max));
            assert!(clon > min_lon && clon < max_lon && clat > min_lat && clat < max_lat, "the centre sits inside its ring");
        }
    }
}
