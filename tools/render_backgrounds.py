"""Render the Home page background patterns.

Each pattern is a white image whose alpha carries the drawing. The app
colours it with the accent colour (Slint `colorize`), so one file serves
every accent.

    matrix.png   columns of falling characters
    topo.png     contour lines of a height field, drawn in ASCII
    map.png      the world's land as a dot grid
    dots.png     a dot grid that fades out from the centre
    thumb-*.png  the Settings previews: what Home shows in a portrait
                 window, scaled down, with the strokes thickened to stay visible

Run from the repository root:

    python tools/render_backgrounds.py path/to/ne_110m_land.geojson

The GeoJSON is Natural Earth's 110m land layer (public domain):
https://github.com/nvkelso/natural-earth-vector/blob/master/geojson/ne_110m_land.geojson
Without it, map.png is left as it is.
"""

import json
import math
import random
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates" / "starpod-ui" / "ui" / "assets" / "backgrounds"
FONTS = Path("C:/Windows/Fonts")
SIZE = 1400


def font(names, size):
    for name in names:
        for folder in (FONTS, Path("/usr/share/fonts/truetype/dejavu")):
            path = folder / name
            if path.exists():
                return ImageFont.truetype(str(path), size)
    return ImageFont.load_default()


def save(alpha, name):
    """Write a white image with `alpha` (a 0..255 L image) as its alpha."""
    image = Image.new("LA", alpha.size, 255)
    image.putalpha(alpha)
    OUT.mkdir(parents=True, exist_ok=True)
    image.save(OUT / name, optimize=True)
    print(name, alpha.size, (OUT / name).stat().st_size, "bytes")


def matrix():
    rng = random.Random(7)
    glyph_font = font(["YuGothM.ttc", "msgothic.ttc", "DejaVuSansMono.ttf"], 15)
    cell_w, cell_h = 16, 20
    cols, rows = SIZE // cell_w, SIZE // cell_h
    chars = [chr(c) for c in range(0xFF66, 0xFF9E)] + list("0123456789Z:.=*+-<>")
    alpha = Image.new("L", (SIZE, SIZE), 0)
    draw = ImageDraw.Draw(alpha)
    for col in range(cols):
        # Every column has one or two streaks; the head is the brightest
        # character and the trail fades above it.
        streaks = [(rng.randrange(rows), rng.randrange(8, 26)) for _ in range(rng.choice((1, 1, 2)))]
        for row in range(rows):
            level = 0.05 if rng.random() < 0.35 else 0.0
            for head, length in streaks:
                behind = head - row
                if 0 <= behind < length:
                    level = max(level, 1.0 if behind == 0 else 0.75 * (1 - behind / length) + 0.08)
            if level <= 0:
                continue
            draw.text(
                (col * cell_w + 1, row * cell_h),
                rng.choice(chars),
                font=glyph_font,
                fill=int(255 * level),
            )
    save(alpha, "matrix.png")


def smooth_noise(shape, scale, rng):
    """Value noise: a coarse random grid, upsampled with cosine smoothing."""
    gh, gw = shape[0] // scale + 2, shape[1] // scale + 2
    grid = rng.random((gh, gw))
    ys = np.arange(shape[0]) / scale
    xs = np.arange(shape[1]) / scale
    y0, x0 = ys.astype(int), xs.astype(int)
    ty = (1 - np.cos((ys - y0) * math.pi)) / 2
    tx = (1 - np.cos((xs - x0) * math.pi)) / 2
    a = grid[np.ix_(y0, x0)]
    b = grid[np.ix_(y0, x0 + 1)]
    c = grid[np.ix_(y0 + 1, x0)]
    d = grid[np.ix_(y0 + 1, x0 + 1)]
    top = a + (b - a) * tx[None, :]
    bottom = c + (d - c) * tx[None, :]
    return top + (bottom - top) * ty[:, None]


def topo():
    rng = np.random.default_rng(11)
    cell_w, cell_h = 8, 13
    cols, rows = SIZE // cell_w, SIZE // cell_h
    # The height field is computed per pixel, so slopes and angles are true
    # whatever the shape of a character cell.
    # A sum of plane waves in random directions: smooth, with no grid
    # direction, unlike value noise.
    ys, xs = np.mgrid[0:SIZE, 0:SIZE].astype(np.float32)
    height = np.zeros((SIZE, SIZE), dtype=np.float32)
    for _ in range(40):
        wavelength = rng.uniform(260, 1100)
        direction = rng.uniform(0, 2 * math.pi)
        k = 2 * math.pi / wavelength
        phase = rng.uniform(0, 2 * math.pi)
        amplitude = (wavelength / 1100) ** 1.5
        height += amplitude * np.cos(k * (xs * math.cos(direction) + ys * math.sin(direction)) + phase)
    height = (height - height.min()) / (height.max() - height.min())
    levels = 11
    band = height * levels
    gy, gx = np.gradient(band)
    slope = np.hypot(gx, gy) + 1e-9
    glyph_font = font(["consola.ttf", "DejaVuSansMono.ttf"], 14)
    alpha = Image.new("L", (SIZE, SIZE), 0)
    draw = ImageDraw.Draw(alpha)
    for row in range(rows):
        for col in range(cols):
            y, x = row * cell_h + cell_h // 2, col * cell_w + cell_w // 2
            value = band[y, x]
            # Distance to the nearest contour, in pixels. A cell gets a
            # character when a line passes through it.
            distance = abs(value - round(value)) / slope[y, x]
            reach = abs(gx[y, x]) / slope[y, x] * cell_w / 2 + abs(gy[y, x]) / slope[y, x] * cell_h / 2
            if distance > reach:
                continue
            # The character runs along the contour, at right angles to the slope.
            angle = math.degrees(math.atan2(gy[y, x], gx[y, x])) % 180
            if angle < 25 or angle >= 155:
                glyph = "|"
            elif angle < 65:
                glyph = "\\"
            elif angle < 115:
                glyph = "-"
            else:
                glyph = "/"
            # Every fourth contour is an index line, drawn brighter.
            index = round(value) % 4 == 0
            strength = (1.0 if index else 0.55) * (1 - 0.5 * distance / reach)
            draw.text((col * cell_w, row * cell_h - 2), glyph, font=glyph_font, fill=int(255 * strength))
    save(alpha, "topo.png")


