//! The DEM ("terrainium") load: AWS Terrarium elevation tiles assembled around the view.
//!
//! Terrarium tiles (`https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png`,
//! no key) decode one elevation per pixel with `e = R·256 + G + B/256 − 32768` (true metres).
//! The host implements [`DemFetcher`] with its own HTTP workers + disk cache (the same workers
//! the map's imagery tiles use); [`DemLoader`] asks for the window around a centre, stitches
//! every answered tile into a [`DemGrid`] whose rows run uniform in **Mercator** space (slippy
//! rows are uniform in Mercator y, not ground metres — sampling by a uniform ground cell would
//! tilt the terrain against the basemap), gap-fills nodata from neighbours and reports ready.
//!
//! Ported from the ArchCraft (effect) project's `mapkit::terrain` + `arch-geo/dem.rs`.

use crate::mercator::{lonlat_to_tile, merc_y, tile_span_m};

/// DEM zoom (z11 ≈ 76 m/px at the equator; the sweet spot for a city view).
pub const DEM_ZOOM: u32 = 11;
/// The Terrarium tile template; `{z}/{x}/{y}.png` filled in.
pub const TERRARIUM_URL: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";
/// How many tiles around the centre tile the loader asks for (3 = the 3×3 block).
pub const DEM_TILES: u32 = 3;

/// The host-side DEM fetch: given a cache key and URL, get the PNG bytes and `tx.send` a
/// [`DemMsg`] (from any thread; the UI polls the other end every frame). The desktop app
/// implements this with the same ureq workers + disk cache the imagery tiles use.
pub trait DemFetcher: Send + Sync {
    fn fetch(&self, key: String, url: String, tx: Sender<DemMsg>);
}

/// An answered DEM tile: raw elevation metres, row-major 256×256 (NaN where undecodable).
pub struct DemMsg {
    /// The slippy key the ask carried (`"11/4988/4250"`).
    pub key: String,
    /// 256×256 elevations in metres, row-major from the tile's north-west corner.
    pub elev: Vec<f32>,
}

use std::sync::mpsc::Sender;

/// One stitched elevation field: a `(lon, lat)`-anchored window of Terrarium tiles.
///
/// `data` runs row-major from the window's north-west corner; rows are uniform in Mercator
/// space (`merc_north` → `merc_north − merc_span`), columns uniform in longitude.
#[derive(Clone, Debug, Default)]
pub struct DemGrid {
    /// Columns (elevation samples across, longitude direction).
    pub w: usize,
    /// Rows (samples down, north → south).
    pub h: usize,
    /// Ground metres per cell (at the window's top latitude).
    pub cell: f32,
    /// The window's north-west corner latitude (degrees).
    pub lat0: f64,
    /// The window's north-west corner longitude (degrees).
    pub lon0: f64,
    /// Mercator y of the north edge; sampling finds rows in Mercator space (see the crate docs).
    pub merc_north: f64,
    /// The window's total Mercator-y span. `0.0` = unknown → uniform-cell fallback.
    pub merc_span: f64,
    /// Longitude degrees the window spans (columns; the window is whole tiles).
    pub lon_span: f64,
    /// Elevations in true metres (NaN = nodata), row-major, `w × h`.
    pub data: Vec<f32>,
}

impl DemGrid {
    /// Bilinear sample at a lon/lat (true metres). `None` outside the window or where the
    /// neighbourhood is all nodata.
    pub fn sample(&self, lon: f64, lat: f64) -> Option<f32> {
        if self.w < 2 || self.h < 2 || self.lon_span <= 0.0 {
            return None;
        }
        // Columns: longitude IS linear in Mercator x, so the column is exact as-is.
        let gx = (lon - self.lon0) * self.w as f64 / self.lon_span;
        // Rows: found in Mercator space (see the crate docs); uniform-cell fallback when the
        // grid carries no Mercator span (older saves).
        let gy = if self.merc_span != 0.0 {
            (self.merc_north - merc_y(lat)) / self.merc_span * self.h as f64
        } else {
            (self.lat0 - lat) * 111_320.0 / self.cell as f64
        };
        if gx < 0.0 || gy < 0.0 || gx >= self.w as f64 - 1.0 || gy >= self.h as f64 - 1.0 {
            return None;
        }
        let (x0, y0) = (gx.floor() as usize, gy.floor() as usize);
        let (fx, fy) = ((gx - x0 as f64) as f32, (gy - y0 as f64) as f32);
        let g = |xx: usize, yy: usize| self.data[yy * self.w + xx];
        let mut acc = 0.0f32;
        let mut wsum = 0.0f32;
        for (v, w) in
            [(g(x0, y0), (1.0 - fx) * (1.0 - fy)), (g(x0 + 1, y0), fx * (1.0 - fy)), (g(x0, y0 + 1), (1.0 - fx) * fy), (g(x0 + 1, y0 + 1), fx * fy)]
        {
            if v.is_finite() {
                acc += v * w;
                wsum += w;
            }
        }
        (wsum > 0.0).then(|| acc / wsum)
    }

