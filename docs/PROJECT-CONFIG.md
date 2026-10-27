# vercel.json on your own server

Vercelify reads the `vercel.json` your repository already has and applies it on your
server: redirects, rewrites (including proxying to another site), headers, `cleanUrls`,
`trailingSlash` and cron jobs. An optional `vercelify.json` next to it adds a firewall:
blocked IPs, trusted IPs, deny rules and rate limits. A project moving from Vercel keeps
its configuration as it is.

Everything runs in the proxy that already serves your apps (the engine's Traefik), so it
costs no memory and nothing extra runs per request. Rules apply a second or two after a
change, without restarting anything.

## Where the configuration comes from

| Source | When |
| --- | --- |
| The repository, at the deployed commit | Apps with [auto-deploy](AUTODEPLOY.md) on GitHub: after each production deploy finishes, and for each pull request preview. The file is read from the app's build context (Vercel's "Root Directory"). |
| The repository, now | **Read from repository** in the app's **vercel.json** tab, or `vfy config sync APP` (GitHub; the app's branch). |
| An upload | **Upload…** in the same tab, or `vfy config push APP [DIR]` from your checkout. Works for any Git host. |

An upload wins until you remove it (**Remove rules**, `vfy config rm APP`); after that
the repository's file is used again. Deleting `vercel.json` from the branch removes its
rules on the next deploy. Pull request previews always use the file from their own
branch, so you can test a rule change in a PR before it reaches production.

Check what a file would do without applying it:

```bash
vfy config check            # ./vercel.json and ./vercelify.json
vfy config check apps/web   # another folder
```

## What is supported

