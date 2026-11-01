# Competitive validation and engineering recommendations

1 November 2026. Companion to [COOLIFY-COMPETITIVE.md](COOLIFY-COMPETITIVE.md).
This is a review and proposed delivery approach, not product implementation approval.
Recommendations and numerical pilot/acceptance targets are explicitly proposals.

## 1. Decision

Proceed with the problem, revise the positioning and sequence. The strongest initial
offer is: **deploy to hardware you own, keep builds away from serving workloads,
and understand whether the machine has capacity.** Safe previews and dependable
recovery make that offer useful beyond a demo. Electricity estimates and power
features alone are unlikely to justify switching; that is a product hypothesis,
not established market data.

Assumed initial audience, based on `CLAUDE.md`: solo developers and small projects
using a spare Linux laptop/mini-PC, with a Mac or another computer for development.
Support qualified small VPSes too, but do not make enterprise teams, arbitrary
hostile tenants, or Kubernetes migration the initial success criterion. Validate
the audience with a pilot before treating it as proven.

Three real approaches were considered:

| Approach | Advantage | Cost / limitation | Recommendation |
| --- | --- | --- | --- |
| Owned-hardware workflow: easy first deploy, off-host build, resource safety, private previews | Closest to the stated product goal and existing agent/mesh foundation | Needs reliable local installation, architecture handling and home-network diagnostics | Start here |
| Broad Coolify/Dokploy parity: browser UI, dozens of templates, roles, every integration | Familiar evaluation checklist | Much larger maintenance surface; weak reason to change from an established tool | Add only observed blockers |
| Preview/database platform first | Attractive for teams reviewing multiple branches | Data isolation, migrations, secrets and resource demands make this expensive on tiny hosts | Start with safe empty/seeded Postgres previews after the base path works |

The earlier eight milestones bundled several independent projects and delayed
first-deploy fixes and trust work. Use the revised sequence in §9 of the main
document. M7a and M7b are separate releases; battery support and backup-copy beta
also have separate gates. Do not sell a single large milestone as a small task.

## 2. What the research actually establishes

### 2.1 Verified and corrected claims

