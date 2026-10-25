# System design: the Vercelify platform and mesh

This is the engineering design behind [PLAN-NATIVE-PLATFORM.md](../PLAN-NATIVE-PLATFORM.md).
The plan says *what* and *in which order*; this document fixes *how*: process and crate
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
`vercelify rollback` restores the pre-update backup, see §15).

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
| `app_domains` | `host` UNIQUE, `app_id`, `https`, `is_primary`, `redirect_to`, `cert_status` | |
| `env_vars` | `scope` project/environment/app, `scope_id`, `key`, `value` BLOB (sealed), `build`, `preview_only`, `secret` | UNIQUE(`scope`,`scope_id`,`key`,`preview_only`) |
| `deployments` | `app_id`, `number` (per app), `kind` production/preview, `pr_number`, `commit_sha`, `trigger`, `rollback_of`, `image`, `node_id`, `status`, `error`, `container`, timestamps | UNIQUE(`app_id`,`number`) |
| `previews` | UNIQUE(`app_id`,`pr_number`), `head_sha`, `deployment_id`, `status` | |
| `databases` | `name` UNIQUE, `kind`, `version`, `image`, `node_id`, `credentials` BLOB (sealed), `public_port`, `public_bind`, `desired`, `backup_schedule` | |
| `database_backups` | `database_id`, `status`, `bytes`, `snapshot` | |
| `volumes` | `owner_kind` app/database, `owner_id`, `name`, `mount_path`, `tier` ssd/hdd, `host_path` | UNIQUE(`owner_kind`,`owner_id`,`name`) |
| `git_sources` | `provider`, `base_url`, `auth` none/token/deploy_key/github_app, `secret` BLOB (sealed), `public_key` | |
| `deploy_hooks` | `app_id`, `secret_hash` UNIQUE | |
| `scheduled_tasks` | `app_id`, `schedule`, `command`, `enabled`, `last_*` | |
| `tokens` | `hash` UNIQUE, `role` owner/deployer/viewer, `project_id`, `revoked_at` | Replaces `tokens.json` (migrated in M1) |
| `audit` | INTEGER PK, `at`, `actor`, `method`, `path`, `status` | Replaces `audit.log` (migrated in M1) |

Rules enforced in SQL (CHECK constraints) rather than only in Rust: enum columns, DNS-label
names, `public_port` range, `number > 0`. Foreign keys cascade from apps to their domains,
variables, deployments, previews, volumes, hooks and tasks; a database refuses deletion
while an app references it in a variable reference (§10).

### 4.2 Backups and standby

Online copy via SQLite's backup API to `platform.db.bak` before every migration, every
`vercelify update`, and every 5 minutes for restic and the warm standby (§8.4). Restic
backs up the copy, never the live file.

## 5. Secrets

