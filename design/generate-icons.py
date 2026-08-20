#!/usr/bin/env python3
"""Draw all eight menu bar tray assets: four states x two appearances.

The mark is an **isometric stack of three slabs** — each layer a rhombus top face
plus its two visible side faces, reading as one solid 3D block, with a status dot
badged on the lower-right corner. That is the visual language GCP's own database
and storage icons use, and it is what the design sheet shows.

# Why the artwork is drawn here rather than recoloured from the delivery

An earlier version of this file said the tray artwork was designer-supplied and
must not be redrawn here, and derived every shipped asset by recolouring the
delivered PNGs. **That is no longer true, and the reason is that the delivery does
not match the sheet.**

The delivered SVGs drew three rounded rectangles under `rotate(-30)` — flat
tilted cards. The sheet draws an isometric stack of extruded slabs. Those are
different marks, not two renderings of one mark: a tilted rectangle has no side
faces and no thickness, so no amount of recolouring turns it into the block on the
sheet. Recolouring can move a pixel's colour; it cannot add a face that was never
drawn. So the geometry is constructed here, from the sheet.

# The app icon is not built here

`src-tauri/icons/icon-source.png` is a finished 1024px master, committed as-is,
and `tauri icon` derives every other size from it. An earlier version of this
script composed that master by cropping a 3D render out of `design/app-source/`,
which meant every run of this script — even one that only touched tray geometry —
silently overwrote the committed icon. Both the render and that code path are
gone: a finished master needs no pipeline.

# The geometry

2:1 dimetric projection, the standard one for this kind of mark: a horizontal
half-width `hw` pairs with a vertical half-height `hw/2`, so each top face is a
rhombus twice as wide as it is tall. That ratio is what makes it read as a cube
rather than a squashed diamond. One slab centred at `(cx, cy)`:

      (cx, cy - hh)          <- back corner
     /            \\
 (cx-hw, cy)    (cx+hw, cy)  <- left / right corners
     \\            /
      (cx, cy + hh)          <- front corner, hh = hw/2

The side faces hang from the left, front, and right corners by `depth`. Three
slabs, bottom drawn first so the upper ones occlude it.

Two things that look like the obvious way to do this and are wrong:

1. **Each slab is drawn opaque, not semi-transparent.** Overlapping translucent
   faces let the lower slab's edges show through the upper one, which reads as a
   wireframe tangle rather than a solid block. The recession is carried by
   stepping the *tint* darker downward instead.
2. **`GAP` must exceed `DEPTH`, but only slightly.** At `GAP=118, DEPTH=52` the
   slabs visibly float apart; `GAP` a little over `DEPTH` leaves a thin sliver of
   each lower slab's side faces showing, which is how the sheet stacks them.
   `GAP <= DEPTH` would bury those faces entirely.

# The two appearance variants are both drawn, not post-processed

The tray needs white ink for a dark menu bar and dark ink for a light one: these
are colour assets rendered with `icon_as_template(false)`, because a template
image is flattened to a black silhouette and would discard both the tint stepping
and the red error dot, so macOS will not invert them for us.

The light variant is **drawn directly with dark ink**, not derived by remapping a
white render's alpha channel. That is a change of method from the previous
recolour path, and it is forced by the opaque-slab decision above: with each slab
drawn opaque, the alpha channel is a flat 255 across the whole stack and every
tint distinction lives in RGB. An alpha remap therefore has no per-slab
information to act on — it would collapse all three slabs to a single ink level
and destroy exactly the recession that makes the stack read as a stack.

What carries over from the recolour work is the *criterion*, which was the useful
part: hold perceived contrast against the menu bar constant. `solve-tray-ink.py`
established it and `DARK_INK` (pure black, counter-intuitively the right choice —
see that script) came out of it. Here it is applied per face tint at draw time by
`dark_level_for`: for each grey level the white ink would use against the dark
bar, solve for the level the dark ink needs against the light bar to reach the
same WCAG ratio. The three top faces come out at 78/52/26 against 26 apart,
evenly separated and monotonic, so the stack keeps both its overall weight and its
internal separation across the two appearances.

# Rasterising

No SVG rasteriser is installed on this machine: `rsvg-convert`, `cairosvg` and
`inkscape` are all absent. The one rasteriser macOS does ship, `qlmanage`,
**flattens transparency onto opaque white**, which is fatal here — every pixel
comes back at alpha 255, and a menu bar icon is mostly transparent. So the assets
are drawn with Pillow directly, which needs no SVG round-trip at all: this
geometry is nothing but filled polygons and two circles, which is precisely what
`ImageDraw` does, and it gives exact alpha. They are drawn at `TRAY_SUPERSAMPLE`x and box-filtered down,
because `ImageDraw.polygon` does not antialias.

The eight assets land in `src-tauri/icons/tray-{state}[-light].png` and are
embedded by `src-tauri/src/tray.rs` with `include_bytes!`. Run this script to
regenerate them after a geometry or ink change; do not hand-edit the PNGs. Every
output is a pure function of the constants in this file, so a re-run with nothing
changed rewrites every file byte-for-byte identically.
"""
import hashlib
from pathlib import Path


