# Field test — 30 October 2026

A full end-to-end test of Vercelify on a real old laptop, done the way a new user
would do it, followed by the developer contract suites. Every check has a pass rule.
Results get filled into the **Result** columns on the day; every failure gets a row
in the [Findings log](#findings-log) with evidence.

## Ground rules (strict)

1. **User path first, dev tooling last.** Tracks A and B install only what the README
   tells a user to install. No Rust, Node or repo checkout on the laptop until Phase 9,
   so nothing the developer toolchain provides can hide a missing dependency.
2. **No workarounds count as a pass.** If a step only works after a command the docs
   don't mention, it's a **fail** (with the workaround written down), even if the
   feature then works.
3. **Every claim needs evidence:** command + output, a screenshot, or a log excerpt,
   saved under `field-test/` in the scratch area and summarised here.
4. **Severity:** P0 = data loss, security hole, or the main path (install → pair →
   deploy) blocked. P1 = a documented feature broken. P2 = works but wrong/confusing
   message, slow, or needs an undocumented step. P3 = cosmetic / docs.
5. **I don't fix during the run.** Findings are logged; fixes come afterwards as their
   own commits with regression tests, then the failing checks are re-run.
6. **Secrets never pass through me.** Where a token is needed you run the command
   yourself (I'll hand you the exact line). I don't type passwords or tokens into
   anything. Nothing is ever printed with a token in it in this log.
7. **Destructive steps are announced first** (uninstall, pulling power, killing
   Docker, disk fills). I say what I'm about to do and wait for your OK.

## A/B design

The same check matrix runs twice on the same laptop:

| Track | What it is | How it's installed |
| --- | --- | --- |
| **A — what users get today** | The latest release, `v0.2.5` (stable channel) | The README one-liner, pinned to the release ref |
| **B — what we're about to ship** | Current `main` (`96f9627`) | `sudo vercelify update --edge` from A (also tests the update path), then a clean reinstall of B from scratch |

For each check the result is recorded per track: ✅ pass, ❌ fail, ⚠️ pass with a
P2/P3 note, ➖ not applicable. A check that passes on A and fails on B is a
**regression** and blocks the release. A check that fails on both is an existing bug.
Timings (install, first build, redeploy, rollback) are recorded for both so B can't
quietly get slower.

## Baseline (Phase 0, before touching anything)

Read-only. Saved to `field-test/00-baseline/`.

- `uname -a`, `/etc/os-release`, `lscpu`, `free -h`, `lsblk -f`, `df -h`, `ip -br a`,
  `ip route`, `systemctl --failed`, `ufw status` / `nft list ruleset`, battery
  (`upower -d`), thermal zones, `docker version` if present, `ss -tulpn`.
- Note: anything already listening on 80, 443, 7420, 7443, 51820/udp; existing Docker
  containers/volumes (must survive the install untouched); existing firewall rules.
- Clock check (`timedatectl`) — TLS and pairing codes depend on it.
- Mac side: macOS version, chip, free disk, whether the app / `vfy` /
  `vercelify-mesh` are already installed (remove for a clean Track A).

## Phase 1 — Server install (A, then B fresh)

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 1.1 | `--dry-run` first | Prints the plan, changes nothing (`diff` of baseline before/after is empty) | | |
| 1.2 | README one-liner, `--storage-device none` | Completes with no prompt beyond sudo; ends with a Mac command and an 8-char code; total time recorded | | |
| 1.3 | Preflight messages | Readable; nothing says Ready unless agent, mesh and its interface are really up | | |
| 1.4 | Services | `vercelify-agent`, `vercelify-mesh`, Docker, Traefik, power guard active; `systemctl --failed` no new entries | | |
| 1.5 | Binaries | Agent ≤ 8 MiB, mesh ≤ 6 MiB, versions match the track; `.sha256` verified in log | | |
| 1.6 | Buildx + Compose v2 present | `docker buildx version`, `docker compose version` | | |
| 1.7 | Exposure | `ss -tulpn` from LAN side: nothing reachable except what the firewall doc says; agent `:7420` not reachable from the LAN without the mesh | | |
| 1.8 | Pre-existing Docker data | Baseline containers/volumes still there and running | | |
| 1.9 | Idempotency | Run the same installer again: finished steps skipped, no duplicated firewall rules / units / cron, same pairing identity | | |
| 1.10 | Interrupted install | Kill the installer mid-Docker-step (announced first), re-run: recovers cleanly | | |
| 1.11 | `sudo vercelify doctor` / `status` | All green, or each warning has a fix that works | | |
| 1.12 | Install log | `/var/log/vercelify-setup.log` complete, no secrets in it | | |

## Phase 2 — Mac app and pairing

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 2.1 | `install-mac.sh --pair vfy2.…` | App in /Applications, `vfy` and `vercelify-mesh` installed, app opens already paired | | |
| 2.2 | Build-from-source path (if no `.dmg`) | Missing-tool messages are exact; build time recorded | | |
| 2.3 | Discovery + 8-char code (second pairing after unpair) | Server listed by name on the LAN; Connect works first try | | |
| 2.4 | Wrong code ×5 | Refused with a clear message, code then discarded; a fresh `vercelify pair` works | | |
| 2.5 | Expired code | Clear message after expiry | | |
| 2.6 | Mesh | `vercelify-mesh status` shows `lan` path, recent handshake; app keeps working after Mac Wi-Fi off/on | | |
| 2.7 | Settings file perms | Settings 0600, token not in plain logs | | |
| 2.8 | Restart app / reboot Mac | Still paired, reconnects without user action | | |
| 2.9 | Every page loads | Apps, AI models, Servers, Health, Storage, Second screen, Settings; ⌘1–7, ⌘K; dark + light | | |
| 2.10 | Health numbers | CPU, memory, disk, temperature, battery match `top`/`upower` on the laptop within reason | | |

## Phase 3 — Deploying apps (the core path)

Test repos (public, created in your GitHub account or mine as forks): a Next.js app,
a Vite static site, an Express API, a Python (FastAPI) app, a Dockerfile app, a Docker
Compose app, and one deliberately broken build.

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 3.1 | New app → Next.js | Live build log, URL works from Mac and phone on the mesh; first-build time | | |
| 3.2 | Vite, Express, Python, Dockerfile, Compose | Each detected without config and served | | |
| 3.3 | `vercel.json` (build/install/output, redirects, rewrites, headers) | Rules behave as `PROJECT-CONFIG.md` says (curl each) | | |
| 3.4 | Env vars: set in app, `.env` import (multi-line PEM, `# comment`, quotes), build-time vs runtime | App sees exact values; `vfy env pull` writes 0600 | | |
| 3.5 | Redeploy | Zero downtime: a `curl` loop every 200 ms during redeploy shows no errors | | |
| 3.6 | Rollback (app + `vfy rollback`) | Old version back without rebuild, in seconds | | |
| 3.7 | Auto rollback | Push a version that crashes on start → previous version keeps serving, message says why | | |
| 3.8 | Broken build | Failure reason visible; **Explain with AI** / `vfy why` give a sensible Cause/Fix/Check | | |
| 3.9 | Push to deploy (poll) | Push to `main` → deploy within ~1 min without webhook | | |
| 3.10 | `[skip deploy]` and watch paths | Not built | | |
| 3.11 | Webhook + double event | One deploy per commit | | |
| 3.12 | PR preview | PR opens → preview URL + GitHub status/comment; push updates it; close removes it; previews-off removes running ones | | |
| 3.13 | Private repo | Works after the documented Git access setup only | | |
| 3.14 | Deploy hook URL | Triggers a deploy; rate-limited | | |
| 3.15 | Cron job with `CRON_SECRET` | Runs on schedule; a slow run isn't stacked | | |
| 3.16 | Logs | Build logs, runtime logs, `vfy logs -f`; a log over the cap still shows the failure reason | | |
| 3.17 | Stop / sleep / remove app | Stopped app returns the documented page; removal cleans container, route, image (volumes per the docs) | | |
| 3.18 | `vfy` CLI tour | `apps`, `deploy`, `logs`, `env`, `open`, `services`, `servers`, `alerts`, `crons`, `config check/push/sync` | | |
| 3.19 | Bad input | Invalid branch names, mount paths, URLs → refused at the API with a reason, not a late failure | | |

## Phase 4 — Databases, storage and backups

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 4.1 | One-click Postgres, MySQL, MongoDB, Redis | Each starts; an app connects with the injected URL | | |
| 4.2 | Data persists | Write rows → restart DB, restart Docker, reboot laptop → rows still there | | |
| 4.3 | Volume name collision | Apps `api-cache`/`data` and `api`/`cache-data` keep separate data (B fix; A expected to fail) | | |
| 4.4 | Backup + restore | Backup, write more, restore → exact earlier state; a failed restore (missing snapshot) leaves live data intact | | |
| 4.5 | Backup password with spaces/`$`/quotes | Setup and restore work | | |
| 4.6 | Second disk move (only if a 2nd disk / USB disk exists) | SSD↔HDD move of a live DB and an app with 2 replicas; data intact | | |
| 4.7 | Disk nearly full (loop file, announced) | Warning shown before failure; builds refuse gracefully; recovery after freeing space | | |

## Phase 5 — Old-hardware behaviour

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 5.1 | 5 apps + 2 DBs running | Memory/CPU recorded; machine stays responsive; cockpit polling isn't the load | | |
| 5.2 | Build while serving | Served apps keep answering during a Next.js build | | |
| 5.3 | Lid closed | Keeps serving (you close it) | | |
| 5.4 | On battery (you unplug) | Power guard reacts as documented; alert in app | | |
| 5.5 | Thermal | Under `stress-ng` (announced) builds pause when hot, resume after | | |
| 5.6 | Reboot | Everything returns on its own: agent, mesh, proxy, apps, DBs, routes; time to all-green recorded | | |
| 5.7 | Power cut (you pull power with battery removed, only if you're OK with it) | Agent starts; no 0-byte key; SQLite state intact | | |
| 5.8 | Docker restart / agent kill -9 | Reconciler brings apps back; no duplicate containers | | |
| 5.9 | 24 h idle (optional, overnight) | No memory growth in agent/mesh, logs don't fill disk | | |

## Phase 6 — Network, remote access, domains

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 6.1 | Phone joins mesh (`vercelify mesh invite --device`) | Opens an app URL | | |
| 6.2 | Mac on another network (phone hotspot) | Still reaches the server via `direct` or `relay` after `vercelify remote on` | | |
| 6.3 | `vercelify remote on/off` | UPnP/NAT-PMP forward appears/disappears on the router; off leaves no open port | | |
| 6.4 | Cloudflare domain (if you give one) | `https://app.yourdomain` with a real certificate; cert status moves from pending to issued; works behind CGNAT | | |
| 6.5 | Firewall rules in `vercelify.json` | Blocks/allows as documented, incl. spoofed tunnel header | | |
| 6.6 | Revoke a device | Revoked Mac/phone loses access immediately; others unaffected | | |

## Phase 7 — AI

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 7.1 | `vercelify ai install` + `ai pull gemma3:1b` (e2b if RAM allows) | Explain with AI streams an answer from the server; RAM impact recorded | | |
| 7.2 | `vfy why --log` locally on the Mac (Ollama) and `--provider gemini` (your key) | Three-line answer | | |

## Phase 7S — Second screen and desktop control (full)

Two directions, both tested in full ([DISPLAY.md](DISPLAY.md)). Nobody has tested these
on real hardware yet: the product audit only used Xvfb, a null video output and fixture
logind sessions. So a physical panel, a real GPU, a real Wayland compositor and real
macOS Screen Recording are all **first-time coverage**. Evidence: a photo or video of the
laptop screen from your phone for each "visible" check, plus `ps`, agent logs and
`ss -ulpn` on both ends.

**Setup checks (Second screen tab)**

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| S.1 | Tab with nothing installed on the Mac (no ffmpeg) | Says exactly what's missing and the command to fix it; Start disabled or refuses with that message | | |
| S.2 | `brew install ffmpeg` → **Refresh setup** | Picks ffmpeg up without reopening the page | | |
| S.3 | Laptop receiver | `mpv` installed by the installer; tab shows the receiver ready; with `--no-display` the tab says why it's unavailable | | |
| S.4 | First Start → macOS Screen Recording prompt | Prompt shows; after you allow it and restart the app, capture works; message is clear if it's denied | | |
| S.5 | Permission survives an app update (Track A → B app) | No second prompt (signed app); if it does prompt, recorded as a finding | | |
| S.6 | Linux-only choices hidden on Mac and vice versa | No Mac capture/encoder options offered where they can't work | | |

**Mac screen → laptop panel (ffmpeg → mpv)**

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| S.10 | Source **Test** (test pattern), UDP, Smooth | Pattern on the laptop panel; status goes waiting → **Playing** only after mpv opens the media | | |
| S.11 | Source **Screen**, main Mac display | Mirrors the Mac screen with the cursor; record time to first frame | | |
| S.12 | Extended desktop with a virtual display (BetterDisplay) or HDMI dummy | That screen appears in the list; windows dragged onto it show on the laptop | | |
| S.13 | Each preset: Smooth, Sharp, Native, Light | Correct resolution/fps on the laptop (mpv stats `i` or `ffprobe` on a capture); bitrate within ±20 % (`nload`/`iftop` on `vfy0`) | | |
| S.14 | Encoder Hardware vs Software | Both play; Mac CPU recorded for each; Software on battery noted | | |
| S.15 | Transport UDP vs SRT | Both play; SRT survives 5 % packet loss (`tc netem` on the laptop, announced) where UDP shows artefacts | | |
| S.16 | Latency | Stopwatch on the Mac screen, photograph both screens together: UDP on LAN within the documented 30–60 ms, recorded for each preset and for SRT | | |
| S.17 | Mesh path | `vercelify-mesh status` path is `lan`/`direct`; a warning or noticeably higher latency on `relay` (hotspot test) | | |
| S.18 | App-mode forward | With default app-mode mesh, ffmpeg sends to `127.0.0.1:15600`; forward removed after Stop | | |
| S.19 | Laptop **without** a desktop session (log out / text console) | mpv draws via DRM straight on the panel | | |
| S.20 | Laptop **with** a logged-in X11 session | Auto-detects it, runs mpv as that user, fullscreen over the desktop | | |
| S.21 | Laptop **with** a Wayland session (Ubuntu default) | Same as S.20 on Wayland; if no session can be used, a recovery message (not a silent DRM attempt) | | |
| S.22 | Lid closed while streaming | Recorded behaviour (panel off is expected); no crash; resumes or explains after opening | | |
| S.23 | Hardware decoding | `--hwdec=auto-safe` uses VA-API on the iGPU (mpv log); laptop CPU recorded | | |
| S.24 | 30-minute stream | No drift or growing delay; no memory growth in mpv/ffmpeg/agent; laptop temperature recorded | | |
| S.25 | Firewall | UDP 5600–5610 reachable only over the mesh; a LAN-side `nc -u` to 5600 is dropped | | |

**Stopping and failure recovery**

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| S.30 | **Stop** | ffmpeg gone on the Mac, mpv gone on the laptop, IPC socket removed, port 5600 free | | |
| S.31 | Start/Stop ×20 quickly | No leftover processes either side; no stuck "Playing" | | |
| S.32 | Double-click Start / two cockpit windows | One receiver, one sender | | |
| S.33 | Force-quit the app while streaming | ffmpeg stops (watchdog); receiver stopped or reclaimed on next Start | | |
| S.34 | Kill mpv on the laptop mid-stream | App leaves "Playing" and says the receiver stopped | | |
| S.35 | Kill ffmpeg on the Mac mid-stream | App reports the sender exited, with a useful reason | | |
| S.36 | Mac Wi-Fi drop 10 s / mesh restart | Recovers or says what to do; no orphaned processes | | |
| S.37 | Agent restart while streaming | Receiver cleaned up; app shows the state honestly | | |
| S.38 | Mac sleeps while streaming | ffmpeg stops or resumes cleanly on wake; nothing stays running on the laptop | | |
| S.39 | Server running apps at the same time | Apps keep serving during a stream; a build during a stream recorded (frame drops, latency) | | |

**Laptop desktop → Mac (Sunshine + Moonlight).** Needs a desktop session on the laptop
and Moonlight on the Mac.

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| S.40 | `sudo vercelify desktop on` | Downloads Sunshine for this distro, SHA-256 checked, installed with apt, starts at login; `vercelify desktop` shows status | | |
| S.41 | Unsupported distro / no desktop | Clear refusal that points to the second-monitor mode | | |
| S.42 | **Launch Moonlight**, first time (B) | Pairs by itself: no Sunshine login, no PIN typed; `sunshine-login.json` 0600 in `/var/lib/vercelify`; the pairing window closes; the desktop opens | ➖ | |
| S.42a | Login pinning (B) | With Sunshine stopped and `nc -l 47990` listening as a normal user, pairing fails with a certificate message and the listener receives no login | ➖ | |
| S.42b | A login made by hand first (B) | Pairing refuses and names `reset-login`; `sudo vercelify desktop reset-login` then **Pair again** pairs; Moonlight devices paired earlier stay paired | ➖ | |
| S.42c | Older server (A) | App shows the manual steps; **Open Sunshine setup** opens the page with the forwards in place | | ➖ |
| S.43 | Manual pairing (A only) | You create the Sunshine login and enter Moonlight's PIN on Sunshine's PIN page (direction matches the app's text); pairs | | ➖ |
| S.44 | Desktop and Steam Big Picture app choices | Each opens the right thing | | |
| S.45 | Mouse, keyboard, clipboard, audio | Input works; record latency the same way as S.16 | | |
| S.46 | Second launch; **Pair again** after removing the server in Moonlight | Second launch connects straight away; Pair again pairs once more | | |
| S.47 | Over a hotspot (app-mode forwards) | Works through `127.0.0.1` forwards, or fails with a clear reason | | |
| S.48 | Restart Sunshine from the app; `vercelify desktop off` | Restart works; off stops autostart, Sunshine stays installed as documented | | |
| S.49 | Exposure | Sunshine ports reachable only over the mesh; its login and PIN required | | |

## Phase 8 — Lifecycle

| # | Check | Pass rule | A | B |
| --- | --- | --- | --- | --- |
| 8.1 | Update A → B (`Update all` in the app) | Apps, DBs, env, pairings, domains survive; DB schema migrates; version shows `0.2.5 → main` | ➖ | |
| 8.2 | `sudo vercelify rollback` | Back to A, still working | ➖ | |
| 8.3 | Failed update (agent made to not answer, announced) | Undoes itself within a minute | ➖ | |
| 8.4 | `restore-state` from a state backup | Works as BACKUPS.md says | | |
| 8.5 | `sudo vercelify uninstall` | Removes what it installed, leaves the baseline's Docker data, firewall and sleep settings as they were (diff against Phase 0) | | |

## Phase 9 — Developer suites on real hardware (after the user tracks)

Repo checkout + toolchain installed now. Results recorded as counts.

```bash
cargo test --workspace --all-features
VERCELIFY_DOCKER_TESTS=1 sudo -E cargo test -p vercelify-platform -- --ignored
bats host/tests
sudo AGENT_BIN=$PWD/target/debug/vercelify-agent host/tests/contract/routing.sh
sudo host/tests/contract/mesh.sh
sudo host/tests/contract/mesh-nat.sh
sudo host/tests/contract/multinode.sh
sudo host/tests/contract/promote.sh
sudo cockpit/e2e/run-live.sh
cockpit/e2e-desktop/run-desktop.sh
sudo AGENT_BINARY=… MESH_BINARY=… host/tests/integration/run.sh
```

These were last marked "not run" on a real machine: the desktop and live e2e suites and
the installer integration test. They are mandatory this time.

## Order and timing

| Block | Phases | Rough time |
| --- | --- | --- |
| Morning | 0, 1 (A), 2 (A), 3 (A) | 3 h |
| Midday | 4, 5, 6, 7, 7S on A | 4–5 h |
| Afternoon | 8.1–8.3 (A → B), then 3–7 re-run on B | 3–4 h |
| Evening | 8.5 uninstall, 1–2 clean install of B, Phase 9 | 2–3 h |
| Overnight | 5.9 idle soak | — |

## Findings log

| ID | Track | Check | Severity | What happened | Evidence | Status |
| --- | --- | --- | --- | --- | --- | --- |
| | | | | | | |

## Sign-off

B is shippable only if: no open P0/P1, no A→B regression, update A→B keeps all data,
and uninstall restores the baseline.