    /// The highest elevation (NaN-safe), for camera framing.
    pub fn max_elev(&self) -> f32 {
        self.data.iter().fold(f32::NEG_INFINITY, |m, v| if v.is_finite() { v.max(m) } else { m })
    }

    /// The lowest finite elevation, for sea carving.
    pub fn min_elev(&self) -> f32 {
        self.data.iter().fold(f32::INFINITY, |m, v| if v.is_finite() { v.min(m) } else { m })
    }
}

// (no extra impls)

/// Decode a Terrarium PNG (256×256) into elevations (metres; NaN where the pixels can't be read).
pub fn decode_terrarium_png(raw: &[u8]) -> Option<Vec<f32>> {
    let img = image::load_from_memory(raw).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    if w != 256 || h != 256 {
        return None;
    }
    let px = img.into_raw();
    let mut out = vec![0f32; 256 * 256];
    for i in 0..256 * 256 {
        let (r, g, b) = (px[i * 4] as f32, px[i * 4 + 1] as f32, px[i * 4 + 2] as f32);
        out[i] = r * 256.0 + g + b / 256.0 - 32768.0;
    }
    Some(out)
}

/// Pure Terrarium elevation math (`elev = R·256 + G + B/256 − 32768`).
pub fn terrarium_elev(r: u8, g: u8, b: u8) -> f32 {
    r as f32 * 256.0 + g as f32 + b as f32 / 256.0 - 32768.0
}

/// The DEM loader: one window around a centre, filled tile by tile as the fetcher answers.
pub struct DemLoader {
    /// The zoom the tiles are asked at.
    pub z: u32,
    /// The tile block's north-west tile (centre tile minus the radius).
    tx0: u32,
    ty0: u32,
    n: u32,
    /// Asks still out.
    pending: std::collections::HashSet<String>,
    /// Answered tiles by slippy key (256×256 elevations).
    tiles: std::collections::HashMap<String, Vec<f32>>,
    /// The answers come back on this side.
    rx: std::sync::mpsc::Receiver<DemMsg>,
    tx: std::sync::mpsc::Sender<DemMsg>,
}

impl DemLoader {
    /// Ask the fetcher for the `DEM_TILES`² window around `(lon, lat)`.
    pub fn new(fetcher: &dyn DemFetcher, lon: f64, lat: f64, z: u32) -> Self {
        let (cx, cy) = lonlat_to_tile(lon, lat, z);
        let n = DEM_TILES;
        let (tx0, ty0) = (cx.saturating_sub(n / 2), cy.saturating_sub(n / 2));
        let (chan, rx) = std::sync::mpsc::channel();
        let mut pending = std::collections::HashSet::new();
        for ty in ty0..ty0 + n {
            for tx in tx0..tx0 + n {
                let key = format!("{z}/{tx}/{ty}");
                let url = format!("{TERRARIUM_URL}/{key}.png");
                pending.insert(key.clone());
                fetcher.fetch(key, url, chan.clone());
            }
        }
        Self { z, tx0, ty0, n, pending, tiles: std::collections::HashMap::new(), rx, tx: chan }
    }

    /// Drain the fetcher's answers (never blocks). Returns how many new tiles landed.
    pub fn tick(&mut self) -> usize {
        let mut landed = 0;
        while let Ok(msg) = self.rx.try_recv() {
            if self.pending.remove(&msg.key) && msg.elev.len() == 256 * 256 {
                self.tiles.insert(msg.key, msg.elev);
                landed += 1;
            }
        }
        landed
    }

