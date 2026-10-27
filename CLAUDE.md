# Vercelify

Your own Vercel on an old laptop. Vercelify turns spare x86 Linux machines (old
PCs/laptops, e.g. i5 7th gen + 8 GB RAM) and optional Raspberry Pi 4/5 workers into
a free, self-hosted PaaS: push-to-deploy, PR previews, rollbacks, databases and
custom domains with HTTPS. It runs many apps and deployments side by side on
hardware the user already owns.

**Product goal:** make deploying to your own old hardware as simple as Vercel, at
zero cost. Every feature should either remove a step from that path or make
running several deployments on a weak machine more reliable.

## Repo layout

| Path | What it is |
| --- | --- |
| `crates/vercelify-core` | Shared Rust library: system collection (disks, power, thermal, the mesh), models, pairing codes, volume/stream helpers. Models export TS types via `ts-rs`. |
| `crates/vercelify-platform` | Vercelify's PaaS engine: SQLite state + migrations, sealed secrets, builds, deployments, previews, databases, proxy routes, reconciler, multi-node. A library the agent mounts. |
| `crates/vercelify-mesh` | Vercelify's private network: addressing, keys, coordinator certificates and peer maps, NAT traversal and relay, and the `vercelify-mesh` daemon. |
| `agent/` | `vercelify-agent`, the small Rust daemon on the host (`:7420`, bearer token). Reports system state, manages containers/services/volumes and the display receiver. |
| `cockpit/` | Tauri v2 desktop app (React 19, TypeScript, Tailwind v4, Framer Motion). `src/` = UI, `src-tauri/` = Rust command layer. |
| `cockpit/src/types/generated` | **Generated** by `cargo test` from Rust models. Never edit by hand. |
| `host/` | One-line installer (`setup-host.sh`), `vercelify` admin CLI, firewall, power-guard, systemd units, bats tests. |
| `install.sh` | `curl … \| sudo bash` entry point that fetches `host/`. |
| `cli/vfy` | Mac/Linux command-line tool (`vfy apps/deploy/logs/env pull…`) using the app's paired servers. Bash 3.2 compatible. |
| `install-mac.sh` | `curl … \| bash` for the Mac: the cockpit app (release `.dmg` or a source build) and `vercelify-mesh`. Must stay bash 3.2 compatible. |
| `website/` | Open-source launch site (Vite, React 19, Tailwind v4, same tokens and fonts as the cockpit). `npm run dev` serves it on `:5180`. |
| `docs/` | Architecture, the platform (PLATFORM) and network (MESH) guides, development, display streaming, troubleshooting, Vercel parity, `vercel.json` support (PROJECT-CONFIG), the design (`design/SYSTEM-DESIGN.md`). |

The stack is Vercelify's own engine (`vercelify-platform`, in the agent: builds with
BuildKit, deployments, previews, databases, a pinned Traefik with Let's Encrypt) and
its own WireGuard network (`vercelify-mesh`) for private access, plus two optional
ways in from outside the LAN: `vercelify remote` (the router forwards the mesh's two
ports by UPnP/NAT-PMP) and a **Cloudflare Tunnel** (public HTTPS, works behind CGNAT).
The cockpit talks to the agent over the mesh. `install-mac.sh --pair CODE` installs the
app and pairs it in one step.

The binding engineering design is `docs/design/SYSTEM-DESIGN.md`. Change the design doc
in the same commit as code that departs from it.

## Commands

```bash
cargo test --workspace --all-features            # also regenerates cockpit/src/types/generated
cargo test -p vercelify-platform -p vercelify-mesh --features vercelify-mesh/daemon   # the new engine and mesh alone (no Tauri libraries needed)
cargo clippy -p vercelify-mesh --all-targets --features daemon -- -D warnings
cargo build -p vercelify-mesh --features daemon  # the vercelify-mesh daemon binary
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
bats host/tests
shellcheck -x -S warning install.sh install-mac.sh cli/vfy host/*.sh host/vercelify host/lib/*.sh host/tests/contract/*.sh
sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/routing.sh   # vercel.json rules on the engine's real proxy
sudo host/tests/contract/mesh.sh                 # four mesh nodes in network namespaces (debug agent + vercelify-mesh)
sudo host/tests/contract/mesh-nat.sh             # NAT traversal: hole punching and the relay behind real Linux NAT
sudo host/tests/contract/multinode.sh            # a worker with its own Docker: placement, mesh registry, replicas, logs/terminal
sudo host/tests/contract/promote.sh              # the warm standby promoted after the main server dies

cd cockpit
npm ci && npm run typecheck && npm test && npx vite build
npm run e2e:demo                                 # Playwright UI flows against the demo backend
npm run tauri dev                                # desktop app
VERCELIFY_AGENT_URL=… npm run dev                # browser mode via the Vite proxy
```

The Tauri crate needs `cockpit/dist` to exist for `generate_context!`. CI creates a
placeholder. Traefik is pinned (`proxy::TRAEFIK_IMAGE` in `crates/vercelify-platform`):
when you bump it, run the routing contract test against the new version first. See
`docs/DEVELOPMENT.md` for the arm64 (QEMU), live, desktop and installer e2e
suites.

## Conventions

- **Git:** `main` is the default branch; contributions arrive as pull requests
  (see `CONTRIBUTING.md`). Commits are authored by the human contributor. Don't
  add `Co-Authored-By`, "Generated with" or other AI-attribution lines to
  commits or PRs.
- **Think before building.** Before implementing a feature, check that it fits the
  goal above, works on low-end hardware (8 GB RAM, HDD, possibly on battery), and
  doesn't add a recurring cost or a required cloud service. Keep Docker, BuildKit,
  Traefik and WireGuard (`boringtun`) as the dependencies; build the rest on top of
  them (design doc §17).
- **Low-end first:** keep the agent small (static musl binary, ≤ 8 MB, gated in CI;
  `vercelify-mesh` ≤ 6 MB). Keep cockpit
  polling intervals modest. Avoid canvas/WebGL or heavy always-on animation in
  the UI.
- **UI:** reuse primitives from `cockpit/src/components/ui.tsx` and the semantic
  tokens in `cockpit/src/index.css` (`bg-surface`, `text-fg-muted`, `border-line`,
  `text-accent`…). Never hard-code colours: both dark and light themes must work.
  New pages are registered once in `cockpit/src/lib/nav.tsx` (sidebar, ⌘1–7, ⌘K
  palette). Fonts: Geist / Geist Mono. Design direction: calm graphite surfaces,
  one ember accent from the logo, white primary buttons, status always shown as
  dot + label. No glassmorphism or purple gradients. Check your changes with
  `VITE_DEMO=1 npm run dev` (mock data, see `src/lib/demo.ts`). e2e specs
  (`cockpit/e2e`, `cockpit/e2e-desktop`) select by `data-testid` and a few visible
  labels, so keep those stable when restyling.
- **Host scripts** must stay idempotent and pass ShellCheck. Add bats tests for new
  CLI behaviour.
- Rust: `cargo fmt` and clippy clean with `-D warnings`. After changing models in
  `vercelify-core`, run `cargo test` and commit the regenerated TS types (CI diffs
  them).
