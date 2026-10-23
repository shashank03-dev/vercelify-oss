# Architecture

## Components

```
┌──────────────────────── MacBook ────────────────────────┐     ┌──────────────────────── Linux laptop ─────────────────────────┐
│ Vercelify cockpit (Tauri v2)                             │     │ vercelify-agent  (Rust/axum, :7420 on tailscale0 + lo)         │
│  React UI ──invoke──▶ Rust commands                      │     │  /v1/system   sysinfo + /sys/class/{power_supply,thermal,hwmon}│
│   ├ agent_request   ─── HTTP + bearer token ────────────────────▶  /v1/tailscale  tailscale status --json                       │
│   ├ coolify_request ─── HTTP + Coolify API token ──────────┐    │  /v1/containers docker CLI (ps, stats, logs, start/stop)       │
│   ├ local_system / local_tailscale (this Mac)              │    │  /v1/volumes    mkdir+chown on HDD / docker volume create      │
│   ├ sender_* → ffmpeg (VideoToolbox) ───── UDP/SRT ─────────────▶  /v1/display    mpv receiver (DRM/X11/Wayland)                │
│   ├ moonlight_* → Moonlight ◀────────────── Sunshine ───────────│  /v1/coolify/volumes  read/update Coolify's volume rows         │
│   └ open_coolify → separate webview window (no IPC) ───────┐   │  /v1/services   systemctl allowlist                             │
│ settings.json (0600) in the app config dir               │ │   │                                                                │
└───────────────────────────────────────────────────────────┘ │   │ Coolify v4 (:8000 dashboard/API, Traefik :80/:443)              │
                                                              └───▶  coolify, coolify-db, coolify-redis, coolify-realtime, proxy   │
                                                                  │ power-guard.sh (systemd) → /run/vercelify/power-guard.state    │
                                                                  │ firewall.sh (DOCKER-USER + INPUT chains)                       │
                                                                  │ cloudflared (optional) → Traefik                               │
                                                                  └────────────────────────────────────────────────────────────────┘
```

All cockpit ↔ host traffic rides Tailscale, which is WireGuard-encrypted and
works across NAT and CGNAT. Nothing listens publicly except Traefik (when you
forward ports yourself) or the outbound-only Cloudflare tunnel.

## Why an agent on the host?

The PRD sketched Tauri commands that read disk usage with `sysinfo` and run
`tailscale status`. Those would describe **the Mac**, not the server. So the
cockpit's Rust side does those things locally (shown under "This computer" in
Node Health), and everything about the server goes through a small
authenticated agent running on the server itself. That also keeps privileged
operations (Docker, `chown`, systemctl, DRM output) on the machine that owns
them, behind one token.

## Shared types

`crates/vercelify-core` defines every request and response type once. `ts-rs`
exports them to `cockpit/src/types/generated/*.ts` on `cargo test`, so the agent,
the Tauri backend and the React UI cannot drift apart. CI fails if the
generated files are stale. Rules that exist on both sides (volume paths,
pairing codes) are tested against the same fixture files from Rust and from
TypeScript.

## Pairing

`vercelify-agent pair` encodes `{agent_url, agent_token, coolify_url,
coolify_token, hostname}` as `vfy1.<base64url JSON>`. The installer creates the
agent token (32 random bytes) and a Coolify API token (via `artisan tinker`
for the root user and team), and prints the code. The cockpit decodes it,
tests both connections, and stores it with 0600 permissions.

## Short pairing codes and discovery

`sudo vercelify pair` runs `vercelify-agent pair --short`, which writes
`/run/vercelify/pair-code.json` (0600): an 8-symbol code, its expiry and the full
pairing code. The cockpit lists online Linux peers from the Mac's `tailscale status`
that answer `GET /v1/health` (which reports `role` and `pairing_open`), and trades the
code for the full pairing at the unauthenticated `POST /v1/pair/claim`. Codes are
single-use, expire after 15 minutes and are destroyed after five wrong attempts, each
of which costs the caller a second.

## Multiple servers

The cockpit keeps `{servers: [...], active}` (`CockpitConfig`) in its settings file;
every `agent_request`/`coolify_request` can name a server by agent URL and defaults to
the selected one. Pooling a worker reads the main host's Coolify key
(`GET /v1/coolify/worker-key`), authorises it on the worker
(`POST /v1/ssh/authorized-keys`, the only write outside `/var/lib/vercelify`, allowed
by `ReadWritePaths=-/root/.ssh` in the unit) and registers the worker through
Coolify's `POST /api/v1/servers`.

