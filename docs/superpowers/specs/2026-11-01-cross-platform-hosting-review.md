# Review: cross-platform hosting design

Date: 2026-11-01
Reviews: `2026-11-01-cross-platform-hosting-design.md` (the "spec")
Method: every source the spec cites was opened and its claims checked against the
code on `main` (17547d6); platform claims were checked against vendor docs and
issue trackers. Nothing here has been run on Windows or macOS VM hardware.

## 1. Verdict

The direction is sound: one Vercelify-owned Linux guest per non-Linux host, running
the unchanged agent, reuses the whole engine and keeps one code path for hosting.
The spec is careful about consent, idempotency and honest limits.

It has three gaps that would hurt real users if built as written, and it
underestimates how much does not exist yet:

1. **Reboot on a Mac does not bring hosting back** when FileVault is on (the macOS
   default). The spec promises boot-time recovery without saying so (§3.1).
2. **The guest reports the wrong machine.** The existing power guard and placement
   read battery, thermal and disk from inside the guest, so on Mac/Windows hosts
   they silently stop protecting the laptop (§3.2).
3. **Removing a server does not stick today.** A removed worker re-registers itself
   on its next heartbeat. The spec's tombstone requirement is right and should be
   fixed first, independent of this project (§2, finding C1).

And: there is **no Windows code at all** today — no Windows desktop build, no
Windows mesh daemon, no Windows CI. "Desktop on Windows" is a project of its own
that the spec lists as a given (§2, C4–C6).

## 2. Spec claims checked against the code

| # | Spec claim | Result | Evidence |
| --- | --- | --- | --- |
| C1 | Worker removal has dependent-workload checks; network revocation is separate | **True, and it exposes a live bug.** `nodes::remove` refuses pinned/running apps, then deletes the row. Nothing revokes the mesh identity, and `nodes::heartbeat` inserts a fresh `worker` row for any unknown `mesh_node_id`. The worker heartbeats every 30 s, so it comes back undrained under a new id. The UI text "It stops being a place for apps" is untrue until the user also revokes it. | `crates/vercelify-platform/src/nodes.rs:123-170, 196-221`; `agent/src/worker.rs:194`; `cockpit/src/components/PlatformServers.tsx:143` |
| C2 | No dedicated individual-deployment deletion route | **True.** Deployments have list/get/cancel/logs only. | `agent/src/platform_api.rs:63-69` |
| C3 | Placement has an "optimistic unknown-main-server scoring path" | **True.** A main server with no report yet scores `i64::MAX / 4` and always wins; scoring uses instantaneous `memory_available_mb` with no reservations, so two concurrent deploys can both claim the same headroom. | `crates/vercelify-platform/src/placement.rs:56-64` |
| C4 | Mesh daemon has Linux/macOS assumptions; Windows is engineering work | **True, understated.** `daemon`, `pair_client`, `tun`/`utun` are `cfg`-gated to Linux/macOS; the binary does not compile on Windows. The userspace pieces (`netstack` on smoltcp, `socks`, `gateway`) are not OS-gated, which makes a Windows **userspace-only** daemon (no Wintun driver) the cheap path. | `crates/vercelify-mesh/src/lib.rs` |
| C5 | Desktop "runs the existing userspace mesh" during bootstrap | **Inaccurate.** The desktop does not embed the mesh. It shells out to a separately installed `vercelify-mesh` (a LaunchAgent from `install-mac.sh`) and reaches servers via its SOCKS proxy on `127.0.0.1:7480`. On Windows there is no such binary to drive. | `cockpit/src-tauri/src/mesh.rs:1-45` |
| C6 | CI artifacts "do not establish all nine combinations" | **True, understated.** CI and release build Linux musl (x86_64, aarch64) and the macOS mesh daemon only. No Windows job, no desktop bundle for Windows or Linux in release. | `.github/workflows/ci.yml`, `release.yml` |
| C7 | Coordinator must be extracted from the Linux-only agent for bootstrap | **True.** The coordinator lives in the agent by design decision D6. | `docs/design/SYSTEM-DESIGN.md` §17 D6 |
| C8 | Mixed architectures are handled | **Partly.** The engine builds extra arches under QEMU emulation on the main server. An Apple Silicon guest is arm64; with an x86 main server every deploy to it becomes an emulated build — slow on the target hardware. | `crates/vercelify-platform/src/build.rs:399-408`, `engine.rs:506-513` |

