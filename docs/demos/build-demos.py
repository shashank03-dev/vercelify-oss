# Builds the animated terminal demos used in the README. Run from the repo root:
#   python3 docs/demos/build-demos.py
# Each demo replays what the real script prints (abridged), so update the text
# here when install.sh, install-mac.sh or `vfy why` change their output.
from html import escape

FONT = "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace"
SIZE = 12.5
CHAR = SIZE * 0.6  # monospace advance, close enough for Menlo/DejaVu/Consolas
LINE = 19
PAD_X, PAD_TOP, BAR = 18, 16, 34
COLORS = {
    "fg": "#d8d6d0", "dim": "#8a8780", "green": "#5bd47e", "cyan": "#6cc9e0",
    "yellow": "#f0bf4c", "red": "#f07a6a", "ember": "#ff8a5c", "bold": "#f4f2ee",
}
BG, CHROME = "#0f0f0e", "#1c1c1a"
WEIGHT = ' font-weight="600"'
HOLD = 5.0  # seconds the final screen stays up before the loop restarts


# Line helpers. A line is a list of (text, colour) segments plus how it appears.
def out(*segs, gap=0.22):
    return {"segs": [s if isinstance(s, tuple) else (s, "fg") for s in segs], "gap": gap}


def cmd(text, gap=0.6):
    return {"segs": [("$ ", "ember"), (text, "bold")], "typed": len(text), "gap": gap}


def answer(prompt, reply, gap=0.5):
    """An installer question followed by what the user types."""
    return {"segs": [("  ? ", "yellow"), (prompt + " ", "fg"), (reply, "bold")],
            "typed": len(reply), "typed_from": len(prompt) + 5, "gap": gap}


def blank(gap=0.05):
    return {"segs": [], "gap": gap}


CLEAR = "clear"


def step(title, gap=0.45):
    return out(("==> ", "cyan"), (title, "bold"), gap=gap)


def ok(text, gap=0.22):
    return out(("  ✓ ", "green"), text, gap=gap)


def info(text, gap=0.22):
    return out(("  • ", "cyan"), text, gap=gap)


def warn(text, gap=0.22):
    return out(("  ! ", "yellow"), text, gap=gap)


def build(name, title, lines, width=1040):
    # Split into screens; each screen replaces the previous one, like `clear`.
    screens, cur = [], []
    for ln in lines:
        if ln == CLEAR:
            screens.append(cur)
            cur = []
        else:
            cur.append(ln)
    screens.append(cur)
    rows = max(len(s) for s in screens)
    height = BAR + PAD_TOP + rows * LINE + PAD_TOP

    # Timeline: when each line appears, when typing ends, when its screen ends.
    t, timed = 0.6, []
    for si, screen in enumerate(screens):
        start_idx = len(timed)
        for row, ln in enumerate(screen):
            typing = min(ln.get("typed", 0) * 0.045, 2.4)
            timed.append({**ln, "row": row, "t0": t, "t1": t + typing, "screen": si})
            t += typing + ln["gap"]
        for item in timed[start_idx:]:
            item["end"] = None
        if si < len(screens) - 1:
            t += 0.8
            for item in timed[start_idx:]:
                item["end"] = t
    total = t + HOLD
    pct = lambda s: f"{min(s / total * 100, 100):.3f}"

    css, body = [], []
    for i, ln in enumerate(timed):
        end = ln["end"] if ln["end"] is not None else total - 0.4
        css.append(
            f"@keyframes v{i}{{0%,{pct(ln['t0'])}%{{opacity:0}}"
            f"{pct(ln['t0'] + 0.01)}%,{pct(end - 0.01)}%{{opacity:1}}{pct(end)}%,100%{{opacity:0}}}}"
            f".v{i}{{animation:v{i} {total:.2f}s linear infinite}}"
        )
        y = BAR + PAD_TOP + ln["row"] * LINE + SIZE
        spans = "".join(
            f'<tspan fill="{COLORS[c]}"{WEIGHT if c == "bold" else ""}>{escape(txt)}</tspan>'
            for txt, c in ln["segs"]
        )
        g = [f'<text x="{PAD_X}" y="{y:.1f}">{spans}</text>']
        if ln.get("typed"):
            # A background-coloured cover slides right to "type" the text.
            n = ln["typed"]
            start_col = ln.get("typed_from", 2)
            x0 = PAD_X + start_col * CHAR
            w = n * CHAR + 4
            css.append(
                f"@keyframes c{i}{{0%,{pct(ln['t0'])}%{{transform:translateX(0);animation-timing-function:steps({n},end)}}"
                f"{pct(ln['t1'])}%,100%{{transform:translateX({w:.1f}px)}}}}"
                f".c{i}{{animation:c{i} {total:.2f}s linear infinite}}"
                # The cursor only shows while this line is being typed.
                f"@keyframes k{i}{{0%,{pct(ln['t0'])}%{{opacity:0}}{pct(ln['t0'] + 0.01)}%,{pct(ln['t1'] + 0.4)}%{{opacity:.75}}"
                f"{pct(ln['t1'] + 0.41)}%,100%{{opacity:0}}}}.k{i}{{animation:k{i} {total:.2f}s linear infinite}}"
            )
            g.append(
                f'<g class="c{i}"><rect x="{x0 - 1:.1f}" y="{y - SIZE:.1f}" width="{w + 12:.1f}" height="{LINE}" fill="{BG}"/>'
                f'<rect x="{x0 - 1:.1f}" y="{y - SIZE + 1:.1f}" width="{CHAR:.1f}" height="{SIZE + 3}" fill="{COLORS["ember"]}" class="k{i}"/></g>'
            )
        body.append(f'<g class="v{i}">{"".join(g)}</g>')

    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="{escape(title)}">
