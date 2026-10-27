# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
