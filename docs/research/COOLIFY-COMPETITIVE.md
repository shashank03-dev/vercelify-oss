# Vercelify competitive research and recommended roadmap

Reviewed 1 November 2026 against primary vendor documentation, public GitHub API
responses and this **dirty working tree**, based on HEAD
`17547d68f0a778856042a5d145db6d096e4354f1`. This is research and a proposed roadmap;
no product implementation or release qualification was performed in this review.

**Recommendation:** build the easiest reliable path from an owned Linux machine
to a first deployment, then off-host builds and safe previews. The opportunity is
integration, low overhead and understandable operations. Claims that competitors
cannot implement these features, that every preview is private today, or that the
proposed database restore equals Neon branching are unsupported.

Read the [full validation and engineering review](2026-11-01-COMPETITIVE-VALIDATION.md)
for architecture alternatives, failure cases, growth experiments and release gates.
[Saved evidence](evidence/2026-11-01/README.md) distinguishes verified counts,
unavailable results and inspected code. Numbers below are observations or explicitly
proposed targets; issue reactions are not customers, votes to switch, or market size.

The user feedback to answer remains:

1. Why use a newer project instead of Coolify or Dokploy?
2. Why does README ask for 4 GB when small workloads run on 512 MB–2 GB?
3. What variables and database does a preview actually receive?

## 0. The landscape

Stars are the public API snapshot retained in `evidence/2026-11-01/github.json`;
they indicate community reach, not product quality or active installations.

