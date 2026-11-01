# Installation and complete feature-journey review

Reviewed 30 October 2026 against checkout `71971d8` (workspace version 0.2.5).

**Validation follow-up:** all 18 findings were re-examined. The
[finding-by-finding validation](validation/2026-10-30/VALIDATION.md) records 12
reproduced or externally verified observations, 4 source/UI-supported limitations,
and 2 items reclassified as UX recommendations. Networking and physical-hardware
limits are explicit; the original blanket failure/UX claims are narrowed below.

## Verdict

Vercelify has substantial functionality, simpler server-install defaults, and broad
automated coverage. It does **not yet meet the goal of an installation and everyday
workflow that a nontechnical person can complete without troubleshooting commands**.
The biggest remaining problems are connections between features: opening a private
app from the default Mac installation, completing GitHub authorization, taking AI
endpoints into other applications, publishing a domain, and recovering data.

This is a review, not a claim that these issues have been fixed. Product source was
not changed. The existing untracked `field-test/` material was preserved.

“Complete” here means an inventory of the currently shipped feature families across
the website, installers, desktop application, CLI, agent and engine, with setup,
normal use and recovery considered. It does not mean every code path or historical
version has been executed. Runtime evidence and remaining verification are separated
below. No sub-agents were used.

## Evidence and checks performed

| Check | Result in this review | What it establishes |
| --- | --- | --- |
| Cockpit `npm run typecheck` | Passed | Frontend type consistency |
| Cockpit `npm test` | 100 tests passed in 23 files | Existing unit/component behavior |
| Cockpit `npm run build` | Passed | Production frontend compiles |
| Cockpit `npm run e2e:demo` | 21 tests passed | Demo onboarding, app operations, databases, AI, GitHub manual fallback, machines, mesh, navigation and both themes |
| Website `npm run build` | Passed | Website type check and production bundle |
| ShellCheck command from CI | Passed | Shell lint for installers, admin CLI, user CLI, helpers and contract scripts |
| `cargo test --workspace --all-features --locked -- --test-threads=1` | 524 passed, 7 ignored | Workspace tests, including agent API tests; ignored Docker/media tests were not exercised |
| Default parallel Rust run | Failed once: `protected_routes_reject_missing_wrong_and_revoked_tokens` timed out starting its agent | A test-start reliability concern; sequential success does not establish its root cause |
| `bats host/tests` directly on macOS | Failed | Several tests assume GNU `stat`, `sed` and `touch`; this is not evidence that Linux installation fails |
| Host Bats suite in existing `vercelify-dev-linux` Docker image, repository mounted read-only | Full rerun passed all 181 tests | Host/CLI fixtures on Linux. An earlier full Linux run failed the maintenance deployment guard test; that test passed in isolation and the complete rerun passed |
| Extra Chromium keyboard probe | Focus escaped the New app dialog after Tab navigation | Reproduced F14 |
| Extra Chromium callback probe | Simulated backend creation produced a Git source, while the GitHub dialog stayed open and the credential selector stayed absent | Reproduced the frontend refresh gap in F4 against the demo backend; not a live GitHub OAuth test |
| Public GitHub latest-release metadata | v0.2.5 has universal macOS DMG + checksum, mesh binary + checksum, Linux agent/mesh binaries and Linux app bundles | The README's “no ready-made download yet” advice is obsolete |
| Relative Markdown file-link scan of README and top-level docs | No missing local file targets found | File existence only; does not verify every external URL or fragment |

The shell lint command was:

```bash
shellcheck -x -S warning dev install.sh install-mac.sh cli/vfy \
  host/*.sh host/vercelify host/lib/*.sh host/tests/*.bash \
  host/tests/contract/*.sh .github/scripts/*.sh
```

Existing reports (`PRODUCT-AUDIT.md`, `TESTING-REPORT.md`, the bug sweep and field-test
notes) were read as historical evidence, not counted as fresh passes. The unfinished
physical field-test plan was not treated as completed.

