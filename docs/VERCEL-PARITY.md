# Vercel feature parity

Every area of Vercel's documentation (October 2026), what you get on your own laptop
instead, and where the two differ. Status:

- **✅**: verified end to end against real software while building Vercelify (Coolify 4.4,
  Traefik 3.7, GitHub, Ollama; see [Testing](../README.md#testing)).
- **Coolify** / **Cloudflare**: provided by Coolify or by Cloudflare in tunnel mode;
  Vercelify sets it up or links to it.
- **Different**: the same result reached another way, because your app runs as a
  container on a machine you own.
- **Not offered**: with the reason it doesn't fit a free, self-hosted setup on weak
  hardware.

## Build and deploy

| Vercel | Vercelify | Status |
| --- | --- | --- |
| Deploy from Git (GitHub, GitLab, Bitbucket) | **Auto-deploy from a branch** by the agent: it polls `git ls-remote`, works behind CGNAT with no webhook ([AUTODEPLOY.md](AUTODEPLOY.md)), or Coolify's GitHub/GitLab/Gitea App + webhooks. Zero-config Nixpacks builds (Next.js, Vite, Nuxt, Astro, SvelteKit, Remix, Go, Python, Rust…), Dockerfile, Docker Compose | ✅ |
| Preview deployment per pull request, with a URL | Webhook-free previews (`pr-<n>-<app>.<domain>`), updated on every push, removed when the PR closes; forks never built; a per-app limit | ✅ |
| GitHub deployment statuses and preview comment | Commit status per deploy and one comment per PR with the preview URL | ✅ |
| Zero-downtime deploys | Coolify rolling update with health checks | ✅ |
| Instant Rollback | Coolify's rollback endpoint reusing the image it kept (5 images per app are never pruned), **Roll back** in the app, `vfy rollback APP COMMIT`; each deployment row says "instant" or "rebuild" | ✅ |
| Automatic rollback (unhealthy after promotion) | The previous version goes back live when a new one comes up unhealthy within 3 minutes | ✅ |
| Ignored Build Step, monorepo skipping | Pushes that change nothing in the app's Coolify **watch paths**, or whose commit says `[skip deploy]`/`[skip ci]`, aren't built; GitHub shows "Not deployed" ([PROJECT-CONFIG.md](PROJECT-CONFIG.md#skipping-builds-that-change-nothing-ignored-build-step)) | ✅ new |
| Deploy Hooks | Coolify deploy webhook + `POST /api/v1/deploy?uuid=…` | ✅ |
| Build cache, build queues | Docker layer cache; builds run one at a time, paused when the machine is too hot | Different |
| Build on a strong machine for small ones | Tailnet image registry + multi-arch builds; Pi workers only pull images | ✅ |
| Environments (production, preview, development) and environment variables | Coolify environments, build-time vs runtime variables, preview-only values, shared variables; `.env` import in the app; `vfy env pull` | Coolify + ✅ |
| Custom environments (staging) | A second Coolify app for the staging branch with its own domain and variables | Different |
| System environment variables (`VERCEL_URL`, `VERCEL_GIT_COMMIT_SHA`…) | Coolify's `COOLIFY_URL`, `COOLIFY_FQDN`, `COOLIFY_BRANCH`, `SOURCE_COMMIT`. `VERCEL=1` is deliberately not set: frameworks would then build for Vercel's platform | Different |
| Deployment retention | Previews removed when their PR closes; old images pruned except the 5 newest per app | ✅ |
| Rolling Releases (percentage of traffic to a new version) | Not offered: it needs two versions running side by side, which doubles memory on an 8 GB machine. Zero-downtime deploys plus automatic rollback cover the "bad deploy" case | Not offered |
| Skew Protection | Not offered (one version runs at a time; rolling deploys switch over once healthy) | Not offered |
| Vercel Drop, deploy files without Git | `vfy` deploys from Git; Coolify can deploy a Docker image or Compose file | Different |
| Microfrontends, Services (several apps on one domain) | `rewrites` to another app's URL in `vercel.json`, or Coolify apps on paths of one domain | Different |

## Routing and CDN

| Vercel | Vercelify | Status |
| --- | --- | --- |
| `vercel.json` redirects (incl. `has`/`missing`, `statusCode`, query strings) | Read from the repository and applied by the proxy ([PROJECT-CONFIG.md](PROJECT-CONFIG.md)) | ✅ new |
| `vercel.json` rewrites (to a path, to another site; after the file system) | Same semantics: a fixed-path rewrite applies when the app answers 404, so SPAs work as on Vercel | ✅ new |
| `vercel.json` headers | Response headers for matching paths, also on redirects and fallbacks | ✅ new |
| `cleanUrls`, `trailingSlash` | Redirects as on Vercel | ✅ new |
| Bulk redirects, project-level routing rules in the dashboard | Upload a `vercel.json` (`vfy config push`) without a commit; up to 200 rules per app | Different |
| Routing Middleware (code before the cache) | Your framework's middleware runs in its server (`next start` runs `middleware.ts`) | Different |
| Custom domains, automatic HTTPS | Per-app domains in the app; Cloudflare Tunnel + Universal SSL (automated by `vercelify domain`) or Traefik + Let's Encrypt with a public IP | ✅ |
| `*.vercel.app` URLs | Every app gets `<name>.<domain>` (or `<uuid>.<tailscale-ip>.sslip.io` with no domain) | ✅ |
| Global CDN, caching static assets | Cloudflare in front (tunnel mode) caches static files at its edge; your laptop is the origin | Cloudflare |
| ISR, Data Cache, image optimisation, OG images | Run in your framework's server (`next start` / standalone): ISR, `next/image`, `@vercel/og` all work, cached on the server's disk | Different |
| Custom error pages | Your framework's 404/500 pages | Different |

## Compute

| Vercel | Vercelify | Status |
| --- | --- | --- |
| Functions, Fluid compute, Edge runtime | Your app's server runs as a container: API routes, Route Handlers, Server Actions, WebSockets, streaming. No cold starts, no execution time limits, no per-invocation billing | Different |
| Cron Jobs (`crons` in `vercel.json`, `CRON_SECRET`) | Run by the agent at the scheduled minute (UTC), with `Authorization: Bearer $CRON_SECRET` and `vercel-cron/1.0`; last run, next run and **Run now** in the app; alerts after 3 failures | ✅ new |
| Scheduled tasks inside the container | Coolify **Scheduled Tasks** (cron syntax, a command in the container) | Coolify |
| Scale to zero | Sleeping apps: idle apps stop, the next visit wakes them in a second or two | ✅ |
| Queues, Workflows, Sandbox | A Redis/Valkey or Postgres-backed queue from Coolify's catalogue (BullMQ, Graphile Worker…); not a managed service | Different |

## Storage

| Vercel | Vercelify | Status |
| --- | --- | --- |
| Postgres, Redis/KV and other Marketplace databases | One-click PostgreSQL, MySQL/MariaDB, MongoDB, Redis/KeyDB/Dragonfly, ClickHouse | ✅ (Postgres + Redis created from the app) |
| Blob | MinIO (S3 API) from Coolify's catalogue | Coolify |
| Edge Config / Global Config, Feature Flags | Environment variables (redeploy to change), or a self-hosted flag service from Coolify's catalogue (Unleash, Flagsmith) | Coolify catalogue |
| Storage placement | SSD/HDD tiers: move a database's volume between disks with data and owners preserved | ✅ |
| Backups | Nightly, encrypted backups to the HDD, another server or S3, including Coolify's secrets ([BACKUPS.md](BACKUPS.md)) | ✅ |

## Security

| Vercel | Vercelify | Status |
| --- | --- | --- |
| Firewall: custom rules, IP blocking, rate limiting | `vercelify.json` `firewall`: blocked IPs, path/user-agent/method deny rules, rate limits per visitor; tunnel visitors identified safely ([PROJECT-CONFIG.md](PROJECT-CONFIG.md#firewall-vercelifyjson)) | ✅ new |
| Deployment Protection: Trusted IPs | `firewall.trustedIps` | ✅ new |
| Deployment Protection: password | Coolify HTTP Basic Auth per app (kept in front of all rules) | Coolify ✅ |
| Deployment Protection: Vercel Authentication | Cloudflare Access in front of `pr-*` hostnames, or keep the app private to your tailnet (the default without a domain) | Cloudflare |
| DDoS mitigation, Attack Challenge Mode, Bot Management, BotID | Cloudflare in tunnel mode (your home IP is never published; "Under Attack" mode and bot rules on the free plan) | Cloudflare |
| Audit log, tokens | Per-device tokens (hashed, revocable) and an append-only audit log of every change | ✅ |
| Secrets, sensitive variables | Coolify encrypts variables; `vfy env pull` writes a mode-600 file | Coolify |
| SAML SSO, RBAC, teams | Coolify teams, members and roles | Coolify |
| OIDC federation, Secure Compute | Not offered (no cloud accounts involved) | Not offered |

## Observability

| Vercel | Vercelify | Status |
| --- | --- | --- |
| Build logs, runtime logs | Live build and runtime logs in the app and `vfy logs` | ✅ |
| Monitoring, alerts | CPU, memory, disks, temperatures, per-container usage, 24 h history; alerts to ntfy, Discord, Slack or a webhook for failed deploys, rollbacks, cron failures, disks, power and more | ✅ |
| Uptime checks | Each app URL checked by the agent; worker servers checked by the main one | ✅ |
| Web Analytics, Speed Insights | One-click Plausible or Umami from Coolify's catalogue | Coolify catalogue |
| Log drains, OpenTelemetry tracing | Ship container logs with a collector from Coolify's catalogue (e.g. Vector, Grafana Alloy) | Coolify catalogue |

## Tools

| Vercel | Vercelify | Status |
| --- | --- | --- |
| Vercel CLI (`deploy`, `logs`, `env pull`, `rollback`, `crons`, `redirects`…) | `vfy apps / deploy / logs / rollback / env pull / open / status / alerts / backup / sleep / config / crons` on the Mac; `vercelify` on the server | ✅ |
| REST API | The agent's API (`/v1/…`) and Coolify's API | ✅ |
| AI SDK, AI Gateway | Local models with Ollama on your own servers, an OpenAI-compatible API on the tailnet ([AI.md](AI.md)) | ✅ |
| Toolbar, Comments, Draft Mode | Not offered: review previews on GitHub (the preview link is commented on the PR) | Not offered |
| Teams, organisations | Coolify teams | Coolify |
| Scale out | Raspberry Pi / spare PCs as Coolify servers over Tailscale (**Add to a pool**, `vercelify add-worker`) ([SERVERS.md](SERVERS.md)) | ✅ |

## Honest differences

- **One origin.** Your laptop is a single machine at one location. Cloudflare
  caches static files worldwide, but dynamic requests travel to your home.
  Expect about 20–80 ms of extra latency for far-away visitors.
- **Availability = your power and internet.** The power guard shuts down
  cleanly on a dying battery and restores containers afterwards. It can't
  replace a second machine; for important apps, keep a Pi worker or nightly
  off-site backups.
- **Capacity.** An i5-7th-gen / 8 GB laptop comfortably runs a handful of
  Next.js apps plus Postgres and Redis. Builds are the heaviest part, so they
  run one at a time, and pushes that change nothing are not built at all.
- **No per-request scaling.** Containers are always on (or asleep). That means
  no cold starts, but also no scaling to a thousand instances, and no gradual
  traffic splitting between two versions.
