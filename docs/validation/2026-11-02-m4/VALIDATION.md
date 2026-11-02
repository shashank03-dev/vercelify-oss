# M4 — small-host admission

3 November 2026 (Asia/Kolkata). Implemented and verified for the recorded configurations below. The one-GiB
profile remains experimental; known recovery findings carry into M6. No push
is authorized.

## Implemented behavior

One durable SQLite resource account covers placement, build/deploy overlap,
image upload/import, database start/restore, wake and reconciliation starts.
Unknown/stale reports defer new work. Restart invalidates old samples and retains
unreconciled promises. Fresh inventory credits work already visible without
spending the unused growth of existing app/database caps twice. Host-manager
configuration additionally requires a fresh accepted grant and physical report.

New app, database, dedicated BuildKit and Compose operations enforce memory,
swap and CPU bounds. Existing unlimited apps/databases are reported as estimates;
they are not recreated solely to add a cap. PostgreSQL defaults include a bounded
connection/buffer preset. Compose divides an aggregate budget across services and
fixes replica counts. A running legacy builder with mismatched limits waits for
its idle stop; updating a stopped builder retains its cache.

The memory API and Health card separate measured working memory, system use,
growth, reservations, caps/estimates and admission headroom. Missing stays unknown.
OOM is based on explicit Docker/cgroup/kernel evidence, not exit 137. The release
profile aborts on Rust panic and relies on installed service supervision; Docker
workloads serve independently. See SYSTEM-DESIGN §§8.3 and 16.

## Functional evidence

- `admission-docker.txt`: simultaneous requests cannot spend one budget twice;
  the other request remains queued. Actual memory/swap/CPU limits inspected.
  Abrupt agent restart preserves serving and unsafe work stays deferred. An
  unrelated Node process dirties 700 MiB; work resumes after pressure exits.
  Actual ENOSPC on the fixture's own 64 MiB tmpfs starts no unsafe workload;
  restoring disk resumes its queue. **All 576 serving probes passed.**
- `databases-docker.txt`: PostgreSQL/Valkey create, concurrent ensure, health,
  backup, restore, failed-restore preservation, storage-tier move, public port and
  removal passed in 27.37 seconds. A later larger restore test passed in 24.39
  seconds with a 20,000-row PostgreSQL table/index and Valkey restore, while all
  234 requests to an unchanged serving app succeeded; kernel OOM delta zero
  (`restore-serving.txt`, `host/tests/contract/restore-load.py`). File restore helpers are capped at 128 MiB,
  equal memory/swap limits and one CPU.
- `preview-safety.txt`: credentials, production DB/management access, direct IP
  and forwarded-header bypass denial, proxy recreation, Compose isolation and
  idempotent cleanup passed in 41.71 seconds.
- `engine-docker.txt`: deploy, rollout, rollback, recovery, Compose and a storm of
  16 simultaneous wake callers passed in 105.33 seconds. Declared replicas remain
  unchanged. The test uses a 1 GiB dedicated builder for its tiny fixture.
- `builder-idle.txt`: three cold/warm/idle cycles passed, retaining cache and never
  stopping an active held build.
- `offhost-builds.txt`: actual local shared pack and authenticated Docker import
  passed; secrets are redacted, changed variables invalidate output, and stale,
  duplicate, cancelled, expired, interrupted and revoked uploads cannot publish.
  Zero target Git/build commands and no leftover upload files.
- `supervisor.txt`: installed agent SIGABRT, systemd PID replacement and active
  recovery; **all 169 unchanged Next HTTP probes passed**.
- `checks.json`: full Rust workspace/all-features: 749 passed, 11 opt-in tests
  ignored by that command; relevant real Docker cases run separately above.
  Fifteen ledger tests passed. Clippy, fmt, ShellCheck, 231 Linux bats tests,
  cockpit typecheck/production build, 135 UI unit tests and all 30 demo browser
  cases passed. Generated ts-rs whitespace is the existing documented exception.
- `binaries.json`: final static ARM agent 5,980,208 bytes, x86 agent 7,186,200 bytes;
  both below 8 MiB. Final installed candidate is identified by the steady samples.

