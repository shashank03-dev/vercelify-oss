# Architecture

The engineering design, with the reasons behind each choice, is
[design/SYSTEM-DESIGN.md](design/SYSTEM-DESIGN.md). This page is the map.

## Components

```
┌──────────────────────── MacBook ────────────────────────┐     ┌──────────────────────── Linux laptop (main server) ─────────────┐
│ Vercelify cockpit (Tauri v2)                             │     │ vercelify-agent  (Rust/axum, :7420 on the mesh address + lo)     │
│  React UI ──invoke──▶ Rust commands                      │     │  /v1/system, /v1/doctor, /v1/metrics   the machine              │
│   ├ agent_request/agent_stream ── HTTP + bearer token ──────────▶  /v1/platform/…   the engine (vercelify-platform):             │
│   ├ mesh_discover/mesh_pair/pair_join (vercelify-mesh)   │     │     SQLite state, builds (BuildKit), deployments, previews,      │
│   ├ local_system, tools_status (this Mac)                │     │     databases, volumes, reconciler, registry for workers         │
│   ├ sender_* → ffmpeg (VideoToolbox) ── UDP/SRT ────────────────▶  /v1/display      mpv receiver (DRM/X11/Wayland)                │
│   └ moonlight_* → Moonlight ◀────────────── Sunshine ───────────│  /v1/mesh/…       the network's coordinator                     │
│ settings.json (0600) in the app config dir               │     │  /v1/projects     vercel.json rules → proxy routes, cron jobs   │
│ vercelify-mesh (LaunchAgent: userspace WireGuard, SOCKS) │     │ vercelify-proxy (Traefik container, :80/:443)                    │
└──────────────────────────────────────────────────────────┘     │ vercelify-mesh (WireGuard on vfy0, 10.77.0.1)                    │
                                                                  │ power-guard.sh (systemd) → /run/vercelify/power-guard.state      │
                                                                  │ firewall.sh (DOCKER-USER, INPUT and mesh chains)                 │
                                                                  │ cloudflared (optional) → the proxy, mesh.DOMAIN → :7443          │
                                                                  └──────────────────────────────────────────────────────────────────┘
```

All cockpit ↔ host traffic rides `vercelify-mesh`, Vercelify's own WireGuard network
([MESH.md](MESH.md)): encrypted, across NAT and CGNAT (hole punching, else the main
server's relay), with no account anywhere. Nothing listens publicly except the proxy
(when you forward ports yourself) and, for joins from outside the LAN, the mesh's control
port through the outbound-only Cloudflare tunnel.

Workers run the same agent with `role=worker`: Docker, the runtime API the main
server drives over the mesh, and their own proxy. The main server places apps, builds
their images and keeps them in its registry (`vfy-registry`), which workers pull from.

## Why an agent on the host?

Everything about the server goes through a small authenticated agent running on the
server itself: the cockpit's own Rust side only reads **the Mac** (shown under "This
computer" in Node Health). That keeps privileged operations (Docker, `chown`,
systemctl, DRM output) on the machine that owns them, behind one token. The engine is a
library the agent mounts (`crates/vercelify-platform`), so a main server runs one small
static binary (≤ 8 MB, gated in CI) next to Docker.

## Shared types

`crates/vercelify-core` defines every request and response type once. `ts-rs`
exports them to `cockpit/src/types/generated/*.ts` on `cargo test`, so the agent,
the Tauri backend and the React UI cannot drift apart. CI fails if the
generated files are stale. Rules that exist on both sides (volume paths,
pairing codes) are tested against the same fixture files from Rust and from
TypeScript.

## Pairing

`vercelify-agent pair` encodes `{agent_url, agent_token, hostname, role, mesh_join}` as
`vfy2.<base64url JSON>`: `agent_url` is the server's mesh address
(`http://10.77.0.1:7420`), `mesh_join` a single-use invite for the device. The cockpit
joins the mesh with it, tests the connection, and stores the rest with 0600
permissions. Each pairing mints its own device token.