# --- the isometric stack, in the abstract ----------------------------------

def slab_faces(cx: float, cy: float, hw: float, depth: float) -> tuple:
    """The three visible faces of one isometric slab, as polygon point lists.

    Returns `(top, left, right)`. `hh = hw / 2` is the 2:1 dimetric ratio; see
    the module docstring for the corner diagram.
    """
    hh = hw / 2
    back = (cx, cy - hh)
    right_c = (cx + hw, cy)
    front = (cx, cy + hh)
    left_c = (cx - hw, cy)

    top = [back, right_c, front, left_c]
    left = [
        left_c, front,
        (front[0], front[1] + depth),
        (left_c[0], left_c[1] + depth),
    ]
    right = [
        front, right_c,
        (right_c[0], right_c[1] + depth),
        (front[0], front[1] + depth),
    ]
    return top, left, right


# The per-slab tint, bottom slab first. These are alphas only in the sense that
# they name how bright the slab would be if white ink were composited at that
# opacity over the tile — the slab is then drawn *opaque* at the resulting level.
# See the module docstring on why opaque.
SLAB_ALPHAS = (0.55, 0.75, 0.95)

# The two side faces are darker than their own top face by these factors, for the
# same reason a real lit cube's are: they face away from the light. Without this
# the three faces of one slab are indistinguishable and the block reads flat.
FACE_LEFT = 0.62
FACE_RIGHT = 0.80


def white_level(alpha: float) -> int:
    """The grey level standing in for white ink at `alpha` over a dark ground.

    The 0.28 floor is the tile showing through: white at alpha 0 over the tile is
    not black, so a ramp that started at 0 would make the bottom slab vanish.
    """
    return int(255 * (0.28 + 0.72 * alpha))


# --- holding perceived contrast constant across the two appearances --------
#
# macOS's two menu bar backgrounds, and the ink used against each. The dark ink is
# pure black, which is counter-intuitive — the naive expectation is that
# dark-on-light reads heavier and so wants softening — but alpha compositing is
# not symmetric about the midpoint and the arithmetic says the opposite. See
# `solve-tray-ink.py` for the working; that script remains the human-checkable
# form of this calculation.
DARK_BG = (0x1C, 0x1C, 0x1E)
LIGHT_BG = (0xF2, 0xF2, 0xF7)
DARK_INK = (0x00, 0x00, 0x00)

# The error dot: Apple's system red. It is the one state whose *colour* is the
# signal, and it is legible on both menu bars (WCAG contrast 4.99 on #1C1C1E,
# 3.05 on #F2F2F7), so it is identical in both variants. Recolouring it to dark
# ink would make `error` indistinguishable from `connected` on a light menu bar.
ERROR_RED = (0xFF, 0x45, 0x3A)


