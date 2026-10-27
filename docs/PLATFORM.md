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
- [8. Coming from Coolify](#8-coming-from-coolify)

## What the installer set up

| Piece | Where | Why |
| --- | --- | --- |
| The engine | in `vercelify-agent` (`engine = "platform"` in `/etc/vercelify/agent.toml`), state in `/var/lib/vercelify/platform.db` | Builds, deploys, previews, rollbacks, databases |
| Master key | `/var/lib/vercelify/master.key` (0600) | Seals every stored secret (variables, database passwords, Git tokens). It is part of every backup: without it, sealed values can't be read. |
| Proxy | the `vercelify-proxy` container (Traefik, pinned and contract-tested) on ports 80 and 443 | Routes `<app>.<domain>` to the app's containers, gets certificates, applies `vercel.json` rules |
| Builder | the `buildx_buildkit_vercelify0` container (BuildKit) | Builds images with a cache; the power guard pauses it when the laptop runs hot |
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
4. **Previews**: each pull request gets its own deployment at `pr-<n>-<app>.<domain>`,
   deleted when the pull request is merged or closed. Forks are never built.
5. **Rollbacks**: **Deployments → Roll back** (or `vfy rollback APP COMMIT|#N`) starts the
   earlier image again at once, without rebuilding.

Apps can **sleep** when idle to free memory (`sudo vercelify sleep on APP 30`, or the app's
**Sleep** card); the next visit wakes them with a short "waking up" page.

## 4. Environment variables

- Variables live on the app (**Environment**), on its project, or on an environment
  (`production`, `staging`…); the app's own win. Mark the ones needed while building
  (`NEXT_PUBLIC_*`) as **build** variables.
- Values are secret by default: stored sealed, shown only when you reveal them.
- **Preview** overrides apply to pull request previews only (for example a separate
  database URL).
- A database's URL is referenced as `${{db.<name>.url}}`, filled in at deploy time, so
  rotating its password needs only a redeploy.
- From the terminal: `vfy env pull APP`, `vfy env set APP KEY=VALUE`, `vfy env push APP`.

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
- All containers share a memory-capped slice below the machine's RAM, so a leaking app
  can't starve the agent, the network or SSH.
- Let rarely used apps **sleep**.
- The engine cleans old images and build caches by itself, keeping what rollbacks need.
- Turn on alerts (**Settings → Alerts** in the app) to hear about failed deployments,
  backups and battery or thermal events.

## 8. Coming from Coolify

Servers installed before Vercelify had its own engine run Coolify and Tailscale. They
keep working as they are until you move them:

```bash
sudo vercelify migrate            # shows what would happen; changes nothing
sudo vercelify migrate --apply    # moves apps, databases, data and settings
```

It takes over apps, previews, variables, domains, databases (with their data) and
services, stops Coolify (kept on disk until `--finish`), and can be undone with `sudo
vercelify migrate --rollback`. When everything works: `sudo vercelify migrate --finish`
removes Coolify, and `sudo vercelify migrate --tailscale-off` stops listening on
Tailscale once your devices are on the mesh. Updates never move a server by
themselves.
