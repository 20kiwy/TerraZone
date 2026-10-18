#!/usr/bin/env python3
"""Generate every TerraZone app icon from the mark defined here and in terrazon.svg.

The mark (ArtCraft family style): a deep-forest rounded tile with concentric emerald
orbit rings and low-opacity terrain contour lines, a white "T" monogram whose stem
terminates in a map-pin point, and the amber zone-dot as the pin head (the "you are
here" dot).

The tile is hand-written SVG (TILE_BODY below); every raster is rendered from that
same SVG with cairosvg, so the vector and the PNGs match exactly.

    python3 packaging/icons.py

Writes into assets/app-icon/ and mirrors the 1024 px mark to
/home/z/my-project/public/terrazon/logo.png for the delivery page (when it exists).
"""
from __future__ import annotations

import io
import struct
from pathlib import Path

import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
DIR = ROOT / "assets" / "app-icon"
PAGE = Path("/home/z/my-project/public/terrazon")
ID = "app.terrazon.terrazon"

# --- the mark, hand-written SVG (512 tile) ------------------------------------
TILE = 512
RX = 112  # tile corner radius, same as the previous icon's grid

TILE_BODY = """<defs><clipPath id="tz"><rect width="512" height="512" rx="112"/></clipPath><linearGradient id="tzg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="rgb(12,59,46)"/><stop offset="1" stop-color="rgb(5,32,24)"/></linearGradient></defs><g clip-path="url(#tz)"><rect width="512" height="512" fill="url(#tzg)"/><ellipse cx="258" cy="240" rx="212" ry="196" fill="none" stroke="rgb(52,211,153)" stroke-opacity="0.141" stroke-width="7"/><ellipse cx="252" cy="246" rx="176" ry="160" fill="none" stroke="rgb(110,231,183)" stroke-opacity="0.180" stroke-width="7"/><ellipse cx="262" cy="238" rx="140" ry="124" fill="none" stroke="rgb(52,211,153)" stroke-opacity="0.235" stroke-width="8"/><ellipse cx="254" cy="244" rx="104" ry="90" fill="none" stroke="rgb(167,243,208)" stroke-opacity="0.259" stroke-width="8"/><ellipse cx="258" cy="240" rx="68" ry="58" fill="none" stroke="rgb(52,211,153)" stroke-opacity="0.314" stroke-width="9"/><path d="M-20 396 C80 372 168 420 272 400 C356 384 428 376 532 398" fill="none" stroke="rgb(110,231,183)" stroke-opacity="0.140" stroke-width="7" stroke-linecap="round"/><path d="M-20 442 C96 418 200 466 312 442 C396 424 452 420 532 444" fill="none" stroke="rgb(167,243,208)" stroke-opacity="0.110" stroke-width="6" stroke-linecap="round"/><path d="M-20 488 C120 466 244 506 372 482 C448 468 496 464 532 476" fill="none" stroke="rgb(52,211,153)" stroke-opacity="0.150" stroke-width="7" stroke-linecap="round"/><g fill="rgb(244,251,247)"><rect x="146" y="140" width="220" height="64" rx="32"/><rect x="224" y="140" width="64" height="196" rx="32"/><path d="M230 290 L282 290 L256 428 Z" stroke="rgb(244,251,247)" stroke-width="12" stroke-linejoin="round"/></g><circle cx="256" cy="352" r="32" fill="rgb(245,165,36)"/></g>"""

TILE_SVG = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">'
    + TILE_BODY
    + "</svg>\n"
)

# Master vector: the same square mark on a larger canvas with breathing room.
MASTER_CANVAS = 720
MASTER_MARGIN = (MASTER_CANVAS - TILE) // 2  # 104
MASTER_SVG = (
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {MASTER_CANVAS} {MASTER_CANVAS}">'
    f'<g transform="translate({MASTER_MARGIN},{MASTER_MARGIN})">'
    + TILE_BODY
    + "</g></svg>\n"
)

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
ICNS_ENTRIES = [  # (Apple code, px) — PNG-based icns, same set as before
    (b"icp4", 16), (b"icp5", 32), (b"ic07", 128), (b"ic08", 256),
    (b"ic09", 512), (b"ic10", 1024),
]


def render_svg(svg: str, size: int) -> Image.Image:
    """Render an SVG document to a square RGBA Pillow image."""
    png = cairosvg.svg2png(bytestring=svg.encode("utf-8"),
                           output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert("RGBA")


def render_tile(size: int) -> Image.Image:
    """The full-bleed rounded tile at `size` px (RGBA, corners transparent)."""
    return render_svg(TILE_SVG, size)


def render_macos(size: int = 512) -> Image.Image:
    """The macOS grid: the tile at 824/1024 of the canvas with a transparent margin."""
    tile_frac = 824 / 1024
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    t = render_tile(round(size * tile_frac))
    off = (size - t.width) // 2
    canvas.paste(t, (off, off), t)
    return canvas


def write_svgs() -> None:
    (DIR / "terrazon-small.svg").write_text(TILE_SVG)
    (DIR / "terrazon.svg").write_text(MASTER_SVG)
    scal = DIR / "hicolor" / "scalable" / "apps"
    scal.mkdir(parents=True, exist_ok=True)
    (scal / f"{ID}.svg").write_text(TILE_SVG)


def write_icns(path: Path) -> None:
    """Apple .icns: PNG entries, written by hand (no iconutil in the sandbox)."""
    blob = b"icns" + struct.pack(">I", 0)  # size patched below
    for code, size in ICNS_ENTRIES:
        buf = io.BytesIO()
        render_tile(size).save(buf, "PNG")
        data = buf.getvalue()
        blob += code + struct.pack(">I", len(data) + 8) + data
    blob = blob[:4] + struct.pack(">I", len(blob)) + blob[8:]
    path.write_bytes(blob)


def write_ico(path: Path) -> None:
    base = render_tile(256)
    base.save(path, format="ICO", sizes=[(s, s) for s in ICO_SIZES])


def main() -> None:
    print("writing svg...")
    write_svgs()

    print("rendering rasters (cairosvg)...")
    render_tile(1024).save(DIR / "terrazon-1024.png")
    render_macos(512).save(DIR / "terrazon-macos-512.png")
    for s in (16, 24, 32, 48, 64, 128, 256, 512):
        out = DIR / "hicolor" / f"{s}x{s}" / "apps"
        out.mkdir(parents=True, exist_ok=True)
        render_tile(s).save(out / f"{ID}.png")

    print("writing ico/icns...")
    write_ico(DIR / "terrazon.ico")
    write_icns(DIR / "terrazon.icns")

    if PAGE.is_dir():
        render_tile(1024).save(PAGE / "logo.png")
        print(f"page logo -> {PAGE / 'logo.png'}")
    print("done.")


if __name__ == "__main__":
    main()
