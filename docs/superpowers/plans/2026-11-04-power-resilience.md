# M8 Power Resilience Implementation Plan

> Execute inline with superpowers:executing-plans. The user forbids sub-agents.

**Goal:** stage opt-in battery conservation without reviving manually stopped work,
and publish evidence bounded to the actual qualified machines.
**Architecture:** extend the existing shell guard and agent sleep journal; retain
existing engine admission, desired-state reconciliation and independent shutdown.
**Tech Stack:** Bash/sysfs, Rust/Tokio/SQLite, existing Docker/Traefik.
**Spec:** `docs/superpowers/specs/2026-11-04-power-resilience.md`.

## Global constraints

No new daemon; no push; no physical shutdown without a reachable owned test host.
Policy disabled by default, thresholds 50/40/30/20/15, two readings, five-point
hysteresis, stable AC sixty seconds. No battery in a VM is no battery evidence.

## Review focus

Manual stop during power sleep must win; generation replacement/PR close must not
wake stale containers; AC bounce/clock rollback must restart recovery delay; paused
containers not owned by this guard must remain untouched; agent restart must retain
power sleep reasons and avoid duplicate admitted wakes.

## Tasks

- [x] Add failing Linux guard tests for policy opt-in, staged debounce/hysteresis,
  stable AC, unknown sensor and ownership. Implement state transitions and exact
  pause ownership; keep shutdown and thermal state precedence.
- [x] Add failing sleep/engine regressions for manual-stop and generation fencing.
  Persist power sleep reason, honor explicit enabled targets and block wake until
  stable recovery; resume only matching journal entries through admitted wake.
- [x] Update doctor, configuration, security/support/launch evidence and examples.
  Record physical matrix as unmeasured where a real device is unavailable; retain
  backup-copy feature disabled.
- [x] Run focused then required regression checks, isolated Docker flow and matched
  memory. Update SYSTEM-DESIGN/research and exact-file commit; report remaining
  external qualification without claiming the physical gate passed.


## External gate still open

- [ ] Complete physical laptop outage/depletion, firmware, lid, thermal and network
  recovery matrix when the owned laptop is available. It currently reports offline;
  software completion does not complete this acceptance gate.