    /// Tiles still in flight.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Tiles stitched so far.
    pub fn ready_count(&self) -> usize {
        self.tiles.len()
    }

    /// The ask's sending half (the loader owns it so the fetcher's clones outlive it).
    pub fn sender(&self) -> Sender<DemMsg> {
        self.tx.clone()
    }

    /// Stitch what has landed so far into a [`DemGrid`]. `None` until the first tile arrives.
    /// Missing tiles stay NaN and are gap-filled from their neighbours (a coastal window with
    /// an ocean tile missing still builds).
    pub fn stitch(&self) -> Option<DemGrid> {
        if self.tiles.is_empty() {
            return None;
        }
        let (tw, th) = (self.n as usize * 256, self.n as usize * 256);
        let mut data = vec![f32::NAN; tw * th];
        let mut got = 0;
        for ty in self.ty0..self.ty0 + self.n {
            for tx in self.tx0..self.tx0 + self.n {
                let key = format!("{}/{tx}/{ty}", self.z);
                let Some(px) = self.tiles.get(&key) else { continue };
                let bx = (tx - self.tx0) as usize * 256;
                let by = (ty - self.ty0) as usize * 256;
                for py in 0..256 {
                    data[(by + py) * tw + bx..(by + py) * tw + bx + 256].copy_from_slice(&px[py * 256..(py + 1) * 256]);
                }
                got += 1;
            }
        }
        let _ = got;
        let n_f = (1u64 << self.z) as f64;
        let lon0 = self.tx0 as f64 / n_f * 360.0 - 180.0;
        let lat_top = ((1.0 - 2.0 * self.ty0 as f64 / n_f) * std::f64::consts::PI).sinh().atan().to_degrees();
        let lat_bottom = ((1.0 - 2.0 * (self.ty0 + self.n) as f64 / n_f) * std::f64::consts::PI).sinh().atan().to_degrees();
        let cell = (tile_span_m(lat_top, self.z) / 256.0) as f32;
        let merc_north = merc_y(lat_top);
        let merc_span = merc_north - merc_y(lat_bottom);
        let mut grid = DemGrid { w: tw, h: th, cell, lat0: lat_top, lon0, lon_span: 360.0 / n_f * self.n as f64, merc_north, merc_span, data };
        fill_missing(&mut grid, 12);
        Some(grid)
    }
}