<title>{escape(title)}</title>
<style>text{{font-family:{FONT};font-size:{SIZE}px;white-space:pre}}{"".join(css)}</style>
<rect width="{width}" height="{height}" rx="10" fill="{BG}"/>
<path d="M0 10a10 10 0 0 1 10-10h{width - 20}a10 10 0 0 1 10 10v{BAR - 10}H0z" fill="{CHROME}"/>
<circle cx="20" cy="17" r="5.5" fill="#ff5f57"/><circle cx="38" cy="17" r="5.5" fill="#febc2e"/><circle cx="56" cy="17" r="5.5" fill="#28c840"/>
<text x="{width / 2}" y="21" text-anchor="middle" fill="{COLORS['dim']}" style="font-size:12px">{escape(title)}</text>
<g xml:space="preserve">{"".join(body)}</g>
</svg>
"""
    with open(f"docs/demos/{name}.svg", "w", encoding="utf-8") as f:
        f.write(svg)
    print(f"docs/demos/{name}.svg  {total:.1f}s loop, {len(svg) // 1024} KB")


REPO = "https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main"
HOST = "old-laptop.tail1234.ts.net"
IP = "100.101.102.103"

build("install-server", "you@old-laptop: Ubuntu 24.04 (the server)", [
    cmd(f"curl -fsSL {REPO}/install.sh | sudo bash -s -- --storage-device none"),
    blank(),
    step("Preflight"),
    ok("Ubuntu 24.04.1 LTS"),
    ok("hardware profile: laptop (Latitude E7450)"),
    ok("architecture amd64"),
    ok("7842 MB RAM, 187 GB free on /"),
    ok("internet access", gap=0.6),
    blank(),
    out(("  Vercelify 0.2.0 — self-hosted Vercel on this machine", "cyan")),
    out(("    Role: main · Host: old-laptop · Answer a few questions (Enter = default).", "dim"), gap=0.5),
    blank(),
    answer("Set up local AI models too? [y/N]", "n"),
    answer("E-mail for your Coolify login (any real address): [admin@example.com]", "you@example.com"),
    answer("Your domain (blank = private only, add one later with 'vercelify domain'):", "", gap=0.9),
    CLEAR,
    step("Plan"),
    info("role:       main"),
    info("hostname:   old-laptop (Tailscale MagicDNS)"),
    info("storage:    none → /mnt/storage"),
    info("domain:     none — apps private to your tailnet via sslip.io", gap=0.5),
    step("Base packages"),
    step("Laptop server mode"),
    step("Kernel & memory tuning"),
    step("Hardware (laptop: Latitude E7450)"),
    step("Storage tier (/mnt/storage)"),
    step("Docker Engine", gap=0.7),
    step("Tailscale mesh VPN"),
    info("Open the link below on any device to add this machine to your tailnet:"),
    out(("      https://login.tailscale.com/a/1a2b3c4d5e6f", "cyan"), gap=1.6),
    ok(f"connected as {HOST} ({IP})", gap=0.9),
    CLEAR,
    step("Coolify (PaaS engine)", gap=1.0),
    step("Coolify defaults"),
    step("Vercelify agent"),
    step("Power & thermal guard"),
    step("Firewall"),
    step("Reboot window"),
    step("Backups", gap=0.9),
    CLEAR,
    out(("✔ Vercelify is ready", "green"), ("  (main, 0.2.0)", "fg"), gap=0.4),
    blank(),
    out(("  Coolify dashboard  ", "bold"), f"http://{HOST}:8000"),
    out(("  Login              ", "bold"), "you@example.com / Vf-3fa9c1-8B2E0D!9"),
    out(("  Apps               ", "bold"), f"http://<app>.{IP}.sslip.io"),
    out(("  Tailscale          ", "bold"), f"{HOST} ({IP})"),
    out(("  Storage            ", "bold"), "single disk (/mnt/storage on /)"),
    blank(),
    out(("  Connect your Mac", "bold")),
    out("    1. Install the app (and Tailscale):"),
    out(f"         curl -fsSL {REPO}/install-mac.sh | bash"),
    out("    2. Open Vercelify, pick ", ("old-laptop", "bold"), " and type the code   ",
        ("K7QF-M2XD", "ember"), "   (valid 1 hour)", gap=0.4),
    blank(),
    out("  New code any time: sudo vercelify pair"),
    blank(),
    out(("  Health check: sudo vercelify doctor     Logs: /var/log/vercelify-setup.log", "dim")),
])

build("install-mac", "you@macbook: Terminal (your Mac)", [
    cmd(f"curl -fsSL {REPO}/install-mac.sh | bash"),
    blank(),
    out(("  Vercelify for Mac", "cyan"), " — your own Vercel on your old laptops"),
    blank(),
    step("Tailscale (private network to your servers)"),
    info("installing Tailscale with Homebrew…", gap=1.2),
    ok("Tailscale installed — open it from the menu bar and sign in", gap=0.6),
    blank(),
    step("Vercelify app"),
    warn("no downloadable release of shashank03-dev/vercelify-oss yet"),
    answer("Build the app from source now (needs Xcode CLT, Node 20+ and Rust)? [y/N]", "y", gap=0.6),
    blank(),
    step("Building Vercelify from source (one time, 5–15 minutes)"),
    out(("  … npm ci, then tauri build (output shortened) …", "dim"), gap=1.8),
    ok("installed /Applications/Vercelify.app"),
    ok("vfy installed in /usr/local/bin (try: vfy apps)", gap=0.6),
    blank(),
    out(("✔ Done.", "green"), " Opening Vercelify…"),
    blank(),
    out("  Next: on your old laptop run the server installer, then pick it in the app"),
    out("  and type the 8-character code it shows."),
])

build("vfy-why", "you@macbook: vercelify-oss", [
    cmd("./cli/vfy why --log cli/examples/failed-build.log --provider gemini", gap=1.4),
    out(("**Cause:**", "ember"), " The `next build` step failed because `./app/page.tsx` attempts to import a component"),
    out("using a case-sensitive path that does not match the actual filename on the Linux filesystem.", gap=0.5),
    blank(),
    out(("**Fix:**", "ember"), " Rename the import in `app/page.tsx` to match the exact casing of the file in the"),
    out("`components/` directory (e.g., change `../components/Header` to `../components/header` if"),
    out("the file is lowercase).", gap=0.5),
    blank(),
    out(("**Check:**", "ember"), " `ls components/` to verify the exact casing of the file, then `npm run build` to"),
    out("test the import locally.", gap=0.4),
    blank(),
    out(("— gemma-4-26b-a4b-it via the Gemini API (Gemma 4, Apache 2.0)", "dim")),
], width=860)
