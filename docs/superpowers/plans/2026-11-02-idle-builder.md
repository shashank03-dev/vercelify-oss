# Idle builder implementation plan

Goal: implement competitive roadmap M2 without interrupting builds or deleting cache.
Spec: docs/research/COOLIFY-COMPETITIVE.md §9 and companion validation §8.
Execution: main chat, as requested; one milestone commit after all checks.

Architecture: build permits record completion on drop. Maintenance nonblockingly
acquires all slots before checking elapsed idle time, inspecting and stopping the
BuildKit container. A separate 30-second agent loop runs even when reconciliation
is disabled. Default builder_idle_minutes=10; 0 disables. Existing builders are
bootstrapped before use. Docker driver and paused/stopped/missing builders are skipped.

Constraints: ≤8 MiB static agent, no new dependencies, cache retained, no laptop
upgrade or push without approval, no unrelated files staged.

Review focus: concurrent slots, cancellation resetting idle time, thermal pause,
bootstrap errors, failed stop retry, agent restart grace, absent builder.

- [x] Add gate lifecycle tests; observe red; implement permit drop and all-slot guard.
- [x] Add builder/engine tests for stop, pause, errors and bootstrap; observe red;
      implement idle maintenance and config with 30-second task.
- [x] Add real Docker cold/warm/cache contract and config checks.
- [x] Run Rust, shell, Linux bats, cockpit unit/build/demo, real Docker contract,
      both musl size builds; fix failures before proceeding.
- [x] Install candidate in vfybench, measure equal-workload builder running/stopped
      with §6 method; document raw results and unmeasured hardware.
- [x] Update PLATFORM, SYSTEM-DESIGN and research; review diff, stage exact files,
      commit M2, report and continue to M3. Do not push without approval.

Ruling: existing roadmap authorization supplies design and execution approval;
use the existing checkout on main to preserve requested milestone commits.

Review: self-review completed against M2 gates; no code findings. All-slot guard
and completion-before-release ordering checked separately from test results.

Evidence: full checks green; VM and laptop automatic stop each 61s at a 60s
timeout. Same-workload system used medians: VM 682→656.5 MiB, laptop 1427→1377 MiB.
Both real hosts restored to the ten-minute default.