- **Master key**: 32 random bytes at `/var/lib/vercelify/master.key`, mode 0600, owned by
  root. Created by the installer, or by the agent on first start if missing. Included in
  restic backups (which are themselves encrypted with the user's restic password).
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
| push | Only if the target node ≠ this node: tag `<registry>/vfy/<app>:<dpl>`, push to the mesh registry | registry down |
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
           recreate (missing; from the deployment's image, no build)
           remove (labelled ours, belongs to no current deployment, older than 2 min)
           report (crash loop: restarted > 5 times in 10 min → alert, back off to 5 min)
regenerate route files for the node; write only files whose content changed
```

The reconciler never builds and never changes `current_deployment_id`. Sleeping apps
(`sleep.rs`) are "desired asleep" while the sleep module says so; the wake listener starts
them, and the reconciler leaves them alone.

### 8.2 Runtime API (worker side)

The main server drives other nodes through their agents over the mesh, authenticated with
the main server's node token (role `owner`, kind `server`):

| Route | Does |
| --- | --- |
| `GET /v1/runtime/containers` | our containers with state, health, restart count, image, labels |
| `POST /v1/runtime/apply` | `{create: [...], start: [...], stop: [...], remove: [...], routes: {...}}`: idempotent batch |
| `POST /v1/runtime/images/pull` | pull `<registry>/vfy/<app>:<dpl>` |
| `GET /v1/runtime/containers/{name}/logs` | logs, streaming with `follow=true` |
| `POST /v1/runtime/exec` | WebSocket `docker exec` for the terminal |

The main server is just another node to itself; the same code path runs locally without HTTP.

### 8.3 Placement

`app.node_id` if set. Otherwise score every usable node (online in the last 2 min, not on
battery below 50 %, not `thermal_paused`, architecture compatible with the image or
buildable for it): `free_memory − requested_memory`, highest wins, ties to the main server.
Placement is decided once per app (sticky), so volumes stay put; moving an app is an explicit
operation (`POST /v1/apps/{id}/move {node_id}`) that refuses when the app has volumes.

Replicas (`runtime.replicas > 1`): spread across distinct nodes when possible; each node's
Traefik load-balances its local replicas; the main server's base route lists the other nodes'
mesh IPs as additional servers.

### 8.4 Warm standby

`settings.standby_node`: every 5 minutes the main server pushes its latest SQLite backup and
`master.key` (encrypted to the standby's mesh key) to `POST /v1/standby/snapshot` on that
node. `sudo vercelify promote` on the standby: verify snapshot age, install it as
`platform.db`, switch `role = main`, start the coordinator with the stored coordinator key,
announce through the mesh. Old main, if it comes back, sees a newer coordinator epoch in the
map and demotes itself to worker (`epoch` in §13.4).

## 9. Proxy and routing

- `vercelify-proxy`: Traefik (pinned, `VERCELIFY_TRAEFIK_VERSION`, contract-tested), host
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
    priorities 5000–50000 (unchanged), now copying our base routers instead of Coolify's.
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
  tagged `db:<name>`. Restore stops dependants, restores, starts them.
- Volume tier moves reuse `coolify.rs`'s move logic, rewritten against `volumes`.

## 11. Git, previews, hooks

- `autodeploy.rs` keeps polling with `git ls-remote` (works behind CGNAT). Its targets
  become our apps; deploy = `deployer.enqueue(app, sha, trigger = push)`.
- Previews: the existing logic (forks never built, per-app cap, GitHub status + comment)
  calls `deployer.enqueue_preview(app, pr, sha)`; closing a PR deletes the preview's
  container, route and row.
- Webhooks: `POST /v1/webhooks/{github|gitlab|gitea}` on the control listener (reachable via
  the tunnel), HMAC-verified with the per-source secret, only nudge the poller (never trust
  the payload's sha without `ls-remote`).
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

Full route list: §4.14 of the plan. Request/response types live in
`vercelify-core::platform` and are exported to TypeScript.

## 13. Mesh

### 13.1 Addressing and keys

- IPv4 `10.77.0.0/16`, IPv6 `fd76:6679:6d65::/48`. `ip_index` n ∈ [1, 65534] maps to
  `10.77.(n>>8).(n&255)` and `fd76:6679:6d65::n`; index 1 is the coordinator. Configurable
  prefix (`mesh.ipv4_prefix`) for LANs that already use `10.77/16`.
- **Node key**: X25519, the WireGuard static key, generated on the node, private part never
  leaves it (`/var/lib/vercelify/mesh/node.key`, 0600; on the Mac in the app's data dir).
- **Coordinator key**: Ed25519 (`/var/lib/vercelify/mesh/coordinator.key`), signs node
  certificates and peer maps.
- **TLS for the control listener**: self-signed certificate generated from a separate P-256
  key; its SHA-256 fingerprint is pinned in pairing codes and in each node's state.

### 13.2 Enrollment

```
installer / app                       coordinator (:7443, TLS pinned)
   │  POST /v1/mesh/enroll                │
   │  { claim: <short code | enroll secret>, name, kind, os, arch, public_key }
   │─────────────────────────────────────▶│ verify claim (single use, TTL, 5 tries)
   │                                      │ allocate ip_index, mint node token (vfyn_…),
   │                                      │ for devices also a device token (vfyd_…) with role
   │◀─────────────────────────────────────│ { node_id, ipv4, ipv6, certificate, node_token,
   │                                      │   agent_token?, coordinator_key, control_urls, map }
```

Certificate: `vfyc1.<base64url(payload JSON)>.<base64url(ed25519 sig)>`, payload
`{v:1, node_id, name, kind, role, public_key, ipv4, ipv6, not_before, not_after, epoch}`;
lifetime 30 days, renewed at day 20 via `POST /v1/mesh/renew` (authenticated by node token +
proof: the request is made over the mesh from the node's own IP).

### 13.3 Pairing code v2

`vfy2.<base64url JSON>`: `{v:2, hostname, role, control_urls: ["https://192.168.1.20:7443",
"https://mesh.example.com"], tls_sha256, coordinator_key, enroll_secret, expires_at}`. The
8-symbol short code still works: the app finds the server on the LAN (mDNS) or the user
types the address, then claims with the code. `vfy1.` codes keep working during migration.

### 13.4 Peer map

`GET /v1/mesh/map` on the control listener, `Authorization: Bearer vfyn_…`, NDJSON stream:

```json
{"type":"map","signed":"vfym1.<base64url payload>.<base64url ed25519 signature>"}
{"type":"ping"}
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
- The map is persisted by the node (`map.json`) and used at start before the stream connects.
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
- macOS app mode: no tun; a `smoltcp` interface over the same WireGuard tunnels. SOCKS5 on
  `127.0.0.1:7480` (CONNECT only, hostname resolution through mesh DNS), fixed TCP/UDP
  forwards configured by the cockpit (`127.0.0.1:<port> → <mesh ip>:<port>`).
- macOS system mode: `utun` via the system socket `com.apple.net.utun_control`, same loop
  as Linux.

### 13.6 Disco and path selection

Disco messages are sealed with a key derived from both nodes' WireGuard keys:
`HKDF-SHA256(X25519(my_priv, peer_pub), "vercelify-disco-v1")` → ChaCha20-Poly1305;
frame `VFYD ‖ sender public key (32) ‖ nonce (12) ‖ sealed { kind: ping|pong|call_me, tx (12 B), observed_addr? }`.

Per peer:

1. Every 5 s while not on a confirmed direct path, and every 25 s after: send `ping` to every
   candidate endpoint (map endpoints + addresses learned from earlier pongs).
2. `pong` carries the address the ping came from (gives each side its public mapping, so
   peers act as STUN servers for each other) and confirms the path; the lowest round-trip
   confirmed path wins, with LAN preferred within 2 ms.
3. `call_me` is sent through the relay so both sides ping each other at the same time (hole
   punching for endpoint-dependent filtering).
4. No confirmed path: WireGuard packets go through the relay. A confirmed path that misses 3
   pings falls back to the relay.

STUN: binding requests to `mesh.stun_servers` (default `stun.cloudflare.com:3478`,
`stun.l.google.com:19302`) every 60 s from the same socket, to learn the public endpoint
before any peer is reachable.

### 13.7 Relay

Endpoint `GET /v1/mesh/relay` (WebSocket, binary) on the control listener, authenticated by
node token. Frames: client → relay `dst public key (32) ‖ packet`; relay → client
`src public key (32) ‖ packet`. The relay forwards only between nodes that the map lets talk
to each other, keeps no packets (drops when the destination's queue holds more than 256),
and counts bytes per node for the Servers page. Payloads are WireGuard (or disco) packets, so
the relay sees only ciphertext.

### 13.8 DNS and ACLs

- DNS server on `<mesh ipv4>:53` and `[<mesh ipv6>]:53` (servers and system-mode Macs), zone
  `vfy`: `A`/`AAAA` for `<node>.vfy` and `*.<node>.vfy`; everything else `REFUSED`. Linux
  hosts with `systemd-resolved` get `resolvectl dns vfy0 10.77.0.x; resolvectl domain vfy0 ~vfy`.
- Firewall (`host/firewall.sh` + a generated `vercelify-mesh` chain): input on `vfy0` from
  device addresses allowed only to `allowed_ports` (7420, 80, 443, 11434, 5600–5610/udp and
  enabled database ports); from server addresses everything. The daemon writes the device
  address set to `/run/vercelify/mesh-acl.json`; `vercelify-firewall` renders it with `ipset`
  (or nftables sets when available) and is reloaded on change.

## 14. Security model

| Asset | Protection |
| --- | --- |
| Agent API | Only on mesh IP + loopback; bearer token (host / device / node), role check per route, audit of every non-GET |
| Control listener `:7443` | TLS with a pinned self-signed certificate; only `enroll`, `renew`, `map`, `endpoints`, `relay`, `webhooks`, `hooks/deploy` exist there; enrollment rate-limited and claim-protected (single use, TTL, 5 tries, 1 s penalty) |
| Mesh traffic | WireGuard; keys generated on each node; certificates signed by the coordinator; map signed; revocation within one map push |
| Secrets at rest | XChaCha20-Poly1305 with AAD per cell; master key 0600 root; backups encrypted by restic |
| Containers | No Docker socket mounted into apps; containers can't reach the mesh except DNS and replies (existing isolation); Docker provider in Traefik off |
| Builds | Run in BuildKit, never on the host shell; repository code never executes outside a container; git hardened (no inherited config, only http(s)/ssh with pinned host keys) |
| Webhooks / deploy hooks | HMAC / 256-bit secret, rate limits, payloads never trusted for the commit |

Threats explicitly out of scope: a root compromise of the main server (it holds the master
key and the coordinator key, as Coolify's host held its `APP_KEY`); a malicious Docker image
the user chose to deploy.

## 15. Failure modes

| Failure | Behaviour |
| --- | --- |
| Agent crash / update on main | Containers, Traefik and the mesh keep running; builds in progress are marked `failed: interrupted` on restart and can be retried; queued deployments resume |
| Power loss mid-deploy | On start, deployments in `building|deploying` → `failed: interrupted`; the reconciler restores the current deployment's container; route files are written atomically, so they always point at a container that was healthy |
| Database corrupt | `PRAGMA quick_check` at start; on failure the agent starts read-only for the platform routes, alerts, and `vercelify restore-state` restores `platform.db.bak` or a restic snapshot |
| Disk full | Builds refuse to start under 2 GB free on the Docker disk; disk cleanup (existing) runs first |
| Worker offline | Its apps keep running on it; the main server marks it offline after 2 min, alerts, does not reschedule apps with volumes; stateless apps with `runtime.failover = true` are started elsewhere |
| Coordinator offline | Mesh keeps the last map; joins/revokes wait; certificates are valid for 10 more days at the earliest |
| Relay offline | Direct paths unaffected; peers without a direct path are unreachable until it returns (doctor explains) |
| Clock wrong | Certificates checked with 5 min skew; the existing clock-sync doctor check alerts |
| Migration to a newer schema, then agent rollback | Newer `user_version` refuses to open; `vercelify rollback` restores the pre-update `platform.db.bak` |

## 16. Resource budgets

| Item | Budget | Enforced by |
| --- | --- | --- |
| `vercelify-agent` static musl binary | ≤ 8 MB | CI size gate |
| `vercelify-mesh` static musl binary | ≤ 6 MB | CI size gate |
| Agent idle RSS (main, 20 apps) | ≤ 40 MB | contract test samples `/proc/<pid>/status` |
| Mesh daemon idle CPU on a Pi 4 | < 1 % | mesh test (`ps` sampling) |
| Reconciler cost | one `docker ps` per node per 30 s | design |
| Build concurrency | hardware profile | §6.1 |

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
| D9 | Self-signed TLS pinned by fingerprint on `:7443` | Plain HTTP on the LAN (tokens in the clear), ACME for the control listener (needs a public name) |
