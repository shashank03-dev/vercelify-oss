# Deploy automatically from a branch

Your server may be private to your network or behind CGNAT, where GitHub's push webhooks
can't reach it. So the engine polls instead: every 60 seconds it asks the Git host for
the head of each app's branch (`git ls-remote`, a few hundred bytes, no clone) and queues
a deployment when the commit changed. It works with GitHub, GitLab, Gitea/Forgejo,
Bitbucket or any https or ssh Git server, and needs no open port. Webhooks, when the
server is reachable, only make it sooner.

## Turn it on

- **New app:** Apps → **New app** → paste the repository URL, pick the branch. The app
  builds right away and every push to that branch deploys it again.
- **Existing app:** its **Deploy on push** card turns it on or off, shows the last check
  and branch head, and has **Check now**.
- **Private repositories** need credentials the server stores sealed: a GitHub App
  (*connect a GitHub App* in **New app**, or `sudo vercelify github-app`), or a read-only
  token or deploy key (`sudo vercelify git-source add NAME`). They reach git through the
  environment, never the command line, and are never returned by the API.
- **Instant deploys** come from the GitHub App's webhook when the server is reachable
  from outside (`sudo vercelify remote on`). Connect the app after turning that on, so
  its webhook starts active. Each time the agent starts, it points the webhook at the
  server's current public address, so a quick tunnel's new address after a reboot
  keeps pushes instant. Polling covers anything a webhook misses.

## Behaviour

- Turning it on never redeploys what is already live: the first check records the head.
- While the power guard is cooling the machine (`thermal_paused`) or shutting down, new
  commits wait and deploy once it clears. On a newer push, a queued older deployment of
  the same app is dropped (*superseded*).
- Pushes that change nothing in the app's watch paths (the app's settings, e.g.
  `apps/web/**`), or whose newest commit says `[skip deploy]`, `[deploy skip]`,
  `[skip ci]` or `[ci skip]`, are not built (GitHub repositories; see
  [PROJECT-CONFIG.md](PROJECT-CONFIG.md#skipping-builds-that-change-nothing-ignored-build-step)).
- After each deploy the app's `vercel.json` at that commit is applied: redirects,
  rewrites, headers, cron jobs and the firewall ([PROJECT-CONFIG.md](PROJECT-CONFIG.md)).
- Change the interval with `autodeploy_interval_secs` in `/etc/vercelify/agent.toml`
  (minimum 15).

## Instant deploys (webhooks)

With a public address (a Cloudflare tunnel, or forwarded ports), push events can reach
the server. `sudo vercelify webhook SOURCE` (or the **Deploy on push** card) makes a
secret for a git source and prints the URL to add in the repository's webhook settings
(GitHub, GitLab, Gitea/Forgejo). A webhook only tells the server to look now: the commit
to deploy still comes from `git ls-remote`, so a forged event can't deploy anything
else. A GitHub App sets its webhook up by itself.

## Pull request previews (GitHub)

Turn on previews in the app's **Pull request previews** card. On every check the engine
lists the repository's open pull requests (GitHub API; give the app credentials if it
has many apps, as GitHub limits anonymous calls to 60 an hour) and:

- deploys a preview for each pull request, and again when its head moves, at
  `pr-<n>-<app>.<domain>`, at most 3 per app at once (the card sets the limit; the oldest
  stops when a new one starts),
- uses the variables marked for previews (for example a separate database URL),
- removes the preview when the pull request closes or merges,
- never builds pull requests from forks: that is someone else's code running on your
  machine.

## GitHub statuses and comments

With a token that may write them (a GitHub App always can; a token needs *Commit
statuses: write* and *Pull requests: write*), the engine adds:

- a `vercelify/<app>` commit status on every automatic deploy: pending, then success
  with the app's URL, or failure,
- a `vercelify/<app> (preview)` status and one comment per pull request with the preview
  URL, updated (not repeated) on new pushes.

## When a deploy goes wrong

- **The build fails, or the new version never answers its health check:** nothing
  switches; the previous version keeps serving and you get an alert with the log.
- **The new version goes live, then crashes** (exits, runs out of memory, or restarts
  twice) within three minutes: the engine rolls back to the deployment that was live
  before, by itself, and says so in the log, on GitHub and in an alert. The next push
  deploys normally again.

The engine keeps the newest 5 images of each app (and every image a server still
runs), so rollbacks start at once without rebuilding.