Release metadata source: [v0.2.5 release](https://github.com/shashank03-dev/vercelify-oss/releases/tag/v0.2.5).

## Findings, ordered by impact

Priority: **P1** = security, data-loss exposure or broken main journey; **P2** = a
significant hurdle, missing integration or misleading behavior; **P3** = polish.

### F1 — P1: Updating can put a usable pairing credential into a readable log

**Validated:** an isolated real agent update request ran the actual installer
summary with a stub systemd launcher. Under umask 022, both a new log and an
existing 0644 log remained 0644. A pairing credential extracted from each log
authenticated to that agent. No real update or user credential was used.

`agent/src/update.rs:94` creates `update.log` with `std::fs::write`, without setting
private permissions. The shipped agent unit does not set `UMask`. The update runs
the server installer and redirects its output into that file; `summary()` in
`host/setup-host.sh:336` generates and prints a full pairing code even on an update.
The pairing contains an agent bearer token. The existing physical field finding F1
records this file as 0644 in a 0755 state directory. The current source still has
the relevant behavior.

**Required result:** updates generate no pairing credentials by default; update logs
are created privately and existing permissive files are corrected. Verify both new
and pre-existing logs under a normal systemd umask. Treat credentials already written
to readable logs as exposed and provide a clear revocation/re-pair procedure.

### F2 — P1, conditional: App-mode setup supplies no browser route for private app links

`install-mac.sh` defaults to a userspace mesh daemon with a SOCKS listener on
127.0.0.1:7480. There is no system network interface in this mode; the application
and `vfy` explicitly use the proxy. Private apps get names such as
`http://blog.10.77.0.1.sslip.io` (`docs/PLATFORM.md:38`).

App rows, app details, preview links and the command palette call `openExternal`,
which only opens the original URL in the ordinary browser
(`cockpit/src/lib/open.ts:4`). It neither configures the browser's proxy nor provides
an app HTTP forward. The display and Moonlight paths do have explicit forwarding,
but the app-browser path does not. This is a source-confirmed integration gap in
the default configuration; an existing system route or manually configured browser
can hide it on a developer's machine.

**Validation boundary:** the UI was observed sending the private URL unchanged to
the browser, and the installer/transport code confirms userspace mode. A clean
physical Mac/server/browser roundtrip was not executed in this validation. The
finding applies when there is no other route or browser proxy to the mesh; it is
not a claim that every installation's browser links fail.

**Required result:** a clean Mac using the default installer can click a private app
and preview and load them successfully. Choose an approach that preserves host-based
routing, asset URLs, cookies, redirects and WebSockets. A TCP forward alone does not
automatically solve those HTTP-origin requirements. Test with no other VPN or proxy.

### F3 — P2, conditional: External AI snippets omit app-mode proxy or forwarding setup

`cockpit/src/views/AiView.tsx:483` generates OpenAI Python, curl and Ollama examples
from the server's private endpoint. Unlike the cockpit HTTP client and `vfy`, these
examples provide no SOCKS setup or local forwarding for userspace mesh mode. Chat
inside Vercelify can work while the copied example fails.

**Validated scope:** generated snippets contain a private mesh endpoint and no
proxy/forward setup. Failure is conditional on the external client having no mesh
route/proxy; real Python/Ollama clients on a clean Mac were not exercised.

**Required result:** provide an address that works from external programs in the
installed mode, then generate all snippets from that address. Execute each offered
snippet on a clean default Mac. Give the user one copyable endpoint and a connection
check, without requiring knowledge of WireGuard or proxy configuration.

### F4 — P1: Successful GitHub authorization does not finish the desktop workflow

The real callback converts the code, consumes its one-time state, stores a source,
and emits `git-sources` (`agent/src/webhooks.rs:271`). The New app source list is
loaded once with `usePoll(..., 0)`; it does not subscribe to that event. The GitHub
modal does not poll for completion or close itself. Only its manual `finish()` path
calls `onCreated` and refreshes sources (`GitIntegrations.tsx`, `PlatformAppsView.tsx:376`).

Consequently, the ordinary successful callback leaves the desktop waiting. Pasting
the already-successful callback URL into Finish can try to convert a consumed code
and state again. The existing demo test only exercises manual conversion, which
explains why it passes. A separate demo probe confirmed backend creation without a
desktop refresh.

**Required result:** callback completion automatically selects the new source and
closes the dialog; already-completed callbacks are recognized; installation on the
chosen repository is verified before deployment. Test successful browser return,
cancel, expiration, manual fallback and app restart.

There is an additional reachability limit: `github_app::start` selects a public
control URL or a server LAN address. A Mac connected only through userspace mesh on
another network may be unable to open that address. Include this browser flow in
the networking work in F2.

### F5 — P2: First deployment starts before the user can supply required configuration

New app exposes repository, branch, build pack, name and port, then immediately
deploys. It has no initial variables or database step (`PlatformAppsView.tsx:362`).
An app requiring an API key or database at build time can fail on its first run,
forcing the user to discover Variables, add values and redeploy. The normal form
also defaults the branch to `main`, rather than discovering the repository default.
README Step 4 explicitly tells users to add variables before deploying, which the
current New app dialog does not allow.

**Required result:** inspect the repository, show detected defaults, allow required
variables/database connection before the first build, and keep uncertain build
settings under Advanced. Validate the actual default branch. The user should not
have to learn through a preventable failed deployment.

### F6 — P2: Public-domain instructions do not adapt to the server's connection mode

The per-app Domains description says to point an A record at the server and promises
HTTPS (`PlatformAppsView.tsx:1097`). That advice does not cover CGNAT, a tunnel, or a
private mesh address. Settings displays connection mode but sends domain setup to a
server command with a Cloudflare token (`ServerCare.tsx:417`). There is no connected
publish/check/retry flow.

**Required result:** one Publish action identifies the mode, explains the remaining
provider action, checks reachability/DNS/TLS, and gives a useful next step on failure.
Do not suggest an A record pointing at a private address as public publication.
Keep private and public URLs visibly distinct.

### F7 — P2: A successful default backup is not necessarily protection against losing the server disk

Backups are enabled by default for main servers. This is a useful existing feature.
However, `default_backup_repo()` falls back to `/var/backups/vercelify-restic` when
no storage disk is mounted (`host/lib/backup.sh:37`). That is the normal single-disk
installation. The backup can succeed while its repository and original data live
on the same physical disk. The app displays success and target strings, without a
separate indication of disk-failure protection.

The encryption password is printed during installation and kept on the server.
There is no desktop recovery-key export/check-off flow. Off-site configuration,
snapshot browsing and whole-instance restoration require terminal work.

**Required result:** distinguish “local restore available” from “copy on another
device”; help the user choose a destination and save the recovery key. Verify a
restore onto a different machine. Avoid claiming disk-loss protection from a local
snapshot alone.

### F8 — P1: Database Restore immediately replaces current data without a review step

The Restore button calls `client.restore(db.id, b.snapshot)` directly
(`PlatformAppsView.tsx:1975`). The engine restores over current data
(`crates/vercelify-platform/src/dbengine.rs:417`). The UI has a typed confirmation
for deleting the database but no comparable review of this destructive restore.

**Required result:** show the database, snapshot date, replacement impact and downtime
before restoration; offer or create a current-data backup first. Require a deliberate
confirmation and show progress and completion. A misclick must not start replacement.

### F9 — P2: Advertised engine capabilities are missing from the ordinary app workflow

The engine accepts Compose, project/environment selection, source/build settings,
resource limits, health settings and watch paths. The desktop creation flow offers
Git, image and catalogue service, always sends null Compose fields, and does not
offer a project/environment selector. App settings offers deployment toggles,
placement, variables, volumes and other operational controls, but no complete
source/build/runtime editor.

**Clarification:** catalogue services already use Compose internally. The missing
path is importing/configuring a user's own Compose application. Repository
configuration and CLI/API alternatives also exist for some advanced settings;
this finding concerns desktop workflow coverage, not absent backend support.

This matters when correcting a branch, deploying a monorepo, setting build/output
commands, changing the app port, creating staging, managing a non-GitHub credential,
or limiting a heavy app on an old machine. API support is not equivalent to a
discoverable desktop feature.

**Required result:** expose supported settings in a small Advanced section attached
to the app. Provide shared project/environment controls only when needed. Add an
import path for supported Compose apps. If a capability is intentionally CLI/API
only, label that accurately in product claims and link its exact instructions.

### F10 — P2: Storage creation still offers destinations that are not available

The database form always offers SSD and HDD (`PlatformAppsView.tsx:1855`). The volume
assistant can recommend/select HDD and still offer Create even when its own label
says no HDD is mounted (`StorageToggle.tsx`). The storage movement table already
disables moves to an absent HDD, so availability handling is inconsistent.

**Validated and expanded:** on a single-disk UI fixture both choices remained
enabled. The real volume-assistant endpoint correctly returned 409 for an unmounted
directory. Initial database creation takes a different path: with Docker stubbed,
the real API accepted `tier=hdd`, recorded that tier and created a bind directory on
the unmounted parent filesystem. Thus the two paths differ at the backend too.
This does not claim a real database container was started in the reproduction.

**Required result:** use the actual disks and mount health throughout creation and
movement. On a single-disk machine, default to “Main disk” and explain how to add
another drive when requested. Describe speed/media based on measured hardware;
“system disk” does not guarantee SSD.

### F11 — P2: Deletion guidance points users to a volume cleanup action the Storage page does not expose

Database deletion says its volume can be deleted on Storage
(`PlatformAppsView.tsx:1987`). The Storage page lists volumes and offers movement,
but no delete action. App/database deletion intentionally retains physical data
while removing its volume metadata (`apps.rs:438`, `databases.rs:516`), so retained
data also needs a clear orphan-data discovery path. The raw host list is read-only.

**Required result:** provide a retained-data view with owner/size/last use and a
deliberate cleanup flow, or correct the guidance to an actually supported path.
Deleting an app must leave an understandable answer to “where is its data now?”

### F12 — P2: Installation instructions are inconsistent with the release and with one another

README lines 274–280 say there is no ready-made app and direct normal users toward
Xcode tools, Node, Homebrew and Rust. Public v0.2.5 metadata includes the universal
DMG and checksum. The website instead says one more line installs the app, without
explaining fallback prerequisites. The README leads with three AI/demo choices
before the full hosting path. The website's three-step process also misses the
existing installer-generated Mac command that installs and pairs in one step.

**Required result:** give normal users one primary path: supported Linux machine →
server installer → generated Mac install-and-connect command → first app. Link the
download directly. Keep source builds and developer demos in a secondary section.
Describe a failed release download accurately rather than always calling it “no
downloadable release yet.”

### F13 — P2: Remote-access promises exceed the new-install default

Remote access was correctly moved behind an explicit optional choice. A fresh
default setup does not automatically run `remote_on`; it needs the appropriate
public coordinator path for use from another network. Yet onboarding says to paste
the full code anywhere else, and README Step 3 initially says the connection keeps
working outside home. A later README paragraph does qualify this with the remote
setup requirement, but the desktop onboarding instruction has no equivalent check
and the new default no longer asks the question that paragraph refers to.

**Required result:** show “This network” versus “Away from home ready” after pairing,
with a guided enable/check action. Test from a genuinely different network behind
CGNAT. Keep the default private choice, but make its reachability limits visible.

### F14 — P2: Dialogs can lose keyboard focus to the page behind them

The shared `Modal` has dialog semantics and Escape handling, but no focus trap,
background inertness or complete focus restoration (`ui.tsx:915`). A fresh Chromium
probe opened New app, focused Repository, pressed Tab repeatedly, and reached the
page body while the modal was open.

**Required result:** keep focus within the innermost dialog, focus a sensible initial
control, and restore focus to its opener. Verify nested GitHub/build-log dialogs,
Tab/Shift-Tab and Escape. Test the command palette's equivalent behavior too.

### F15 — P3, UX recommendation: Some repairs still require server commands

Doctor checks show shell snippets; broken engine, missing backups, missing mesh,
domain changes, multiarch setup, standby promotion and some display setup send users
to the server. Many instructions already explicitly say “On the server” or “Change
it on the server”; my earlier claim that their destination could not be inferred
was too broad and is withdrawn. No usability study established that users cannot
follow these instructions. App terminals run inside containers, so they are not a
substitute for a host-admin workflow. In-app repair is an improvement recommendation,
not a reproduced functional failure.

**Required result:** for supported repairs, provide a narrowly scoped action with
plain-language impact and result. Where a physical/privileged action is unavoidable,
name the machine, show exact steps and preserve a retry/check button. Link existing
Add a machine and Settings flows instead of repeating old invite commands.

### F16 — P3, UX recommendation: Optional display setup could be more integrated

The screen-sharing UI correctly explains that it mirrors an existing screen and
that extending the Mac desktop requires another display. It has preflight, refresh,
cleanup and playback confirmation. However, a default Mac installation does not
include FFmpeg/Moonlight; missing tools send the user to Homebrew commands. Remote
desktop also requires Sunshine and a logged-in graphical Linux session. Screen
Recording consent and virtual-display setup remain separate steps.

**Correction and scope:** `install-mac.sh --with-display` already automates
FFmpeg/Moonlight installation when Homebrew is available; Linux mpv is installed by
default. The app already has separate Share a screen / Linux desktop sections,
Refresh setup, preflight and Sunshine setup actions. These are existing working
setup aids. The recommendation is to connect them more clearly; physical capture,
latency and desktop reliability were not validated here and are not findings of
failure.

The navigation hint still says “Use the laptop's screen as a monitor for this Mac,”
which can imply automatic desktop extension (`lib/nav.tsx:28`).

**Recommended improvement:** retain the existing separate mirror/control sections
and make their prerequisites easier to install/check. Link system permission
settings where possible. Keep the existing extension explanation. Verify on
physical macOS and Linux hardware; demo playback is insufficient.

### F17 — P2: A configuration-load error is displayed as first-time onboarding

The primary native cause is `read_config()` in `cockpit/src-tauri/src/lib.rs:66`:
it swallows settings-path/read/parse errors and calls `unwrap_or_default()`.
The browser loader in `transport.ts:124` similarly catches corrupt JSON and returns
an empty configuration. `App.tsx` also converts load rejections to an empty list,
but corrupt native JSON normally never reaches that rejection path. A browser
corrupt-JSON reproduction showed first-time onboarding while preserving the original
corrupt value until a subsequent save. Native permission failures were source-traced,
not reproduced against an installed desktop app.

Corrupt or unreadable configuration therefore looks like an unconfigured app.
This hides the distinction between “never connected” and “couldn't read your saved
connection,” and encourages unnecessary pairing.

**Required result:** display a recoverable configuration error with Retry and a
supported recovery/export path. Do not imply that the user has no servers when
loading them failed. Test permission errors and corrupt JSON.

### F18 — P2: Hardware and availability claims need boundaries users can understand

The physical findings note a battery with only 3.5% of design capacity while doctor
was green. Current doctor facts track charge percentage but not battery wear. Charge
percentage alone is not a runtime estimate. The website also promises apps come
back when power returns, although a fully powered-off laptop may need a button press
or firmware support before Linux can restore containers.

**Validated narrower claim:** a real collector with a synthetic 100%-charged,
3.5%-health battery exposed that wear value, but doctor emitted no wear warning.
The Health page already displays battery health; that capability was not missing.
Physical AC-loss/restart behavior remains unverified. The automatic-power-on concern
is a hardware-dependent qualification of the website's wording, not a reproduced
failure on the user's laptop.

**Required result:** warn about a severely worn battery when telemetry is available,
state that automatic recovery happens after the machine boots, and explain the
restart requirement. Recheck startup after real AC loss with the actual laptop.

## Feature coverage map

Every row identifies a currently implemented feature family and its best entry
point. **Present** means the path exists in source and/or the cited automated
checks; it is not a guarantee for all hardware or external services. References
to findings identify the specific remaining friction.

| Feature family | Best entry point | Review result / remaining work |
| --- | --- | --- |
| Product prerequisites and supported hardware | Website → Setup guide | Linux OS setup and a Mac remain prerequisites; make these visible before installation (F12, F18) |
| Standard server installation | Setup guide → one server command | Simple private defaults, single-disk default and preflight present; host fixtures passed |
| Stable/edge/pinned/source installation | Advanced installation | Matching script/binary selection present; keep outside normal setup |
| Rerun/idempotency | Installer/update | Fixtures cover saved roles, domains and disk safety; fresh physical rerun still required |
| Docker, Buildx and Compose prerequisites | Automatic installer check | Existing-Docker/plugin regressions covered by host tests |
| Disk selection, partitions and formatting | Advanced storage setup | Explicit formatting consent present; do not introduce an automatic destructive disk choice |
| macOS DMG installation and checksum | Setup guide / direct download | Release assets exist; normal instructions are stale (F12) |
| Source-build fallback | Developer setup | Requires developer tools; distinguish failed downloads from absence of a release |
| Mac mesh daemon setup and launchd | Automatic Mac installation | Userspace default present; browser/external-program integration incomplete (F2, F3) |
| Install and pair in one command | Server completion screen | Present; make it the primary documented Mac path (F12) |
| LAN discovery and short-code pairing | Connect | Demo and unit/API coverage present; clean-machine discovery still needs physical validation |
| Full-code pairing and pending pairing | Connect | Present; distant-network prerequisites and expiration wording need clarity (F13) |
| Pair again and connection repair | Settings → This computer | Join/test/save flow present; distinguish configuration-read failure (F17) |
| Server selection and persistence | Sidebar | Demo navigation and restart/persistence checks present |
| First Git deployment | Apps → New app | Deploy/status/log path present; configure before first build (F5) |
| Repository/default-branch discovery | New app | Manual URL and `main` default; needs detection and recoverable auth feedback |
| Private GitHub access | New app → GitHub connection | Normal callback handoff and off-LAN browser reachability incomplete (F4) |
| GitLab/Bitbucket/Gitea/deploy-key credentials | Advanced repository connection | Engine/admin CLI support; desktop credential management gap (F9) |
| Deploy on push | App → Settings | Polling/toggle/status/check present; clarify polling versus instant delivery |
| Webhooks | App → Settings → Webhook | Setup/secret UI present; provider-side work and public reachability remain explicit |
| GitHub commit statuses and preview comments | Repository integration | Engine/provider behavior present; live provider roundtrip not rerun |
| Docker image deployments | Apps → New app → Docker image | Demo/agent API coverage present; private registry credentials need an explicit supported path |
| Service catalogue | Apps → New app → Service | Catalogue and generated secrets present; validate prerequisites and first-run login links per service |
| Compose deployments | Apps → import / Advanced | Engine supports Compose; creation UI lacks a Compose path (F9) |
| Build-pack detection and Dockerfile builds | New app | Detection present; custom settings need a desktop editor (F9) |
| Monorepo root/build/output/watch paths | App → Advanced | Supported backend/config concepts; discoverability/editor gap (F9) |
| Build queue/cache/thermal pause | Deployment progress | Engine controls present; tell users why a build is queued or paused |
| Zero-downtime deployment and drain | App → Deployments | Engine behavior and unit/API coverage; real proxy/container contract not rerun |
| Manual deployment/rebuild/cancel | App → Deployments | Present in UI/client; provide consistent job progress and recovery |
| Manual rollback and rollback pin | App → Deployments | Demo exercises rollback; clearly show when automatic deployment is pinned |
| Automatic unhealthy-release rollback | Deployment status | Engine behavior present; needs live crash/OOM verification in release gate |
| Build logs and local AI explanation | Failed deployment → Explain | Demo and unit coverage present; existing Open AI models link is a useful connection |
| Runtime logs/restart/terminal | App → Logs / Terminal / Settings | Demo coverage present; distinguish container terminal from server administration |
| Environment variables and `.env` import/export | App → Variables / `vfy env` | Present; require a clear apply/redeploy action; initial configuration gap F5 |
| Build/preview variables and inherited scopes | Variables → Advanced | Backend supports scopes; full project/environment management missing (F9) |
| Projects and staging/custom environments | Apps → project/environment context | API/schema present; desktop creation/selection gap (F9) |
| Private app URL and browser opening | App → Open | Main integration blocker F2 |
| Pull-request previews and cleanup | App → Previews | Demo controls present; private-link and live-provider checks remain (F2, F4) |
| Public domains, direct DNS and HTTPS | App → Publish / Domains | Underlying modes present; mode-aware guidance/action missing (F6) |
| Cloudflare tunnel and edge TLS | Publish → recommended public mode | Admin installer/CLI automation present; connected desktop setup needed (F6) |
| `vercel.json` / `vercelify.json` rules | App → vercel.json | Rules/warnings/upload/remove demo coverage; real Traefik contract not rerun |
| Redirects/rewrites/headers/firewall | App → vercel.json | Engine/proxy implementation present; keep unsupported semantics explicit |
| HTTP cron jobs and scheduled container commands | App → vercel.json / Settings | Both paths present; explain their different execution contexts and UTC times |
| Deploy hooks | App → Settings | Secret URL management present; copy/test and credential handling matter |
| Password protection | App → Settings | UI/backend present; make protected state visible and changes deliberate |
| Idle sleep and wake | App → Settings | UI/status/demo coverage present; live wake latency not measured |
| App removal and retained data | App → Settings → Danger zone | Typed deletion present; retained-data cleanup gap F11 |
| Database creation and app linking | Apps → New database | Generated credentials and app linking present; unavailable tier choice F10 |
| Database connection references | Database → Connect / app variables | Present; make required redeployment clear and avoid exposing plaintext by default |
| Database console | Database → Console | Demo covers terminal path; real DB process not rerun |
| Database backup/schedule | Database → Backups | Manual UI and backend scheduling present; repository setup and restore verification remain |
| Database restore | Database → Backups → Restore | Destructive action review missing (F8) |
| Database delete | Database → Danger zone | Typed confirmation present; storage cleanup instruction inaccurate (F11) |
| Persistent app volumes | App → Settings → Volumes | Creation/removal API and UI present; choose actual available storage (F10) |
| Volume movement between disks | Storage → Volumes | Job/status and missing-HDD guard present; real data movement not rerun |
| Volume assistant/Compose snippets | Storage → Advanced | Functional technical utility; do not make it the default nontechnical storage path (F10) |
| Disk capacity/SMART/memory/temperature | Health / Storage | Present; interpret device roles accurately and include battery condition (F18) |
| Power/thermal guard and lid-close mode | Automatic setup / Health | Existing unit/host coverage; physical AC/temperature behavior still needs validation |
| Health history, uptime checks and activity | Health | Demo and unit behavior present; clear next action for reported issues (F15) |
| Alerts and desktop notifications | Settings → Alerts | UI/test action present; external webhook/provider permissions require live verification |
| Local AI install/start/stop/restart | AI models | Demo/API/unit coverage present; keep model memory/disk impact visible |
| Model catalogue/download/delete/chat | AI models | Demo covers download/chat; real model download, inference quality and memory not rerun |
| External AI/OpenAI-compatible endpoint | AI models → Use from Mac | Default-mode connection gap F3 |
| AI from deployed apps | AI models → Your apps | Internal network examples present; verify connectivity on worker placement separately |
| Add independent server or worker/Pi | Servers → Add a machine | Generated-command wizard and demo coverage present; replace redundant CLI hints (F15) |
| Mesh invites/peers/paths/revocation | Servers → Network | UI/API/host coverage present; network namespace/NAT contract not rerun |
| Away-from-home access and CGNAT relay | Servers → Network / guided remote setup | Backend/admin automation present; readiness and desktop enable path gap F13 |
| Worker placement/replicas/drain/failover | App → Settings / Servers | Demo controls and engine behavior present; shared-volume/state limits need visible explanation |
| Cross-architecture builds/registry | Worker addition → Advanced | Backend/CLI support; missing dependency action can become a terminal hurdle (F15) |
| Warm standby/promote/demote | Servers → Standby / recovery | Standby selection present; promotion remains a recovery command, needs a rehearsed runbook |
| Nightly encrypted whole-server backups | Settings → Backups | Default setup and host fixtures present; destination/key/restore gaps F7 |
| Whole-instance restore/state recovery | Recovery flow | CLI implementation present; not a nontechnical desktop journey (F7, F15) |
| Server updates and automatic update rollback | Servers → Update | UI/job/host coverage present; credential-log issue F1, transient test failures recorded |
| Desktop update discovery | Sidebar → Update app | Release detection present; download/install remains a user step; verify clean upgrade permissions |
| Security reboot window | Settings/Health → Maintenance | Backend/timer present; configure/explain in desktop; transient host-test failure needs tracking |
| Device tokens/roles/revocation/audit | Settings → Access / Recent changes | UI/API/unit coverage present; coordinate device and mesh revocation clearly |
| Unpair/remove machine | Settings / Servers | Present; explain it removes this connection and does not uninstall the server |
| Uninstall/reinstall/data retention | Advanced maintenance | Host CLI and tests present; retained containers/data behavior needs clear user-facing guidance |
| Theme/navigation/search/keyboard | Sidebar / command palette | Both themes and minimum-window demo checks passed; modal keyboard gap F14 |
| Mirror Mac screen to Linux | Second screen → Share screen | Preflight/cleanup/playback checks present; guided dependency/permission work F16 |
| Control Linux desktop | Second screen → Linux desktop | Moonlight/Sunshine path present; physical session/tool validation required (F16) |
| True extended Mac desktop | Screen-sharing setup | Requires a separately created display; do not present it as automatically implemented (F16) |
| User CLI and AI-assistant skill/API | Optional advanced tools | `vfy`, admin CLI and agent skill exist; keep normal users on the graphical path |
| Documentation, website and support entry | Contextual Help | Local file links exist; external links, snippets and release claims need ongoing integration checks |

## Recommended implementation order

1. **Close the broken/sensitive main paths:** private browser URLs, GitHub callback
   completion, update-log credentials and deliberate database restore.
2. **Make install → first working app continuous:** update release instructions,
   use the install-and-connect command, discover repository defaults, collect initial
   variables, and verify the opened app from the Mac.
3. **Connect optional capabilities to the same journey:** Publish, remote readiness,
   working external AI endpoint, available-storage selection, backup destination and
   recovery-key export. Reuse existing backend capabilities where possible.
4. **Finish recovery and advanced discoverability:** contextual repairs, app build/
   source/runtime settings, retained-data cleanup, project/environment controls,
   keyboard dialogs and guided display setup.

A suitable default journey is:

```text
Check hardware/OS → install server → install and connect Mac
→ choose repository → review detected settings and required variables
→ deploy → open the actual working app
→ optionally publish, add a database, enable AI or add another machine
```

Each optional feature should show whether it is ready, what is missing, one next
action, progress, and a retry. Advanced configuration stays accessible without
becoming a prerequisite for the simple path.

## Release acceptance checklist

These checks remain necessary before calling the complete experience ready for
nontechnical users. They are acceptance criteria, not passes from this review.

- A clean Mac with no Rust, Node, Homebrew, other VPN or proxy can install the
  published app, pair, deploy and open a private app and preview in its normal browser.
- A fresh supported Linux installation needs only documented prerequisites; first
  install, rerun, reboot and update preserve the user's choices and data.
- Public and private Git repositories complete from the desktop, including the normal
  successful callback, canceled/expired authorization and a repository not using `main`.
- An app needing a build-time secret/database can succeed on its first intended build;
  monorepo/build settings can be corrected without recreation or undocumented API calls.
- A nontechnical person can distinguish local-only, away-from-home and public access;
  test on separate networks, including CGNAT, and verify real DNS/HTTPS behavior.
- Every AI endpoint/snippet offered to external Mac apps actually works in the default
  networking mode. Real inference and memory use are checked on target hardware.
- Single-disk machines never offer unusable storage as ready; volume movement retains
  data, and retained data after deletion has a discoverable cleanup path.
- Backups survive loss of the original disk, the owner has the recovery key, and a
  fresh-machine restoration is rehearsed. Database restores require deliberate review.
- Updates never create or reveal unnecessary pairing credentials; log permissions are
  checked on existing installations as well as new ones.
- Unplug/replug, worn-battery handling, lid-close, restart and thermal behavior are
  verified on a physical old laptop. State when a manual power-on is needed.
- Screen capture, Screen Recording consent, physical Linux output and Sunshine/
  Moonlight control work on actual macOS and Linux sessions; separately verify Wayland.
- Failure states explain the affected machine and offer a supported repair/check;
  corrupt saved settings never masquerade as first-time setup.
- Keyboard-only navigation works through nested dialogs; help links lead to the exact
  matching instructions, and website/release/README instructions stay consistent.
- Run real Docker/BuildKit/Traefik/database/storage contracts, mesh NAT/relay/multinode/
  promotion contracts and live/desktop tests. Investigate the transient test-start and
  maintenance-guard failures rather than treating one rerun as proof of a fix.

The core product is already present. The remaining work is to make these feature
boundaries reliable and discoverable, and prove the ordinary journey on clean
machines without developer-installed tools masking missing prerequisites.
