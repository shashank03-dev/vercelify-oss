# Running apps on Vercelify

Vercelify builds and runs your apps with its own engine: it lives inside the agent
on your main server, keeps its state in one SQLite file, and drives Docker,
BuildKit and a Traefik proxy. This guide covers what the installer set up and the
things you choose yourself: domains, Git access, databases, storage and extra
servers. The engineering details are in [the design](design/SYSTEM-DESIGN.md).

- [What the installer set up](#what-the-installer-set-up)
- [1. Domains and HTTPS](#1-domains-and-https)
- [2. Git: private repositories and instant deploys](#2-git-private-repositories-and-instant-deploys)
- [3. Apps, previews and rollbacks](#3-apps-previews-and-rollbacks)
- [4. Environment variables](#4-environment-variables)
- [5. Databases and storage on the HDD](#5-databases-and-storage-on-the-hdd)
- [6. More servers: workers and a standby](#6-more-servers-workers-and-a-standby)
- [7. Settings for an 8 GB laptop](#7-settings-for-an-8-gb-laptop)

## What the installer set up

| Piece | Where | Why |
| --- | --- | --- |
| The engine | in `vercelify-agent` (`engine = "platform"` in `/etc/vercelify/agent.toml`), state in `/var/lib/vercelify/platform.db` | Builds, deploys, previews, rollbacks, databases |
| Master key | `/var/lib/vercelify/master.key` (0600) | Seals every stored secret (variables, database passwords, Git tokens). It is part of every backup: without it, sealed values can't be read. |
| Proxy | the `vercelify-proxy` container (Traefik, pinned and contract-tested) on ports 80 and 443 | Routes `<app>.<domain>` to the app's containers, gets certificates, applies `vercel.json` rules |
| Builder | the `buildx_buildkit_vercelify0` container (BuildKit) | Builds images with a retained cache; stops when idle; the power guard pauses it when hot |
| Network | `vercelify-mesh` (WireGuard on `vfy0`, `10.77.0.0/16`); the main server coordinates it as `10.77.0.1` | Your Mac and other servers reach the agent privately, from anywhere. See [MESH.md](MESH.md). |
| App domain | `http://<app>.10.77.0.1.sslip.io` until you set a domain | Works at once on every device on your network |
| State copy | `/var/lib/vercelify/platform.db.bak`, every 5 minutes and before upgrades | What backups, `vercelify rollback` and `vercelify restore-state` use; the live file is never copied while it is written |
| Firewall | the agent, Ollama and the display stream are reachable only over the mesh; containers can't reach your other machines | Your control plane is never on the internet |

Run `sudo vercelify doctor` at any time to check all of it.

## 1. Domains and HTTPS

Pick one of three modes. Switch at any time with `sudo vercelify domain …`; it sets
the domain for **new** apps (change existing ones in the app's **Domains** card).

### A. No domain (private, zero setup)

Apps get `http://<app>.10.77.0.1.sslip.io`. [sslip.io](https://sslip.io) resolves that
name to the server's mesh address, so every device on your network can open it and
nothing is public. Good for staging and personal tools.

### B. Your domain on Cloudflare (recommended for public apps)

```bash
sudo vercelify domain example.com --cloudflare-token <TOKEN>
```

Create the token at <https://dash.cloudflare.com/profile/api-tokens> → **Create Custom
Token** with:

- Account › **Cloudflare Tunnel** › Edit
- Zone › **DNS** › Edit
- Zone › **Zone** › Read (zone resources: your domain)

What happens:

1. A tunnel named `vercelify-<hostname>` is created (or reused).
2. Its routes send `*.example.com` and `example.com` to the proxy on `localhost:80`, and
   `mesh.example.com` to the mesh's control listener, so your devices can join and reach
   the server from outside your home network.
3. Proxied CNAMEs `*.example.com` and `example.com` → `<tunnel-id>.cfargotunnel.com` are
   created. **Existing A/AAAA records are left alone** (add `--force-dns` to replace
   them); MX/TXT records (your e-mail) are never touched.
4. `cloudflared` runs as a hardened systemd service.
5. New apps get `https://<app>.example.com`.

HTTPS ends at Cloudflare's edge with its free certificate for `example.com` and
`*.example.com`; the proxy serves plain HTTP to the tunnel. Turn on **SSL/TLS → Edge
Certificates → Always Use HTTPS** in Cloudflare. Apps on other servers are served
through the main server's proxy over the mesh, so the tunnel needs no per-app routes.

No router port forwarding is needed, and it works behind CGNAT, mobile hotspots and
university networks.

### C. Your domain pointing at your public IP

```bash
sudo vercelify domain example.com
```

At your DNS provider create `A example.com → <public IP>` and `A *.example.com → <public
IP>`, then forward TCP 80 and 443 on your router to the server. The proxy gets a Let's
Encrypt certificate for each app (HTTP-01 challenge).

**Previews** get `pr-<n>-<app>.<domain>`: one label deep, so `*.example.com` DNS and the
free Cloudflare certificate cover them.

## 2. Git: private repositories and instant deploys

Public repositories need nothing: paste the URL in **New app**. The engine checks the
branch with `git ls-remote` (every minute; works behind CGNAT) and deploys new commits.

**Private repositories.** Either:

- **A GitHub App** (best for organisations): in **New app**, paste a GitHub URL and pick
  *Private GitHub repositories: connect a GitHub App*, or run `sudo vercelify github-app
  [--org ORG]`. GitHub creates the app from a manifest; its key
  and secrets are stored sealed on the server, and every fetch uses a short-lived
  installation token. Nothing to copy by hand.
- **A token or a deploy key** (GitHub, GitLab, Gitea/Forgejo, any Git host):
  `sudo vercelify git-source add NAME --provider github` asks for the token (or reads it
  from stdin) and stores it sealed; `--deploy-key` instead prints a read-only key to add
  to the repository. Then pick it under **New app → Credentials**.

**Instant deploys.** With a public URL (mode B, or your own tunnel), push events can
reach the server: `sudo vercelify webhook github` (or `gitlab`, `gitea`) makes a secret
and prints the URL to add in the repository's webhook settings. A webhook only makes the
server look at the repository now; the commit to deploy still comes from Git itself. A
GitHub App sets its webhook up by itself.

**From CI**, any app can have a deploy hook (the app's **Deploy hooks** card): a secret
URL that queues a deployment when called.

## 3. Apps, previews and rollbacks

1. **New app** in the Mac app: a Git repository (with its branch), a Docker image, or a
   service from the catalogue (Umami, Plausible, MinIO, Uptime Kuma, Vaultwarden…).
2. The build pack detects Next.js, Vite, Nuxt, Astro, SvelteKit, Remix, Express, Go,
   Python, Rust, Ruby, PHP, Deno and Bun; a `Dockerfile` or a compose file is used when
   the repository has one. `buildCommand`, `installCommand`, `outputDirectory` and
   `framework` in `vercel.json` are honoured, and the rest of `vercel.json` (redirects,
   rewrites, headers, firewall, crons) is applied by the proxy: see
   [PROJECT-CONFIG.md](PROJECT-CONFIG.md).
3. Every deployment starts a new container, waits until it answers its health check, and
   only then switches traffic; the old one keeps serving while it drains. A deployment
   that fails its health check never takes traffic.
4. **Previews**: each pull request gets a private deployment at
   `http://pr-<n>-<app>.<server>.vfy`, accessible through the Vercelify mesh.
   Choose **Public** deliberately to use `pr-<n>-<app>.<wildcard-domain>` instead.
   Closing or merging the PR removes its containers, routes and owned resources.
   Forks are never built. Docker 28+ is required; Compose previews need 2.24.4+.
5. **Ports**: a Git app listens where its framework or Dockerfile says. An image app
   with no port set uses the one TCP port its image `EXPOSE`s; if it exposes none or
   several, the deployment stops at once and asks you to set the port (Settings → Port,
   or `"runtime": {"port": 8080}` in the API).
6. **Rollbacks**: **Deployments → Roll back** (or `vfy rollback APP COMMIT|#N`) starts the
   earlier image again at once, without rebuilding.

Apps can **sleep** when idle to free memory (`sudo vercelify sleep on APP 30`, or the app's
**Sleep** card); the next visit wakes them with a short "waking up" page.

### Verified deploy recovery

In **Settings → Deploy recovery**, enable automatic image rollback only after
confirming older images remain compatible with the app's current data and migrations.
The default is off. A production version is observed for three minutes, with a
ten-second startup grace and three consecutive health failures by default. Two
additional restarts also trigger recovery. The original window survives an agent
restart. A newer deployment, stop/sleep or changed runtime, variables, storage or
placement cancels stale recovery; a queued rollback cannot undo a later stop.

Recovery needs a matching prior configuration snapshot and retained immutable image
(a generated build tag, image ID or pinned digest). Older deployments without that
snapshot and mutable external tags need manual recovery. Compose recovery is manual.
A rollback request is shown as pending until the old image passes runtime health and
local ingress. Missing images, failed rollback or verification timeout remain visible
as needing attention. This does not restore database/volume writes, reverse migrations
or prove external DNS/TLS/ISP availability. Protected/custom ingress may need manual
verification. A minimum two-second route propagation allowance applies to old-version
retirement even with zero requested drain; the normal default drain is ten seconds.

Host updates save control state and, when the installed agent supports it, record
local runtime/ingress readiness and intentionally stopped/sleeping apps. Checks never
wake those apps. A healthy control API alone is insufficient for a qualified update.
Legacy agents without the capability report workload verification as unmeasured.
An intentional workload change during update preserves current state for review.
If automatic binary rollback would require a schema rewind, the candidate and later
control writes are retained. Repair it, or explicitly run
`sudo vercelify rollback --restore-control-state` to rewind control metadata to the
update snapshot. That flag discards subsequent control changes; app volume data stays
unchanged. Keep the reported snapshot/replaced-state files for recovery.

### Fresh PostgreSQL for PR previews

For trusted same-repository PRs, enable **Previews → Fresh PostgreSQL** and save
the runtime variable (default `DATABASE_URL`), migration command and optional
synthetic-seed command. Commands run with `sh -ec` in the built app image before
publication; the image must contain the required shell and migration tools.
Failure leaves the previous deployment serving. No production data is copied.

This opt-in uses a separate PostgreSQL 17 service per app, shared by at most three
active PRs. It defaults to 256 MiB and one CPU, admitted through the host resource
budget. Each PR has a fresh database and restricted login, separate from production
and other PR databases. Keep the application's pool at three connections or fewer:
the role limit is four, leaving room for a migration. Runtime credentials never
enter the image build or resolved-variable responses. Production database references
in other preview variables remain blocked.

Built-in database previews currently require the main node and a non-Compose app.
The cluster is for trusted previews; shared process limits do not provide hostile
tenant resource isolation. Data persists across new commits and app sleep while
the PR is open. Closing, disabling the option or deleting the app schedules owned
cleanup; failures remain visible and retry after restart. Reopening waits for cleanup
and creates a new database and credential. The last close removes the owned service
and volume. Preview data is disposable: no production-copy, backup-seed or independent
per-database sleep is offered. Redeploy migrations/seed after preview volume loss.

The health capability `preview-databases` gates the settings UI. The authenticated
`GET /v1/platform/apps/{id}/preview-databases` endpoint returns lifecycle metadata
and redacted errors, never passwords or connection URLs.

### Build on your computer or in CI

Install the shared helper on the computer that builds: the Mac installer accepts
`--with-builds` (or `--from-source --with-builds` before helper release assets exist).
From a checkout, `cargo install --path crates/vercelify-platform --bin vercelify-pack`
installs the same detector/build logic on macOS or Linux. You also need Git,
Docker and its Buildx plugin, with a running compatible Linux runtime. Select an
existing runtime with `DOCKER_CONTEXT`/`DOCKER_HOST`; Vercelify does not switch it
or mount your home directory or SSH agent into builds. If no runtime is available,
start one first; the existing Vercelify environment is managed from Servers.

```bash
vfy deploy blog --build-here --commit FULL_COMMIT_SHA
vfy deploy blog --image ghcr.io/me/blog@sha256:DIGEST --commit FULL_COMMIT_SHA
```

The first command uses a fresh exact-commit checkout, honors `.dockerignore`,
builds for the app's selected server architecture, then uploads through the
agent's authenticated API. An ARM computer targeting x86 explicitly builds
`linux/amd64`; emulation must be supported by the selected runtime and can be
slower. The second command pulls an immutable CI image locally and hands it off;
use the OCI revision label instead of `--commit` if it contains the full source
SHA. Image apps can also deploy pinned registry references directly.

Server-tool control keys (PATH/HOME, loader/shell controls and names starting with
DOCKER_, BUILDX_, BUILDKIT_, COMPOSE_ or GIT_) are reserved. Use an app-specific
name and map it inside your container when needed; deployment errors name the key,
never its value. Existing containers continue serving.

Build-marked variables are supplied only to that build. Secret values use BuildKit
secret mounts and are redacted from helper logs. A repository Dockerfile reads
secrets from `RUN --mount=type=secret,id=env . /run/secrets/env; …`; only nonsecret
build values become ARGs. A malicious build can still exfiltrate an authorized
secret. Production DB references marked for builds need a database mesh port and
a runtime that can reach that mesh address; runtime-only references do not.

The serving host receives a complete, verified image before deployment. Failed,
cancelled, expired, revoked or superseded handoffs leave the current app serving.
A missing imported image fails with a computer/CI retry instruction. The local
builder cache is per app and retained; the CLI prints its name and removal command.
A retained builder must match the selected Docker endpoint; after switching runtimes,
remove that app builder and retry to create it in the selected context.
Generated recipes use a per-deployment build argument to rerun steps with current
build variables while retaining package cache mounts. Repository Dockerfiles use
`--no-cache` conservatively; their cache mounts reset for each handoff.

For a runtime-only serving host, set `build_concurrency = 0` in
`/etc/vercelify/agent.toml` and restart the agent. Normal Git/Compose deploys then
remain queued with a no-permitted-builder reason; cancel them or retry explicitly
from a computer/CI. No disconnected computer silently starts a target-side build.
Small-RAM support still depends on runtime startup/pull/rollout measurements.

The first release supports explicit CLI handoffs, not automatic desktop scheduling.
The upload cap is 3 GiB with bounded metadata; save/upload/load consumes disk and
I/O even though compilation runs elsewhere. Leases renew every 45 seconds and
expire after five minutes without renewal (90-minute total limit). A sleeping Mac,
interrupted connection or agent restart requires a new build attempt. Upgrade
workers together with the main server: their registry reader now authenticates
with a server node token; old workers cannot pull through the retired raw mesh port.

## 4. Environment variables

- Variables live on the app (**Environment**), on its project, or on an environment
  (`production`, `staging`…); the app's own win. Mark the ones needed while building
  (`NEXT_PUBLIC_*`) as **build** variables.
- Values are secret by default: stored sealed, shown only when you reveal them. A secret
  value printed by a build or by the app shows as `[redacted]` in its logs (a safety
  net: code that is given a secret can still send it elsewhere).
- Normal values are **production-only** until deliberately shared with previews,
  even when they are marked nonsecret. The Variables tab shows scope, policy and
  the keys previews are missing. **Preview** overrides beat shared normal values
  at every level; among preview overrides app wins over environment, then project.
- Platform `${{db.…}}` references are refused in previews, including preview-only
  variables: those references currently identify production databases. Runtime
  previews have no outbound access to the host, production networks or internet;
  keep required services inside their preview Compose stack. Isolated platform
  PostgreSQL previews are a later milestone, not part of this change.
- On upgrade to this policy, existing previews are retired before new work starts.
  Review their Variables settings, then redeploy the pull requests.
- A database's URL is referenced as `${{db.<name>.url}}`, filled in at deploy time, so
  rotating its password needs only a redeploy.
- From the terminal: `vfy env pull APP`, `vfy env set APP KEY=VALUE`, `vfy env push APP`.
  `vfy env pull` writes what production deployments get: the project's, environment's
  and app's variables merged (the app's win), without preview overrides; database
  references stay as `${{db.…}}`. Deployer or owner tokens can change variables at every
  level; creating or deleting projects and environments stays with owners.

## 5. Databases and storage on the HDD

Create databases with one click (**Databases → New database**): PostgreSQL, MySQL,
MariaDB, MongoDB, Redis (Valkey), KeyDB, Dragonfly or ClickHouse, with generated
credentials and a URL to link into apps. Each can expose a port on the mesh, so tools on
your Mac (TablePlus, `psql`) connect directly.

**Storage tiers.** With a second disk mounted at `/mnt/storage`, the **Storage** page
moves a database's or an app's data between the SSD and the HDD: it stops the database,
copies the data with owners kept, and starts it on the new tier. Keep latency-sensitive
data (Redis, caches) on the SSD and big data on the HDD.

**Backups.** `sudo vercelify backup setup` turns on encrypted nightly backups (restic) to
the HDD, another server or any S3 bucket. They hold the engine's state, the master key,
dumps of every database and the apps' volumes. A database's **Backups** tab also runs
and restores its own dumps. More in [BACKUPS.md](BACKUPS.md).

### Check a backup with an isolated restore

For PostgreSQL 17, open the database's **Restore drills** card after taking a new
backup. Select its snapshot and optionally enter a read query with a result known
for that backup (for example its migration marker). **Run restore drill** creates
a temporary isolated database, checks relations/indexes, extensions and queries,
then removes its container, volume and network. Production keeps serving throughout.
An interrupted, failed or cleanup-pending job is visible and is never labelled a
successful verified restore. Cancel requests also wait for cleanup.

**Also read all repository data** adds restic's full-data integrity check, with
extra time/bandwidth. Otherwise repository metadata is checked and the selected
dump is still fully read/restored. These are separate results. A successful check
covers that snapshot and the displayed assertions; it does not prove every backup
or guarantee a recovery objective. The displayed backup age and duration are observed.
Older backups without a recorded manifest require a new supported backup.

Owners can enable a five-field UTC schedule, monthly by default (`0 3 1 * *`). It
starts disabled. Low battery, thermal/disk pressure or insufficient capacity defer
heavy work; the existing alerts surface overdue and persistently deferred checks.
The server serializes restores and resumes cleanup after restart. Other database
engines, PostgreSQL majors and remote-repository qualification remain outside the
first tested scope. A drill using the host key is labelled accordingly; losing the
machine still requires an independently retained recovery kit and encrypted backup.
The recovery-import API accepts supplied recovery settings with a fresh control
store. Same-VM copied-repository qualification is not a physical disaster-recovery test.

## 6. More servers: workers and a standby

A worker (another old laptop or a Raspberry Pi 4/5) runs apps the main server places on
it. In the app, **Add a server… → A worker for a main server** gives a one-line install
command with a single-use invite. On the command line instead:

```bash
# on the main server
sudo vercelify mesh invite
# on the new machine, with the token it printed
curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh |
  sudo bash -s -- --worker --join vfyj1.…
```

The worker joins the mesh and registers with the engine. A new app goes to the server
with the most memory to spare (or the one you pin it to in its **Where it runs** card);
the main server builds the images and keeps them in its own registry, which workers pull
from over the mesh (for a Pi, `sudo vercelify registry multiarch` lets an x86 server build
`linux/arm64` images). Visitors reach every app through the
main server's proxy, wherever it runs. Stateless apps can run several replicas across
servers and move when a server goes offline. Details for the Pi are in
[RASPBERRY-PI.md](RASPBERRY-PI.md).

**Warm standby.** `sudo vercelify standby <worker>` keeps a sealed copy of the main
server's state on a worker, refreshed every 5 minutes. If the main server dies, run
`sudo vercelify promote` on the standby to take over.

## 7. Settings for an 8 GB laptop

- One build at a time (the default on 8 GB or less): running apps stay responsive.
- The dedicated BuildKit container stops after ten idle minutes. Set
  `builder_idle_minutes` in `/etc/vercelify/agent.toml` and restart the agent to
  change this (`0` disables it). A check runs every 30 seconds; active builds
  and paused builders are skipped. The next build starts it again with its
  cache intact. The daemon's `docker` build driver is unaffected. Restarting
  the agent starts a fresh idle grace period.
- New deployments have enforced app budgets: 256 MiB and one CPU by default;
  the experimental 1 GiB runtime-only profile defaults to 192 MiB and refuses
  local builds/databases. Old and new instances both count during rollout.
  Configure `runtime.memory_mb`/`cpus` for your actual workload. Compose shares
  the declared app budget over one container per service, including sidecars.
- Admission uses fresh RAM, accepted host grants, existing caps and their unused
  growth, operation reservations and temporary disk. Pressure queues new work;
  production is left serving. Missing/stale readings defer work. Swap is not
  treated as more guaranteed RAM. Health's **Memory budget** card explains
  measured use, unknown readings, commitments, reservations and headroom.
  Older workers without capacity reports keep serving but need upgrading before
  receiving new work. Managed VMs also need an updated host manager with fresh
  grant and physical-memory reports.
- New PostgreSQL databases use a 256 MiB/one-CPU small preset (20 connections,
  32 MiB shared buffers, 2 MiB work memory and 32 MiB maintenance memory). Other
  new database templates use 512 MiB/one CPU. Existing databases keep their
  configuration. An explicit `PATCH /v1/platform/databases/{id}` with
  `{ "memory_mb": 512 }` requests a different cap; applying it passes admission
  and can restart that database. Cap exhaustion is possible; size for your load.
- The serving agent uses a bounded dedicated BuildKit builder (default 2048 MiB,
  one CPU), rather than the unbounded daemon build driver. Runtime-only hosts use
  `build_concurrency = 0` and a selected computer/CI (§4). Existing unlimited
  workloads are shown as estimates, without being stopped solely to lower caps.
- Let rarely used apps **sleep**.
- The engine cleans old images and build caches by itself, keeping what rollbacks need.
- Turn on alerts (**Settings → Alerts** in the app) to hear about failed deployments,
  backups and battery or thermal events.
