# Builds the README architecture diagram in light and dark versions. Run from the repo root:
#   python3 docs/diagrams/build-architecture.py
# Keep it in step with docs/ARCHITECTURE.md when components or ports change.
from html import escape

W, H = 1200, 740
SANS = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"
MONO = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"

THEMES = {
    "light": {
        "bg": "#fbfaf8", "card": "#ffffff", "inner": "#f6f5f2", "band": "#f1efea",
        "line": "#e2dfd8", "strong": "#c9c5bc", "fg": "#1c1b19", "muted": "#6b6862",
        "accent": "#e8572a", "accent_fill": "#fff1ea", "accent_line": "#f6b79e",
        "green": "#1f9d55", "pill": "#efede8",
    },
    "dark": {
        "bg": "#0f0f0e", "card": "#171716", "inner": "#1d1d1b", "band": "#131312",
        "line": "#2c2b28", "strong": "#45433e", "fg": "#ecebe7", "muted": "#8f8c85",
        "accent": "#ff8a5c", "accent_fill": "#2a1810", "accent_line": "#6b3520",
        "green": "#5bd47e", "pill": "#252522",
    },
}


def render(t):
    out = []
    add = out.append

    def text(x, y, s, size=13, color="fg", weight=400, family=SANS, anchor="start"):
        add(f'<text x="{x}" y="{y}" font-family="{family}" font-size="{size}" font-weight="{weight}" '
            f'fill="{t[color]}" text-anchor="{anchor}">{escape(s)}</text>')

    def rect(x, y, w, h, fill, stroke, r=12, dashed=False, sw=1):
        dash = ' stroke-dasharray="5 5"' if dashed else ""
        add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{t[fill]}" stroke="{t[stroke]}" stroke-width="{sw}"{dash}/>')

    def port(x_right, y, label):
        w = len(label) * 7.2 + 14
        rect(x_right - w, y - 15, w, 21, "pill", "pill", r=10.5)
        text(x_right - w / 2, y, label, 11.5, "muted", 500, MONO, "middle")

    def box(x, y, w, h, title, sub, p=None, accent=False, dashed=False, tag=None):
        if accent:
            rect(x, y, w, h, "accent_fill", "accent_line", r=10, sw=1.2)
        else:
            rect(x, y, w, h, "inner", "line", r=10, dashed=dashed)
        text(x + 16, y + 26, title, 14.5, "accent" if accent else "fg", 600)
        if tag:
            tx = x + 22 + len(title) * 8.2
            text(tx, y + 26, tag, 11.5, "muted", 500)
        text(x + 16, y + 46, sub, 12.5, "muted")
        if p:
            port(x + w - 12, y + 25, p)

    def arrow(x1, y1, x2, y2, color="strong", dashed=False):
        dash = ' stroke-dasharray="4 4"' if dashed else ""
        add(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{t[color]}" stroke-width="1.6"{dash} marker-end="url(#a-{color})"/>')

    def card(x, y, w, h, eyebrow, title):
        rect(x, y, w, h, "card", "line", r=16)
        text(x + 24, y + 30, eyebrow, 11, "muted", 600)
        text(x + 24, y + 52, title, 17, "fg", 650)

    add(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" '
        f'aria-label="Vercelify architecture: the Mac app talks to the old laptop over Vercelify’s own WireGuard network; the laptop runs the Vercelify agent with its engine, Docker, Traefik and Ollama.">')
    add("<title>Vercelify architecture</title>")
    add("<defs>" + "".join(
        f'<marker id="a-{c}" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
        f'<path d="M0 0L10 5L0 10z" fill="{t[c]}"/></marker>' for c in ("strong", "accent")) + "</defs>")
    rect(0, 0, W, H, "bg", "bg", r=20)

    # Top: the Git host the agent polls.
    box(524, 20, 300, 60, "GitHub", "or any Git host")
    arrow(674, 222, 674, 84, "strong")
    text(686, 118, "checks for new commits", 12, "muted")
    text(686, 134, "every 60 s, outbound only", 12, "muted")

    # Left: the Mac.
    card(40, 150, 300, 466, "YOUR MAC", "Control centre")
    box(64, 222, 252, 76, "Vercelify app", "Desktop app · Tauri + React", accent=True)
    text(80, 288, "deploy · logs · Explain with AI", 11.5, "muted")
    box(64, 318, 252, 64, "vfy", "Command-line tool")
    box(64, 402, 252, 64, "Ollama", "AI for vfy why --log", tag="optional", dashed=True)
    box(64, 486, 252, 64, "Gemini API", "Hosted Gemma 4", tag="optional", dashed=True)

    # Middle: Vercelify's own network (vercelify-mesh).
    rect(352, 150, 136, 466, "band", "line", r=16, dashed=True)
    text(420, 186, "vercelify-mesh", 14, "fg", 650, anchor="middle")
    text(420, 204, "WireGuard network", 11.5, "muted", anchor="middle")
    text(420, 244, "agent API", 11.5, "accent", 600, anchor="middle")
    text(432, 326, "engine API", 11.5, "muted", 600, anchor="middle")
    text(420, 566, "encrypted", 11.5, "muted", anchor="middle")
    text(420, 582, "no open ports", 11.5, "muted", anchor="middle")
    add(f'<circle cx="420" cy="544" r="5" fill="{t["green"]}"/>')

    # Right: the server. Connections from the Mac are drawn after the card so
    # their arrowheads stay on top.
    card(500, 150, 660, 466, "OLD LAPTOP", "Main server · Ubuntu")
    arrow(316, 251, 520, 251, "accent")
    add(f'<path d="M316 282H372V335H520" fill="none" stroke="{t["strong"]}" stroke-width="1.6" marker-end="url(#a-strong)"/>')
    box(524, 222, 612, 58, "Vercelify agent", "Health, disks, pairing, the network's coordinator and AI requests", p=":7420", accent=True)
    arrow(674, 280, 674, 304)
    arrow(986, 280, 986, 304)
    box(524, 306, 300, 58, "Engine", "Builds, deploys, previews, databases", p="in the agent")
    box(836, 306, 300, 58, "Ollama", "Gemma 4 runs here, locally", p=":11434", tag="optional")
    arrow(674, 364, 674, 388)

    # Docker: the apps themselves.
    rect(524, 390, 612, 116, "inner", "line", r=10)
    text(540, 416, "Docker", 14.5, "fg", 600)
    text(600, 416, "each app runs in its own container", 12.5, "muted")
    chips = [("blog", False), ("notes-api", False), ("PR #12 preview", False), ("PostgreSQL", True), ("Redis", True)]
    cx = 540
    for label, db in chips:
        w = len(label) * 7.4 + 28
        rect(cx, 434, w, 30, "card", "line", r=15)
        add(f'<circle cx="{cx + 13}" cy="449" r="3.5" fill="{t["muted"] if db else t["green"]}"/>')
        text(cx + 22, 453, label, 12.5, "fg", 500)
        cx += w + 10
    text(540, 490, "apps", 11.5, "muted")
    text(cx - 187, 490, "databases", 11.5, "muted")

    arrow(674, 530, 674, 508)
    box(524, 532, 300, 58, "Traefik", "Routes web traffic to apps", p=":80 :443")
    box(836, 532, 300, 58, "Guard + firewall", "Battery, heat, private ports")

    # Bottom: optional public access and workers.
    box(160, 662, 250, 56, "Visitors", "Anyone on the internet")
    arrow(410, 690, 518, 690, "strong", dashed=True)
    box(524, 662, 300, 56, "Cloudflare Tunnel", "Public HTTPS for your domain", tag="optional", dashed=True)
    arrow(674, 662, 674, 594, "strong", dashed=True)
    box(836, 662, 300, 56, "Raspberry Pi worker", "Extra room for apps", tag="optional", dashed=True)
    arrow(986, 662, 986, 620, "strong", dashed=True)

    add("</svg>")
    return "\n".join(out) + "\n"


for name, theme in THEMES.items():
    path = f"docs/diagrams/architecture-{name}.svg"
    with open(path, "w", encoding="utf-8") as f:
        f.write(render(theme))
    print(path)
