# Local branch review — 4 November 2026

## Follow-up: four blocking findings corrected

The user authorized applying the fixes and adding them to local main. Relevant
changes from `fix/m7-verification` were ported selectively, preserving main's
isolated-preview probe path and Windows mesh support. No whole feature branch or
dependency branch was merged; their remaining work retains the status below.

- Restore aborts before replacement on Docker inspection failure or a writer that
  remains running; stopped services are restarted on those abort paths.
- Owned volumes restore through clean staging trees, retaining the previous tree.
  An unsuccessful final rename attempts to put the previous tree back. Unrelated
  Docker volumes require explicit `--all-volumes`.
- Backup setup retains off-site credentials/retention and replaces settings only
  after repositories can be opened; paired tokens are restored with instance state.
- Mesh saves lock the complete identity check/write operation and use unique,
  exclusively created temporary files. Unix flock semantics are preserved; Windows
  uses standard file locking. Windows execution was not tested.
- HTTP readiness accepts only 2xx/3xx responses, retaining proxy-based probes for
  isolated previews.

Fresh checks on the combined fixes, before committing:

| Check | Result |
| --- | --- |
| Full Rust workspace, all features | 802 passed, 0 failed, 14 ignored |
| Full Linux host suite in disposable vfybench copy | 248 passed, 0 failed |
| Final Linux backup suite | 15 passed, 0 failed |
| Focused macOS restore regressions | 3 passed, including Docker inspection failure |
| Clippy workspace/all targets/all features, warnings denied | Passed |
| Windows GNU mesh library compile check | Passed |
| Rust formatting, CI ShellCheck, diff whitespace | Passed |
| Generated TypeScript | Unchanged |

An initial Linux full-suite copy omitted repository manifests/structure, causing
four installer test fixture failures. Re-running with those files present passed
all 248 tests. These runs used mocked system commands and disposable copies; no
serving install was upgraded. UI source was unchanged by this follow-up; the 150
unit and 33 browser results below belong to the initial review and were not rerun.
No push, hardware test, or cross-laptop/Tailscale test was performed.

## Original review record

Initial review decision: **not approved for integration**. Main remains at `d0e453927e97e0d8f593b0a67b9fa6aab8a5ecce`; origin/main is `17547d68f0a778856042a5d145db6d096e4354f1`. No merge, push, deployment, or physical laptop change was made. Origin refs were refreshed with `git fetch --all --no-tags`.

## Blocking findings

1. **P1 — Restore writes despite live database writers.** `host/lib/backup.sh:278` warns when `docker stop` fails and continues replacing engine state and volume data. Abort before changing data unless every affected writer is stopped, and resume stopped services on failure. Existing regression `--instance (audit A05): a writer that won't stop means nothing changes, and everything starts again` from `fix/m7-verification` fails against main: the command returns success where failure is required.
2. **P1 — Restore mixes snapshot data with current data and affects unrelated volumes.** `host/lib/backup.sh:291` copies the entire snapshot volume tree over the current tree without deleting newer files. Database files/WAL created after the snapshot survive, so this is not a consistent point-in-time restore. Restore selected owned volumes into clean staging trees, replace atomically while retaining old data, and require explicit scope for unrelated volumes. The branch's A05/A06 regression also fails against main. Tests used a disposable `/tmp/vercelify-review-repro.*` checkout with mocked system commands, never a serving host.
3. **P1 — Mesh state check/write race can undo a fresh pairing.** `crates/vercelify-mesh/src/daemon.rs:633` checks identity and saves without a shared lock; another process can pair between those operations. `state.rs:46` also shares a fixed temporary filename between writers. Carry over the verification branch's locked transaction and unique temporary files, adapting its Unix-only implementation for the current cross-platform code. Blindly replacing main's file would break Windows support.
4. **P2 — HTTP readiness accepts authentication errors and missing paths.** `crates/vercelify-platform/src/health.rs:127` accepts every code below 500, including 401/403/404/429. A bad health endpoint can publish a deployment and displace the working version. `fix/m7-verification` has a 2xx/3xx fix and regression coverage which are absent from main.

