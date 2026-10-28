# Product and installation audit — 29 October 2026

This audit follows the server installer, pairing/linking, desktop application,
platform/runtime and display flows. Fixes use the existing stack and keep the
normal path to install → select server → enter code.

## Fixes

| Priority | Problem | Result |
| --- | --- | --- |
| P1 | An already installed Docker daemon could lack Buildx and Compose v2. Builds failed with `unknown flag: --load`; deployments then failed to parse Compose files. | Setup verifies both plugins even when Docker already runs, installs Docker CE or Ubuntu packages as appropriate, and refuses to claim installation success if a required plugin remains unavailable. Reproduced on Ubuntu and verified with real builds/deployments. |
| P1 | Edge/local/commit installs could run current scripts with binaries downloaded from an older numbered release. | Numbered binaries are used only for matching release refs. Source installs build matching daemons; source builds exclude local target, node_modules and Git output. |
| P1 | Rerunning the installer directly on a worker could reset its saved role to main. | The saved role is restored before preflight, including workers with no domain. Explicit worker selection wins. |
| P1 | Settings “Pair again” decoded a mesh code without joining its network. Failed connection tests also discarded the code. | Re-pairing joins before testing/saving and keeps the code for recovery on failure. |
| P1 | A sender-stop error could be swallowed while the UI reported that screen sharing stopped. A lost receiver-start response could leave a receiver behind. | Both stop operations are attempted and failures are reported. Receiver cleanup is attempted even when its start response is lost. |
| P2 | A Linux desktop could occupy the GPU while the receiver blindly attempted DRM output. | Auto mode resolves the active local non-root X11/Wayland session, reads its actual display/auth environment, and launches mpv as that user. Explicit configuration remains available. Root-to-user X11 rendering was tested with real Xvfb, Xauthority and mpv. |
| P2 | Concurrent receiver requests and helper children could escape lifecycle cleanup. Old process output could pollute a new session. | Start/stop operations are serialized, receiver process groups are killed on cleanup, IPC sockets are removed and output is associated with its originating process. |
| P2 | Two running processes were labeled Live without evidence of media reaching the receiver. | The agent queries mpv's private IPC socket for media position. The UI distinguishes waiting, unconfirmed streaming and confirmed playback; older servers get an update hint. |
| P2 | Too many optional setup choices interrupted installation; automatically selecting a spare disk could block the default path. | Domains, remote access, AI and Sunshine questions move behind `--configure`. The system disk is the default; a second disk is an explicit choice. |
| P2 | Setup could print Ready despite an unavailable private network. | The summary checks agent health, the mesh daemon and its actual interface address, records readiness and prints recovery commands. |
| P2 | Selecting a partition, as suggested by setup, failed because storage lookup only searched whole disks. | The lookup traverses the block-device tree and preserves partition identity. |
| P2 | Optional Mac streaming-tool failures could abort setup or falsely report that both tools were ready. | Failures produce retry commands without blocking installation of the app, and readiness is reported accurately. |
| P2 | Mirroring was described as creating a second desktop; Linux builds offered unsupported Mac capture/encoder choices. | Screen sharing is described accurately, platform-specific choices are restricted and advanced source/transport/encoder controls are folded away. The primary path is screen, quality, Start. |
| P2 | Sunshine pairing instructions reversed the PIN direction and required manually finding its address. | Open Sunshine setup establishes private forwards before opening the browser; instructions explain that Moonlight displays the PIN. |
| P2 | Missing tools/permissions required reopening the page to retry; capture could fail immediately without useful context. | Preflight checks and recovery messages run before starting; Refresh setup retries discovery. Early sender exits are reported. Tool discovery also recognizes Moonlight installed in the user’s Applications or Flatpak directory. |
| P2 | Dry-run Docker setup still downloaded a third-party installer. The network summary repeated the address. | Dry runs avoid that download and the summary prints one address. |
| Test gap | The real desktop test claimed to stub the mesh but provided no state, so it could never start streaming. | A complete loopback state fixture makes media tests independent; separate contracts test real mesh networking. Desktop tests assert confirmed playback. |

## Verification

Completed checks include:

- Rust workspace tests and Clippy, including the mesh daemon feature; Linux agent/core/platform/mesh tests.
- Host ShellCheck, 173 Bats tests in Debian, and an installer dry run.
- Desktop TypeScript checks, 89 unit tests, production build and 21 browser demo flows in both themes, including minimum-window layouts.
- Six tests driving the real Tauri application through WebKitWebDriver: pairing and private settings, Rust HTTP requests, tool discovery, real H.264 streaming into mpv, playback confirmation, stop, force-quit cleanup and restart persistence.
- The real Debian 12 ARM64 installer and repeated installation, with the agent, mesh, firewall and power guard active. Deployment, proxy serving, redeployment, rollback and app removal passed against that installed server.
- Real MPEG-TS playback and start/stop cycles against the Debian receiver, and automatic root-to-user X11 session handling under Xvfb with actual Xauthority.
- Mesh: 28 checks; NAT traversal/relay: 8 checks; proxy routing contract; two-server placement/replicas/rollback/revocation: 15 checks; standby promotion and recovery: 19 checks.
- Docker integration with real PostgreSQL/Valkey and storage movement, plus real source build, deployment, redeployment, rollback and recovery tests. These run with `VERCELIFY_DOCKER_TESTS=1` as root because direct Docker volume access requires it.
- Seven live browser tests against the real Linux agent: pairing, image deployment and serving, redeployment/rollback/logs, Redis and backup, live database moves between SSD/HDD, resources/services, browser-only streaming recovery and unpairing.

The full evidence is automated source/runtime validation, not a certification of
every hardware combination. Physical macOS capture permissions, a physical Linux
panel/GPU and a real Wayland compositor are not replaced by Xvfb/null-output tests.
Wayland session selection and missing-session recovery have fixture coverage.

## Display product decision

Keep mirroring and remote control on the existing FFmpeg/mpv and
Sunshine/Moonlight pipelines. Simplify discovery, prerequisites, session selection,
private forwarding and error recovery around those pipelines. These are the setup
problems this change addresses; changing codecs alone would not solve them.

A genuinely extended macOS desktop requires an additional display. The app
therefore explains the virtual-display or display-adapter requirement rather than
promising to create one. No unverified private macOS display-driver API is shipped.
Screen Recording consent and Sunshine's first-time login/PIN remain explicit user
steps. Playback confirmation proves media opened; it is not a measurement of
end-to-end latency, frame loss or ongoing visual quality.

## Primary references

- [Docker Engine installation](https://docs.docker.com/engine/install/ubuntu/): official installation includes the Buildx and Compose plugins.
- [Ubuntu Buildx package](https://packages.ubuntu.com/noble/amd64/docker-buildx): Ubuntu uses a different package name from Docker CE.
- [mpv manual](https://mpv.io/manual/stable/): GPU contexts and JSON IPC.
- [Sunshine getting started](https://docs.lizardbyte.dev/projects/sunshine/master/md_docs_2getting__started.html) and [Moonlight setup](https://github.com/moonlight-stream/moonlight-docs/wiki/Setup-Guide): web setup and client PIN pairing.
