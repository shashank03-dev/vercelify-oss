# M1 validation: first-deploy and preview safety

## Result (final pass, 2 November 2026)

All M1 gates below ran on the working tree that is committed as M1. Earlier
sections are kept as the history of this milestone, including the failing
isolation gate that was then fixed.

| Gate | Result | Evidence |
| --- | --- | --- |
| `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` | Pass | |
| `cargo test --workspace --all-features` | Pass (platform lib 167, agent 154, agent e2e 20, others unchanged); Docker-only suites run separately below | |
| Real Docker preview contract (`host/tests/contract/preview-safety.sh`), disposable Docker 29.9.2 / Compose 5.6.1 daemon, aarch64 | Pass: single-EXPOSE image port; preview gets its preview value, never the production one; production DB and host management ports unreachable from preview code; forged Host / direct IP / forwarded-header requests refused (403); unrelated bridge can't reach the preview; proxy recreation rejoins; Compose networks replaced; cleanup idempotent | [preview-safety-contract.txt](preview-safety-contract.txt) |
| Routing contract (`host/tests/contract/routing.sh`), same disposable daemon, release aarch64 agent | Pass | [routing-contract.txt](routing-contract.txt) |
| `bats host/tests` on Linux (vfybench) | 222 pass; the 6 installer cases also pass as root | |
| shellcheck (CLAUDE.md command) | Pass | |
| Cockpit typecheck, `npm test` (131), `vite build`, `npm run e2e:demo` (29) | Pass | |
| Static binary budgets | agent 7.0 MB aarch64 / 8.18 MB x86_64 (budget 8 MiB = 8.39 MB: 205 KiB left on x86_64); mesh 2.9 / 3.3 MB (6 MiB) | |
| Upgrade path | Real installer re-run on vfybench with the candidate binaries: schema 7, mesh ingress recorded, retirement list drained, all 6 apps stayed healthy | [after-install.txt](after-install.txt) |

### Memory, research §6 method (vfybench, same workload)

