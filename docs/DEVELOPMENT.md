# Development

## Prerequisites

- Rust stable (`rustup`), Node 22, `jq`, `shellcheck`, `bats`
- Linux: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev` (for the Tauri crate)
- For the live and contract tests: Docker (and root, for the network namespace ones)

## Everyday commands

```bash
cargo test --workspace --all-features        # Rust unit + fixture + agent black-box tests (also regenerates TS types)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
bats host/tests                              # host scripts
shellcheck -x -S warning install.sh install-mac.sh cli/vfy host/*.sh host/vercelify host/lib/*.sh host/tests/contract/*.sh
cargo test -p vercelify-platform -p vercelify-mesh --features vercelify-mesh/daemon   # the engine and the mesh alone

cd cockpit
npm ci
npm test                                     # vitest
npm run typecheck
npm run tauri dev                            # desktop app with hot reload
```

### Browser mode

The UI also runs in a plain browser: Vite proxies `/__agent`, and settings live in
`localStorage`. Point it at a running agent:

```bash
VERCELIFY_AGENT_URL=http://127.0.0.1:7420 npm run dev
```

Desktop-only features (the network daemon, ffmpeg sender, Moonlight) report
"Available in the desktop app only".

### Demo mode (no host needed)

```bash
cd cockpit && VITE_DEMO=1 npm run dev
```

Every screen gets realistic canned data from `src/lib/demo.ts` (apps,
deployments, databases, disks, the network…), so you can design and screenshot
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
VERCELIFY_LISTEN=127.0.0.1 VERCELIFY_TOKEN=dev-token-0123456789 VERCELIFY_STATE_DIR=/tmp/vfy \
VERCELIFY_REQUIRE_STORAGE_MOUNT=false VERCELIFY_HDD_VOLUME_ROOT=/tmp/hdd \
VERCELIFY_PROXY_HTTP_PORT=8080 VERCELIFY_PROXY_HTTPS_PORT=8443 \
  cargo run -p vercelify-agent -- --config /nonexistent
curl -H "Authorization: Bearer dev-token-0123456789" localhost:7420/v1/system | jq
```

Every config key has a `VERCELIFY_*` environment override (see `agent/src/config.rs`),
which is how the tests point it at fixture sysfs trees and stub binaries.

## Live end-to-end tests

```bash
cockpit/e2e/run-live.sh          # as root, a loop-mounted ext4 "HDD" makes the tier moves real
```

This builds the agent, starts it with the engine and its own proxy on spare ports
against the local Docker (a fresh engine database each run), serves the production UI,
and runs `cockpit/e2e/live.spec.ts`: pairing, creating and deploying an image app,
the build log, serving it through the proxy, redeploying and rolling back, runtime
logs, a one-click Redis with a backup (into a restic repository the script sets up),
moving a live database between SSD and HDD with its data checked, health, container
logs, services, the screencast receiver and unpairing. Screenshots land in
`test-results/screens/`.

## Desktop end-to-end tests (real Tauri binary)

Drives the actual desktop app through `tauri-driver` + WebKitWebDriver under
Xvfb, so the Rust command layer runs for real: pairing and 0600 settings,
HTTP to the agent, local tool detection, a real ffmpeg test-pattern
stream decoded by the agent's mpv, the orphan watchdog (force-quitting the
app must stop ffmpeg), and settings surviving a restart.

```bash
sudo apt install webkit2gtk-driver xvfb ffmpeg mpv
cargo install tauri-driver --locked
cockpit/e2e-desktop/run-desktop.sh
```

Screenshots of every step land in `e2e-logs/desktop/`.

## Installer integration test

Runs the real installer (no dry run) inside a privileged Debian 12 container
with systemd, nested Docker and a loop-device "HDD", then asserts on the
result (the engine, its proxy, the mesh with this server as `10.77.0.1`, an image app
deployed and served) and runs it a second time to prove idempotency:

```bash
rel=target/x86_64-unknown-linux-musl/release
sudo AGENT_BINARY=$rel/vercelify-agent MESH_BINARY=$rel/vercelify-mesh host/tests/integration/run.sh
```

## Project rules contract test (the real proxy)

Runs the real agent with the engine and its proxy, deploys two apps as image apps (a
static site with real 404s built from `routing_app.mjs`, and whoami over https behind
basic auth) plus a whoami on the host as an external site. Drives everything through
the agent's API and checks redirects, rewrites, headers, the firewall in direct and
Cloudflare-tunnel mode (including a spoofed tunnel header from another address), cron
jobs with `CRON_SECRET`, a stopped app and removal:

```bash
cargo build -p vercelify-agent
sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/routing.sh   # KEEP=1 leaves it running
TRAEFIK_IMAGE=traefik:v3.4 sudo -E host/tests/contract/routing.sh                  # an older proxy
```

Needs Docker, curl and jq; uses ports 18080, 18083, 18084, 18443 and 17421.
`TRAEFIK_IMAGE` becomes the proxy's image (`VERCELIFY_PROXY_IMAGE`). CI
(`integration.yml`) runs it on the pinned Traefik (a gate) and 3.4 (an early warning).

## The mesh and several servers

```bash
cargo build -p vercelify-agent && cargo build -p vercelify-mesh --features daemon
sudo host/tests/contract/mesh.sh          # four nodes in network namespaces: joins, paths, DNS, firewall, revocation
sudo host/tests/contract/mesh-nat.sh      # behind real NAT routers: hole punching, the relay
sudo host/tests/contract/multinode.sh     # a worker with its own Docker: placement, registry, replicas, logs
sudo host/tests/contract/promote.sh       # the warm standby promoted after the main server dies
```

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

`-p vercelify-mesh --features daemon` builds the network daemon the same way. They are
static binaries (the agent at most 8 MiB, `vercelify-mesh` 6 MiB, both gated in CI);
the release workflow publishes them with `.sha256` files, which `host/lib/agent.sh`
verifies.

## Releasing

Tag `vX.Y.Z` → `.github/workflows/release.yml` publishes the agent and
`vercelify-mesh` binaries (Linux x86_64 and arm64, and a universal macOS
`vercelify-mesh`) and the cockpit bundles (macOS universal `.dmg`, Linux `.deb`/`.AppImage`).
The macOS app is signed and notarised when the repository has these Actions
secrets: `APPLE_CERTIFICATE` (a *Developer ID Application* certificate exported
as `.p12`, base64 on one line: `base64 -i cert.p12 | tr -d '\n'`),
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` (e.g. `Developer ID
Application: Your Name (TEAMID)`), `APPLE_ID`, `APPLE_PASSWORD` (an
app-specific password) and `APPLE_TEAM_ID`. Without them the build is unsigned:
right-click → Open the first time (`install-mac.sh` clears the quarantine flag
itself).