| Key | Behaviour here | Difference from Vercel |
| --- | --- | --- |
| `redirects` | `source`, `destination` (path or URL), `permanent` (default true), `statusCode`, `has`, `missing`, `caseSensitive`. The visitor's query string is kept, merged with the destination's. | Permanent redirects answer 301 for GET/HEAD (308 otherwise), temporary ones 302 (307). Browsers treat them the same. |
| `rewrites` to a path without parameters (e.g. `/(.*)` → `/index.html`) | Served when the app has nothing at the requested path (it answers 404), exactly like Vercel applies rewrites after its file system. Single-page apps work as on Vercel. | On Traefik older than 3.5 the fallback page is served with status 404 instead of 200 (Vercelify runs 3.7). |
| `rewrites` with parameters (e.g. `/blog/:slug` → `/posts/:slug`) | The path is rewritten before the app sees it. | Applied even if a file exists at the original path; a query in the destination is dropped (a warning says so). |
| `rewrites` to `https://…` | Proxied to that site with the path rewritten; the other site gets its own `Host`. | Addresses on this machine or a private network (`127.0.0.1`, `10.x`, `localhost`, Docker names like `vercelify-ollama`, `*.internal`) are refused with a note: the proxy can reach them, and the rewrite would put them on your public domain. `rewrite_private_targets = true` in `agent.toml` allows it. |
| `headers` | Response headers for matching paths, also on redirects and fallback pages. | See [several rules for one path](#several-rules-for-one-path). |
| `cleanUrls` | `/about.html` → `/about`, `/blog/index.html` → `/blog`, `/index.html` → `/`. | Your server must answer the clean address (most static servers do). |
| `trailingSlash` | `true` adds a slash to paths without a file extension, `false` removes it. | Same. |
| `crons` | `path` + five-field `schedule` in UTC. See [cron jobs](#cron-jobs). | No per-plan limits. |
| `has` / `missing` | `header`, `cookie`, `query` and `host`, with an optional regex `value`. | Values captured in `has` can't be used in the destination. |

`buildCommand`, `installCommand`, `outputDirectory` and `framework` are read by the build
([PLATFORM.md](PLATFORM.md#3-apps-previews-and-rollbacks)). Keys that make no sense for a
container (`functions`, `regions`, `images`…) are listed as notes in the app and in
`vfy config check` with what to do instead. Nothing is silently ignored.

### Sources

Sources use the same syntax as on Vercel (path-to-regexp):

| Source | Matches |
| --- | --- |
| `/about` | `/about` and `/about/` (a trailing slash is optional), any letter case |
| `/blog/:slug` | one segment, available as `:slug` in the destination |
| `/docs/:path*` | `/docs`, `/docs/a`, `/docs/a/b`… (`:path+` needs at least one segment, `:id?` is optional) |
| `/user/:id(\d+)` | a custom pattern for the parameter |
| `/assets/(.*)` | anything below `/assets/`, available as `$1` |

Matching is case-insensitive unless the rule has `"caseSensitive": true`. Patterns run in
the proxy as Go regexes, which have no look-ahead: a source like `/:path((?!uk/).*)` is
skipped with a note.

### Several rules for one path

The proxy picks one route per request, so rules are arranged like this:

- Redirects, `cleanUrls`/`trailingSlash`, headers and rate limits for every path
  (`/(.*)`), and a catch-all rewrite to a fixed page apply to every request, in
  Vercel's order: headers, then redirects, then rewrites.
- Rules for some paths get their own route, in this order of precedence: firewall,
  redirects with `has`/`missing`, rewrites, rate limits, headers.
- A header or rate limit for a prefix (`/api/(.*)`, `/docs/:path*`) is also applied to
  the routes of other rules inside that prefix (`/api/users/:id`).

The one case that differs from Vercel: when two path-specific rules of different kinds
match the same request, and neither is a prefix of the other, only the one with higher
precedence applies. Example: a header for `/(.*)\.js` and a rewrite for
`/legacy/:file` both matching `/legacy/a.js`: the rewrite applies, the header doesn't.

## Firewall (`vercelify.json`)

```json
{
  "firewall": {
    "blockIps": ["203.0.113.0/24", "198.51.100.7"],
    "trustedIps": ["192.0.2.10"],
    "rules": [
      { "name": "no wordpress probes", "path": "/wp-admin/(.*)", "action": "deny" },
      { "userAgent": "(?i)badbot", "methods": ["POST"], "action": "deny" },
      { "path": "/api/(.*)", "action": "rate_limit", "requests": 60, "window": 60 },
      { "action": "rate_limit", "requests": 600, "window": 60 }
    ]
  }
}
```

| Setting | Effect |
| --- | --- |
| `blockIps` | These addresses or ranges get 403. |
| `trustedIps` | Only these may visit; everyone else gets 403 (Vercel's "Trusted IPs" deployment protection). This server itself is always allowed, so cron jobs keep working. |
| `rules[].action: "deny"` | 403 for requests matching all of `path`, `userAgent` (a regex) and `methods`. A rule without any condition is refused, so a typo can't take the site down. |
| `rules[].action: "rate_limit"` | `requests` per `window` seconds (1–3600) per visitor, then 429. Without conditions it covers the whole app. |

**Visitors behind the Cloudflare tunnel.** The tunnel connects from this machine, so the
visitor's address comes from Cloudflare's `Cf-Connecting-Ip` header. That header is only
trusted on connections from this machine (loopback, and the Docker networks the proxy is
on, read from Docker each minute): a request from anywhere else can't fake it.
Rate limits count per tunnel visitor when the server's domain mode is `cloudflare`.
IPv6 ranges through the tunnel must be on a 16-bit boundary (`/48`, `/64`) without a zero
group; anything else is matched for direct visitors only, with a note.

Cloudflare's own WAF and "Under Attack" mode still sit in front in tunnel mode; this
firewall covers private and direct-mode servers too, and needs no Cloudflare account.

## Cron jobs

```json
{ "crons": [{ "path": "/api/cron/sitemap", "schedule": "0 5 * * *" }] }
```

At each scheduled minute (UTC) the agent sends a `GET` to the production URL with
`User-Agent: vercel-cron/1.0`, and `Authorization: Bearer <CRON_SECRET>` when the app has
a `CRON_SECRET` variable, as on Vercel. So the handler you wrote for Vercel works
unchanged:

```ts
if (request.headers.get("authorization") !== `Bearer ${process.env.CRON_SECRET}`) {
  return new Response("Unauthorized", { status: 401 });
}
```

- The request goes to the proxy on the same machine: no public DNS needed, and a
  sleeping app wakes up like for any visitor.
- A run may take up to 5 minutes. A run still going when the next one is due is skipped.
- Each job's last status, duration and next run show in the app; **Run now** starts one by
  hand (`vfy crons APP run /api/cron/sitemap`). Three failures in a row raise an alert.
- Jobs pause while the power guard is shutting the machine down. They don't run on
  previews.

Schedules use five fields: `*`, numbers, `a-b`, `*/n`, `a-b/n`, lists and names
(`mon-fri`, `jan`). Like standard cron, if both day of month and day of week are set, a
day matching either counts.

## Skipping builds that change nothing (Ignored Build Step)

Builds are the heaviest thing a small server does. Auto-deploy skips a push when:

- no changed file is in the app's **watch paths** (its build settings, e.g. `apps/web/**`
  and `packages/**`; `!pattern` excludes): `*` stays in one folder, `**` crosses
  folders, the last matching pattern decides;
- or the newest commit's message contains `[skip deploy]`, `[deploy skip]`, `[skip ci]`
  or `[ci skip]`.

The commit still counts as seen. GitHub shows a "Not deployed: …" status on it, and the
app shows how many pushes were skipped. When GitHub can't list the changes (more than
300 files, or the API is unreachable) the push is built, so nothing is skipped by
mistake. Monorepos without watch paths build every push, like on Vercel without
an Ignored Build Step.

## How it works

```
visitor ─▶ Traefik
            ├─ vfy-<app>-…-fw*      priority 50000   firewall (403)
            ├─ vfy-<app>-…-r*       priority 40000   redirects with has/missing
            ├─ vfy-<app>-…-rw*      priority 30000   rewrites
            ├─ vfy-<app>-…-rl*      priority 20000   rate limits for some paths
            ├─ vfy-<app>-…-h*       priority 10000   headers for some paths
            ├─ vfy-<app>-…-base     priority  5000   everything else
            ├─ vfy-<app>-<n>[-secure] the engine's base route (priority 100)
            └─ vercelify-wake       priority     1   sleeping apps
```

The agent reads each app's base routes from the engine (its routers, service and
middlewares) and writes `<state>/proxy/dynamic/vercelify-routes-<app id>.yaml` with
copies of those routers at higher priorities. Each copy keeps the base route's own
middlewares (basic auth, www redirects), and routers that only redirect http to https
are left as they are. One file per app or preview, so a problem in one never affects
another. Apps on workers are served by the main server's proxy, so their rules apply
there too.

When the app is stopped (asleep, or between deploys) its base route and service
disappear, Traefik skips these routes, and requests fall through to the wake listener
or a clean 404. The agent checks once a minute and right after every change. If it
can't read Docker for a moment, the routes stay as they are. Apps served under a
sub-path (`https://example.com/app`) aren't supported and get a note.

## Troubleshooting

| What you see | What to do |
| --- | --- |
| "this app isn't deployed yet" | Deploy the app once; its rules apply as soon as it has a route. |
| "the proxy isn't running" | `docker ps` should list `vercelify-proxy`; the agent starts it again within a minute, `sudo vercelify doctor` says why it can't. |
| A rule shows as a note | The note says why it was skipped or how it differs. `vfy config check` shows the same before you push. |
| Rules don't apply | `docker logs vercelify-proxy` shows configuration errors; `sudo cat /var/lib/vercelify/proxy/dynamic/vercelify-routes-*.yaml` shows what was generated. |

## Tests

- Unit tests for the path-to-regexp compiler, every rule kind, the generated Traefik
  configuration, cron schedules, watch-path globs and the GitHub responses
  (`cargo test -p vercelify-agent`).
- `host/tests/contract/routing.sh` runs the real agent with the engine and its own
  proxy (Traefik 3.7, and 3.4 in CI) and two apps deployed as image apps. It checks 69
  behaviours end to end, including spoofed tunnel headers from another address, a cron
  run with `CRON_SECRET`, a stopped app and removal. See [DEVELOPMENT.md](DEVELOPMENT.md).
