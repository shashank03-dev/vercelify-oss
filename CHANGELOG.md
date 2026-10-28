# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- The app says when a newer release is out: "Update the app" at the bottom of the
  sidebar opens the release with the `.dmg`, and server cards show `0.2.5 → 0.3.0`
  against the newest release, not only against each other.
- Development on Linux: `./dev setup` on Debian or Ubuntu installs everything itself
  (system packages, Docker, Node 22, Rust, zig), so `./dev check` runs natively there.

### Fixed

- On a Linux machine that is also a Vercelify server, `./dev agent` and the test suites
  that run an engine of their own (live e2e, routing, multinode, promote, the Docker
  Rust tests) replaced the server's proxy and removed its apps' containers. They now
  refuse to run there.
- Git deployments failed on every installed server ("could not create the BuildKit
  builder: … read-only file system"): the agent's sandbox kept `/root` read-only, where
  `docker buildx` keeps its builder. The agent's unit now lets it write `/root/.docker`.
- An update from a server installed before 0.2.0 left the agent stopped: its
  `agent.toml` still listed the `tailscale` listen address, which the agent refused.
  It is now skipped with a warning.
- A rolled-back update also puts back `agent.toml`, so the previous agent can read its
  configuration again.
- A fresh install right after the machine booted stopped at "apt-get update failed": Debian's
  own daily apt run holds apt's locks for the first minutes. The installer now waits for it.
- While a new database's image was still downloading (minutes on home internet), the
  reconciler waited for it, so crashed apps and databases didn't come back meanwhile. The
  download now runs on its own; everything else recovers within 30 seconds.
- `vercelify doctor` reported the proxy as running when another program held port 80
  (an old Coolify proxy, say) and it served nothing; it now says so.
- Remote desktop from the Mac (app mode) showed "No video received from host": Sunshine's
  ~1.4 kB video packets arrive in IP fragments over the mesh's 1280-byte MTU, and the Mac's
  userspace network stack dropped fragments. It now reassembles them, and its UDP sockets
  hold a keyframe's burst (1024 packets) instead of 64.

## [0.2.5] - 2026-10-27

### Fixed

- The 0.2.4 release had no Mac app: signing couldn't find its certificate on the build
  machine. 0.2.5 carries the 0.2.4 signing fix.

- Remote desktop: Sunshine aborted right after start, at its tray icon, when run as the
  desktop user's service (GNOME on X11), so Moonlight never connected. `vercelify desktop
  on` now turns the tray off in the user's Sunshine settings (a `system_tray` line the
  user set stays) and clears a failed state left by earlier crashes.

## [0.2.4] - 2026-10-27

### Fixed

- The Mac app kept asking for Screen Recording although it was switched on: it carried
  only the linker's signature, which macOS can't hold the permission for. Releases are
  now signed with a stable (self-signed) certificate under the app's own id, so the
  permission sticks and survives updates; source builds are signed too, and
  `install-mac.sh` clears the stale entry when the app's signature changes.

## [0.2.3] - 2026-10-27

### Added

- `sudo vercelify desktop on` (and the installer's *Set up remote desktop?*, asked when
  the machine has a desktop): installs Sunshine from its release for this distribution,
  checked against GitHub's SHA-256 for the file, and starts it on the logged-in desktop
  for the Mac app's **Launch Moonlight**. The agent follows Sunshine as the desktop
  user's own service.

### Fixed

- `vercelify uninstall` left the Cloudflare quick tunnel service running.

## [0.2.2] - 2026-10-27

### Added

- GitHub App webhooks follow the server: on start, the agent points each GitHub
  App's webhook at its current public address (a Cloudflare quick tunnel gets a new
  one on every restart), so pushes keep deploying instantly instead of waiting for the
  minute's poll.

## [0.2.1] - 2026-10-27

### Added

- `vercelify remote on` works on any network: when the router won't forward the ports
  (or behind CGNAT) it opens a free Cloudflare quick tunnel to the private network's
  control port, with no account or domain, and switches back to a direct forward when
  it can. The main server posts its address, signed with the mesh's key, to a public
  message drop (ntfy.sh), so Macs and servers that lose it after a restart find it
  again. The installer's *Reach this server from anywhere?* now covers this.

### Fixed

- `vercelify remote on` asked the router to forward empty ports when `agent.toml` didn't
  set `mesh_port` and `mesh_control_listen` (the default): the lookup failed under the
  CLI's `pipefail`.

## [0.2.0] - 2026-10-27

### Added

- First open-source release: agent, cockpit desktop app, host installer, `vfy` CLI,
  docs, and community files (contributing guide, code of conduct, security policy,
  issue and pull request templates).
- `vercelify remote on|off`: the router forwards the private network's two ports by
  itself (UPnP or NAT-PMP), so Macs and servers join from anywhere without Cloudflare
  or manual port forwarding. The installer asks.
- `install-mac.sh --pair CODE`: install the Mac app and pair it in one command; the
  server installer and `vercelify pair` print it.

### Removed

- The migration from Coolify (`vercelify migrate`) and every Tailscale fallback
  (`pair --tailscale`, the `"tailscale"` listen address, `tailscale0` firewall rules).
  `vercelify pair --host HOST` pairs over an address the Mac reaches directly.

### Security

- `install-mac.sh` verifies the disk image's published SHA-256 before installing it.
- The firewall's private ports are the ones this stack uses (Ollama 11434, the image
  registry 5055) instead of Coolify's.
- `rustls-webpki` 0.103.16 and `webpki-roots` 1.0.11 (RUSTSEC-2026-0382, -0381).

### Fixed

- `vercelify uninstall` removes the image registry container (`vfy-registry`).

## [0.2.0]

- Version the code base was published at.
