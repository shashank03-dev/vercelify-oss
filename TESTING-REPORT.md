# Testing report

Updated: 2026-10-27. Baseline: `ef20712fafbd1ada89c84e42ae8536cb47d9b4d2` (main). Branch: `test/full-pass`. Host: Apple Silicon macOS, Rust 1.99.0, Node 26.11.1, Bats 1.14.0. This is a partial test pass, not a production-readiness assessment.

## Suites

| Suite | Command | Result and counts |
| --- | --- | --- |
| Formatting | `cargo fmt --all --check` | Pass. |
| Workspace Clippy | `cargo clippy --workspace --all-targets -- -D warnings` | Pass on macOS after the portable openpty fix. Linux agent build remains unverified. |
| Mesh Clippy | `cargo clippy -p vercelify-mesh --all-targets --features daemon -- -D warnings` | Pass, including after the fix. |
| Workspace Rust | `cargo test --workspace --all-features` | Pass on macOS: 487 reported passing after the openpty and macOS listener fixes; seven Docker-gated tests returned early without exercising Docker. No generated-type diff. |
| Core Rust | `cargo test -p vercelify-core` | Pass: 177 tests. Generated types left no Git diff. |
| Mesh Rust | `cargo test -p vercelify-mesh --features daemon` | Pass: 45 tests, including both new regressions. |
| Platform Rust | `cargo test -p vercelify-platform` | Latest workspace run reports 114 passed; six Docker-gated cases immediately return without testing when `VERCELIFY_DOCKER_TESTS` is unset. Thus 108 actually executed, six not exercised. |
| Host Bats | `bats host/tests` | Fail: 131 passed, seven failed, two not executed (Bats reports 138 of 140). Failures: backup tests 1 and 3, CLI test 23, hardware test 42, maintenance tests 58 and 59, migrate test 78. Several show macOS `stat`, `sed`, `touch` or `date` rejecting GNU options; remaining failures need investigation on Linux. |
| ShellCheck | `shellcheck -x -S warning install.sh install-mac.sh cli/vfy host/*.sh host/vercelify host/lib/*.sh host/tests/contract/*.sh host/tests/integration/run.sh` | Pass. |
| Cockpit install | `cd cockpit && npm ci` | Pass; 179 packages installed, audit reported zero vulnerabilities. |
| Cockpit types | `cd cockpit && npm run typecheck` | Pass. |
| Cockpit unit | `cd cockpit && npm test` | Pass: 18 files, 75 tests. |
| Cockpit production build | `cd cockpit && npx vite build` | Pass. |
| Website | `cd website && npm ci && npm run typecheck && npm run build` | Pass; npm audit reported two high-severity advisories through a tooling dependency, detailed below. |
| Cockpit demo | `cd cockpit && npm run e2e:demo` | Pass: 17 Chromium tests, including new navigation checks in both themes. Demo backend only. |
| Bash 3.2 | `/bin/bash -n cli/vfy install-mac.sh` and `/bin/bash` invoking each `--help` | Pass on Bash 3.2.57. A separate Bash 3.2 Bats run of `vfy.bats`, `vfy-platform.bats`, and `install-mac.bats` reported 24 passing of 26 expected; its two skipped-case results were not emitted and Bats exited nonzero. Full behavioral coverage remains incomplete. |
| Linux binaries | `cargo build -p vercelify-agent && cargo build -p vercelify-mesh --features daemon` | Not run: this host is macOS, agent compilation above fails, and Linux cross-build prerequisites were not set up. |
| Routing contract | `sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/routing.sh` | Not run: requires a Linux Docker host. |
| Mesh contract | `sudo host/tests/contract/mesh.sh` | Not run: requires Linux network namespaces and working Linux binaries. |
| NAT contract | `sudo host/tests/contract/mesh-nat.sh` | Not run: requires Linux network namespaces and NAT. |
| Multi-node contract | `sudo host/tests/contract/multinode.sh` | Not run: requires Linux namespaces and Docker. |
| Promote contract | `sudo host/tests/contract/promote.sh` | Not run: requires Linux namespaces and Docker. |
| Migration contract | `sudo host/tests/contract/migrate.sh` | Not run: requires Linux and Docker with real Coolify. |
| Installer cross-build and integration | `cargo zigbuild --release -p vercelify-agent --target x86_64-unknown-linux-musl`; same for mesh daemon; `host/tests/integration/run.sh` with both binaries | Not run: cargo-zigbuild is absent and the integration requires privileged Linux systemd and Docker. |
| Desktop E2E | `cockpit/e2e-desktop/run-desktop.sh` | Not run: Linux WebKitWebDriver/Xvfb setup and Linux agent unavailable. |
| Live E2E | `cockpit/e2e/run-live.sh` | Not run: needs Linux, root, loop-mounted ext4 and Docker. |
| arm64 Linux/QEMU | `cargo test -p vercelify-core -p vercelify-agent --target aarch64-unknown-linux-musl` with QEMU runner | Not run: QEMU runner and Linux build setup unavailable. |
| Docker opt-in | `VERCELIFY_DOCKER_TESTS=1 cargo test -p vercelify-platform` | Not run: Docker daemon unavailable. Includes real builds, Compose, lifecycle, registry, engine and database integration. |
| Low-end resource test | five-plus apps under 8 GB cgroup, slow loop-backed disk | Not run: requires a Linux host with Docker, cgroups and loop devices. |

