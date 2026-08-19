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

MENUBAR = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
  <g fill="none" stroke="#000000" stroke-width="9"
     stroke-linecap="round" stroke-linejoin="round">
    {brackets}
    {node_stroke}
  </g>
  {node_fill}
</svg>"""

IDLE_NODE_STROKE = '<circle cx="50" cy="50" r="9"/>'
ACTIVE_NODE_FILL = '<circle cx="50" cy="50" r="14" fill="#000000"/>'

# The app icon. Blue-to-indigo reads as "infrastructure" without being the
# generic macOS-utility grey, and white-on-blue keeps the mark legible when
# Finder shrinks it to 16px.
APP = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#4C8DFF"/>
      <stop offset="100%" stop-color="#2B5BD7"/>
    </linearGradient>
  </defs>
  <rect x="92" y="92" width="840" height="840" rx="188" fill="url(#bg)"/>
  <g transform="translate(192,192) scale(6.4)"
     fill="none" stroke="#FFFFFF" stroke-width="9"
     stroke-linecap="round" stroke-linejoin="round">
    {brackets}
  </g>
  <circle cx="512" cy="512" r="90" fill="#FFFFFF"/>
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
        brackets=BRACKETS, node_stroke=IDLE_NODE_STROKE, node_fill=""
    )
    active = MENUBAR.format(
        brackets=BRACKETS, node_stroke="", node_fill=ACTIVE_NODE_FILL
    )

    # Menu bar template images: 1x and 2x for the 22pt slot.
    render("trayTemplate", idle, 22)
    render("trayTemplate@2x", idle, 44)
    render("trayActiveTemplate", active, 22)
    render("trayActiveTemplate@2x", active, 44)

    # App icon master. `tauri icon` derives every other size from this.
    render("app-icon", APP.format(brackets=BRACKETS), 1024)

    print("final icons:", sorted(p.name for p in OUT.glob("*.png")))
