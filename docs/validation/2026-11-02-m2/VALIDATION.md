# M2 validation: idle BuildKit lifecycle

Working tree based on M1 `8cae1a3`, 2 November 2026. All M2 gates below pass, including automatic idle stopping and matched memory
measurements in the VM and on the physical x86 laptop.

## Checks

- Rust formatting, workspace Clippy with `-D warnings`, full workspace/all-feature
  tests pass. Total: 702 passed, 11 opt-in tests ignored. Platform library: 170 passed, 5 opt-in tests ignored. Agent:
  155 passed, 1 ignored; agent HTTP e2e: 20 passed. Full output: `rust-tests.txt`.
- Linux `bats host/tests`: all 223 cases pass, including the new builder wrapper
  safety test. Six installer cases separately pass as root.
- ShellCheck passes the repository command and the measurement script.
- Cockpit typecheck, all 131 unit tests, Vite production build and all 29 demo
  E2E tests pass. No UI behavior changed in this milestone.
- Dedicated Docker 29.9.2 daemon on aarch64: three build/stop/restart cycles pass,
  same cache volume retained, warm `CACHED` steps observed, paused/stopped builders
  skipped, held build slot prevents stop. `builder-contract.txt`.
- First contract attempt failed downloading pinned BuildKit (connection reset).
  Loading the identical cached `moby/buildkit:v0.18.2` from the outer daemon
  resolved it; the final complete contract passed. No product change for this.
- x86_64 static agent: 8,186,584 bytes; aarch64: 7,056,656 bytes.
  Both below the 8,388,608-byte CI budget; no new dependencies.
- Installed candidate through the real installer in vfybench. Installer output
  omits pairing/credential lines (`install.txt`).

## Behavioral review

Self-review, no sub-agents per the user's instruction. The semaphore excludes
maintenance throughout active builds and excludes new starts during inspect/stop.
Permit drop records completion before releasing its slot. Cancellation, failure,
concurrency >1, a fresh agent's grace period, power states, stopped/paused builders,
bootstrap failure and failed-stop retry are covered by unit/contract tests.
Maintenance never removes a container or volume. The dedicated 30-second loop
runs even when the general reconciler interval is zero.

External processes using the agent's named builder must coordinate or turn idle
stopping off; the build gate covers agent-managed builds. Compose's default Docker
builder and the embedded Docker driver are unaffected. No cloud service added.

## Memory measurements

VM matched pair complete: five whoami apps and Next.js at commit
`90cfee0f74c25bb3688369a0587e435f8b8f114d`, after its successful warm rebuild.
The one-minute configured timeout stopped BuildKit **61 seconds** after an agent
restart. Same app/container IDs throughout both sample sets.

| VM median MiB | Builder running | Automatically stopped |
| --- | ---: | ---: |
| System used (range) | 682 (674–708) | 656.5 (638–689) |
| BuildKit RSS | 42.22 | 0 |
| Agent RSS | 7.95 | 7.74 |
| Mesh RSS | 2.90 | 2.90 |
| Traefik RSS | 91.95 | 91.68 |
| dockerd / containerd RSS | 107.29 / 51.33 | 107.53 / 51.15 |
| All shims RSS | 86.22 | 75.52 |

The observed builder process exits and its RSS disappears. System-used medians
fell 25.5 MiB with overlapping ranges; this is one matched run, not a universal
memory-saving guarantee. Next.js container memory stayed around 113 MiB.
This fresh warm build differs from the older 110 MiB BuildKit footprint.
The application restarted before both states; compare this pair internally rather
than attributing differences from M1's earlier 702.5 MiB row to the idle feature.
Raw samples and generated JSON share the `builder-running`/`builder-stopped` names.

Laptop matched pair complete: same production app, PostgreSQL, unrelated media
service and desktop session in both states (PR preview already retired).
Automatic stop: **61 seconds**, with a one-minute timeout.

| Laptop median MiB | Builder running | Automatically stopped |
| --- | ---: | ---: |
| System used (range) | 1427 (1415–1438) | 1377 (1364–1397) |
| BuildKit RSS | 60.63 | 0 |
| Agent / mesh RSS | 10.30 / 3.60 | 9.99 / 3.60 |
| Traefik RSS | 89.23 | 89.35 |
| dockerd / containerd RSS | 100.80 / 72.82 | 100.29 / 73.63 |
| All shims RSS | 66.25 | 55.28 |

Median system used fell 50 MiB; ranges do not overlap in this one run. Container
IDs and images match within each pair except for the stopped BuildKit container.
Laptop BuildKit had been running from an earlier app build; unlike the VM, no
fresh Next.js build was triggered on it. Both agents' temporary one-minute settings
were removed after sampling, restoring the ten-minute default. Raw laptop samples
and summaries use the `laptop-builder-*` prefix; `laptop-idle-stop.txt` records time.
Raw method is research §6: 12 samples, 6 seconds between samples (plus Docker
stats collection time), after at least two minutes settling. Per-process RSS
counts shared pages; system used is reported separately. The disposable test daemon
is stopped for VM samples. `summarize-memory.py` produces median/range JSON.

The physical laptop was explicitly authorized for upgrade in this chat. Its
pre-upgrade sample (`laptop-current.txt`) is retained as context, not as the M2
matched comparison: the safety migration retired its existing PR #1 preview.
Candidate transfer succeeded using 48 KiB base64 chunks in Tailscale SSH command
arguments, fixed file offsets and SHA256 verification (3,716,706 compressed bytes,
41.1 seconds). Prior bulk stdin transfers had stalled. The previous binary,
config and stopped SQLite state were saved under the root-only
`/var/lib/vercelify/qualification-m2-before` before replacement. `laptop-upgrade.txt`
records health, the installed digest and preview retirement.

Not measured here: 1/2/4 GiB matrix, x86 build throughput, physical outage recovery,
comparative competitor memory or population variability. No small-host support
claim follows from these lifecycle tests.

Documentation repair: restored research §§7–9 from baseline `4f04691`; M1 had
accidentally removed that text while inserting its §6 results. No new competitor
claims were introduced by the restoration.

Serving probes after the upgrade return HTTP 200 for all six VM apps and the
laptop production app (`vm-serving.txt`, `laptop-serving.txt`); response bodies
and credentials are omitted.
