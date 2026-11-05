# Independent review of field-test follow-up — 2026-11-05

Reviewed `bdc2c72` (physical evidence) and `8b8e28d` (field fixes) on local main.
Read the battery/thermal observations, shutdown and recovery journals, code and
updated design together. This review did not initiate another physical outage.

## Findings corrected

- Restore writer detection split bind source paths on spaces and only recognized
  mounts below a restored directory. A container mounting the parent storage tree
  could remain running while its data changed. Inspect JSON now preserves path
  boundaries and recognizes parent and child mounts; unrelated paths stay excluded.
- Disk admission silently continued when sizing or filesystem measurement failed.
  It now refuses before stopping the engine on missing/invalid measurements.
- Allocated disk usage underestimated sparse files copied without sparse mode.
  Admission now budgets apparent sizes and groups by the actual staging filesystem,
  including paths with spaces. Five new Linux regression cases cover these paths.

The first three regression cases failed against the reviewed code and passed
after correction. Linux tests use stub Docker/systemctl/restic inside temporary
sandboxes: these checks never perform an instance restore on the serving laptop.

## Fresh checks

- `cargo test --workspace --all-features`: 807 passed, zero failed, 14 ignored.
  Ignored tests were not blindly enabled on a serving Docker daemon. Their earlier
  environment evidence is catalogued in ../2026-11-04-final-qualification/IGNORED-TESTS.md;
  this review does not claim they were rerun against this new revision.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- Rust format, changed shell script syntax/ShellCheck, and diff whitespace: passed.
- Linux `bats host/tests`: all 259 passed. After final staging-path refinement,
  the affected backup suite passed all 22 tests again. Logs accompany this report.
- Fresh ARM64 musl release build of agent and mesh: passed. The isolated Linux
  namespace mesh contract passed 28 checks (including key rotation, revocation,
  and saved-map restart); NAT contract passed eight (hole punching and relay).
  These used rebuilt binaries and separate namespaces, not the physical host.
- Read-only Tailscale checks: three Vercelify services active; original five
  protected container identities unchanged and running. Both Mac mesh browser routes returned HTTP 200.
  Installed x86 agent and mesh hashes match the local release binaries (JSON).
  No leftover thermal/battery observer workload appeared in the process check.

## Evidence limits retained

The physical depletion run exercised the previous battery guard; revised polling,
critical first-reading shutdown and worn-cell timeout have automated coverage but
still need a separately approved physical retest. The thermal run peaked at 86 C;
thermal policy thresholds were not reached. Frequency samples alone cannot rule
out throttling. The report now states 295/300 production and 294/300 preview HTTP
successes (293/300 paired successes), rather than conflating request counts.

Forty successful mesh probes after the routing changes do not reproduce the
original boot/overlay transition or conclusively establish its cause. Automatic
AC-return startup was not observed under the current firmware. Abrupt power cuts,
missing/healthy battery permutations and physical critical thermal thresholds
remain unqualified. Earlier public/cloud qualification remains as documented;
no ongoing production R2 backup policy was created by those provider tests.

These fixes are local-main changes. No Git push, BIOS change, production restore,
or new physical outage was performed for this review.
