# Development

## Prerequisites

- Rust stable (`rustup`), Node 22, `jq`, `shellcheck`, `bats`
- Linux: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev` (for the Tauri crate)

On macOS, `./dev setup` installs what's missing with Homebrew. On Debian and
Ubuntu it installs everything itself: the packages above, Docker (and adds you
to the `docker` group), Node 22 (22.19 or later) and Rust when they're missing or too old, and
zig with `cargo-zigbuild` from PyPI, as CI does. Node and zig go under
`~/.local`, so add `~/.local/bin` and `~/.cargo/bin` to your `PATH`. Other
distributions get the list to install themselves.
- For the live and contract tests: Docker (and root, for the network namespace ones)

## The `./dev` script

Everything below is wrapped by `./dev` at the repo root, so you can work,
check and ship from your own machine instead of waiting for CI:

```bash
./dev setup                # once: tools, Rust targets, npm deps, Playwright, pre-push hook
./dev ui                   # cockpit in the browser with demo data (http://localhost:1420)
./dev app                  # desktop app with hot reload
./dev agent                # local agent on :7420 (token dev-token-0123456789)

./dev check                # what CI runs, for the parts your branch changed
./dev check all --full     # + Playwright, installer dry run, agent binaries
./dev fix                  # cargo fmt + regenerate the TS types

./dev build --install      # production Vercelify.app (+ .dmg) into /Applications
./dev host me@old-laptop   # this checkout on a real host, rolled back if the agent fails
./dev release 0.3.0        # bump versions, full check, commit and tag v0.3.0
```

A typical loop:

1. `git switch -c my-change`, then `./dev ui` (or `./dev app`) while you work.
2. `./dev check` until it's green. The pre-push hook runs it again on every
   `git push` (`git push --no-verify` skips it once; `./dev hooks off` removes it).
3. Try it for real: `./dev build --install` for the app, `./dev host USER@HOST`
   for the agent and host scripts on a test machine (`sudo vercelify rollback`
   there undoes it).
4. Push the branch and open a PR. CI runs the same checks again, on Linux.
5. To release: `./dev release X.Y.Z` on `main`, then `git push origin main vX.Y.Z`.
   GitHub builds the signed `.dmg`, `.deb`/`.AppImage` and agent binaries.

### Working on a Mac and a Linux machine

Each machine has its own clone and runs `./dev setup` once; git keeps them in
step (push a branch on one, pull it on the other). On Linux, `./dev check` runs
everything natively, including the agent, platform and mesh tests that a Mac
runs in a VM.

A Linux machine can also be a Vercelify server. Develop there as usual, but
the dev agent (`./dev agent`) and the suites that drive Docker with an engine of
their own (the live e2e, `routing.sh`, `multinode.sh`, `promote.sh` and the
`VERCELIFY_DOCKER_TESTS` Rust tests) refuse to run on it: they replace the
`vercelify-proxy` container and would remove the server's app containers as
strays. Use `./dev host USER@HOST` to try your changes on a server, or set
`VERCELIFY_TESTS_ON_SERVER=1` on a server you don't mind breaking.

On macOS the host tests (bats) run in a Debian 12 container, so Docker (or
Colima, started automatically) must be installed. The agent, platform and mesh
tests need a real Linux machine: with Colima they run inside its VM (Rust is
installed there on the first run, ~15 min once); with another Docker they run
in a container and skip the one test that needs a real root disk. `./dev setup`
installs `zig` for the static agent and mesh binaries, a large one-time
Homebrew download.

### A real host to test against

Any spare machine (or a VM) works. To try the whole path on the Mac itself,
create a Debian 12 VM with Lima and install this checkout on it:

```bash
limactl start --name vfy-host --cpus 4 --memory 6 template://debian-12
./dev host lima-vfy-host     # lima-NAME uses the VM's own ssh config
```

`./dev host` builds the agent and mesh for the machine's architecture, copies
them with the host scripts and runs the installer (or `vercelify update` when
it is already installed, which rolls back on its own if the agent doesn't come
up). Lima forwards the VM's ports to the Mac's `127.0.0.1`.

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

The C code in the build (`ring`, the bundled SQLite) needs a C compiler for
arm64 musl: CI uses `zig cc` through a small wrapper (see the arm64 job in
`.github/workflows/ci.yml`; it drops cc-rs's `--target=` flag, which zig can't
read, and turns UBSan off). On an Apple Silicon Mac `./dev check` already runs
these tests on arm64 Linux, in Colima's VM.

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
app-specific password) and `APPLE_TEAM_ID`. Without them the app isn't
notarised: right-click → Open the first time (`install-mac.sh` clears the
quarantine flag itself). It is still signed (`.github/scripts/seal-macos-app.sh`)
with the self-signed certificate in `MACOS_SELF_SIGNED_P12` (base64 `.p12`,
password in `MACOS_SELF_SIGNED_PASSWORD`), so macOS keeps Screen Recording
allowed across updates; without that secret it is signed ad hoc and the
permission must be given again after each update. Keep using the same
certificate: a new one means everyone allows Screen Recording once more.

### How a release reaches people

- **Servers:** "Update all" on the Servers page (or `sudo vercelify update` on
  the server) installs the newest release. The previous agent, scripts and
  `agent.toml` are kept: if the new agent doesn't answer within a minute the
  update undoes itself, and `sudo vercelify rollback` undoes it later. Server
  cards show `0.2.5 → 0.3.0` when a newer release is out.
- **The desktop app:** it asks GitHub for the newest release every few hours;
  when one is out, "Update the app" appears at the bottom of the sidebar and
  opens the release with the `.dmg`. Running the `install-mac.sh` one-liner
  again does the same from a terminal.
