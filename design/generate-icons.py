#!/usr/bin/env python3
"""Produce the app icon master, and derive the light-mode tray variants.

The app icon: the Big Sur shape — a rounded rectangle ("squircle"-ish) with the
bracket mark reversed out in white. Shown in Finder and the DMG; the app itself
has no Dock icon. `tauri icon` derives every other size from the 1024px master
this writes.

# The tray artwork is designer-supplied; the recolour is not

The tray uses a designer-supplied set — a translucent Cloud SQL database glyph
with a status dot, for `disconnected`, `connecting`, `connected`, and `error`.
That *artwork* is not drawn here and must not be redrawn here: the geometry, the
layer opacities, and the dot treatment are the designer's, and changing them
means getting new artwork. The committed sources are in `design/tray-source/`.

What *is* done here is mechanical and therefore reproducible: the designer drew
the ink as `white` at various opacities, which is legible on a dark menu bar and
close to invisible on a light one. macOS will not invert it for us, because
these are colour assets rendered with `icon_as_template(false)` — a template
image would be flattened to a black silhouette, discarding both the translucency
and the red error dot. So a light-menu-bar variant is derived by substituting the
ink colour and remapping the alpha channel to hold perceived contrast constant;
see `DARK_INK` and `tray_alpha_map` for how that was chosen, and
`solve-tray-ink.py` for the working. The red error dot is exempt and passes
through unchanged.

The eight assets land in `src-tauri/icons/tray-{state}[-light].png` and are
embedded by `src-tauri/src/tray.rs` with `include_bytes!`. Run this script to
regenerate them after the designer redelivers or the ink choice changes; do not
hand-edit the PNGs.
"""
import hashlib
import re
import subprocess
from pathlib import Path

OUT = Path(__file__).parent / "final"
OUT.mkdir(exist_ok=True)

# The app icon: the designer's database glyph on a dark tile, matching the
# design sheet. Derived from the same `connected.svg` the tray uses, so the two
# icons cannot drift — an app icon that looks unrelated to its menu bar icon is
# the kind of mismatch nobody reports but everybody notices.
#
# Three adjustments the tray artwork needs at tile size, each because a menu
# bar and an app icon are looked at differently:
#
# 1. Layer opacities are lifted (0.38/0.52/0.66 -> 0.62/0.78/0.94). The menu
#    bar values are deliberately faint so the mark recedes; at 1024 that same
#    faintness reads as a smudge rather than three distinct plates.
# 2. The glyph is centred on its *ink*, not its viewBox. The plates occupy
#    roughly x 3.5-13.5, y 2-14 of the 18x18 box, so centring the box leaves
#    the artwork visibly high and left.
# 3. The status dot is tucked against the plates and shrunk. At 18px it sits
#    clear of them with a wide gap so it survives rasterisation; on a tile that
#    same gap leaves it floating in empty space instead of reading as a badge
#    on the database. It belongs against the lower-right plate corner.
#
# The tile is a blue-shifted graphite rather than flat grey — dark like the
# design sheet, but with enough hue not to read as inert. It is deliberately
# NOT transparent: Big Sur onward a macOS app icon is a rounded tile, and a
# transparent one looks broken in Finder rather than minimal.
APP_TILE_TOP = "#3E4552"
APP_TILE_BOTTOM = "#1B1F26"
APP_LIFT = {
    "0.38": "0.62", "0.52": "0.78", "0.66": "0.94",
    "0.22": "0.40", "0.28": "0.46", "0.34": "0.52",
}
APP_SCALE = 34
# Ink centre in glyph units, from the plate geometry plus the tucked-in dot.
APP_INK_CX, APP_INK_CY = 9.1, 8.4

# The status dot, in the designer's 18-unit glyph space. The lower plate's
# rotated corner reaches about (12.4, 12.6); putting the dot's centre a little
# inside that overlaps it slightly, so it reads as a badge sitting on the
# database rather than a separate mark floating beside it.
APP_DOT_CX, APP_DOT_CY = 11.9, 11.6
APP_DOT_R = 1.5

APP_TILE = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="{top}"/>
      <stop offset="100%" stop-color="{bottom}"/>
    </linearGradient>
  </defs>
  <rect x="92" y="92" width="840" height="840" rx="188" fill="url(#tile)"/>
  <g transform="translate({ox},{oy}) scale({scale})">
    {glyph}
  </g>
