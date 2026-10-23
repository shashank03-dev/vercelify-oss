# Vercelify gap analysis (October 2026)

A full audit of the repository: host installer and CLI, power guard, firewall, agent,
core crate, cockpit (Tauri and React), CI/release, and docs. The goal was to find
everything that stands between Vercelify and "as simple as Vercel, on hardware you
already own, and nothing falls over", with a specific look at Raspberry Pi workers.

Baseline at the time of the audit (`ae78451`): `cargo test -p vercelify-core -p
vercelify-agent` passes (126 tests). The code is careful and well tested *for the
paths it covers*. The gaps are mostly about what it does not cover yet: low-end and
ARM hardware, failure handling over long periods, multi-node operation, and the
lifecycle (updates, backups, version skew).

Severity used below: **P0** = will break real installs or loses data, fix first.
**P1** = reliability or security gap that will bite within months. **P2** = feature
or polish that makes Vercelify competitive.

---

## Status after the fix round

**Fixed / built** (with tests; see each module):

| Item | Where |
| --- | --- |
| B1 agent build-from-source fallback | `host/lib/agent.sh` |
| B2 scripts and agent from the same release; staged `vercelify update` with automatic rollback, `vercelify rollback`, release tag must match `Cargo.toml` | `install.sh`, `host/vercelify`, `host/lib/agent.sh`, `release.yml` |
| B3 Coolify auto-update off, schema compatibility check (installer + doctor, one shared column list), `vercelify coolify upgrade` | `host/lib/coolify.sh`, `agent/src/coolify.rs` |
| B4/B5 one doctor (CLI prints the agent's report), thresholds from `power-guard.conf` and the hardware profile | `agent/src/doctor.rs`, `host/vercelify` |
| B6 unknown `agent.toml` keys warn instead of failing; B7 one service allowlist (tested against the shipped file) | `agent/src/config.rs` |
| B8 SD cards / eMMC reported as `sd_card` | `crates/vercelify-core/src/disks.rs` |
| B9/B10 hardware profiles (pi5/pi4/pi/laptop/pc), memory cgroup on Pi, zram instead of swap on flash, journald limits, clock sync wait, hang watchdog, Wi-Fi power saving off | `host/lib/hardware.sh`, `crates/vercelify-core/src/hardware.rs` |
| B11 graded thermal response (pause builds + cap app CPU → freeze apps → shut down; databases never frozen) | `host/power-guard.sh` |
| B12 Tailscale key expiry in doctor, Health and alerts | `agent/src/doctor.rs`, cockpit Hardware card |
| B13 worker apps reachable through the tunnel (`vercelify routes sync`, 5-minute timer) | `host/lib/cloudflare.sh` |
| B15 Health's `docker stats` poll from 5 s to 15 s; history comes from the server | cockpit |
| B16 Coolify token scoped to read/write/deploy instead of root | `host/lib/coolify.sh` |
| OOM protection: memory-capped `vercelify.slice`, OOM priorities by container kind, PSI + OOM kills in doctor/Health | `host/lib/hardware.sh`, `agent/src/watch.rs` |
| Alerts (ntfy/Discord/Slack/webhook) for every doctor change, uptime of apps and pooled servers, failed deploys, rollbacks, backups; self-repair of firewall/tunnel/Docker; safe disk cleanup at 90% | `agent/src/watch.rs` |
| SMART disk health, reboot-required, clock sync checks | `agent/src/doctor.rs` |
| Container → tailnet isolation | `host/firewall.sh` |
| Per-device tokens (hashed), revocation, audit log | `agent/src/tokens.rs`, `agent/src/auth.rs` |
| Backups and restore (restic; HDD, another server over sftp, S3) | `host/lib/backup.sh`, `docs/BACKUPS.md` |
| API version + capabilities, version skew on the Servers page | `agent/src/routes.rs`, `cockpit/src/lib/caps.ts` |
| 24 h metrics history recorded by the agent | `agent/src/watch.rs`, cockpit History card |
| Webhook-free PR previews (forks never built, per-app cap), GitHub statuses + preview comments, automatic rollback (Coolify's rollback endpoint, pin + deploy on older Coolify), instant/rebuild marks, 5 images kept per app | `agent/src/autodeploy.rs`, `agent/src/github.rs` |
| Tailnet image registry, multi-arch builds | `host/lib/registry.sh` |
| `vfy` CLI on the Mac | `cli/vfy` |

**Second round: the items left open above, done.**

| Item | Where |
| --- | --- |
| One job model for long-running work (backups, updates, volume moves, AI model downloads, automatic deploys and rollbacks, disk cleanups, sleep and wake), kept across agent restarts; `GET /v1/jobs` | `agent/src/events.rs` |
| Live event stream `GET /v1/events` (newline-delimited JSON: alerts, job progress, "changed" hints, pings). The cockpit subscribes, refreshes pages when told, and polls only older agents. Volume moves run in the background (`?background=true`) instead of holding a request for minutes | `agent/src/routes.rs`, `cockpit/src/lib/live.tsx` |
| Power guard managed from the agent and the app: status, thresholds (validated, written to an overrides file the guard reads after its own config, since `/etc` is read-only to the agent), reset to the machine's defaults, restarted by the agent when it stops. It stays its own small daemon on purpose: it must shut down on an empty battery even when the agent is broken | `agent/src/powerguard.rs`, `host/power-guard.sh`, Settings → Power guard |
| Sleeping apps (scale to zero) without a Traefik plugin: Traefik's own request counters tell idle apps; idle ones are `docker stop`ped; a priority-1 route in Coolify's file provider sends their visitors to the agent's wake listener, which `docker start`s them and shows a "waking up" page. Turning it on adds the metrics flags to Coolify's proxy in its database and on disk (Coolify keeps them as custom flags), verified against Coolify 4.4.6 and Traefik 3.7 in the contract test | `agent/src/sleep.rs`, `vercelify sleep`, app → Domains |
| Coolify pinned at install to the tested release (4.4.6); `vercelify coolify upgrade` defaults to it; `VERCELIFY_COOLIFY_VERSION=latest` opts out | `host/lib/coolify.sh` |
| B14 Ollama pinned to 0.40.6 (install, model pull, chat, ps and delete checked against that image) | `agent/src/config.rs` |
| Nightly reboot window for security updates (`reboot-required`, or a replaced kernel), never during a build, backup, update, on battery, when hot or right after boot; on by default at 04:30, `vercelify maintenance off` | `host/lib/maintenance.sh`, doctor |
| CI: Coolify contract test (schema, API routes, the agent's SQL, the proxy edit, a full sleep/wake cycle) against the pinned and the newest Coolify; the real installer in a systemd container weekly; agent and core tests as arm64 binaries under QEMU, the bats suite and a Raspberry Pi 5 installer dry run on arm64 Debian 12 | `.github/workflows/integration.yml`, `ci.yml`, `host/tests/contract/coolify.sh` |
| Mac app: signing and notarisation in the release workflow when the Apple secrets are set; a menu-bar item with the selected server's state; native notifications for alerts; per-app domains and the server's domain mode in the app | `release.yml`, `cockpit/src-tauri/src/tray.rs`, `cockpit/src/lib/notify.ts`, `AppDomains.tsx` |

**Third round: Vercel parity** (after comparing every area of Vercel's docs, see
[VERCEL-PARITY.md](VERCEL-PARITY.md)).

| Item | Where |
| --- | --- |
| `vercel.json` redirects, rewrites (fallback semantics for SPAs, external proxy), headers, `cleanUrls`, `trailingSlash`, `has`/`missing`, compiled from path-to-regexp to RE2 and applied as Traefik routes layered over Coolify's own | `agent/src/project.rs`, `agent/src/routing.rs`, `agent/src/projects.rs` |
| Firewall (`vercelify.json`): blocked/trusted IPs (tunnel-safe), deny rules, rate limits | same |
| Cron jobs with `CRON_SECRET`, run-now, alerts after repeated failures | `agent/src/cron.rs`, `agent/src/projects.rs` |
| Ignored build step: Coolify watch paths and `[skip deploy]` | `agent/src/autodeploy.rs`, `agent/src/github.rs` |
| The app's **vercel.json** tab, `vfy config` / `vfy crons` | `cockpit/src/components/ProjectPanel.tsx`, `cli/vfy` |
| Contract test on a real Traefik (3.7 and 3.4) | `host/tests/contract/routing.sh` |

Still needs a person: a real Apple Developer ID (the workflow only signs when
its secrets exist), a first run of the new paths on real Raspberry Pi
hardware, and a release tag so installs pick all of this up.

---

## 1. Confirmed bugs and inconsistencies

| # | Sev | Where | Problem |
| --- | --- | --- | --- |
| B1 | P0 | `host/lib/agent.sh:38` | `run_cmd docker run_cmd --rm …` — the "build the agent from source" fallback runs `docker run_cmd`, which is not a Docker command. Any install where the release download fails (no release yet, offline mirror, checksum mismatch) dies here. |
| B2 | P0 | `install.sh` (`REF=main`), `host/vercelify` `cmd_update`, `host/lib/agent.sh:24` (`releases/latest`) | Host scripts come from **`main`** while the agent binary comes from the **latest release**. They can be any distance apart, so an install or `vercelify update` can pair new scripts with an old agent (or the reverse). There is no version pin, no compatibility check and no rollback. |
| B3 | P0 | `host/lib/coolify.sh`, `agent/src/coolify.rs` | Vercelify writes Coolify's Postgres schema directly (`server_settings`, `applications.preview_url_template`, `local_persistent_volumes`, `servers.user`). Coolify **auto-updates itself by default**, so a Coolify release that renames a column silently breaks volume moves, wildcard changes and worker keys on an unattended box. Nothing pins or gates the Coolify version. |
| B4 | P1 | `host/vercelify` `cmd_doctor` vs `agent/src/doctor.rs` | Two independent doctors with different checks. The CLI checks the Coolify SSH user, Traefik health, wildcard domain, cloudflared and lid handling; the agent (what the cockpit shows) checks memory, temperature, battery and auto-deploy. Each misses what the other catches. |
| B5 | P1 | `agent/src/doctor.rs:165-167`, fix texts | Temperature thresholds (92/80 °C) and "shuts down at 15%" are hard-coded instead of read from `power-guard.conf`; wrong as soon as the user tunes the guard, and wrong for every Pi (see §2). |
| B6 | P1 | `agent/src/config.rs:8` + `host/lib/agent.sh:60` | `agent.toml` is only written on first install and parsed with `deny_unknown_fields`. New defaults never reach existing hosts, and rolling the agent back after a config key was added makes it refuse to start. Needs a config version and migration. |
| B7 | P1 | `agent/src/config.rs:101` vs `host/config/agent.toml` | The compiled default service allowlist omits `vercelify-firewall`; the shipped TOML includes it. Two sources of truth. |
| B8 | P1 | `crates/vercelify-core/src/disks.rs:77` | SD cards and eMMC (`mmcblk*`) report `rotational=0` and are shown as **SSD**. On a Pi that hides the single most important fact about the machine (its system disk wears out). |
| B9 | P1 | `host/lib/common.sh:96` | `raspbian:12` is listed as tested, but 64-bit Raspberry Pi OS reports `ID=debian`; there is no board detection anywhere, so Pi installs get laptop defaults. |
| B10 | P1 | `host/lib/system.sh:79` | `ensure_swap` creates a 2–4 GB swapfile on `/`. On a Pi booting from SD that is a flash-wear generator; on a 2 GB Pi it is also too small to matter. |
| B11 | P1 | `host/power-guard.sh:168` | Thermal pause `docker pause`s **every** non-core container, including databases and every app. A hot afternoon takes all sites offline; Coolify then sees unhealthy containers. Builds should be paused first and apps throttled, not frozen. |
| B12 | P1 | `host/lib/tailscale.sh:40` | Tailscale node keys expire after 180 days by default. The installer only prints a tip; nothing records or warns about `KeyExpiry`. Half a year after install the server silently drops off the tailnet and the cockpit can no longer reach it. |
| B13 | P1 | `host/lib/cloudflare.sh:60` | The tunnel only routes to Traefik on the **main** host. Apps placed on a worker (the whole point of "Add to a pool") are not publicly reachable without manual Cloudflare dashboard work (`docs/COOLIFY.md` §7). |
| B14 | P2 | `agent/src/config.rs:118` | `ollama/ollama:latest` is unpinned; a breaking Ollama release arrives on the next `llm install`/restart. |
| B15 | P2 | `cockpit/src/views/HealthView.tsx:33` | Health polls `/v1/containers?stats=true` every 5 s; `docker stats --no-stream` samples every container for ~2 s each time. Fine on the i5, noticeable CPU on a Pi, and multiplied per open window/server. |
| B16 | P2 | `host/lib/coolify.sh:137` | The pairing code carries a Coolify token with the `root` ability. Losing the code (it is printed to the terminal and stored in `summary.txt`) hands out full Coolify admin, not just deploy rights. |

---

## 2. Raspberry Pi and low-end hardware: what a proper design needs

There is currently no Pi-specific code path. A Pi 4/5 worker gets the laptop's swap
file, thermal thresholds, storage assumptions and Docker defaults. The design below
makes the hardware a first-class input instead.

### 2.1 Hardware profiles (the foundation)

Detect the board once at install (`/proc/device-tree/model`, `/proc/cpuinfo`,
`MemTotal`, root device) and write `/etc/vercelify/profile.toml`. Everything else
reads its defaults from the profile, and the user can override any value:

| Setting | x86 laptop 8 GB | Pi 5 8 GB | Pi 4 4 GB | ≤ 2 GB |
| --- | --- | --- | --- | --- |
| Thermal pause / resume / shutdown | 92 / 80 / 98 °C | 80 / 70 / 85 °C | 78 / 68 / 85 °C | as Pi 4 |
| Swap | swapfile on SSD | zram (50% RAM, zstd) | zram | zram |
| Memory reserved for the OS | 1 GB | 768 MB | 512 MB | 384 MB |
| Builds allowed on this node | yes | yes, 1 at a time | no (pull images) | no |
| Ollama offered | yes | small models | tiny models, opt-in | no |
| Cockpit polling hint | normal | normal | relaxed | relaxed |

Ship the profile in `SystemSnapshot` so the cockpit can explain *why* a choice was made
("builds are off on this Pi 4: 4 GB can't build Next.js").

### 2.2 Things a Pi needs that a laptop doesn't

1. **Memory cgroup check.** Docker memory limits are silently ignored when the
   `memory` controller is missing (Raspberry Pi kernels have historically shipped with it
   disabled; k3s still documents adding `cgroup_memory=1 cgroup_enable=memory`). The
   installer should read `/sys/fs/cgroup/cgroup.controllers`; if `memory` is absent, add
   the flags to `cmdline.txt` (`/boot/firmware/cmdline.txt` on Bookworm) and ask for a
   reboot. Doctor check: "memory limits enforceable".
2. **Under-voltage and throttling.** Read
   `/sys/devices/platform/soc/soc:firmware/get_throttled` (or `vcgencmd get_throttled`):
   bit 0 under-voltage now, bit 2 throttled, bit 16 under-voltage has occurred. A
   weak USB-C supply is the most common cause of "random" Pi crashes and SD corruption.
   Show it in Health, alert on it, and add it to doctor.
3. **Storage that doesn't die.**
   - Classify `mmcblk` as `SdCard`/`Emmc` (fixes B8) and warn loudly when `/` or
     `/var/lib/docker` is on an SD card. Recommend USB SSD / NVMe HAT boot, and offer to
     move `/var/lib/docker` to the storage disk (`data-root`).
   - zram instead of a swapfile on flash (`systemd-zram-generator` or `zram-tools`).
   - journald: `Storage=volatile` or `SystemMaxUse=64M` on SD; Docker log limits are
     already set (`10m × 3`), keep them.
   - `commit=60` on SD-backed ext4 to batch writes.
4. **Power loss without a battery.** A Pi has no battery, so the guard's battery logic never
   runs and a power cut is an unclean stop. Support UPS HATs that expose a
   `power_supply` device (several do via kernel drivers), and otherwise rely on the
   storage rules above plus Docker's restart policies to recover cleanly.
5. **Clock.** The Pi 4 has no RTC. After a power cut it boots with a stale clock; TLS,
   `git ls-remote` and apt fail until NTP syncs. Enable `systemd-time-wait-sync` and
   order Docker, the agent and cloudflared after `time-sync.target`. Doctor check:
   `timedatectl show -p NTPSynchronized`.
6. **Hardware watchdog.** Enable the SoC watchdog (`bcm2835_wdt` on Pi, `iTCO_wdt` on most
   Intel laptops) with systemd `RuntimeWatchdogSec=15s`. A kernel or I/O hang then
   reboots the box instead of leaving it dead until someone walks over. Same value on
   laptops.
7. **Wi-Fi.** If the node is on `wlan0`, turn off Wi-Fi power saving (a known cause of
   Pi dropouts) and warn that Ethernet is strongly preferred for a server.
8. **Architecture-aware placement.** An x86 main and an arm64 worker do not share
   images. Before placing an app on a worker, check the image manifest for
   `linux/arm64` (Dockerfile/Compose apps) and say so in **Run on**; for Nixpacks,
   building on a 4 GB Pi is the problem (§4.4).

### 2.3 Whole-box protection against OOM (all hardware)

Out-of-memory is the most likely failure on 8 GB laptops and every Pi, and today nothing
handles it (no earlyoom/oomd, no `oom_score_adj`, no default limits on apps).

- **Reserve the OS.** Put all containers under a systemd slice with a memory ceiling
  (`"cgroup-parent": "vercelify.slice"` in `daemon.json`, slice with
  `MemoryHigh`/`MemoryMax` = RAM − profile reserve). sshd, tailscaled, the agent and
  cloudflared then always have memory, so the machine stays reachable when an app leaks.
- **Kill the right thing.** Enable `systemd-oomd` (or earlyoom) with PSI thresholds, and
  set `oom_score_adj` so build containers die first, then previews, then apps, and the
  Coolify DB/Redis last.
- **Default limits on new apps.** The cockpit's New app / New database flows create
  resources without limits. Apply framework defaults (e.g. Next.js 512 MB, Postgres
  1 GB on 8 GB machines, scaled by profile) through the Coolify API, editable later.
- **Admission control.** Before a deploy or a new preview, compare the sum of limits on
  the target node with its capacity; refuse with a clear message or suggest another node.

### 2.4 Graded thermal response (replaces B11)

1. ≥ pause threshold: pause **build** containers and Ollama only.
2. Still rising: cap app CPU with `docker update --cpus` (cgroup `cpu.max`), keep them serving.
3. Only at the critical threshold: freeze non-core containers, then shut down as today.

Never freeze databases; they hold locks other services wait on.

---

## 3. Reliability: failure modes and what handles them

| Failure | Today | Gap / fix |
| --- | --- | --- |
| Battery runs out (laptop) | Guard stops apps then core, powers off, restores on boot | Works. Add: refuse to restore if the last shutdown was thermal and it is still hot. |
| Power cut (Pi, desktop) | Nothing | Storage rules in §2.2, UPS HAT support, watchdog. |
| HDD unplugged | `nofail` + immutable mount point, doctor | Works. Add an **alert** (today only visible if someone opens doctor). |
| HDD failing slowly | Nothing | `smartctl` (smartmontools) health and reallocated-sector trend in Health/doctor; alert on change. Old laptop disks are the most likely component to die. |
| System disk full | Doctor warns at 85% | Automatic safe cleanup at a threshold (dangling images, build cache older than N days, never volumes), and protect rollback images (§4.5). |
| OOM | Nothing | §2.3. |
| Kernel/I/O hang | Nothing | Hardware watchdog (§2.2.6). |
| Unattended upgrade needs a reboot | Nothing | Detect `/var/run/reboot-required`, reboot in a maintenance window when no build is running, and notify. |
| Tailscale key expiry | Tip printed once | Record expiry from `tailscale status --json` (`Self.KeyExpiry`), warn 14 days ahead, recommend tagged auth keys (tagged nodes don't expire). B12. |
| Tailscale down | Agent unreachable | Optional LAN fallback listener (off by default) plus clear guidance; doctor over SSH. |
| Coolify auto-update changes schema | Silent breakage | B3: pin Coolify, disable auto-update, upgrade Coolify through `vercelify update` after a schema compatibility check. |
| Main host dies | Workers keep serving what they run, but nothing can deploy | Backups + documented restore to another node (§4.3); HA-lite for stateless apps (§4.2). |
| Agent crashes mid volume move | Safe (DB updated only after the copy and re-check) | Good design. Add a persistent job record so the cockpit shows "interrupted" instead of nothing. |
| Update breaks the host | No rollback | §5.2. |

**Alerts are the cross-cutting gap.** Today only the power guard can notify (`NOTIFY_URL`),
and the cockpit's activity bell only knows what happened while the app was open. Build one
alert path in the agent (power, thermal, under-voltage, disk, SMART, HDD missing, OOM kill,
deploy failed, app down, key expiry, backup failed) → ntfy / Discord / Telegram / e-mail
webhook, plus native macOS notifications from the cockpit when it is running.

---

## 4. Features that would make Vercelify genuinely competitive

Ordered by how much they move the product goal (fewer steps, more reliable on weak
machines). Each one extends Coolify rather than replacing it.

### 4.1 Webhook-free PR previews (P1, headline feature)

PR previews are advertised, but they rely on GitHub webhooks reaching Coolify, which is
exactly what a private tailnet or CGNAT prevents. The agent already polls branches;
extend it:

- Poll `refs/pull/*/head` with `git ls-remote` (GitHub; GitLab uses
  `refs/merge-requests/*/head`), and use the GitHub API with the stored token to learn
  whether each PR is open.
- New/updated open PR → Coolify `deploy?uuid=…&pr=N`; closed PR → delete the preview.
- Post a **commit status and a PR comment with the preview URL** (what makes Vercel feel
  like Vercel). Needs a token with `statuses`/`pull_requests: write`; keep it optional.
- Cap concurrent previews per node by the memory budget (§2.3) and expire stale ones.

### 4.2 Multi-node ingress and HA-lite for stateless apps (P1)

- Run a cloudflared connector on every node that serves apps, routing to its own Traefik
  (fixes B13). Cloudflare Tunnel supports several connectors ("replicas") for one tunnel
  and fails over between them.
- With Coolify deploying the same app to more than one server, a stateless app then
  survives the main laptop going down, at zero cost. Databases stay single-node with
  backups (§4.3); say so honestly in the parity doc.

### 4.3 Backups and disaster recovery (P0 for trust)

Today: Coolify's per-database backups (if configured by hand) and a bind mount. Missing:
the instance itself. Losing the SSD loses `/data/coolify/source/.env` (its `APP_KEY`
decrypts every stored secret), SSH keys and `/etc/vercelify`.

- `vercelify backup` / `restore`: Coolify DB dump, `.env`, SSH keys, `/etc/vercelify`,
  auto-deploy state, plus every app/database volume, using restic (deduplicated,
  encrypted).
- Targets: the local HDD, **another node in the pool over Tailscale** (free off-site on
  hardware the user already owns), or any S3 endpoint (Cloudflare R2's free tier, B2).
- Scheduled, with retention, a freshness check in doctor, an alert on failure, and a
  documented restore drill onto a fresh machine.

### 4.4 Builds that fit weak machines (P1)

Builds are the heaviest load and the first thing to OOM.

- A **build node** concept: build on the strongest machine (the i5, or optionally the Mac
  when it is awake), push to a **private registry on the tailnet**, and let workers only
  pull. Coolify already supports build servers plus a registry; Vercelify should install
  the registry (`registry:2`, on the HDD) and wire it up.
- Multi-arch: `buildx --platform linux/amd64,linux/arm64` on the build node so Pi workers
  get native images.
- Keep build caches on the HDD and prune them by age, not only by disk threshold.

### 4.5 Real instant rollback (P1)

"Roll back" redeploys an earlier commit, which is instant only while that image still
exists. Docker cleanup (daily, 80% threshold) prunes old images, so on a small SSD a
rollback quietly becomes a full rebuild. Keep the last N images per app (label them and
exclude them from cleanup) and show "instant" vs "rebuild" on each deployment row.
Add **automatic rollback**: watch health checks and 5xx rate for a few minutes after a
deploy and roll back on failure.

### 4.6 Scale-to-zero for idle apps and previews (P2)

The way to run many deployments on 8 GB is to not run them all at once. Sablier (open
source, has a Traefik plugin) stops idle containers and starts them on the first request.
Default it on for previews, opt-in for apps.

### 4.7 Observability that answers "what happened at 3 a.m.?" (P2)

- The agent keeps a small in-memory ring buffer (e.g. 24 h at 1-minute resolution,
  a few hundred KB) of CPU, memory, PSI, temperature, disk and per-container usage, and
  persists a compressed copy to disk hourly. Sparklines in Health and Servers.
- Per-app request metrics from Traefik (request count, p95 latency, 5xx), the
  self-hosted answer to Vercel Analytics/Speed Insights.
- Uptime checks of each app URL from the agent, and **cross-node checks** (each node
  checks the others), so the main laptop going down is noticed and alerted on.

### 4.8 Developer experience (P2)

- A `vercelify` CLI on the Mac: `deploy` (from Git, or upload the current folder),
  `logs`, `env pull/push`, `rollback`, `open`. Vercel's CLI is a large part of its feel.
- Domains in the cockpit (today CLI-only), including per-app custom domains with
  Cloudflare DNS created automatically.
- "Connect" for databases: a tailnet-only connection string for local development,
  with the firewall keeping it private.
- Env vars: preview vs production values and build-time flags in the cockpit (the
  bulk save currently writes production only).
- A read-only status page served on the tailnet so a phone can check the servers.

---

## 5. Architecture and system design

### 5.1 Agent internals

- **One sampler, many readers.** Every request recomputes everything (sysinfo, `docker
  stats`, `tailscale status`, doctor shells out several times). Run one background
  sampler per resource at a sensible interval, serve cached data, and add an SSE stream
  (`/v1/events`) so the cockpit stops polling. Lower load on a Pi, live UI, and the
  history in §4.7 falls out of it.
- **A shared job model.** LLM pulls, updates, volume moves and auto-deploy checks each
  have their own task bookkeeping. One persistent job type (id, kind, progress, log tail,
  result) makes every long operation resumable, visible and testable the same way.
- **Reconciler.** A periodic loop that compares desired state (firewall chains present,
  cloudflared running, storage mounted, zram on, watchdog armed) with reality, repairs
  what it safely can, and reports the rest through doctor and alerts. Self-healing rather
  than "run doctor and type the fix".
- **One doctor.** The CLI should call `GET /v1/doctor` (falling back to local checks only
  when the agent is down). Fixes B4/B5.
- **Fold the power guard into the agent** over time (same sysfs readers, typed, unit
  tested), keeping a minimal shell fallback for "agent not running".

### 5.2 Versioning, updates and compatibility

- Release everything together under one version tag: scripts, agent binaries, and a
  manifest with checksums. `install.sh` and `vercelify update` install a **tag**, never
  `main` (fixes B2). Channels: `stable` and `edge`.
- Signed artifacts (minisign or cosign) verified by the installer, not only SHA-256 from the
  same server.
- Update = stage → health check → commit, with automatic rollback to the previous
  version kept on disk. Across a pool: update one worker first, then the rest.
- **Coolify compatibility matrix**: pin the Coolify version in the release manifest,
  disable its auto-update, and run the SQL/API contract tests in CI against that version
  (today the live suites are manual).
- **API capability negotiation**: `/v1/health` returns `api_version` and `capabilities`;
  the cockpit hides or explains features a server's agent doesn't have, instead of
  failing when servers run different versions.
- Config schema version plus migrations for `agent.toml` and `power-guard.conf` (B6).

### 5.3 Single source of truth

Versions live in `Cargo.toml` and `tauri.conf.json`; thresholds in `power-guard.conf`
and `doctor.rs`; service allowlists in `config.rs` and `agent.toml`. Each should have one
owner, read by the others, with a test that fails on drift.

---

## 6. Security

| Sev | Finding | Fix |
| --- | --- | --- |
| P1 | **Containers can reach the whole tailnet.** Docker masquerades bridge traffic out of any interface, including `tailscale0`, so app code (a PR preview from a fork, a compromised npm dependency) can reach the Mac, other servers' agents, and unauthenticated Ollama (11434) on every node. Verify with `curl` from a container to another node's `:11434`. | `DOCKER-USER` rule dropping traffic from Docker bridges to `100.64.0.0/10` and `fd7a:115c:a1e0::/48`, with explicit allows for intended cross-node flows; ship a Tailscale ACL template with `tag:vercelify-server`. |
| P1 | One bearer token per server equals root (it can add SSH keys for root, run updates, control Docker). No revocation, no audit trail. | Per-device tokens issued at pairing, revocable from the cockpit and `vercelify pair --revoke`; append-only audit log of mutating calls; optional read-only tokens. |
| P1 | Pairing code embeds a `root` Coolify token (B16). | Mint a token with `write`/`deploy` abilities only; make the full code single-use and short-lived like the short code. |
| P2 | Ollama and the display receiver are open to the whole tailnet (including shared nodes). | Bind to tailnet but filter by tag/ACL; document. |
| P2 | Agent binaries and scripts are verified only by a checksum from the same origin. | Signatures (§5.2). |
| P2 | Unsigned macOS app ("right-click → Open"). | Notarised builds when a developer ID is available; until then keep the source build path. |

---

## 7. Cockpit

- **Polling budget.** Health: system 3 s, containers with stats 5 s, logs 3 s; Apps: 5 s;
  Servers: per server 10/30/60 s; plus the live context at 10/15 s. Move to the SSE stream
  (§5.1); until then, drop `stats=true` to 15 s and pause hidden tabs (visibility is
  already handled per window).
- **Alerts outside the app.** Native notifications and a menu-bar item showing server
  health (calm, no animation), since the main window is often closed.
- **Explain the hardware.** Show the profile (§2.1), throttling/under-voltage, SD-card
  warnings and memory budgets where decisions are made (Run on, New database, AI models).
- **Version skew UI.** Show each server's agent version, "update available", and which
  features need an update.

---

## 8. Testing gaps

- No CI run of the agent on aarch64 (only cross-compiled). Run the e2e suite under QEMU
  (`qemu-user` with the musl binary is cheap).
- No automated Coolify contract test; the live suites are manual. A nightly job with
  Coolify in Docker at the pinned version would catch B3 before users do.
- Failure injection: HDD disappearing mid-move, agent killed during an update, disk full
  during a build, clock in the past at boot.
- Installer integration runs Debian 12 x86 only: add Ubuntu 24.04 and a Raspberry Pi OS
  arm64 image (QEMU), including the cgroup and SD-card paths from §2.

---

## 9. Suggested order of work

**Phase 1: make what exists unbreakable (P0/P1)**
1. B1, B2 (tagged releases, one version), B3 (pin Coolify, disable auto-update), B6, B7.
2. Hardware profiles + Pi hardening (§2.1, §2.2), SD-card classification (B8, B9, B10).
3. OOM protection (§2.3), graded thermal response (§2.4).
4. One doctor, reading thresholds from config (B4, B5); Tailscale key expiry (B12).
5. Container → tailnet isolation and per-device tokens (§6).
6. Alerts from the agent (§3).

**Phase 2: trust and multi-node**
7. Backups and restore, including to another node (§4.3).
8. Per-node ingress and HA-lite (§4.2, B13).
9. Update staging and rollback, capability negotiation (§5.2).
10. Agent sampler + SSE + jobs + reconciler (§5.1).

**Phase 3: the Vercel feel**
11. Webhook-free PR previews with GitHub status and comments (§4.1).
12. Instant and automatic rollback (§4.5), build node and tailnet registry (§4.4).
13. Scale-to-zero (§4.6), metrics history and uptime checks (§4.7), Mac CLI and DX (§4.8).

Each item fits the product rules in `CLAUDE.md`: no recurring cost, no required cloud
service (Cloudflare and S3 stay optional), and they reduce load on weak machines rather
than add to it.