`sudo vercelify pair` also shows an 8-symbol code. The cockpit finds servers on the LAN
by multicast DNS (`_vercelify._tcp.local`, answered by the agent) and among its own mesh
peers (`GET /v1/health`, which reports `role` and `pairing_open`), and claims the full
pairing code with SPAKE2 (`POST /v1/pair/pake` on the control port): the code never
crosses the network, works once, expires after 15 minutes and dies after five wrong
attempts, each of which costs the caller a second.

## Multiple servers

The cockpit keeps `{servers: [...], active}` (`CockpitConfig`) in its settings file;
every `agent_request` can name a server by agent URL and defaults to the selected one.
A worker is added with an install command that carries an invite to its main server's
network (`--join`); it then registers with the engine by itself.

`GET /v1/doctor` returns the host's health checks with a fix for each problem, and
`POST /v1/update` starts `vercelify update` with `systemd-run` as the transient unit
`vercelify-update`; its output and exit code go to `/var/lib/vercelify/update.*` so the
agent, restarted by the update itself, can report the result.

## Push to deploy and local AI

`platform_git.rs` checks each app's branch with `git ls-remote` (`vercelify-platform::git`:
inherited git config ignored, prompts off, only https and ssh transports, credentials in
the environment, never in arguments) and queues a deployment when the head moves; webhooks and the GitHub App make it immediate ([PLATFORM.md](PLATFORM.md)).
`llm.rs` manages the `vercelify-ollama` container through the docker CLI and talks to
Ollama on `127.0.0.1:11434` with a small hyper client (`http.rs`); chat responses stream
through the agent unchanged and on to the cockpit over a Tauri channel.

## Storage tiers

| | SSD (`/`) | HDD (`/mnt/storage`) |
| --- | --- | --- |
| Holds | OS, `/var/lib/docker` (images, layers, build cache), named volumes | `/mnt/storage/volumes/<owner>/<volume>` bind mounts, backups |
| fstab | as installed | `UUID=… /mnt/storage ext4 defaults,nofail,noatime,x-systemd.device-timeout=10s 0 2` |
| Missing disk | — | Boot continues (`nofail`); the bare mount point is `chattr +i`, so containers fail instead of writing to the SSD |

Moving a volume (`POST /v1/platform/volumes/{id}/move`, a job): the engine stops what
uses it, copies the data with owners and modes kept (`cp -a`) to the other tier, records
the new place and starts it again; a database restarts on the new path, an app gets a
fresh container from the same image.

## Display path