| Product | Stars | Licence source and qualification | Relevant capability |
| --- | ---: | --- | --- |
| [Coolify](https://github.com/coollabsio/coolify) | 65,496 | [Apache-2.0](https://github.com/coollabsio/coolify/blob/main/LICENSE) | Web UI, previews, separate build servers, image deployment, resource limits |
| [Dokploy](https://github.com/Dokploy/dokploy) | 38,173 | [Apache-2.0 outside proprietary directories; separate proprietary terms inside them](https://github.com/Dokploy/dokploy/blob/canary/LICENSE.MD) | Swarm, web UI, build servers, protected previews, automatic and manual rollback |
| [CapRover](https://github.com/caprover/caprover) | 15,195 | [Apache text with additional restrictions](https://github.com/caprover/caprover/blob/master/LICENSE); do not call it unmodified Apache | Web UI, Docker/Swarm, image deployment |
| [Dokku](https://github.com/dokku/dokku) | 32,217 | [MIT](https://github.com/dokku/dokku/blob/master/LICENSE) | Lean deployment tooling, plugins, registry and image-archive deployment |
| [Kamal](https://github.com/basecamp/kamal) | 14,699 | [MIT](https://github.com/basecamp/kamal/blob/main/MIT-LICENSE) | Local/remote builds, proxy health gating, explicit rollback |
| Vercelify | — | [MIT](../../LICENSE) | Rust agent, mesh, desktop, Docker/Traefik, previews, databases; code presence is not release proof |

Do not limit “open-source PaaS” to those five: [Kubero](https://www.kubero.dev/)
also advertises PR apps, basic authentication and sleeping containers on Kubernetes.
[Kamal builders](https://kamal-deploy.org/docs/configuration/builders/) and
[Dokku image workflows](https://dokku.com/docs/deployment/methods/image/) already
keep compilation away from the serving host. None of these tools requires rented
hardware as a technical prerequisite.

Hosted references: [Render](https://render.com/docs/preview-environments) creates
preview datastore resources without production data and supports initialization;
[Railway](https://docs.railway.com/environments) documents environment workflows.
[Neon](https://neon.com/blog/instantly-copy-tb-size-datasets-the-magic-of-copy-on-write)
uses copy-on-write storage for branches. Restoring a dump is a different operation.

## 1. Established competitors' advantages

Coolify and Dokploy already have browser dashboards, broader integrations, tutorials,
service catalogues and established communities. Do not frame a small new project as
more maintainable merely because it is MIT or written in Rust. Trust needs visible
releases, migration compatibility, security response, export/restore and user results.

[Coolify roles and permissions](https://coolify.io/docs/core/team/roles-and-permissions)
are documented today. Its open [#2378](https://github.com/coollabsio/coolify/issues/2378)
asks for more granular **per-resource** membership, not for roles to exist at all.
A desktop-led product can still serve solo developers; browser access and collaboration
should move forward only if actual onboarding and reviewer friction justify them.

## 2. Pain evidence: what it does and does not establish

The earlier inventory mixed historical reports with present gaps. These reports
establish that a problem occurred or was requested, not that the current product
still lacks a solution. Primary documentation takes precedence for current behavior.

| Theme | Evidence | Correct interpretation / Vercelify action |
| --- | --- | --- |
| Updates break installs | [Coolify #7193](https://github.com/coollabsio/coolify/issues/7193) (closed) | Historical incident; test app reachability and safe update recovery here |
| Builds contend with apps | [Coolify #6656](https://github.com/coollabsio/coolify/issues/6656), [Dokploy #711](https://github.com/Dokploy/dokploy/issues/711) | Strong problem signal; separate build servers already solve part of it |
| CPU spikes | [Coolify #2110](https://github.com/coollabsio/coolify/issues/2110) (closed) | Reproduce idle/background work here; no comparative efficiency guarantee |
| Host command injection | [Coolify advisories](https://github.com/coollabsio/coolify/security/advisories), [Dokploy advisories](https://github.com/Dokploy/dokploy/security/advisories) | Audit our privileged paths; advisory totals are not a security ranking |
| Backup usability | [Coolify #2389](https://github.com/coollabsio/coolify/issues/2389) | Evidence for backup/restore workflow, indirect evidence for scheduled drills |
| Preview variable confusion | [Coolify #3079](https://github.com/coollabsio/coolify/issues/3079) (closed) | Show effective provenance and enforce safe defaults here |
| PR code receives secrets | [Coolify preview guidance](https://coolify.io/docs/applications/deployments/preview-deployments) | Treat PR code as untrusted; same-repo membership is not a sandbox |
| Preview migration conflicts | [Dokploy #2462](https://github.com/Dokploy/dokploy/issues/2462) | Direct demand for DB/schema isolation and lifecycle hooks, not production-data copying |
| Closed previews remain | [Coolify #2679](https://github.com/coollabsio/coolify/issues/2679) (closed) | Historical bug; test close/build races and cleanup retries here |
| Disk/cache growth | [Coolify #5074](https://github.com/coollabsio/coolify/issues/5074) (closed) | Budget cache, layers, logs and temporary restore space |
| Registry rate limits | [Coolify #3395](https://github.com/coollabsio/coolify/issues/3395) (closed) | External base images remain a dependency for us too |
| Reboot recovery | [Coolify #5933](https://github.com/coollabsio/coolify/issues/5933) (closed) | Test real reboot and serving, beyond a live agent |
| Build variable behavior | [Coolify #2107](https://github.com/coollabsio/coolify/issues/2107) (closed) | Separate build/runtime configuration and secret transport |
| Deployment downtime | [Dokploy zero-downtime docs](https://docs.dokploy.com/docs/core/applications/zero-downtime) | Existing capability; verify our single-container and Compose contracts separately |
| Compose failures | [Coolify #1996](https://github.com/coollabsio/coolify/issues/1996) (closed) | Historical reports; do not generalize to all Compose deployments |
| Health-check confusion | [Coolify #2073](https://github.com/coollabsio/coolify/issues/2073) (closed) | Make probe target, timeout and response clear |
| Gateway timeouts | [Coolify #4483](https://github.com/coollabsio/coolify/issues/4483) (closed) | Test real ingress and routing changes here |
| Home-network access | [Dokploy tunnel guide](https://docs.dokploy.com/docs/core/guides/cloudflare-tunnels) | Integration opportunity, not architectural impossibility for competitors |
| Small-host floor | [Coolify installation](https://coolify.io/docs/get-started/installation) and user feedback | Measure complete installation and actual workload capacity |
| UI friction | Earlier HN anecdotes | Qualitative leads; validate with observed installation/deployment sessions |
| Scheduled-task reliability | [Coolify #4539](https://github.com/coollabsio/coolify/issues/4539) (closed) | Add reliability only where our reproduction exposes a gap |
| External DB access | [Coolify #5450](https://github.com/coollabsio/coolify/issues/5450) (closed) | Keep preview credentials and access scoped; no gratuitous public DB port |
| Scale to zero | [Dokploy #2154](https://github.com/Dokploy/dokploy/issues/2154) | 26 reactions, open; useful fit, with alternatives such as Sablier already mentioned |
| Rollback usability | [Dokploy #319](https://github.com/Dokploy/dokploy/issues/319) | 69 reactions, **closed**; current Dokploy documents automatic rollback |
| Project longevity | [Coolify #5685](https://github.com/coollabsio/coolify/issues/5685) and licence files above | Maintenance uncertainty applies to us too; publish actual support practices |

All 23 distinct issue links retained in this document were freshly checked for
state, title, body and reaction totals; 15 are closed and 8 are open. See the
[snapshot index](evidence/2026-11-01/README.md#issue-state-index). Closure is not proof
that a problem can never recur, but closed historical bugs are not present-day
feature gaps. The older broad phrase/advisory counts are deliberately not restated.

## 3. What is differentiated, and what is not

- **Credible opportunity:** one coherent owned-hardware experience: pairing,
  off-host builds, capacity-aware deployment, private access and recovery. Existing
  pieces can reduce setup work; they do not create an uncopyable architecture.
- **Memory:** the earlier 142 vs 624 MiB platform deltas are promising, provisional
  measurements. Moving a control plane off the serving host changes the comparison.
- **Security:** argument vectors avoid some shell parsing risks; they do not prove
  isolation from hostile Dockerfiles, Compose, mounts, build tools or privileged APIs.
- **Networking:** an integrated mesh can simplify private access. CGNAT does not
  disappear: a reachable peer/relay/tunnel remains necessary for outside access.
- **Hardware:** battery/thermal policies fit reused laptops. They do not provide
  high availability when the router, ISP or mains fails.

## 4. Existing Vercelify foundations, checked in this tree

| Foundation | Observed code | Remaining qualification |
| --- | --- | --- |
| Post-live automatic rollback | `engine.rs::watch_after_deploy`, 180 s window, 10 s polling | Durable restart recovery, unhealthy-but-running detection, verified rollback completion |
| Placement memory reservations | `placement.rs::Reservations` | Build/restore/pull/rollout peaks, reserve policy, restart recovery, fresh accounting |
| Preview overrides | `env.rs::resolve` applies normal then preview-only groups | Normal values are inherited; introduce explicit safe policy before expansion |
| Preview teardown lock | `engine.rs::remove_preview`, `previews.rs` | New DB/share resources must join durable, retryable cleanup |
| Private registry | `registry.rs`, `engine.rs::start_one` | Paired device upload authorization, Docker VM reachability, immutable digest handoff |
| Basic-auth routing | `proxy.rs::render` | Does not prove every preview is mesh-only; inspect all ingress paths |
| Encrypted snapshots and DB dumps | `host/lib/backup.sh` | Restore freshness, per-DB scope, isolated drills, off-device recovery |
| Battery shutdown | `host/power-guard.sh` | Load tiers, resume ownership and real hardware recovery |
| Optional Mac hosting | `install-mac.sh --host`, `vercelify-host` | Native host manager + Linux guest differs from a universally running build agent |

These are source observations, not test results or released guarantees. See the
[inspected-file hashes](evidence/2026-11-01/code-snapshot.json).

## 5. Priority decisions

| Priority | Work | Why |
| --- | --- | --- |
| P0 | First-deploy fixes, preview secret/route safety, reproducible footprint | Remove failure and trust barriers before adding breadth |
| P1 | Explicit local/CI build handoff, idle BuildKit lifecycle, capacity admission | Strong direct demand, strong fit with small owned hardware |
| P1 | Harden existing deploy/update/reboot recovery | Retention depends on apps surviving ordinary maintenance |
| P1 | Empty/seeded isolated PostgreSQL previews | Directly addresses migration conflicts without production-data exposure |
| P2 | Revocable guest preview links, restore drills | Valuable workflow/trust additions after prerequisites |
| P2 | Sanitized backup-seeded previews | Opt-in only after restore and sanitization qualification |
| P2 | Battery load shedding | Valuable for the laptop segment; bounded supported hardware |
| P3 | Cost estimate, badges, QR, more integrations | Useful polish; no direct evidence these drive switching |
| Later | Full migration importer, team RBAC, managed relay, sleeping hosts | Revisit from observed blockers, not feature-count competition |

## 6. Earlier footprint measurements (retained, not independently reproduced)

**Validation status, 1 November 2026:** these are the earlier author’s observations.
This review did not rerun the VMs. Matching raw samples, exact image digests and a
reproduction script were not found under `field-test/` or `docs/validation/`. Treat
the tables as provisional, not a supported minimum or a release benchmark. The
installation disables backups, power guard and other optional components; it is
not the full default installation. Linux ARM guests on an M-series Mac do not
qualify an old x86 laptop. Full validation requirements are in
[the engineering review](2026-11-01-COMPETITIVE-VALIDATION.md#8-benchmark-protocol).

**Setup.** The field-test laptop was offline on the tailnet on 1 Nov 2026, so this ran
in two identical Lima VMs on an M-series Mac: Ubuntu 24.04.5, aarch64, 2 vCPU, 4 GiB,
`vz`. The Coolify VM is a clone of the Vercelify VM made before anything was installed.
Lima's own rootless containerd/BuildKit were turned off in both. Vercelify: this
checkout (v1.0.0 + working tree), installed with `install.sh -y --no-remote
--no-display --no-desktop --no-backup --no-power-guard`. Coolify: its official
`install.sh` from `cdn.coollabs.io`, latest on that date.

**Method.** `ps` RSS summed per process name (all PIDs) and `free -m` "used", sampled
every 6–8 s, median of 10–15 samples, after the system settled. RSS counts shared pages
in each process, so per-process numbers are upper bounds; "used" is the honest total.
Apps for the 1- and 5-app rows are `traefik/whoami` (a ~1 MiB Go server), so those rows
measure the platform's cost per app, not a real app's. The Next.js app is
`vercel/nextjs-portfolio-starter`, built by Vercelify's detector.

### Vercelify (MiB)

| | OS only | 0 apps | 1 app | 5 apps | 5 apps + Next.js, after its build | same, BuildKit stopped |
| --- | --- | --- | --- | --- | --- | --- |
| `vercelify-agent` | — | 7 | 8 | 8 | 8 | 8 |
| `vercelify-mesh` | — | 2 | 2 | 2 | 2 | 2 |
| Traefik (process RSS) | — | 88 | 88 | 93 | 91 | 91 |
| dockerd | — | 107 | 105 | 101 | 104 | 103 |
| containerd | — | 49 | 45 | 47 | 49 | 49 |
| containerd-shim (all) | — | 10 | 21 | 63 | 85 | 74 |
| BuildKit (`buildkitd`) | — | 0 (not started yet) | 0 | 0 | **110** | 0 |
| Apps (container memory) | — | — | 1 | 5 | 5 + 160 (Next.js) | 5 + 161 |
| **System "used"** | **343** | **485** | **504** | **521** | **761** | **656** |
| Platform over OS | — | **142** | — | — | — | — |

- **Next.js build peak:** "used" peaked at **1,389 MiB** (from 521 before the build),
  so the build itself needed about **870 MiB** on top of everything else. The build
  container's cgroup peak (page cache included) was 2,157 MiB. No swap was used. The
  build took 576 s on 2 vCPUs.
- **BuildKit after a build** keeps ~110 MiB RSS (239 MiB with its cache, per `docker
  stats`) for as long as the server runs. Stopping it gave back **105 MiB** of "used".
- **Per app**, the platform adds ~10 MiB of containerd-shim; the agent doesn't grow.
- **Idle CPU** over 120 s with 6 apps: agent 0.10 %, mesh 0.12 %, Traefik 0.03 %, dockerd
  0.18 %, containerd 0.24 % of one core.

Interpretation: agent + mesh RSS was about 10 MiB in these runs; total installed
platform overhead was about 142 MiB over the OS. Off-host builds should reduce
serving-host pressure. These observations do **not** establish why 4 GB was chosen,
prove that 1 GiB is supported, or qualify arbitrary Next.js builds on 2 GiB. The
2,157 MiB cgroup peak and coarse system samples measure different things; neither
turns the 870 MiB system delta into a universal build requirement.

Found while measuring: an image app defaults to port 3000 instead of the image's
`EXPOSE` (whoami listens on 80), so the first deploy failed after 180 s.

### Coolify 4.5.3 (MiB), same VM spec, same method

| | OS only | 0 apps |
| --- | --- | --- |
| Coolify app (`php` + `php-fpm` + nginx, container memory) | — | 405 (process RSS: php 383, php-fpm 121, nginx 13) |
| PostgreSQL (`coolify-db`) | — | 39 (process RSS 84) |
| Redis (`coolify-redis`) | — | 7 (process RSS 11) |
| Traefik (`coolify-proxy`, process RSS) | — | 94 |
| Sentinel | — | 10 |
| dockerd / containerd / shims | — | 110 / 52 / 66 |
| **System "used"** | **343** | **967** |
| Platform over OS | — | **624** |

Idle CPU over 120 s (summed per process name; short-lived PHP workers are partly
missed, so this is a lower bound): php 0.46 %, php-fpm 0.38 %, redis 0.76 %, dockerd
0.73 %, containerd 0.57 %, Traefik 0.10 %, postgres 0.06 %, Sentinel 0.07 %, about
**3.1 %** of a core in total, against about **0.7 %** for Vercelify with 6 apps.

Not measured: Coolify with 1 and 5 apps, and a Next.js build under Coolify. Deploying
needs Coolify's web onboarding, which creates an account; we didn't automate that.
Its per-app container cost is Docker's, the same as ours; its build runs in its own
helper container (`coollabsio/coolify-helper`, 673 MB on disk). Dokploy was not
measured.

### Side by side

| | Vercelify | Coolify |
| --- | --- | --- |
| Platform memory over the OS, 0 apps | **142 MiB** | **624 MiB** |
| Of which the control plane itself | 9 MiB (agent 7, mesh 2) | ~460 MiB (app, PostgreSQL, Redis, Sentinel) |
| Idle CPU, all platform processes | ~0.7 % | ~3.1 % (lower bound) |
| Stated minimum RAM | 4 GB (README) | 2 GB (docs) |

The earlier idle platform deltas suggest a useful footprint advantage in this
configuration. Compare **142 vs 624 MiB** for the whole platform, not 9 vs 624.
Agent + mesh RSS and Coolify total system delta are different accounting boundaries.
The CPU rows also use different workloads and miss short-lived processes; they are
observations, not an apples-to-apples speed or efficiency result. External image
deployment already exists; automatic selection of a paired builder is additional work.

### Re-measured after M1, 2 November 2026

Same `vfybench` VM and method as above (RSS by process name, `free -m` used, median of
12 samples 6 s apart), same workload in both columns: five whoami apps and the Next.js
app, BuildKit not running. "Before" is the installed v1.0.0 + earlier tree; "after"
is M1 installed with the real installer (`setup-host.sh`, same flags). Kernel
6.8.0-142-generic aarch64, Docker 29.9.2, swap configured but unused.

| Median MiB | Before M1 | After M1 |
| --- | ---: | ---: |
| System `free -m` used (range) | 693 (682–696) | 702.5 (675–712) |
| `vercelify-agent` RSS | 8.86 | 7.70 |
| `vercelify-mesh` RSS | 3.11 | 2.89 |
| Traefik RSS | 91.15 | 91.10 |
| dockerd / containerd RSS | 103.9 / 50.3 | 104.7 / 51.2 |
| All containerd-shim RSS | 75.0 | 75.4 |

The field-test laptop (x86_64) was online and its before-M1 state was sampled
(1,381 MiB used with its own, different workload), but installing M1 there failed on
stalled transfers, so it has no after-M1 row; see the M1 validation.

The ranges overlap: M1 (preview policy, isolated preview networks, log handling) adds
no measurable memory. These are steady-state rows only; OS-only, zero-app and
build-peak rows were not rerun, and an ARM VM still does not qualify an old x86
laptop. Raw samples, binary hashes and scripts:
[before](../validation/2026-11-02-m1/inherited-baseline.txt),
[after](../validation/2026-11-02-m1/after-install.txt),
[script](../validation/2026-11-02-m1/sample-memory.sh),
[M1 validation](../validation/2026-11-02-m1/VALIDATION.md).

### Re-measured for M2 idle BuildKit, 2 November 2026

M2 candidate, same §6 method and matched workloads **within each machine**.
The VM rebuilt Next.js commit `90cfee0f74c25bb3688369a0587e435f8b8f114d`
before both sample sets; five whoami apps plus that one Next.js container stayed
running throughout. Test daemon stopped. The physical laptop runs its production
app, PostgreSQL, proxy, unrelated media service and desktop. Its old PR preview
was retired by the explicitly authorized safety upgrade before both candidate
sample sets. These machines are not directly comparable.

| Median MiB | VM builder running | VM auto-stopped | Laptop builder running | Laptop auto-stopped |
| --- | ---: | ---: | ---: | ---: |
| System used (range) | 682 (674–708) | 656.5 (638–689) | 1427 (1415–1438) | 1377 (1364–1397) |
| BuildKit RSS | 42.22 | 0 | 60.63 | 0 |
| Agent RSS | 7.95 | 7.74 | 10.30 | 9.99 |
| Mesh RSS | 2.90 | 2.90 | 3.60 | 3.60 |
| Traefik RSS | 91.95 | 91.68 | 89.23 | 89.35 |
| dockerd / containerd RSS | 107.29 / 51.33 | 107.53 / 51.15 | 100.80 / 72.82 | 100.29 / 73.63 |
| All shim RSS | 86.22 | 75.52 | 66.25 | 55.28 |

Both hosts automatically stopped the builder **61 seconds** after restart with
`builder_idle_minutes = 1` (gate: timeout + 60 seconds). The default is ten minutes;
zero disables stopping. Cache survives; the real disposable Docker contract passed
three build/stop/restart cycles, including warm-cache reuse. VM system-used medians
fell 25.5 MiB with overlapping ranges; observed builder RSS disappears, but a fixed
105 MiB saving is not a product guarantee. The laptop medians fell 50 MiB;
its sample ranges do not overlap, but this is still one matched run. No new minimum-RAM or comparative
competitor claim follows. Raw samples, hashes, scripts, tests and boundaries are
in [M2 validation](../validation/2026-11-02-m2/VALIDATION.md).

### Re-measured for M3 off-host builds, 2 November 2026

Same four-GiB VM and §6 method: twelve samples after a two-minute settle, five
whoami apps plus the pinned Next.js app; BuildKit stopped and the disposable test
daemon stopped. These are matched steady-state workloads, not build peaks.

| Median MiB | M2 builder stopped | M3 builder stopped |
| --- | ---: | ---: |
| System used (range) | 656.5 (638–689) | 685.5 (674–705) |
| Agent RSS | 7.74 | 8.38 |
| Mesh RSS | 2.90 | 2.90 |
| Traefik RSS | 91.68 | 91.74 |
| dockerd / containerd RSS | 107.53 / 51.15 | 107.85 / 50.36 |
| All shim RSS | 75.52 | 75.49 |
| BuildKit RSS | 0 | 0 |

The system-used medians differ by 29 MiB with overlapping ranges; agent RSS differs
by 0.64 MiB. This one run does not establish a fixed memory cost or saving. The
physical laptop went offline during qualification; its M3 steady state is
**unmeasured**. A newly rebuilt safe PR preview also makes its current workload
different from M2. The user chose to continue in the lab VM.

A separate **one-GiB runtime-only** fixture used 956.1 MiB visible RAM, swap disabled,
one pinned Next.js app, no database, builds disabled and no disposable daemon.
It imported the complete off-host image and rolled out a second deployment under
four concurrent clients paced at 250 ms each: 940 requests in 60 seconds, zero
failures, 9.22 ms p95. After a real VM stop/start, the Next.js response was verified;
a second 60-second run completed 948 requests, zero failures, 10.86 ms p95. Kernel
OOM kills stayed zero. Across 1,168 samples at 200 ms over 239.9 seconds, minimum
MemAvailable was 326.4 MiB; swap remained disabled. Container memory was uncapped.
This is one named synthetic fixture and an **experimental** configuration, not a
claim for arbitrary Next.js apps, databases or concurrent builds. Four-GiB VM,
swap configuration and the original six apps were restored afterward.

ARM→ARM and ARM→amd64 handoffs passed against separate target daemons; the
amd64 target was emulated. Its pinned Next.js rollout served 948 requests with
zero failures. The initial Ubuntu QEMU interpreter failed a minimal Node program;
the successful lab run used the interpreter already bundled in pinned BuildKit.
Physical x86 qualification remains unmeasured.

Raw samples, workload scripts, binary hashes and qualification boundaries:
[M3 validation](../validation/2026-11-02-m3/VALIDATION.md).

### Re-measured for M4 admission, 3 November 2026 (Asia/Kolkata)

Same four-GiB ARM lab VM, six original apps and §6 sampling method: two-minute
settle, twelve samples at six-second intervals (plus collection time), BuildKit
and the disposable Docker daemon stopped. Existing app containers were preserved;
new default caps do not silently recreate legacy apps for this comparison.

| Median MiB | M3 builder stopped | M4 builder stopped |
| --- | ---: | ---: |
| System used (range) | 685.5 (674–705) | 633.5 (623–650) |
| Agent RSS | 8.38 | 7.70 |
| Mesh RSS | 2.90 | 2.90 |
| Traefik RSS | 91.74 | 79.45 |
| dockerd / containerd RSS | 107.85 / 50.36 | 101.23 / 48.34 |
| All shim RSS | 75.49 | 73.85 |
| BuildKit RSS | 0 | 0 |

This is one matched-workload observation. Several unchanged daemon RSS values
also fell; the 52 MiB system-used difference cannot be attributed to admission or
promised as a saving. Physical laptop M4 measurements remain **unmeasured**: it
is offline, and the user selected lab continuation. Declared runtime-only 1/2/4
GiB qualification is recorded separately from this steady-state comparison;
cached-image results do not qualify cold downloads, arbitrary apps or local builds.

Each separate runtime profile ran three 60-second rollouts and one 60-second
post-reboot load run, four clients paced at 250 ms, one cached pinned Next app,
192 MiB per instance, one CPU and the default ten-second drain. Builds, swap and
databases were disabled.

| VM RAM | Requests across four runs | Failures | Minimum sampled available MiB | Kernel OOM-kill delta |
| --- | ---: | ---: | ---: | ---: |
| 1 GiB (experimental) | 3,792 | 0 | 318.5 | 0 |
| 2 GiB | 3,780 | 0 | 1,255.7 | 0 |
| 4 GiB | 3,743 | 0 | 3,094.1 | 0 |

This qualifies the recorded cached-image serving/load observations only. Reboot
retained a superseded version alongside the current one; admission counted both
192 MiB caps. M6 must address this cleanup gap. A separate one-GiB zero-drain
attempt had four failures in its second 948-request rollout and is not qualified.
The one-GiB configuration remains experimental. Four-GiB RAM, original swap/config
and all six original HTTP routes were verified restored after the matrix.

Scripts, raw samples, binary hashes, Docker regressions and profile boundaries:
[M4 validation](../validation/2026-11-02-m4/VALIDATION.md).

## 7. Feature verdicts after validation

### 7.1 Your computer builds, your server serves — build early

**Demand confirmed; uniqueness rejected.** Dokploy [#711](https://github.com/Dokploy/dokploy/issues/711)
has 50 reactions and is closed; [#2386](https://github.com/Dokploy/dokploy/issues/2386)
has 24 and is open for pre-built-image **previews**. Saved phrase searches returned
172 Coolify and 80 Dokploy issues mentioning “build server”; these are overlapping
text matches, not 252 independent requests. [Coolify](https://coolify.io/docs/core/infrastructure/servers/build-servers)
already separates build/run roles and controls fallback. [Kamal](https://kamal-deploy.org/docs/configuration/builders/)
already supports local builders.

Start with explicit `--build-here` plus a documented CI image flow, using the same
build plan and digest handoff. Later add opt-in automatic builder selection. An
offline Mac must not silently trigger a build on a builds-disabled server. Apple
Silicon to x86 needs an explicit `linux/amd64` build and qualification;
[QEMU can be slower](https://docs.docker.com/build/building/multi-platform/).

### 7.2 Isolated preview databases — split into two releases

**Isolation is supported by demand; backup copying is an unvalidated extension.**
[Dokploy #2462](https://github.com/Dokploy/dokploy/issues/2462) has 29 reactions and
asks for isolated databases/schemas and migration hooks. Ship empty PostgreSQL
preview DBs with migrations and optional synthetic seed data first. Use a dedicated
preview PostgreSQL service, separate database and role per PR; isolate containers
per PR only when stronger trust separation and sufficient capacity justify it.

Later offer an explicit **backup-seeded copy**, not “Neon branching” or “at no cost.”
It consumes storage, memory, I/O and administrator effort. Never restore a full
`pg_dumpall` into a shared preview cluster. Sanitize in quarantine before preview
code can connect; fail closed. Details and alternatives are in the engineering review.

### 7.3 Private previews with guest links — useful, not exclusive

[Dokploy previews inherit configured security](https://docs.dokploy.com/docs/core/applications/preview-deployments);
its [application settings include basic auth](https://docs.dokploy.com/docs/core/applications/advanced).
[Coolify also documents basic auth](https://coolify.io/docs/core/networking/proxy/traefik/basic-auth).
Do not describe both as necessarily public and unprotected.

Vercel's [$20/project/month Pro password-protection price](https://vercel.com/changelog/password-protection-now-costs-20-per-project-per-month-on-pro)
is confirmed as of this review. However,
[Vercel also supports sharable links, including a limited Hobby allowance](https://vercel.com/docs/deployment-protection/methods-to-bypass-deployment-protection/sharable-links).
Password protection is not the only comparison.

First enforce mesh-only preview routes on all public ingress. Guest links require
explicit public HTTPS ingress/tunnel, revocable scoped access and expiry enforcement.
A signed URL supplies authorization; it does not make a private machine reachable.

### 7.4 Deploy guard — harden existing code

**Needed for trust; not a new exclusive feature.** [Dokploy documents automatic
Swarm rollback](https://docs.dokploy.com/docs/core/applications/rollbacks). Our engine
already has a three-minute post-live crash/restart watcher. Persist the watch and
rollback outcome, detect configured health failures, prevent stale rollback over a
newer deploy, and retain rollback images. Ten minutes is a proposed default to test,
not evidence of superiority. App rollback does not undo a database migration.

### 7.5 Memory admission — build on reservations

Keep this early, but promise conservative admission and clear failure reasons,
not “never OOM.” Include OS/platform headroom, old+new rollout overlap, pulls,
database startup, restore jobs and builds. Reconcile reservations after restart.
Display limits, working set and observed peaks separately. A Docker memory kill
needs explicit evidence; exit 137 alone is not proof. **142 vs 624 MiB** is the
provisional whole-platform comparison; **9 MiB** is only agent+mesh RSS.

### 7.6 Restore verification — prioritize before backup-seeded previews

[Coolify #2389](https://github.com/coollabsio/coolify/issues/2389) supports backup
usability, not specifically monthly drill demand.
[Dokploy #416](https://github.com/Dokploy/dokploy/issues/416) asks for more backup
destinations, another indirect signal. Scheduled application restore checks remain
a sensible reliability proposal. Do not claim nobody else tests
restores: absence was not established. Repository integrity checks and opening a
restored DB test different things. Record exactly which snapshot, DB and assertions
passed; never imply every future restore is guaranteed.

### 7.7 Battery resilience — qualify hardware, narrow the promise

Add optional staged load shedding, hysteresis and recovery of only workloads the
policy paused. A laptop battery protects the laptop, not the router or ISP. A
software health check cannot universally read or set BIOS “power on after AC loss.”
Use supported/manual/unknown and retain a physical qualification checklist. An
unplug test is not a power-loss-plus-depleted-battery recovery test.

### 7.8 Running cost — defer and label estimates

[Linux powercap](https://docs.kernel.org/power/powercap/powercap.html) exposes energy
for power zones. CPU/package energy is not whole-machine wall consumption; battery
readings do not measure AC idle draw. Prefer user-entered wall-meter readings or a
supported meter. Show source, coverage and assumed duty cycle. Hosted-price
comparisons need dated plan assumptions and omit any claim of equivalent SLA.

### 7.9 Share CLI and sleeping hosts — later, with corrected reasoning

[Tailscale Funnel](https://tailscale.com/docs/features/tailscale-funnel) already shares
local services publicly. No PaaS tracker evidence for `vfy share` does not mean no
market demand; it means we lack segment-specific evidence. Wake-on-LAN is
hardware/network-dependent and is unsuitable as an assumed wake path for critical
services. Validate either with users before expanding scope.

## 8. Small changes: keep, move or qualify

These are task candidates, not reliable “one day each” estimates.

| ID | Recommendation | Engineering constraint |
| --- | --- | --- |
| S1 | Move image-port detection into M1 | Explicit configured port wins; auto-select one unambiguous TCP `EXPOSE`; ask on none/multiple. Metadata is a hint, not proof of a listener |
| S2 | Suggest log-reported port after a probe failure | Bounded parsing and redaction; confirm changes, never silently switch routing from log text |
| S3 | Move structured log repair into M1 | Preserve multiline output and streams; redact secrets; fixture the observed garbling |
| S4 | Variable names added/removed/changed | Compare deployment snapshots; use keyed fingerprints if comparing secrets, never expose raw or reversible low-entropy hashes |
| S5 | Move variable provenance into M1/M5 | Show scope and build/runtime/preview policy; inherited is not permission to use production |
| S6 | QR after safe routes/links | QR does not grant mesh membership; guest tokens are credentials, do not post them automatically to public PRs |
| S7 | Duration and image-size delta | Distinguish compressed transfer bytes from unpacked/shared disk use; compare like architectures |
| S8 | Memory card with M4 | Show full boundary: OS, platform, apps, databases, builds, headroom; distinguish missing from zero |
| S9 | Optional status badge later | No private metadata leakage, endpoint cost bounded, no claim of historical uptime from current status |
| S10 | OOM reason with M4 | Use explicit OOM evidence and timestamps; distinguish cap exhaustion from host pressure and manual kill |

[Docker's EXPOSE reference](https://docs.docker.com/reference/dockerfile/#expose)
explains why port metadata cannot replace a health probe.

## 9. Revised delivery sequence (recommendation, not implementation approval)

Milestones are intentionally smaller and independently useful. Numeric gates below
are **proposed acceptance targets**, not measured results or market facts. Detailed
failure tests and scope are in the companion review. Existing cross-platform host
work is a dependency; this roadmap does not approve a second VM/provider stack.

| # | Milestone / dependencies | Main files or area | Proposed acceptance gate |
| --- | --- | --- | --- |
| M1 | First-deploy and preview safety; baseline evidence | `docker.rs`, `logs.rs`, `env.rs`, `proxy.rs`, engine, cockpit, docs | Single-EXPOSE image deploys; explicit override wins; log fixture intact; no unapproved production secret or direct public bypass in preview contract; publish raw baseline |
| M2 | Idle BuildKit lifecycle; after baseline | `build.rs`, `engine.rs`, host configuration | Idle builder stops within configured timeout + 60 s; active build is never stopped; next cold/warm build works; compare memory at equal app steady state |
| M3 | Explicit computer/CI builds; after M1 | build plan/API, `build.rs`, `registry.rs`, `engine.rs`, CLI, host adapter, models/types | arm64→arm64 and arm64→amd64 qualified; 1 GiB target runs pinned Next.js image under declared load without OOM; zero build workload on target; interrupted handoff cannot publish stale image |
| M4 | Small-host admission, memory card and honest profiles; uses M1–M3 | extend `placement.rs`, runtime accounting, host grant integration, DB presets, API/UI | Parallel requests cannot double-spend reservations; stale samples/restart reconciled; 1/2/4 GiB measured separately; 1 GiB is experimental until full gate passes |
| M5 | Isolated PostgreSQL previews, migrations and synthetic seed; after M1/M4 | `dbengine.rs`, `databases.rs`, `env.rs`, `previews.rs`, migrations, reconcile | Two PRs can run conflicting migrations without changing production or each other; close/restart/retry leaves no owned DB/credential/resource leaks |
| M6 | Durable deploy guard and update/reboot recovery; may run before M5 | `engine.rs`, `reconcile.rs`, deployment state, `host/vercelify`, contracts | Healthy→unhealthy and crash-loop fixtures recover when compatible rollback exists; no “rolled back” before verified serving; manual/new deploy cancels stale watch; reboot restores declared state |
| M7 | Independently shippable trust workflows: M7a restore drills; M7b guest links | backup manifest/job, restore isolation; share API/auth/router after privacy contract | Drill catches corrupt/missing/incompatible dump and missing key; reports scope/RPO/RTO. Guest link works from non-mesh network, fails on expiry/revoke/PR close and all bypass paths |
| M8 | Qualified laptop resilience; evidence-based launch material; gated backup-copy beta | power guard/doctor; documentation/examples/security; backup-copy path only after M5/M7a | Physical outage/recovery matrix recorded; claims link to qualified configuration; backup copy remains disabled until quarantine+scrub+resource gates pass |

Cost meter, status badges, generic share CLI and sleeping workers are outside the
initial release gate. Comparison documentation, `SECURITY.md`, support scope and
quickstart work start with M1 and mature throughout; they are not held until M8.

**Growth gate:** recruit a small owned-hardware/solo-developer pilot, observe real
first deployments and repeat use, then expand scope only around measured blockers.
No issue count predicts adoption. See the companion review for the proposed pilot,
activation metrics and contributor path.