`GET /v1/doctor` returns the host's health checks with a fix for each problem, and
`POST /v1/update` starts `vercelify update` with `systemd-run` as the transient unit
`vercelify-update`; its output and exit code go to `/var/lib/vercelify/update.*` so the
agent, restarted by the update itself, can report the result.

## Auto-deploy and local AI

`autodeploy.rs` checks watched branches with `git ls-remote` (inherited git config
ignored, prompts off, only http(s) transports) and calls Coolify's
`POST /api/v1/deploy` on localhost when the head moves. `llm.rs` manages the
`vercelify-ollama` container through the docker CLI and talks to Ollama on
`127.0.0.1:11434` with a small hyper client (`http.rs`); chat responses stream through
the agent unchanged and on to the cockpit over a Tauri channel.

## Storage tiers

| | SSD (`/`) | HDD (`/mnt/storage`) |
| --- | --- | --- |
| Holds | OS, `/var/lib/docker` (images, layers, build cache), named volumes | `/mnt/storage/volumes/<service>/…` bind mounts, `/mnt/storage/volumes/coolify/<volume>` (moved Coolify volumes), Coolify backups |
| fstab | as installed | `UUID=… /mnt/storage ext4 defaults,nofail,noatime,x-systemd.device-timeout=10s 0 2` |
| Missing disk | — | Boot continues (`nofail`); the bare mount point is `chattr +i`, so containers fail instead of writing to the SSD |

Moving a Coolify volume (`POST /v1/coolify/volumes/{id}/move`):

1. Refuse if a running container mounts it (inspected with `docker inspect` on all running containers).
2. Copy with `rsync -aHAX --numeric-ids --delete` (or clear + `cp -a`) to the target.
3. Check again that nothing started meanwhile; abort without changes if something did.
4. Set `local_persistent_volumes.host_path` to the HDD path, or back to `NULL` for a named volume.
5. The cockpit starts the resource; Coolify renders the bind mount.

The cockpit orchestrates stop → wait (Coolify reports exited *and* the volume
is unmounted, twice in a row) → move → start.

## Display path

```
Mac: AVFoundation capture ─▶ h264_videotoolbox (no B-frames, GOP 2 s, CBR) ─▶ MPEG-TS ─▶ udp://<tailscale-ip>:5600
Linux: mpv --profile=low-latency --untimed --no-cache (nobuffer, probesize 32) ─▶ GPU (VA-API) ─▶ DRM/KMS ─▶ laptop panel
```

## Power guard state machine

```
          on battery & ≤15% (×2)           ≥98°C (×3)
 ok ──────────────────────────────▶ shutting_down ◀──────────── any
  │  on battery          ▲                │ stop apps → stop core → sync → poweroff
  ▼                      │                ▼ remember ids in /var/lib/vercelify/power-guard.restore
 warning ── AC back ─────┘          next boot: docker start <ids> once AC/battery OK
  │
 ≥92°C ─▶ thermal_paused (pause builds, cap app CPU) ── ≥95°C: freeze apps too
                         └── ≤80°C ─▶ ok (unpause, CPU limits back)
```

Thresholds come from the hardware profile (lower on a Pi). Changed from the app
(Settings → Power guard), they go to `/var/lib/vercelify/power-guard.local.conf`,
which the guard reads after `/etc/vercelify/power-guard.conf`. The guard is its
own daemon so it keeps the machine safe even when the agent is down; the agent
restarts it if it stops.

## Ports

| Port | Who | Reachable from |
| --- | --- | --- |
| 80, 443 | Traefik (apps) | LAN/internet if you forward them; Cloudflare tunnel connects outbound |
| 8000 | Coolify dashboard + API | tailnet only (firewall) |
| 6001, 6002 | Coolify realtime/terminal | tailnet only |
| 8080 | Traefik API | tailnet only |
| 7420 | vercelify-agent | bound to Tailscale IP + 127.0.0.1 only |
| 7421 | agent wake listener (sleeping apps) | Docker bridges only (firewall) |
| 11434 | Ollama (local AI, optional) | tailnet only (firewall) and the `coolify` Docker network |
| 5600–5610/udp | display receiver | tailnet only |
| 22 | SSH (keys only for root) | as your network allows; Tailscale for Coolify workers |

## The agent's background loop (`watch.rs`)

Once a minute (`watch_interval_secs`) one loop does everything that used to need
someone looking:

1. **Metrics**: one sample of CPU, memory, swap, disk, load, temperature and
   memory pressure, kept for 24 hours (`/v1/metrics`, flushed to
   `/var/lib/vercelify/metrics.json`). The cockpit reads history instead of
   making the server recompute it.
