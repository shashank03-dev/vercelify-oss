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

## 6. Builds

### 6.1 Queue

- A single `BuildQueue` task with concurrency `settings.build_concurrency` (default from the
  hardware profile: 1 on ≤ 8 GB, 0 on a Pi 4, meaning "build elsewhere").
- Queue order: FIFO, except that a newer production deployment of the same app supersedes a
  queued (not yet started) older one (`status = superseded`).
- Thermal pause: before starting a build and every 5 s during one, read
  `/run/vercelify/power-guard.state`. `thermal_paused` → `docker pause` the BuildKit
  container (`buildx_buildkit_vercelify0`), resume when it clears. `shutting_down` → cancel.

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
repository on the host).

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

1. Pick the node (§8.3). Ensure networks `vfy-<project>` and `vfy-proxy` exist on it.
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

Automatic rollback (today's feature): a deployment that becomes unhealthy (container exited
or health failing 3 times) within 3 minutes after step 4 triggers a rollback deployment of
the previous healthy one.

Compose apps: `docker compose -p vfy-<app> -f <file> -f <generated override> up -d --wait`,
where the override adds our labels, networks, limits and the env file. The routed service is
`runtime.service`. Zero downtime is best effort (compose recreates in place).

### 7.3 Rollback

`POST /v1/apps/{id}/rollback {deployment_id}` creates a deployment with `trigger = rollback`,
`rollback_of`, the target's `image` and `commit_sha`. If the image still exists on the node
(or in the registry) it skips the build ("instant"), otherwise it rebuilds that commit
("rebuild"). Rollback pins: `apps.source.pinned_sha` is set so auto-deploy does not
immediately redeploy the branch head; deploying again clears it.

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
  128 MB, published on 127.0.0.1:5055 and its mesh address:5055, data in
  `<state>/registry`). Every server calls it `127.0.0.1:5055`: Docker trusts a
  loopback registry without TLS, so no `daemon.json` changes and no Docker restart;
  on a worker the agent forwards that port to the main server over the mesh
  (WireGuard encrypts it). The engine starts the registry before the first push and
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
removed (`DELETE`). Compose apps run on the main server; previews run one instance on
the app's server.

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
    priorities 5000–50000 (unchanged), copying our base routers.
  - `vercelify-wake.yaml`: priority 1 catch-all to the wake listener (unchanged).
- Router naming keeps today's `http-0-<app>` / `…-pr-<n>` pattern so `sleep.rs` reads the
  same Prometheus series.

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
  container, route and row.
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

Roles: **viewer** = GET; **deployer** = apps, deployments, env, domains, previews,
databases (not delete), logs, terminal; **owner** = everything incl. tokens, mesh, nodes,
settings, deletes. The host token (`agent.token`) is owner.

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
  response open, and the `docker logs` process ends with it.
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
  change; the coordinator adds the source address it observed.

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

1. Candidates: the map's endpoints, then what the node learned (the source of a peer's
   pings, `call_me` lists, authenticated WireGuard packets), at most 16.
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


- DNS server on `<mesh ipv4>:53` and `[<mesh ipv6>]:53` (servers and system-mode Macs), zone
  `vfy`: `A`/`AAAA` for `<node>.vfy` and `*.<node>.vfy`; everything else `REFUSED`. Linux
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
| Migration to a newer schema, then agent rollback | Newer `user_version` refuses to open. `vercelify update` takes a fresh consistent copy of the live database first (`vercelify-agent db-backup`, SQLite's backup API, no migration; an older agent is stopped for a plain copy), not the 5-minute `.bak`. `rollback` and a failed update's self-undo stop the agent (checkpointing the WAL), restore that copy when the live schema is newer, start the agent again and check the engine answers (`platform` in `/v1/health`); if it doesn't, they say `sudo vercelify restore-state …` |

## 16. Resource budgets

| Item | Budget | Enforced by |
| --- | --- | --- |
| `vercelify-agent` static musl binary | ≤ 8 MB | CI size gate |
| `vercelify-mesh` static musl binary | ≤ 6 MB | CI size gate |
| Agent idle RSS (main, 20 apps) | ≤ 40 MB | contract test samples `/proc/<pid>/status` |
| Mesh daemon idle CPU on a Pi 4 | < 1 % | mesh test (`ps` sampling) |
| Reconciler cost | one `docker ps` per node per 30 s | design |
| Build concurrency | hardware profile | §6.1 |

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
`panic = "abort"` would save more but turns a panicking task into a dead agent; not taken. `vercelify-mesh` measures 3.2 MiB, 1.1 MiB of it rustls with
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
