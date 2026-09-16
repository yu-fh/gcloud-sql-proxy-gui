#!/usr/bin/env python3
"""Draw the menu bar tray icons: an isometric database stack plus a status dot.

Run from this directory with no arguments; it overwrites the `tray-*.png` files
next to it. Requires only Pillow.

    python3 generate-tray-icons.py

# Why a generator and not hand-drawn assets

There are ten assets and they differ along three axes that must stay consistent:
the state (which dot colour), the menu bar appearance (which ink for the glyph),
and, for `connecting`, which blink frame. Hand-editing ten PNGs in step is how
the set drifted before -- `connected` shipped with no dot at all and
`connecting` with a near-invisible grey one, which is the bug this rewrite
fixes. Geometry lives here once and every asset is derived from it.

# Sizing

`TARGET_PX = 72` is a 4x raster of the 18pt height that `tray-icon` hardcodes
for the status item (see the note on `TRAY_ICON_PX` in `tray.rs`). The PNG's
pixel size sets only the aspect ratio and the sharpness, never the on-screen
height -- so this is about crispness on Retina, not about a bigger icon. To make
the *dot* read larger, the glyph is drawn smaller within the frame and the dot
takes a bigger share of it: `DOT_DIAMETER` is 0.38 of the canvas where the old
artwork used roughly 0.22.

Everything is drawn at `SUPERSAMPLE`x and downsampled with LANCZOS; PIL has no
antialiased polygon fill, so drawing at final size gives visibly jagged slab
edges at this scale.
"""

from PIL import Image, ImageDraw

TARGET_PX = 72
SUPERSAMPLE = 8
CANVAS = TARGET_PX * SUPERSAMPLE

# --- Geometry, in fractions of the canvas -----------------------------------
#
# The glyph is an isometric stack of three slabs. It is inset from the right and
# bottom edges to leave room for the status dot to sit in the corner without
# overlapping the silhouette -- the dot is the signal and must never be read as
# part of the database.

GLYPH_WIDTH = 0.62      # of canvas
GLYPH_CX = 0.40         # centre of the stack, left of frame centre
GLYPH_TOP = 0.13
SLAB_HEIGHT = 0.105     # vertical thickness of one slab's side face
SLAB_GAP = 0.055        # air between slabs
ELLIPSE_RATIO = 0.50    # top face height / width, the isometric foreshortening

DOT_DIAMETER = 0.38     # of canvas -- was ~0.22 in the superseded artwork
DOT_CX = 0.775
DOT_CY = 0.775

# --- Ink --------------------------------------------------------------------
#
# Two appearances, because these are colour assets rather than template images
# (see `icon_as_template(false)` in `tray.rs`). Each is a top/side/shadow triple
# so the isometric faces stay distinguishable; the values were picked to hold
# contrast against their own menu bar rather than to match each other.

INK = {
    # White artwork for a dark menu bar.
    "dark": {"top": (255, 255, 255, 255), "side": (255, 255, 255, 170), "edge": (255, 255, 255, 90)},
    # Dark artwork for a light menu bar.
    "light": {"top": (32, 34, 38, 255), "side": (32, 34, 38, 180), "edge": (32, 34, 38, 105)},
}

# Status dot colours. These are exempt from the appearance recolour: a red error
# must read as red on either menu bar, and these hues clear 3:1 against both.
DOT = {
    "error": (255, 69, 58, 255),       # systemRed
    "connected": (48, 209, 88, 255),   # systemGreen
    "connecting": (10, 132, 255, 255), # systemBlue
}

# No ring around the dot. An earlier version drew one in the menu bar's own
# ground -- translucent black on a dark bar -- to separate the dot from the glyph
# behind it. But the dot sits outside the glyph's silhouette, in the corner the
# stack is inset from, so there was nothing to separate: the ring only read as a
# black outline drawn around an otherwise clean dot. A white ring is worse, not
# better -- on a dark menu bar it becomes a bright halo that pulls more attention
# than the colour it surrounds.


def draw_glyph(draw, ink):
    """The isometric stack: three slabs, back to front, bottom to top."""
    w = GLYPH_WIDTH * CANVAS
    half = w / 2
    cx = GLYPH_CX * CANVAS
    ell_h = w * ELLIPSE_RATIO
    slab_h = SLAB_HEIGHT * CANVAS
    gap = SLAB_GAP * CANVAS

    # Bottom slab first so upper slabs overlap it, which is what reads as depth.
    for i in (2, 1, 0):
        top_y = GLYPH_TOP * CANVAS + i * (slab_h + gap)

        # The side face: the band between the top ellipse and the bottom one.
        # Drawn as a rectangle capped by the lower ellipse, so the silhouette
        # curves at the bottom the way a cylinder's would.
        draw.rectangle(
            [cx - half, top_y + ell_h / 2, cx + half, top_y + ell_h / 2 + slab_h],
            fill=ink["side"],
        )
        draw.ellipse(
            [cx - half, top_y + slab_h, cx + half, top_y + slab_h + ell_h],
            fill=ink["side"],
        )
        # A hairline under the top face separates adjacent slabs when the ink is
        # flat -- without it the stack reads as one tall blob at 18pt.
        draw.ellipse(
            [cx - half, top_y + ell_h * 0.10, cx + half, top_y + ell_h * 1.10],
            fill=ink["edge"],
        )
        # The top face last: it is the brightest surface and must win.
        draw.ellipse([cx - half, top_y, cx + half, top_y + ell_h], fill=ink["top"])


def draw_dot(draw, colour):
    """The status dot: flat colour, no outline. See the note on RING above."""
    r = DOT_DIAMETER * CANVAS / 2
    cx, cy = DOT_CX * CANVAS, DOT_CY * CANVAS
    draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=colour)


def render(appearance, dot_colour):
    """One asset: the glyph in the appearance's ink, plus an optional dot."""
    img = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    draw_glyph(draw, INK[appearance])
    if dot_colour is not None:
        draw_dot(draw, dot_colour)
    return img.resize((TARGET_PX, TARGET_PX), Image.LANCZOS)


def main():
    # `disconnected` carries no dot: nothing is connected, so there is no status
    # to signal, and an empty menu bar slot would leave no way to open the menu.
    # `connecting` blinks between its dot frame and the bare glyph. With no ring
    # left to hold, that dark half is pixel-for-pixel what `disconnected` already
    # is, so it is not generated as its own file -- `tray.rs` points the off phase
    # at the disconnected asset instead. Two names for identical bytes would be
    # two things to keep in step for no benefit.
    assets = {
        "tray-disconnected": None,
        "tray-connected": DOT["connected"],
        "tray-error": DOT["error"],
        "tray-connecting": DOT["connecting"],
    }

    for name, dot in assets.items():
        for appearance in ("dark", "light"):
            # The dark-menu-bar asset is the unsuffixed one: it is the artwork as
            # originally delivered, and `tray.rs` names the light one explicitly.
            suffix = "" if appearance == "dark" else "-light"
            img = render(appearance, dot)
            path = f"{name}{suffix}.png"
            img.save(path)
            print(f"wrote {path} ({TARGET_PX}x{TARGET_PX})")


if __name__ == "__main__":
    main()
