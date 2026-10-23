# Contributing to Vercelify

Thanks for wanting to help! Vercelify turns old laptops and Raspberry Pis into a
free, self-hosted Vercel. Every contribution, from a typo fix to a new feature, is
welcome.

## Ground rules

- Be kind. This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).
- Found a security issue? **Don't open a public issue.** See [SECURITY.md](SECURITY.md).
- For anything bigger than a small fix, open an issue first so we can agree on the
  approach before you spend time on it.

## What fits the project

Before building a feature, check that it:

1. Removes a step from "push code → app online", or makes running several apps on a
   weak machine more reliable.
2. Works on low-end hardware (8 GB RAM, HDD, possibly on battery).
3. Adds **no** recurring cost and **no** required cloud service.
4. Extends Coolify where possible instead of re-implementing it.

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/vercelify-core` | Shared Rust library (system probes, models exported to TypeScript). |
| `agent/` | `vercelify-agent`, the small Rust daemon on the host. |
| `cockpit/` | Tauri v2 desktop app: `src/` (React UI) and `src-tauri/` (Rust). |
| `host/` | Server installer, `vercelify` admin CLI, firewall, power-guard, systemd units, bats tests. |
| `cli/vfy` | Mac/Linux command-line tool. |
| `docs/` | Architecture, development and user guides. |

## Development setup

Prerequisites: Rust stable (`rustup`), Node 22, `jq`, `shellcheck`, `bats`.
On Linux also install the Tauri system libraries:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev
```

Then:

```bash
git clone https://github.com/shashank03-dev/vercelify-oss
cd vercelify-oss
mkdir -p cockpit/dist && echo '<!doctype html>' > cockpit/dist/index.html   # needed by the Tauri crate
cargo test --workspace --all-features

cd cockpit
npm ci
VITE_DEMO=1 npm run dev       # the UI with mock data, no server needed
npm run tauri dev             # the desktop app
```

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for browser mode, live tests, arm64
and installer end-to-end suites.

## Before you open a pull request

Run the same checks as CI:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features
bats host/tests
shellcheck -x -S warning install.sh install-mac.sh cli/vfy host/*.sh host/vercelify host/lib/*.sh host/tests/contract/*.sh

cd cockpit
npm run typecheck && npm test && npx vite build
npm run e2e:demo
```

### Conventions

- **Rust:** `cargo fmt` and clippy clean with `-D warnings`. After changing models in
  `vercelify-core`, run `cargo test` and commit the regenerated files in
  `cockpit/src/types/generated` (never edit them by hand; CI diffs them).
- **Host scripts:** idempotent, ShellCheck clean, and covered by bats tests for new
  CLI behaviour. `install-mac.sh` and `cli/vfy` must stay bash 3.2 compatible.
- **UI:** reuse primitives from `cockpit/src/components/ui.tsx` and the semantic colour
  tokens in `cockpit/src/index.css`. Never hard-code colours: dark and light themes must
  both work. Keep `data-testid` attributes stable; e2e specs depend on them.
- **Low-end first:** keep the agent tiny and polling intervals modest. No heavy
  always-on animation.

### Commits and pull requests

- Branch from `main`, keep pull requests focused on one change.
- Write clear commit messages in the imperative mood ("Add backup schedule", not
  "Added…").
- Describe what changed and how you tested it in the PR template.
- Update docs and the [CHANGELOG](CHANGELOG.md) "Unreleased" section when behaviour
  changes.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
