# M6 — durable deploy guard and recovery

Execute in the main chat using the already approved M1–M8 roadmap. No sub-agents,
no push. Freeze M5 code in the index before implementing this plan; commit that tested snapshot before staging M6. Existing three-minute watcher
is in-memory, samples restart counts late, ignores HTTP failures and announces
rollback success at enqueue. Existing update checks only control-plane health.

## Decisions

- Persist a guard per watched production deployment with publication deadline,
  prior compatible deployment, restart baselines, consecutive failures, evidence,
  optional rollback job and explicit watching/requested/verified/failed/cancelled/
  expired outcomes. Agent restart resumes the original deadline.
- Use existing per-app operation fences and deployment queue. A newer/manual job,
  desired stop/sleep or changed runtime/config cancels the old guard. Remote
  inspection failure is unknown, never an application failure. Missing containers
  remain the reconciler's responsibility. Paused workloads remain paused.
- Probe the declared health policy, including HTTP while running, and compare
  restarts with the publication baseline. Three bad observations or two added
  restarts trigger one attempt. Persist before enqueue in the same transaction;
  resume requests after restart without issuing another job. Never recurse on an
  auto-rollback deployment. Image rollback never restores database/volume writes.
- Capture a configuration compatibility fingerprint at healthy publication. For
  automatic rollback require a recorded matching prior configuration and retained
  image; older unrecorded deployments are manual recovery only. Do not restore
  revoked/changed secrets or pretend image rollback reverses migrations. An
  explicit migration compatibility policy must gate unsafe automatic recovery.
- Verify rollback job healthy/current plus direct health and actual local ingress
  before recording recovered. Expose journal state through authenticated API and
  concise UI status; queueing is reported as requested only. Bounded deadlines
  and failed outcomes remain visible rather than looping forever.
- Reboot reconciliation removes superseded instances only after the existing
  grace, preserving active/wanted deployments and Compose sidecars. Tests include
  recent containers followed by elapsed grace, real VM reboot and app desired
  states. Shared project bridge deletion checks stored peers, including stopped
  apps and databases; unknown query state preserves the bridge. Clear queued
  reasons when a deployment starts or becomes healthy.
- Update snapshots record intentionally running healthy apps, then validate
  equivalent runtime/local ingress after update. Sleeping/stopped apps are not
  started. Failures distinguish app/ingress from platform-control failures and
  never silently overwrite post-update control/user data to get a green result.

## Execution and evidence

1. Store/pure decision red-green tests: restart deadlines, early/delayed crash,
   running HTTP failure, missing/paused/unreachable state, stop/new-job fencing,
   config compatibility, one-attempt journal, verified outcome and failure.
2. Integrate durable watcher and publication snapshots with normal queue/admission.
   Use existing remote health path; no new always-on dependency. Test journal
   reopen before request and after enqueue, including duplicate callbacks.
3. Fix shared-network ownership and stale error with targeted regressions; verify
   superseded cleanup against observed M4 failure rather than assuming grace
   elapsed. Guard image pruning and rollback dependencies during observation.
4. Add update workload snapshot/check contracts, including bad proxy update and
   migration-safe failure reporting. Validate real serving after recovery.
5. Real Docker failure matrix and real lab agent restart/VM reboot under probes;
   preserve all original apps and restore lab configuration. Full Rust/clippy/fmt,
   shell/bats, cockpit checks and Docker regressions appropriate to edits. Static
   agents <=8 MiB; matched six-app memory per research §6, physical laptop unknown.
6. Update SYSTEM-DESIGN, PLATFORM and research evidence; exact-file M6 commit,
   report measured limitations and continue M7a then M7b.

Ruling: prepare M6 while the frozen M5 candidate settles/samples. M5 code is already
staged; only exact M5 documentation/evidence paths may be added before its commit.
This avoids idle qualification time without mixing milestone implementations.

## Execution ledger

- M5 frozen/qualified snapshot committed as `7647c9c`; later work belongs only to M6.
- Red/green: stale queued error, stopped-peer bridge deletion, single atomic rollback
  request/new-job fencing, and queued rollback after intentional stop.
- Durable guard/schema/config hashes implemented. SQLite file reopen preserves
  baseline/deadline. Explicit port mismatch caught by real Docker, fixed and covered.
- Real six-case Docker matrix passed (HTTP failure, crash loop, missing image,
  changed config, newer job, failed rollback), 870 unchanged production probes,
  zero failures. Full final regressions/reboot/measurement still pending.
- Ruling: compatibility is explicit and defaults off; no old-secret/config replay.
  Retained immutable images with matching snapshot are required. Cost: an older
  unrecorded deployment or mutable external tag needs manual recovery.
- Ruling: serialize API control changes with publication; enqueue can return a
  retry conflict during that short commit. This closes stop/new-job publication
  races without holding SQLite across remote I/O. Release before image pruning.
- Update control-plane checks extended to recorded local workload readiness.
  Schema rewind is explicit, preserving later control writes by default. Ten
  update tests pass, including broken workload/healthy API and changed intent.
- Qualification corrections: test fixture named http.py shadowed Python's standard
  library; renamed and startup now fails fast. Initial Docker fixture used an
  image-ID form omitted by the immutability check; content IDs are supported.

- Ruling: managed app/Compose containers already use `unless-stopped`, so explicit
  stops persist across Docker restart. Qualify this on the VM; do not add redundant
  restart-policy mutations based on the earlier restart-always hypothesis.

- Final regressions: engine 101.29s; PG preview 77.24s/369 successful production
  probes; preview safety 41.83s; off-host handoff passed. All 767 Rust, 234 bats,
  142 UI and 32 browser cases passed. Static ARM/x86 remain below 8 MiB.
- Installed candidate schema 11, agent restart and real VM stop/start qualified.
  Deadlines/baselines persisted, pending retirement completed, declared stopped/
  sleeping fixtures stayed stopped, peer bridge preserved. Agent restart 132/132
  successful probes; expected VM outage 504/510 failures, recovery 1260/1260 passes.
  Cleanup inspection race corrected/retried; original six apps restored.
- Ruling: freeze the tested M6 code in the index while matched memory samples run.
  Only exact M6 evidence/research paths may be staged before its commit; M7 work
  stays unstaged. This uses qualification idle time without mixing milestones.

- Matched sample complete: total 588 MiB (580–616), agent 8.42 MiB, mesh 2.81 MiB,
  BuildKit zero. All original six routes pass after cleanup. Reboot/cache history
  limits attribution. M6 complete for recorded lab scope; physical field unknowns
  remain explicit. M7a follows.