## Bugs and observations

1. **Low — malformed app-mode forward silently binds a port. Fixed in `c10aac2`.** Put `127.0.0.1:17420=:7420` in the forwards file alongside a valid line. Before the fix the parser accepted both: it checked only that the suffix after the last colon was a numeric port, and never checked the host was nonempty. The bad entry could bind locally but `resolve_target` could not resolve it. The new regression failed before the fix (two valid entries instead of one) and passes afterwards; valid lines remain usable and the malformed entry is reported separately.
2. **Low — Docker-gated tests report success without executing. Unfixed.** Run `cargo test -p vercelify-platform` without `VERCELIFY_DOCKER_TESTS=1`; the four Docker-dependent unit cases and two integration cases return immediately, yet the Rust harness labels them `ok`. This can mislead readers about real Docker coverage. Root cause: early returns in the test bodies. Proposed patch: mark these tests ignored by default and provide a documented explicit command such as `VERCELIFY_DOCKER_TESTS=1 cargo test -p vercelify-platform -- --ignored`; adjust CI so the real Docker suite is an explicit gate. This changes suite selection and needs verification on Linux before adopting.

3. **Low — app-mode forward does not recover from a transient bind failure. Fixed in `9ae0306`.** Hold a local port, add that port to the forwards file, then release it. Before the fix the failed task was retained and the unchanged file was never reconsidered. A new test reproduced the timeout; the watcher now retries failed tasks, and the mesh suite passes. The design document was updated in that commit.
4. **Low — workspace did not compile on macOS. Fixed in `9857912`.** Apple libc declares mutable `openpty` pointer arguments; the original code used const pointers. The original workspace test failed to compile on macOS, while the portable call and a PTY size regression now pass.
5. **Test harness — agent black-box readiness used Linux-only `/proc`. Fixed in `957043a`.** All eight existing black-box tests timed out on macOS because `listens_on` could not inspect `/proc`. The macOS check uses `lsof` to verify the same process owns the listening socket; all eight passed. No expectations were loosened.
6. **Dependency observation — website `npm audit` reports high-severity advisories. Unfixed.** `vgpu@0.5.0` depends on `@modelcontextprotocol/server@2.0.0`, affected by GHSA-jm63-cq47-j5jp and GHSA-46jr-4wx8-v453. The site imports the browser entry and no MCP server exposure was established; do not infer that the static website is exploitable. Proposed patch: update the upstream dependency when a compatible fixed version is available, then verify the site build and visual behavior.

The host Bats failures on macOS remain partly explained by GNU-only utilities and need a Linux run. No test expectations were changed.

## New tests

- `socks::tests::forwards_file_rejects_empty_target_host`: rejects an empty host and preserves a valid line; red before fix, green after.
- `socks::tests::forwards_file_retries_after_listen_port_becomes_free`: detects failed-bind recovery; red before fix, green after.
- `terminal::tests::pty_starts_with_requested_size`: opens a PTY and checks its size. The original Apple build failed before this test could compile.
- `protected_routes_reject_missing_wrong_and_revoked_tokens`: 26 representative protected routes, including terminal and runtime endpoints, reject all three invalid credential cases.
- `oversized_json_is_rejected_without_creating_an_app`: a >2 MiB request receives 413 and creates no app.
- `app_creation_rejects_injection_payloads_without_writing_rows`: rejects shell, SQL, routing and traversal-like app names and checks the store.
- `navigation-exploratory.spec.ts`: visits all seven pages using keyboard shortcuts in both themes and exercises the palette.

## Untested scope

The requested broad exploratory matrix is incomplete. In particular, no real deployment, restart during build/migration, preview cap, end-to-end secret exposure, full API authorization matrix, comprehensive path traversal and all oversized-body cases, real multi-node or revocation timing, host update/uninstall/backup on Linux, slow-disk/OOM endurance, visual inspection of all pages in both themes, agent-down/slow and pairing-error UI flows, or comprehensive Bash 3.2 behavior was newly exercised. Existing unit and demo tests cover portions of these but are not substitutes for the requested scenarios.

Real macOS utun/mDNS and DMG installation, physical Raspberry Pi hardware, real CGNAT/Cloudflare Tunnel, and actual laptop battery/lid behavior were not tested. Demo runs do not establish hardware or network behavior.

## Environment and workarounds

A separate 8 GiB Colima VM was requested without activating the user’s existing Docker context; the image download failed with unexpected EOF, and a resumed attempt is in progress. The requested repository was cloned separately because `/Users/shashank/vercelify` is a different repository with uncommitted changes. Docker CLI exists but its configured Colima daemon was stopped; it was not started or modified. No image prune, disk cleanup, mirror, CA change, privileged system modification, or host firewall change was performed. System Chromium/Playwright setup already supported the demo suite. macOS has BSD rather than GNU userland; rerun Bats and all Linux suites on an isolated Linux test host before treating their outcomes as authoritative.
