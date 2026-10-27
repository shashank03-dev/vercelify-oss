# Security Policy

Vercelify runs on machines that host your apps and hold credentials (device tokens,
the master key that seals app secrets, mesh keys, Git and Cloudflare tokens), so we
take security reports seriously.

## Supported versions

Only the latest release and `main` receive security fixes.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately through GitHub:
[Security → Report a vulnerability](https://github.com/shashank03-dev/vercelify-oss/security/advisories/new).

Please include:

- what is affected (agent, cockpit, host scripts, installer…) and the version or commit,
- steps to reproduce or a proof of concept,
- the impact you expect.

You should get an acknowledgement within 7 days. We will keep you updated while we
work on a fix and credit you in the advisory unless you prefer to stay anonymous.

## Scope

In scope: code in this repository. Vulnerabilities in Docker, BuildKit, Traefik,
WireGuard/boringtun, Cloudflare or other upstream projects should be reported to those
projects.

## What is exposed

- The agent API (TCP 7420) listens on 127.0.0.1 and the mesh address only, behind
  per-device bearer tokens (hashed at rest, revocable, role-checked, audited).
- The mesh control port (TCP 7443) and WireGuard (UDP 51820) are the only ports meant
  for outside the LAN (`vercelify remote`: a router forward, a Cloudflare quick tunnel
  to the control port, or your own Cloudflare tunnel). The control port accepts sealed,
  single-use, rate-limited joins, SPAKE2 pairing and the relay (WireGuard packets,
  already encrypted end to end) only.
- With remote access on, the main server posts its current relay address, signed with
  the mesh's coordinator key, to a public message drop (ntfy.sh) under a topic derived
  from that key. Machines that lose the server read it there; a note that doesn't
  verify is ignored. Set `VERCELIFY_MESH_BEACON=""` on the daemons, and
  `MESH_BEACON_URL` for `vercelify remote`, to use your own ntfy server or none.
- Apps are published through the proxy on 80/443. Containers can't reach the mesh
  beyond its ACL; Ollama and the image registry are firewalled to the mesh.
- Installers verify SHA-256 checksums of the release binaries and the Mac disk image.