2. **Repairs**: Docker not running, the firewall's `DOCKER-USER` jump gone,
   the tunnel failed → fixed (at most every 10 minutes each) and reported.
3. **OOM priorities**: `oom_score_adj` for every process of every container,
   by kind (builds 900, previews 600, AI 500, apps 0, core −300, data −500).
4. **Uptime** (main server): every app URL through its own server's Traefik
   (`curl --resolve`), and every pooled server's agent (`/v1/health`).
5. **Doctor → alerts**: the same checks as `GET /v1/doctor` and `sudo vercelify
   doctor`; a change of status becomes an alert (`/v1/alerts`), sent to the
   notification URL. Last statuses are persisted, so restarts don't re-alert.
   Slow facts (SMART, Coolify's schema) are cached for an hour.

6. **Sleeping apps** (main server): Traefik's request counters per app;
   apps with sleep on that got no request for their idle time are stopped
   (see below).

## Jobs and live events

Work that outlasts a request is a job (`/v1/jobs`): backups, updates, volume
moves, AI model installs and downloads, automatic deploys and rollbacks,
cleanups, apps going to sleep and waking. Jobs are kept in
`/var/lib/vercelify/jobs.json` (the last 60), so the activity list survives a
restart; an update's job is picked up again after the update restarts the agent.

`GET /v1/events` is a newline-delimited JSON stream that stays open: `hello`,
then every alert, every job change, `changed` hints (`sleep`, `power-guard`,
`volumes`…) and a `ping` every 20 s. A reader that falls behind gets
`changed: all` and fetches everything again. The cockpit keeps one stream per
selected server, refreshes pages on hints, shows native notifications for
alerts, and reconnects with backoff. Agents without the `events` capability are
polled as before.

## Sleeping apps

```
visitor ──▶ Traefik ──▶ app's own route (priority = rule length)     app running
                    └─▶ vercelify-wake (file provider, priority 1) ──▶ agent :7421
                                                                      docker start, "waking up" page (reloads)
```

Idle is measured by Traefik itself (`traefik_router_requests_total` per router
`http-0-<uuid>` / `…-pr-<n>`), so health checks inside the container and database
traffic don't keep an app awake, and the agent's uptime checks skip apps that
sleep. The first app with sleep on adds `--metrics.prometheus=true` to Coolify's
proxy configuration, in Coolify's database (where it reads it from) and on
disk, and restarts the proxy once; Coolify keeps unknown flags when it
regenerates its configuration. A woken app stays up at least 5 minutes. Only
containers the agent stopped are started again.

## Project rules (`vercel.json`)

```
repository ──(GitHub API, at the deployed commit)──┐
app / vfy upload ──────────────────────────────────┤
                                                   ▼
               project.rs: vercel.json + vercelify.json → plan (RE2 regexes, notes)
                                                   │
   container labels (Coolify's routers) ──▶ routing.rs: one Traefik file per app/preview
                                                   │
          /data/coolify/proxy/dynamic/vercelify-routes-<key>.yaml ──▶ Traefik (file provider)
```

`projects.rs` keeps each app's files in the state directory, reconciles the proxy
files once a minute and after every change (leaving them alone when Docker can't be
read), and runs cron jobs at the start of each minute through the local proxy with the
app's `CRON_SECRET` from its container. Our routers copy Coolify's (same rule, entry
point, TLS, service and middlewares) at priorities 5000–50000, so they win while the
app runs and drop out with its service when it stops, leaving Coolify's route and the
wake listener as before. Visitor IPs come from `Cf-Connecting-Ip` only on connections
from this machine (loopback and the proxy's own Docker networks, where the tunnel
connects from); everyone else is judged by the connection's address.
Details: [PROJECT-CONFIG.md](PROJECT-CONFIG.md).

## Tokens and audit

`agent.token` is the host's own token (the CLI). Every pairing mints a device
token (`vfyd_…`) stored as a SHA-256 hash in `/var/lib/vercelify/tokens.json`;
`DELETE /v1/tokens/{id}` or `sudo vercelify tokens revoke ID` cuts one device
off. The auth middleware records every non-GET request (time, token id, method,
path, status) in `/var/lib/vercelify/audit.log` (`/v1/audit`).

## Versions

`/v1/health` reports `api_version` and `capabilities`. The cockpit shows a
feature only when the selected server's agent offers it, and the Servers page
marks agents older than the newest one you run. Releases are installed by tag:
`install.sh` resolves the latest release and the agent of exactly that version
(CI refuses a tag that differs from `Cargo.toml`).
