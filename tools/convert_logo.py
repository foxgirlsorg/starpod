#!/usr/bin/env python3
"""Convert the foxgirls fox-head mark into a Slint `Path` component.

The mark is two SVG paths in a `viewBox="0 0 117.29976 203.22969"`, nested
inside two `translate(...)` transforms:

- the first path is the filled silhouette;
- the second is a stroked outline (`stroke-width: 1.165`) offset from it,
  which gives the mark its double-line quality.

Both paths sit inside *both* transforms, so the offset that applies to them
is the sum of the two translations. This script converts their relative
commands to absolute ones and folds that offset in, leaving a path in the
mark's own coordinate space — which is what Slint's `Path` scales.

Usage: convert_logo.py <logo.svg> <output.slint>
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

PATH_RE = re.compile(r"<path\b(.*?)/>", re.S)
ATTR_RE = re.compile(r'([\w:-]+)\s*=\s*"([^"]*)"')
TRANSFORM_RE = re.compile(r"translate\(\s*([-\d.eE+]+)\s*,\s*([-\d.eE+]+)\s*\)")
NUMBER_RE = re.compile(r"[-+]?\d*\.?\d+(?:[eE][-+]?\d+)?")

# How many coordinates each command consumes.
ARITY = {"M": 2, "L": 2, "C": 6, "Q": 4, "Z": 0}


def attrs(text: str) -> dict[str, str]:
    return {k: v for k, v in ATTR_RE.findall(text)}


def tokenize(data: str) -> list[tuple[str, list[float]]]:
    """Split a path's `d` attribute into `(command, coordinates)` pairs."""
    tokens: list[tuple[str, list[float]]] = []
    pos = 0
    while pos < len(data):
        char = data[pos]
        if char.isalpha():
            tokens.append((char, []))
            pos += 1
            continue
        if char in " ,\n\r\t":
            pos += 1
            continue
        match = NUMBER_RE.match(data, pos)
        if match is None:
            raise ValueError(f"cannot parse path data at {data[pos:pos + 20]!r}")
        value = float(match.group(0))

        # The current command takes a fixed number of coordinates. Once it
        # has them, the next number repeats the command implicitly.
        if tokens:
            letter, coords = tokens[-1]
            arity = ARITY.get(letter.upper())
            if arity is None:
                raise ValueError(f"unsupported path command {letter!r}")
            if len(coords) < arity:
                tokens[-1][1].append(value)
                pos = match.end()
                continue
            # The command is full: repeat it, as a line-to for a moveto.
            repeated = "l" if letter.upper() == "M" else letter
            tokens.append((repeated, [value]))
        else:
            raise ValueError("path data starts with a number")
        pos = match.end()
    return tokens


def to_absolute(tokens: list[tuple[str, list[float]]]) -> list[tuple[str, list[float]]]:
    """Rewrite a relative path into absolute `(command, coordinates)` pairs."""
    out: list[tuple[str, list[float]]] = []
    cursor = (0.0, 0.0)
    start = (0.0, 0.0)

    for letter, coords in tokens:
        upper = letter.upper()
        if upper not in ARITY:
            raise ValueError(f"unsupported path command {letter!r}")
        count = ARITY[upper]

        if upper == "Z":
            out.append(("Z", []))
            cursor = start
            continue

        if len(coords) % count != 0:
            raise ValueError(f"command {letter!r} has {len(coords)} coordinates")

        for index in range(0, len(coords), count):
            chunk = coords[index : index + count]
            relative = letter.islower()

            if upper == "M":
                x = cursor[0] + chunk[0] if relative else chunk[0]
                y = cursor[1] + chunk[1] if relative else chunk[1]
                out.append(("M", [x, y]))
                cursor = (x, y)
                start = (x, y)
                # A repeated `m` is an implicit `l`.
                letter = "l" if relative else "L"
                continue

            if upper == "L":
                x = cursor[0] + chunk[0] if relative else chunk[0]
                y = cursor[1] + chunk[1] if relative else chunk[1]
                out.append(("L", [x, y]))
                cursor = (x, y)
                continue

            if upper == "C":
                if relative:
                    c1 = (cursor[0] + chunk[0], cursor[1] + chunk[1])
                    c2 = (cursor[0] + chunk[2], cursor[1] + chunk[3])
                    end = (cursor[0] + chunk[4], cursor[1] + chunk[5])
                else:
                    c1 = (chunk[0], chunk[1])
                    c2 = (chunk[2], chunk[3])
                    end = (chunk[4], chunk[5])
                out.append(("C", [c1[0], c1[1], c2[0], c2[1], end[0], end[1]]))
                cursor = end
                continue

            raise ValueError(f"unsupported path command {letter!r}")

    return out


def format_commands(
    commands: list[tuple[str, list[float]]], dx: float, dy: float
) -> str:
    """Fold the offset in and render the path as an SVG command string."""
    parts: list[str] = []
    for letter, values in commands:
        if letter == "Z":
            parts.append("Z")
            continue
        moved = [v + (dx if i % 2 == 0 else dy) for i, v in enumerate(values)]
        parts.append(letter + " " + " ".join(f"{v:g}" for v in moved))
    return " ".join(parts)


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2

    source = Path(sys.argv[1]).read_text(encoding="utf-8")
    target = Path(sys.argv[2])

    # Every `translate()` in the file, in document order. The paths sit
    # inside all of them, so the offset that applies is their sum.
    offsets = [tuple(float(v) for v in m) for m in TRANSFORM_RE.findall(source)]
    if not offsets:
        print("no translate() found", file=sys.stderr)
        return 1
    dx = sum(x for x, _ in offsets)
    dy = sum(y for _, y in offsets)

    paths = PATH_RE.findall(source)
    if not paths:
        print("no paths found", file=sys.stderr)
        return 1

    blocks: list[str] = []
    for raw in paths:
        d = attrs(raw).get("d")
        if not d:
            continue
        blocks.append(format_commands(to_absolute(tokenize(d)), dx, dy))

    if not blocks:
        print("no drawable paths found", file=sys.stderr)
        return 1

    # The two paths are drawn with different treatments — the first is
    # filled, the second is a hairline outline. Slint's `Path` has one
    # fill and one stroke for the whole shape, so the mark is split into
    # two components and the app composes them.
    target.write_text(
        """// The foxgirls fox-head mark.
//
// The mark is the app's identity: the sidebar logo and the tray icon. It
// is drawn in the brand red, which is why it lives in its own file rather
// than in the icon set — it is not a Lucide glyph.
//
// Generated by `tools/convert_logo.py`. Do not edit by hand.
//
// The source is `src/lib/Logo.svelte` in foxgirls.org. That file draws the
// mark as two paths: a filled silhouette and a hairline outline offset from
// it, which gives it its double-line quality. The two are separate
// components here because Slint's `Path` carries one fill and one stroke
// for the whole shape, and the mark needs both.

// The filled silhouette.
export component FoxMarkFill inherits Path {
    commands: "%s";
    fill: #de6161;
    stroke: transparent;
}

// The hairline outline, drawn over the silhouette.
export component FoxMarkOutline inherits Path {
    commands: "%s";
    fill: transparent;
    stroke: #de6161;
    stroke-width: 1.165px;
}
"""
        % (blocks[0], blocks[1] if len(blocks) > 1 else ""),
        encoding="utf-8",
    )
    print(f"wrote {len(blocks)} paths to {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