def _linear(channel: float) -> float:
    """sRGB channel (0-255) to linear light, per WCAG 2.x."""
    c = channel / 255.0
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _luminance(rgb: tuple) -> float:
    r, g, b = (_linear(c) for c in rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def _contrast(a: tuple, b: tuple) -> float:
    ya, yb = _luminance(a), _luminance(b)
    hi, lo = max(ya, yb), min(ya, yb)
    return (hi + 0.05) / (lo + 0.05)


def dark_level_for(level: int) -> int:
    """The dark-ink grey that matches `level`'s contrast, appearance for appearance.

    `level` is a grey the white-ink variant uses against the dark menu bar. This
    returns the grey the dark-ink variant should use against the light menu bar so
    that both achieve the same WCAG contrast ratio against their own background —
    which is what keeps the slab stack reading with the same weight and the same
    internal separation on either menu bar.

    Bisection rather than a closed form: the WCAG ratio composes a piecewise gamma
    curve with a linear blend. Contrast against the light ground *rises* as the
    level falls, hence the inverted branch.
    """
    target = _contrast((level, level, level), DARK_BG)
    lo, hi = 0.0, 255.0
    for _ in range(50):
        mid = (lo + hi) / 2
        if _contrast((mid, mid, mid), LIGHT_BG) < target:
            hi = mid
        else:
            lo = mid
    return round((lo + hi) / 2)


TRAY_OUT = Path(__file__).parent.parent / "src-tauri" / "icons"

# The states the app actually renders. `paused` is in the designer's delivery and
# on the sheet (a dot with a pause glyph) but deliberately not built: the app has
# no pause concept, so a `paused` asset would be dead weight and an invitation to
# wire up an unreachable state. See the note on `IconState` in `tray.rs`.
TRAY_STATES = ("disconnected", "connecting", "connected", "error")

# The size the code embeds: the @2x size, and the right one.
#
# An earlier iteration asserted 18px on the theory that the menu bar takes a PNG's
# declared pixel size at face value, so a 36px asset would fill the ~22pt slot and
# read as a solid block. That was measured and is false. The path is
# `tauri::image::Image` -> `tray_icon::Icon` -> `NSStatusItem.button.image`, and
# `tray-icon`'s macOS backend (0.24.2, `set_icon_for_ns_status_item_button`)
# hardcodes the point size:
#
#     let icon_height: f64 = 18.0;
#     let icon_width: f64 = (width as f64) / (height as f64 / icon_height);
#     nsimage.setSize(NSSize::new(icon_width, icon_height));
#
# The PNG's pixel dimensions therefore set only the *aspect ratio* and the
# backing-store density; the drawn size is 18pt regardless. Probing the live
# `NSImage` confirmed it: an 18px asset gives `size = 18x18 pt` with an `18x18 px`
# representation (1x — soft on a Retina display), and a 36px asset gives the same
# `18x18 pt` with a `36x36 px` representation (2x — crisp). Nothing overflows and
# nothing is a solid block.
#
# So 36px is the @2x asset this Retina-only target wants, and it matches the
# delivery README's recommendation. The 18pt drawn height is `tray-icon`'s choice,
# not ours: it cannot be raised from here, so the glyph occupies 18 of the 22pt
# slot whatever we embed. Size is fixed; sharpness is what this buys.
TRAY_SIZE = 36

# `ImageDraw.polygon` does not antialias, so the stack is drawn at this multiple
# and box-filtered down. At 36px the slab edges are the whole mark — an aliased
# rhombus edge at native size reads as a stair-step, not a plane. 8x puts 64
# samples behind every output pixel, which is past the point where more helps at
# this size.
TRAY_SUPERSAMPLE = 8

# Tray geometry, proportional to the app icon's but retuned for 36px rather than
# scaled blindly: the dot has to stay a legible badge at native size, so it keeps
# slightly more of the frame than a pure scale would give it.
#
# The stack is centred a little left of the frame to leave the dot its corner, and
# the whole mark is inset from the edges — a menu bar icon drawn to its own bounds
# sits flush against its neighbours.
#
# `TRAY_GAP` exceeds `TRAY_DEPTH` by a small margin so a sliver of each lower
# slab's side faces stays visible; see the module docstring on why `GAP <= DEPTH`
# buries them and a large `GAP` makes the slabs float apart.
#
# **Size the mark to the frame, not to a comfortable-looking margin.** These
# values are an earlier iteration's (`HW` 9.0, `GAP` 5.5) scaled by 1.35, and that
# earlier iteration was a mistake worth recording: it was shrunk to buy the
# `connecting` ring its margin, which left the mark filling only 58% of the frame
# width and 67% of its height. In isolation that looks balanced. In the menu bar it
# reads as a small icon next to everyone else's, because `tray-icon` draws the
# asset at a hardcoded 18pt regardless of its pixel size (see `TRAY_SIZE`) — so the
# *only* lever on apparent size is how much of the 36px canvas the ink covers.
# Whitespace inside the asset is whitespace in the menu bar.
#
# 1.35 is the largest scale that still fits: at 1.45 the `connecting` ring's
# bounding box overflows the frame. The mark now spans ~84% of the frame width,
# in line with the 78% the pre-redesign asset used.
TRAY_HW = 12.15
TRAY_DEPTH = 4.05
TRAY_GAP = 7.43
TRAY_CX = 14.97
TRAY_BASE_Y = 22.33

# The status dot, badged on the bottom slab's right corner as on the sheet.
#
# The radius is smaller than a proportional scale from the app icon would give.
# That is deliberate and was arrived at by looking at it: at 1024 the dot can be a
# bold badge, but at 36px the same proportion makes the dot compete with the stack
# for the eye instead of annotating it, and it leaves the `connecting` ring no room
# to be a ring. Sized so the ring fits inside the frame with the dot still clearly
# a disc and not a dash.
#
# The centre is set absolutely rather than derived from the slab corner. Deriving
# it (`TRAY_CX + TRAY_HW - 2.4`, `TRAY_BASE_Y + TRAY_DEPTH + 0.4`) put the dot on
# the bottom slab's *right face* instead of beside its corner, which at 36px reads
# as a bulge on the slab rather than as a badge. These values tuck it against the
# corner with clear space on the outward sides.
#
# How far left the dot can sit is bounded by `disconnected`, not by the solid
# states: its dimmed ink is the closest in value to the slab it abuts, so it is the
# first to lose its edge and read as a bulge. Pulling the dot inward past roughly
# this offset merges it into the bottom slab's front and right faces on both menu
# bars. Move it and re-check `disconnected` first — the other three states will
# still look fine well past the point where it has failed.
TRAY_DOT_CX = 27.66
TRAY_DOT_CY = 29.08
TRAY_DOT_R = 3.1

# `connecting` adds a ring around the dot. A dashed ring at 36px turns to mush, so
# it is a plain thin ring separated from the dot by a gap of clear space. The gap
# is what makes it read as a ring *around* a dot rather than as one fatter dot, and
# it has to be wide enough to survive the downsample — at a 1px gap the two merge.
# It matters most on the light variant, where ring and dot are both near-black and
# the gap is the only thing distinguishing them.
TRAY_RING_R = 4.97
TRAY_RING_W = 1.05

# `disconnected` dims its dot rather than dropping it: an absent dot reads as a
# rendering failure, a dim one reads as "off".
#
# How far to dim it is constrained from both sides, and the obvious value fails.
# It has to stay well below the solid dot so `disconnected` and `connected` are not
# confusable — but it also has to stay clear of the *bottom slab's side faces*,
# because the dot is drawn overlapping exactly those. At 0.42 the dot came out at
# grey 148 against a right face at 137: a 11-level edge, which is no edge at all,
# and the dot read as a bulge on the slab rather than as a dot. Dropping to 0.18
# puts it at 114 against 137/106 — below both, so it has a real edge on every side
# it touches while still reading as ink rather than as a hole.
TRAY_DIM_DOT = 0.18


def _ink_levels(alpha: float, light: bool) -> tuple:
    """The (top, left, right) face greys for one slab, for one appearance.

    The white-ink levels are the reference; the dark-ink ones are solved from them
    to match contrast against their own menu bar. See `dark_level_for`.
    """
    level = white_level(alpha)
    faces = (int(level * 1.0), int(level * FACE_LEFT), int(level * FACE_RIGHT))
    if light:
        return tuple(dark_level_for(face) for face in faces)
    return faces


def draw_tray(state: str, light: bool) -> "object":
    """Draw one tray asset: the isometric stack plus this state's status dot.

    Returns a `TRAY_SIZE` square RGBA image with real alpha — transparent
    everywhere the mark is not, which is most of it.
    """
    from PIL import Image, ImageDraw

    scale = TRAY_SUPERSAMPLE
    big = Image.new("RGBA", (TRAY_SIZE * scale, TRAY_SIZE * scale), (0, 0, 0, 0))
    draw = ImageDraw.Draw(big)

    def up(points: list) -> list:
        return [(x * scale, y * scale) for x, y in points]

    for index, alpha in enumerate(SLAB_ALPHAS):
        # index 0 is the bottom slab, drawn first so the upper ones occlude it.
        # Each face is opaque at its own grey: see the module docstring on why
        # translucent overlapping faces read as a wireframe tangle.
        cy = TRAY_BASE_Y - index * TRAY_GAP
        faces = slab_faces(TRAY_CX, cy, TRAY_HW, TRAY_DEPTH)
        for face, value in zip(faces, _ink_levels(alpha, light)):
            draw.polygon(up(face), fill=(value, value, value, 255))

    # The status dot. Its ink is the solid end of the ramp — the brightest white
    # on a dark bar, the solved black on a light one — except for `error`, whose
    # red is the signal and is identical in both variants.
    solid = 255 if not light else dark_level_for(255)
    dot_ink = ERROR_RED if state == "error" else (solid, solid, solid)

    if state == "disconnected":
        # Dimmed toward the menu bar, not made transparent: a translucent dot
        # would let the slab edge it overlaps show through it.
        dim = white_level(TRAY_DIM_DOT)
        value = dim if not light else dark_level_for(dim)
        dot_ink = (value, value, value)

    box = [
        (TRAY_DOT_CX - TRAY_DOT_R, TRAY_DOT_CY - TRAY_DOT_R),
        (TRAY_DOT_CX + TRAY_DOT_R, TRAY_DOT_CY + TRAY_DOT_R),
    ]
    draw.ellipse(up(box), fill=(*dot_ink, 255))

    if state == "connecting":
        ring = [
            (TRAY_DOT_CX - TRAY_RING_R, TRAY_DOT_CY - TRAY_RING_R),
            (TRAY_DOT_CX + TRAY_RING_R, TRAY_DOT_CY + TRAY_RING_R),
        ]
        draw.ellipse(
            up(ring),
            outline=(*dot_ink, 255),
            width=max(1, round(TRAY_RING_W * scale)),
        )

    # Box filter down. `LANCZOS` would ring on these hard edges and can overshoot
    # past the 0-255 range on the alpha channel; `BOX` is an exact average of the
    # 64 samples behind each output pixel, which is what supersampling wants.
    return big.resize((TRAY_SIZE, TRAY_SIZE), Image.Resampling.BOX)


def assert_square(png: Path, size: int) -> None:
    """Fail loudly if an asset is not exactly `size` x `size`.

    The IHDR width/height are big-endian u32 at a fixed offset in every PNG, so
    this needs no image library. Wrong dimensions here are not visible in a diff
    and degrade quietly rather than loudly: `tray-icon` draws whatever it is given
    at 18pt, so an 18px asset is not broken -- just soft, at 1x on a Retina
    display, which is the bug this size exists to fix. `tray.rs` asserts the same
    number on the embedded bytes.
    """
    header = png.read_bytes()[16:24]
    width = int.from_bytes(header[:4], "big")
    height = int.from_bytes(header[4:], "big")
    if (width, height) != (size, size):
        raise SystemExit(
            f"{png.name} is {width}x{height}, expected {size}x{size}"
        )


def build_tray() -> list:
    """Draw and install both appearance variants of every wired tray state."""
    written = []
    for state in TRAY_STATES:
        for light in (False, True):
            suffix = "-light" if light else ""
            dest = TRAY_OUT / f"tray-{state}{suffix}.png"
            # `optimize=True` so the byte output is a deterministic function of
            # the pixels; Pillow writes no timestamp, so a re-run is byte-equal.
            draw_tray(state, light).save(dest, optimize=True)
            written.append(dest)

    for png in written:
        assert_square(png, TRAY_SIZE)

    # A geometry slip that renders two (state, appearance) pairs identically is
    # otherwise completely invisible: the tray still shows *an* icon, just the
    # wrong one, and only for the states nobody was looking at. `tray.rs` asserts
    # the same property on the embedded bytes.
    seen = {}
    for png in written:
        digest = hashlib.sha256(png.read_bytes()).hexdigest()
        if digest in seen:
            raise SystemExit(
                f"{png.name} is byte-identical to {seen[digest]} -- two states "
                f"or appearances are sharing one asset."
            )
        seen[digest] = png.name
    return written


if __name__ == "__main__":
    # Both appearances of all four wired states. The app icon is NOT built here
    # -- see the module docstring.
    tray = build_tray()
    print(f"tray icons ({TRAY_SIZE}px, all distinct):",
          sorted(p.name for p in tray))