| Earlier claim | Verdict | Evidence / correction |
| --- | --- | --- |
| The five competitors require rented hardware | False | Deployment hosts can be owned machines; [Kamal's local builder is documented](https://kamal-deploy.org/docs/configuration/builders/) |
| Nobody builds on a developer's computer and ships the result | False | Kamal does; [Dokku supports registry and streamed archive deployment](https://dokku.com/docs/deployment/methods/image/) |
| Competitors' only answer is an always-on second build server | Too strong | [Coolify build roles/fallback](https://coolify.io/docs/core/infrastructure/servers/build-servers) and external CI/image workflows already separate build and run; automatic opportunistic pairing can still improve UX |
| Every tool only rolls back when clicked | False | [Dokploy supports automatic health-based Swarm rollback](https://docs.dokploy.com/docs/core/applications/rollbacks); it is configuration-dependent |
| Coolify/Dokploy previews are necessarily public and unprotected | False | [Coolify basic auth](https://coolify.io/docs/core/networking/proxy/traefik/basic-auth); [Dokploy preview security inheritance](https://docs.dokploy.com/docs/core/applications/preview-deployments) |
| Vercel password protection costs $20/project/month on Pro | Confirmed, dated | [9 September 2026 announcement](https://vercel.com/changelog/password-protection-now-costs-20-per-project-per-month-on-pro); do not confuse it with all preview protection |
| Vercel has no equivalent accountless link workflow | False | [Sharable links](https://vercel.com/docs/deployment-protection/methods-to-bypass-deployment-protection/sharable-links), including limited Hobby access |
| A restored dump is Neon's branching, free | False equivalence | [Neon's copy-on-write architecture](https://neon.com/blog/instantly-copy-tb-size-datasets-the-magic-of-copy-on-write) differs; restores consume host resources and take workload-dependent time |
| Restore is I/O, not memory | False | Restore runs database processes, decompression, buffers and index work; admission must reserve memory too |
| Nobody tests restores | Unestablished universal negative | Reviewed sources do not establish absence across the market; call it a proposed integrated workflow |
| 624 MiB vs 9 MiB compares control planes | Inconsistent boundary | 624 is the earlier whole-platform delta including Docker/proxy; compare to 142, with benchmark caveats |
| A 870 MiB delta is the only reason for a 4 GB minimum | Not proven | One coarse-sampled build on a 4 GiB ARM VM cannot establish a supported minimum or original design rationale |
| A mesh makes a signed share link externally reachable | False | Recipient still needs public ingress, a tunnel, or mesh access |
| No PaaS tracker requests a share CLI, so there is no demand | Invalid inference | [Tailscale Funnel](https://tailscale.com/docs/features/tailscale-funnel) demonstrates existing adjacent capability; our segment's demand is still unknown |
| CPU/battery watts equal AC wall consumption | False | [Kernel powercap zones](https://docs.kernel.org/power/powercap/powercap.html) cover particular components; coverage must be stated |

Scope matters: [Kubero](https://www.kubero.dev/) also documents PR apps, basic auth
and sleeping containers. A five-product sample cannot justify “no open-source PaaS.”
The opportunity is fewer steps and better defaults in a coherent workflow, not an
architectural inability of other projects to compete.

### 2.2 Direct demand versus implementation inference

Fresh API results are saved in [github.json](evidence/2026-11-01/github.json).
Reaction totals include all reaction types; they are not unique interested buyers.

| Source | Verified state / reactions | Supports | Does not establish |
| --- | --- | --- | --- |
| [Coolify #6656](https://github.com/coollabsio/coolify/issues/6656) | Open / 4 | A reported Next.js build contention failure | All Next.js builds fail; our benchmark app has the same requirements |
| [Dokploy #711](https://github.com/Dokploy/dokploy/issues/711) | Closed / 50 | Desire to separate builds from serving | An unmet current build-server gap or preference for Mac scheduling |
| [Dokploy #2386](https://github.com/Dokploy/dokploy/issues/2386) | Open / 24 | Pre-built-image preview deployments | Demand for only desktop builds |
| [Dokploy #2462](https://github.com/Dokploy/dokploy/issues/2462) | Open / 29 | Isolated preview DB/schema and migrations | Nightly production-data restores or automatic PII scrubbing |
| [Dokploy #319](https://github.com/Dokploy/dokploy/issues/319) | Closed / 69 | Historical rollback usability demand | Present absence of rollback or preference for a ten-minute watcher |
| [Dokploy #2154](https://github.com/Dokploy/dokploy/issues/2154) | Open / 26 | Sleeping low-traffic workloads | Wake-on-LAN demand or uniqueness of scale-to-zero |
| [Coolify #2389](https://github.com/coollabsio/coolify/issues/2389) | Open / 38 | Better backup management and restore UX | Specific monthly automated restore-drill demand |
| [Dokploy #416](https://github.com/Dokploy/dokploy/issues/416) | Open / 29 | More backup destinations | Restore drill frequency or backup-seeded previews |

Saved [search queries](evidence/2026-11-01/search-counts.json) reproduce 172/80
“build server” matches and 13 Coolify “preview database” matches. Two “restore test”
queries were rate-limited; their earlier 4/11 counts are not revalidated. Search
counts include open and closed issues, overlapping discussions and incidental
mentions. Do not sum them, score market size with them, or call a historical issue
an unmet current gap without checking docs and resolution.

No fresh user interviews, switching commitments, retention data or willingness-to-pay
evidence were collected. Vendor docs prove a documented capability, not demand;
HN comments can identify an experience, not its prevalence. Current docs also do not
prove an earlier vendor release supported the same feature. A versioned comparison
needs a pinned release and a real test on each side.

## 3. Existing code changes the engineering scope

This review inspected a dirty tree. [Code hashes](evidence/2026-11-01/code-snapshot.json)
identify key contents; they are not a tagged release. No runtime tests were executed
for these findings.

- `engine.rs::watch_after_deploy` already watches for 180 seconds at ten-second
  intervals. It handles nonrunning/restarting containers, describes OOM evidence,
  and excludes rollback-triggered deployments from recursive watching. It skips
  unreachable nodes and exits for removed containers. It does not implement the
  proposed durable ten-minute health-failure state machine. Its event can currently
  say “rolled back” after **enqueue**, before the replacement has succeeded.
- `placement.rs::Reservations` atomically subtracts concurrent placement reservations,
  uses a 256 MB default, and retains started reservations for 90 seconds. It is
  in-memory. Extend this design rather than introducing a competing broker. Its
  restart assumptions need review against real containers and fresh node samples.
- `env.rs::resolve` merges normal groups and then preview-only groups. Production
  inheritance is real behavior, not an accidental UI-only issue. The safe policy
  must apply before reference expansion and before either build or runtime dispatch.
- `proxy.rs` listens on web/websecure and writes domain routers; `hosts_for` can
  generate a wildcard preview hostname. The inspected path does not establish a
  universal mesh-only preview policy. Public forwards/tunnels must be tested too.
- `registry.rs` uses a mesh/loopback-bound registry, and workers see a local forwarding
  endpoint. A Docker daemon inside a Mac VM has a different loopback namespace from
  a native desktop CLI. Its route to that endpoint must be explicitly implemented
  and tested. Network encryption alone does not scope upload authority.
- `backup.sh` creates `pg_dumpall` output, other engine-specific dumps and volume
  snapshots. Dump failure marks the backup status failed, but a snapshot can still
  exist. Selecting restic's unqualified `latest` does not prove the required DB dump
  succeeded, is fresh, or even belongs to the intended host.
- `power-guard.sh` already performs a low-battery shutdown with repeated readings.
  Optional `install-mac.sh --host` installs a native host manager with a Linux guest;
  the default desktop/mesh setup is not proof of an always-available Mac build worker.

Align new build/resource work with
[the existing host design](../superpowers/specs/2026-11-01-cross-platform-hosting-design.md)
and its [qualification review](../superpowers/specs/2026-11-01-cross-platform-hosting-review.md).
Reuse provider, resource-grant and managed-environment abstractions; do not create
a second Lima installation or count guest free RAM as physical-host availability.

## 4. Off-host builds: recommended architecture

### 4.1 Choose the smallest useful path

1. **First release: explicit local build and external CI image deployment.**
   `--build-here` is a proposed CLI surface, not an existing documented command.
   Reuse the current detector/Dockerfile generation, build variables and health gate.
   Dockerfile/image apps should not be forced through an unrelated buildpack path.
2. **Next: enrolled build workers using the same job contract.** A native Linux
   worker is the preferred fast path for a matching target architecture. A Mac uses
   a qualified Linux container runtime/provider. Native or emulated builds are
   reported explicitly.
3. **Later: opt-in opportunistic selection.** Add automatic desktop scheduling only
   after job ownership, sleep interruption and fallback semantics work. Do not add
   a distributed scheduler solely to ship the first local-build flow.

External CI is an equal supported escape hatch: a user need not keep a personal
computer awake for every webhook. Show “queued: no permitted builder” with
cancel/retry when none is available. A serving-host build requires explicit policy
permission and successful admission; swap availability is not permission.

### 4.2 Shared job contract and ownership

Recommended persisted record: deployment ID, app/environment/PR, exact commit or
source archive hash, build-plan version/hash, required `linux/architecture`, variable
revision, owner generation, builder identity, deadline, phase and eventual image
digest. Treat this as a proposed schema, not existing models.

Use phases such as `queued → leased → building → uploading → verified → deployable`;
terminal failure/cancellation must survive process restart. A lease alone cannot
stop a disconnected old worker. Fence publication by generation: an expired worker
may finish locally, but cannot publish or promote a stale result. Worker heartbeat,
timeouts, cancellation and idempotent retry are required. CLI and desktop observe
the same server-owned job; quitting the window must not duplicate deployment.

Deploy only after the registry manifest/blobs are complete, target-platform metadata
matches, digest is verified and the deployment is still current. Bind artifact
authorization to app + job + generation; never trust a client-supplied image name as
permission. Retain previous images through the rollback window and budget registry
storage/garbage collection. If repository-scoped registry credentials are unavailable,
use an authenticated upload gateway; mesh membership alone must not permit replacing
another app's artifacts. Don't expose an unauthenticated raw registry to all devices.

### 4.3 Mac, architecture and trust boundaries

Apple Silicon producing an ARM image does not solve serving an old x86 laptop.
Set the requested platform explicitly. Qualify arm64→arm64 and arm64→amd64, including
native dependencies and runtime smoke probes. [Docker warns that QEMU emulation can
be slower](https://docs.docker.com/build/building/multi-platform/); compare total
build+upload+pull time, not just compilation. Prefer a native worker/CI for demanding
x86 builds. Do not promise faster builds from a single native ARM benchmark.

Inspect the chosen runtime without changing global Docker contexts or other projects'
configuration. First try an explicitly selected compatible runtime; offer the existing
managed provider when absent. Publish runtime prerequisites, disk overhead and which
runtime receives cleanup. No default home-directory/SSH-agent/Docker-socket mounts
into build steps. Honor `.dockerignore`; exclude local secrets and bulky irrelevant
files; exact-commit builds are the default, with dirty-local builds explicitly marked.

Only send variables approved for this build/environment. [Use BuildKit secret mounts
for build secrets](https://docs.docker.com/build/building/secrets/); they avoid automatic
layer persistence but cannot stop malicious build code from exfiltrating an authorized
secret. Public client bundles intentionally embed public variables; explain that
separately. Isolate caches between trust domains and redact logs. No unattended
untrusted fork build on a personal machine in the first release.

**Required contract cases:** missing runtime, unavailable/emulated target platform,
Mac sleep during build/upload, expired lease, duplicate completion, PR close/new
commit during upload, revoked worker, full disk, interrupted registry, wrong digest,
unauthorized repository and no permitted fallback. An unchanged production app must
remain reachable throughout failed handoffs.

## 5. Small-host admission: extend placement

A prediction cannot guarantee no OOM. The goal is to reject obviously unsafe work,
enforce limits and explain observed failures. Keep a single capacity policy shared
by placement, builders, preview DBs, restores and host-manager grants.

Recommended components:

- Versioned OS/platform reserve derived from measurements, plus a configurable safety
  margin. Reserve the entire VM/provider footprint on a physical Mac/Windows host.
- Per-operation peak estimates: old+new app overlap during rollout, image
  decompression/page cache, DB connections/startup/migrations, builds and restore jobs.
  Baseline current demand and additional reservations must not double-count memory
  already included in a fresh sample.
- Explicit limits and an aggregate workload ceiling. History is advisory; missing
  history uses conservative declared defaults, not zero. Tune database presets with
  connection limits and buffer settings, not just a tiny container cap.
- Freshness and sequencing: sample timestamp/epoch, atomic admission, persisted
  operation ownership, and restart reconciliation against actual containers/jobs.
  Do not blindly recover stale reservations or forget still-running work.
- RAM pressure, disk temporary-space reserve, CPU/I/O concurrency and an operation
  queue. zram/swap may soften failures; it is not extra guaranteed RAM or permission
  to thrash an HDD. Preview wake-up must also pass admission.

Read cgroup/host evidence with [the kernel's documented meanings](https://docs.kernel.org/admin-guide/cgroup-v2.html).
Differentiate memory usage/working set, configured limit, peak and OOM events. Exit
137 can have other causes; missing metrics are unknown. Sample peaks can miss the
instant of failure. Don't invent precise “raise to 768 MiB” advice from a truncated
498 MiB observation.

On refusal, say what is requested and what is reserved; offer a permitted builder,
queue, or user-approved preview sleep. Do not automatically stop production or
raise limits. Defaults like Docker `live-restore`, disabling userland-proxy, or tight
Traefik caps each need a measured benefit and routing/reboot regression tests.

**Acceptance:** simultaneous build/deploy/preview requests, restart with active jobs,
stale node sample, unknown main-node capacity, old+new rollout, preview wake storm,
restore plus serving load, host pressure from unrelated processes and disk exhaustion.
On supported benchmark fixtures, no observed OOM under the declared load; beyond
that, publish limits rather than claiming universal prevention.

## 6. Previews: safety and databases

### 6.1 Fix policy before adding more preview automation

Introduce explicit variable policy: preview-only, shared nonsecret, production-only
and deliberate approved sharing where needed. Never infer safety from a variable's
name or from `secret=false` alone. Deny production-only values and production DB
references before interpolation, at both build and runtime stages. A missing required
preview value should produce an actionable error, not silently fall back to production.
Migration behavior for existing apps must be visible: show impacted keys and require
deliberate policy selection, without a silent security downgrade.

Scope and redact values in plan APIs, caches, images, logs, errors, generated types
and UI. Same-repo PR authors can also run malicious code. Keep fork execution off by
default; adding explicit approval later requires a separate execution boundary and
scoped credentials. Runtime network rules must block access to production DBs and
management interfaces; withholding a password alone is not complete isolation.

Private visibility must be a route/access policy, not a hard-to-guess hostname.
Reject direct public access by hostname and IP/Host header, alternate domain, forwarded
header, old route, IPv6 and tunnel path. Apply the same check to higher-priority
`vercel.json` routes, static assets, wake pages and WebSockets. Verify from a genuinely
external client. If the app is intentionally public, say so instead of calling it private.

### 6.2 Database alternatives and recommended first release

| Approach | Resource cost | Isolation | Call |
| --- | --- | --- | --- |
| Schema per PR in production's Postgres | Lowest extra process cost | Weaker namespace/privilege boundaries, easy search-path mistakes, shared production resources | Avoid as the default |
| Database + restricted role per PR in a dedicated preview Postgres service | One shared preview process plus per-DB storage/connections | Logical separation; shared service resources and extensions remain | Recommended for trusted small-project previews |
| Separate Postgres container/volume per PR | Higher RAM/disk/startup cost | Stronger process separation, still the same host kernel unless further isolated | Opt-in for stronger isolation; gate on capacity |
| True storage-engine copy-on-write branching | Potential efficient copies at scale | Requires a suitable engine/storage architecture and operations | Do not attempt to recreate Neon for this milestone |

For the first release, support PostgreSQL only; other engines have different backup
and privilege semantics. Use a separate preview service rather than production's
cluster, unique database and credentials per PR, safe grants and connection quotas.
Prevent cross-PR database access, including PostgreSQL default PUBLIC privileges and
shared extension/schema paths. Do not claim a shared DB server is hostile-tenant
isolation. User-selected external preview DBs remain a valid path on tiny hosts.

Lifecycle: allocate with an idempotency key → create DB/role → run migration → optional
synthetic seed → bind preview connection → probe → publish. Migrations get credentials
for only that preview. Failed migrations never publish and never run against production.
Persist resource ownership and reconcile orphaned databases/roles after crashes.
Close marks the PR closed first, revokes shares/credentials, cancels jobs, removes
routes/apps and cleans only owned DB resources; retries must be safe.

Sleeping an app does not sleep its database inside a shared preview Postgres process.
Use connection quotas and an explicit retention/expiry policy; suspend the shared
service only when all dependents are asleep and wake it before serving. Do not promise
per-preview database sleep without accounting for the selected architecture.

### 6.3 Backup-seeded copies: separate, gated beta

[PostgreSQL `pg_dump`](https://www.postgresql.org/docs/current/app-pgdump.html)
provides a consistent per-database logical export. It is not an atomic snapshot of
all application services. The current cluster-wide dump can include unrelated DBs,
roles and sensitive material. Add a per-DB custom-format export and manifest instead
of parsing or replaying `pg_dumpall` into the shared preview service.

Manifest: source host/DB identifiers, engine/version/image digest, exact snapshot ID,
successful dump result, start/end times, checksum, schema/migration version when
available, uncompressed-size estimate and sanitization-policy version. Choose the
latest **successful compatible dump for the requested DB**, subject to an age limit;
absence or stale data is explicit. A restic snapshot ID alone isn't sufficient.

Restore in a separate quarantine runtime with no production network, management
credentials, public ingress or untrusted PR code. [PostgreSQL warns that restore can
execute code from the source](https://www.postgresql.org/docs/current/app-pgrestore.html).
Use controlled restore privileges, no production owners/ACLs, allowed extensions,
fresh credentials and quotas. Scrub before export into the preview DB, with deterministic
referentially consistent rules, canary PII checks and a policy gate for new columns,
JSON, free text, blobs, tokens and embedded connection strings. A column replacement
is not a blanket anonymization guarantee. Failed checks leave nothing accessible.

Prefer curated synthetic seeds as the default. For opted-in backup copies, disable
outbound email, payments, webhooks, scheduled workers and external writes by default;
network restrictions must back up the configuration. Preserve snapshot age in the UI
and PR context. Enforce storage/restore CPU, memory, I/O and time budgets; compressed
200 MB can expand into far more disk and index work. No universal 60-second promise.

**Acceptance:** conflicting simultaneous migrations, cross-PR access attempts,
production-write attempt, absent/failed/stale/wrong-host dump, incompatible extension,
schema drift in scrub policy, PII canary, rogue outbound side effect, restore timeout,
restart/PR-close races and eventual cleanup without deleting another PR's resources.

## 7. Reliability workflows

### 7.1 Deploy guard and platform updates

Retain the existing watcher as the starting point. Persist watched deployment,
previous compatible image/config revision, observation deadline, health policy,
failure evidence and rollback job/outcome. Resume after restart; an agent restart
must not reset the watch indefinitely. Distinguish failed probes, crashes, OOM,
planned stop/sleep, missing containers and unreachable nodes. A network partition
is not evidence that an image is defective.

Use configurable startup grace and consecutive failures; record restart baselines
at publication rather than missing early crashes before the first poll. Fence rollback
against newer/manual deployments and use existing app locks/queues. One attempted
rollback per incident, with bounded retry and an alert on failure. Do not recursively
roll back forever. “Rollback requested” and “previous version serving” are separate
states; only report success after the old version passes probes and actual ingress.

Retain the old image and needed configuration/secrets revision, and reserve capacity
for recovery. A revoked secret may prevent restoring old configuration. App image
rollback does not reverse volume writes or database migrations: use expand/contract
schema changes and an explicit compatibility policy; never auto-restore production
data as an image rollback side effect.

For platform updates, capture which apps were intentionally running and healthy;
validate equivalent local/runtime and ingress probes after the update. Sleeping apps
must remain asleep. Disambiguate an application failure from a transient ISP failure
before rolling back the whole platform. A schema migration can make binary-only
rollback unsafe: snapshot/control state needs version compatibility and tested
recovery. Do not overwrite post-update user data to make a health check green.

**Acceptance:** unhealthy but running, early crash, delayed crash, agent restart,
missing prior image, incompatible migration, rollback failure, new deployment during
watch, manual stop, remote-node outage, bad proxy update and real reboot. Separate
detection time from total recovery time; cold pulls/boot time affect the latter.

### 7.2 Restore drills

Separate three results: successful backup upload; repository integrity; application
restore. [Restic's full data verification](https://restic.readthedocs.io/en/stable/045_working_with_repos.html)
requires reading stored data and can cost bandwidth. It does not prove database or
application semantics. Rotate data verification if appropriate, documenting coverage.

Run actual DB restore into an isolated destination using the source's supported
version and expected extensions. No public ports, production credentials or outbound
jobs. Check object presence, expected migration marker, selected invariants and a
read/query smoke test; row counts alone can match an incomplete/wrong dataset.
Use known fixtures or assertions captured consistently at backup time, not a live
production row count that has changed since last night. Optional application smoke
probes need isolated configuration too.

Record snapshot/DB IDs, age, assertions, duration, bytes, result and cleanup. “Last
verified restore of DB X from snapshot Y” is an honest label; “backups you can prove”
needs that scope. Show backup recovery point age (RPO observation) and measured
restore duration (RTO observation), not guaranteed service objectives. Alert on overdue
drills and repeated capacity deferral, not just failed ones. Test recovery keys from
an off-device copy on a clean machine. A drill using the original live machine's key
does not prove recovery after losing that machine.

Monthly + on-demand is a proposed low-cost starting cadence; valuable data may need
more frequent checks. Run serially, under admission and maintenance budgets. Defer
on low battery/pressure and preserve a visible overdue status. Never restore over
production to test a backup.

### 7.3 Guest share links

Choose an existing operator-configured public HTTPS route/tunnel for the first release.
Do not add a managed relay service without a separate cost, availability and abuse
model; the product currently avoids a vendor cloud dependency. No public route means
“private link: recipient needs mesh,” not a broken promise of universal sharing.

Recommended credentials: random opaque bearer capability, hashed at rest, scoped to
preview + generation + audience policy, with server-side expiry/revocation. Signed
tokens are also possible but need server-side revocation/version state; a purely
stateless signature cannot provide immediate revocation. Redeem into a short-lived
Secure/HttpOnly/SameSite host-scoped cookie and remove the token from the browser URL.
Bound every session by the original grant's expiry; revoking a token must revoke its
sessions too. Don't consume one-use links on an unauthenticated GET: chat/email
scanners prefetch links. Use an explicit confirmation POST if single use is required.

Enforce auth at every ingress/router before assets, proxy rewrites, wake-up and
WebSocket upgrades. Strip untrusted identity headers; forward-auth fails closed;
no general access to the agent API or registry. An expiring link can still leave an
already-upgraded WebSocket open: specify and test a maximum session lifetime and
forced termination if expiry is meant to end live access. Document that downloaded
content cannot be revoked. Use referrer policy, token/log redaction, rate limits,
no shared authenticated caches and no indexing. Treat links and QR codes as secrets;
creating a PR comment must not automatically expose the capability to a public repo.

**Acceptance:** external non-mesh browser, expired/revoked link and cookie, PR close,
new preview generation, agent restart/key rotation, clock change, forged headers,
alternate hostname/direct backend, all route priorities, cached assets, wake page,
WebSocket lifetime and link scanner. This is substantial auth/network work, not a
small extension of basic auth.

### 7.4 Power resilience and cost

Battery tier policy is configurable and opt-in initially: stop admitting builds,
pause owned work, sleep eligible previews, then selected noncritical apps, then
clean shutdown. Require debounced samples, hysteresis and an AC-stable recovery delay.
Persist why each workload was paused, so return of power does not wake manually
stopped apps or duplicate jobs. Distinguish laptop sensor readings from a Linux
guest's virtual/no battery. Leave critical-service behavior explicit when the battery
is depleted; it cannot serve after shutdown.

Power-on-after-AC settings are vendor/firmware-dependent. Doctor can offer
supported/manual/unknown guidance; qualify with real hardware after full shutdown
or depletion. Test lid close, sleep, unstable AC, missing/aged battery, thermal load,
reboot and recovery of network gear. Treat router/ISP loss as expected in an outage;
home hosting without independent redundant infrastructure has no cloud-like SLA.

For cost, prefer wall-meter measurements/user input and label CPU-only/battery
estimates. Formula: average wall watts × hours / 1000 × currency/kWh. Handle time
coverage, variable tariffs and missing sensors; no manufactured precision. Hosted
comparisons must date plan/usage/region assumptions and distinguish electricity
from hardware, storage, bandwidth, backup, maintenance and availability costs. This
feature can wait until pilots demonstrate it changes decisions.

## 8. Benchmark protocol

The earlier §6 tables are retained as provisional observations, not rerun here.
The “BuildKit stopped” 656 MiB row still includes Next.js, while the 521 MiB
pre-build row does not. Returning within 15 MiB of that earlier baseline after adding
a persistent app is not a valid acceptance test. Use the same applications before
and after and report both runtime addition and builder release.

Proposed qualification procedure:

1. Pin source commit, release/working-tree patch, OS/kernel, provider, architecture,
   vCPUs, actual RAM, swap/zram, Docker/BuildKit/proxy versions and image digests.
   Record enabled features. Test both a declared minimal profile and default install.
2. Save command scripts and raw timestamped samples with units. Capture host
   MemAvailable/used, cgroup current/peak/events, process RSS/PSS where supported,
   swap, CPU, disk use and network/registry transfer. Keep boundaries distinct.
3. Repeat independent cold installs/builds and warm-cache builds. Start with at least
   three runs per fixture; publish all results and variability, not a single best run.
   Sample at roughly one second for transient system pressure and use cgroup peaks
   where available. Record medians/maxima and run count; don't imply population p95
   from three runs.
4. Cover a 1/2/4 GiB Linux VM matrix and a real old x86 host before broad hardware
   claims. A Raspberry Pi or Mac-hosted guest is a separate qualification. Measure
   native ARM and emulated x86 builders separately, including Mac VM overhead.
5. Fixtures: tiny image, pinned Next.js app with lockfile/native dependencies,
   representative small API + Postgres, and multiple previews. `whoami` is an overhead
   fixture, not “five real apps.” Record load/concurrency, latency, failures and duration.
6. Exercise cold image pull, new/old rollout overlap, backup/restore, concurrent work,
   disk growth, restart and OOM behavior while external probes monitor serving.
   Offloading compilation does not remove pull, unpack, registry or startup peaks.
7. Compare competitors with equivalent feature/workload settings and whether their
   control plane runs locally or elsewhere. Publish unmeasured cells as unmeasured.

Suggested first support gate: a 1 GiB **experimental runtime-only** profile with one
pinned Next.js image, declared request load and full target-side accounting, no observed
OOM and successful rollout/reboot across the fixture runs. Derive available headroom
and error/latency thresholds from the baseline and intended app load before advertising
support. Test 2 GiB separately; do not conclude it from the 1 or 4 GiB result. Do not
promise five apps plus PostgreSQL and 250 MiB spare without a named workload and proof.

BuildKit idle targets should assert safe lifecycle and release of its observed
footprint, not force a 130 MiB platform number with arbitrary proxy caps. Backup
restore performance reports must include logical rows/bytes, expansion, indexes,
engine, media and I/O limits. These gates are work still to perform.

## 9. Adoption and developer growth

Feature novelty is not yet a growth strategy. The following is a proposed validation
programme, with targets chosen for a small pilot; none are claimed industry benchmarks.

### 9.1 Validate activation and repeat use

Recruit ten pilot developers: spare-laptop owners, mini-PC/self-hosting users and
small-host users currently fighting build contention. Ask about their last deploy,
actual hardware, existing workaround and switching barrier before presenting features.
Include some current Coolify/Dokploy users, not only people already enthusiastic
about Vercelify. Observe them installing without the author operating the machine.

Offer three honest entry paths: deploy a pinned image, deploy a supported Git repo,
or build locally/CI and hand off an image. Give a ready-to-run Next.js + PostgreSQL
example, a small API example, and a Dockerfile example with safe preview configuration.
Each guide explains machine availability, private versus guest access, backups and
how to leave/export. Don't advertise all language/framework combinations as supported.

Suggested expansion gate: at least eight of ten can reach a first working app with
the documented path, and at least five independently make a second deploy within
seven days. Track first-install and already-prepared-host timing separately; a cold
VM/download is not “deploy in seconds.” If gates miss, fix the observed blockers before
adding breadth. Small samples are directional; report denominators, assistance and
reasons for dropout rather than declaring product-market fit.

Metric definitions:

| Metric | Definition / purpose |
| --- | --- |
| First-deploy completion | Attempted new installs reaching an externally or privately verified app, divided by attempted installs |
| Time to first working app | Installer start to successful probe, with downloads and assistance recorded separately |
| Repeat deploy | Activated users independently completing another deploy within seven days |
| Retained host | Activated host still used for intentional serving/deploys in week four; distinguish sleeping/offline by design |
| Recovery success | Compatible failed-deploy/update fixtures returning to verified service, with elapsed time and failures |
| Preview usefulness | Users who actually reviewed a PR against isolated data and then cleaned it up |
| Switching blocker | Observed missing capability or friction that prevented use, in the user's own words |

Respect the no-cloud product direction: use explicit opt-in local diagnostic exports
and pilot check-ins. The research does not authorize telemetry collection or contacting
anyone. Do not count stars, downloads or issue comments as activation/retention.

### 9.2 Earn trust before a comparison campaign

Publish a supported-configuration matrix, limitations, security reporting route,
versioned changelog, upgrade/rollback procedure, backup recovery guide and tested
export path. Set only a release/support cadence the maintainer can actually sustain.
Show real demonstration artifacts: off-host build while an old laptop keeps serving,
isolated conflicting preview migrations and a deliberately broken deployment recovering.
Publish raw results and reproduction scripts with the comparison, including unknowns.

Promote tutorials and measured case studies to relevant communities after pilot results;
do not imply this review has verified an acquisition channel. A lightweight import
guide for image/env/domain configuration should precede a full migration engine.
Never make production volume migration look like a one-click metadata import.

If repeated pilot feedback says collaborators cannot access reviews or manage apps
without installing a desktop, bring a supported private browser dashboard forward.
Reuse a static UI and scoped APIs; a browser UI does not inherently require another
server daemon. If individual users are satisfied with CLI/desktop, avoid building it
just for parity. Likewise, add team roles before pursuing teams, rather than promising
collaboration with shared owner tokens.

### 9.3 Make contributing tractable

Document a clean-checkout development path, architecture map, fixture-based tests,
review expectations, issue reproduction template and how a contributor can run a
supported minimal Linux environment without the author's private hardware. Expose
small, owned contribution areas: example apps, template validation, docs, failure
fixtures and hardware qualification reports. Keep deterministic contracts for build
handoff, preview cleanup and route policy.

Do not design a plugin marketplace or promise compatibility with hundreds of templates
before the base contracts settle. Publish stable CLI/API schemas and version/deprecation
rules where integrations already depend on them. Track independent successful
contributions and setup failures; a growing backlog is not community success.

## 10. Scope and verification of this review

Completed here: primary-document research; five repository/licence snapshots; 23
issue state/reaction snapshots; five recorded phrase-search attempts (three succeeded,
two rate-limited); inspection of relevant source paths; corrected research/roadmap;
and proposed architecture, failure contracts, benchmarks and adoption programme.

Not performed: new interviews, runtime/competitor benchmarking, security audit,
production suitability certification, implementation or release. All 23 distinct
issue links retained in the main document have saved current-state records (15 closed, 8 open). Older broad issue/advisory counts were not comprehensively
reproduced and were removed from the current inventory. Negative competitor
claims and supported hardware minimums require evidence beyond this review.

The next implementation work should begin with M1's reproducible baseline,
first-deploy fixes and preview policy design. Approve individual engineering scope
and its contracts when selecting a milestone; this research does not authorize
silently implementing the entire roadmap.
