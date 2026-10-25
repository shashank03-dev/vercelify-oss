# Plan: replace Coolify and Tailscale with our own platform

Status: **proposal**, October 2026. Nothing here is built yet.

Goal: Vercelify keeps everything it does today (push-to-deploy, PR previews,
rollbacks, databases, custom domains with HTTPS, pools of servers, private
access from the Mac) without Coolify and without Tailscale. One Rust control
plane and one mesh network, both ours, built into the agent and the cockpit.

This reverses a rule in `CLAUDE.md` ("Prefer extending Coolify over
re-implementing it"). If we accept this plan, that rule and the stack
description in `CLAUDE.md` change in milestone M0.

---

## 1. Why, and what it buys

| Today | After |
| --- | --- |
| Coolify stack (`coolify`, `coolify-db` Postgres, `coolify-redis`, `coolify-realtime`): roughly 1 GB+ of RAM at idle on an 8 GB laptop, and Coolify asks for 2 GB minimum | Control plane inside `vercelify-agent` using SQLite. Target: under 40 MB RSS at idle, under 120 MB during a build (the build itself runs in BuildKit) |
| We write Coolify's Postgres schema directly (volumes, proxy flags, worker keys, preview templates). Every Coolify release is a contract-test risk (B3 in GAP-ANALYSIS) | We own the schema and migrations. No version pinning dance |
| Two dashboards (cockpit + Coolify webview), two APIs, two token kinds | One API (`/v1/…`), one token model, one UI |
| Workers are reached by Coolify over SSH as root (`/root/.ssh` write in the agent unit) | Workers run the same agent. The main server's scheduler calls the worker's agent over the mesh. No SSH keys at all |
| Tailscale: an account and coordination server outside our control, node keys that expire after 180 days (B12), install from `tailscale.com` | Our own WireGuard mesh. The coordinator runs on the main server. Keys rotate on their own and never expire silently |

What it costs:

- A lot of code. The repo is about 43k lines today and leans on Coolify for builds,
  deploys, databases and workers. A rough estimate for this plan is 35–50k more lines,
  tests included.
- **NAT traversal is the real regression.** Tailscale's free DERP relays let any two
  devices connect, even when both sit behind CGNAT. We have no free global relay. Our
  answer (section 3.6): direct WireGuard when possible, then a relay on the main server
  published through the Cloudflare tunnel, then any node with a public IP. With **no
  domain on Cloudflare and both ends behind CGNAT**, the Mac can only reach the server
  on the same LAN. We have to say that plainly in the docs.
- Nixpacks covered dozens of languages. Our own build detection (section 2.4) will
  cover the common ones well. Anything else uses a Dockerfile.

## 2. What we keep as dependencies (and why)

"Build everything ourselves" stops at components that are not Coolify or Tailscale and
that would be unwise to rewrite:

| Kept | Reason |
| --- | --- |
| **Docker Engine + BuildKit (`docker buildx`)** | The container runtime. Rewriting it is out of scope |
| **Traefik** (pinned, run by us, not by Coolify) | We already generate Traefik dynamic files (`routing.rs`) and contract-test them on 3.4/3.7. ACME/Let's Encrypt, HTTP/2, WebSockets and the metrics we use for sleeping apps all come with it. A homemade TLS-terminating proxy would be the riskiest part of the whole system |
| **WireGuard** (Linux kernel module; `boringtun` userspace on the Mac) | The crypto and the protocol. We build the coordination, NAT traversal, relay, DNS and ACLs |
| **Cloudflare Tunnel** (optional, as today) | Public HTTPS behind CGNAT, and the carrier for our mesh relay |
| **restic**, official database images, `git` | As today |

If you want Traefik replaced too, that would be its own project after this one. I
recommend against it.

---

## 3. The mesh (replacing Tailscale): `vercelify-mesh`

### 3.1 Shape

```
                 ┌──────────── main server ────────────┐
                 │ agent (coordinator role)             │
                 │  /v1/mesh/*  node registry, ACLs,    │
                 │              key signing, peer maps  │
                 │ vercelify-mesh  (kernel WG: vfy0)    │
                 │  relay  :443 via tunnel / :7443      │
                 └──────▲───────────────▲───────────────┘
           WireGuard UDP│ or relay       │ WireGuard UDP or relay
     ┌──────────────────┴───┐     ┌──────┴─────────────────┐
     │ worker / Pi           │     │ Mac                     │
     │ vercelify-mesh (vfy0) │     │ vercelify-mesh (user)   │
     │ agent (worker role)   │     │  userspace WG + TCP/IP  │
     └───────────────────────┘     │  SOCKS5 + forwards on   │
                                   │  127.0.0.1 → cockpit,vfy │
                                   └─────────────────────────┘
```

- **New crate** `crates/vercelify-mesh` (library) and a binary `vercelify-mesh`.
  It is a separate daemon on purpose, the same reasoning as the power guard: the
  network must stay up when the agent restarts or is updated.
- **Addresses:** IPv4 `10.77.0.0/16` and IPv6 ULA `fd76:6679:6d65::/48`, both
  configurable. Chosen so they can run next to Tailscale's `100.64.0.0/10` during
  migration. The main server is `10.77.0.1`.
- **Topology:** full mesh between servers (apps, registry and uptime checks need
  server-to-server traffic). Devices (Macs) peer only with servers. Peer lists stay
  small on a Pi.

### 3.2 Data plane

| Platform | How |
| --- | --- |
| Linux servers (x86, Pi) | Kernel WireGuard interface `vfy0`, configured over netlink (generic netlink `wireguard` family + rtnetlink for addresses/routes), no `wireguard-tools` dependency. Fallback when the module is missing: `boringtun` on `/dev/net/tun` |
| macOS, default "app mode" (no root) | `boringtun` + `smoltcp` userspace TCP/IP stack in the daemon, run as a per-user LaunchAgent. It exposes a SOCKS5 proxy and fixed port forwards on `127.0.0.1`. The cockpit (`reqwest` proxy) and `vfy` (`curl --socks5-hostname`) use it. No kernel extension, no admin password |
| macOS, optional "system mode" | A privileged LaunchDaemon (installed by `install-mac.sh --system-network` or the app via `SMAppService`) that creates a `utun` interface, so a browser can open private app URLs directly. `/etc/resolver/vfy` points `*.vfy` at the mesh DNS |
| Display streaming (UDP 5600–5610) | System mode: direct. App mode: a UDP forward on `127.0.0.1` that ffmpeg sends to |

### 3.3 Control plane (coordinator, in the agent on the main server)

- **Node registry** in the platform database: node id, name, kind (server/device),
  WireGuard public key, mesh IPs, endpoints (local, STUN-observed, coordinator-observed),
  OS/arch, last seen, owner, tags.
- **Enrollment:** the existing pairing flow (8-symbol code, single use, 15 minutes,
  five wrong attempts) now trades the code for a mesh enrollment. The new node
  generates its own key pair. The private key never leaves the node. The coordinator
  signs a node certificate (Ed25519): node id, public key, IPs, role, expiry.
- **Peer map distribution:** each `vercelify-mesh` keeps one long-lived stream to the
  coordinator (`GET /v1/mesh/map`, newline-delimited JSON like `/v1/events`). Changes
  arrive within a second. A full map comes after every reconnect.
- **Key rotation:** node certificates last 30 days and renew themselves at 20 days.
  An expired certificate is a doctor check and an alert well before it matters.
  This replaces B12.
- **Revocation:** `DELETE /v1/mesh/nodes/{id}`, the cockpit's Servers/Devices page, or
  `sudo vercelify mesh revoke`. Peers drop the key on the next map.
- **Coordinator unreachable:** nodes keep their last map (persisted) and keep working.
  Only joins, revokes and endpoint changes wait.

### 3.4 Names (replacing MagicDNS)

- A small DNS responder inside `vercelify-mesh`, on the mesh IP, port 53, for the
  `vfy` zone: `<node>.vfy` → mesh IP, `<app>.<node>.vfy` → that node's mesh IP
  (private apps through its Traefik).
- Linux: per-link DNS for `vfy0` through `systemd-resolved` (`resolvectl`), when present.
- Private apps without a domain keep working through sslip.io with the mesh IP
  (`<app>.10.77.0.1.sslip.io`), as they do with the Tailscale IP today.

### 3.5 ACLs

Enforced on every server with nftables/iptables (`host/firewall.sh`, interface `vfy0`
instead of `tailscale0`) and by the coordinator in the peer map:

| From → to | Allowed |
| --- | --- |
| device → server | agent `:7420`, Traefik `:80/:443`, Ollama `:11434`, display UDP, database public ports the user enabled |
| server → server | everything (registry, app traffic, agent API between servers) |
| device → device | nothing |
| containers → mesh | DNS and established connections only (today's container → tailnet isolation, kept) |

Roles on the device certificate (owner / deployer / viewer, section 4.12) also limit
which agent calls a device may make.

### 3.6 NAT traversal and relay

In order, per peer pair, re-tried every 30 s while a worse path is in use:

1. **LAN:** same subnet, local endpoint. Mac and laptop at home connect directly.
2. **Direct over the internet:** endpoints learned through STUN (a minimal RFC 5389
   client, public free STUN servers, configurable) and from the coordinator's view of
   the source address. Both sides send at the same time (coordinator-signalled),
   persistent keepalive 25 s. Works for most home routers that aren't symmetric NAT.
3. **Relay:** `vercelify-mesh relay` on the main server forwards opaque WireGuard
   packets keyed by public key, over WebSocket/HTTPS. Traffic stays end-to-end
   encrypted by WireGuard, so the relay can't read it. Kernel WireGuard points the
   peer endpoint at a local UDP socket that the daemon bridges to the relay.
   The relay is reachable:
   - through the **Cloudflare tunnel** at `relay.<domain>` (works behind CGNAT, free),
   - on a **public IP** (`:7443/tcp`, opened by the firewall when the user chooses it),
   - on **any node with a public IP** (a cheap VPS is optional, never required).

Without any of these and with both ends behind CGNAT, only LAN works. The doctor
explains this, and the Servers page shows the path per peer (LAN / direct / relay).

### 3.7 First contact (replacing "discover peers from `tailscale status`")

- **LAN:** the agent announces `_vercelify._tcp` over mDNS (a small responder, no
  Avahi dependency) with `pairing_open`. The cockpit browses for it.
- **Remote:** the installer prints the 8-symbol code plus the relay or public address.
  The pairing code format becomes `vfy2.<base64url JSON>`, holding coordinator public
  key, endpoints, relay URL and a one-time enrollment secret, instead of
  `coolify_url`/`coolify_token`.
- **Manual:** type an IP or hostname.

---

## 4. The platform (replacing Coolify): control plane in the agent

### 4.1 Roles and crates

- One `vercelify-agent` binary, with its role chosen at runtime: `main` (control plane,
  coordinator, scheduler, builder) or `worker` (runtime only, may also build).
- New crate `crates/vercelify-platform`: store, migrations, scheduler, builder,
  deployer, proxy configuration, database templates, secrets. The agent mounts its
  routes. Models stay in `vercelify-core` so `ts-rs` keeps the cockpit types generated.
- Size budget: the agent is about 3 MB. SQLite (`rusqlite` bundled), ChaCha20-Poly1305,
  Ed25519 and the netlink code add an estimated 2–3 MB. Target ≤ 7 MB static musl. CI
  fails above 8 MB.

### 4.2 State: SQLite

- `/var/lib/vercelify/platform.db`, WAL mode, `synchronous=NORMAL`, one writer task,
  migrations embedded and numbered (`platform/migrations/NNNN_*.sql`), checked on start.
- Tables: `projects`, `environments`, `apps`, `app_domains`, `env_vars`, `deployments`,
  `deployment_logs` (index; logs themselves are files), `previews`, `databases`,
  `database_backups`, `volumes`, `nodes`, `mesh_nodes`, `git_sources`, `deploy_hooks`,
  `scheduled_tasks`, `users`, `tokens` (moves from `tokens.json`), `audit`
  (moves from `audit.log`), `settings`.
- The existing JSON state files (`jobs.json`, `metrics.json`, sleep, autodeploy,
  projects) are migrated in on first start and kept as a backup copy.
- Backed up by the existing restic job (online backup through SQLite's backup API, not
  a file copy).

### 4.3 Secrets

- Master key `/etc/vercelify/master.key` (32 bytes, root 0600), created by the installer
  and included in the encrypted restic backup (as Coolify's `APP_KEY` is today).
- Environment variables, database passwords, git credentials and deploy-hook secrets are
  stored encrypted with XChaCha20-Poly1305. The values never appear in logs or the audit
  trail. API responses mask them unless the caller asks with `?reveal=true` and has the
  deployer role.

### 4.4 Builds

Pipeline, one job per deployment, logs streamed to a file and to `/v1/events`:

1. **Fetch:** `git` clone at the commit (shallow, sparse when watch paths are set),
   credentials from the git source, the same safe git settings `autodeploy.rs` uses.
2. **Detect (our "vfy pack"):** in this order: `Dockerfile` (or the configured path) →
   `docker-compose.yml` (compose app) → language detection that generates a Dockerfile:
   - Node: npm/pnpm/yarn/bun from the lockfile, Node version from `engines`/`.nvmrc`;
     Next.js (standalone output when possible), Nuxt, SvelteKit, Remix/React Router,
     Astro (static or server), Vite/CRA/static SPA (served by a tiny static server);
     plain `start` script otherwise
   - Python (pip/poetry/uv; Django, FastAPI, Flask), Go, Rust, Deno, Bun, Ruby
     (Rails), PHP (Laravel, via php-fpm + Caddy image), static `index.html`
   - The generated Dockerfile is shown in the build log and can be saved into the repo
   - `vercel.json` `buildCommand`, `installCommand`, `outputDirectory` are honoured
3. **Build:** `docker buildx build` with BuildKit cache mounts for package managers,
   build args from build-time variables, inside the `vercelify.slice` build cgroup
   (OOM priority 900, as today). Concurrency from the hardware profile (1 on 8 GB).
   Paused when the power guard says thermal pause. Multi-arch when the target node's
   architecture differs.
4. **Store:** tag `vfy/<app>:<deployment>`; push to the mesh registry
   (`host/lib/registry.sh`, now on the mesh) when the target is another node. Keep the
   5 newest images per app (today's rule).

Build servers: any node can be marked "builds here". A Pi 4 only pulls.

### 4.5 Deploys (zero downtime, rollbacks)

1. Start the new container next to the old one: labels `vercelify.app`,
   `vercelify.deployment`, `vercelify.kind`; networks `vfy-<project>` and `vfy-proxy`;
   memory/CPU limits; restart policy; volumes.
2. Wait for healthy: the configured health check (HTTP path or command) or, if none,
   the port accepting connections. Timeout configurable, default 3 minutes.
3. Switch the route: write the app's Traefik file pointing at the new container
   (atomic rename). Traefik reloads by itself.
4. Drain the old container (grace period, default 10 s), then stop and remove it.
5. Failure at any step: the new container goes, the old one keeps serving, the
   deployment is marked failed, an alert goes out.

- **Rollback** = deploy an older deployment's image, no build ("instant"). If its image
  was pruned, rebuild that commit ("rebuild"). `vfy rollback` and the cockpit keep working.
- **Automatic rollback** (unhealthy within 3 minutes after a deploy) moves from calling
  Coolify to calling our deployer.
- **Compose apps:** `docker compose -p vfy-<app>` with our labels, networks and
  limits injected through an override file; one service is the routed one. Zero-downtime
  is best-effort for compose (documented).
- **Docker image apps:** skip the build.

### 4.6 Runtime reconciler

Desired state (database) vs actual state (Docker on each node), every 30 s and after
every change:

- restart crashed containers (with back-off), recreate missing ones, remove orphans
  carrying our labels
- respects sleeping apps (a stopped sleeping app is "desired asleep", not crashed)
- respects the power guard (frozen/paused containers are left alone)
- per node: the main server talks to each worker's agent over the mesh
  (`/v1/runtime/containers`, `/v1/runtime/apply`). The worker applies locally and
  reports back. If the main server is down, workers keep running what they have

### 4.7 Placement and scaling

- Nodes report CPU, free memory, architecture, disk tiers, temperature and battery
  (already in `SystemSnapshot`).
- Placement for a new app: the user's choice ("Run on"), or the node with the most free
  memory that matches the architecture and isn't on battery or hot. The same rule the
  cockpit shows today, now done on the server.
- Replicas: an app may run `n` replicas across nodes; Traefik load-balances over the mesh
  (each node's Traefik routes to local replicas; the main server's Traefik or the tunnel
  routes to nodes). Default 1, for memory.
- Control plane availability: a single main server, with a **warm standby**. The
  database is shipped to a chosen worker every minute (SQLite backup API over the mesh).
  `sudo vercelify promote` on the worker makes it the main server: coordinator, platform,
  and tunnel credentials. A cluster with Raft consensus is a non-goal: it needs three
  always-on machines, which isn't the target hardware.

### 4.8 Routing and TLS (Traefik, run by us)

- Container `vercelify-proxy` (pinned Traefik), static config in
  `/etc/vercelify/proxy/traefik.yml`, file provider in
  `/var/lib/vercelify/proxy/dynamic/`. Prometheus metrics always on (sleeping apps no
  longer patch Coolify's proxy configuration).
- `routing.rs` becomes the only router generator: the app's base route, then the
  `vercel.json` routes (today they sit above Coolify's routes at priorities
  5000–50000; that layering stays and Coolify's base route becomes ours), the
  wake route at priority 1, HTTP Basic Auth (previously Coolify's).
- HTTPS: Traefik ACME, HTTP-01 for direct mode, DNS-01 (Cloudflare) for a wildcard;
  tunnel mode serves HTTP behind Cloudflare as today. Certificate status per domain
  (`/v1/domains`), shown in AppDomains.
- Domain modes (`vercelify domain …`) write our settings instead of Coolify's
  `server_settings.wildcard_domain`. Preview template stays `pr-<n>-<app>.<domain>`.

### 4.9 Previews

`autodeploy.rs` already does webhook-free previews (poll, forks never built, per-app cap,
GitHub status + comment). It switches from Coolify's preview endpoints to our deployer:
a preview is a deployment with `pull_request_id`, its own container, route and
preview-only variables, removed when the PR closes.

### 4.10 Git sources and webhooks

- **Public repos:** no credentials.
- **Private repos:** per-app deploy key (Ed25519, generated by the agent, the public key
  shown for the user to add) or a fine-grained token per git source. GitHub, GitLab,
  Gitea/Forgejo and plain git.
- **Webhooks (optional, faster than polling):** `POST /v1/webhooks/github|gitlab|gitea`
  with signature checks, reachable only through the tunnel. Polling stays the default
  because it works behind CGNAT.
- **GitHub App** (manifest flow, installation tokens with RS256 JWT): later milestone M9,
  for orgs that won't use tokens.
- **Deploy hooks:** `POST /v1/hooks/deploy/<secret>` (unauthenticated, secret in the URL,
  rate-limited), replacing Coolify's deploy webhook for CI.

### 4.11 Databases

- One-click PostgreSQL, MySQL, MariaDB, MongoDB, Redis/Valkey, KeyDB, Dragonfly,
  ClickHouse. Each is a template (pinned image, port, data path, health command,
  generated credentials, internal/public URL format).
- Volumes on the SSD or HDD tier; moving between tiers uses today's move logic, now on
  our own volume table instead of `local_persistent_volumes`.
- Public port: bound to the mesh IP by default, to all interfaces only on request
  (firewall rule added and removed with it).
- Backups: scheduled logical dumps (`pg_dump`, `mysqldump`, `mongodump`, `redis-cli
  --rdb`) into restic, restore from the app. "Create Postgres/Redis for this app" stays
  and injects `DATABASE_URL`/`REDIS_URL`.

### 4.12 Users, roles, tokens, audit

- Device tokens (`vfyd_…`) stay, moved into the database, with a role: **owner** (all),
  **deployer** (apps, deploys, env, logs), **viewer** (read-only). Optional per-project
  scope. Coolify teams map to this.
- Audit log of every non-GET call stays, in the database, with the same `/v1/audit`.

### 4.13 Everything else Coolify did for us

| Coolify feature we used | Replacement |
| --- | --- |
| Deployment and runtime logs | Build log files with retention (last 20 per app), runtime via `docker logs` (as today), live in the cockpit and `vfy logs` |
| Scheduled tasks (command in a container) | `scheduled_tasks` table, run by the existing cron engine (`cron.rs`) via `docker exec` |
| Web terminal (`:6001/6002`) | `GET /v1/apps/{id}/terminal` WebSocket → `docker exec -it`; xterm.js loaded on demand in the cockpit |
| Shared / build-time / preview variables | `env_vars` with scope (project/app), build-time flag, preview flag |
| System variables (`COOLIFY_URL`, `SOURCE_COMMIT`…) | `VERCELIFY_URL`, `VERCELIFY_GIT_COMMIT_SHA`, `VERCELIFY_GIT_BRANCH`, `VERCELIFY_ENV`, `VERCELIFY_DEPLOYMENT_ID`, plus `SOURCE_COMMIT` and `COOLIFY_*` aliases for apps migrated from Coolify |
| Docker cleanup | Disk cleanup at 90% (today) + build cache pruning, keeping 5 images per app |
| Health checks, restart policies | Section 4.5/4.6 |
| Watch paths | Field on the app (today read from Coolify) |
| Resource limits | Memory/CPU per app, defaults from the hardware profile |
| Service catalogue (Plausible, Umami, MinIO…) | Compose templates in `platform/templates/`, starting with the ones our docs point to (MinIO, Plausible, Umami, Unleash) |

### 4.14 API

New routes under `/v1` (capability `platform`):

```
/v1/projects  /v1/projects/{id}/environments
/v1/apps  /v1/apps/{id}  /v1/apps/{id}/deploy  /v1/apps/{id}/rollback
/v1/apps/{id}/env  /v1/apps/{id}/domains  /v1/apps/{id}/previews  /v1/apps/{id}/logs
/v1/apps/{id}/terminal  /v1/apps/{id}/{start|stop|restart}
/v1/deployments  /v1/deployments/{id}  /v1/deployments/{id}/logs  /v1/deployments/{id}/cancel
/v1/databases  /v1/databases/{id}  /v1/databases/{id}/backups
/v1/volumes  (replaces /v1/coolify/volumes)
/v1/nodes  /v1/git-sources  /v1/hooks/deploy/{secret}  /v1/webhooks/{provider}
/v1/runtime/*  (server → server only)
/v1/mesh/enroll  /v1/mesh/map  /v1/mesh/nodes  /v1/mesh/status  (capability `mesh`)
```

Removed when migration is done: `/v1/coolify/*`, `/v1/tailscale`, `/v1/ssh/authorized-keys`.
`/v1/health` keeps reporting capabilities, so a cockpit can manage old and new servers
during the transition.

---

## 5. Changes across the repo

| Area | Change |
| --- | --- |
| `crates/vercelify-core` | New models (App, Deployment, Database, Node, MeshNode, MeshStatus, EnvVar…), exported with `ts-rs`. `tailscale.rs` stays until M10 for the migration, then goes. `Pairing` v2 |
| `crates/vercelify-mesh` (new) | WireGuard netlink control, boringtun/smoltcp userspace, STUN, relay client/server, DNS responder, mDNS, map client |
| `crates/vercelify-platform` (new) | Store, migrations, secrets, builder, detector, deployer, reconciler, scheduler, templates |
| `agent/` | Mount platform and mesh routes; `autodeploy.rs`, `sleep.rs`, `projects.rs`, `routing.rs`, `doctor.rs`, `watch.rs`, `ssh.rs`, `coolify.rs` move off Coolify; `coolify.rs` and `ssh.rs` deleted at the end; binds to the mesh IP |
| `cockpit/src-tauri` | Remove `coolify_request` and `open_coolify`; mesh client (talks to the local `vercelify-mesh` SOCKS5); discovery via mDNS + mesh map instead of `tailscale status`; `local_tailscale` → `local_mesh` |
| `cockpit/src` | `lib/api.ts` loses `makeCoolify`; `types/coolify.ts` deleted; Apps view gets **New app** (repo, branch, build pack, domain, variables, Run on), **New database**, terminal; Servers view shows mesh path per peer, a **Devices** list with revoke; Health's Tailscale card becomes Mesh; Onboarding without Tailscale; demo data (`lib/demo.ts`) and e2e specs updated, `data-testid`s kept |
| `cli/vfy` | Reaches servers through the mesh SOCKS5 (or directly in system mode); same commands, plus `vfy create`, `vfy db`, `vfy mesh status`. Still bash 3.2 |
| `host/` | `setup-host.sh` stops installing Coolify and Tailscale; installs `vercelify-mesh`, the proxy and the master key; new units `vercelify-mesh.service`; `firewall.sh` on `vfy0`; `lib/coolify.sh` → `lib/platform.sh`, `lib/tailscale.sh` → `lib/mesh.sh`; `vercelify mesh …`, `vercelify promote`, `vercelify migrate`; bats tests for each |
| `install.sh`, `install-mac.sh` | No Tailscale; the Mac installs the mesh LaunchAgent (and, if asked, the system-mode daemon) |
| `website/`, `README.md`, `docs/` | Stack description; `COOLIFY.md` → `PLATFORM.md`; new `MESH.md`; `VERCEL-PARITY.md` "Coolify" rows rewritten; `TROUBLESHOOTING.md`; `CLAUDE.md` stack and conventions |
| CI | Coolify contract test replaced by a platform contract test; new mesh test; binary-size gate |

---

## 6. Migration for existing installs

`sudo vercelify migrate` (idempotent, resumable, dry run first by default):

1. **Mesh first, next to Tailscale.** Install `vercelify-mesh`, enroll this server and,
   over the existing tailnet, every paired device and worker. Both networks work.
2. **Read Coolify's database**: projects, environments, applications (source, branch,
   build pack, ports, domains, health check, watch paths, limits), environment
   variables (decrypted with Coolify's Laravel `APP_KEY`: AES-256-CBC + HMAC payloads),
   previews, databases (credentials, images, volumes), persistent volumes, scheduled
   tasks, servers (workers).
3. **Write our database** and show the diff. Nothing is changed yet.
4. **Switch over per app**, one at a time: start our container from the same image
   Coolify built, health check, move the route to our Traefik files, stop Coolify's
   container. Databases keep their volume (same data directory), stopped and started
   once.
5. **Stop the Coolify stack** (containers kept, not deleted, for 14 days),
   `vercelify migrate --rollback` brings it back. `vercelify migrate --finish` removes
   it afterwards and frees the RAM.
6. **Tailscale off** only after every device and worker has shown a mesh connection
   for 24 hours. `tailscale down`, optional uninstall.

The cockpit shows the migration as a job with steps and a per-app result.

---

## 7. Testing

| Layer | What |
| --- | --- |
| Unit (Rust) | Store migrations up/down, secrets round-trip, detector against fixture repos (one per framework in `crates/vercelify-platform/tests/fixtures`), deploy state machine with a fake Docker, reconciler decisions, scheduler placement, pairing v2, mesh map diffing, STUN/relay framing, DNS answers, ACL generation, Laravel decryption against Coolify fixtures |
| Platform contract test (`host/tests/contract/platform.sh`, real Docker + Traefik) | Create app from a sample repo → build → deploy → HTTP 200; push → zero-downtime redeploy (no failed request during the switch); rollback instant; preview up and torn down; Postgres + Redis created and reachable; volume tier move; sleep/wake; `vercel.json` routing (today's routing test); deploy hook; compose app |
| Mesh test (`host/tests/contract/mesh.sh`, network namespaces) | Three namespaces behind simulated NAT (iptables MASQUERADE, one symmetric): LAN path, direct path after hole punching, relay fallback; revoke drops traffic; coordinator restart keeps traffic; userspace mode (boringtun/smoltcp) talking to kernel WG |
| Migration test | Real Coolify 4.4.6 with two apps, a database and a preview → `vercelify migrate` → everything serves from our stack → `--rollback` restores Coolify |
| Cockpit | Vitest for the new API layer and views; Playwright demo flows (`e2e-demo`) updated; desktop e2e |
| Hosts | bats for new CLI commands; installer e2e in the systemd container; arm64 under QEMU; real Pi run before release |
| Performance gates | Agent ≤ 8 MB, idle RSS ≤ 40 MB, mesh idle CPU near zero on a Pi 4, cockpit polling unchanged |

---

## 8. Milestones

Each milestone ends green (`cargo test`, clippy, fmt, bats, shellcheck, cockpit
typecheck/tests/build) and leaves the product working. Coolify and Tailscale stay the
default until M10, so nothing is half-replaced at any point.

| # | Milestone | Contents | Done when |
| --- | --- | --- | --- |
| M0 | Decisions + scaffolding | `CLAUDE.md`/docs stack update, the two new crates, capability flags, SQLite store + migrations + secrets, size gate | Store tests pass, agent builds under budget |
| M1 | Models + API skeleton | Core models with `ts-rs`, `/v1/projects`, `/v1/apps`, `/v1/env`, `/v1/volumes` CRUD, roles on tokens | API tests pass, TS types generated |
| M2 | Builder | Git fetch, detector (Node + static first, then Python/Go/Rust/Deno/Bun/Ruby/PHP), Dockerfile/compose/image, BuildKit, queue, logs, thermal pause | Fixture repos build in the contract test |
| M3 | Deployer + proxy | Our Traefik container, base routes, zero-downtime deploy, rollback, auto-rollback, health checks, reconciler (single node) | Contract test: deploy, redeploy without errors, rollback |
| M4 | Existing features off Coolify | autodeploy + previews, sleep, `vercel.json` projects, crons, domains + TLS, doctor, watch, uptime, alerts, scheduled tasks, terminal, deploy hooks | Every current feature runs with `platform` and no Coolify |
| M5 | Databases | Templates, credentials, tier moves, backups/restore, public ports | Contract test creates, backs up and restores each kind |
| M6 | Cockpit on the new API | Remove Coolify paths behind the capability, New app / New database / terminal, demo data, e2e | Demo e2e passes; a Coolify-free server is fully usable from the app |
| M7 | Mesh: servers | `vercelify-mesh` on Linux, kernel WG over netlink, coordinator, enrollment, peer maps, DNS, ACL firewall, rotation, revocation | Mesh test: LAN + direct paths, revoke |
| M8 | Mesh: NAT + Mac | STUN, hole punching, relay (tunnel/public), userspace boringtun+smoltcp, SOCKS5 + forwards, LaunchAgent, system mode, mDNS discovery, pairing v2, cockpit and `vfy` over the mesh, display over the mesh | Mesh test incl. symmetric NAT → relay; cockpit talks to a server with Tailscale off |
| M9 | Multi-node | Worker runtime API, placement, mesh registry, multi-arch builds, replicas, tunnel routes for workers, warm standby + `promote`, GitHub App + webhooks | Two-node contract test; promote test |
| M10 | Migration + switch default | `vercelify migrate` (+rollback/finish), installer without Coolify/Tailscale, docs/website/README, remove `coolify.rs`, `ssh.rs`, `tailscale.rs`, `types/coolify.ts` | Migration test passes; fresh install has neither |

On doing this "in one shot in this session": I will work through the milestones in
order and commit each one when it's green, so the branch is always in a usable state.
Some things can't be verified from this container: real macOS behaviour (utun,
LaunchAgent, notarisation), real CGNAT networks, and Raspberry Pi hardware. Those
paths get unit and namespace tests here and need one run on real devices before a
release. That limit is the same today for the Mac app and the Pi.

## 9. Decisions to confirm before M0

1. **Keep Traefik, Docker/BuildKit and WireGuard as dependencies** (section 2).
   Recommended: yes.
2. **Relay story** (section 3.6): main server through the Cloudflare tunnel, a public IP,
   or any public node. No hosted relay of ours, since that would be a recurring cost.
   Recommended: yes, and document the LAN-only limitation for CGNAT without a domain.
3. **Mac default = app mode (userspace, no admin password)**, system mode optional.
   Recommended: yes.
4. **Single main server + warm standby**, no consensus cluster. Recommended: yes.
5. **Mesh address range** `10.77.0.0/16` + `fd76:6679:6d65::/48`. Change it if your LAN
   already uses `10.77.x.x`.