def world_map(geojson):
    data = json.loads(Path(geojson).read_text(encoding="utf-8"))
    # Equirectangular, longitude -180..180, latitude 84..-58 (no Antarctica).
    width, north, south = 3200, 84.0, -58.0
    height = round(width * (north - south) / 360)
    mask = Image.new("L", (width, height), 0)
    draw = ImageDraw.Draw(mask)

    def project(lon, lat):
        return ((lon + 180) / 360 * width, (north - lat) / (north - south) * height)

    for feature in data["features"]:
        geometry = feature["geometry"]
        polygons = geometry["coordinates"] if geometry["type"] == "MultiPolygon" else [geometry["coordinates"]]
        for polygon in polygons:
            outer, *holes = polygon
            draw.polygon([project(*point) for point in outer], fill=255)
            for hole in holes:
                draw.polygon([project(*point) for point in hole], fill=0)
    land = np.array(mask)
    step, radius = 16, 3.6
    alpha = Image.new("L", (width, height), 0)
    dots = ImageDraw.Draw(alpha)
    for y in range(step // 2, height, step):
        for x in range(step // 2, width, step):
            if land[y, x] > 127:
                dots.ellipse((x - radius, y - radius, x + radius, y + radius), fill=230)
    save(alpha, "map.png")


def dot_grid():
    step, radius = 22, 1.6
    alpha = Image.new("L", (SIZE, SIZE), 0)
    draw = ImageDraw.Draw(alpha)
    centre = SIZE / 2
    for y in range(step // 2, SIZE, step):
        for x in range(step // 2, SIZE, step):
            # Full strength in the middle, gone at the edges.
            distance = math.hypot(x - centre, y - centre) / centre
            level = max(0.0, 1 - distance) ** 1.3
            if level < 0.03:
                continue
            draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=int(255 * level))
    save(alpha, "dots.png")


def thumbnails():
    """A preview per pattern, as Home shows it, for the Settings tiles.

    The tiles are as small as 60x80 and as wide as 120x80, so the previews
    are 240x200 and the tile crops them. Scaling a one-pixel stroke down
    leaves a faint grey, so the previews get their alpha raised. The map's
    dots are drawn again at the preview's size: shrinking them makes moire.
    """
    from PIL import ImageFilter

    size = (240, 200)
    for name in ("matrix", "topo", "dots"):
        image = Image.open(OUT / f"{name}.png").getchannel("A")
        # A 720x600 view of the middle, shrunk by three.
        crop_w, crop_h = size[0] * 3, size[1] * 3
        left, top = (image.width - crop_w) // 2, (image.height - crop_h) // 2
        view = image.crop((left, top, left + crop_w, top + crop_h)).filter(ImageFilter.MaxFilter(3))
        small = view.resize(size, Image.LANCZOS).point(lambda v: min(255, int(v * 1.9)))
        save(small, f"thumb-{name}.png")

    # The map: the land as Home shows it in a portrait window (the map's
    # height covering about 840 pixels), then a dot grid sized for the tile.
    image = Image.open(OUT / "map.png").getchannel("A")
    scale = 840 / image.height
    crop_w, crop_h = round(720 / scale), round(600 / scale)
    left, top = (image.width - crop_w) // 2, (image.height - crop_h) // 2
    land = image.crop((left, top, left + crop_w, top + crop_h))
    land = land.filter(ImageFilter.BoxBlur(12)).resize(size, Image.BILINEAR)
    alpha = Image.new("L", size, 0)
    draw = ImageDraw.Draw(alpha)
    step, radius = 10, 3.2
    for y in range(step // 2, size[1], step):
        for x in range(step // 2, size[0], step):
            if land.getpixel((x, y)) > 40:
                draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=235)
    save(alpha, "thumb-map.png")


if __name__ == "__main__":
    matrix()
    topo()
    dot_grid()
    if len(sys.argv) > 1:
        world_map(sys.argv[1])
    thumbnails()
