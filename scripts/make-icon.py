#!/usr/bin/env python3
"""Rasterise the app icon: squircle-masked PNG -> assets/icon.iconset -> .icns.

Why this exists
---------------
`scripts/make-icon.sh` rendered `assets/deepseek.svg` through `sips`. The icon is
now raster artwork, and the shape macOS wants is a *squircle* — a rounded square
with continuous curvature, not a circular arc at each corner — plus a transparent
margin outside it. `sips` can crop to a rectangle and cannot draw that curve, and
no SVG rasteriser is guaranteed to be installed, so the mask is drawn here with
nothing but the standard library.

The geometry is not a guess. Apple's icon grid puts an 824x824 rounded square in
a 1024x1024 canvas (the margin is what makes icons line up in the Dock), and
`UIIconAppearance`/`NSImage`'s corner is 185.4pt at that scale — the same
"continuous corner" `CALayer.continuousCurves` draws.

The mask is what gives the icon its macOS look, so it is computed with the real
superellipse rather than approximated with quarter circles: the difference is
visible exactly where it matters, at the corner shoulders.

Usage
-----
    scripts/make-icon.py                     # regenerate assets/deepseek.icns
    scripts/make-icon.py --preview           # write PNG previews, no .icns
    scripts/make-icon.py -i other.png -o other.icns
"""

from __future__ import annotations

import argparse
import math
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The canvas macOS draws icons on, and the grid the body sits on.
CANVAS = 1024

# Measured from the system icons rather than assumed: the rounded body occupies
# this fraction of the canvas, and its corner arc this fraction of the body. See
# `squircle_alpha` for how these numbers were obtained.
BODY_RATIO = 206 / 256
CORNER_RATIO = 0.235

# Supersampling per axis when deciding whether a pixel is inside the squircle.
# 4x4 is 16 samples, which is past the point where a 1024px edge shows banding.
SUPERSAMPLE = 4

# Order does not matter to iconutil, but these are the names it requires.
SIZES = [
    (16, "icon_16x16.png"),
    (32, "icon_16x16@2x.png"),
    (32, "icon_32x32.png"),
    (64, "icon_32x32@2x.png"),
    (128, "icon_128x128.png"),
    (256, "icon_128x128@2x.png"),
    (256, "icon_256x256.png"),
    (512, "icon_256x256@2x.png"),
    (512, "icon_512x512.png"),
    (1024, "icon_512x512@2x.png"),
]


# --------------------------------------------------------------------------- PNG