## Resource measurements and scope

`vm-steady.txt/json`: four-GiB ARM vfybench, same six original apps, BuildKit and
its disposable Docker daemon stopped, two-minute settle, twelve samples at
six-second intervals plus collection time. System used median **633.5 MiB**
(623–650), agent RSS **7.70 MiB**, mesh **2.90 MiB**, BuildKit zero. M3 system use
was 685.5 MiB; unchanged daemon footprints also differ. This one comparison does
not establish a fixed saving. Research §6 includes the component table.

`profile-matrix.py` runs the separate 1/2/4 GiB experiments with one pinned cached
Next image, a 192 MiB cap, one CPU, the default ten-second drain, swap/builds disabled and no database. Each
profile runs three rollouts under four clients paced at 250 ms each for 60 seconds,
then a real VM stop/start and another load run. Roughly one-second host samples
and final container/cgroup capacity evidence are saved. All four runs per profile passed: 3,792 / 3,780 / 3,743 requests for 1 / 2 / 4 GiB,
zero failures and zero kernel OOM kills. Minimum sampled available RAM was
318.5 / 1,255.7 / 3,094.1 MiB respectively. Raw results: `profile-matrix.json`,
`profile-1g-samples.json`, `profile-2g-samples.json`, `profile-4g-samples.json`. The one-GiB profile remains experimental. These
cached-image tests do not establish cold-download/import or arbitrary-app support.

`lab-environment.txt` pins OS, kernel, Docker and image identity. Host-manager
physical grant/provider integration has unit coverage; this native Linux lab does
not qualify an actual managed Mac/Windows provider. Physical x86 M4 execution and
laptop memory remain **unmeasured**. The laptop is still offline; the user chose
lab continuation. Its saved M3 build settings still need restoration when it
returns; preserve the safe PR1 and unrelated services (M3 evidence).

## Qualification corrections and cleanup

Initial Docker qualification found watch_interval=0 disabling capacity reports;
reporting now runs independently and live regression passes. The disk test fixture
now preserves proxy bind directory identity and waits for asynchronous app removal
before restart. The engine test initially correctly deferred a 3 GiB operation
against a 2 GiB tmpfs; its passing run uses disk-backed TMPDIR. An initial supervisor
probe had no SSH forward; the corrected run checks a valid baseline before killing
anything. These initial attempts are not counted as passing qualification.

The final label-only correction says “Below the admission minimum”, avoiding a
claim that every admitted RAM size was tested. The original lab binary/config/fstab
are saved in its private qualification directory. The matrix finally block restored
four GiB RAM, original configuration/swap and all six original apps. Restoration
was verified, including all six original HTTP routes (`matrix-restored.txt`). No unrelated Mac containers, Docker
contexts, physical laptop workloads or user-owned files are changed or committed.

A separate zero-second-drain one-GiB attempt passed its first 948-request rollout,
then had four failures in the second 948-request rollout. Kernel OOM stayed zero
and minimum sampled available RAM was 363.4 MiB. This configuration is not
qualified (`profile-zero-drain-*`). The matrix is rerunning with the platform's
default ten-second drain; it does not claim zero-drain continuity.

That attempt also exposed an existing cleanup limitation: deleting an app in a
shared project while every peer is stopped can remove the shared Docker network.
The six stopped lab peers were reattached to a recreated project network and
restored. The matrix now uses its own project. Preserve this finding for M6's
recovery/cleanup work; the existing deletion path must check stored ownership,
not rely on Docker refusing removal. The runner's four-GiB freshness check was
also corrected to account for guest-reserved RAM (3905 MiB visible of 4096 MiB).

The one-GiB reboot observation also retained two 192 MiB versions of the same
app (one superseded). Admission counted both, leaving 60 MiB of logical headroom;
serving stayed successful with no OOM. This is not a claim of single-container
reboot cleanup. M6 must reconcile superseded restart-always containers, as well
as avoid retaining old “queued” text on a healthy deployment. These observations
are preserved in the matrix capacity inventory and API state.
