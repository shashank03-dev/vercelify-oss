# M7a — isolated PostgreSQL restore drills

Qualified 3 November 2026 (Asia/Kolkata); documentation finalized 4 November.
Main chat only, no sub-agents, no push. M7b guest links and M8 remain separate work.

Implemented schema-12 manifest/job/service/schedule journals, an isolated temporary
PostgreSQL 17 destination, separate pinned bounded repository reader, metadata or
explicit full-data integrity checks, exact dump streaming, catalog/extensions/query
verification, sealed known-result assertions and recovery settings, restart cleanup,
cancellation, resource/power deferral, monthly UTC scheduling and overdue alerts.
Owner-only controls and schedule assertions are enforced over HTTP; readable job
metadata exposes scope/status without private expected values or recovery settings.
Cockpit controls are capability-gated. Production-copy previews remain disabled.
See SYSTEM-DESIGN §10.0 for limits and the distinction between backup upload,
repository integrity, application restore and physical disaster recovery.

`checks.json` summarizes 779 unique passing Rust cases (14 ignored Docker/hardware
cases), clippy/fmt, 234 Linux bats cases, ShellCheck, UI types/build, 146 UI unit
cases and all 32 demo browser cases. Rust totals combine workspace package runs;
the seven-case restore store binary was replaced by its final nine-case run.
Linux ran 229 unprivileged cases plus the five root-only installer cases; all six
installer cases also passed in the root rerun. Generated TypeScript is unchanged.
Both static agents are below 8 MiB; exact sizes/hashes are in `binaries.json`.

`restore-drills.txt`: real Docker fault matrix passed in 123.28 seconds, with 853
unchanged production probes and zero failures. It verifies the selected snapshot's
20,000 rows/index/migration after production advances to 20,001/new marker;
restored state never rewinds production. Wrong/missing keys, wrong assertions,
missing/invalid dump, incompatible extensions and ciphertext pack corruption fail.
Explicit full repository reading is distinguished from metadata-only checks.
Cancellation during verification stays unverified. A real SIGKILL of a controller
process followed by a reopened SQLite journal cleans its unverified destination.
Simulated low battery and exhausted measured memory defer work; cancellation cleans
pending ownership, including metadata prepared before any container starts.

The live observer checks destination/reader CPU and memory/swap limits, disabled
Docker logs, read-only repository mounts, no host port, isolated internal destination
bridge and absence of proxy membership even after preview-network reconnect.
`preview-safety.txt` separately requalifies real production/management/ingress denial,
forwarded-header/direct-IP bypasses, proxy recreation and Compose isolation (56.82s).
`postgres-previews.txt` passes migration/role/ownership recovery with 396 unchanged
production probes (83.36s). `engine.txt` passes deploy/rollout/rollback/recovery and
wake storm (133.39s). These contracts ran only inside the disposable nested daemon.

The clean logical recovery fixture copies the encrypted repository and a mode-0600
recovery artifact into another control directory, uses a different master key and
no original source row, and deliberately supplies a wrong host credential. Provided
recovery settings succeed and are cleared afterward. This is one-VM logical recovery
evidence, not an off-device physical machine-loss certification. Remote repository
providers are implemented but not live-qualified. Other engines/majors and legacy
backups without manifests remain unsupported, not implicitly verified.

`lab-install.txt` records the consistent pre-upgrade binary/control backup and the
installed schema-12 ARM hash. `native-drill.txt` exercises the installed HTTP API and
actual background runner, independently supplied repository key, full-data checking,
128-row/migration assertions and physical source/destination cleanup. All 438 probes
to the original six applications succeeded. A private synthetic recovery artifact
and encrypted repository remain at the recorded lab path; no running fixture remains.

`vm-steady.txt/json` records twelve matched six-app samples after two-minute settle:
system used median **589.5 MiB** (581–599), agent **9.02 MiB**, mesh **2.80 MiB**,
BuildKit zero. Original app IDs and all six HTTP-200 routes are preserved in
`original-apps.json`. M6 was 588 MiB/8.42 MiB agent. Both runs follow VM restart;
unchanged daemon RSS/cache varies, so this is not a causal overhead/savings claim.
Restore peak usage is separately constrained by destination/reader admission/caps.

Qualification corrections: the initial HTTP fixture accidentally requested HTTPS;
its explicit plain-HTTP domain fixed the probe. A real run caught ordinary bridge
creation for the temporary service; dedicated isolated restore-network creation and
an ingress reconnect regression fixed it before qualification. A store regression
caught generic variable interpolation into quarantine; this is now refused along
with link/terminal/patch/move/start/backup/restore controls. The link guard preserves
existing unresolved-reference semantics. A test restored an older capacity timestamp;
refreshing its synthetic timestamp fixed the fixture. Simultaneous Playwright runs
collided in their shared artifact directory; a single final run with a separate
output directory passed all 32 cases. An orchestration helper found multiple cached
binary hashes; it now selects the newest compiled executable and preserves completed
results. Earlier failed attempts are never counted as passing qualification.

The first installed-agent attempt exceeded its source-image download wait, leaving
only the owned synthetic source record. On resume the VM was stopped; it was started,
the prior exact fixture was retired and the cached-image native flow passed. The
helper now waits for the database operation to finish and handles a missing container
on cleanup. This qualifies the cached-image flow; cold download timing is not claimed.
The physical laptop remains unmeasured/offline from the prior qualification and its
saved M3 build settings still need restoring when it reconnects. No unrelated Mac
containers, original lab app definitions or physical laptop services were changed.
