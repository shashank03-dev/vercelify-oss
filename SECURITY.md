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

## Preview trust boundary

Previews are private by default and receive only preview-only values or deliberately
shared normal values. Making a value nonsecret does not grant sharing. Production
database references are refused before credentials are resolved. Runtime previews
use a dedicated ingress-only network, with no production/shared proxy bridge or
outbound internet route. Docker 28+ and Compose 2.24.4+ are required for these paths;
unsupported isolation fails closed. Existing previews are retired during the policy
upgrade, so review variables before redeploying them.

Vercelify is intended for repositories you trust on hardware you administer, not
hostile multi-tenant execution. Image builds still execute repository instructions
through Docker/BuildKit; private ingress is not a build sandbox. Code that receives
an approved secret can deliberately reveal or misuse it despite log redaction.
Fork builds remain disabled. Runtime isolation does not protect against Docker or
kernel exploits, a compromised host administrator, or a compromised ingress proxy.

## Guest invitations

An Owner may expose a selected private preview on a separate public HTTPS guest
host. Its invitation is a bearer secret: anyone receiving the complete link can
redeem it. The gateway removes the fragment before explicit confirmation, then
uses a host-only Secure/HttpOnly cookie. Metadata stores only credential hashes;
secrets are shown once and are never automatically posted to a PR.

All assets, advanced routes, wake requests and upgrades require a live invitation
session. Revocation, expiry, PR close, generation replacement, privacy changes and
manual app stop end access; previously downloaded bytes remain with the recipient.
Streams and sockets are rechecked every second with bounded inspection time.
Browser workers and frames are unavailable through guest hosts. Operator DNS/TLS,
the host, Docker and Traefik remain trusted; a guest invitation is not a sandbox
for malicious repository code. The private network URL remains available.


## Power and recovery scope

Battery conservation is opt-in. The guard acts on platform-labelled containers and
exact installed core/builder names; unrelated containers are excluded. Its own pause
IDs and generation-bound power-sleep intent persist. Recovery uses declared state
and admitted wake, respecting manual app stops and closed PRs. Root-owned host
configuration and journals remain trusted. These software controls do not certify
battery health, firmware AC power-on, storage survival after sudden loss or router/
ISP recovery. Physical qualification is recorded separately; backup-copy previews
remain disabled pending their independent data-scrubbing and isolation gates.