## Branch inventory

All 12 local branch tips were compared to main. Eight non-main branches are ancestors of main, and therefore have no unique commits to merge:

| Branch | Disposition |
| --- | --- |
| docs/m6-release | Already in main |
| feat/m3-browser-gateway | Already in main |
| fix/m1-server-lifecycle | Already in main |
| fix/m2-pairing | Already in main |
| fix/m4-second-screen | Already in main |
| fix/m5-workflows | Already in main |
| shashank-agent/installation-product-audit | Already in main |
| sunshine-auto-pair | Already in main |
| fix/audit-findings | Four unique commit IDs; much of the implementation is already in main through the baseline. Reverse-patch checks confirm many files. Other files have newer changes. A full merge conflicts in seven files; do not overwrite newer preview isolation, admission, and recovery behavior. |
| fix/m7-verification | One unique commit; missing restore, locking, readiness, and CI coverage improvements. Eleven files conflict in a trial merge. Not approved without adapted integration and tests. |
| screen-sharing-unpushed | One unique commit; contains additional receiver progress/watchdog, sender restart, even-width validation and a network impairment contract. Five files conflict. Preserve main's newer packet sizing, capture diagnostics, and Moonlight behavior when integrating. Not approved by existing main-only checks. |
| main | Ten commits ahead of origin/main; local checks below apply to this exact tree. Blocking findings remain. |

Trial merges used `git merge-tree --write-tree`, which does not alter the index or working tree. No branch was deleted or rewritten.

The 13 remote Dependabot branches were inventoried and their changed-file scope inspected: Cargo hkdf/rusqlite/tauri/tauri-build/x25519-dalek; Actions checkout/download-artifact/upload-artifact; cockpit framer-motion/lucide-react/Tauri CLI/Vite/xterm addon-fit. These remain **unapproved**: their individual trees were not built or runtime-tested. In particular, hkdf changes its digest generation while sha2 stays at 0.10 and needs an explicit compile check; terminal addon changes need terminal compatibility testing. Main's passing checks do not validate any of these upgrades.

## Fresh validation of main

- `cargo test --workspace --all-features`: **798 passed, 0 failed, 14 ignored**. Ignored Docker/hardware tests are not counted as validation.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed (ts-rs emits existing serde-attribute parse notices).
- `cargo fmt --all -- --check`: passed.
- CI ShellCheck command including scripts, test helpers, and contracts: passed.
- Cockpit typecheck, 150 unit tests, production build: passed.
- Website production build: passed.
- Generated TypeScript: no tracked differences after Rust tests.
- `bats host/tests`: **236 passed, 7 failed** on macOS. Failures: 1, 3, 76, 112, 113, 152, 157; these involve GNU-only stat/sed/touch assumptions. Linux suite not rerun here; do not call this green.
- Cockpit demo browser suite: **33 passed** (3.8 minutes).
- Backup branch regression tests against main: **2 failed**, confirming absent safeguards.

## Limits and preserved local files

This is a branch reconciliation review with local automated validation and focused source findings, not an exhaustive correctness proof of every line in the ten large main commits or individual dependency branches. No binary-size cross-build, Linux namespace/Docker contracts, hardware matrix, or cross-laptop/Tailscale test was performed. Physical testing is deferred per the user's request.

The working tree initially had no tracked edits. Untracked `.agents/`, `.claude/skills/`, `AUDIT-FINDINGS.md`, `HANDOFF.md`, `field-test/`, launch-film/, launch-video/, skills-lock.json, and the unused install-candidate.py were preserved and excluded from commits. HANDOFF explicitly marks these as not to commit and warns that raw field logs may contain tokens. Launch assets and raw logs were not audited as product source.

Next integration work: resolve the four findings above, integrate the three divergent branches selectively while preserving newer main behavior, then rerun tests on the combined tree. Dependency branches need independent validation. Do not mark all branches reviewed-and-approved based on this report.
