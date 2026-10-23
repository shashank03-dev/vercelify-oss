# Coolify configuration guide

The installer already configures most of this for you. This guide explains
what it did, and covers the steps that need a browser (GitHub App, workers).

- [What the installer configured](#what-the-installer-configured)
- [1. First login](#1-first-login)
- [2. Domains: wildcard DNS and HTTPS](#2-domains-wildcard-dns-and-https)
- [3. Connect GitHub (push-to-deploy + PR previews)](#3-connect-github-push-to-deploy--pr-previews)
- [4. Deploy your first app](#4-deploy-your-first-app)
- [5. Environments and environment variables](#5-environments-and-environment-variables)
- [6. Databases and storage on the HDD](#6-databases-and-storage-on-the-hdd)
- [7. Add a Raspberry Pi worker over Tailscale](#7-add-a-raspberry-pi-worker-over-tailscale)
- [8. Recommended settings for an 8 GB laptop](#8-recommended-settings-for-an-8-gb-laptop)

## What the installer configured

These were verified against Coolify 4.4:

| Setting | Value | Why |
| --- | --- | --- |
| Admin account | the e-mail you entered (default `admin@example.com`) with a generated password, printed at the end and stored in `/etc/vercelify/summary.txt` (mode 0600). Coolify checks the e-mail's domain in DNS, so made-up domains like `laptop.local` are replaced with `admin@example.com`. | No "first visitor becomes admin" window |
| API | enabled, plus a token for the cockpit (`/etc/vercelify/coolify.token`) | The cockpit's Apps tab uses it |
| Server wildcard domain | `http://<tailscale-ip>.sslip.io`, `http://<domain>` (tunnel) or `https://<domain>` (direct) | New apps get `<name>.<domain>` automatically |
| Preview URL template | `pr-{{pr_id}}-{{domain}}` | Gives `pr-42-app.example.com`, **one** label deep, so `*.example.com` DNS and the free Cloudflare certificate cover previews. Coolify's default `42.app.example.com` needs a second-level wildcard that neither DNS nor free certificates provide. |
| Concurrent builds | 1 | Keeps running apps responsive on 8 GB of RAM |
| SSH user of `localhost` | `root` | Works around an installer edge case where an empty `$USER` breaks every deployment |
| Coolify backups | `/data/coolify/backups` bind-mounted to `/mnt/storage/backups/coolify` | Backups on the big disk |
| Dashboard exposure | ports 8000/6001/6002/8080 reachable only via `tailscale0` | Your control plane is never on the public internet |

Run `sudo vercelify doctor` at any time to re-check all of this.

## 1. First login

Open `http://<host>.<tailnet>.ts.net:8000` from any device on your tailnet
(the installer prints the exact URL and password). Change the password under
**Profile**, and enable 2FA there.

> The dashboard is deliberately **not** reachable from your LAN or the
> internet. To expose it on a domain anyway, set **Settings → Instance's
> Domain** to `https://coolify.example.com`; Traefik will route it.

## 2. Domains: wildcard DNS and HTTPS

Pick one of three modes. You can switch at any time with
`sudo vercelify domain …`; it updates Coolify's wildcard for **new** apps
(change existing apps in their **Domains** field).

### A. No domain (private, zero setup)

Apps get `http://<app>.<tailscale-ip>.sslip.io`. [sslip.io](https://sslip.io)
resolves that name to your Tailscale IP, so every device on your tailnet can
open it, and nothing is public. Good for staging and personal tools.

### B. Your domain on Cloudflare (recommended for public apps)

```bash
sudo vercelify domain example.com --cloudflare-token <TOKEN>
```

Create the token at <https://dash.cloudflare.com/profile/api-tokens> →
**Create Custom Token** with:

- Account › **Cloudflare Tunnel** › Edit
- Zone › **DNS** › Edit
- Zone › **Zone** › Read (zone resources: your domain)

What happens:

1. A tunnel named `vercelify-<hostname>` is created (or reused).
2. Its routes send `*.example.com` and `example.com` to Traefik on `localhost:80`.
3. Proxied CNAMEs `*.example.com` and `example.com` → `<tunnel-id>.cfargotunnel.com`
   are created. **Existing A/AAAA records are left alone** (add `--force-dns`
   to replace them); MX/TXT records (your e-mail) are never touched.
4. `cloudflared` runs as a hardened systemd service.
5. Coolify's wildcard becomes `http://example.com`.

HTTPS is terminated at Cloudflare's edge with its free Universal SSL
certificate, which covers `example.com` and `*.example.com`. Use `http://`
URLs in Coolify for apps in this mode: Traefik serves plain HTTP to the
tunnel, and visitors still get HTTPS. Turn on **SSL/TLS → Edge Certificates →
Always Use HTTPS** in Cloudflare.

No router port forwarding is needed, and it works behind CGNAT, mobile
hotspots and university networks.

### C. Your domain pointing at your public IP

```bash
sudo vercelify domain example.com
```

At your DNS provider create `A example.com → <public IP>` and
`A *.example.com → <public IP>`, then forward TCP 80 and 443 on your router
to the laptop. Coolify's wildcard becomes `https://example.com`, and Traefik
obtains a Let's Encrypt certificate per app (HTTP-01 challenge).

For a *wildcard* certificate instead (needed only for thousands of hosts),
configure a DNS challenge in **Servers → localhost → Proxy** (Traefik
`certificatesresolvers.letsencrypt.acme.dnschallenge.provider=cloudflare`
plus `CF_DNS_API_TOKEN`).

## 3. Connect GitHub (push-to-deploy + PR previews)

1. Coolify → **Sources → + Add → GitHub App**. Choose **Automated
   installation**, give it a name, and pick the account or organisation.
2. GitHub opens; approve the app and select the repositories it may access.
3. **Webhook URL**: GitHub must reach Coolify to report pushes.
   - With a domain (mode B/C): set **Settings → Instance's Domain** to
     `https://coolify.example.com` *before* creating the GitHub App, so the
     webhook points there. The webhook path is only `/webhooks/…`; the
     dashboard login is still required for everything else.
     This makes the dashboard reachable on that name (it is still behind the
     Coolify login). To keep it private, add a **Cloudflare Access**
     application for `coolify.example.com` that requires your e-mail, with a
     *bypass* policy for the path `/webhooks/*` so GitHub can still deliver.
   - Without a public domain GitHub cannot call you: use the CI deploy hook
     below instead (push → GitHub Action → Coolify API over Tailscale).
4. Coolify generates the private key, client secret and webhook secret for
   you. Nothing to copy by hand.

**Deploy webhook (works without inbound access):** each app has a
**Webhooks → Deploy webhook** URL. Call it from CI:

```yaml
# .github/workflows/deploy.yml
on: { push: { branches: [main] } }
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: tailscale/github-action@v3   # puts the runner on your tailnet
        with: { oauth-client-id: ${{ secrets.TS_OAUTH_ID }}, oauth-secret: ${{ secrets.TS_OAUTH_SECRET }}, tags: tag:ci }
      - run: curl -fsS -X POST -H "Authorization: Bearer ${{ secrets.COOLIFY_TOKEN }}" "http://vercelify-host:8000/api/v1/deploy?uuid=${{ vars.APP_UUID }}"
```

## 4. Deploy your first app

1. **Projects → + Add** → name it.
2. **+ New → Private Repository (with GitHub App)** → pick repo and branch.
3. Build pack **Nixpacks** detects Next.js, Vite, Nuxt, Astro, SvelteKit,
   Remix, Go, Python, Rust, Ruby, PHP, Deno, Bun… with no config. Use
   **Dockerfile** or **Docker Compose** if your repo has one.
4. **Domains**: leave the generated one, or type `https://blog.example.com`.
5. **Deploy**. From now on every push to the branch deploys automatically
   (zero-downtime rolling update with health checks).
6. **Preview Deployments**: in the app's **Preview Deployments** tab enable
   them. Each PR gets `pr-<n>-<app>.<domain>` and is deleted when the PR is
   merged or closed.

Rollbacks: **Deployments** lists every build; the cockpit's **Apps → ⟲**
(or Coolify's **Rollback**) redeploys an earlier commit instantly from the
cached image.

## 5. Environments and environment variables

- Projects have environments (`production` by default). Add `staging`
  under **Project → + New environment**; deploy the same repo with a
  different branch there.
- Variables live on each app (**Environment Variables**). Tick
  **Is Build Variable?** for values needed at build time (e.g.
  `NEXT_PUBLIC_*`). The **Preview** section holds overrides used only by
  PR previews (e.g. a separate database URL).
- **Shared variables** (Team / Project / Environment) are referenced as
  `{{ project.DATABASE_URL }}` from apps.

## 6. Databases and storage on the HDD

Create databases with one click in the cockpit (**Apps → Databases → New
database**) or in Coolify (**+ New → Database**). Coolify stores their data in
a Docker named volume on the SSD by default.

**Move a database to the HDD (recommended for big data):** cockpit →
**Storage Allocator → Coolify volumes → Move to HDD**. It stops the database,
copies the data with ownership preserved to
`/mnt/storage/volumes/coolify/<volume>`, points Coolify's volume at that path
(a bind mount), and starts it again. The old copy is kept until you delete it.

**Doing it by hand in Coolify:** stop the resource → **Persistent Storage** →
edit the volume and set **Source Path** to
`/mnt/storage/volumes/<service>/<name>` → start it (copy existing data first).

**Docker Compose apps:** write the volume as a bind mount to
`/mnt/storage/volumes/...`; the cockpit's **Volume assistant** generates the
exact YAML and creates the folder with the right owner.

There is no global "default volume path" in Coolify; per-resource placement
(above) is the supported way, and it lets you keep latency-sensitive data
(Redis, caches) on the SSD.

**Backups:** each database has a **Backups** tab (cron schedule, retention,
optional S3). Point S3 at a MinIO service on the HDD or at an off-site bucket.
Local backups go to `/data/coolify/backups`, which the installer bind-mounts
to the HDD.

## 7. Add a Raspberry Pi worker over Tailscale

Requirements: Raspberry Pi 4/5 with **64-bit** Raspberry Pi OS (Bookworm) or
Ubuntu, on your tailnet (`curl -fsSL https://tailscale.com/install.sh | sh &&
sudo tailscale up`).

On the **main host**:

```bash
sudo vercelify add-worker pi@raspberrypi      # MagicDNS name or Tailscale IP
```

This copies the installer to the Pi and runs it with `--worker` (Docker,
agent, power/thermal guard, kernel tuning, optional `/mnt/storage`), authorises
Coolify's SSH key for `root` on the Pi, and registers the Pi in Coolify through
the API (`ip` = its Tailscale IP, user `root`, the localhost key). Coolify then
validates it and installs its proxy.

Manual alternative: run `curl -fsSL …/install.sh | sudo bash -s -- --worker
--coolify-pubkey "$(sudo cat /root/.ssh/authorized_keys | grep coolify)"` on
the Pi, then in Coolify **Servers → + Add**: IP = Pi's Tailscale IP, port 22,
user root, private key "localhost's key", **Validate**.

Using it:

- **Deploy to the Pi:** when creating a resource choose the Pi as server.
  Images must support `linux/arm64` (Nixpacks builds natively on the Pi).
- **Pi as build server:** **Servers → pi → General → Use it as a build
  server**; images are built there and pushed to a registry you configure.
- Traffic: a Pi has its own Traefik. In tunnel mode add a route to the Pi in
  the Cloudflare tunnel (Zero Trust → Networks → Tunnels → your tunnel →
  Public hostname `pi-app.example.com` → `http://<pi-tailscale-ip>:80`), or
  run a second tunnel on the Pi with `vercelify domain`.

## 8. Recommended settings for an 8 GB laptop

- **Servers → localhost → Docker Cleanup:** keep the default (daily) with a
  disk threshold of 80%; build caches grow fast.
- Set **memory limits** on apps (**Resource Limits**): e.g. 512 MB for a
  Next.js app, 1 GB for Postgres.
- Prefer `node:22-alpine`-based images or Nixpacks with `NIXPACKS_NODE_VERSION`.
- Use **Sentinel** (Servers → localhost → Sentinel) for per-container CPU/RAM
  history in Coolify; the cockpit shows live values from the agent.
- Turn on Coolify **Notifications** (Discord/Telegram/e-mail) for failed
  deployments and backups, and set `NOTIFY_URL` in
  `/etc/vercelify/power-guard.conf` for battery/thermal events.
