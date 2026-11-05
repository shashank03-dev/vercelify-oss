# System design: the Vercelify platform and mesh

This document fixes how Vercelify works: process and crate
boundaries, the database schema, state machines, wire protocols, security, and what
happens when things fail. Code follows this document. A change of design changes this
document in the same commit.

Contents

1. [Principles](#1-principles)
2. [Processes and crates](#2-processes-and-crates)
3. [Identifiers, time, errors](#3-identifiers-time-errors)
4. [Platform state (SQLite)](#4-platform-state-sqlite)
5. [Secrets](#5-secrets)
6. [Builds](#6-builds)
7. [Deployments](#7-deployments)
8. [Reconciler and runtime](#8-reconciler-and-runtime)
9. [Proxy and routing](#9-proxy-and-routing)
10. [Databases](#10-databases)
11. [Git, previews, hooks](#11-git-previews-hooks)
12. [API](#12-api)
13. [Mesh](#13-mesh)
14. [Security model](#14-security-model)
15. [Failure modes](#15-failure-modes)
16. [Resource budgets](#16-resource-budgets)
17. [Decisions log](#17-decisions-log)
18. [Setup and reaching the main server](#18-setup-and-reaching-the-main-server)

---

## 1. Principles

1. **The main server's database is the only source of truth for desired state.** Docker,
   Traefik files and the mesh are derived from it and can be rebuilt from it.
2. **Every derived artefact is reconciled, never patched incrementally.** A loop compares
   desired with actual and converges. Handlers write desired state and nudge the loop.
3. **Nothing in the data path depends on the control plane being up.** Running containers
   keep running, Traefik keeps routing, the mesh keeps its last peer map.
4. **One writer per resource.** Deployments of one app are serialised; the build queue has a
   fixed concurrency; the database has one writer connection.
5. **Small and boring.** The docker CLI instead of a socket client (as the agent does
   today), SQLite instead of a server database, a pinned Traefik instead of our own proxy,
   userspace WireGuard (`boringtun`) instead of a kernel/netlink stack.
6. **Every long operation is a job** (`events.rs`) with progress on `/v1/events`.

## 2. Processes and crates

```
                          main server                                         worker / Pi
┌──────────────────────────────────────────────────────────┐   ┌──────────────────────────────────┐
│ vercelify-agent  (role = main)                           │   │ vercelify-agent (role = worker)  │
│  ├ http API  :7420 on mesh IP + 127.0.0.1                │   │  ├ http API :7420                │
│  ├ control   :7443 TLS (pinned) — enroll, map, relay     │   │  ├ runtime  (/v1/runtime/*)      │
│  ├ platform  store · builder · deployer · reconciler     │◀──┼──┤  └ existing host features        │
│  ├ coordinator (mesh registry, certificates, maps)       │   │                                  │
│  └ existing host features (doctor, sleep, backups…)      │   │ vercelify-mesh  (tun vfy0)       │
│ vercelify-mesh  (tun vfy0, UDP :51820, DNS on mesh IP)   │   │ vercelify-proxy (Traefik)        │
│ vercelify-proxy (Traefik container, :80/:443)            │   │ power-guard, firewall            │
│ power-guard, firewall, cloudflared (optional)            │   └──────────────────────────────────┘
└──────────────────────────────────────────────────────────┘
                 ▲ control :7443 (LAN / public / tunnel)  ▲ WireGuard UDP or relay
┌────────────────┴──────────────────────────────────────────┴───────┐
│ Mac: cockpit (Tauri) ──SOCKS5 127.0.0.1:7480──▶ vercelify-mesh     │
│      vfy CLI ─────────curl --socks5-hostname──▶ (LaunchAgent,      │
│                                                  userspace netstack)│
└────────────────────────────────────────────────────────────────────┘
```

| Crate | Kind | Depends on | Responsibility |
| --- | --- | --- | --- |
| `vercelify-core` | lib | — | Wire models (ts-rs), pairing codes, host probes (unchanged role) |
| `vercelify-platform` | lib | core | Store + migrations, secrets, ids, builder, detector, deployer, reconciler, templates, Traefik file generation for base routes. **No HTTP server**, no axum: the agent mounts it |
| `vercelify-mesh` | lib + bin `vercelify-mesh` | core | Addressing, keys, certificates, peer maps (shared with the coordinator); the daemon: WireGuard (boringtun), tun device / userspace netstack, disco, STUN, relay client, DNS, SOCKS5 |
| `vercelify-agent` | bin | core, platform, mesh (lib, coordinator half) | HTTP API, auth, jobs/events, coordinator, relay server, existing host features |
| cockpit `src-tauri` | bin | core | UI backend; talks to agents through the local mesh SOCKS5 |

Why the mesh daemon is its own process: the network must survive agent restarts and
updates (the update itself restarts the agent over the mesh). Why the coordinator lives
in the agent: it needs the same store, tokens and audit as everything else.

## 3. Identifiers, time, errors

- **Ids**: `<prefix>_<16 chars Crockford base32>` from 80 random bits, e.g.
  `app_7k2m9q4w1x8z3c5v`. Prefixes: `prj env app dom var dpl prv db bak vol node mn git hook task tok`.
  Readable in logs, unambiguous across tables, safe in URLs, paths and Docker names.
- **Slugs**: app and database `name` are DNS labels (`^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$`),
  unique across apps *and* databases, because they become hostnames and container names.
- **Time**: RFC 3339 UTC with seconds (`2026-10-25T05:01:18Z`), as the agent does today.
- **Errors**: `vercelify_platform::Error` (`NotFound`, `Conflict`, `Invalid`, `Unavailable`,
  `Internal`) maps 1:1 onto the agent's `AppError` (404, 409, 400, 503, 500). Messages are
  for people and say what to do.

## 4. Platform state (SQLite)

File: `/var/lib/vercelify/platform.db` (main server only). Opened with
`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`.
Migrations are embedded SQL files run in order inside one transaction each; the version is
`PRAGMA user_version`. A database newer than the binary refuses to open (rollback safety:
`vercelify rollback` restores the copy taken when the update started, see §15; the error
names `vercelify restore-state`).

Access: one `Store` owning a write connection behind a mutex, used from
`spawn_blocking`. Every query is a few rows on a few thousand-row tables; a pool would buy
nothing on this hardware.

### 4.1 Schema (migration 0001)

The authoritative copy is `crates/vercelify-platform/migrations/0001_init.sql`. Summary:

| Table | Key columns | Notes |
| --- | --- | --- |
| `settings` | `key` PK, `value` (JSON) | Domain mode, wildcard, preview template, build concurrency… |
| `nodes` | `id`, `name` UNIQUE, `role` main/worker, `agent_url`, `mesh_node_id`, `arch`, `builds`, `labels` JSON, `last_seen_at` | Servers that run apps |
| `mesh_nodes` | `id`, `name` UNIQUE, `kind` server/device, `public_key` UNIQUE, `ip_index` UNIQUE, `role`, `token_hash` UNIQUE, `endpoints` JSON, `cert_expires_at`, `revoked_at` | Everything on the mesh |
| `projects` / `environments` | `name` UNIQUE (per project) | `production` created with every project |
| `apps` | `environment_id`, `name` UNIQUE, `kind` git/image/compose, `source` JSON, `build` JSON, `runtime` JSON, `node_id`, `desired` running/stopped/sleeping, `current_deployment_id`, `previews` JSON | |
| `app_domains` | `host` UNIQUE, `app_id`, `https`, `is_primary`, `redirect_to`, `cert_status` | `cert_status` pending ↔ issued follows the certificates in the proxy's ACME storage, read at each reconcile (TLS ends on the main server) |
| `env_vars` | `scope` project/environment/app, `scope_id`, `key`, `value` BLOB (sealed), `build`, `preview_only`, `secret` | UNIQUE(`scope`,`scope_id`,`key`,`preview_only`) |
| `deployments` | `app_id`, `number` (per app), `kind` production/preview, `pr_number`, `commit_sha`, `trigger`, `rollback_of`, `image`, `node_id`, `status`, `error`, `container`, timestamps | UNIQUE(`app_id`,`number`) |
| `previews` | UNIQUE(`app_id`,`pr_number`), `head_sha`, `deployment_id`, `status` | |
| `databases` | `name` UNIQUE, `kind`, `version`, `image`, `node_id`, `credentials` BLOB (sealed), `public_port`, `public_bind`, `desired`, `backup_schedule` | |
| `database_backups` | `database_id`, `status`, `bytes`, `snapshot` | |
| `volumes` | `owner_kind` app/database, `owner_id`, `name`, `mount_path`, `tier` ssd/hdd, `host_path`, `storage_name` | UNIQUE(`owner_kind`,`owner_id`,`name`) |
| `git_sources` | `provider`, `base_url`, `auth` none/token/deploy_key/github_app, `secret` BLOB (sealed), `public_key` | |
| `deploy_hooks` | `app_id`, `secret_hash` UNIQUE | |
| `scheduled_tasks` | `app_id`, `schedule`, `command`, `enabled`, `last_*` | |

Tokens and the audit log stay in `tokens.json` and `audit.log` on every node: workers
keep no platform database but must still authenticate the main server and devices.
Tokens gain a `role` (§12).

Rules enforced in SQL (CHECK constraints) rather than only in Rust: enum columns, DNS-label
names, `public_port` range, `number > 0`. Foreign keys cascade from apps to their domains,
variables, deployments, previews, volumes, hooks and tasks; a database refuses deletion
while an app references it in a variable reference (§10).

### 4.2 Backups and standby

Online copy via SQLite's backup API to `platform.db.bak` before every migration, every
`vercelify update`, and every 5 minutes for restic and the warm standby (§8.4). Restic
backs up a copy, never the live file: `vercelify backup run` takes a fresh one with
`sqlite3 .backup` into its staging directory when `sqlite3` is installed, else it saves
the 5-minute copy; `platform.db`, `-wal` and `-shm` are excluded. The master key and the
mesh directory go with it. `vercelify backup restore --instance` stops the agent and the
app containers, puts back the master key, the state copy (as `platform.db`, without a
stale WAL) and the mesh directory (so workers and devices keep their certificates), then
the volumes, and starts the agent, whose reconciler brings apps and databases back.

## 5. Secrets

- **Master key**: 32 random bytes at `/var/lib/vercelify/master.key`, mode 0600, owned by
  root. Created by the installer, or by the agent on first start if missing. Included in
  restic backups (which are themselves encrypted with the user's restic password).
- **Backup recovery kit**: the restic password lives in `/etc/vercelify/backup.env` (0600)
  and never appears in `GET /v1/backup`. An owner token can `POST /v1/backup/recovery-kit`
  (audited like any change) for the password, repositories and S3 keys with restore steps;
  the Mac app saves it as a 0600 file in Downloads, never over an existing file.
- **Sealed value** (`secrets::seal`): `0x01 ‖ nonce (24 B) ‖ XChaCha20-Poly1305(ciphertext ‖ tag)`.
  AAD = `"<table>/<column>/<row id>"`, so a value copied into another row or column fails
  to open instead of silently decrypting.
- **Key rotation** (`vercelify secrets rotate`): new key file, re-seal every sealed column in
  one transaction, keep the old key as `master.key.old` until the next backup succeeded.
  Version byte `0x01` leaves room for a future scheme.
- Secrets never enter logs, audit entries, job messages, error strings or Docker labels.
  They reach containers as `--env-file` from a 0600 file in `/run/vercelify/env/` that is
  deleted after `docker run`/`create` returns.

### 5.1 Preview policy (M1)

Migration 0007 adds `env_vars.share_with_previews`, default false for existing
and new normal values. Preview-only values win over explicitly shared values;
production-only values are withheld before interpolation, and production database
references are rejected for previews. Provenance responses expose keys, scopes and
policy, without values. Sharing a variable is a deliberate operation, independent
of its `secret` flag.

Deployment and runtime log redaction must hide every nonempty secret, including
one-character values. Match against the original text, longest value first at
each position; inserted `[redacted]` markers must never be processed again.
Multiline secrets also supply individual nonempty line patterns. Redaction is
defense in depth, not containment of malicious code that receives a secret.

Runtime previews join only `vfy-preview-<app id>-pr<n>`: a labelled Docker bridge
with `internal=true`, IPv6 off, and IPv4 gateway mode `isolated` (Docker 28+).
Validate existing networks before use; unsupported Docker or a name collision
fails closed. The proxy alone joins these bridges for ingress. Preview containers
never join production project networks or `vfy-proxy`; raw sockets are dropped and
privilege escalation disabled. This conservative first version disables runtime
outbound internet access as well. Compose dependencies can share their own PR's
bridge. M5 will add scoped platform preview database credentials; until then all
`${{db.…}}` references are production references and are refused in previews.

Health probes run through the pinned proxy's `nc`/`wget` utilities because the
isolated bridge has no host gateway. Proxy recreation reconnects existing bridges
without recreating concurrently deleted ones. Failed starts remove unused bridges;
PR close and app deletion remove owned bridges after containers. Remote runtime
apply carries the same cleanup operations. Compose 2.24.4+ `!override` replaces
all service networks, including sidecars, with the PR bridge; direct published
ports, host namespaces, elevated privileges, Docker API socket injection and custom
providers are refused before execution. Compose subprocesses inherit only Docker
connection/configuration settings and explicit deployment variables, rather than
the agent's environment.

Migration 0007 saves the existing active previews in a durable retirement list.
Startup closes and cleans them before starting deployment/reconciliation work;
each successful cleanup is persisted so a crash can retry. An upgrade therefore
requires owners to review preview variables and redeploy existing previews.
The real Docker contract is `host/tests/contract/preview-safety.sh`; validation
results and remaining qualification boundaries are recorded with M1's evidence.

## 6. Builds

### 6.1 Queue

- A single `BuildQueue` task with concurrency `settings.build_concurrency` (default from the
  hardware profile: 1 on ≤ 8 GB, 0 on a Pi 4, meaning "build elsewhere").
- Queue order: FIFO, except that a newer production deployment of the same app supersedes a
  queued (not yet started) older one (`status = superseded`).
- Thermal pause: before starting a build and every 5 s during one, read
  `/run/vercelify/power-guard.state`. `thermal_paused` → `docker pause` the BuildKit
  container (`buildx_buildkit_vercelify0`), resume when it clears. `shutting_down` → cancel.
  Cancelling a deployment that waits for the power guard or a build slot ends it at once
  (the wait is raced against the cancel) and frees the app for its next deployment.

### 6.1.1 Idle BuildKit lifecycle (competitive M2)

`agent.toml` `builder_idle_minutes` defaults to 10; 0 disables idle stopping.
A dedicated agent task checks every 30 seconds, independently of reconciliation.
`BuildGate` returns an RAII permit that records monotonic completion time on drop
(success, failure or cancellation). Maintenance nonblockingly acquires all build
slots before checking elapsed time, and retains them through Docker inspection
and stop. An active or starting build therefore cannot race an idle stop.
Startup grants a fresh full idle interval; maintenance does not reset the clock.

Only the dedicated `buildx_buildkit_vercelify0` container is stopped, with a
10-second Docker grace. Missing, stopped or paused containers and held power
states are skipped; errors are logged and retried next tick. The container and
cache volume are retained. Existing builders are explicitly bootstrapped before
buildx build; bootstrap failure fails the build without replacing its builder.
The embedded `docker` driver is untouched. External commands using this builder
must coordinate with the agent or disable idle stopping.

### 6.2 Pipeline

| Step | What | Fails when |
| --- | --- | --- |
| fetch | `git init` + `git fetch --depth 1 origin <sha>` into `/var/lib/vercelify/builds/<dpl>/src`, `GIT_TERMINAL_PROMPT=0`, inherited config ignored, credentials from the git source (token in an `http.extraHeader` passed by env, or `GIT_SSH_COMMAND` with the deploy key and a pinned known_hosts) | repository or commit unreachable |
| plan | `detect::plan(src, app.build, vercel_json)` → `BuildPlan { kind: Dockerfile(path) \| Generated(String) \| Compose(path) \| Image(ref), context, port_hint, notes }` | no plan: message lists what was looked for |
| build | `docker buildx build --builder vercelify --load -t vfy/<app>:<dpl> --build-arg …` (build-time variables as `--secret id=env,src=…` for secrets, `--build-arg` for the rest), `--cgroup-parent vercelify-build.slice`, BuildKit cache mounts in the generated Dockerfiles | non-zero exit; log tail in the error |
| push | Only for instances on other servers: tag `127.0.0.1:5055/vfy/<app>:<dpl>[-<arch>]`, push to the mesh registry (§8.2); image apps are pulled here once and pushed the same way | registry down |
| cleanup | remove `src`, keep the log, prune images of the app beyond the 5 newest that are not current/rollback targets | never fails the build |

The build log is `/var/lib/vercelify/logs/<app>/<dpl>.log` (line-oriented, `ts stream text`),
streamed live to subscribers through `/v1/deployments/{id}/logs?follow=true`. The last 20
logs per app are kept.

### 6.3 Detector ("vfy pack")

Order: explicit `build.pack` → `Dockerfile` at `build.dockerfile` (default `Dockerfile`) →
compose file → language detection. Detection reads only files (never runs code from the
repository on the host), and only the checkout's own: the context, Dockerfile, Compose
file, output directory and every file read are resolved with symbolic links followed and
refused (or ignored) when they lead outside it; BuildKit gets the resolved context, checked
again where the build uses it. Every pack runs the install and build commands it is given
(the app's over `vercel.json`'s): Node and Static as their steps, the other languages after
`COPY . .` (Go and Rust then compile unless the build command left the binary, `/out/app`
/ `/app`). What a plan can't use (commands with a Dockerfile, an output directory for a
server) is a note in the build log.

| Detected by | Generated image |
| --- | --- |
| `package.json` (+ lockfile → npm/pnpm/yarn/bun; Node from `engines.node`/`.nvmrc`/`.node-version`, default 22) | multi-stage `node:<v>-slim`; Next.js: `output: standalone` when configured, else `next start`; Nuxt/SvelteKit/Remix/Astro-server: `node <entry>`; Vite/CRA/Astro-static/plain static: build then serve `outputDirectory` with the static server stage |
| `requirements.txt` / `pyproject.toml` / `uv.lock` / `poetry.lock` | `python:<v>-slim`; Django → `gunicorn <project>.wsgi`, FastAPI → `uvicorn`, Flask → `gunicorn` |
| `go.mod` | `golang:<v>` build → `gcr.io/distroless/static` |
| `Cargo.toml` | `rust:<v>` build (release, cache mounts) → `debian:12-slim` |
| `deno.json` / `deno.jsonc` | `denoland/deno` |
| `bun.lockb` / `bun.lock` without Node framework | `oven/bun` |
| `Gemfile` (+ Rails) | `ruby:<v>-slim`, `bundle exec puma` / `rails server` |
| `composer.json` (+ Laravel) | `php:<v>-fpm` + Caddy |
| `index.html` at the root | static server stage only |

`vercel.json` `installCommand`, `buildCommand`, `outputDirectory`, `framework` override the
detected values. The generated Dockerfile is printed at the start of the build log. The
static server stage is a pinned `caddy:2-alpine` with a Caddyfile that honours `cleanUrls`
and `trailingSlash`; redirects/rewrites/headers stay in Traefik (`routing.rs`) as today.

### 6.4 Explicit computer and CI builds (competitive M3)

`vercelify-pack` reuses the detector and builder on the selected computer's Docker
runtime. `vfy deploy APP --build-here` fetches an exact commit into a private
throwaway checkout; dirty working files and the developer's home/SSH agent are
not mounted into build steps. Docker contexts are never switched; retained builders must match the selected
endpoint, and new builders receive that endpoint explicitly. A named
`vfy-local-<app id>` container builder isolates each app's cache; local handoffs
rerun generated recipe steps using an opaque deployment ID build argument because
BuildKit secret contents do not change its cache key. Package cache mounts remain
reusable. Repository Dockerfiles use `--no-cache` conservatively, which also resets
their cache mounts. The helper
redacts build output and errors and handles cancellation before removing its
private work directory. The CLI explicitly enables errexit inside its command
substitution and cancels its lease on failure or signals.

`--image IMAGE@sha256:DIGEST` hands off an image already built by CI, with a full
source SHA supplied by `--commit` or the OCI revision label. It uses the same job
and upload path, without forcing an image through a build pack. Git apps use this
handoff; ordinary image apps can already deploy a pinned registry digest directly.
No unattended fork scheduler or automatic desktop selection is introduced.

Migration 0008 persists `build_jobs`, one per deployment: device-token owner,
generation, exact commit (deployment), target platform, plan/version/hash,
configuration/resolved-variable revision, soft/hard deadlines, phase and image
config digest. The first explicit lease uses generation 1; retries create a new
deployment/job identity rather than reassigning a running job. A lease lasts
300 seconds, renewed by the CLI every 45 seconds, with a 90-minute hard limit.
States are `leased → uploading → verified`, or `failed`. Public job metadata
contains no variable values; only the authorized creation response supplies
build-marked variables. Build-time DB references require an explicitly configured
mesh port and resolve to the main server's mesh address. Runtime-only DB variables
need no published port for a local app.

Deployment and lease creation are one transaction. The dispatcher skips a queued
job until verified; every five seconds it expires abandoned jobs. Restart fences
incomplete jobs, while already verified artifacts may dispatch. Upload, renewal
and publication check owner, generation, deadline, deployment status and preview
head/open state. Publication additionally checks the immutable settings/variable
revision. Newer deployment creation supersedes an older queued handoff. Revocation
is checked again after load. No missing/failed off-host image falls back to a
serving-host build. `build_concurrency = 0` now enforces a runtime-only host: normal
Git/Compose work stays queued with “no permitted builder,” with cancellation and
an explicit computer/CI retry available.

The authenticated upload streams a declared tar (maximum 3 GiB) to mode-0600
files, with disk headroom for the archive, its sanitized copy and 512 MiB reserve.
Metadata is capped at 1 MiB, paths/regular entries/checksums/digests are validated,
links/extensions/duplicate names rejected, and no archive entry is extracted onto
the host. Only one job-owned tag, config and referenced layers reach `docker load`;
alternate OCI indexes and unrelated objects are omitted. The protocol's digest is
the SHA256 of the saved image config, not Docker 29's store-dependent `.Id` (which
can name an OCI index). Layer content hashes, loaded rootfs diff IDs, architecture
and runtime configuration are checked. Temporary guards survive a background
sanitizer and clean up on interruption. An incomplete upload cannot dispatch.

API (device-token authentication): `POST /v1/platform/apps/{app}/build-jobs`,
`GET /v1/platform/build-jobs/{id}`, `POST …/{id}/renew`, `PUT …/{id}/image` (generation
and config digest query parameters). Cancellation uses the deployment endpoint.
The target node is pinned for the job; foreign worker images receive the existing
architecture tag before registry distribution. Qualification and hardware limits
are recorded with M3, not inferred from the presence of these APIs.

## 7. Deployments

### 7.1 State machine

```
             ┌──────────── cancel ────────────┐
             │                                ▼
 queued ──▶ building ──▶ deploying ──▶ healthy ──▶ superseded
   │           │              │           │
   │           └──▶ failed ◀──┘           └──▶ (rolled back: superseded, rollback deployment becomes healthy)
   └──▶ superseded (a newer deployment of the same app was queued first)
image apps / rollbacks: queued ──▶ deploying (no build)
```

Only one deployment per app is in `building|deploying` at a time (per-app mutex in the
deployer). `apps.current_deployment_id` points at the `healthy` one serving traffic; the
switch of that pointer and the route file is the commit point of a deploy.

### 7.2 Deploy procedure (single container apps)

The listening port: the app's `runtime.port` when set; for a build, the plan's port;
for an image app with neither, the image's `EXPOSE` (`.Config.ExposedPorts`, pulled
first): exactly one TCP port is used and kept on the deployment (rollbacks reuse it);
none or several stop the deployment at once, naming the ports and asking for one.
`EXPOSE` is metadata, not proof of a listener, so the health check still decides.

1. Pick the node (§8.3). Ensure networks `vfy-<project>` and `vfy-proxy` exist on it.
   Previews instead use only the PR's isolated bridge (§5.1), with the proxy joined
   for ingress and health probes; they never attach to these production networks.
2. `docker create` the container `vfy-<app>-<dpl suffix>` with labels
   (`vercelify.managed=platform`, `vercelify.app`, `vercelify.deployment`, `vercelify.kind=app|preview|db`,
   `vercelify.project`), env file, limits (`--memory`, `--cpus`, `--pids-limit 4096`),
   `--restart unless-stopped`, `--cgroup-parent vercelify.slice`, volumes, `--network vfy-<project>`,
   then `docker network connect vfy-proxy`; `docker start`.
3. Health: `runtime.healthcheck` (`{http: "/healthz"}` or `{cmd: [...]}`) polled every 2 s from
   inside the proxy network (an HTTP GET through the agent to the container IP), else TCP
   connect to the port. Must pass 2 times in a row within `runtime.health_timeout_secs`
   (default 180).
4. Commit: write the app's base route file (§9) pointing at the new container, atomically
   (`write tmp → fsync → rename`); set `current_deployment_id`, old deployment `superseded`.
5. Drain: wait `runtime.drain_secs` (default 10), `docker stop --time 20` the old container,
   `docker rm` it. Keep its image.
6. On failure before step 4: remove the new container, keep the old one, deployment `failed`
   with the reason and the last 50 lines of the container's log, alert.

M6 deploy recovery is explicit: `runtime.rollback_compatible` defaults false and
means older images remain compatible with current migrations and persistent data.
It never restores a production database or volume. Every production publication
records a hash of declared runtime, resolved variables, placement, volume identity
and effective port. A matching previous snapshot plus a retained generated build
image or immutable image ID/digest can arm the three-minute observation journal.
Legacy snapshots and mutable external tags require manual recovery. Compose remains
manual. Startup grace defaults to ten seconds (0–120), failure threshold to three
(1–10); two additional restarts also qualify. Baselines are collected before
publication, not at the first poll. API policy changes and production publication
share a commit gate; an enqueue during publication may ask its caller to retry.

`0011_deploy_guard.sql` persists original deadline, restart baselines, consecutive
known probe failures, prior image/config identity, evidence, one rollback job and
watching/requested/verified/failed/cancelled/expired outcomes. Unknown remote state,
missing containers and paused workloads never count as defective-image evidence.
Missing containers remain the reconciler's work. Healthy-but-running failures use
the declared health policy. A new job, stop/sleep or changed configuration fences
the watch. Guard request and rollback enqueue are one transaction; work and
publication recheck that authority, so a queued rollback cannot override a later
stop or manual deploy. Automatic rollbacks do not recursively arm another guard.

A requested rollback is reported as requested. Only healthy/current rollback,
direct runtime health and a successful local ingress probe mark verified. Recovery
has a five-minute verification deadline and never spins indefinitely. Protected or
custom ingress may need manual verification; this is local readiness, not ISP or
external DNS/TLS availability. Metadata/error classifications contain no environment
values or raw health-command output. `deployment-guards` and `serving-checks` health
capabilities gate the UI and update checks. Authenticated GETs expose per-app
`deployment-guards` (latest 100) and a read-only `serving-check` result; stopped or
sleeping apps are never started to test readiness.

Publication also records exact node/container/deployment retirement ownership and
an absolute drain deadline. A minimum two-second propagation allowance applies even
when requested drain is zero; default drain stays ten seconds. Reconciliation
protects an unexpired drain, then removes the exact superseded generation after
restart and clears its stale instance metadata. A reused name with a different
owner is never adopted for deletion. Image pruning/deployment deletion preserve
active guard targets. Starting/healthy transitions clear obsolete queue reasons.
Shared project bridge removal checks stored app/database ownership, including
stopped peers, and preserves the bridge if ownership is unknown.

Compose apps: `docker compose -p vfy-<app> -f <file> -f <generated override> up -d --wait`,
where the override adds our labels, networks, limits and the env file. The routed service is
`runtime.service`. Zero downtime is best effort (compose recreates in place). A preview's
override also renames every top-level volume to `vfy-<app>-pr<n>_<key>` (not external,
labelled with the app and PR), even one the file names explicitly; the merged
`docker compose config` is checked before `up` and the preview is refused when storage
could still be shared (a bind mount resolving outside the checkout, driver options, another
container's volumes) or it would read the server's files (an env file, secret or config
file, or SSH key outside the checkout; external secrets). `config` runs with
`--no-env-resolution`, so env files are read only by `up`, after the check; a Compose too old
for the flag gets no compose previews. What Compose reads while resolving (each project
directory's `.env` for `${…}`, `include` paths, env files and project directories,
`extends.file`, recursively) is found in the YAML itself (`compose::implicit_reads`,
yaml-rust2, ~95 KiB of the agent) and must be in the checkout before Compose runs at all.
`<<` merges are resolved throughout each document first, every `extends.file` anywhere in it
counts (a superset of Compose's reads), a path Compose would interpolate is refused, and a
file over 1 MiB or 100,000 nodes with its aliases expanded (counted from parser events,
before anything is built) is refused. Closing the PR removes those labelled volumes. Every
compose app's builds read only the checkout: a `build.context`, Dockerfile or
`additional_contexts` path outside it is refused (production may still mount server files
at run time).

### 7.3 Rollback

`POST /v1/apps/{id}/rollback {deployment_id}` creates a deployment with `trigger = rollback`,
`rollback_of`, the target's `image` and `commit_sha`. If the image still exists on the node
(or in the registry) it skips the build ("instant"), otherwise it rebuilds that commit
("rebuild"). Rollback pins: `apps.source.pinned_sha` is set so auto-deploy does not
immediately redeploy the branch head; deploying again clears it.

### 7.4 Deleting an old deployment

`DELETE /v1/platform/deployments/{id}` (owners) removes the row and its log. It is
refused (409) for the app's live deployment, one in progress, an open preview's, one
with containers (`instances`), and for any deployment while the app has one in
progress (a queued rollback may need it). It runs in one SQLite transaction, so it is
serialized with deploys and rollbacks. Its image is not removed: the per-app pruning
(keep the 5 newest, plus what other deployments use) collects it, so a digest shared
with another deployment, replica or preview is never deleted early. `rollback_of`
pointing at it becomes NULL.

## 8. Reconciler and runtime

### 8.1 Loop

Every 30 s and on every nudge (a handler wrote desired state):

```
for each node:
   actual  = runtime.list(node)            // docker ps --filter label=vercelify.managed=platform
   desired = store.desired_containers(node) // current deployments of running apps, previews, databases
   diff →  start (stopped but desired running, not asleep, not frozen by the power guard)
           recreate (missing; a `recover` deployment of the same image, no build,
                     through the normal health-checked path, at most every 5 min per app)
           remove (labelled ours, belongs to no current deployment, older than 2 min)
           report (crash loop: restarted > 5 times in 10 min → alert, back off to 5 min)
regenerate route files for the node; write only files whose content changed
```

Starts are admitted against the node's capacity sample. When a pass is refused only
because no fresh sample exists yet (right after a boot or an agent restart, before
the fleet task's first measurement), the next pass runs after 3 s instead of the
normal interval, for up to 20 retries, so apps stopped by a clean shutdown come back
without waiting a full tick.

Stopped and sleeping apps lose their base route, so visitors get the wake page
(sleeping) or a 404 instead of a 502 from a dead upstream; routes of apps with a
deployment in flight are left to the engine. The reconciler never builds and never
changes `current_deployment_id` itself. Snapshot-and-apply serializes with deployment
route publication through the app-runtime lock. Every active deployment protects
its route key, including a first deployment without a serving predecessor. It
never waits on a download: a database
whose image isn't on the machine yet gets its pull started in the background (once
per image), and a later pass starts it, so a first pull over slow home internet
doesn't hold up bringing other apps back. Sleeping apps
(`sleep.rs`) are "desired asleep" while the sleep module says so; the wake listener starts
them, and the reconciler leaves them alone.

### 8.2 Workers and the runtime API

A worker is a server on the mesh whose agent runs with `role = worker`, `engine =
"platform"` (`sudo vercelify mesh join TOKEN --platform`, which a platform main
server's invite prints). It keeps no platform state:

- **Registration.** Every minute (every 10 s while failing) it sends `POST
  /v1/platform/nodes/heartbeat` to the main server over the mesh with its node token,
  accepted from its own mesh address only (§13.2). The body carries its architecture,
  agent port, resources and condition (`NodeStatus`: CPUs, memory, battery, the power
  guard's thermal pause, Docker up, its proxy's HTTP port) and its **runtime token**
  (32 random characters in `runtime.token`, 0600). The first heartbeat creates the
  `nodes` row; later ones refresh it. The main server keeps the runtime token sealed
  (`nodes.token`, AAD `nodes/token/<id>`); revoking the node on the mesh clears it. A
  worker not heard from for 2 minutes is offline.
- **Runtime API** (`/v1/runtime/*`, `runtime_api.rs`): accepted only with the runtime
  token *and* from the coordinator's mesh address (auth.rs), and only for these paths.
  The worker does with the same platform code what the main server does for itself:

| Route | Does |
| --- | --- |
| `GET /v1/runtime/containers` | our containers (`docker ps`, label `vercelify.managed=platform`) |
| `GET /v1/runtime/containers/{name}` | `docker inspect` (404: gone) |
| `GET /v1/runtime/containers/{name}/logs?tail&follow` | `{at, stream, text}` lines |
| `POST /v1/runtime/containers/{name}/exec` | run a command in it (`{cmd, timeout_secs}` → `{code, tail}`): scheduled tasks of apps placed there |
| `POST /v1/runtime/instances` | start one instance (`StartSpec`: app, deployment, image, variables, volumes, port, replica) and wait for health; progress as `{"log"}` lines, then `{"container"}` or `{"error", "status"}` |
| `POST /v1/runtime/apply` | idempotent batch: start, stop, restart, remove, write/remove routes, remove volumes, prune images; failures listed per item |
| `GET /v1/runtime/routes` | route keys on its proxy |
| `POST /v1/runtime/health` | wait until a container answers (waking) |
| `/v1/runtime/terminals…` | a terminal in a container (§12.1 protocol) |

- **Its proxy.** Each worker runs `vercelify-proxy` too. Public traffic arrives at the
  main server (port forwards and the tunnel point there): its route for an app lists
  its own containers and `http://<worker mesh IP>:<http port>` for every server with
  instances; the worker's route sends the same hosts (plain HTTP, the Host header
  kept) to its local containers. TLS, redirects and Basic Auth are the main server's
  (Basic Auth is checked on both). Servers that are offline leave the main route
  (unless none is online: a 502 says more than nothing).
- **Images.** The main server builds and runs `vfy-registry` (pinned `registry:2`,
  128 MB, published only on 127.0.0.1:5055, data in `<state>/registry`). Every
  server calls it `127.0.0.1:5055`: Docker trusts a loopback registry without TLS,
  so no `daemon.json` changes and no Docker restart. A worker serves read-only
  requests on that local port and forwards them through the main agent's
  `/v1/mesh/registry/v2/…` route, authenticating with its mesh node token.
  Only active enrolled servers may read; device tokens and all registry writes
  are refused. Accept/Range and response bodies stream without buffering layers.
  WireGuard encrypts the node-to-node path. Upgrade recreates an existing raw
  registry with loopback-only bindings, including when no workers remain; old
  workers must be upgraded to use the authenticated reader. The engine starts the registry before the first push and
  waits for it. Built images go as `vfy/<app>:<dpl>`; image apps placed elsewhere are
  pulled here once (for the target's architecture) and pushed the same way, so
  workers need neither internet access nor registry credentials. Registry tags this
  server no longer keeps are deleted after each deploy, layers collected daily.
- **Architectures.** Builds run on the main server; for a target of another
  architecture the builder runs a second `buildx --platform linux/<arch>` into
  `vfy/<app>:<dpl>-<arch>` (QEMU emulation; without binfmt the build fails with
  `sudo vercelify registry multiarch` or "pin it to a <arch> server").
- **Everything through the engine:** deploys, the reconciler (one `docker ps` per
  server per 30 s), sleep/wake, restart, logs, terminals (relayed: the main server
  opens it on the worker and passes output, input and resizes through under an id of
  its own, owned by the token that opened it), automatic rollback (it inspects the
  instances where they run), preview teardown and app deletion (containers, routes,
  images and, when asked, volumes on every server). The main server is just another
  node to itself: `NodeRuntime::Local` runs the same calls without HTTP.
- **Databases** run on the main server. An app on another server gets
  `${{db.<name>.url}}` with the main server's mesh address and the database's public
  port, so that database needs one (it binds the mesh address by default); the
  coordinator lists public database ports in the maps (§13.8).
- Moving volumes between disks, compose apps and databases stay on the main server.

### 8.3 Placement

`app.node_id` if set. Otherwise the usable server with the most memory to spare
(`memory_available − memory_mb` (default 256 MB)) wins, ties to the main server.
Usable: heard from in 2 minutes, not drained, Docker up, not paused by the power guard,
not on battery below 50 %. Placement is decided once per app (sticky: the first deploy
records `node_id`), so volumes stay put; changing `node_id` of an app with volumes
that has run is refused. A drained server keeps what is pinned to it and takes nothing
new (`PATCH /v1/platform/nodes/{id} {drained}`); a server with instances can't be
removed (`DELETE`). Removing a worker marks its mesh identity
(`mesh_nodes.hosting_removed_at`): its next heartbeat is refused (409, the worker then
asks every 10 minutes) instead of registering it again as a new, undrained worker.
Pairing the machine again clears the mark; revoking it on the mesh is separate.

Capacity admission (competitive M4) extends `placement::Reservations` with one
SQLite ledger (`0009_capacity.sql`); builders, placement, uploads, database starts,
restores, wakes and reconciler starts use the same ownership records. The main
reports every 30 seconds and on requested refresh, independently of optional
watching; workers report on their heartbeat. Samples older than 90 seconds,
unknown memory/Docker state and stale configured host-manager grants defer new
work. Pinned nodes, Compose and aggregate replicas have the same checks.

Admission checks measured `MemAvailable` and a configured workload ceiling
separately. Policy 1 keeps 384 MiB for OS/platform below 2500 MiB visible RAM,
1024 MiB otherwise, plus 128 MiB safety; swap is not capacity. Existing workload
caps/estimates include their unused growth above observed working memory. An
in-flight promise receives credit for its matching visible containers, so observed
usage and future cap growth are not charged twice. Old and new instances both
count throughout a rollout. Idle BuildKit counts observed use; an active build
promises its full configured budget. Temporary disk includes a 512 MiB recovery
allowance, and one heavy operation per node uses the CPU/I/O slot.

Reservations and placement identities survive restart. A completion or cancellation
that may have started physical work waits for a newer inventory frame before
release; interrupted heavy processes additionally reconcile against live inventory.
No timer simply forgets a live allocation. Rejected work leaves serving services
alone and explains the remaining budget. Work is queued or refused before its
expensive stage; a capacity decision cannot predict arbitrary future workload peaks.

New app deployments default to a real 256 MiB/one-CPU limit (192 MiB on the
experimental 1 GiB profile). Compose divides the aggregate budget over its services,
with one container per service, matching deployment/resource limits and a selected
bounded BuildKit builder. New PostgreSQL databases use 256 MiB, one CPU,
`max_connections=20`, `shared_buffers=32MB`, `work_mem=2MB` and
`maintenance_work_mem=32MB`; other new database templates use 512 MiB/one CPU.
Legacy databases retain their old spec hash and unlimited configuration until an
explicit change. `DatabasePatch.memory_mb` changes a cap through admission; the
engine never raises it automatically. Existing unlimited apps remain identified as
estimates until a deployment explicitly recreates them. App/DB limits do not add
swap allowance. A stopped dedicated builder can have its limits corrected without
removing its retained cache; a running builder is left alone until its idle stop.

Inside a Vercelify environment (§19), guest readings are bounded by the accepted
host grant and fresh physical available memory/provider allowance, rather than
treating the VM's RAM as the physical computer's spare RAM. Native Linux also
honors the installed workload slice. No operation silently changes a host grant.
`GET /v1/platform/capacity` and Health's memory card separate measured working use,
cap commitments/growth, additional reservations, system availability and policy
headroom. Missing observations stay unknown; explicit Docker/cgroup OOM evidence
and timestamps are shown, without interpreting exit 137 as an OOM.

Image uploads reserve archive copies before receiving bytes, stream-validate plain,
gzip and bounded-window zstd layers against their uncompressed rootfs identities,
then atomically resize the same promise before Docker load. The declared disk peak
includes compressed content storage plus unpacked layers. Unpacked input is bounded
to 16 GiB; a failed resize retains its previous promise and cannot publish the image.
Restores require a successful database backup's recorded size, reserve staging and
runtime overhead, and verify the staged byte count before changing live data.

Application variables cannot override server-tool authority: PATH/HOME, loader/shell
control keys and DOCKER_/BUILDX_/BUILDKIT_/COMPOSE_/GIT_ names are reserved. The API,
legacy deployment resolution, local pack helper and preview Compose dotenv inputs
validate this boundary without logging values. Ordinary multiline secrets remain
supported. Serving production containers are not recreated solely for this policy.

Replicas (`runtime.replicas`, 1–32): spread over usable servers, the app's own
first, round robin; apps with volumes keep every replica on their server. Each replica
is a container (`vfy-<app>-<suffix>`, then `…-r1`, `…-r2`); `instances` records each
one's server (`deployments.container`/`node_id` keep the first, for what reads only
those; older deployments read as one instance on the main server). A deploy starts the
instances one by one (any failure removes the ones started), then switches routes:
the other servers first, so the main server never forwards to a server without the
route, then its own. The old instances drain, then go; servers that served them and
not the new ones drop the route.

Failover: when a server is offline, stateless apps with `runtime.failover = true`
(no volumes, not compose) are redeployed (a `recover` deployment of the same image,
at most every 5 minutes) on another server; the others wait for it (their containers
keep running if it is only cut off).

The pure parts are unit tested (`placement::place`, `fleet::routes`,
`reconcile::plan` per server); `host/tests/contract/multinode.sh` runs a worker with
its own Docker daemon in a network namespace on the mesh: registration, an app placed
on it through the registry and routed through both proxies, logs, restart and a
terminal from the main server, replicas on both servers, automatic rollback after a
replica dies there, and revocation.

### 8.4 Warm standby

`sudo vercelify standby <worker>` (`PUT /v1/platform/settings/standby`, the cockpit's
servers card) chooses it. Every 5 minutes (and right after choosing) the main server
builds a snapshot: a consistent copy of `platform.db` (SQLite's backup API), the master
key, the coordinator's signing key and WireGuard key, `tokens.json`, the host token,
and when present `agent.toml`, `cloudflare.env` and the tunnel's `cloudflared.env`. It
seals it to the standby's WireGuard key with its own (static X25519 → HKDF →
ChaCha20-Poly1305, as enrollment): only the standby can open it, and the standby
checks it came from the coordinator, then keeps it sealed
(`<state>/standby/snapshot.sealed`, 0600; `POST /v1/runtime/standby`). The status
(last copy, size, error) is in `settings.standby`.

`sudo vercelify promote` on the standby, once the main server is gone for good:
stops its agent and mesh daemon; `vercelify-agent promote` opens the copy (refused
when it is over an hour old without `--force`, or while the main server still
answers at its mesh address), restores the database and keys (the old files are kept
as `*.before-promote`) and fixes the database: this server's worker identity goes
(what ran on it is the main server's now: it is the main server), the coordinator's
endpoints are re-learned and the **map epoch** moves on. The script switches
`role = main`, `mesh_coordinator = true`, takes over the old main server's public
mesh URL, ACME e-mail and tunnel, and starts the agent (which writes the coordinator's
own mesh state) and the daemon. The new main server *is* the coordinator: 10.77.0.1,
its WireGuard key, its signing key. Nodes need no change: the new coordinator's
daemon handshakes from its own address, they roam to it (§13.6), their map streams
reconnect over the mesh and they take the new epoch's maps (a newer epoch replaces
any version). Apps that ran on the old main server come back on the new one through
the reconciler (`recover` deployments); apps on the standby keep running; paired
devices keep their tokens. Router port forwards and DNS that named the old main
server are the owner's to change (the script says so).

An old main server that comes back would fight over the coordinator's key. On start
and every 2 minutes, a main server with a standby asks it (its LAN address from the
mesh endpoints, the control port: `GET /v1/mesh/hello`) for its coordinator key and
epoch; the same key at a newer epoch means it was replaced. It writes
`mesh/superseded.json`, stops its mesh daemon and exits; started again, it opens
neither the coordinator nor the platform (the API says why), and `sudo vercelify
demote` clears its platform state and mesh identity (kept aside) so it can join the
new main server as a worker. `host/tests/contract/promote.sh` runs all of it: two
servers with Docker and a third mesh node, the copy, a refused promote while the main
server answers, the promotion, the third node following, both apps serving on the new
main server, and the old one stepping down.

## 9. Proxy and routing

- `vercelify-proxy`: Traefik (pinned `proxy::TRAEFIK_IMAGE`; `proxy_image` in agent.toml or
  `VERCELIFY_PROXY_IMAGE` overrides it, which the routing contract uses to try an older
  Traefik as an early warning; contract-tested: `host/tests/contract/routing.sh`), host
  network ports 80/443, static config `/etc/vercelify/proxy/traefik.yml`: entry points
  `web`/`websecure`, file provider on `/var/lib/vercelify/proxy/dynamic` (watch), Docker
  provider **off** (we never route by labels, so a container cannot claim a hostname), ACME
  resolvers `le` (HTTP-01) and `le-dns` (DNS-01 Cloudflare, token from `/etc/vercelify/`),
  Prometheus metrics on `127.0.0.1:8082`, access log off, API on 127.0.0.1 only.
- Files per resource, all generated by the platform:
  - `base-<app>.yaml` / `base-<app>-pr-<n>.yaml`: routers for each domain
    (`Host(...)`, priority 100, TLS when `https`), service with the container's address on
    `vfy-proxy`, Basic Auth middleware when set, HTTPS redirect middleware.
  - `vercelify-routes-<key>.yaml`: the existing `vercel.json` layer from `routing.rs`,
    priorities 5000–50000 (unchanged), copying our base routers. Redirects keep their array
    order across `has`/`missing`: a conditional redirect's router runs only the unconditional
    ones listed before it. A rewrite to another site by name is pinned: looked up each
    reconcile (every minute), refused if any address is private, and served from those
    addresses with the name as `Host` and TLS server name (`serversTransports`), so Traefik
    never resolves it itself (no DNS rebinding).
  - `vercelify-wake.yaml`: priority 1 catch-all to the wake listener (unchanged).
- Router naming keeps today's `http-0-<app>` / `…-pr-<n>` pattern so `sleep.rs` reads the
  same Prometheus series.

### 9.1 Preview ingress (M1)

Preview access defaults to private. Private routes use
`pr-<number>-<app>.<node>.vfy`, plain HTTP inside the mesh, with a connection-source
allowlist for the recorded mesh ranges. No mesh configuration means no private
route; do not fall back to a public wildcard. Explicitly public previews retain
wildcard routing. Apply the source restriction before other middleware on base
routes, higher-priority project rules and wake routes. Forwarded request headers
must not grant membership. This ingress policy does not establish container egress
isolation by itself; the independent runtime boundary is specified in §5.1.
The real-Docker preview contract checks direct IPv4, forged `Host` and forwarded
headers against the running proxy; an external IPv6 client, the tunnel path and
WebSocket upgrades are still to be qualified (`docs/validation/2026-11-02-m1`).

## 10. Databases

Templates in `crates/vercelify-platform/templates/databases.toml` (versioned with the code):

| Kind | Image (pinned major) | Port | Data path | Health |
| --- | --- | --- | --- | --- |
| postgresql | `postgres:17-alpine` | 5432 | `/var/lib/postgresql/data` | `pg_isready -U $USER` |
| mysql | `mysql:8.4` | 3306 | `/var/lib/mysql` | `mysqladmin ping` |
| mariadb | `mariadb:11.4` | 3306 | `/var/lib/mysql` | `healthcheck.sh --connect` |
| mongodb | `mongo:8` | 27017 | `/data/db` | `mongosh --eval 'db.runCommand({ping:1})'` |
| redis | `valkey/valkey:8-alpine` | 6379 | `/data` | `valkey-cli ping` |
| keydb | `eqalpha/keydb:latest` pinned by digest | 6379 | `/data` | `keydb-cli ping` |
| dragonfly | `docker.dragonflydb.io/dragonflydb/dragonfly:v1` | 6379 | `/data` | `redis-cli ping` |
| clickhouse | `clickhouse/clickhouse-server:24.8` | 8123/9000 | `/var/lib/clickhouse` | `wget -qO- localhost:8123/ping` |

- Credentials generated (32 chars base62), sealed in `databases.credentials`.
- Internal URL `postgres://u:p@<name>:5432/<db>` on the project network. Apps reference it
  with a variable value of `${{db.<name>.url}}`, resolved at deploy time (so a password
  rotation needs only a redeploy).
- Public port: `-p <mesh IP>:<port>:<internal>` by default; `public_bind = all` binds all
  interfaces and the firewall opens it.
- Backups: `docker exec` dump (`pg_dump -Fc`, `mysqldump --single-transaction`,
  `mongodump --archive`, `BGSAVE` + copy of the RDB) streamed into `restic backup --stdin`,
  tagged `db:<name>`. Restore first stages the complete backup in a private
  temporary directory under the build work directory and validates file archives;
  missing/corrupt snapshots fail before touching live data. Staging uses disk
  space proportional to the backup and is removed afterward. Database runtime
  operations serialize per database so reconciliation cannot restart one mid-copy.
  Restore takes a fresh backup of current data after staging succeeds, under the
  same database-operation lock; a backup failure aborts replacement. Restore stops
  dependants, restores, starts them. The cockpit requires the database name and
  shows the snapshot date before requesting restoration.
- Storage identities: schema 5 gives new app/database volumes an immutable
  `vfyv2-<volume ID>` name, with `-pr-<number>` for previews. Existing volumes keep
  their legacy names and HDD paths; the migration does not move data. Before
  mounting, moving or purging, legacy bindings are checked against other apps,
  databases and known previews. Aliases return a conflict and preserve the data.
  Runtime starts carrying new identities use `/v1/runtime/instances-v2`; older
  workers fail safely and need upgrading before mounting those volumes.
- Volume tier moves (`POST /v1/platform/volumes/{id}/move`, a job): stop, `cp -a` to the
  other tier (owners and modes kept), record the new place, start. Every production
  replica/sidecar (including rollout leftovers) is stopped; the move serializes
  with app reconciliation/restart and excludes concurrent deployment of that app. A database restarts on
  the new path; an app volume is mounted by a fresh container, created by a `recover`
  deployment from the same image. The app is down while the data is copied. A missing
  volume source can be initialised empty, but an inaccessible or non-directory source
  aborts the move and restarts the original container without switching the tier.
- App volumes are handed to the image's user on every deploy (`chown` of the volume's top
  directory, run in the image itself so `node` or `1000:1000` resolve): our images run as
  non-root and could not write to a new, root-owned volume otherwise.
- Database console: `POST /v1/platform/databases/{id}/terminal` opens the database's own
  shell (`psql`, `mysql`, `mongosh`, `valkey-cli`, `clickhouse-client`) logged in from the
  container's environment, so the password never crosses the API.

### 9.9 Private-preview guest invitations (competitive roadmap M7b)

Schema 13 stores reserved guest namespaces, hash-only grants and sessions, a persisted
clock high-water mark and a sealed key-rotation probe. Only Owners configure a dedicated
HTTPS guest domain or issue/revoke invitations; this first release supports main-node
private previews. DNS/TLS provisioning stays with the operator. Configuration is not
proof of public reachability. Guest domains remain reserved after revocation.

A unique host carries each invitation. Its 256-bit secret is shown once in a URL
fragment on `/__vfy/invite`; the gateway clears that fragment before explicit redemption.
Scanner GETs never consume invitations or reach the app. Host-only Secure/HttpOnly
SameSite=Lax cookies expire within 15 minutes and the grant lifetime. One-session
invitations are the default; reusable ones allow eight live sessions. There are at
most 100 active grants, 128 live sessions and 32 proxied connections.

The existing wake listener checks authorization before assets, routing or waking.
The reserved public router has priority 60000. Protected backend aliases, including
advanced application rules, require both a random service Basic Auth capability and
live ForwardAuth with a separate bridge secret. Only a bcrypt verifier is written to
the proxy file. After these checks Traefik restores the original application Authorization
and strips all bridge/session headers; the gateway strips guest cookies and forwarded
identity before proxying. Existing app Basic Auth still applies. Worker nodes receive
no guest aliases. Host processes and loopback remain trusted.

Every request validates host, private/active PR and current deployment. Revocation,
PR close, generation replacement, privacy/enablement changes and manual app stop
permanently retire access. Restart preserves sessions; clock rollback fails closed;
master-key replacement revokes them. Streaming HTTP and WebSockets have independent
one-second authorization supervisors (750 ms inspection timeout) and a monotonic
session deadline. HTTP uploads cap at 16 MiB; transfers cap at 64 MiB. Guest responses
force no-store, no-referrer and noindex; workers and frames are blocked to protect
future invitation navigations. Downloaded content cannot be revoked.

Owner issuance waits for routing readiness and revokes an unreturned invitation on
timeout. A stopped listener, absent alias, failed inspection or unavailable middleware
denies access. No extra daemon/container or cloud relay is introduced. Qualification
and limitations: `docs/validation/2026-11-04-m7b/VALIDATION.md`.

### 10.0 Isolated restore drills (competitive roadmap M7a)

Schema 12 journals `restore_drills`, exact owned temporary services and schedules;
new successful PostgreSQL 17 backups optionally carry a version-1 manifest of
source identity, custom dump filename/bytes, start time, database-size estimate,
server major, extension versions and relation/index catalog. Legacy or unsupported
backups remain unverified. Concurrent DDL may cause a conservative catalog mismatch;
no live row count captured after backup becomes its expected value.

One serial runner handles on-demand jobs, restart cleanup and disabled-by-default
monthly UTC schedules. Missed schedules coalesce into one catch-up job. Missing
supported backups preserve overdue status. Doctor/the existing watcher warns about
failed latest drills, overdue schedules and deferral/cleanup beyond five minutes.
It deduplicates unchanged status; it never treats scheduling health as restore proof.

A drill restores into its own PostgreSQL 17 service, random sealed credentials and
volume, with no host port, source credentials or app environment. Its internal
Docker 28+ bridge uses isolated gateway mode and a distinct restore-only label;
preview/proxy recovery never connects it. Generic patch/delete/start/backup/restore/
move, terminal and connection-variable paths cannot repurpose the destination.
Dump content itself may include sensitive data and executable database definitions;
it remains quarantined. Production-data preview copying stays disabled.

A separate pinned official restic helper reads an absolute local repository mounted
read-only or a supported S3/REST/B2/Azure backend. `--no-lock --no-cache` avoids source
repository writes. Metadata integrity and optional full `--read-data` verification
are recorded separately; concurrent repository pruning can safely make a check
incomplete. Selected dump data is always decrypted, byte-bounded and streamed into
`pg_restore --single-transaction --exit-on-error --no-owner --no-acl`; plaintext dump
files are never staged. Helper and destination use Docker log driver `none` so a
streamed dump cannot enter Docker logs. Remote backends remain unqualified in the lab.

M4 admission reserves 384 MiB for destination/overhead plus 512 MiB for the reader,
with estimated disk headroom of max(3× dump bytes, 2× recorded database bytes) +
1024 MiB. Destination is capped at 256 MiB/1 CPU and the helper at 512 MiB/1 CPU,
with equal memory/swap ceilings. Power/thermal/low-disk checks stop long work and
clean before deferral. Mid-work retries cap at three; operators may cancel pending
work. These are bounded trusted-backup operations, not a hostile-dump sandbox or
a hard storage quota against arbitrary executable database definitions.

Verification compares restored catalog, server major and extension versions and
runs a read smoke test. Optional known-result assertions execute under a fresh
non-superuser read role, in read-only transactions with a five-second statement
limit and bounded output. Stacked statements and psql metacommands are rejected.
Neither expected values, observed rows nor repository credentials appear in public
job metadata. Assertion specifications and supplied recovery settings are sealed
with per-job AAD; terminal outcomes clear their sealed copies.

`verified_at` is recorded only after checks; success additionally requires exact
container/volume/network cleanup. Restart discards an interrupted unverified
restore and resumes its journaled cleanup. Source deletion cannot erase ownership
or the immutable job manifest. Observed snapshot age and elapsed check/restore time
are measurements, never RPO/RTO guarantees. Host-key checks prove that selected
host credential works. Imported manifests and explicitly supplied recovery settings
also work with a new control store/master key; the copied-repository/recovery-artifact
lab fixture is logical recovery evidence on one VM, not physical machine-loss proof.

Capability `postgres-restore-drills` gates cockpit controls. Metadata GETs are readable
by paired viewers. Create/import/cancel and schedule GET/PUT require Owner in both
middleware and handlers; schedule GET contains private assertion specifications.
Routes are `/v1/platform/databases/{db}/restore-drills`,
`/v1/platform/databases/{db}/restore-drill-schedule`,
`/v1/platform/restore-drills/{id}` and `/{id}/cancel`; independent recovery import is
`POST /v1/platform/restore-drills/import`. Queued/active jobs cap globally at twenty.

### 10.1 Service catalogue

`vercelify-platform::templates` holds the one-click services: Umami, Plausible,
MinIO, Unleash, Uptime Kuma, Vaultwarden (more by adding an entry). A template is a compose
file with images pinned to a version (a test refuses `latest`), the routed service, its
port and health path, and the secrets it needs. `POST /v1/platform/templates/{id}` creates
a compose app without a repository (`source.compose_content`), generates each secret
(base62) as a secret variable of the app, and deploys it. Compose interpolation fills
`${VAR}` from the app's variables plus `VERCELIFY_URL`. Each template lists the memory it
needs, so the cockpit can say "about 2 GB" for Plausible before it is created.

## 11. Git, previews, hooks

- `autodeploy.rs` keeps polling with `git ls-remote` (works behind CGNAT). Its targets
  become our apps; deploy = `deployer.enqueue(app, sha, trigger = push)`.
- Previews: the existing logic (forks never built, per-app cap, GitHub status + comment)
  calls `deployer.enqueue_preview(app, pr, sha)`; closing a PR deletes the preview's
  container, route and row. The row is closed first, under the lock a preview's commit
  point takes (check open → route → record), so a deployment of it under way can't go
  live afterwards; its running deployments are cancelled and the teardown waits for them.
  A failed teardown opens the row again for the next check to retry.
- M5 opt-in fresh PostgreSQL previews (`PreviewSettings.database`): one bounded,
  separate PostgreSQL 17 service per app on main, max three active PRs, no Compose.
  Each PR gets a logical database, a restricted LOGIN (four connections) and a
  hidden NOLOGIN database owner. The login owns its schema objects but cannot
  grant another PR database CONNECT. PUBLIC access to preview and maintenance
  databases is revoked; database creation starts at CONNECTION LIMIT 0 before ACL
  installation. Dedicated internal base and M1 PR bridges are its only networks;
  production/proxy networks and host ports are refused. This is trusted-PR isolation,
  not a hostile-tenant resource sandbox. The default service cap is 256 MiB/one CPU;
  M4 reserves service, deployment overlap and temporary migration work together.
- `0010_preview_db.sql` journals exact app/preview/service/database/role ownership
  before external work. Sealed random credentials use resource-specific AAD;
  metadata survives app deletion. Ownership comments fence SQL adoption/deletion.
  A new SHA retains the open PR database and reruns migration/optional synthetic
  seed in the built image, capped and on the PR bridge, before route publication.
  The configured URL overrides only its explicit runtime binding, never build
  variables; all other production DB reference denial remains. SQL is supplied on
  stdin and both credential and URL are redacted. Off-host job revision hashes
  include preview database policy, fencing uploads made against old commands.
- PostgreSQL close is durable: mark closed/closing, cancel work, remove the exact
  PR runtime/helpers, revoke LOGIN, terminate its sessions and drop its database
  and roles. Reconcile retries failures after restart or app deletion; reopening
  waits for old cleanup and rotates identity. Last close journals service retirement
  before removing its owned container, volume, networks and SQL metadata. No
  production data copy or backup seed. App sleep retains preview DBs; unexpected
  disposable volume loss requires redeploying migrations/seed. The authenticated
  preview-databases GET exposes metadata only, with the matching health capability.
- Webhooks (M9): `POST /v1/webhooks/{github|gitlab|gitea}[/{source}]` on the control
  listener, reachable through the tunnel's public URL (`mesh_public_url`) or on the LAN
  (a self-hosted Gitea). `POST /v1/platform/git-sources/{id}/webhook` (`vercelify
  webhook SOURCE`, the app's "Deploy on push" card) makes a secret for a git source
  (sealed, `git_sources.webhook_secret`, shown once) and says where to send events. A
  request is accepted only when signed with a source's secret (GitHub: `X-Hub-Signature-256`
  HMAC-SHA256 of the body; Gitea/Forgejo: `X-Gitea-Signature`; GitLab: `X-Gitlab-Token`,
  compared in constant time), at most 60 a minute per source. `push` and `pull_request`
  events only make the poller check the apps of that repository now (the source's apps
  and apps without a source; URLs matched across https/ssh forms): the payload's commit
  is never trusted, `git ls-remote` decides.
- GitHub App (M9), for organizations that won't hand out personal tokens: `POST
  /v1/platform/github-app` (`vercelify github-app [--org ORG]`, the cockpit's New app
  form) returns a start page on the control listener with a one-time state (30 min).
  The page posts GitHub's manifest (permissions: contents and metadata read, statuses
  and pull requests write; events: push, pull_request; webhook to the public URL, or
  inactive without one; back to `/v1/github-app/callback`). The callback (or the address
  pasted into `…/github-app/convert`) trades the code for the app's id, private key and
  webhook secret, stored sealed as a `github_app` git source. Fetches, `ls-remote` and
  GitHub statuses/comments then use installation tokens: a JWT (RS256, 9 minutes) signed
  by the `openssl` command (the agent carries no RSA code), `GET
  /repos/{owner}/{repo}/installation`, `POST /app/installations/{id}/access_tokens`, kept
  50 of their 60 minutes. The engine asks the agent for them through a token minter, so
  the platform crate stays free of HTTP to GitHub.
- Deploy hooks: `POST /v1/hooks/deploy/{secret}` on the control listener; 32-byte secret,
  SHA-256 stored, rate limit 6/min per hook.

## 12. API

Conventions: JSON, `snake_case`, lists return arrays, errors `{ "error": "…" }` (as today),
`If-Match: <updated_at>` optional on PATCH for optimistic concurrency (409 on mismatch),
long operations return a `Job` and progress on `/v1/events` (topics `apps`, `deployments`,
`databases`, `mesh`). New capabilities in `/v1/health`: `platform`, `mesh`.

Roles: **viewer** = GET; **deployer** = apps, deployments, env (at app, environment and
project level; creating or deleting projects and environments is an owner's), domains,
previews, databases (not delete), logs, terminal; **owner** = everything incl. tokens, mesh, nodes,
settings, deletes. The host token (`agent.token`) is owner. `GET …/apps/{id}/env?resolved=true` lists what production
deployments of the app get (project < environment < app, no preview overrides, database
references unexpanded; `vfy env pull`), each with the scope it comes from; `reveal=true`
needs a deployer.

The platform routes live under `/v1/platform/` (`/v1/projects` is already the
`vercel.json` rules API): `/v1/platform/projects`, `/v1/platform/apps`,
`/v1/platform/apps/{id}/env`, `…/domains`, `…/volumes`, `…/deployments`,
`/v1/platform/deployments/{id}`, `/v1/platform/nodes`. Apps are addressed by id or
name. Full list: §4.14 of the plan. Request/response types live in
`vercelify-core::platform` and are exported to TypeScript. `App.last_status` carries
the newest production deployment's status, so lists show build state without a
request per app.

### 12.1 Runtime: logs, restart, terminals

- `GET /v1/platform/apps/{id}/logs?tail=N[&follow=true]`: the live container's output
  (`docker logs --timestamps`) as JSON lines `{at, stream, text}`; following keeps the
  response open, and the `docker logs` process ends with it. Lines from another server's
  agent are redacted on the main server, which alone knows the app's secrets.
- Output handling (deployment and runtime logs alike, `proc::read_line`,
  `logs::strip_ansi`): output is read as bytes, invalid UTF-8 is replaced (it never ends
  the stream: a child still writing to an unread pipe would block until the build
  timeout), and a line over 64 KiB arrives in pieces. A `\r` returns to the start of the
  line as on a terminal, so only a progress bar's last state is kept; CSI/OSC/charset
  escapes and other control characters except tab are removed. A message of several
  lines becomes several log entries, so the file's `<time> <stream> <text>` format and
  the sequence numbers followers rely on stay intact.
- `POST /v1/platform/apps/{id}/restart`: every container of the live deployment (compose
  sidecars too).
- Terminals: not a WebSocket. `POST …/apps/{id}/terminal` (or `…/databases/{id}/terminal`)
  starts `docker exec -it` on a pseudo-terminal the agent owns (`openpty`, the child leads
  its own session) and answers `{id}`. `GET /v1/platform/terminals/{id}/output` streams
  `{"data": base64}` lines (the last 64 KiB first, for a reader that reattaches) and ends
  with `{"exit": code}`; `POST …/input {data: base64}` and `POST …/resize {cols, rows}`
  write to it; `DELETE` ends it. This reuses the authenticated request and stream path the
  Tauri app, the browser and `vfy` already have, and adds no WebSocket stack to the agent.
  A session belongs to the token that opened it (others get 404), at most 8 run at once,
  and one without a reader or input for 15 minutes is closed. Terminals are a deployer's.
  The cockpit loads xterm.js only when a terminal opens, and batches keystrokes for 12 ms
  into one ordered request.

The cockpit tells a main server by its pairing (`role: "main"`) and its health (the
`platform` capability). There the Apps page, ⌘K and the sidebar use `/v1/platform`;
everything else is shared.

## 13. Mesh

### 13.1 Addressing and keys

- IPv4 `10.77.0.0/16`, IPv6 `fd76:6679:6d65::/48`. `ip_index` n ∈ [1, 65534] maps to
  `10.77.(n>>8).(n&255)` and `fd76:6679:6d65::n`; index 1 is the coordinator. Configurable
  prefix (`mesh.ipv4_prefix`) for LANs that already use `10.77/16`.
- **Node key**: X25519, the WireGuard static key, generated on the node, private part never
  leaves it (`/var/lib/vercelify/mesh/node.key`, 0600; on the Mac in the app's data dir).
- **Coordinator key**: Ed25519 (`/var/lib/vercelify/mesh/coordinator.key`), signs node
  certificates and peer maps.
- **No TLS anywhere.** The coordinator also has an X25519 key: its own node key (the
  coordinator is mesh node 1). Join tokens and pairing codes carry that public key, so a
  joining node seals its enrollment to it (§13.2); everything after enrollment runs inside
  the mesh. No certificate to pin, rotate or explain.

### 13.2 Enrollment

The control listener (`mesh_control_listen`, default `0.0.0.0:7443`, plain HTTP) has only
two routes: `GET /v1/mesh/hello` (the coordinator's public key and mesh address, for
diagnostics) and `POST /v1/mesh/enroll`. Everything else a node does goes to the agent at
the coordinator's mesh address once its tunnel is up.

```
joining node                                     coordinator (:7443)
   │ join token vfyj1.<b64url {v, control[], coordinator_wg, claim, kind}>
   │ POST /v1/mesh/enroll  Sealed{v:1, public_key, nonce, data}
   │   data = seal_req({ claim, name, kind, os, arch, endpoints[] })
   │──────────────────────────────────────────────▶│ open with its node key; claim:
   │                                               │ single use, TTL (1 min–7 days),
   │                                               │ 10 tries/min per source address
   │                                               │ allocate ip_index, sign certificate,
   │                                               │ mint node token (vfyn_…)
   │◀──────────────────────────────────────────────│ Sealed: seal_resp({ node_id, ipv4,
   │                                               │   ipv6, certificate, node_token,
   │                                               │   coordinator_key, coordinator, api_url })
```

Sealing (`vercelify-mesh::seal`): `X25519(node, coordinator_wg)` → HKDF-SHA256 (salt
`vercelify-enroll-v1`, info = node public key ‖ coordinator public key) → 64 bytes, one
ChaCha20-Poly1305 key per direction (a request can't be replayed as an answer), random
nonces, low-order keys refused. Someone on the path can't read the claim, can't swap the
key being enrolled (the request is bound to it), and can't read the node token. The answer
is only accepted if it opens under the coordinator key the token named.

A node token (`vfyn_…`, stored hashed) is accepted only from the node's own mesh address
(IPv4 or IPv6), or from loopback for the coordinator's own daemon: a stolen token is useless
outside the tunnel that proves the node's key. Node routes on the agent (`:7420`, over the
mesh): `GET /v1/mesh/map`, `POST /v1/mesh/endpoints`, `POST /v1/mesh/renew`.

Certificate: `vfyc1.<base64url(payload JSON)>.<base64url(ed25519 sig)>`, payload
`{v:1, node_id, name, kind, role, public_key, ipv4, ipv6, not_before, not_after, epoch}`;
lifetime 30 days, renewed after day 20 (§13.9). Checked with 5 min clock skew.

### 13.3 Pairing code v2

`vfy2.<base64url JSON>`: the `vfy1.` fields plus `mesh_join` (a single-use device join
token, `vfyj1.…`, at least a day valid); `agent_url` is the server's mesh address
(`http://10.77.0.1:7420`). `sudo vercelify pair` prints one on a mesh coordinator; the
app joins with `mesh_join`, then talks to `agent_url` over the mesh. A `vfy1.` code
(`pair --host`) points at an address the Mac reaches directly, without the mesh.

The 8-symbol short code works without typing an address:

- **Discovery.** The coordinator's agent answers multicast DNS for `_vercelify._tcp.local`
  (`agent/src/mdns.rs`, on 5353 next to Avahi with `SO_REUSEPORT`): PTR, SRV (the control
  port), TXT (`name`, `wg` = the coordinator's key, `pair=1` while a code waits) and A
  records for its LAN addresses. `vercelify-mesh discover` (and the app through it) asks
  with a legacy-unicast query and lists what answered. Nothing in an answer is trusted.
- **Claim.** `POST /v1/pair/pake` on the control listener runs SPAKE2 (Ed25519 group,
  identities `vercelify-app`/`vercelify-server`) with the code as the password; the server
  seals the full pairing code with ChaCha20-Poly1305 under `HKDF(key, "vercelify-pair-v1")`.
  The code never travels and an answer can't be tested offline: someone answering mDNS in
  the server's place, or listening, gets one online guess per attempt. The server can't
  tell a right code from a wrong one, so each request spends one of the code's 5 attempts
  and a second; `vercelify-mesh pair CODE [--server ADDR] [--join]` is the client.

### 13.4 Peer map

`GET /v1/mesh/map` on the coordinator's agent over the mesh, `Authorization: Bearer vfyn_…`,
NDJSON stream (a `ping` every 25 s; the node reconnects with backoff when it goes quiet):

```json
{"type":"map","signed":"vfym1.<base64url payload>.<base64url ed25519 signature>"}
{"type":"ping"}
{"type":"revoked"}
```

The payload (`PeerMap`) is

```json
{"version":42,"epoch":1,"self":{...},"peers":[{"node_id":"mn_…","name":"pi5",
 "kind":"server","public_key":"<b64>","ipv4":"10.77.0.3","ipv6":"fd76:6679:6d65::3",
 "endpoints":["192.168.1.31:51820","81.2.3.4:51820"],"allowed_ports":null}],
 "relays":["wss://mesh.example.com/v1/mesh/relay","wss://192.168.1.20:7443/v1/mesh/relay"],
 "dns":{"zone":"vfy","records":{"pi5":"10.77.0.3"}}}
```

Certificates (`vfyc1.`) and maps (`vfym1.`) use the same envelope: the signature covers
`<prefix>.<payload>`, so one can't be passed off as the other, and verification never
depends on re-serialising JSON (`vercelify-mesh::cert`).

- Peers visible to a node: servers see all servers and all devices; devices see servers only.
- `allowed_ports`: for a server's view of a device, the ports the device may reach (enforced
  by the server's firewall, §13.8).
- The map is persisted by the node (in `state.json`, 0600) and used at start before the
  stream connects, so a reboot brings the mesh back while the coordinator is away.
- `epoch` increases when a standby is promoted; nodes ignore maps with a lower epoch.
- Endpoint reports: `POST /v1/mesh/endpoints { local: [...], stun: [...] }` whenever they
  change; the coordinator adds the source address it observed. `local` never includes
  addresses on another VPN's interface (`tailscale*`, `utun*`, `wg*`, `zt*`, `tun*`): a
  WireGuard path nested in another overlay stalls whenever that overlay changes its own
  path. Join tokens still list those addresses last, since they may be the only way in.

### 13.5 Data plane (`vercelify-mesh`)

- **One UDP socket** (`:51820` on servers, random on devices) carries WireGuard, disco and
  STUN, demultiplexed by the first bytes: WireGuard message types 1–4 followed by three zero
  bytes; STUN has magic cookie `0x2112A442` at offset 4; disco starts with `VFYD`.
- WireGuard: `boringtun::noise::Tunn` per peer, driven by our own event loop (tokio): timers
  every 250 ms, keepalive 25 s.
- Linux: tun device `vfy0` (`/dev/net/tun`, `IFF_TUN|IFF_NO_PI`), MTU 1280, addresses and
  route `10.77.0.0/16` set with rtnetlink-free `ioctl`s (`SIOCSIFADDR`, `SIOCSIFNETMASK`,
  `SIOCSIFMTU`, `SIOCSIFFLAGS`) and IPv6 via `/proc/net/if_inet6`-compatible `ioctl(SIOCSIFADDR)` on an
  `AF_INET6` socket.
- App mode (`up --userspace`, the Mac without root; works on Linux too, which is how it is
  tested): no tun; a `smoltcp` stack (`netstack.rs`) is the engine's device.
  It advertises an internal MTU of 1276 so smoltcp 0.12 emits IPv4 fragment
  payloads aligned to eight bytes; the WireGuard interface remains at 1280.
  The egress test echoes a 1316-byte UDP payload's actual fragments and verifies
  complete reassembly. SOCKS5
  (`--socks 127.0.0.1:7480`, CONNECT only, `*.vfy` and bare node names resolved from the
  map, anything off the mesh refused) and forwards (`[udp:]127.0.0.1:PORT=HOST:PORT`,
  loopback only): fixed ones from `--forward`, and the ones the app adds while the daemon
  runs in `<run-dir>/forwards` (one per line, `# owner` comments; checked every second,
  new lines start, removed ones stop, bad lines logged; a forward that fails to bind is
  retried while its line remains). The app uses them for programs
  that can't speak SOCKS when the target is on the mesh: ffmpeg's display stream
  (`udp:127.0.0.1:<port+10000>`, the sender's URL rewritten to it) and Moonlight
  (Sunshine's ports on the same numbers, Moonlight started on `127.0.0.1`). The daemon's
  own requests to mesh addresses (map stream, endpoint reports, renewal) go over the same
  stack. Devices bind a random UDP port (`--port 0`) and report the real one. No DNS
  server (nothing could query it); the server's firewall applies as to any device.
- Browser gateway (`--gateway 127.0.0.1:7481`, app mode):
  `http://<original app host>.localhost:7481/path` reaches the server's port 80
  through the same stack. It removes `.localhost` from Host, resolves embedded
  sslip.io/nip.io IPv4 addresses before mesh-zone and system DNS, and refuses
  destinations outside the mesh prefixes. Plain HTTP connections close after
  each request; WebSocket upgrades bridge bytes in both directions. The app sees
  its own name, so the gateway puts it back on the way out: a `Location` to the
  app's own host (any scheme or port) becomes the gateway URL, and a cookie's
  `Domain=` for that host or a parent of it is dropped (the browser keeps it for
  the `.localhost` name); `Origin`/`Referer` naming the gateway go in with the
  app's name. Response heads are rewritten as they stream (1xx interim heads
  too); bodies and upgraded connections are untouched. The Mac
  installer enables it alongside SOCKS. The cockpit maps private app, preview
  and HTTP domain links; public HTTPS links keep their public route. Browser
  opener errors appear as a toast. Chrome/Firefox resolve `*.localhost` locally;
  Safari remains unverified and needs the M7 Mac field check.
  Ollama remains on its own mesh port (11434), with no `ai.*` proxy route: clients
  use SOCKS or a local TCP forward, rather than an invented gateway hostname.
- macOS system mode: `utun` via the kernel control `com.apple.net.utun_control` (`utun.rs`),
  addresses and routes with `ifconfig`/`route`, `*.vfy` through `/etc/resolver/vfy`; the
  same engine as Linux.
- On the Mac `install-mac.sh` installs `vercelify-mesh` (release asset
  `vercelify-mesh-universal-apple-darwin`, else a source build) with a LaunchAgent
  `dev.vercelify.mesh` in app mode (`--mesh-system`: a LaunchDaemon as root, utun). It
  waits until pairing joins it.

### 13.6 Disco and path selection

Disco messages (`vercelify-mesh::disco`) are sealed with a key only the two nodes share:
`HKDF-SHA256(X25519(my_priv, peer_pub), info "vercelify-disco-v1")` → ChaCha20-Poly1305;
frame `VFYD ‖ sender public key (32) ‖ nonce (12) ‖ sealed body`, the first 36 bytes as
associated data. Bodies: `ping {tx}`, `pong {tx, observed}`, `call_me {endpoints[≤16]}`.

Per peer, in the engine (`wg.rs`):

1. Candidates: the map's endpoints (for the coordinator, the enrollment-time ones only
   while the map has none: they go stale), then what the node learned (the source of a peer's
   pings, `call_me` lists, authenticated WireGuard packets), at most 16. Addresses on the
   network of this node's own VPN interfaces (Tailscale's 100.64.0.0/10, else the same /24)
   are never learned or passed on as observed: a path inside another overlay stalls when
   that overlay re-routes.
2. Every 5 s while no direct path is confirmed, every 12 s once one is: a `ping` to every
   candidate. A `pong` from the address pinged confirms that path with its round trip and
   tells the node where the peer saw it (peers act as STUN servers for each other; the
   coordinator, usually reachable, gives every node its public mapping this way).
3. The best confirmed path wins: lowest round trip, a private (LAN) address within 2 ms
   preferred. A path without a pong or a packet for 40 s (three missed pings) is dropped.
4. While nothing is confirmed and the relay is up, `call_me` goes through the relay at most
   every 15 s with this node's local and observed endpoints; the peer adds them and pings
   at once, so both sides send at the same moment (hole punching for NATs that only let
   replies in).
5. Where packets go: the confirmed path; else an endpoint the peer's authenticated packets
   arrive from (it works both ways); else the relay; else the candidates in turn (4 s each,
   the only mechanism without a relay).

The daemon reports its interface addresses plus what peers and STUN servers observed to the
coordinator (`POST /v1/mesh/endpoints`, at 10 s and 30 s after start, then every minute);
the map passes them on as candidates.

STUN (`vercelify-mesh::stun`, RFC 5389 binding only) on the same socket: requests to
`--stun` (default `stun.cloudflare.com:3478,stun.l.google.com:19302`, `""` for none,
`VERCELIFY_MESH_STUN`) every 60 s, resolved hourly; every node also answers binding
requests (rate-limited with the handshake budget, 200/s).

Measured in `host/tests/contract/mesh-nat.sh` with real Linux NAT: two nodes behind
port-preserving NATs that only let replies in punch a direct path within seconds; a node
behind a symmetric NAT (`MASQUERADE --random-fully`) reaches public nodes directly and the
others through the relay. One Linux behaviour matters: a router that accepts unsolicited
UDP to itself records it in its NAT table, which then takes the port the LAN host needs and
turns a cone NAT symmetric; home routers drop such packets (the test's routers do too).

### 13.7 Relay

Endpoint `GET /v1/mesh/relay` (WebSocket, binary) on the control listener; the map's
`relays` lists `ws://<control address>/v1/mesh/relay` for every control address and, with
`mesh_public_url` set (a Cloudflare tunnel to `127.0.0.1:7443`), `wss://<host>/v1/mesh/relay`.
Join tokens carry that URL too, so a node can enroll through the tunnel (`https://`, the
daemon checks it with the webpki roots).

- **Authentication by key, not token.** The relay sends `{"challenge": <32 random bytes>}`;
  the node answers it sealed to the coordinator's WireGuard key (§13.2's sealing); only the
  holder of the node's private key can, and the key must belong to a node that isn't
  revoked. So a node uses the relay before it has any path into the mesh, and rotation
  reconnects with the new key.
- Frames: client → relay `dst public key (32) ‖ packet`; relay → client
  `src public key (32) ‖ packet`, at most 4 KiB + 32. The relay forwards only between nodes
  the map lets talk (a device to a server and back, servers among themselves), keeps no
  packets beyond 256 queued per destination (then drops), and counts bytes per node
  (`relay_connected`, `relayed_bytes` in `GET /v1/mesh/nodes`).
- A newer connection of the same key replaces the older; revocation closes it at once. So
  the node opens its relay URLs side by side (each 300 ms behind the one before it, in the
  map's order) and answers the challenge on the first to open only: a LAN address that never
  answers from another network doesn't hold up the tunnel, and a new map mid-round restarts
  it.
- Payloads are WireGuard or disco packets, so the relay only sees ciphertext. Pings every
  25 s (client) and 30 s (relay) keep tunnels and proxies from idling it out; a client that
  hears nothing for 40 s takes the connection as dead (it can die without a reset) and
  reconnects. When the relay comes (back) up, the engine handshakes at once with every peer it
  carries, rather than at WireGuard's next retry.

### 13.8 DNS and ACLs

Containers and the mesh (M9): containers stay off the mesh except what the coordinator
lists per server in the maps (`Peer.container_ports`): every server's proxy (80/tcp,
for the main server's forwarding) and, on the main server, the databases' public
ports (which devices may reach too). The daemon writes them into the ACL file
(`container`), and `firewall.sh mesh` turns them into `VERCELIFY-MESH-OUT` (`-o vfy0
-d <server> -p tcp --dport <port> -j ACCEPT`, everything else falls through to the
container block). A database's public port changing bumps the map version.


- Linux DNS listens on `<mesh ipv4>:53`; native macOS uses an ephemeral loopback
  UDP listener recorded in `/etc/resolver/vfy`. Zone `vfy` returns `A`/`AAAA` for
  `<node>.vfy`, and `A` only for `*.<node>.vfy`; everything else is `REFUSED`. Linux
  hosts with `systemd-resolved` get `resolvectl dns vfy0 10.77.0.x; resolvectl domain vfy0 ~vfy`.
- Firewall: the daemon writes `/run/vercelify/mesh-acl.json` (`{servers: [ip], devices:
  [{ip, ports}]}`) whenever the map changes and runs its ACL hook, `firewall.sh mesh`, which
  rebuilds two chains for both address families: `VERCELIFY-MESH` (jumped to from INPUT for
  `vfy0`) and `VERCELIFY-MESH-FWD` (from `DOCKER-USER`, matching published ports with
  `conntrack --ctorigdstport`). Servers: accept all. Devices: accept only their
  `allowed_ports` (`DEVICE_PORTS`: 7420, 80, 443, 11434/tcp, 5600–5610/udp, and Sunshine's
  47984, 47989–47990, 48010/tcp and 47998–48000/udp for Moonlight). Anything else
  from the mesh: drop. Plain iptables rules, no ipset: a mesh has tens of devices, not
  thousands. The chains are replaced in one `iptables-restore --noflush` commit, never
  flushed and refilled rule by rule, so a map update has no moment without the final drop. `vfy0` is in the container isolation rules (containers can't reach the
  mesh).

### 13.9 Rotation and revocation

- **Renewal** (hourly check, day 20 of 30, or `vercelify-mesh rotate` → SIGUSR1): the node
  generates `node.key.next`, asks `POST /v1/mesh/renew {public_key}` over its current
  tunnel, and on the answer saves the new certificate, swaps the key in, and rekeys its
  WireGuard state. The coordinator answers first and pushes the changed maps 800 ms later,
  so the answer isn't lost to the tunnel it is about to replace. If the answer is lost
  anyway, the next attempt finds the pending key already registered and treats it as
  done (a rekey to the same key is a no-op). 4 tries, then the next hourly check.
- **Revocation** (`DELETE /mesh/nodes/{id}`, the cockpit or `vercelify mesh revoke`): the
  node's map stream gets `{"type":"revoked"}` first; 1.5 s later the node is deleted and the
  other maps change, so peers drop it within one push. The revoked node clears its peers,
  marks itself revoked in its status and stops retrying.

### 13.10 Operating it

- `agent.toml`: `mesh_coordinator = true` on the main server; `mesh_control_listen`,
  `mesh_port` (51820), `mesh_name`, `mesh_public_endpoints` (addresses behind port
  forwards); `listen = "mesh"` binds the agent to its mesh address.
- `vercelify mesh init | invite [--device] | join TOKEN | nodes | revoke NAME`; on
  workers the installer installs `vercelify-mesh` and `vercelify-mesh.service`.
- `vercelify-mesh join TOKEN [--name] [--port] | up [--stun …] [--userspace --socks ADDR
  --forward …] | status [--json] | rotate | pubkey | discover [--json] | pair CODE [--join]`;
  `up` writes `/run/vercelify/mesh-status.json` every 5 s: per peer the endpoint, the path
  (`lan`, `direct`, `relay`, `none`), the round trip, the last handshake and bytes.
- `mesh_public_url` in `agent.toml` (`VERCELIFY_MESH_PUBLIC_URL`): the control listener
  through a tunnel, for joins and the relay from outside the LAN.
- `host/tests/contract/mesh.sh` runs four nodes in network namespaces (a LAN, a router, a
  network behind it) with the real binaries and checks joining, paths, DNS, the firewall,
  rotation, revocation and restart on the saved map. `host/tests/contract/mesh-nat.sh`
  puts three nodes behind real NAT routers (two that keep ports, one symmetric) and checks
  hole punching, the relay and revocation on it. The first also pairs a device the way the
  Mac app does (mDNS, short code, SPAKE2, join) and runs it in app mode: SOCKS, forwards,
  4 MB through the userspace stack, UDP, and the firewall still applying.
- **The app.** The cockpit drives the installed `vercelify-mesh` (app mode, a
  LaunchAgent): the pairing form lists servers from `discover` and claims one with the
  short code (`pair --join`), or joins with a pasted `vfy2.` code's `mesh_join`, then waits
  for the first handshake. Requests to mesh hosts (`10.77.*`, `fd76:6679:6d65:*`, `*.vfy`)
  go through the daemon's SOCKS proxy (`socks5h://127.0.0.1:7480`); everything else
  directly. The Servers page's network panel reads the daemon's status file for this
  computer's path to each node and, from the coordinator, lists nodes with invite and
  revoke (`/v1/mesh/nodes`, `/v1/mesh/invites`, `DELETE /v1/mesh/nodes/{id}`).

## 14. Security model

| Asset | Protection |
| --- | --- |
| Agent API | Only on mesh IP + loopback; bearer token (host / device / node), role check per route, audit of every non-GET |
| Control listener `:7443` | Plain HTTP carrying sealed enrollment (§13.2), the relay (authenticated by node key, §13.7) and a hello; only `enroll`, `hello`, `relay` (later `webhooks`, `hooks/deploy`) exist there; enrollment rate-limited (10/min per address) and claim-protected (single use, TTL); node tokens work only from the node's own mesh address |
| Mesh traffic | WireGuard; keys generated on each node; certificates signed by the coordinator; map signed; revocation within one map push |
| Secrets at rest | XChaCha20-Poly1305 with AAD per cell; master key 0600 root; backups encrypted by restic |
| Containers | No Docker socket mounted into apps; containers can't reach the mesh except DNS and replies (existing isolation); Docker provider in Traefik off |
| Builds | Run in BuildKit, never on the host shell; repository code never executes outside a container; git hardened (no inherited config, only http(s)/ssh with pinned host keys) |
| Webhooks / deploy hooks | HMAC-SHA256 per git source (GitLab: its token), 60/min per source; deploy hooks a 256-bit secret, 6/min; payloads never trusted for the commit |
| GitHub App | Private key and webhook secret sealed; the manifest flow gated by a one-time state; installation tokens per repository, an hour at most, signed JWTs 9 minutes |

Threats explicitly out of scope: a root compromise of the main server (it holds the master
key and the coordinator key); a malicious Docker image
the user chose to deploy.

## 15. Failure modes

| Failure | Behaviour |
| --- | --- |
| Agent crash / update on main | Containers, Traefik and the mesh keep running; builds in progress are marked `failed: interrupted` on restart and can be retried; queued deployments resume |
| Power loss mid-deploy | On start, deployments in `building|deploying` → `failed: interrupted`; the reconciler restores the current deployment's container; route files are written atomically, so they always point at a container that was healthy |
| Database corrupt | `PRAGMA quick_check` at start; on failure the agent starts read-only for the platform routes, alerts, and `vercelify restore-state` restores `platform.db.bak` or a restic snapshot |
| Disk full | Builds refuse to start under 2 GB free on the Docker disk; disk cleanup (existing) runs first |
| Worker offline | Its apps keep running on it; the main server marks it offline after 2 min (no heartbeat), takes it out of the routes, does not reschedule apps with volumes; stateless apps with `runtime.failover = true` are redeployed elsewhere; when it returns, the containers of the old deployment are orphans and go after 10 min |
| Main server offline | Workers keep running what they have (their proxies keep serving anything reaching them); nothing is placed or deployed until it returns or the standby is promoted (§8.4, losing at most 5 minutes of changes) |
| Old main server returns after a promotion | It sees its coordinator key at a newer epoch on the standby and steps down (no coordinator, no platform) until `vercelify demote` |
| Coordinator offline | Mesh keeps the last map; joins/revokes wait; certificates are valid for 10 more days at the earliest |
| Relay offline | Direct paths unaffected; peers without a direct path are unreachable until it returns (doctor explains) |
| Clock wrong | Certificates checked with 5 min skew; the existing clock-sync doctor check alerts |
| Migration to a newer schema, then agent rollback | Newer `user_version` refuses to open. Updates save a fresh consistent control snapshot. Automatic binary rollback is allowed only without a schema rewind; otherwise current candidate/control writes are retained and repair or explicit `vercelify rollback --restore-control-state` is required. The explicit flag rewinds control metadata to the update snapshot, preserving the replaced file and warning that later control writes are lost. Application volume data is never restored as a side effect. Before/after update, capable agents record equivalent runtime/local-ingress and intentional stop/sleep state; changed workload intent preserves the candidate for review. A healthy control plane with failed workloads tries only compatible previous binaries and verifies the recorded scope. Legacy readiness remains explicitly unmeasured. |

### 15.1 Opt-in battery conservation (competitive roadmap M8)

The generated workload `vercelify.slice` orders itself before `docker.service`.
Systemd reverses that relationship during shutdown, keeping the workload slice
alive until Docker has stopped; it must not tear down container cgroups while
the daemon can still attempt restart-policy recovery. This ordering does not
change application stop intent or container restart policies.
Docker's transient `docker-<id>.scope` units also receive a prefix drop-in with
`Before=docker.service`: scopes may be stopped independently of their parent
slice during poweroff. Both relationships are needed. Uninstall removes only
Vercelify's named scope drop-in and preserves other administrator snippets.

The existing independent shell guard sources `host/lib/battery-policy.sh`; the
installer places both files together. `BATTERY_POLICY_ENABLED=0` preserves existing
behavior. Opt-in stages at 50/40/30/20 percent hold builds, pause owned build work,
sleep explicitly sleep-enabled previews, then sleep opted-in production apps. The
existing 15-percent clean-shutdown path remains independent of agent health.

Two consistent readings advance a tier. Recovery needs continuous AC for sixty
seconds and charge five points above the entered tier. Backward clock movement or
AC loss resets that delay. Missing sensors never authorize resume. Persisted
root-private tier and pause ownership survive guard restart; only owned pause IDs
are unpaused, never started. Managed container labels and exact installed core/
builder names exclude unrelated containers from all guard actions. Boot restoration
starts only core; app/database desired-state reconciliation owns workload recovery.

Agent `power_sleep.rs` reuses explicit Sleep settings and the existing engine stop/
admitted-wake path. `sleep.json` records battery reason and deployment identity before
stop. A restarted agent completes pending stops. A paused build's durable Building
status does not block conservation. Previously idle-sleeping work remains asleep;
manual app stop, PR close and generation replacement cannot be undone by power
recovery. Power-owned wake requires a fresh guard `ok` state; visiting while held
shows Waiting for power. Disabling sleep while a power-recovery intent remains is refused explicitly, even
between AC recovery and the automatic admitted wake.
Worker placement and the local build gate recognize battery hold/recovery states.

Doctor reports firmware AC recovery as unverified, with manual model-specific
guidance, and warns that worn batteries can lose writes. Physical outage, depletion,
lid, thermal and network recovery remain an open gate while the laptop is offline.
The Linux VM and synthetic sysfs qualify software behavior only. Production-data
backup-copy previews stay disabled. See `docs/POWER-RESILIENCE.md`,
`docs/RELEASE-QUALIFICATION.md` and milestone raw evidence for the actual scope.

## 16. Resource budgets

| Item | Budget | Enforced by |
| --- | --- | --- |
| `vercelify-agent` static musl binary | ≤ 8 MB | CI size gate |
| `vercelify-mesh` static musl binary | ≤ 6 MB | CI size gate |
| Agent idle RSS (main, 20 apps) | ≤ 40 MB | contract test samples `/proc/<pid>/status` |
| Mesh daemon idle CPU on a Pi 4 | < 1 % | mesh test (`ps` sampling) |
| Reconciler cost | one `docker ps` per node per 30 s | design |
| Build concurrency | hardware profile | §6.1 |
| Idle dedicated BuildKit | stops after configured idle interval (default 10 min), checked every 30 s | all-slot gate, §6.1.1 |

The release profile uses fat LTO and `opt-level = "z"` for the whole workspace (per-crate
levels measured larger under fat LTO): with thin LTO and `"s"` the agent measured 8.9 MiB
after M6, over budget; this way 7.6 MiB. M7 (the coordinator, `vercelify-mesh` as a library)
brought it to 8.0 MiB; logging filters with `tracing_subscriber::filter::Targets` instead of
`EnvFilter` (which pulls in a regex engine) brought it back to 7.75 MiB; the relay hub (axum's
WebSocket) added 0.07 MiB, mDNS and SPAKE2 0.08 MiB (7.90); `toml` 1 with only its parser
(no `toml_edit`) and axum without its unused default features brought it back to 7.75 MiB.
M9 (workers, the runtime API, the registry, the warm standby) is kept in check by three
changes: request bodies go through a small JSON extractor of our own on plain serde_json
instead of axum's (which runs every type through `serde_path_to_error`, a second copy of
each deserializer; axum's `json` feature is off), the command line is parsed by hand
instead of with clap, and the worker's runtime API is one handler dispatching on the path
(each axum handler costs several KiB). After M9c the agent measures 7.65 MiB as CI builds
it (`cargo zigbuild`; other linkers give larger numbers, so budgets are checked that way).
The competitive-roadmap M4 release profile uses `panic = "abort"` to keep admission
and bounded archive decoding within the static budget. An unexpected Rust panic
terminates the process; installed daemons rely on their service supervisor to
restart and reconcile durable state. Docker workloads remain independent of the
agent process. Development/tests retain unwinding. This policy does not guarantee
an interrupted control-plane request succeeds; clients must retry after recovery.
`vercelify-mesh` measures 3.2 MiB, 1.1 MiB of it rustls with
webpki roots for `wss://` and `https://` through a tunnel, 0.3 MiB the userspace stack. CI builds the binaries of every pull request with
`cargo zigbuild` and fails over budget, so growth shows up the commit it happens.

## 17. Decisions log

| # | Decision | Alternatives rejected |
| --- | --- | --- |
| D1 | SQLite, one writer connection | Postgres (RAM), JSON files (no transactions, no constraints) |
| D2 | docker CLI | Docker socket client crate (size, API version drift) |
| D3 | Traefik kept, run by us, Docker provider off | Own proxy (risk), Caddy (we'd rewrite `routing.rs` and its contract tests) |
| D4 | Userspace WireGuard (`boringtun`) on every platform, one UDP socket for WG + disco + STUN | Kernel WireGuard via netlink: can't share its socket, so hole punching and peer-assisted STUN wouldn't work, and it needs a second code path for the Mac anyway |
| D5 | Relay over WebSocket on the control listener, reachable through the Cloudflare tunnel | A hosted relay (recurring cost), TURN (heavier, separate service) |
| D6 | Coordinator in the agent, data plane in its own daemon | Both in the agent (network dies on agent restart), both in the daemon (two stores, two token systems) |
| D7 | Our own build detection with generated Dockerfiles | Nixpacks/Railpack as a dependency (the request is to own the stack; Dockerfiles stay inspectable) |
| D8 | Single main + warm standby | Raft cluster (needs 3 always-on machines) |
| D9 | Enrollment sealed to the coordinator's WireGuard key, everything else inside the mesh (changed in M7) | Self-signed TLS pinned by fingerprint (a second key and certificate to rotate, a TLS stack in a 6 MB daemon), plain HTTP (tokens in the clear), ACME (needs a public name) |
| D10 | Workers pull images from the main server's registry as `127.0.0.1:5055`, forwarded by their agent over the mesh (M9) | An insecure registry in every `daemon.json` (a Docker restart per server, trust in a plain-HTTP registry on the network), `docker save`/`load` per deploy (no layer sharing, full images over the relay) |
| D11 | Mac and Windows computers host through one Vercelify-owned Linux VM each (Lima: `vz`, or QEMU with HVF on macOS < 13; QEMU with WHPX on Windows), run by `vercelify-host`, with the unchanged agent inside (§19) | Native per-project execution (a second hosting engine), a VM per project (repeated overhead), Docker Desktop (licence terms for larger organisations; its VM and limits aren't ours), OrbStack (proprietary), WSL2 (`memory`, `processors` and `instanceIdleTimeout` are global `.wslconfig` settings shared with the owner's distributions, and instances stop after their last process) |

## 18. Setup and reaching the main server

The server installer (`install.sh` → `host/setup-host.sh`) and the Mac installer
(`install-mac.sh`) are the whole setup; neither needs an account anywhere.

1. **Server.** The installer sets up Docker, the agent with the engine, the mesh
   (`vercelify mesh init`: this server coordinates it as `10.77.0.1`), storage, the
   guard and the firewall, then prints a one-time short code and a full `vfy2.` code.
2. **Mac.** `install-mac.sh --pair vfy2.…` (the command the server prints) installs the
   app, `vercelify-mesh` and `vfy`, and leaves the code in the app's config directory
   (`pending-pairing`, mode 0600). On its first start the app reads it once, deletes
   it (a day old at most, decoded before use), joins the mesh with `mesh_join` and
   saves the server. With the app installed already, the short code works on the LAN
   (SPAKE2 at the control port, found by mDNS).
3. **Outside the LAN.** Joining needs the control port (TCP 7443) and WireGuard
   (UDP 51820) of the main server. `vercelify remote on` (`host/lib/remote.sh`; the
   installer asks) maps both on the router with UPnP IGD (`upnpc`), else NAT-PMP
   (`natpmpc`), for an hour, renewed every 20 minutes by `vercelify-remote.timer`. The
   router's external address is accepted only when public (not RFC 1918, CGNAT
   100.64/10 or link-local; behind CGNAT the mappings are removed and a Cloudflare
   tunnel is the way). It goes into `agent.toml` as `mesh_public_endpoints`
   (`IP:51820`, in every peer map) and `mesh_public_url` (`http://IP:7443`, in every
   invite; an `https://` tunnel URL is left alone), and the agent restarts only when the
   address changed. Nothing else is exposed: WireGuard drops packets from unknown keys,
   and the control port takes sealed, single-use, rate-limited enrollments and SPAKE2.
   `vercelify remote off` removes the mappings, the timer and only the keys it wrote.
   `GET /v1/mesh/status` reports `reachable_from_anywhere` (a public endpoint or URL is
   set), and the app shows "This network only" with the command, or "Ready away from home".

The cockpit finds servers among its own mesh
peers (`discover_servers` probes each server peer's `/v1/health`) and on the
LAN (mDNS); a worker joins with the invite its install command carries
(`--join`).

The proxy's container is recreated when the directory it mounts changes, not
only its configuration: a proxy that reads another directory never sees our
routes.


### Installation and display readiness (October 2026)

Default host installation defers optional AI, domains, remote access and Sunshine
questions; `--configure` retains the interactive wizard. Storage defaults to the
system disk; a second disk is selected explicitly with `--storage-device`. Final readiness reports
agent health and mesh availability, and supplies recovery commands for incomplete
connections. A rerun preserves the saved machine role. Only matching release refs
may use numbered release binaries; local and branch refs build their own. An edge
install resolves `main` to its commit (`/repos/…/commits/main`), downloads that exact
tree and first tries CI's build of it from the rolling `edge` prerelease
(`<binary>-<target>-<sha12>` plus `.sha256`, published by `.github/workflows/edge.yml`
for the last 20 commits); without a matching, checksum-verified asset it builds locally.
Existing Docker installations are checked for both Buildx and Compose v2 before
setup succeeds; the engine being reachable alone is insufficient.

The optional display receiver resolves an active local, non-root graphical session
through logind. In auto mode it reads only the user's display/auth/home environment
from the user service manager, drops to that user with runuser, and shares the X11
socket directory through the agent's private /tmp boundary. Explicit backend/env
configuration overrides detection. Receiver lifecycle operations are serialized;
stop and agent shutdown kill the receiver process group, including a runuser child.
A private mpv IPC socket supplies playback confirmation (`playback_active`) only
when `vo-configured` is true and `core-idle` is false, separate
from process liveness. The cockpit exposes a refresh/recovery step and establishes
Moonlight forwards before opening Sunshine's setup page. Share screen can mirror
a screen or extend the desktop: in Extend mode the app makes a display with
CoreGraphics' `CGVirtualDisplay` (macOS 11+, the API DeskPad and BetterDisplay
use; not in Apple's public headers, so looked up at run time and refused with a
message where it's missing), sized to the stream's quality (16:9), and streams the
screen AVFoundation lists for it. Nothing is installed: the display exists while
the app holds it and goes when the stream stops, its windows moving back.

Pairing Moonlight with Sunshine needs no Sunshine login or PIN from the user. While
Sunshine has no login it accepts a first one without authentication: on the first
`POST /v1/display/sunshine/pair` the agent sets one (user `vercelify`, a random
password in `<state_dir>/sunshine-login.json`, 0600) and keeps it. The cockpit
chooses a four-digit PIN, starts `moonlight pair HOST --pin PIN` and sends the same
PIN to the agent, which waits up to 25 s for the request in Sunshine's pending list
(`GET /api/pin`) and submits it with its pairing id (`POST /api/pin`; Sunshine
without pairing ids takes the PIN alone). The agent reaches Sunshine's HTTPS
interface through curl, as for GitHub, so it links no TLS stack; Sunshine's
certificate names no host, so curl skips name checks but pins the public key read
from `~/.config/sunshine/credentials/cacert.pem`, and the login never reaches another
listener on that port. A login made by hand is never overwritten: pairing reports it,
and `vercelify desktop reset-login` (Sunshine stopped, its `username`/`salt`/`password`
removed from `sunshine_state.json`, paired devices kept) lets the agent set its own.
Agents advertise `sunshine-pair`; the cockpit keeps the manual steps for older ones
and remembers per server that Moonlight is paired.

Display senders use `pkt_size=1128`. ffmpeg reports output-byte progress on stderr;
if it has produced no bytes after five seconds, the cockpit stops that process
and shows the Screen Recording repair steps beside the stream controls. A watchdog
checks the process identity so it cannot stop a replacement capture. `desktop on`
refuses to restart the agent while its jobs API reports running work, naming the
jobs to finish first; otherwise it announces the restart.

### Workflow safeguards (October 2026)

New Git apps enable previews by default. New app accepts Compose YAML and optional
secret environment variables, saved for build and runtime before requesting the
first deployment. GitHub manifest completion refreshes on `git-sources` events
and polls while waiting, closing the setup dialog when a new source appears.
Dialogs contain Tab navigation and restore focus when closed. Public-domain
instructions distinguish Cloudflare edge TLS from direct public DNS access.

Database creation refuses an unmounted storage tier before recording desired
state. Linking a database refuses to replace an existing connection variable;
callers can choose a different variable name. Retained volume metadata survives
owner deletion, including its physical storage identity; the Storage page lists
it as retained data. `DELETE /v1/platform/volumes/{id}` refuses volumes whose owner
still exists and removes retained SSD data through Docker (which refuses in-use
volumes). HDD folder deletion requires manual mount checks and is not offered by
the UI. Volumes orphaned by earlier releases remain visible in the raw host list;
recovering their removed ownership metadata is not implemented.

When the coordinator has been silent for 30 seconds (the map stream pings every 25), devices
check its signed beacon address every 10 seconds for five minutes, then every 30 seconds,
so a replacement quick tunnel (published about 15 seconds after it starts) is followed
within about 40 seconds without waiting for the old three-minute gate.
Every start of the quick tunnel is a new address, so the tunnel's unit starts
the `vercelify remote refresh` oneshot as it starts (not only the 20-minute
timer): the agent learns the address and the note is posted right away.

AI install and model-download jobs accept `DELETE /v1/jobs/{id}`. Cancellation
interrupts their pending command/HTTP future; child commands use kill-on-drop,
and the job/task finishes with “cancelled by user”. Finished jobs and operations
that cannot be safely interrupted return 409. `vercelify llm --cancel` cancels the
server's running AI jobs. Normal model-download completion also closes its job.

## 19. Hosting on a Mac or Windows computer

`crates/vercelify-host` (`vercelify-host`, ~1 MB) runs on the computer itself, before
and without Desktop. It owns exactly one Linux guest, the **Vercelify environment**,
shared by every app, database and build placed there; the guest runs the unchanged
agent and mesh (`install.sh` at the host manager's release), so hosting has one engine.
Linux servers don't use it.

- **Providers** (`provider::Provider`): `lima::Lima` with its own `LIMA_HOME` (the
  owner's Lima/Colima instances are never seen) and `qemu::Qemu`. Both boot the same
  pinned Debian 12 genericcloud image, checked by SHA-512. The guest mounts nothing
  from the computer. A worker forwards no ports; a main server forwards only the mesh
  control (7443/tcp) and WireGuard (51820/udp) ports, as a Linux main server exposes
  them on its LAN. Every provider step has a time limit (a stalled download fails as
  retry-safe instead of hanging).
- **Resources** (`vercelify_core::host::recommend`, policy v1): from physical
  measurements only (a missing one is a blocker, never a zero). It leaves the OS
  max(4 GiB, ⅓ of RAM) on macOS/Windows, 2 CPUs or a quarter, and max(10 GiB, 10 %)
  of free disk; it adds a 256 MiB hypervisor allowance; it refuses below 2 GiB /
  1 CPU / 20 GiB and says a computer under 3 GiB to spare is better used as a desktop.
  The owner may change it within `max`; the grant is validated again against fresh
  measurements when submitted.
- **Lifecycle** (`manager.rs`): `not_created → preparing → starting → verifying →
  ready`, `ready → stopping → stopped`, `updating`, `removing`, `blocked`, `failed`.
  Every change is an operation journaled (atomic JSON, 0600 / restricted ACL) before
  it acts, replayed by idempotency key, refused while another runs, and checked
  against the revision the owner saw (stop, resize, delete). Resizing a running
  environment restarts it and needs `restart_ok`; the disk only grows. Delete needs
  `acknowledged` (Desktop shows what is lost) and gives the next environment a new
  id; revisions only go up. A machine-wide lock (`/Users/Shared/Vercelify/host.lock`,
  `%ProgramData%`) makes a second host manager refuse.
- **Ready** means the health script passed inside the guest: Docker and the agent
  active, `/v1/health` answering, the isolation rule present, and the guest clock
  within 10 s of the host's (chrony steps after any jump: mesh certificates and ACME
  are time-bound).
- **Recovery**: every 20 s the manager compares the recorded state with the provider:
  a ready environment whose VM is down is started again; a stopped one is kept
  stopped; an operation the last process left running is reported failed, retry
  safe. On macOS the job is a LaunchAgent (Apple's Virtualization framework is meant
  for a logged-in user); with FileVault nothing starts before a login, and the
  installer says so. On Windows it is a SYSTEM task at startup. While ready it holds
  an idle-sleep assertion (`caffeinate -is`, `SetThreadExecutionState`); a closed
  laptop on battery still sleeps, and the UI says a sleeping computer serves nothing.
- **Isolation**: in the guest a `DOCKER-USER` rule drops container traffic to the
  host's address (behind which are the computer's loopback services and the host
  manager), except DNS (Lima's resolver is on that address).
- **API**: management on `127.0.0.1:7425`, every route behind the owner token
  (`owner.token`, readable by the owner only; 20 bad tokens a minute and the rest
  wait); the guest's read-only listener on `127.0.0.1:7426` (`/v1/host/status`,
  cloud-init seed). Desktop reaches the first through `host_request` (paths under
  `/v1/host/` only).

Not yet: a remote Desktop managing another computer's host manager (it is local to
the computer; the environment's agent is managed over the mesh as usual), Windows
hosting before real-hardware qualification, automatic database failover between
computers.

### Restore, pairing and readiness review (November 2026)

An instance restore enumerates its owned volumes and all their running writers.
Writer detection includes bind sources above or below a restored storage path,
without splitting paths containing spaces. Before stopping the engine, free-space
admission groups destinations by their staging filesystem and budgets apparent file
sizes (staging does not preserve sparse holes); missing or invalid size/free-space
measurements refuse the restore without stopping workloads.
Docker inspection errors or writers remaining after Stop abort replacement before
any live data changes and restart the stopped services. Other Docker volumes are
excluded unless `--all-volumes` is explicit. Each volume is copied to a clean staging
tree and swapped into place; the previous tree is retained for recovery, preventing
post-snapshot WAL/data files from being mixed into restored state. Backup setup
preserves off-site credentials and retention settings. See `docs/BACKUPS.md`.

Mesh state updates hold a cross-process lock from identity verification through
replacement, including pairing writes. Unix uses flock; Windows uses the standard
file lock. Every atomic write owns a unique exclusive temporary file, so concurrent
writers cannot truncate or rename another writer's temporary file.

HTTP deployment readiness requires a 2xx or 3xx response, including probes made
through the proxy for isolated previews. Authentication failures, missing paths,
and throttling responses are readiness failures rather than successful publication.

### Missing builder recovery qualification (4 November 2026)

If a dedicated BuildKit container has been removed but its buildx registration
remains, a bounded build discards only that stale registration with
`buildx rm --keep-state`, preserving its cache volume, then recreates the builder
with the configured memory, swap and CPU limits. Other inspection or bootstrap
errors continue to fail closed; an existing running builder is never replaced.

### Streaming image handoff deadline (4 November 2026)

Image upload is an authenticated bulk route outside the ordinary 120-second API
budget. It retains a 30-second body inactivity limit, the declared 3-GiB ceiling,
resource reservation and per-frame lease/generation/configuration fencing. The
route's overall ceiling matches the 90-minute hard build lease; the normal
five-minute renewable lease and 15-minute Docker-load deadline still apply.

### Native macOS system resolver (4 November 2026)

Native macOS system mode serves its signed-map DNS zone on an ephemeral loopback
UDP port and writes both `nameserver` and `port` to `/etc/resolver/vfy`. Packets to
the device's own point-to-point mesh address do not reach its local DNS socket.
Linux continues to listen on its mesh address at port 53. New resolver directories
and files are explicitly system-readable (0755/0644), independently of a private
launch umask. Private node keys/state retain their existing restrictive permissions.

Mesh node names retain A/AAAA records. Wildcard application names advertise A
records only: the current Docker proxy ingress preserves the private-preview
client address on IPv4; its IPv6 userland-proxy path rewrites the source and
correctly fails private access closed. Publishing that path as an app AAAA record
causes normal dual-stack browsers to receive 403 instead of using the working
IPv4 ingress. This does not remove node-to-node IPv6, nor public IPv6 ingress at
a configured external tunnel. Direct private-app IPv6 ingress remains unsupported
until source-preserving IPv6 hosting is implemented and qualified.
