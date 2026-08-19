#!/usr/bin/env python3
"""Produce the app icon master from the chosen mark: brackets around a node.

The Big Sur shape: a rounded rectangle ("squircle"-ish) with the mark reversed
out in white. Shown in Finder and the DMG; the app itself has no Dock icon.
`tauri icon` derives every other size from the 1024px master this writes.

# The menu bar icons are not generated here

This script used to also emit the two tray template images (a black bracket
mark, idle and active). It no longer does, and reintroducing that would be
wrong: the tray now uses a designer-supplied four-state set — a translucent
Cloud SQL database glyph with a coloured status dot, for `disconnected`,
`connecting`, `connected`, and `error` — which is not derivable from this
bracket mark and is not a template image.

Those assets live in `src-tauri/icons/tray-*.png`, are committed, and are
embedded by `src-tauri/src/tray.rs` with `include_bytes!`. They come from the
designer's delivery at 18px (the size the code embeds — see the note in
`tray.rs` on why the 36px `@2x` variant would render as a solid block). To
change them, get new artwork; do not regenerate them from here.
"""
import subprocess
from pathlib import Path

OUT = Path(__file__).parent / "final"
OUT.mkdir(exist_ok=True)

BRACKETS = """<path d="M34 24 L20 24 L20 76 L34 76"/>
    <path d="M66 24 L80 24 L80 76 L66 76"/>"""

# The app icon. Blue-to-indigo reads as "infrastructure" without being the
# generic macOS-utility grey, and the mark stays legible when Finder shrinks
# it to 16px.
#
# The first version drew a heavy pure-white mark on a mid blue and read as a
# white slab. Three things fixed that, and all three matter together: the tile
# goes deeper (#1E3A8A at the foot rather than #2B5BD7), the mark is inset
# further and drawn thinner (scale 5.6 / stroke 7.5, was 6.4 / 9), and it is
# near-white rather than #FFF. Pure white against a saturated tile glares; the
# slight tint is what Apple's own utility icons do.
#
# It is deliberately NOT transparent. Big Sur onward, a macOS app icon is a
# rounded tile — a transparent one looks broken in Finder, not minimal.
APP = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#3D7BF0"/>
      <stop offset="100%" stop-color="#1E3A8A"/>
    </linearGradient>
  </defs>
  <rect x="92" y="92" width="840" height="840" rx="188" fill="url(#bg)"/>
  <g transform="translate(232,232) scale(5.6)"
     fill="none" stroke="#E8F0FE" stroke-width="7.5"
     stroke-linecap="round" stroke-linejoin="round">
    {brackets}
  </g>
  <circle cx="512" cy="512" r="70" fill="#E8F0FE"/>
</svg>"""


def render(stem: str, svg: str, size: int) -> Path:
    svg_path = OUT / f"{stem}.svg"
    png_path = OUT / f"{stem}.png"
    svg_path.write_text(svg)
    try:
        subprocess.run(
            ["rsvg-convert", "-w", str(size), "-h", str(size),
             str(svg_path), "-o", str(png_path)],
            check=True, capture_output=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        subprocess.run(
            ["qlmanage", "-t", "-s", str(size), "-o", str(OUT), str(svg_path)],
            check=True, capture_output=True,
        )
        produced = OUT / f"{svg_path.name}.png"
        if produced.exists():
            produced.rename(png_path)
    return png_path


if __name__ == "__main__":
    # App icon master, and the only thing this script produces. `tauri icon`
    # derives every other size from it; the menu bar assets are supplied by the
    # designer — see the module docstring.
    render("app-icon", APP.format(brackets=BRACKETS), 1024)

    print("final icons:", sorted(p.name for p in OUT.glob("*.png")))
