# Development

## Prerequisites

- Rust stable (`rustup`), Node 22, `jq`, `shellcheck`, `bats`
- Linux: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev` (for the Tauri crate)
- For the live tests: Docker and a Coolify instance you can throw away

## Everyday commands

```bash
cargo test --workspace --all-features        # Rust unit + fixture + agent black-box tests (also regenerates TS types)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
bats host/tests                              # host scripts
shellcheck -x -S warning install.sh host/*.sh host/vercelify host/lib/*.sh

cd cockpit
npm ci
npm test                                     # vitest
npm run typecheck
npm run tauri dev                            # desktop app with hot reload
```

### Browser mode

The UI also runs in a plain browser: Vite proxies `/__agent` and `/__coolify`,
and settings live in `localStorage`. Point it at a running agent and Coolify:

```bash
VERCELIFY_AGENT_URL=http://127.0.0.1:7420 VERCELIFY_COOLIFY_URL=http://127.0.0.1:8000 npm run dev
```

Desktop-only features (local Tailscale, ffmpeg sender, Moonlight) report
"Available in the desktop app only".

### Demo mode (no host needed)

```bash
cd cockpit && VITE_DEMO=1 npm run dev
```

Every screen gets realistic canned data from `src/lib/demo.ts` (apps,
deployments, databases, disks, Tailscale…), so you can design and screenshot
the UI without a server. Pair with any text, or set
`localStorage["vercelify.settings"]` to skip onboarding. Production builds
never include it.

### UI building blocks

- **Tokens** live in `cockpit/src/index.css` as CSS variables (`--bg`, `--surface`,
  `--fg-muted`, `--accent`, status colours…), with a light set under
  `[data-theme="light"]`. Components use the semantic Tailwind names
  (`bg-surface`, `text-fg-muted`, `border-line`), never raw hex or `white/[…]`.
- **Primitives** are in `cockpit/src/components/ui.tsx`: Button, IconButton (with
  tooltip), Card, Stat, Badge, StatusPill, Tabs, Segmented, Modal (`sheet` for the
  slide-over), Meter, Ring, Sparkline, CodeBlock, LogView, EmptyState, Logo…
- **Navigation** is declared once in `cockpit/src/lib/nav.tsx`; the sidebar, ⌘1–⌘5
  shortcuts and the ⌘K command palette all read from it. Add a page there.
- **Fonts** are Geist and Geist Mono, bundled via `@fontsource-variable` (works
  offline in the desktop app).
- **Brand**: `docs/brand/` holds the icon and mark SVGs and the script that builds them.

### Running the agent locally

```bash
VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_TOKEN=dev-token-0123456789 \
VERCELIFY_REQUIRE_STORAGE_MOUNT=false VERCELIFY_HDD_VOLUME_ROOT=/tmp/hdd \
  cargo run -p vercelify-agent -- --config /nonexistent
curl -H "Authorization: Bearer dev-token-0123456789" localhost:7420/v1/system | jq
```

Every config key has a `VERCELIFY_*` environment override (see `agent/src/config.rs`),
which is how the tests point it at fixture sysfs trees and stub binaries.

## Live end-to-end tests

```bash
# Coolify must be running on this machine (Docker), with an API token:
COOLIFY_TOKEN='1|…' E2E_HDD=/mnt/storage cockpit/e2e/run-live.sh
```

This builds the agent, starts it against the local Docker, serves the
production UI, and runs `cockpit/e2e/live.spec.ts`: pairing, deploying,
build logs, rollback, database provisioning, moving a live Postgres between
tiers (with data verification), health, logs, services, screencast and unpair.
The spec expects a Dockerfile-based app called `gitapp` whose repository has at
least two commits that serve different pages (the test rolls back to the
previous one and checks the live site changes), a Postgres called `pg-main`
with a table `t` containing `survives the move`. Screenshots land in
`test-results/screens/`.

Bootstrapping such a Coolify in a throwaway VM:

```bash
curl -fsSL https://cdn.coollabs.io/coolify/install.sh | sudo env ROOT_USERNAME=admin ROOT_USER_EMAIL=a@b.c ROOT_USER_PASSWORD='Pass-word-123!' bash
docker exec coolify-db psql -U coolify -d coolify -c "update instance_settings set is_api_enabled=true"
docker exec coolify php artisan tinker --execute="session(['currentTeam'=>\App\Models\Team::find(0)]); echo \App\Models\User::find(0)->createToken('e2e',['root'])->plainTextToken;"
```

## Desktop end-to-end tests (real Tauri binary)

Drives the actual desktop app through `tauri-driver` + WebKitWebDriver under
Xvfb, so the Rust command layer runs for real: pairing and 0600 settings,
HTTP to the agent and Coolify, local tool detection, a real ffmpeg test-pattern
stream decoded by the agent's mpv, the orphan watchdog (force-quitting the
app must stop ffmpeg), and settings surviving a restart.

```bash
sudo apt install webkit2gtk-driver xvfb ffmpeg mpv
cargo install tauri-driver --locked
COOLIFY_TOKEN='1|…' cockpit/e2e-desktop/run-desktop.sh
```

Screenshots of every step land in `e2e-logs/desktop/`.

## Installer integration test

Runs the real installer (no dry run) inside a privileged Debian 12 container
with systemd, nested Docker and a loop-device "HDD", then asserts on the
result and runs it a second time to prove idempotency:

```bash
sudo AGENT_BINARY=target/x86_64-unknown-linux-musl/release/vercelify-agent host/tests/integration/run.sh
```

## Coolify contract test

Starts Coolify (the version the installer pins, or `COOLIFY_VERSION=…`) from its
own compose files and checks everything Vercelify relies on: the database
columns, the API routes the cockpit, `vfy` and the agent call, the agent's SQL
against the real database, the proxy edit for sleeping apps, and a full
sleep → visit → wake cycle through Coolify's Traefik:

```bash
cargo build -p vercelify-agent
sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/coolify.sh   # KEEP=1 leaves it running
```

CI runs it (`integration.yml`) when that code changes, weekly, and against
Coolify's newest release as an early warning.

## Project rules contract test (real Traefik)

Runs the real agent against a real Traefik started the way Coolify starts it (Docker
and file providers), with two apps labelled exactly as Coolify 4.4.6 labels them (a
static site with real 404s, and whoami over https behind Coolify's basic auth) and a
second whoami as an external site. Drives everything through the agent's API and
checks redirects, rewrites, headers, the firewall in direct and Cloudflare-tunnel mode
(including a spoofed tunnel header from another address), cron jobs with `CRON_SECRET`,
a stopped app and removal:

```bash
cargo build -p vercelify-agent
sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/routing.sh   # KEEP=1 leaves it running
TRAEFIK_IMAGE=traefik:v3.4 sudo -E host/tests/contract/routing.sh                  # an older proxy
```

Needs Docker, curl, jq and python3; uses ports 18080–18083, 18443 and 17421. Traefik
before 3.6 speaks a Docker API that Docker 29 refuses unless dockerd runs with
`DOCKER_MIN_API_VERSION=1.24`. CI (`integration.yml`) runs it on Traefik 3.7 (a gate)
and 3.4 (an early warning).

## arm64 (Raspberry Pi) under QEMU

```bash
sudo apt-get install qemu-user-static
rustup target add aarch64-unknown-linux-musl
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUNNER=qemu-aarch64-static \
  cargo test -p vercelify-core -p vercelify-agent --target aarch64-unknown-linux-musl
```

CI also runs the bats suite and a Raspberry Pi 5 installer dry run in an
arm64 Debian 12 container (`DEVICE_TREE_MODEL` points the board detection at a
fake `/proc/device-tree/model`).

## Cross-compiling the agent

```bash
pip install cargo-zigbuild
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo zigbuild --release -p vercelify-agent --target aarch64-unknown-linux-musl   # Raspberry Pi 4/5
```

Both produce ~3 MB static binaries; the release workflow publishes them with
`.sha256` files, which `host/lib/agent.sh` verifies.

## Releasing

Tag `vX.Y.Z` → `.github/workflows/release.yml` publishes the agent binaries
and the cockpit bundles (macOS universal `.dmg`, Linux `.deb`/`.AppImage`).
The macOS app is signed and notarised when the repository has these Actions
secrets: `APPLE_CERTIFICATE` (a *Developer ID Application* certificate exported
as `.p12`, base64 on one line: `base64 -i cert.p12 | tr -d '\n'`),
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` (e.g. `Developer ID
Application: Your Name (TEAMID)`), `APPLE_ID`, `APPLE_PASSWORD` (an
app-specific password) and `APPLE_TEAM_ID`. Without them the build is unsigned:
right-click → Open the first time (`install-mac.sh` clears the quarantine flag
itself).
