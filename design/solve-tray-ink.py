#!/usr/bin/env python3
"""Show the working behind the light-mode tray ink, layer by layer.

`generate-icons.py` derives the light variant by remapping the whole alpha
channel through one monotonic curve (`tray_alpha_map`), which is the right shape
for artwork whose opacities have already been composited and antialiased into
8-bit alpha. This script is the same calculation restricted to the discrete
opacities the designer's SVGs actually name, which is the form a human can check:
it prints, per layer, what the white version achieves on a dark menu bar and what
the dark version achieves on a light one.

Run it to sanity-check `DARK_INK`, or after the designer changes a source
opacity, and confirm the deviations stay small.

The question it answers: the designer drew the tray glyph as `white` at opacities
0.22-0.66, which sits on a dark menu bar. For a light menu bar the ink has to
change. What ink, at what opacities?

The method: hold *perceived contrast against the menu bar* constant. For each
source opacity, compute the WCAG contrast ratio the white version achieves
against macOS's dark menu bar (#1c1c1e), then solve for the opacity at which the
dark ink achieves that same ratio against the light menu bar (#f2f2f7). That
keeps the three-layer stack reading as three layers, with the same separation
between them, on both backgrounds.

The result is counter-intuitive and worth stating plainly, because the intuition
points the wrong way: the opacities go UP, not down. Alpha compositing is not
symmetric about the midpoint. White at 0.38 over near-black multiplies relative
luminance by ~14; black at 0.38 over near-white divides it by under 3. At equal
opacity the dark-on-light variant is the fainter one. So the ink is pure black
(the strongest available) and the opacities are all raised.

A single multiplier will not do it either: contrast is non-linear in alpha, so
the 1.25x that lands the lower fill within 1% overshoots the upper fill by 60%.
Hence a per-opacity solve.
"""

# macOS's two menu bar backgrounds, sampled from the system appearance.
DARK_BG = (0x1C, 0x1C, 0x1E)
LIGHT_BG = (0xF2, 0xF2, 0xF7)

WHITE = (0xFF, 0xFF, 0xFF)
INK = (0x00, 0x00, 0x00)

# Every distinct opacity in the designer's sources, with where it is used. Keep
# this in step with `design/tray-source/*.svg`.
SOURCE_OPACITIES = [
    (0.38, "fill, lower layer"),
    (0.52, "fill, middle layer"),
    (0.66, "fill, upper layer"),
    (0.22, "stroke, lower layer"),
    (0.28, "stroke, middle layer; disconnected dot"),
    (0.34, "stroke, upper layer"),
    (0.35, "connecting ring"),
]

# The error dot. Not solved for -- it is kept as-is in both variants -- but
# checked here, because "legible on both backgrounds" is the claim that lets it
# stay unchanged, and a claim in a comment should be a number somewhere.
ERROR_RED = (0xFF, 0x45, 0x3A)


def _linear(channel: int) -> float:
    """sRGB channel (0-255) to linear light, per WCAG 2.x."""
    c = channel / 255.0
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def luminance(rgb: tuple) -> float:
    r, g, b = (_linear(c) for c in rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def over(ink: tuple, alpha: float, bg: tuple) -> tuple:
    """Composite `ink` at `alpha` over an opaque `bg`."""
    return tuple(ink[i] * alpha + bg[i] * (1 - alpha) for i in range(3))


def contrast(a: tuple, b: tuple) -> float:
    ya, yb = luminance(a), luminance(b)
    hi, lo = max(ya, yb), min(ya, yb)
    return (hi + 0.05) / (lo + 0.05)


def solve(target: float, ink: tuple, bg: tuple) -> float:
    """The alpha at which `ink` over `bg` hits `target` contrast.

    Bisection rather than a closed form: the WCAG ratio composes a piecewise
    gamma curve with a linear blend, and 40 iterations over a monotonic function
    on [0, 1] is exact well past the two decimal places an SVG attribute gets.
    """
    lo, hi = 0.0, 1.0
    for _ in range(40):
        mid = (lo + hi) / 2
        if contrast(over(ink, mid, bg), bg) < target:
            lo = mid
        else:
            hi = mid
    return (lo + hi) / 2


if __name__ == "__main__":
    print(f"dark menu bar  #{'%02X%02X%02X' % DARK_BG}")
    print(f"light menu bar #{'%02X%02X%02X' % LIGHT_BG}")
    print(f"ink            #{'%02X%02X%02X' % INK}\n")

    header = f"{'source α':>9s} {'target':>7s} {'solved':>7s} {'2dp':>5s} {'actual':>7s} {'dev':>6s}"
    print(header)
    print("-" * len(header))

    table = {}
    worst = 0.0
    for alpha, where in sorted(SOURCE_OPACITIES):
        target = contrast(over(WHITE, alpha, DARK_BG), DARK_BG)
        exact = solve(target, INK, LIGHT_BG)
        rounded = round(exact, 2)
        actual = contrast(over(INK, rounded, LIGHT_BG), LIGHT_BG)
        deviation = abs(actual - target) / target
        worst = max(worst, deviation)
        table[f"{alpha:g}"] = f"{rounded:.2f}"
        print(f"{alpha:9.2f} {target:7.2f} {exact:7.3f} {rounded:5.2f} "
              f"{actual:7.2f} {deviation * 100:5.1f}%   {where}")

    print(f"\nworst deviation from the white-on-dark reference: {worst * 100:.1f}%")
    print("\nper-layer alphas (the generator applies the same solve to all 256):")
    for source, mapped in table.items():
        print(f"    {source} -> {mapped}")

    print(f"\nerror dot #{'%02X%02X%02X' % ERROR_RED} stays red in both variants:")
    print(f"  vs dark menu bar  {contrast(ERROR_RED, DARK_BG):.2f}")
    print(f"  vs light menu bar {contrast(ERROR_RED, LIGHT_BG):.2f}")
