# M7a — isolated PostgreSQL restore drills

Continue the explicitly approved M1–M8 roadmap in the main chat. No sub-agents,
no push. Freeze M6 in the index first; commit its tested snapshot before staging M7. M7a and M7b are separate commits/releases.
Binding spec: companion competitive validation §7.2; roadmap M7 acceptance.

## Initial supported scope

PostgreSQL logical dumps only, with explicit recorded version/extension/manifest
compatibility. Other database kinds and legacy backups without that metadata
remain unsupported by the drill rather than becoming implicitly verified.
Never restore over production. Repository integrity and application restore are
separate results. No production data-copy preview feature is enabled here.

## Design

- Add a versioned backup manifest recording immutable database/snapshot identity,
  engine version, required extensions, dump filename/format and byte count. Record
  selected consistent assertions/migration marker at backup time; do not compare
  a restored backup against a live production row count captured later.
- A durable drill job owns a dedicated temporary PostgreSQL service, restricted
  internal bridge (M1 isolated gateway, no egress/proxy/production attachment),
  sealed random credential and volume. Persist ownership before external work.
  M4 admits/caps the destination, streaming helpers and disk/maintenance work.
  Only main is initially supported. Never mount production volumes or supply its
  credentials, and never expose the restored database through a host port.
- Reuse restic streaming and supported PostgreSQL tooling with bounded timeout.
  Verify repository/snapshot/manifest before restore. Restore without source
  owners/ACLs; check expected objects, captured assertions/migration marker and
  read/query smoke checks. Full data verification is explicitly scoped and
  separately recorded; a successful upload is not restore proof.
- Persist snapshot age, selected assertion scope, elapsed restore/verification
  time, bytes, status, evidence and cleanup state. Report observed RPO/RTO, never
  a guaranteed service objective. Missing password/key, corrupt/missing dump,
  unsupported version/extensions and failed assertions remain visible failures.
- Cleanup/restart uses the exact journal, including partial creates. A completed
  result never hides a leftover destination/volume/network. Generic database
  port/link/move operations cannot expose or repurpose a quarantine service.
- Host-key automatic drills prove only that recorded host credential works.
  An independently supplied recovery artifact/fresh-machine fixture is labelled
  separately; using the original machine's key is never called machine-loss
  recovery. No plaintext recovery credential enters job/API/log output.
- On demand plus a modest configurable monthly schedule; serialize heavy work,
  defer safely under pressure/low battery and keep overdue/capacity status visible.
  Reuse existing jobs/settings/alerts, with no additional daemon.
- Cockpit shows the last verified snapshot and assertion scope/age, never a broad
  "all backups proven" badge. Capability-gate controls on older agents.

## Execution gates

1. Red/green store/model tests: manifest identity/version, exact destination
   ownership, failed/overdue/cleanup states, credential redaction and restart.
2. Isolated destination and restore/verification pipeline, with independent
   repository-integrity and semantic outcomes. Production cannot be a target.
3. API/job/schedule and cockpit/demo status. Generated types via cargo tests.
4. Real Docker fixtures: valid dataset/index/migration marker, corrupt and missing
   dump, incompatible engine/extensions, missing/wrong key, assertion failure,
   capacity deferral and restart during restore/cleanup. Probe unchanged serving
   throughout and assert destination has no public/production/management path.
5. Full relevant checks/static-size gate; matched original six-app memory per
   research §6, honest hardware unknowns. Update SYSTEM-DESIGN/PLATFORM/evidence,
   exact-file commit. Continue M7b guest links only after its privacy contract.

Execution ruling: prepare the M7a journal/tests while the frozen M6 candidate is
measured. No M7 source/type/schema changes may enter the staged M6 snapshot.

## Implementation rulings and scope

The first manifest records PostgreSQL 17 major/extension versions and relation/index
catalog, not arbitrary migration semantics. Known-result owner assertions cover
selected application invariants; they are not captured from a later live count.
Concurrent DDL mismatches conservatively fail. Restic authenticates snapshot data;
selected dump byte count is additionally exact. Unsupported majors/legacy backups
require a new supported backup.

Repository metadata check is the default; full-data reading is explicit and separately
reported. The bounded official restic helper has repository credentials but no
source-service credential/app environment; the isolated destination has fresh local
credentials and no repository credentials. A dedicated restore-network label avoids
proxy reconnect. Dump/database logs are disabled. Local repository mounts are
read-only; remote backends are implemented but not live-qualified. The dataset itself
may contain secrets and executable database definitions. No production-copy preview
beta or generalized hostile-dump sandbox is claimed.

Drills cap at twenty queued/active globally, serialize heavy work, and admit destination
384 MiB including overhead plus reader 512 MiB and conservative disk headroom.
Destination/reader caps are 256/512 MiB, 1 CPU each, swap equal memory; active power/
heat/low-disk checks abort into cleanup/deferral. Three mid-work attempts maximum.
The first SQL assertion UI edits one check while preserving additional API-supplied
checks. No assertion/recovery secret enters readable metadata; schedule GET is Owner-only.

Monthly schedules start disabled. Restart catch-up coalesces missed occurrences;
missing backups keep overdue status. The existing doctor/watcher surfaces failed
latest drills, overdue schedules and deferral/cleanup beyond five minutes. Interrupted
jobs discard unverified data and resume cleanup rather than silently restarting restore.

The recovery test copies an encrypted repository and mode-0600 recovery artifact
into a fresh logical control environment with a different master key and deliberately
wrong host credential. It establishes independent provided-key recovery on the lab
VM, not an off-device physical machine-loss certification. That remains unmeasured.
