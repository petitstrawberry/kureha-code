#!/usr/bin/env python3
"""Lay out two matching PNG screenshots as a diagonal, self-contained SVG.

The screenshots are embedded verbatim; no generated or repainted UI is used.
Usage: python3 scripts/theme-screenshot.py light.png dark.png output.svg
Then render the SVG to PNG for Markdown viewers without SVG support:
    resvg output.svg docs/screenshots/kureha-light-dark.png
"""
import argparse
import base64
from pathlib import Path
import struct


def read_png(path):
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise ValueError(f"Not a PNG: {path}")
    return struct.unpack(">II", data[16:24]), base64.b64encode(data).decode("ascii")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("light", type=Path)
    parser.add_argument("dark", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    light_size, light = read_png(args.light)
    dark_size, dark = read_png(args.dark)
    if light_size != dark_size:
        parser.error("Both screenshots must have matching dimensions")
    width, height = light_size
    top, bottom = width * 0.70, width * 0.30
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(f'''<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">
  <title id="title">Kureha Code — Kureha Light and Kureha Dark</title>
  <desc id="desc">Matching screenshots of split code editors and a Markdown preview. Kureha Light is on the left and Kureha Dark is on the right, divided diagonally.</desc>
  <defs><clipPath id="dark-half"><polygon points="{top},0 {width},0 {width},{height} {bottom},{height}"/></clipPath></defs>
  <image width="{width}" height="{height}" xlink:href="data:image/png;base64,{light}"/>
  <image width="{width}" height="{height}" clip-path="url(#dark-half)" xlink:href="data:image/png;base64,{dark}"/>
</svg>
''', encoding="utf-8")


if __name__ == "__main__":
    main()
