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
| `crates/vercelify-core` | Shared Rust library: system collection (disks, power, thermal, Tailscale), models, pairing codes, volume/stream helpers. Models export TS types via `ts-rs`. |
| `crates/vercelify-platform` | Vercelify's own PaaS engine (replacing Coolify): SQLite state + migrations, sealed secrets, builds, deployments, reconciler. A library the agent mounts. |
| `crates/vercelify-mesh` | Vercelify's own private network (replacing Tailscale): addressing, keys, coordinator certificates and peer maps, and the `vercelify-mesh` daemon. |
| `agent/` | `vercelify-agent`, the small Rust daemon on the host (`:7420`, bearer token). Reports system state, manages containers/services/volumes and the display receiver. |
| `cockpit/` | Tauri v2 desktop app (React 19, TypeScript, Tailwind v4, Framer Motion). `src/` = UI, `src-tauri/` = Rust command layer. |
| `cockpit/src/types/generated` | **Generated** by `cargo test` from Rust models. Never edit by hand. |
| `host/` | One-line installer (`setup-host.sh`), `vercelify` admin CLI, firewall, power-guard, systemd units, bats tests. |
| `install.sh` | `curl … \| sudo bash` entry point that fetches `host/`. |
| `cli/vfy` | Mac/Linux command-line tool (`vfy apps/deploy/logs/env pull…`) using the app's paired servers. Bash 3.2 compatible. |
| `install-mac.sh` | `curl … \| bash` for the Mac: Tailscale + the cockpit app (release `.dmg` or a source build). Must stay bash 3.2 compatible. |
| `website/` | Open-source launch site (Vite, React 19, Tailwind v4, same tokens and fonts as the cockpit). `npm run dev` serves it on `:5180`. |
| `docs/` | Architecture, Coolify notes, development, display streaming, troubleshooting, Vercel parity, `vercel.json` support (PROJECT-CONFIG). |

The stack today is **Coolify v4** as the PaaS engine (builds, Traefik, Let's Encrypt,
previews, DBs), plus **Tailscale** for private access with no open ports, plus an
optional **Cloudflare Tunnel** for public HTTPS behind CGNAT. The cockpit talks
to the agent and to Coolify's API over Tailscale.

**Both are being replaced** by our own engine (`vercelify-platform`, selected per
server with `engine = "platform"` in `agent.toml`) and our own WireGuard mesh
(`vercelify-mesh`). The plan and milestones are in `docs/PLAN-NATIVE-PLATFORM.md`;
the binding engineering design is `docs/design/SYSTEM-DESIGN.md`. Change the
design doc in the same commit as code that departs from it. Until milestone M10,
Coolify and Tailscale stay the default and must keep working; new features go
into the platform engine, not into Coolify integration.

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
sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/coolify.sh   # real Coolify (Docker)
sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/routing.sh   # vercel.json rules on a real Traefik
sudo host/tests/contract/mesh.sh                 # four mesh nodes in network namespaces (debug agent + vercelify-mesh)

cd cockpit
npm ci && npm run typecheck && npm test && npx vite build
npm run e2e:demo                                 # Playwright UI flows against the demo backend
npm run tauri dev                                # desktop app
VERCELIFY_AGENT_URL=… VERCELIFY_COOLIFY_URL=… npm run dev   # browser mode via Vite proxies
```

The Tauri crate needs `cockpit/dist` to exist for `generate_context!`. CI creates a
placeholder. Coolify is pinned (`COOLIFY_TESTED_VERSION` in `host/lib/coolify.sh`):
when you bump it, run the contract test against the new version first. See
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
  Traefik and WireGuard (`boringtun`) as dependencies; build everything Coolify and
  Tailscale did for us on top of them (design doc §17).
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