/// Fill NaN (nodata) cells from finite 8-neighbours, one ring per pass. Returns the count still
/// NaN (open ocean beyond the fill reach stays NaN and renders as sea).
pub fn fill_missing(g: &mut DemGrid, passes: usize) -> usize {
    let (w, h) = (g.w, g.h);
    for _ in 0..passes {
        let mut filled = 0usize;
        let src = g.data.clone();
        for r in 0..h {
            for c in 0..w {
                let i = r * w + c;
                if src[i].is_finite() {
                    continue;
                }
                let mut sum = 0f64;
                let mut n = 0usize;
                for dr in -1i64..=1 {
                    for dc in -1i64..=1 {
                        if dr == 0 && dc == 0 {
                            continue;
                        }
                        let (rr, cc) = (r as i64 + dr, c as i64 + dc);
                        if rr < 0 || cc < 0 || rr >= h as i64 || cc >= w as i64 {
                            continue;
                        }
                        let v = src[rr as usize * w + cc as usize];
                        if v.is_finite() {
                            sum += v as f64;
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    g.data[i] = (sum / n as f64) as f32;
                    filled += 1;
                }
            }
        }
        if filled == 0 {
            break;
        }
    }
    g.data.iter().filter(|v| !v.is_finite()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetcher that records asks in a queue (no network in tests); the test drains the queue
    /// and answers each ask itself.
    struct FakeFetch {
        asks: std::sync::Mutex<Vec<(String, String, Sender<DemMsg>)>>,
    }

    impl DemFetcher for FakeFetch {
        fn fetch(&self, key: String, url: String, tx: Sender<DemMsg>) {
            if let Ok(mut q) = self.asks.lock() {
                q.push((key, url, tx));
            }
        }
    }

    fn fake() -> (FakeFetch, impl Iterator<Item = (String, String, Sender<DemMsg>)>) {
        let fetcher = FakeFetch { asks: std::sync::Mutex::new(vec![]) };
        (fetcher, FakeDrain)
    }

    /// Drains a FakeFetch's queue: the test fetches through it.
    struct FakeDrain;
    impl Iterator for FakeDrain {
        type Item = (String, String, Sender<DemMsg>);
        fn next(&mut self) -> Option<Self::Item> {
            None // replaced by explicit ask-draining in each test
        }
    }

    /// Answer every ask the fetcher recorded with `make`'s tile.
    fn answer_all(f: &FakeFetch, make: impl Fn() -> Vec<f32>) {
        let asks = f.asks.lock().unwrap().drain(..).collect::<Vec<_>>();
        for (key, _url, tx) in asks {
            tx.send(DemMsg { key, elev: make() }).unwrap();
        }
    }

    /// A flat 40 m tile with one hill: row r rises to 40 + r/4 m.
    fn tile_with_hill() -> Vec<f32> {
        let mut v = vec![40.0f32; 256 * 256];
        for r in 0..64 {
            for c in 0..64 {
                v[r * 256 + c] = 40.0 + (64 - r) as f32 / 4.0;
            }
        }
        v
    }

    #[test]
    fn terrarium_math_matches_the_spec() {
        // R=1, G=2, B=3: 1·256 + 2 + 3/256 − 32768.
        assert!((terrarium_elev(1, 2, 3) - (256.0 + 2.0 + 3.0 / 256.0 - 32768.0)).abs() < 1e-4);
        assert_eq!(terrarium_elev(128, 0, 0), 0.0);
    }

    #[test]
    fn decode_rejects_a_wrongly_sized_png_and_reads_a_good_one() {
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([128, 0, 0, 255]));
        let mut buf = std::io::Cursor::new(vec![]);
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        assert!(decode_terrarium_png(buf.get_ref()).is_none(), "64×64 is not a Terrarium tile");

        let img = image::RgbaImage::from_fn(256, 256, |x, _y| image::Rgba([128, 0, (x % 256) as u8, 255]));
        let mut buf = std::io::Cursor::new(vec![]);
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let px = decode_terrarium_png(buf.get_ref()).unwrap();
        assert_eq!(px.len(), 256 * 256);
        assert!((px[0] - 0.0).abs() < 0.01);
    }

    #[test]
    fn the_loader_stitches_answered_tiles_into_a_grid() {
        let (fetcher, _) = fake();
        let mut loader = DemLoader::new(&fetcher, 39.2236, -6.7780, DEM_ZOOM);
        assert_eq!(loader.pending_count(), 9, "a 3×3 block asks 9 tiles");
        answer_all(&fetcher, tile_with_hill);
        assert_eq!(loader.tick(), 9);
        assert_eq!(loader.ready_count(), 9);
        let grid = loader.stitch().expect("tiles answered → a grid");
        assert_eq!(grid.w, 768);
        assert_eq!(grid.h, 768);
        assert!(grid.merc_span > 0.0);
        // Somewhere in the window the hill's 56 m crown shows.
        assert!(grid.max_elev() > 50.0, "max {} ", grid.max_elev());
    }

    #[test]
    fn sample_is_none_outside_and_finite_inside() {
        let (fetcher, _) = fake();
        let mut loader = DemLoader::new(&fetcher, 39.2236, -6.7780, DEM_ZOOM);
        answer_all(&fetcher, tile_with_hill);
        loader.tick();
        let grid = loader.stitch().unwrap();
        // Just inside the window's north-west corner samples; far outside it does not.
        assert!(grid.sample(grid.lon0 + 0.001, grid.lat0 - 0.001).is_some());
        assert!(grid.sample(0.0, 0.0).is_none(), "far outside the window");
        assert!(grid.sample(grid.lon0 + grid.lon_span * 0.5, grid.lat0 - 0.004).is_some());
    }

    #[test]
    fn fill_missing_closes_a_hole() {
        let mut g = DemGrid { w: 4, h: 4, cell: 10.0, lat0: 0.0, lon0: 0.0, lon_span: 0.01, merc_north: 0.0, merc_span: 0.0, data: vec![10.0; 16] };
        g.data[5] = f32::NAN;
        g.data[6] = f32::NAN;
        let left = fill_missing(&mut g, 3);
        assert_eq!(left, 0);
        assert!((g.data[5] - 10.0).abs() < 1e-4);
    }
}
