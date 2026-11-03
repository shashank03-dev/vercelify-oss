# Isolated PostgreSQL previews implementation plan

Execution: `superpowers:executing-plans`, main chat only, as explicitly requested
by Shashank. No sub-agents. The approved M1–M8 roadmap supplies the design and
execution authorization; continue without another approval round. M4 is committed
as `16d8b27`. One plain M5 commit after all checks/evidence; no push.

**Goal:** Two PRs run conflicting PostgreSQL migrations and synthetic seed data
without accessing production or each other's database, with durable owned cleanup.

**Architecture:** One optional dedicated preview PostgreSQL service per app,
separate from production. Each PR lifecycle has a fresh logical database, restricted
role and sealed credential. Share the bounded preview process, not production's
cluster. Its isolated base network and the PR's existing M1 isolated network are
the only attachments. Initial built-in support is PostgreSQL on the main node.

**Tech stack:** Existing Rust/SQLite/master-key sealing, Docker/PostgreSQL 17,
M1 preview bridges, M4 admission, current React primitives and generated TS types.

**Spec:** Competitive roadmap §9 M5; companion validation §6.2; SYSTEM-DESIGN §11.

## Constraints and decisions

- Keep the selected-computer/CI build path. Fresh DB credentials are runtime-only,
  never supplied to builds. Explicit enabling is required; existing settings stay off.
- Configuration: optional `PreviewSettings.database` with a connection variable
  (default DATABASE_URL), migration and optional synthetic-seed commands. Validate
  keys/command bounds. Reject unsupported Compose/remote built-in DB combinations
  before side effects. No production copy or backup-seed path in M5.
- Choose the companion's recommended dedicated shared preview service. Schema in
  production is rejected by the binding isolation requirement; a separate process
  for each PR remains a later opt-in and is not needed for this first release.
- PG service: 256 MiB/one CPU preset via M4, no host port, no production network.
  A hidden NOLOGIN role owns each database, preventing the runtime login from
  granting cross-PR CONNECT. Per-PR runtime roles: LOGIN, NOSUPERUSER, NOCREATEDB, NOCREATEROLE, NOREPLICATION,
  NOBYPASSRLS, connection limit 4. Revoke PUBLIC CONNECT on every service database
  including maintenance databases; only the owned role connects to its database.
- Retain data while the PR is open; close/app deletion revokes and removes it.
  App sleep does not claim per-DB sleep. Database-enabled apps are bounded to three active PRs. Stop/remove the owned shared service after the last dependent
  closes; no automatic copying of production data, roles, extensions or secrets.
- Journal identifiers before external work. Reopening a closed PR waits for owned
  cleanup and gets new credentials/database identity; pushing a new SHA to the
  same open PR keeps its DB and reruns migration under the app's existing guard.
- Close remains durably closed even on cleanup failure. Background reconciliation
  retries exact recorded resources. Never reopen merely because teardown failed.
- Preserve user-owned untracked files, lab originals and offline laptop state.

## Review focus

1. Existing PostgreSQL PUBLIC defaults must not permit cross-PR or maintenance DB
   connections; verify with actual PR credentials, not only generated SQL strings.
2. A crash between role creation and DB creation must resume idempotently without
   adopting an unowned pre-existing role/database.
3. Closing/reopening during migration must fence late publication and cleanup by
   lifecycle identity, without deleting the new generation.
4. App deletion must leave durable owned cleanup metadata; foreign-key cascades
   must not discard the only link to credentials, roles, volumes or service IDs.
5. Capacity refusal, migration failure, seed failure and missing images leave
   production serving and never expose administrator credentials in app env/logs.

## Tasks

- [x] Add failing store/model tests for off-by-default settings, validation,
  per-lifecycle ownership, sealed credentials, reopen rotation and close fencing.
  Files: core/platform.rs, platform/src/preview_db.rs, migrations/0010_preview_db.sql,
  store.rs and tests/preview_db.rs. Interface: `prepare(c,key,app,pr) -> Binding`,
  `mark_closing(c,app,pr)`, owned service/resource queries and explicit states.
- [x] Implement journaled logical resources and separate bounded service mapping;
  tests must pass without weakening M1's production-reference denial. Add runtime
  binding provenance and strip only the explicitly bound variable from build vars.
  Keep `env::resolve_for_deploy` production behavior unchanged.
- [x] Integrate Engine provisioning and migration/seed-before-publish using the
  existing app operation/cancellation fences. SQL goes over stdin, credentials
  stay sealed and logs redact both URL and password. Reuse preview networking;
  dbengine must refuse a generic/production bridge for a preview service. Pin
  built-in DB previews to main and hold M4 operation budgets throughout.
- [x] Close: mark closed/closing first, cancel work, revoke LOGIN, terminate only
  that role's sessions, remove its DB/role, detach only its bridge, retire service
  only when no dependents remain. Reconcile after restart, retries and app delete.
  Add crash-point and failed-cleanup tests; never use preview reopen as rollback.
- [x] Cockpit configuration/status and provenance use existing fields/primitives;
  demo mirrors fresh DB states. Generate types via Rust tests. Add unit/demo tests
  for opt-in settings, failure status and runtime-only provenance without secrets.
- [x] Real Docker contract: production sentinel plus two PRs with conflicting
  migrations/seed values; prove cross-PR/production/maintenance access denied,
  migration failure cannot publish, close/restart/retry/reopen cleans only owned
  resources and rotates credentials. Probe unchanged production throughout.
- [x] Full Rust/clippy/fmt, Linux bats/ShellCheck, UI types/unit/build/demo, relevant
  preview/DB/admission regressions; cross-agent size. Matched §6 VM memory with
  original six apps and disposable daemon stopped. Laptop only if reachable.
- [x] Update PLATFORM, SYSTEM-DESIGN, research §6 and M5 evidence; main-chat
  self-review against the five failure cases, exact-file commit, report and M6.

M6 follow-ups discovered by M4, preserved in its evidence: shared project network
cleanup must consider stopped peers; superseded restart-always containers were
observed after 1/2 GiB reboots; healthy state must not retain stale queued text.

Qualification evidence: `docs/validation/2026-11-03-m5/VALIDATION.md`.
757 Rust, 231 bats, 139 UI and 31 browser cases passed, plus real PG/M1/off-host
contracts. Matched six-app sample complete; laptop remains explicitly unmeasured.
