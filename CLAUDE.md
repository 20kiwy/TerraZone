# TerraZone — instructions for agents

TerraZone is a Rust-native **map illustration editor**: the document is the Web Mercator world
(EPSG:3857) with a live XYZ tile basemap, and Illustrator-style vector tools draw zones, paths and
labels on top of it. Desktop (eframe) first; the WASM web build stays compilable. TerraZone is an independent
open-source project (https://github.com/terrazon-app/terrazon, MIT OR Apache-2.0).

## Start every session here
1. Skim this file and `README.md`. In TerraZone's docs "the document" means **the map**:
   document points are Mercator world points (`terrazon_map`: one point = one tile
   pixel of zoom 4, the world is 4096 × 4096 pt, y points south).
2. Read the honest gap list below unless the user gives you a task.
3. Work autonomously: orient → plan → implement + test → verify → record.

## Non-negotiables (inherited from upstream, still in force)
- **Never crash: no panics in shipped code.** `unwrap`/`expect`/`panic!`/`todo!` are denied by
  workspace lints; untrusted input (files, tiles' bytes, MCP/control messages) returns `Result`.
  Tile responses are capped (8 MB) and PNG-decoded fallibly (`crates/ui-egui/src/tiles.rs`).
- **Everything is a command.** Map behaviour = commands in `crates/engine/src/cmd/map.rs`
  (`map.setSource`, `map.showTiles`, `map.addSource`, `map.removeSource`, `map.sources`) with
  tests; tools and UI act through `Session::execute`.
- **Layering** is enforced by `cargo xtask layers`: `terrazon-map` is Layer 0 (serde only, no
  geometry, no IO); `doc` depends on it for `Document.map` (`terrazon_map::MapSettings`); the
  network lives only in the app (`apps/terrazon/src/tiles.rs`, ureq workers) and reaches the UI
  through `Services::tile_fetch` (`crates/ui-egui/src/tiles.rs`). Nothing below L6 depends on
  egui/eframe/winit/rfd; the web build has `tile_fetch: None` and draws placeholders.
- **Never break wasm** (`cargo xtask wasm`): the tile cache is `std::sync::mpsc` + egui textures
  only; no threads are spawned below the app layer.
- **The UI is thin**: panels read engine state and act through `app.run(id, params)`.
- **Rust only** (no handwritten JS/TS).
- **Clean-room**: never use Adobe software or copy Adobe-authored code/assets. Don't copy GPL code.
- **Assets**: every image/icon/font has a row in `ASSETS.md`; third-party material only under an
  open licence, attributed.

## The map's moving parts
- `crates/map` — Web Mercator ↔ document points, `TileId`/`tiles_for_view` (which zoom's tiles to
  draw and which of them intersect the view), `TileSource`/`MapSettings` (serde, saved in the
  document), built-in sources (OSM, CARTO light/dark, Esri, OpenTopoMap) with attributions.
- `crates/doc` — `Document.map: MapSettings` (serde default, so older files load).
- `crates/engine/src/cmd/map.rs` — the map commands (one undo step each).
- `crates/engine/src/cmd/fileio/{geojson,shp}.rs` — the importers/exporter (WGS84 in and out;
  EvenOdd polygons so holes round-trip; caps on nodes/points; warnings for non-WGS84 coordinates).
- `crates/engine/src/cmd/fileio/mod.rs` — the `FORMATS` table (append-only), `OPEN_EXTS`,
  `PLACE_EXTS`; `save.rs` `SAVE_FORMATS` includes GeoJSON.
- `crates/ui-egui/src/tiles.rs` — fetch requests/answers over a channel, PNG decode, texture cache
  (pruned), placeholder grid while tiles load, the canvas attribution line and the pointer's
  lat/lon readout. Drawn in `canvas.rs::show` under the art; artboard paper is skipped while tiles
  show.
- `crates/ui-egui/src/canvas.rs` — `zoom_range()` widens the zoom clamps (0.2%…65 536×) for map
  documents; the home screen's basemap cards; `state.rs` `View::of` reopens saved map views.
- `apps/terrazon/src/tiles.rs` — two ureq workers, a TerraZone user agent, 10 s timeout, 8 MB cap,
  `TileMsg::{Done,Failed}` back on the per-request channel.
- Menus/dialogs: Map menu (`menus.rs`), the Tile Sources dialog (`dialogs/map_sources.rs`,
  kind `mapSources`).

## Where the work is (honest gap list)
- Zone naming UX: imported features take their `name`/`title` properties (GeoJSON); SHP names need
  a `.dbf` sidecar reader (currently "Zone N").
- Exports carry vector art only (no basemap tiles) by design; a "map render" export could composite
  tiles CPU-side later.
- No i18n rows yet for the new English strings (Tile Sources dialog, Map menu, home cards) — the
  catalogs fall back to English; `cargo xtask ci`'s menu-coverage test will ask for them.
- The vector tools' snap/hit tolerances are screen-space already, but nobody has tuned them at
  street-level zooms yet.
- WASM tiles: `Services::tile_fetch` is `None` on the web; a fetch-backed implementation (or a
  proxy) is future work.

## Running and looking at the app
- `cargo run --release -p terrazon -- --control 7979` then JSON lines on `127.0.0.1:7979`:
  `{"id":1,"method":"engine.execute","params":{"command":"file.new","params":{"preset":"World Map"}}}`
  → `{"id":2,"method":"ui.screenshot","params":{"path":"/tmp/shot.png"}}`. On a headless machine,
  run under `xvfb-run` (the glow build variant works where wgpu has no adapter).
- For UI work, look at the result (`ui.screenshot`, read the PNG).
- Shell gotcha inherited from upstream: `mv`/`cp` may be aliased interactive — use `/bin/mv -f`.

## Quality gates
`cargo xtask ci` (fmt, clippy -D warnings, tests, layers, wasm, assets, brands) before every
commit; one task id per commit (`M1.2: tile sources dialog`).