</svg>"""


def app_icon_svg() -> str:
    """Compose the app icon from the designer's `connected` tray artwork."""
    source = (Path(__file__).parent / "tray-source" / "connected.svg").read_text()
    glyph = source.split("<g>", 1)[1].rsplit("</g>", 1)[0]

    def lift(match: re.Match) -> str:
        attr, value = match.group(1), match.group(2)
        return f'{attr}="{APP_LIFT.get(value, value)}"'

    glyph = re.sub(r'(fill-opacity|stroke-opacity)="([0-9.]+)"', lift, glyph)
    glyph = glyph.replace(
        'cx="14.7" cy="14.1" r="2.05"',
        f'cx="{APP_DOT_CX}" cy="{APP_DOT_CY}" r="{APP_DOT_R}"',
    )

    return APP_TILE.format(
        top=APP_TILE_TOP,
        bottom=APP_TILE_BOTTOM,
        glyph=glyph,
        ox=round(512 - APP_INK_CX * APP_SCALE, 1),
        oy=round(512 - APP_INK_CY * APP_SCALE, 1),
        scale=APP_SCALE,
    )


def rasterise(svg_path: Path, png_path: Path, size: int) -> Path:
    """Render an SVG on disk to a PNG of exactly `size` x `size`.

    `rsvg-convert` when it is installed, else `qlmanage`, which every macOS has.
    `qlmanage` names its output after the input file and only honours `-s` as an
    upper bound on the longest edge, so the caller checks the real dimensions
    rather than trusting the flag.

    Only the app icon goes through here. Do NOT use this for the tray assets:
    `qlmanage` flattens transparency onto opaque white, which is invisible in a
    thumbnail and fatal for a menu bar icon (every pixel comes back at alpha
    255). The tray path recolours the designer's already-rasterised PNGs
    instead — see `tray_light_variant`.
    """
    try:
        subprocess.run(
            ["rsvg-convert", "-w", str(size), "-h", str(size),
             str(svg_path), "-o", str(png_path)],
            check=True, capture_output=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        subprocess.run(
            ["qlmanage", "-t", "-s", str(size), "-o", str(png_path.parent),
             str(svg_path)],
            check=True, capture_output=True,
        )
        produced = png_path.parent / f"{svg_path.name}.png"
        if produced.exists():
            produced.replace(png_path)
    return png_path


def render(stem: str, svg: str, size: int) -> Path:
    """Write an inline SVG string into `design/final/` and rasterise it."""
    svg_path = OUT / f"{stem}.svg"
    svg_path.write_text(svg)
    return rasterise(svg_path, OUT / f"{stem}.png", size)




# --- tray: the light-menu-bar variants -------------------------------------
#
# The designer's artwork is white ink on transparent, which reads on a dark menu
# bar and is close to invisible on a light one. macOS will not invert it for us:
# these are colour assets rendered with `icon_as_template(false)`, because a
# template image is flattened to a black silhouette and would discard both the
# translucency and the red error dot. So a second set is derived here.
#
# The derivation works on the designer's rasterised PNGs, not on the SVGs. That
# is deliberate. Rasterising the SVGs here would need a rasteriser this machine
# does not reliably have -- `rsvg-convert` is not installed and `qlmanage`
# flattens alpha onto opaque white -- and would risk the derived geometry drifting
# from the delivered geometry. The PNGs already carry the artwork exactly: pure
# white RGB with every layer opacity, every overlap, and all the antialiasing
# resolved into the alpha channel. Recolouring those is a per-pixel operation that
# cannot move an edge.

TRAY_SRC = Path(__file__).parent / "tray-source"
TRAY_OUT = Path(__file__).parent.parent / "src-tauri" / "icons"

# The states the app actually renders. `paused` is in the designer's delivery
# but deliberately not built: the app has no pause concept, so a `paused` asset
# would be dead weight and an invitation to wire up an unreachable state. See
# the note on `IconState` in `tray.rs`.
TRAY_STATES = ("disconnected", "connecting", "connected", "error")

# The size the code embeds: the designer's `@2x` size, and the right one.
#
# An earlier iteration of this file asserted 18px on the theory that the menu bar
# takes a PNG's declared pixel size at face value, so a 36px asset would fill the
# ~22pt slot and read as a solid block. That was measured and is false. The path
# is `tauri::image::Image` -> `tray_icon::Icon` -> `NSStatusItem.button.image`,
# and `tray-icon`'s macOS backend (0.24.2,
# `set_icon_for_ns_status_item_button`) hardcodes the point size:
#
#     let icon_height: f64 = 18.0;
#     let icon_width: f64 = (width as f64) / (height as f64 / icon_height);
#     nsimage.setSize(NSSize::new(icon_width, icon_height));
#
# The PNG's pixel dimensions therefore set only the *aspect ratio* and the
# backing-store density; the drawn size is 18pt regardless. Probing the live
# `NSImage` confirmed it: an 18px asset gives `size = 18x18 pt` with a `18x18 px`
# representation (1x -- soft on a Retina display), and a 36px asset gives the
# same `18x18 pt` with a `36x36 px` representation (2x -- crisp). Nothing
# overflows and nothing is a solid block.
#
# So 36px is exactly the designer's README recommendation ("18px for 1x, 36px for
# @2x"), and it is the @2x asset that this Retina-only target wants. Note the
# 18pt drawn height is `tray-icon`'s choice, not ours: it cannot be raised from
# here, so the glyph occupies 18 of the 22pt slot whatever we embed. Size is
# fixed; sharpness is what this buys.
TRAY_SIZE = 36

# The designer ships 18/24/36/48px per state, so 36px is taken directly from the
# delivery rather than resampled from the 48px one.
#
# That was measured rather than assumed, because "downscale from the largest
# source" is the usual advice and here it is the worse option. Counting distinct
# non-zero alpha values -- a proxy for how much antialiasing detail survives --
# the designer's native 36px export beats a `sips` 48 -> 36 downscale on every
# state: 218 vs 210 (connected), 215 vs 212 (connecting), 204 vs 194
# (disconnected), 218 vs 210 (error). The designer rasterised each size from the
# vector artwork independently, so the 36px export has never been through a
# resampling filter; a downscale of the 48px one has. Straight copy wins.
TRAY_SOURCE_SIZE = 36

# macOS's two menu bar backgrounds, and the ink used against each.
DARK_BG = (0x1C, 0x1C, 0x1E)
LIGHT_BG = (0xF2, 0xF2, 0xF7)
WHITE_INK = (0xFF, 0xFF, 0xFF)

# The dark ink for the light-menu-bar variant: pure black.
#
# The naive expectation is that dark-on-light reads *heavier* than light-on-dark,
# so the ink should be softened to a grey and the opacities pulled down. The
# arithmetic says the opposite, because alpha compositing is not symmetric about
# the midpoint. White at 0.38 over #1C1C1E lifts relative luminance from ~0.011
# to ~0.16, a factor of 14. Black at 0.38 over #F2F2F7 only drops it from ~0.88
# to ~0.34, under a factor of 3. At equal opacity the dark variant is the
# *fainter* one -- so a softer ink would make an already-too-weak mark weaker,
# and pure black is what a stack of translucent layers on a light ground needs.
#
# Softer inks were checked: #1C1C1E, #2C2C2E, #3A3A3C and up all fall further
# below the white-on-dark reference on every layer. See `solve-tray-ink.py`.
DARK_INK = (0x00, 0x00, 0x00)

# Accents that carry meaning rather than just ink, and so survive the recolour
# unchanged. `#FF453A` is the error dot: Apple's system red, legible on both
# backgrounds (WCAG contrast 4.99 on #1C1C1E, 3.05 on #F2F2F7) and the one state
# whose colour *is* the signal. Recolouring it to dark ink would make `error`
# indistinguishable from `connected` on a light menu bar. `#6E6E73` is the pause
# glyph, present in the designer's `paused.svg`, which this script does not
# build; it is listed so that wiring `paused` up later does not silently
# recolour it.
#
# Matched with a tolerance because the delivered PNGs are palettised and the
# red lands on neighbouring values (255,69,58), (255,70,58), (255,68,58).
TRAY_KEEP = ((0xFF, 0x45, 0x3A), (0x6E, 0x6E, 0x73))
TRAY_KEEP_TOLERANCE = 6


def _linear(channel: int) -> float:
    """sRGB channel (0-255) to linear light, per WCAG 2.x."""
    c = channel / 255.0
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _luminance(rgb: tuple) -> float:
    r, g, b = (_linear(c) for c in rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def _over(ink: tuple, alpha: float, bg: tuple) -> tuple:
    return tuple(ink[i] * alpha + bg[i] * (1 - alpha) for i in range(3))


def _contrast(a: tuple, b: tuple) -> float:
    ya, yb = _luminance(a), _luminance(b)
    hi, lo = max(ya, yb), min(ya, yb)
    return (hi + 0.05) / (lo + 0.05)


def _alpha_for_contrast(target: float, ink: tuple, bg: tuple) -> float:
    """The alpha at which `ink` over `bg` reaches `target` contrast.

    Bisection rather than a closed form: the WCAG ratio composes a piecewise
    gamma curve with a linear blend, and 40 halvings of a monotonic function on
    [0, 1] is exact far past 8-bit alpha.
    """
    lo, hi = 0.0, 1.0
    for _ in range(40):
        mid = (lo + hi) / 2
        if _contrast(_over(ink, mid, bg), bg) < target:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2


def tray_alpha_map() -> list:
    """A 256-entry lookup: white-on-dark alpha -> dark-on-light alpha.

    Holds *perceived contrast against the menu bar* constant. For each input
    alpha, take the contrast the white ink achieves against the dark menu bar,
    then find the alpha at which the dark ink achieves the same contrast against
    the light one. Applied to the whole alpha channel this preserves the
    three-layer stack's internal separation as well as its overall weight, and
    because it is one monotonic curve it cannot introduce banding or reorder two
    layers that were distinct in the source.

    The curve rises for most of its range (alpha 97 -> 121 at the lower fill)
    and falls only near the top (255 -> 238), which is what keeps the opaque
    dots reading as ink rather than as a hole punched in the menu bar.
    """
    out = []
    for value in range(256):
        alpha = value / 255
        target = _contrast(_over(WHITE_INK, alpha, DARK_BG), DARK_BG)
        out.append(round(_alpha_for_contrast(target, DARK_INK, LIGHT_BG) * 255))
    return out


def _is_kept(rgb: tuple) -> bool:
    """Whether a pixel is a meaning-carrying accent rather than ink."""
    return any(
        all(abs(rgb[i] - keep[i]) <= TRAY_KEEP_TOLERANCE for i in range(3))
        for keep in TRAY_KEEP
    )


def tray_light_variant(source: Path, dest: Path) -> Path:
    """Write the light-menu-bar variant of one designer tray PNG.

    White ink becomes dark ink with its alpha remapped through
    `tray_alpha_map`; accents in `TRAY_KEEP` pass through untouched, alpha
    included, so the red error dot is bit-identical between the two variants.
    """
    from PIL import Image

    image = Image.open(source).convert("RGBA")
    if image.size != (TRAY_SIZE, TRAY_SIZE):
        raise SystemExit(
            f"{source.name} is {image.size[0]}x{image.size[1]}, expected "
            f"{TRAY_SIZE}x{TRAY_SIZE}"
        )

    lut = tray_alpha_map()
    pixels = []
    # `get_flattened_data`, not `getdata`: Pillow 12 deprecated the latter (it
    # warns, and goes away in Pillow 14) and this is its replacement. Both return
    # the same flat sequence of RGBA tuples.
    for r, g, b, a in image.get_flattened_data():
        if a == 0:
            pixels.append((0, 0, 0, 0))
        elif _is_kept((r, g, b)):
            pixels.append((r, g, b, a))
        else:
            pixels.append((*DARK_INK, lut[a]))

    out = Image.new("RGBA", image.size)
    # `putdata`, not `put_flattened_data`: Pillow 12 renamed the getter but kept
    # the setter's name, so the symmetric-looking spelling does not exist.
    out.putdata(pixels)
    out.save(dest)
    return dest


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
    """Install both appearance variants of every wired tray state.

    The dark-menu-bar asset is the designer's PNG copied verbatim -- nothing
    here is entitled to alter the delivered artwork. The light-menu-bar asset is
    derived from it by `tray_light_variant`.
    """
    written = []
    for state in TRAY_STATES:
        source = TRAY_SRC / f"{state}-{TRAY_SOURCE_SIZE}px.png"
        dark = TRAY_OUT / f"tray-{state}.png"
        dark.write_bytes(source.read_bytes())
        written.append(dark)
        written.append(
            tray_light_variant(source, TRAY_OUT / f"tray-{state}-light.png")
        )

    for png in written:
        assert_square(png, TRAY_SIZE)

    # A copy-paste slip that maps two (state, appearance) pairs onto one asset is
    # otherwise completely invisible: the tray still shows *an* icon, just the
    # wrong one, and only for the states nobody was looking at.
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
    # App icon master. `tauri icon` derives every other size from it.
    render("app-icon", app_icon_svg(), 1024)
    print("final icons:", sorted(p.name for p in OUT.glob("*.png")))

    # Tray: the designer's artwork installed as-is for a dark menu bar, plus the
    # derived recolour for a light one. See the section comment on the line
    # between "supplied" and "derived".
    tray = build_tray()
    print(f"tray icons ({TRAY_SIZE}px, all distinct):",
          sorted(p.name for p in tray))