```
Mac: AVFoundation capture ─▶ h264_videotoolbox (no B-frames, GOP 2 s, CBR) ─▶ MPEG-TS ─▶ udp://<server>:5600
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
restarts it if it stops. The core it never freezes and stops last: the proxy, the
registry and databases.

## Ports

| Port | Who | Reachable from |
| --- | --- | --- |
| 80, 443 | the proxy (apps) | LAN/internet if you forward them; the Cloudflare tunnel connects outbound |
| 7420 | vercelify-agent | bound to the mesh address and 127.0.0.1 |
| 7421 | agent wake listener (sleeping apps) | Docker bridges only (firewall) |
| 7443 | mesh control port (joins, relay) | LAN, or `mesh.DOMAIN` through the tunnel |
| 51820/udp | WireGuard | anywhere (it answers only known keys) |
| 5055 | the engine's registry | the main server itself, and workers over the mesh |
| 11434 | Ollama (local AI, optional) | the mesh (firewall) and the proxy's Docker network |
| 5600–5610/udp | display receiver | the mesh |

On the mesh, servers reach each other on any port; devices reach only 7420, 80, 443,
11434 and 5600–5610/udp (plus databases' public ports); containers reach only other
servers' proxies and those database ports (design §13.8).

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
4. **Uptime** (main server): every app URL through the local proxy, and every
   worker's agent.
5. **Doctor → alerts**: the same checks as `GET /v1/doctor` and `sudo vercelify
   doctor`; a change of status becomes an alert (`/v1/alerts`), sent to the
   notification URL. Last statuses are persisted, so restarts don't re-alert.
   Slow facts (SMART) are cached for an hour.
6. **Sleeping apps** (main server): the proxy's request counters per app; apps with
   sleep on that got no request for their idle time are stopped (see below).

The engine has its own loop: the reconciler (every 30 s, and at once after a stop,
start or password change) compares what runs with what the database wants on every
server, starts, stops and removes containers, and writes the proxy's base routes.

## Jobs and live events

Work that outlasts a request is a job (`/v1/jobs`): backups, updates, volume
moves, AI model installs and downloads, automatic deploys and rollbacks,
cleanups, apps going to sleep and waking. Jobs are kept in
`/var/lib/vercelify/jobs.json` (the last 60), so the activity list survives a
restart; an update's job is picked up again after the update restarts the agent.

`GET /v1/events` is a newline-delimited JSON stream that stays open: `hello`,
then every alert, every job change, `changed` hints (`apps`, `sleep`, `power-guard`,
`volumes`…) and a `ping` every 20 s. A reader that falls behind gets
`changed: all` and fetches everything again. The cockpit keeps one stream per
selected server, refreshes pages on hints, shows native notifications for
alerts, and reconnects with backoff.

## Sleeping apps

```
visitor ──▶ proxy ──▶ app's base route vfy-<app>-<n> (priority 100)        app running
                  └─▶ wake route (file provider, priority 1) ──▶ agent :7421
                                                                   engine starts it, "waking up" page (reloads)
```

Idle is measured by the proxy itself (`traefik_router_requests_total` per router
`vfy-<app>-<n>[-secure]@file`), so health checks inside the container and database
traffic don't keep an app awake, and the agent's uptime checks skip apps that sleep.
A stopped (asleep) app has no base route, so the wake route answers. A woken app stays
up at least 5 minutes.

## Project rules (`vercel.json`)

```
repository (at the deployed commit) ─┐
app / vfy upload ────────────────────┤
                                     ▼
       project.rs: vercel.json + vercelify.json → plan (RE2 regexes, notes)
                                     │
   the engine's base routes ──▶ routing.rs: one Traefik file per app/preview
                                     │
       <state>/proxy/dynamic/vercelify-routes-<app id>.yaml ──▶ proxy (file provider)
```

`projects.rs` keeps each app's files in the state directory, reconciles the proxy
files once a minute and after every change, and runs cron jobs at the start of each
minute through the local proxy with the app's `CRON_SECRET`. Our routers reuse each base
router's rule, entry point, TLS, service and middlewares (basic auth included) at
priorities 5000–50000, so they win while the app runs and drop out with its service when
it stops, leaving the wake route as before. Visitor IPs come from `Cf-Connecting-Ip`
only on connections from this machine (loopback and the proxy's own Docker networks,
where the tunnel connects from); everyone else is judged by the connection's address.
`host/tests/contract/routing.sh` checks all of it on the real proxy. Details:
[PROJECT-CONFIG.md](PROJECT-CONFIG.md).

## Tokens and audit

`agent.token` is the host's own token (the CLI). Every pairing mints a device
token (`vfyd_…`) stored as a SHA-256 hash in `/var/lib/vercelify/tokens.json`;
`DELETE /v1/tokens/{id}` or `sudo vercelify tokens revoke ID` cuts one device
off. Mesh nodes have their own tokens (`vfyn_…`), accepted only from the node's own
mesh address. The auth middleware records every non-GET request (time, token id,
method, path, status) in `/var/lib/vercelify/audit.log` (`/v1/audit`).

## Versions

`/v1/health` reports `api_version` and `capabilities` (`platform` on a main server
running the engine). The cockpit shows a feature only when the selected server's
agent offers it, and the Servers page marks agents older than the newest one you run.
Releases are installed by tag: `install.sh` resolves the latest release and the
agent and `vercelify-mesh` of exactly that version (CI refuses a tag that differs from
`Cargo.toml`).