## 3. Real-world problems the spec does not cover

### 3.1 Boot, login and sleep (highest user impact)

- **FileVault blocks boot-time start on macOS.** With FileVault on, launchd daemon
  definitions sit on the encrypted volume and nothing starts until a user unlocks
  the disk at the login window. After a power cut or a macOS update, the Mac hosts
  nothing until someone types a password. The spec's "boot service" and "reboot"
  tests will pass only on test machines without FileVault.
  - Fix: state it in the setup flow and in Environment settings ("After a restart,
    this Mac needs you to log in once before your apps come back"). For planned
    restarts (Vercelify's own updates) use `fdesetup authrestart` where the user
    has opted in. Never disable FileVault.
- **Apple discourages Virtualization.framework from a root daemon** (it links
  AppKit; DTS reports Secure Enclave errors in that context). Lima's boot mode runs
  the VM as a named user from a system LaunchDaemon. Recommend: host manager as a
  LaunchDaemon (small, no VZ), and the VM itself started in the user's context —
  a LaunchAgent at login, or Lima's `--condition=boot --user`, qualified on real
  hardware first. Pin which one in the design.
- **Lid close and sleep.** A closed laptop on battery sleeps on both OSes; Windows
  Modern Standby machines suspend the VM. The existing Linux power guard
  (`agent/src/powerguard.rs`) does not exist on the host side. The host manager
  needs an OS sleep-prevention assertion while hosting (IOPMAssertion on macOS,
  `SetThreadExecutionState`/power requests on Windows), off on battery by default,
  and the UI must say plainly that a sleeping machine serves nothing.
- **Windows Update auto-reboots.** Expect a reboot roughly monthly. Read active
  hours, surface "Windows will restart at …" as a host warning, and make
  boot-time recovery (§3.3) the tested path, not an edge case.
- **Clock jump after resume.** A guest that was suspended keeps its old clock until
  it resyncs. Mesh certificates are time-bounded (`MeshError::Expired`), and so are
  ACME orders and tokens. The guest needs a forced time sync on resume (chrony
  `makestep`, or the hypervisor clock), and the host manager should check guest
  skew in its health probe before marking `ready`.

### 3.2 The guest reports the wrong machine

The agent's `NodeStatus` (memory, battery, thermal, disks) is collected inside the
guest. There:
- battery is absent → the "on battery below 50 %" placement gate never fires;
- thermal zones are virtual → the thermal pause never fires;
- disk free is the virtual disk's logical free space, not the host's physical free
  space under a sparse image.

The spec says recommendations use physical measurements but does not say the
**runtime** status must too. Add: the host manager publishes host power/thermal/
physical-disk into the guest (a read-only file on a dedicated virtio channel or a
local endpoint), and the agent's collector prefers it when present. Add a test
that a Mac host on battery at 40 % is refused new placements.

### 3.3 Networking

- **Public ports 80/443.** Let's Encrypt HTTP-01 and direct public serving need host
  ports forwarded into the guest. On Windows, ports 80/443 are often taken (IIS,
  `http.sys` reservations, other dev servers) and the first bind triggers a
  Windows Defender Firewall prompt. Default to the Cloudflare Tunnel path (no
  inbound ports) on non-Linux hosts; make inbound ports an explicit opt-in that
  checks for conflicts and explains the firewall prompt.
- **QEMU user networking (slirp) is slow** and makes inbound traffic depend on
  host forwards. Qualify throughput for the mesh's UDP path through it; if it is
  inadequate, the alternative needs a driver (TAP) or Hyper-V switch, which is a
  new admin-install step to declare up front.
- **Mesh inside the guest only, at first.** The guest is Linux, so the existing
  daemon and agent work unmodified. Requiring a native Windows mesh daemon before
  any Windows host can work puts the hardest port on the critical path. See §4.2.

### 3.4 Storage and security software

- **Antivirus on the VM disk image.** Windows Defender real-time scanning of a
  multi-GB growing disk image makes builds dramatically slower. Detect it, explain
  the impact, and offer to add an exclusion for the Vercelify data folder only
  with the user's explicit approval (a security setting: never silently).
- **HDDs.** The product targets old laptops. Measure build and database
  performance with the image on a spinning disk; the recommendation should warn
  when the chosen disk is rotational.
- **Sparse image vs physical disk.** Agree with spec §8; add a hard stop when host
  physical free space falls below the reserve even if the guest thinks it has room,
  because a host volume that fills up corrupts the image for everyone.

### 3.5 Low-end fit

An 8 GB Windows laptop running Windows itself plus a 3–4 GB guest is at the edge
of usefulness. The spec rightly says minimums come from benchmarks; add that the
recommendation must be able to say **"This computer is better used as a desktop
than a host"** and point to a Linux reinstall or a Pi worker, rather than offer a
grant that will swap. That is consistent with the product goal and with
CLAUDE.md's "low-end first".

Older hardware: Lima's `vz` needs macOS 13+, which excludes many pre-2018 Macs —
exactly the "old laptop" audience. Lima's QEMU driver with HVF is
hardware-accelerated, not software emulation, and works on older macOS. The spec's
"no slow fallback" rule should not exclude it.

## 4. Design improvements

### 4.1 Shrink the platform matrix

The spec requires "all nine desktop-OS/host-OS pairs". The desktop talks to hosts
over the mesh with OS-independent JSON; the desktop's OS does not change host
behaviour. Test instead:
- 3 desktop builds × a protocol conformance suite against a fake host manager;
- 3 host types × the real hosting suite, driven from one desktop.

That is 6 recorded configurations instead of 9, with better coverage of what
actually differs.

### 4.2 Phase so that each step ships something true

The spec's §15 is "one complete build, not permission to label an intermediate
scaffold complete". That is the right honesty rule, but one build touching three
OSes, two hypervisors, a new daemon, a coordinator extraction and a new scheduler
cannot be reviewed or rolled back as a unit. Ship it as independently releasable
milestones, each with its own acceptance evidence:

| Milestone | Delivers | Platform risk |
| --- | --- | --- |
| M0 Correctness on Linux | Removal tombstones (C1), deployment deletion (C2), atomic admission reservations replacing optimistic scoring (C3), host-status override hook (§3.2). All Linux-only and testable in existing CI. | None |
| M1 Desktop on Windows/Linux | Tauri desktop builds for Windows and Linux; Windows userspace `vercelify-mesh` (smoltcp + SOCKS, no driver); Windows CI job. Manages existing Linux hosts only. | Low |
| M2 macOS host | Host manager + Lima (vz, QEMU/HVF fallback), FileVault-honest boot story, sleep assertion, host-status feed, resource grant. Guest runs the unchanged agent + mesh, joined through an **existing** main server's invitation. | Medium |
| M3 Windows host | Same host manager, QEMU+WHPX provider, firewall/antivirus/port-conflict handling. | High |
| M4 First-ever install on a non-Linux machine | Bootstrap trust root, coordinator extraction, guest becomes the main server. | High |

M4 is last on purpose: until then, the first server is Linux (today's path) and a
Mac/Windows host joins through the invitation flow that already exists. That
removes the coordinator extraction and the bootstrap handoff — the riskiest
security surface in the spec — from the first three releases.

### 4.3 Make the host manager smaller

The spec has the host manager join the mesh as its own identity so a broken guest
stays repairable remotely. That forces a native mesh daemon on Windows (M1 work)
into the host path and doubles the identities per machine. Alternative for M2/M3:
the host manager listens only on a local socket/named pipe; the native desktop on
that machine, and the guest's agent over a virtio channel, are its only clients.
Remote repair of a broken guest becomes "the guest's agent is down → the main
server shows *needs attention on the machine*". Add the remote-native endpoint in
M4 once the Windows mesh exists. Simpler, smaller attack surface, fewer identities
to revoke.

### 4.4 Name the guest image and its update path

The spec pins "guest image" but does not say what it is. Recommend: a minimal
Debian stable cloud image, built reproducibly in CI per arch, with
`setup-host.sh` run at image build time, digest-pinned in the host-manager
release. Guest OS updates: unattended security upgrades inside the guest (as on a
Linux host today); image replacement only across major versions, with
`/var/lib/vercelify` and Docker volumes on a separate data disk so the system disk
can be swapped without touching app data. A separate data disk also makes "disk
expansion" a data-disk-only operation and keeps backup/restore scoped.

### 4.5 Smaller corrections

- §1/§13: "UI gates only non-Linux hosts lacking a ready environment" — also gate on
  an **unhealthy** environment, otherwise a deploy to a stopped guest queues
  silently.
- §6: WSL rejection is correct and should be stated as a decision, not a
  candidate: `instanceIdleTimeout`, `memory` and `processors` are global
  `.wslconfig` settings, and WSL instances are per-user and stop after the last
  process exits (default 15 s). It cannot meet "survives logout" without editing
  global config.
- §6: the QEMU+WHPX docs list known limits (legacy VGA, MMX/SSE MMIO); use a
  headless virtio guest with no VGA. One old report caps `-smp` at 5 on Windows
  Home; include a Home-edition CPU count test in qualification.
- §9: `PATCH /v1/host/environment` that needs a restart must return the impact and
  require a second confirmed call, not restart on the first.
- §11: deleting a deployment must also check `standby` replication state so the
  warm standby does not reference a removed image.
- §17 of `SYSTEM-DESIGN.md`: Lima and QEMU are new runtime dependencies, which
  CLAUDE.md asks to keep to Docker/BuildKit/Traefik/WireGuard. Record them as a
  decision (D11) with the alternatives rejected (Docker Desktop: licence for
  larger orgs, its own VM we don't control; OrbStack: proprietary; WSL: §6
  above) in the same change as the first code.

## 5. Recommended first build step

M0 is entirely inside today's Linux engine, removes real bugs users can hit now,
and is a prerequisite for every later milestone:

1. Removal tombstones: `nodes::remove` records the `mesh_node_id`; `heartbeat`
   rejects a tombstoned id with a clear error; "Remove" optionally revokes the mesh
   identity in the same action; UI copy matches what happens.
2. Deployment deletion with reference checks (live, rollback target, previews,
   replicas, standby, shared digests) and a cleanup job.
3. Admission reservations: placement reserves memory per pending deploy/build in
   the transaction that chooses the node, released on success, failure or crash
   recovery.
4. Host-status override: the agent's collector accepts a host-provided status file
   when present (Linux ignores it), with tests.

## Sources

- [Lima automatic startup](https://lima-vm.io/docs/usage/autostart/) — boot mode via LaunchDaemon, runs as a named user.
- [Lima vz driver](https://lima-vm.io/docs/config/vmtype/vz/)
- [Apple Developer Forums: Virtualization under a launchd daemon](https://developer.apple.com/forums/thread/841688)
- [Apple Developer Forums: launchd scripts run after login, not boot](https://developer.apple.com/forums/thread/745343) — FileVault.
- [Jamf: auto-login with FileVault](https://community.jamf.com/general-discussions-2/auto-login-with-filevault-enabled-29417), [FileVault authenticated restarts](https://github.com/green-philodendron/Jamf-Interaction-Toolkit/wiki/07-FileVault-Authenticated-Restarts)
- [QEMU Windows Hypervisor Platform](https://www.qemu.org/docs/master/system/whpx.html)
- [QEMU issue 1043: -smp limit on Windows 11 Home](https://gitlab.com/qemu-project/qemu/-/issues/1043)
- [WSL discussion 8659: vmIdleTimeout vs instance](https://github.com/microsoft/WSL/discussions/8659)
- [WSL configuration](https://learn.microsoft.com/en-us/windows/wsl/wsl-config)

## 6. Implementation status (2026-11-01)

Built on `main` (uncommitted at the time of writing), following §4.2's milestones.
"Verified" names the evidence; nothing is claimed beyond it.

| Milestone | What was built | Evidence |
| --- | --- | --- |
| M0 removal | `mesh_nodes.hosting_removed_at` (migration 6); a removed worker's heartbeat is refused (409) until it pairs again; the worker backs off to 10 min | unit test `a_removed_worker_stays_removed_until_it_pairs_again` |
| M0 deployment deletion | `DELETE /v1/platform/deployments/{id}` (owners), refusing live, in-progress, preview-current, running and busy-app cases; log removed; images left to pruning; two-step delete in the deployments list | unit test, auth policy test, demo e2e |
| M0 reservations | `placement::Reservations` replaces optimistic scoring for concurrent deploys (90 s grace after start) | unit test `concurrent_deploys_do_not_spend_the_same_memory` |
| M0 host status | agent reads `VERCELIFY_HOST_STATUS` (file or URL); fresh host battery/thermal replace the guest's | unit test proving placement then refuses a Mac at 40 % battery |
| M1 Windows | `vercelify-mesh` daemon builds for Windows in userspace mode (no driver); desktop: `%LOCALAPPDATA%` data dir, `.exe` lookup, logon task, `taskkill`; Tauri NSIS bundle with both binaries; CI and release jobs | clippy `-D warnings` for `x86_64-pc-windows-gnu` (mesh, host, cockpit); **not run on Windows** |
| M2 macOS host | `vercelify-host` (state journal, machine lock, Lima provider, measurements, keep-awake, API, CLI); `install-mac.sh --host` with pinned Lima 2.2.1 | 33 unit tests; 23 installer bats tests; **live on macOS 27 / M5**: create (pinned image, Vercelify installed, health passed, ~10 min), stop, start, resize (verified inside: 3 CPUs, 5 GB, 35 GB root), recovery after the VM was force-stopped outside Vercelify, delete |
| M3 Windows host | QEMU+WHPX provider (argv, cloud-init seed over HTTP, SSH exec, QMP), `install-windows.ps1` | unit tests incl. a QMP exchange; cross-compiled; PowerShell parse runs in CI; **no WHPX run yet** |
| M4 first server | `--main` / "This is my first server": mesh ports 7443/tcp and 51820/udp forwarded (only for a main), port-conflict check, `POST /v1/host/environment/pairing` with the computer's LAN address; "Connect this app" / "Pair another computer" | unit and demo e2e tests; the live main-server create above ran the unchanged v1.0.0 agent as main |
| Desktop UI | **Vercelify environment** card on Servers: recommendation, adjust, invite or first server, staged progress, resize with restart consent, stop, type-to-delete | demo e2e (create → resize → stop → delete), both themes |

### Bugs found by running it for real (and fixed)

1. The once-a-minute reconcile marked an operation still running in the same
   process as "interrupted". Fixed with an in-process active-operation set and a test.
2. A stalled image download hung creation indefinitely (Lima has no download
   timeout). Every provider step now has a time limit and fails as retry-safe.
3. The isolation rule also blocked container DNS, because Lima's resolver lives on
   the host address. DNS is allowed before the drop; verified live (containers
   reach the internet, not the Mac).
4. Stopping showed "stopped" while the VM was still shutting down, so the UI
   offered actions the manager refused. Added a `stopping` state.
5. Removal reset the revision to 0. Revisions now only go up.

### Still open (release blockers for the platforms named)

- Windows hosting: a real WHPX run on Windows 10 and 11 (Home and Pro), Defender
  impact on the disk image, sleep/resume, Windows Update restart.
- macOS 11–12 (Lima with QEMU): a real run.
- Boot-time recovery after an actual macOS restart (the crash path is verified; a
  FileVault restart needs a person at the login window by design).
- A remote Desktop managing another computer's host manager over the mesh (today
  the host manager is managed from its own computer; its environment's agent is
  managed over the mesh as usual).
- The new agent features (host status, removal tombstones, deployment deletion,
  reservations) reach environments with the next release: the guest installs the
  release matching the host manager.
