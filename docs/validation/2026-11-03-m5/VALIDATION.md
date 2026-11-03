# M5 — fresh PostgreSQL previews

3 November 2026 (Asia/Kolkata). Opt-in trusted-PR PostgreSQL support on main,
non-Compose apps. No production-copy or backup-seed path. No push authorized.

## Behavior and review

A separate PostgreSQL 17 service per app defaults to 256 MiB/one CPU through M4
admission, shared by at most three previews. Each open PR has its own logical
database, restricted login, sealed random password and hidden NOLOGIN database
owner. The owner separation prevents a runtime login from granting cross-PR
CONNECT. Maintenance/PUBLIC access is revoked; new databases start with connection
limit zero until ACLs are installed. Only dedicated internal/M1 preview networks
are allowed. This is not a hostile-tenant resource sandbox.

The runtime-only binding never enters builds or provenance responses. Other
production DB references remain forbidden. Migration/optional synthetic seed run
in capped helpers from the app image, before publication, with URL/password log
redaction. A new commit retains the PR DB; commands rerun. Pools should use at most
three connections against the four-connection role limit. Off-host preview jobs
hash the policy so changing commands fences stale uploads.

Exact ownership is journaled before SQL/Docker work and survives app deletion.
Restart resumes partial creation; close stays closed on failure and retries owned
cleanup. Reopen waits for retirement and rotates identity. Last close retires the
owned service, volume and network; passwords are cleared from deleted journal rows.
App sleep retains data. Unexpected disposable volume loss needs redeploy migration
and seed. Built-in preview DBs cannot move storage tier or publish a host port.

Main-chat review covered PUBLIC defaults, partial role creation, lifecycle fencing,
app-row deletion and pre-publication migration failures. No sub-agents were used.

## Functional evidence

`postgres-docker.txt` runs the real engine and PostgreSQL on the owned disposable
Docker-in-Docker daemon, never the serving daemon. It verifies two PRs with
conflicting migrations and seeds; cross-PR/maintenance connections and privilege
escalation are denied. Production sentinel data remains unchanged, including when
its test credential is deliberately supplied to the preview network fixture.
Partial owner-role creation, SQLite reopen, a real PostgreSQL restart, repeated
same-job provisioning, a full HTTP deployment, failed migration/no publication,
retry, legacy preview cleanup, close/reopen rotation and app deletion before the
cleanup callback are exercised. **All 381 production probes succeeded**; test
passed in 80.87 seconds.

`preview-store.txt`: seven tests cover default-off, command/key bounds, sealed
identity, durable close/app deletion, runtime-only binding and preview-policy job
revision fencing. The revision and direct-deploy cap regressions failed before their fixes and passed after. Closing resources continue to occupy their slot until physical cleanup finishes.
`api-regression.txt`: the existing platform API test passes, including idempotent
removal of a nonexistent preview. The full-suite check initially found that edge
case; the corrected teardown only removes a preview network with recorded ownership.

The first off-host regression began uploading before its target's first capacity
sample and correctly received a 503. The fixture now waits for complete inventory
instead of merely a listening socket. Its corrected run verifies local pack,
changed variables, authenticated import, duplicate/stale/wrong-digest/cancelled/
expired/interrupted/revoked uploads and zero target Git/build commands.

`checks.json`: 757 Rust tests passed (12 opt-in cases ignored by the aggregate
command, with relevant real Docker cases run separately). Clippy, fmt, ShellCheck,
231 Linux bats tests, typecheck, 139 UI unit tests, production build and all 31 demo
browser cases passed. `preview-safety.txt` passes the production credentials,
network, direct-IP/forwarded-header ingress, proxy recreation, Compose isolation
and idempotent cleanup contract in 49.28 seconds. Generated ts-rs trailing whitespace
is the existing documented exception. Static binary sizes/hashes are in
`binaries.json`; both architectures remain below 8 MiB. Physical laptop execution remains unmeasured: it is offline,
and the user explicitly selected the lab VM. Its saved M3 build settings still need
restoration on reconnect. Preserve its safe PR1 and unrelated services.


## Matched footprint and restoration

Final M5 agent is identified in `binaries.json` and `lab-install.txt`. Installed on
four-GiB ARM vfybench with a consistent pre-upgrade SQLite/binary backup, all six
original app definitions preserved. `original-apps.json` verifies all six original
HTTP routes return 200. The disposable Docker daemon and BuildKit were stopped;
no preview DB was enabled on the original apps. After two minutes settling,
twelve samples at six-second intervals plus collection time measured system-used
median **642.5 MiB** (622–655), agent **8.03 MiB**, mesh **3.20 MiB**, BuildKit zero.
M4 was 633.5 MiB/7.70 MiB agent. This one matched-workload observation includes
changes in unchanged daemon footprints; it establishes no fixed memory saving or
feature-specific overhead. Preview PostgreSQL is on demand and adds its own
admitted workload when enabled. Physical x86 runtime and laptop remain unmeasured.
The existing M4 one-GiB profile remains experimental; M5 does not qualify a
one-GiB PostgreSQL configuration. User-owned Mac containers and offline laptop
workloads were untouched. Final binaries remain below 8 MiB on ARM and x86.
