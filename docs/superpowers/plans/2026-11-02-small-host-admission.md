# M4 — small-host admission and honest resource accounting

Execution: main chat only. Continue the approved M1–M8 roadmap; no push without
fresh user permission. Preserve user-owned untracked files and unrelated services.
M3 is committed as 3660af3. Binding requirements: competitive roadmap §9,
companion validation §5, cross-platform hosting design resource budget section.

## Design

Extend placement; do not introduce a parallel scheduler. Replace volatile holds
with SQLite operation reservations, owned by deployment/build/database/wake/restore.
Each admission runs in one Store transaction against fresh, versioned capacity and
container inventory. Unknown, stale, inconsistent or incomplete capacity refuses
new work; running services continue. The optimistic unknown-main exception goes.
Pinned and Compose placements must pass the same capacity checks as automatic ones.

Capacity has two checks: measured MemAvailable minus additional in-flight demand
and safety margin; configured workload ceilings plus in-flight peaks against a
workload budget below the guest/host grant. OS/platform reserve is not subtracted
twice from measured availability. Old and new containers both count during rollout.
Inventory reconciles reservations already visible in a sample, avoiding counting
the same memory both as actual usage and as outstanding demand. Completion does
not release unseen allocations merely because a 90-second timer elapsed.

Persist ownership and sample generation. At restart, fail interrupted operations,
then reconcile their claims with fresh actual containers and owned build processes;
never clear claims before fresh inventory, and never resume work using stale memory.
Keep the existing build concurrency/power gate. Reject or queue resource pressure
before Git/build/pull/start work; do not stop production to make room. Wakes,
database create/restart/restore and authenticated image upload use this policy too.
Reserve disk temporary writes atomically and bound concurrent heavy CPU/I/O work.

Inventory separates apps, databases, BuildKit and platform containers. Display
observed container working memory separately from configured caps/estimates,
reservations, system used/available, OS/platform reserve and recoverable cache.
Unavailable values remain null/unknown. Include sample time, policy version,
profile/support boundary and explicit OOM evidence; never infer OOM from exit 137.

Use existing accepted host grants and measured provider allowance; guest RAM is
not the physical computer's spare capacity. If host-manager data is configured
but stale/missing, defer new work. Native Linux uses the existing workload slice;
aggregate ceiling and policy reserve must agree with it. Do not change running
environment grants or interrupt their workloads without prior authorization.

Profiles are explicit measured configurations. One GiB remains experimental,
runtime-only, one named app with a declared cap and no DB/build. Two/four GiB are
measured separately. Defaults for new operations use conservative app/build/DB
estimates; PostgreSQL gets connection/buffer settings plus a real memory cap.
Existing unlimited workloads are identified honestly; avoid silently recreating
production solely to lower its cap. No general no-OOM or arbitrary-app guarantee.

## Steps and checks

- [x] Red tests: unknown/stale main, pinned/Compose shortfall, aggregate replicas,
      concurrent requests, restart durability and fresh-sample reconciliation.
- [x] Implement shared policy and persistent operation/inventory schema; integrate
      existing Reservations and placement without redundant memory brokers.
- [x] Collect main/worker inventory and host-grant context, reconcile on restart,
      wire build/deploy/upload/database/restore/wake admission and queue reasons.
- [x] Add enforced per-workload defaults and small PostgreSQL preset; verify no
      unapproved production stop, cap increase or hidden local-build fallback.
- [x] Add memory API/model exports and Health memory card using existing primitives;
      demo unknown/stale states, unit coverage and real demo browser flow.
- [x] Real Docker concurrency/restart/pressure/wake/restore/limits contracts; all
      Rust, clippy, fmt, shell, bats, typecheck, UI/build and demo checks. Check
      static x86 agent size early: M3 leaves only 23,976 bytes below 8 MiB.
- [x] Measure declared 1/2/4 GiB fixtures separately; restore lab configuration.
      Repeat §6 steady-state samples; physical laptop only if reachable. It remains
      offline with saved build settings awaiting restoration (M3 validation).
- [x] Update PLATFORM, SYSTEM-DESIGN, research §6, evidence/support boundaries;
      self-review and plain milestone commit of exact files. Report, continue M5.
