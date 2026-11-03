# M6 — durable recovery

3 November 2026 (Asia/Kolkata). Main chat only; no push authorized.

Implemented: explicit migration-compatible image recovery, persisted publication
configuration/restart baseline/deadline, one atomic rollback job, health failure
and crash-loop detection, stop/new-job/config fencing through execution and
publication, verified runtime/local ingress outcome, bounded failure status,
durable generation-specific retirement, stopped-peer bridge ownership checks
and stale queued-error clearing. Compose recovery remains manual. Legacy/mutable
image targets without a matching retained immutable snapshot need manual recovery.
Image rollback never rewinds production data or migrations.

The updater records local workload readiness and declared stop/sleep state when
the installed agent supports it. Missing legacy support stays unmeasured. Changed
intent preserves current state. A schema rewind is explicitly requested with
`rollback --restore-control-state`, preserving later control writes otherwise.
The control snapshot/replaced file is retained; app volume data is unchanged.

`checks.json`: 767 Rust tests, 234 Linux bats tests, clippy, fmt, ShellCheck,
typecheck, production build, 142 UI unit tests and all 32 demo browser cases passed.
Generated ts-rs whitespace is the existing exception. Both static agents are below
8 MiB (`binaries.json`). Ten updater cases include broken serving with a healthy
control API, explicit state rewind, preserved post-update writes and changed intent.

`deploy-guard.txt`: final six-case real Docker matrix passed in 94.15 seconds,
with 871 unchanged production probes and zero failures. HTTP failure and crash
loop recover with exactly one job and verified serving; missing image fails with
no job; changed config/newer work cancels; injected old-container creation failure
stays failed after one job. SQLite reopen retains deadline/baseline.
`queued-stop-red.txt`/`queued-stop-green.txt` reproduce and fix late queued rollback
overriding an intentional stop. A publication gate serializes API control changes
and queue intent with the route/state commit, without holding SQLite over network I/O.

`engine.txt`: final engine/rollout/rollback/recovery/Compose/16-caller wake storm
contract passed in 101.29 seconds. `postgres-previews.txt` passes with 369 production probes/zero failures (77.24s).
`preview-safety.txt` passes credentials/network/ingress/Compose isolation (41.83s).
`offhost-builds.txt` passes authenticated image handoff and stale/cancelled/expired/
interrupted/revoked upload fencing with zero target Git/build commands.

`lab-install.txt` identifies the final installed schema-11 ARM candidate and its
consistent pre-upgrade backup. `reboot.json` records a real installed agent restart
and VM stop/start with a pending retirement. Guard deadline/baseline remained
identical across both. The expired retirement left one current generation; the
stopped and explicitly arranged sleeping fixtures stayed physically stopped. The
sleep fixture qualifies recovery of declared sleep, not idle detection. Deleting
the running fixture preserved the stored peers' bridge. The serving check returned
runtime/ingress true for the current generation.

All 132 original-app probes during agent restart succeeded. The deliberate VM
outage/recreated SSH forward took 21.59 seconds to regain all original routes;
504 of 510 probes during that phase failed as expected. All 1,260 post-recovery
probes succeeded. This is reboot recovery evidence, never zero-downtime reboot.
Cleanup's inspect raced a disappearing owned container after all substantive
assertions passed. The helper now accepts that specific disappearance; the recorded
IDs were cleaned in a separate successful retry (`cleanup.json`/`cleanup.txt`).
All six original routes returned 200; original app IDs/definitions are preserved.
`vm-steady.txt/json`: after a two-minute settle, twelve samples six seconds apart
plus collection time measured system-used median **588 MiB** (580–616), agent
**8.42 MiB**, mesh **2.81 MiB**, BuildKit zero. Same original six apps, four-GiB
ARM VM, disposable daemon stopped. M5 was 642.5 MiB/8.03 MiB agent. This run follows
a VM reboot and unchanged daemon footprints vary; the difference is not a causal
saving/overhead estimate. `original-apps.json` again verifies all original routes.
M6 code and evidence are complete for this scope; physical hardware remains unknown.

Qualification corrections: unused disposable test bridges exhausted Docker's
address pools, and an interrupted run left other apps also named web. Cleanup
removed only unused test-owned bridges while preserving every active or stopped
container attachment (`network-pool-cleanup.json`). The wake-storm assertion now
selects its exact app ID rather than every same-name container. Test HTTP fixture
http.py shadowed Python's standard library; it was renamed and startup fails fast.
Explicit runtime ports and immutable Docker content IDs have regressions after
real Docker qualification caught initial omissions. None of those failed attempts
are counted as passing qualification.

Scope: local readiness is not external DNS/TLS/ISP availability. Protected/custom
ingress may require manual verification. Worker inspection outages stay unknown.
The lab uses four-GiB ARM Linux; physical x86 and laptop results remain unmeasured.
The laptop is offline and the user chose lab continuation; its saved M3 build
settings still need restoration on reconnect. No unrelated Mac containers,
physical laptop services or user-owned files are changed.