Five whoami apps and the Next.js app, BuildKit not running, 12 samples 6 s apart,
the test daemon stopped during sampling (it wasn't running for the baseline either).

| | Before (v1.0.0 + earlier tree) | After M1 |
| --- | ---: | ---: |
| System `free -m` used, median (range) | 693 (682–696) | 702.5 (675–712) |
| `vercelify-agent` RSS | 8.86 | 7.70 |
| `vercelify-mesh` RSS | 3.11 | 2.89 |
| Traefik / dockerd / containerd / shims RSS | 91.2 / 103.9 / 50.3 / 75.0 | 91.1 / 104.7 / 51.2 / 75.4 |

The ranges overlap: no measurable change from M1. [Summary](after-install-summary.json).

### Field-test laptop (Dell Inspiron 5567, i5, 7,840 MiB, x86_64, Docker 29.1.3)

Reached over Tailscale. Before M1 (installed v1.0.0), same script, 12 samples: system
used median **1,381 MiB** (1,374–1,409), agent 20.9, mesh 3.3, Traefik 88.3, dockerd
99.1, containerd 72.7, shims 65.5, BuildKit 35.5 MiB RSS. Workload: one app, its
Postgres, a PR #1 preview, BuildKit, the proxy, an unrelated `mediamtx` container and
the desktop session ([raw](laptop-before.txt), [summary](laptop-before-summary.json)).
Not comparable with the VM rows: different workload and a desktop session.

**After M1: not measured.** Small Tailscale SSH commands worked (28 ms ping), but every
transfer of the 3.7 MB compressed x86_64 agent stalled and timed out, by SSH push,
in chunks, or pulled over HTTP from the Mac; the laptop was left on v1.0.0, its PR #1
preview untouched. The x86_64 build itself compiled and meets the size budget.

### Not established by M1 (left for later milestones or unmeasured)

- A genuinely external IPv6 client, a Cloudflare tunnel path and WebSocket upgrades to a
  private preview were not exercised; the contract covers IPv4 direct-IP, forged Host
  and forwarded-header requests on the real proxy.
- Private previews need a device that resolves `*.vfy` (a server, or a Mac with the mesh
  in system mode). In the Mac's default app mode, opening the URL in a browser depends
  on the known F2 gap (2026-10-30 validation).
- Preview runtime has no outbound internet; apps needing external services in previews
  must run them inside their Compose stack until a later egress policy.
- 1/2/4 GiB matrix, OS-only and build-peak rows: not rerun in M1.

## History: continuation notes

The user resumed work and the PC is reachable with Tailscale SSH:
`/Applications/Tailscale.app/Contents/MacOS/Tailscale ssh user@100.123.113.77`.
Passwordless sudo works; Docker 29.1.3. Raw before-install memory samples are in
`/tmp/vercelify-m1-resume/laptop-before.txt`; no PC upgrade yet.

Implemented since the first stop (not yet qualified/committed): dedicated
Docker 28+ `internal`/`isolated` preview bridges, no production/shared proxy
network membership; proxy attachment/reconnection; health probes from proxy
namespace; NET_RAW dropped/no-new-privileges; Compose network overrides and
privilege/port rejection; preview DB references denied in preview-only values
as well as shared values; durable migration retirement of old previews before
the agent starts new jobs. Added `preview_network.rs`, a real Docker contract
`tests/preview_safety.rs`, host wrapper and bats guard test. The platform lib
suite passed 162 tests before the latest upgrade-cleanup additions.

Disposable Docker-in-Docker container `vfy-m1-contract-docker` inside `vfybench`
mounts the repo read-only at `/workspace`. This avoids the real benchmark proxy.
Dependencies bash/curl/jq/nodejs/python3/git/iproute2/openssl installed inside.
Real routing contract **passed** (`/tmp/vercelify-m1-resume/routing-real.log`),
with its existing no-second-address skip. ARM static agent compiled; cross-built
preview contract is in `target/aarch64-unknown-linux-musl/release/deps/preview_safety-*`.
First contract run failed on invalid fixture SHA `fixture`, fixed to 40 `a`s;
rebuild and rerun needed. Reset only the inner daemon's test containers before
rerunning (never remove outer benchmark containers). Workspace test run found
absent-preview deletion regression (400 vs 200), fixed by skipping network
removal for absent preview records; rerun needed. Generated TS whitespace fix
is in Rust field comments, but core tests must run to regenerate output.

Known work still in progress: finish packet-level contract, retire-upgrade test,
app-delete/failed-remote-deploy network ownership cleanup, verify Compose cannot
inherit agent secrets through process environment, documentation and all checks.
Do not claim M1 complete from this intermediate record. M2–M8 untouched.

---

2 November 2026 (Asia/Kolkata). Base HEAD:
`4f04691` (`Add cross-platform hosting, competitive research and validation evidence`).
Work resumed on an already dirty `main`; no commit, staging or push was performed.
M2–M8 have not been started in this continuation.

Read before continuation: `CLAUDE.md`, `docs/research/COOLIFY-COMPETITIVE.md`,
the entire companion `2026-11-01-COMPETITIVE-VALIDATION.md`,
`docs/design/SYSTEM-DESIGN.md`, and the earlier
`docs/validation/2026-10-30/VALIDATION.md`. Historical validation is not a new pass.

## Inherited implementation

The working tree already contained M1 work in Rust and the cockpit: single-TCP
EXPOSE detection, explicit-port precedence, bounded subprocess log reading,
terminal/multiline cleanup, preview variable sharing/provenance, migration 0007,
private preview host generation and source checks on base/project/wake routes.
These changes were not committed or release qualified. Unrelated untracked
field-test, launch assets, skills and audit files were preserved.

## Changes in this continuation

- Applied `cargo fmt --all` to inherited Rust changes.
- Reproduced short-secret leakage in `logs::Redactor` with a failing regression,
  then removed the six-character exemption. Reproduced replacement-mask corruption
  with overlapping short secrets, then changed replacement to match original text
  once, preferring the longest match at each position. All four log tests pass;
  the terminal-output fixture now expects short secrets to be hidden too.
- Added the nonignored engine regression
  `preview_does_not_join_the_production_database_network`. **It fails.** The real
  engine, using its Docker command fixture, starts a preview with
  `--network vfy-net-<production-project>`. Production databases use that network.
  [Failure output](preview-network-failure.txt). This proves the command-level
  isolation violation; it is not a real packet-level penetration test.
- Recorded the unfinished policy and blocker in the design doc and roadmap.
- Saved a reproducible, read-only inherited-installation memory baseline below.

## Checks

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Pass after formatting |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass; ts-rs prints its existing serde-attribute parsing warning |
| `cargo test --workspace --all-features` before the new isolation gate | Pass, with Docker-required tests ignored by their existing attributes |
| Same workspace command with the new isolation gate | **Fail:** platform library 158 passed, 1 failed, 5 ignored; failure is the preview-network regression |
| Log regression suite | 4 passed ([output](redaction-tests.txt)) |
| `bats host/tests` on macOS | Fails at GNU `sed -i` usage in `backup.bats`; rerun on Linux |
| `bats host/tests` in vfybench | 216 passed, 5 root-only installer cases skipped ([output](bats-linux.txt)) |
| Installer bats rerun as root in vfybench | All 6 passed, including the 5 skipped cases ([output](bats-installer-root.txt)) |
| Required shellcheck command, plus the sampling script | Pass |
| Cockpit `npm run typecheck` | Pass |
| Cockpit `npm test` | 32 files, 131 tests passed |
| Cockpit `npm run build` | Pass (TypeScript + production Vite build) |
| Cockpit `npm run e2e:demo` | 29 passed ([output](e2e-demo.txt)); demo-backend browser tests, not real Docker/mesh qualification |
| `host/tests/contract/routing.sh` in vfybench | Refused by the existing server guard because the VM has an active installation; no override used ([output](routing-contract.txt)) |
| `git diff --check` | Fails on trailing spaces in three generated TS model files; regenerate/fix at the Rust/ts-rs source, never hand edit generated types |

Full latest Rust workspace failure output is retained as
[cargo-isolation-gate.txt](cargo-isolation-gate.txt). Other temporary execution
logs are in `/tmp/vercelify-m1/` for this session; those paths are not durable
release evidence.

## Memory boundary

The existing vfybench installation was **not upgraded** to this working tree.
[Raw samples](inherited-baseline.txt) include installed agent/mesh SHA-256 hashes,
kernel, Docker version, image identities and timestamped free/RSS/container output.
[Script](sample-memory.sh), [summary](memory-summary.json).

Twelve samples, six seconds between collections plus collection time; same five
whoami apps and one Next.js app, BuildKit not running. Median system used:
**693 MiB**, range **682–696 MiB**. Agent/mesh RSS medians: **8.86/3.11 MiB**.
Swap: 4,095 MiB configured, zero used. RSS counts shared pages per process and
must not be summed into a platform delta. No fresh OS-only reference exists for
this run. Host-side checks ran concurrently. This is an inherited steady-state
baseline, not a completed M1 measurement or a claim about memory regression.

Tailscale reported both saved field-test laptop identities offline, at
`100.124.130.83` and `100.123.113.77`. The historical SSH alias `vfy-laptop` no longer
resolved. No physical-host measurements were possible.

## Resume at M1

Keep the failing isolation test enabled. Implement runtime isolation across local,
remote and Compose preview paths, including production DBs, the common proxy
network, host management addresses, IPv4/IPv6, restart and owned-resource cleanup.
A separate bridge name alone does not establish those boundaries.

Add/run the real host preview safety contract and all ingress bypass cases from
the companion review §6.1 in an isolated Docker/host test environment. The current
routing contract cannot qualify them and must not replace the benchmark proxy
without arranging restoration. Complete quickstart/security/support documentation,
resolve generated-type whitespace, rerun all gates, install the qualified binary
in vfybench, and remeasure before marking M1 complete. Commit only the reviewed M1
files with a plain message once green. Report M1 before starting M2; push to main
only with all required checks green.

Stopping here follows the user's explicit instruction to fix a failed check or
stop and report it. No milestone completion or end-to-end release qualification
is claimed.