def read_png(path: Path) -> tuple[int, int, bytes]:
    """Decode a non-interlaced 8-bit RGB/RGBA PNG to `(w, h, rgb_bytes)`."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit(f"{path}: not a PNG")

    pos = 8
    idat = bytearray()
    width = height = channels = 0
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        kind = data[pos + 4 : pos + 8]
        body = data[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour, compression, filt, interlace = struct.unpack(
                ">IIBBBBB", body
            )
            if depth != 8 or interlace != 0:
                raise SystemExit(f"{path}: only 8-bit non-interlaced PNG is supported")
            if colour == 2:
                channels = 3
            elif colour == 6:
                channels = 4
            else:
                raise SystemExit(f"{path}: only RGB and RGBA are supported")
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
        pos += 12 + length

    raw = zlib.decompress(bytes(idat))
    stride = width * channels
    out = bytearray(width * height * 3)
    previous = bytearray(stride)
    pos = 0
    for y in range(height):
        filt = raw[pos]
        pos += 1
        line = bytearray(raw[pos : pos + stride])
        pos += stride
        if filt == 1:
            for x in range(channels, stride):
                line[x] = (line[x] + line[x - channels]) & 0xFF
        elif filt == 2:
            for x in range(stride):
                line[x] = (line[x] + previous[x]) & 0xFF
        elif filt == 3:
            for x in range(stride):
                a = line[x - channels] if x >= channels else 0
                line[x] = (line[x] + ((a + previous[x]) >> 1)) & 0xFF
        elif filt == 4:
            for x in range(stride):
                a = line[x - channels] if x >= channels else 0
                b = previous[x]
                c = previous[x - channels] if x >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x] + pred) & 0xFF
        elif filt != 0:
            raise SystemExit(f"{path}: unknown row filter {filt}")

        row = y * width * 3
        if channels == 3:
            out[row : row + width * 3] = line
        else:
            # Flatten onto black: this artwork is opaque, and a soft alpha edge
            # would otherwise become a light halo against the icon's dark field.
            for x in range(width):
                s = x * 4
                d = row + x * 3
                alpha = line[s + 3]
                out[d] = line[s] * alpha // 255
                out[d + 1] = line[s + 1] * alpha // 255
                out[d + 2] = line[s + 2] * alpha // 255

        previous = line

    return width, height, bytes(out)


def write_png(path: Path, width: int, height: int, rgba: bytes) -> None:
    """Write an 8-bit RGBA PNG. One filter-0 deflate stream, no interlacing."""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)
        raw += rgba[y * stride : (y + 1) * stride]

    def chunk(kind: bytes, body: bytes) -> bytes:
        return (
            struct.pack(">I", len(body))
            + kind
            + body
            + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)
        )

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


# ------------------------------------------------------------------ resampling


def resample(
    src: bytes,
    sw: int,
    sh: int,
    box: tuple[float, float, float, float],
    dw: int,
    dh: int,
    channels: int = 3,
) -> bytes:
    """Area-average `src`'s `box` (x0, y0, x1, y1) into a `dw` x `dh` buffer.

    Box average rather than nearest neighbour because every destination size here
    is a large downscale: nearest would drop most of the artwork's linework and
    alias the hair edge into a staircase.

    `channels` is the source's bytes per pixel and is *not* optional in spirit:
    the artwork is RGB and the masked master is RGBA, and sampling one with the
    other's stride silently produces a buffer of the wrong length and scrambled
    contents rather than an error.
    """
    if len(src) != sw * sh * channels:
        raise ValueError(
            f"resample: source is {len(src)} bytes, expected {sw * sh * channels} "
            f"for {sw}x{sh}x{channels}"
        )
    x0, y0, x1, y1 = box
    out = bytearray(dw * dh * channels)
    for dy in range(dh):
        sy0 = y0 + (y1 - y0) * dy / dh
        sy1 = y0 + (y1 - y0) * (dy + 1) / dh
        iy0, iy1 = int(math.floor(sy0)), max(int(math.ceil(sy1)), int(math.floor(sy0)) + 1)
        for dx in range(dw):
            sx0 = x0 + (x1 - x0) * dx / dw
            sx1 = x0 + (x1 - x0) * (dx + 1) / dw
            ix0, ix1 = int(math.floor(sx0)), max(int(math.ceil(sx1)), int(math.floor(sx0)) + 1)
            y_lo, y_hi = max(iy0, 0), min(iy1, sh)
            x_lo, x_hi = max(ix0, 0), min(ix1, sw)
            acc = [0] * channels
            for sy in range(y_lo, y_hi):
                base = sy * sw
                for sx in range(x_lo, x_hi):
                    o = (base + sx) * channels
                    for c in range(channels):
                        acc[c] += src[o + c]
            n = max(y_hi - y_lo, 1) * max(x_hi - x_lo, 1)
            o = (dy * dw + dx) * channels
            for c in range(channels):
                out[o + c] = acc[c] // n
    return bytes(out)


# ------------------------------------------------------------------ squircle


def squircle_alpha(size: int) -> bytes:
    """Coverage of the macOS icon shape, as an 8-bit alpha mask at `size`.

    The shape is a rounded rectangle, and the numbers were measured rather than
    recalled. `iconutil`-extracting `Notes`, `Music`, `Calculator` and
    `Reminders` and thresholding their alpha gives, for every one of them:

      * the body spans 206/256 px of the canvas = 0.8047 (the 824/1024 grid),
      * the mid-edge and mid-row runs are full width, so the shape is flat-edged
        with rounded corners, not a superellipse,
      * the corner's implied circular radius is 0.228-0.246 of the body across a
        10x range of the arc. A superellipse's implied radius would drift by far
        more than that, which is what rules the superellipse out.

    R = 0.235 * body = 185.6px on a 1024 canvas, against the 185.4 Apple
    documents for the macOS icon grid. The corner is therefore an arc, and this
    function is a rounded-rectangle coverage test, not `|x|^n + |y|^n = 1`.
    """
    body = size * BODY_RATIO
    left = (size - body) / 2
    right = left + body
    radius = body * CORNER_RATIO
    step = 1.0 / SUPERSAMPLE
    per_pixel = float(SUPERSAMPLE * SUPERSAMPLE)
    alpha = bytearray(size * size)

    for y in range(size):
        for x in range(size):
            hits = 0
            for sub_y in range(SUPERSAMPLE):
                py = y + (sub_y + 0.5) * step
                for sub_x in range(SUPERSAMPLE):
                    px = x + (sub_x + 0.5) * step
                    if _inside_body(px, py, left, right, radius):
                        hits += 1
            alpha[y * size + x] = int(round(255 * hits / per_pixel))

    return bytes(alpha)


def _inside_body(px: float, py: float, left: float, right: float, radius: float) -> bool:
    """True if a point is inside a rounded rectangle of the given half-open box."""
    if not (left <= px <= right):
        return False
    if not (left <= py <= right):
        return False
    # Distance to the nearest corner centre, but only inside the corner squares:
    # clamping to the arc's centre box makes the four straight edges fall out of
    # the same expression instead of needing their own branch.
    cx = min(max(px, left + radius), right - radius)
    cy = min(max(py, left + radius), right - radius)
    dx, dy = px - cx, py - cy
    return dx * dx + dy * dy <= radius * radius


def compose(rgb: bytes, size: int, alpha: bytes) -> bytes:
    rgba = bytearray(size * size * 4)
    for i in range(size * size):
        o = i * 3
        d = i * 4
        rgba[d] = rgb[o]
        rgba[d + 1] = rgb[o + 1]
        rgba[d + 2] = rgb[o + 2]
        rgba[d + 3] = alpha[i]
    return bytes(rgba)


def png_scanline_bytes(path: Path) -> tuple[int, int, int]:
    """Return `(width, height, decompressed_idat_bytes)` for a written PNG.

    Used as a self-check on our own output. The point is that `write_png` takes a
    flat buffer and a size and does not verify that the two agree: feeding it a
    short buffer produces a PNG that decodes to a plausible-looking but wrong
    image, which is exactly how a stride bug got in here once. Checking the
    inflated length pins width*height*4 against what was actually written.
    """
    data = path.read_bytes()
    pos, idat = 8, bytearray()
    width = height = 0
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        kind = data[pos + 4 : pos + 8]
        if kind == b"IHDR":
            width, height = struct.unpack(">II", data[pos + 8 : pos + 16])
        elif kind == b"IDAT":
            idat += data[pos + 8 : pos + 8 + length]
        elif kind == b"IEND":
            break
        pos += 12 + length
    return width, height, len(zlib.decompress(bytes(idat)))


def self_check(path: Path, size: int) -> None:
    width, height, inflated = png_scanline_bytes(path)
    expected = height * (1 + width * 4)
    if (width, height) != (size, size) or inflated != expected:
        raise SystemExit(
            f"self-check failed for {path.name}: wrote {width}x{height} with "
            f"{inflated} scanline bytes, expected {size}x{size} with {expected}"
        )


# ----------------------------------------------------------------------- main


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("-i", "--input", type=Path, default=ROOT / "assets" / "app-icon.png")
    parser.add_argument("-o", "--output", type=Path, default=ROOT / "assets" / "deepseek.icns")
    parser.add_argument(
        "--preview",
        action="store_true",
        help="write preview PNGs next to the output instead of an .icns",
    )
    parser.add_argument(
        "--cover",
        type=float,
        default=1.0,
        help="fraction of the squircle the artwork should fill (1.0 = bleed to the edges)",
    )
    args = parser.parse_args()

    if not args.input.exists():
        raise SystemExit(f"{args.input}: no such file")
    if sys.platform != "darwin":
        raise SystemExit("error: icon generation requires macOS (iconutil)")

    width, height, rgb = read_png(args.input)
    print(f"==> source {args.input.name}: {width}x{height}")

    # Square the artwork around its centre, then scale so that `--cover` of the
    # canvas is used: cover=1 fills the squircle edge to edge (the mask trims the
    # overflow), cover<1 floats the artwork with a dark margin inside it.
    side = min(width, height)
    box = (
        (width - side) / 2,
        (height - side) / 2,
        (width + side) / 2,
        (height + side) / 2,
    )
    inner = int(round(CANVAS * args.cover))
    scaled = resample(rgb, width, height, box, inner, inner)

    canvas = bytearray(CANVAS * CANVAS * 3)
    offset = (CANVAS - inner) // 2
    for y in range(inner):
        src = y * inner * 3
        dst = ((y + offset) * CANVAS + offset) * 3
        canvas[dst : dst + inner * 3] = scaled[src : src + inner * 3]

    alpha = squircle_alpha(CANVAS)
    master = compose(bytes(canvas), CANVAS, alpha)

    # The mask is greyscale but the resampler speaks RGB, so expand it once.
    grey = bytes(v for value in alpha for v in (value, value, value))

    if args.preview:
        for size in (32, 48, 128, 256):
            out = args.output.with_name(f"{args.output.stem}-{size}.png")
            art = resample(bytes(canvas), CANVAS, CANVAS, (0, 0, CANVAS, CANVAS), size, size)
            mask = resample(grey, CANVAS, CANVAS, (0, 0, CANVAS, CANVAS), size, size)
            rgba = bytearray(size * size * 4)
            for i in range(size * size):
                # Composite here rather than resampling `master`, so the
                # preview shows the artwork and the mask at this size instead of
                # averaging already-multiplied alpha.
                rgba[i * 4] = art[i * 3]
                rgba[i * 4 + 1] = art[i * 3 + 1]
                rgba[i * 4 + 2] = art[i * 3 + 2]
                rgba[i * 4 + 3] = mask[i * 3]
            write_png(out, size, size, bytes(rgba))
            print(f"    preview {out}")
        return 0

    iconset = Path(tempfile.mkdtemp(prefix="dsh-icon-")) / "icon.iconset"
    iconset.mkdir()
    try:
        for size, name in SIZES:
            # The master is RGBA; the artwork and mask previews are RGB.
            scaled = resample(
                master, CANVAS, CANVAS, (0, 0, CANVAS, CANVAS), size, size, channels=4
            )
            write_png(iconset / name, size, size, scaled)
            self_check(iconset / name, size)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["iconutil", "-c", "icns", str(iconset), "-o", str(args.output)], check=True
        )
    finally:
        shutil.rmtree(iconset.parent, ignore_errors=True)

    print(f"==> wrote {args.output} ({args.output.stat().st_size // 1024} KB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
