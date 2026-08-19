#!/usr/bin/env python3
"""Produce the final icon set from the chosen mark: brackets around a node.

Two distinct jobs:

* Menu bar — a *template* image. macOS inverts template images automatically
  for light/dark menu bars and for the highlighted state when the menu is open,
  so it must be pure black on transparent with no colour of its own. Two
  states: idle (hollow node) and active (filled node).

* App icon — the Big Sur shape: a rounded rectangle ("squircle"-ish) with the
  mark reversed out in white. Shown in Finder and the DMG; the app itself has
  no Dock icon.
"""
import subprocess
from pathlib import Path

OUT = Path(__file__).parent / "final"
OUT.mkdir(exist_ok=True)

BRACKETS = """<path d="M34 24 L20 24 L20 76 L34 76"/>
    <path d="M66 24 L80 24 L80 76 L66 76"/>"""

# The menu bar mark is hinted for 22px rather than being the app mark scaled
# down. At 22 the app geometry crowds the node against the brackets and the
# hollow centre closes up into a blob. Hinting means: brackets pushed to the
# edges with shorter feet, thicker strokes, and a larger node with a thicker
# ring so the hole survives rasterisation.
MENUBAR_BRACKETS = """<path d="M32 18 L16 18 L16 82 L32 82"/>
    <path d="M68 18 L84 18 L84 82 L68 82"/>"""

MENUBAR = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
  <g fill="none" stroke="#000000" stroke-width="12"
     stroke-linecap="round" stroke-linejoin="round">
    {brackets}
    {node_stroke}
  </g>
  {node_fill}
</svg>"""

IDLE_NODE_STROKE = '<circle cx="50" cy="50" r="11"/>'
ACTIVE_NODE_FILL = '<circle cx="50" cy="50" r="17" fill="#000000"/>'

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
    idle = MENUBAR.format(
        brackets=MENUBAR_BRACKETS, node_stroke=IDLE_NODE_STROKE, node_fill=""
    )
    active = MENUBAR.format(
        brackets=MENUBAR_BRACKETS, node_stroke="", node_fill=ACTIVE_NODE_FILL
    )

    # Menu bar template images at 22px only.
    #
    # No `@2x` variant: `Image::from_bytes` in tray.rs knows nothing about the
    # filename convention and hands whatever pixel size it finds straight to
    # the menu bar, so a 44px asset fills the 22pt slot edge to edge and reads
    # as a solid block. Shipping only the size the code embeds removes the
    # chance of wiring up the wrong one.
    render("trayTemplate", idle, 22)
    render("trayActiveTemplate", active, 22)

    # App icon master. `tauri icon` derives every other size from this.
    render("app-icon", APP.format(brackets=BRACKETS), 1024)

    print("final icons:", sorted(p.name for p in OUT.glob("*.png")))
