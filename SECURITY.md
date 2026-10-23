# Security Policy

Vercelify runs on machines that host your apps and hold credentials (Coolify API
tokens, Tailscale keys, GitHub tokens), so we take security reports seriously.

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

In scope: code in this repository. Vulnerabilities in Coolify, Tailscale, Cloudflare,
Traefik or other upstream projects should be reported to those projects.
