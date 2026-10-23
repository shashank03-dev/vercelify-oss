# Builds the Vercelify icon and mark SVGs. Run from the repo root:
#   python3 docs/brand/build-logo.py
# then render vercelify-icon.svg to cockpit/src-tauri/icons/source.png (1024px)
# and run `npx tauri icon src-tauri/icons/source.png -o src-tauri/icons` in cockpit/.
import math
# Superellipse squircle centred at (cx,cy), half-size a, exponent n.
def squircle(cx, cy, a, n=3.9, steps=240):
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        x = cx + a * math.copysign(abs(c) ** (2 / n), c)
        y = cy + a * math.copysign(abs(s) ** (2 / n), s)
        pts.append(f"{x:.1f} {y:.1f}")
    return "M" + " L".join(pts) + " Z"


def T(x, y, s=824 / 1032 * 1.05):
    # source image (1254px) -> 1024 grid, squircle centre (626,628) -> (512,512)
    return (512 + (x - 626) * s, 512 + (y - 628) * s)

def inset(poly, d):
    """Offset a simple polygon (clockwise in screen coords) inward by d."""
    n = len(poly)
    lines = []
    for i in range(n):
        (x1, y1), (x2, y2) = poly[i], poly[(i + 1) % n]
        dx, dy = x2 - x1, y2 - y1
        L = math.hypot(dx, dy)
        nx, ny = -dy / L, dx / L  # inward normal for clockwise (y down)
        lines.append(((x1 + nx * d, y1 + ny * d), (dx, dy)))
    out = []
    for i in range(n):
        (p1, d1), (p2, d2) = lines[i - 1], lines[i]
        den = d1[0] * d2[1] - d1[1] * d2[0]
        t = ((p2[0] - p1[0]) * d2[1] - (p2[1] - p1[1]) * d2[0]) / den
        out.append((p1[0] + d1[0] * t, p1[1] + d1[1] * t))
    return out

def path(poly):
    return "M" + " L".join(f"{x:.1f} {y:.1f}" for x, y in poly) + " Z"

R = 26
chev_src = [(626, 333), (934, 576), (934, 692), (880, 694), (626, 498), (372, 694), (318, 692), (318, 576)]
base_src = [(352, 718), (900, 718), (1004, 834), (972, 878), (280, 878), (248, 834)]
CHEVRON = path(inset([T(*p) for p in chev_src], 20))
BASE = path(inset([T(*p) for p in base_src], R))
sx, sy = T(500, 799); ex, ey = T(752, 837)
SLOT = f'<rect x="{sx:.1f}" y="{sy:.1f}" width="{ex-sx:.1f}" height="{ey-sy:.1f}" rx="{(ey-sy)/2:.1f}"/>'

def glyph(fill):
    return (f'<g fill="{fill}" stroke="{fill}" stroke-width="{2*R}" stroke-linejoin="round">'
            f'<path d="{CHEVRON}"/><path d="{BASE}"/></g>')

GRAD = '''<linearGradient id="ember" gradientUnits="userSpaceOnUse" x1="400" y1="760" x2="620" y2="250">
      <stop offset="0" stop-color="#FF4A2B"/><stop offset="0.55" stop-color="#FF8A4A"/><stop offset="1" stop-color="#FFD596"/>
    </linearGradient>'''

def glyph_with_slot(fill, slot_fill):
    # Slot is cut out with a mask so the glyph stays transparent there.
    return f'''<mask id="slot"><rect width="1024" height="1024" fill="#fff"/><g fill="#000">{SLOT}</g></mask>
  <g mask="url(#slot)">{glyph(fill)}</g>'''

icon = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#3A3038"/><stop offset="0.38" stop-color="#211A20"/><stop offset="1" stop-color="#121014"/>
    </linearGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#fff" stop-opacity="0.22"/><stop offset="0.3" stop-color="#fff" stop-opacity="0.04"/><stop offset="1" stop-color="#fff" stop-opacity="0.08"/>
    </linearGradient>
    <radialGradient id="glow" cx="512" cy="560" r="360" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#FF6A3D" stop-opacity="0.16"/><stop offset="1" stop-color="#FF6A3D" stop-opacity="0"/>
    </radialGradient>
    {GRAD}
  </defs>
  <rect x="92" y="92" width="840" height="840" rx="196" fill="url(#bg)"/>
  <rect x="92" y="92" width="840" height="840" rx="196" fill="url(#glow)"/>
  <rect x="93.5" y="93.5" width="837" height="837" rx="194.5" fill="none" stroke="url(#rim)" stroke-width="3"/>
  {glyph_with_slot("url(#ember)", None)}
</svg>
'''

mark = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="198 260 628 466">
  <defs>{GRAD}</defs>
  {glyph_with_slot("url(#ember)", None)}
</svg>
'''
open("docs/brand/vercelify-icon.svg", "w").write(icon)
open("docs/brand/vercelify-mark.svg", "w").write(mark)
