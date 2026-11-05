# Local branch reconciliation before remote-main update — 2026-11-05

Source verification: `7a3da21`. All development branch tips present on this PC
are ancestors of the integration result. No local branch was deleted or reset.
Physical AC-start and revised battery-policy retests are explicitly deferred.

## Branch decisions

- `screen-sharing-unpushed`: imported the missing frame-progress detection, SRT
  reconnect/relisten behavior, port refusal, sender supervision and real-stream
  contract. Kept current 1128-byte packets, virtual desktop support, process
  ownership protections and slow image-upload route.
- `fix/m7-verification`: imported missing Compose host-boundary checks, private
  generated files, literal env encoding, retained read-only checkout mounts,
  Compose-only repository detection, non-database service selection, declared
  port handling, volume-removal refusal and CI website/Docker checks. Existing
  private preview networks, capacity/builder admission, HTTP readiness, mesh
  locking and newer restore safeguards take precedence over older equivalents.
- `fix/audit-findings`: its nine audit fixes and follow-ups were already present
  (the same project/routing/detector implementations and regression cases).
  Recorded reviewed ancestry with an `ours` merge, preserving newer publication,
  preview variable, Compose-network and builder behavior. No raw root audit
  report was added to the final tree.
- `origin/ccr-72703b0b-tacfi3`: earlier copies of the bug sweep were already
  represented; a merge-tree check produced the same tree before recording its
  history with a normal merge. Dependency-bot PRs are separate pending upgrades,
  not local development work, and were not bulk-merged.
- Every other local branch was already an ancestor of main.

| Local branch | Snapshot before final main update | Outcome |
| --- | --- | --- |
| `docs/m6-release` | `a72f248` | Included |
| `feat/m3-browser-gateway` | `6100e94` | Included |
| `fix/audit-findings` | `be01ad1` | Included |
| `fix/m1-server-lifecycle` | `3cdf779` | Included |
| `fix/m2-pairing` | `046635f` | Included |
| `fix/m4-second-screen` | `50d8ac9` | Included |
| `fix/m5-workflows` | `0aecfd5` | Included |
| `fix/m7-verification` | `bb65b14` | Included |
| `main` | `a5187c7` | Included |
| `screen-sharing-unpushed` | `27d4a9c` | Included |
| `shashank-agent/branch-integration` | `7a3da21` | Included |
| `shashank-agent/installation-product-audit` | `5d549c1` | Included |
| `shashank-agent/review-fixes` | `db1072b` | Included |
| `sunshine-auto-pair` | `71971d8` | Included |

## Corrections made during integration

Small-body buffering now runs after authentication within the ordinary API
120-second deadline; bulk image uploads remain streaming under their existing
lease/deadline. A hanging unauthenticated-body regression failed before the layer
correction and passed afterwards.

Compose fallback no longer masks invalid commands in a detected language. Build
classification checks the same YAML text/alias-expansion bounds before loading
as the downstream implicit-file scanner. Both regression cases failed before the
correction and passed afterwards. Inferred Compose ports are persisted for
recovery. The durable-retirement fixture now waits for the first operation to
settle before reporting capacity: observing Healthy alone precedes settlement.

Uninstall stops managed apps as documented, but keeps the shared workload slice
and its active limits so unrelated Docker containers are not stopped. Its unit
regression failed before correction. The old GitHub installer assertion that apps
keep serving after uninstall was inconsistent with current behavior and was
updated to assert stopped apps, preserved data and retained slice instead.
No uninstall or outage was run on the physical laptop.

## Fresh verification

- Rust workspace/all features: **833 passed, zero failed, 14 ignored**.
- All-target/all-feature Clippy with warnings denied, Rust format, complete
  configured ShellCheck list, generated-type consistency and diff whitespace:
  passed. The known ts-rs serde-attribute diagnostic remains a generator note.
- Linux host suite: **260 passed** (sandbox Docker/systemd commands).
- Cockpit: typecheck, **152 unit tests**, production Vite build and **33 Chromium
  demo browser flows** passed. Website production build passed too.
- Actual ARM64 Docker Compose startup/env handoff and teardown passed inside the
  owned disposable `vfy-final-qualification-docker` daemon.
- The actual Docker engine contract passed deploy/redeploy, rollback, recovery,
  auth, failed deploy, previews and retained read-only Compose checkout/redeploy
  checks. An initial attempt correctly refused a 3-GiB disk reservation because
  this container mounts `/tmp` as a 2-GiB tmpfs. The successful run used a private
  disk-backed TMPDIR with measured headroom; admission was not disabled or faked.
- Native ARM64 agent + FFmpeg/mpv in Linux namespaces: **8/8** selected screen
  scenarios passed (API/sender validation, UDP, lossy/cellular SRT, reconnects,
  killed receiver, occupied port, agent shutdown). This is headless stream/network
  evidence, not physical laptop screen or macOS capture-permission evidence.

The first browser attempt reused node_modules via a cross-worktree symlink and
Vite correctly refused the out-of-root font files (HTTP 403). The successful run
used a separate local dependency directory; product serving policy was unchanged.
The 14 ignored tests were not blindly enabled against a serving Docker daemon;
previous runtime evidence remains catalogued in the final-qualification folder.

## Local-only files and publication boundary

Preserved local tools/skills, HANDOFF.md, AUDIT-FINDINGS.md, the unused M1 installer
helper, raw field-test material, skills-lock.json and launch-film/launch-video work.
These are untracked local material (including raw logs and about 140 MB of film
assets), not reviewed product commits. They were not swept into a public push.
A credential-pattern scan of the tracked tree found only an existing synthetic
`abc` PEM test fixture; it is not a real private key.

This record precedes the authorized fast-forward of local and remote main.
GitHub CI/edge and a manually dispatched integration workflow must be assessed
on that pushed revision separately. No force push or branch deletion is needed.

## Pre-push helper follow-up

The first push attempt ran the repository's pre-push hook and was withheld: two
concurrent GNU linker processes exhausted the existing 2-GiB Colima guest (kernel
OOM log identifies `ld`; Mac tests, Linux Clippy, host tests and builds passed).
The Colima test/build helpers now default to one Cargo build job, preserving an
explicit guest override. No check was disabled and the VM was not restarted.

Review of that helper also found that `cargo build --example sender_args` filters
out the agent binary even when both packages are selected. `dev screencast` now
builds the agent and example separately before running its contract. A sandbox
regression refused the original single build (no fresh agent artifact), then
passed with both builds; all four dev-helper tests passed on Linux.
