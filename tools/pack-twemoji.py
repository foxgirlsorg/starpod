#!/usr/bin/env python3
"""Rebuild crates/starpod-app/assets/twemoji.bin from the @twemoji/svg npm tarball.

Usage:
    python3 tools/pack-twemoji.py [TARBALL] [OUTPUT]

TARBALL defaults to a fresh download of
https://registry.npmjs.org/@twemoji/svg/-/svg-15.0.0.tgz (the version pinned
below). OUTPUT defaults to crates/starpod-app/assets/twemoji.bin.

Blob format (the gzip stream decompresses to this; read by
crates/starpod-app/src/ui/emoji.rs):

    u32 LE          entry count N
    N x entry:      u8 name length, name bytes (ASCII, e.g. "1f1e9-1f1ea.svg"),
                    u32 LE data length
    concatenated    the SVG bytes of every entry, in the same order

Only the stdlib is used. The gzip header carries mtime 0 so the output is
reproducible for a given tarball.
"""

import gzip
import io
import os
import re
import struct
import sys
import tarfile
import urllib.request

TWEMOJI_TARBALL_URL = "https://registry.npmjs.org/@twemoji/svg/-/svg-15.0.0.tgz"
DEFAULT_OUTPUT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "crates",
    "starpod-app",
    "assets",
    "twemoji.bin",
)
NAME_RE = re.compile(r"^[0-9a-f]+(-[0-9a-f]+)*\.svg$")


def load_tarball(path_or_none):
    if path_or_none:
        with open(path_or_none, "rb") as f:
            return f.read()
    with urllib.request.urlopen(TWEMOJI_TARBALL_URL) as resp:
        return resp.read()


def collect_svgs(tarball_bytes):
    entries = []
    with tarfile.open(fileobj=io.BytesIO(tarball_bytes), mode="r:gz") as tar:
        for member in tar.getmembers():
            if not member.isfile():
                continue
            base = os.path.basename(member.name)
            if not NAME_RE.match(base):
                continue
            # Only the top-level `package/` folder; the npm tarball has no
            # other SVG-named files, but guard against nested copies anyway.
            if member.name.count("/") != 1:
                continue
            entries.append((base, tar.extractfile(member).read()))
    entries.sort(key=lambda e: e[0])
    return entries


def pack(entries):
    out = io.BytesIO()
    out.write(struct.pack("<I", len(entries)))
    for name, data in entries:
        raw = name.encode("ascii")
        if len(raw) > 255:
            raise ValueError(f"name too long: {name}")
        out.write(struct.pack("<B", len(raw)))
        out.write(raw)
        out.write(struct.pack("<I", len(data)))
    for _, data in entries:
        out.write(data)
    return out.getvalue()


def main(argv):
    tarball = argv[1] if len(argv) > 1 else None
    output = argv[2] if len(argv) > 2 else DEFAULT_OUTPUT
    entries = collect_svgs(load_tarball(tarball))
    if not entries:
        raise SystemExit("no SVG files found in tarball")
    payload = pack(entries)
    compressed = gzip.compress(payload, compresslevel=9, mtime=0)
    with open(output, "wb") as f:
        f.write(compressed)
    print(f"{len(entries)} SVGs, {len(payload)} bytes raw, {len(compressed)} bytes gzip -> {output}")


if __name__ == "__main__":
    main(sys.argv)
