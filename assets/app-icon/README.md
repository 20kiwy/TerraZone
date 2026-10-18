# TerraZone app icon

**Mark:** the TerraZone "T" pin — the monogram over orbit rings and terrain contour lines.
The white T's stem terminates in a map-pin point and the amber zone-dot (the "you are here"
dot) is the pin head, so the T doubles as a map pin dropped onto the terrain.

**Style:** flat geometric on a full-bleed rounded field; the orbit rings read as the globe,
the contour lines as terrain, and the white T-pin stays legible from 16 px up.

**Palette:**

| Colour | Hex | Used for |
|---|---|---|
| Deep forest | `#0c3b2e` → `#052018` | the full-bleed field (vertical gradient) |
| Rings & contours | `#34d399`, `#6ee7b7`, `#a7f3d0` | orbit ellipses + terrain contour lines |
| Paper | `#f4fbf7` | the T monogram / map pin |
| Zone dot | `#f5a524` | the amber pin head |

**Tile:** `viewBox="0 0 512 512"`, a rounded square with `rx=112`, clipped. `terrazon.svg` presents
the same square mark on a larger 720×720 canvas (104 px margin) for docs and store listings;
`terrazon-small.svg` and the scalable hicolor entry are the bare 512 tile. The macOS files add
Apple's transparent margin (tile = 824/1024 of the canvas); Windows and Linux use the tile edge
to edge.

**Provenance:** original artwork for TerraZone by TerraZone contributors. The tile is hand-written
SVG; `packaging/icons.py` renders every raster from that same SVG (with cairosvg), so the vector
and the PNGs match exactly. Licence: see `LICENSE.txt`.
## Files

| File | What | Used by |
|---|---|---|
| `terrazon.svg` | master vector — the 512 tile on a 720 canvas | docs, store listings |
| `terrazon-small.svg` | compact 512 tile (hand-written, about 1.8 KB) | `hicolor/scalable` |
| `terrazon-1024.png` | full tile, 1024 px | docs, store listings |
| `terrazon-macos-512.png` | tile with Apple margin, 512 px | runtime Dock icon (`apps/terrazon/src/main.rs`) |
| `terrazon.icns` | macOS icon set, 16 to 1024 px | `cargo xtask bundle` (`CFBundleIconFile`) |
| `terrazon.ico` | Windows icon, 16 to 256 px | `apps/terrazon/build.rs` (embedded in the `.exe`) |
| `hicolor/<size>/apps/app.terrazon.terrazon.png` | Linux theme icons, 16 to 512 px; the 256 one is also the runtime icon on Windows and Linux, the 128 one the brand mark in the app bar and About box (`crates/ui-egui/src/brand.rs`) | `packaging/linux/app.terrazon.terrazon.desktop` |
| `hicolor/scalable/apps/app.terrazon.terrazon.svg` | Linux scalable icon | as above |

## How it reaches each OS

- **macOS:** the app sets the Dock / app-switcher icon at runtime (`ViewportBuilder::with_icon`); the
  `.app` from `cargo xtask bundle` carries `terrazon.icns`.
- **Windows:** `build.rs` embeds `terrazon.ico` and VERSIONINFO with `winresource` (taskbar, Start
  menu, Explorer, Alt-Tab); the runtime icon covers the title bar.
- **Linux:** install `packaging/linux/app.terrazon.terrazon.desktop` to `share/applications/` and
  `hicolor/` to `share/icons/`. The app sets its Wayland app ID / X11 class to
  `app.terrazon.terrazon`, so the dock matches the window to the launcher.

## Regenerate

The mark lives as the `TILE_BODY` SVG in `packaging/icons.py` (it is written out verbatim as
`terrazon-small.svg`). Edit it there, then run

    python3 packaging/icons.py

It writes the SVGs and renders every PNG, the `.ico` and the `.icns` from that same SVG
(needs `cairosvg` + `Pillow`). `packaging/icons.sh` does the same on machines that have `resvg`.
