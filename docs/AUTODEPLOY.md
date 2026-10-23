# Deploy automatically from a branch

GitHub push webhooks can't reach a Coolify that is private to your tailnet or behind
CGNAT. So the Vercelify agent polls instead: every 60 seconds it asks the Git host for
the head of each watched branch (`git ls-remote`, a few hundred bytes, no clone) and
queues a Coolify deployment when the commit changed. It works with GitHub, GitLab,
Gitea, Bitbucket or any https Git server, and needs no open port or webhook.

## Turn it on

- **New app:** Apps → **New app** → paste the repository URL, pick the branch, leave
  *Deploy automatically when I push* on. The app builds right away and every push to
  that branch deploys it again.
- **Existing app:** open it → **Auto-deploy** tab → **Turn on auto-deploy**. Works for
  apps created in Coolify too (including GitHub App sources).
- **CLI:** `sudo vercelify autodeploy add <app-uuid> https://github.com/you/app.git main`.

Private repositories need a read-only token (for GitHub, a fine-grained token with
*Contents: read*). It is stored on the server in `/var/lib/vercelify/autodeploy.json`
(mode 0600), passed to git through the environment (never the command line), and never
returned by the API.

## Behaviour

- The first check only records the current head, so turning it on never redeploys what
  is already live.
- A deploy that Coolify refuses is retried on the next check; an app deleted in Coolify
  turns its watch off.
- While the power guard is cooling the machine (`thermal_paused`) or shutting down, new
  commits wait and deploy once it clears.
- Pushes that change nothing in the app's watch paths (Coolify's *Watch Paths* setting),
  or whose newest commit says `[skip deploy]`, `[deploy skip]`, `[skip ci]` or `[ci skip]`,
  are not built (GitHub repositories; see
  [PROJECT-CONFIG.md](PROJECT-CONFIG.md#skipping-builds-that-change-nothing-ignored-build-step)).
- After each deploy the app's `vercel.json` at that commit is applied: redirects,
  rewrites, headers, cron jobs and the firewall ([PROJECT-CONFIG.md](PROJECT-CONFIG.md)).
- Each watch shows its last check, branch head, number of automatic deploys, skipped
  pushes and the last error in the app and in `sudo vercelify autodeploy list`.
- Change the interval with `autodeploy_interval_secs` in `/etc/vercelify/agent.toml`
  (minimum 15; 0 = only when you press *Check now*).

If your Coolify *is* reachable by GitHub (direct mode with a public IP) and you use the
GitHub App, Coolify's own webhooks also deploy on push; use one or the other per app to
avoid a second build.

## Pull request previews (GitHub)

Turn on **Preview every pull request** in the app's Auto-deploy tab. On every
check the agent lists the repository's open pull requests (GitHub API; with a
token each check, without one every 5 minutes to stay inside GitHub's anonymous
limit) and:

- deploys a preview for each new pull request, and again when its head moves
  (`POST /api/v1/deploy?uuid=…&pr=N`), at most 3 per app at once
  (`max_previews` in `agent.toml`; more wait for a slot),
- removes the preview when the pull request closes or merges,
- never builds pull requests from forks: that is someone else's code running on
  your machine. Deploy one by hand in Coolify if you trust it.

No webhook, no open port: it works behind CGNAT like branch auto-deploy.

## GitHub statuses and comments

**Report to GitHub** (needs a token with *Commit statuses: write* and *Pull
requests: write*) adds:

- a `vercelify` commit status on every automatic deploy: pending, then success
  with the app's URL, or failure,
- a `vercelify/preview` status and one comment per pull request with the preview
  URL, updated (not repeated) on new pushes.

## Automatic rollback

With **Roll back automatically** (on by default for new watches) the agent
follows each deployment it started. If Coolify reports the build failed, the
previous version simply keeps running and you get an alert. If the new version
finishes but is *unhealthy*, *exited* or *restarting* within three minutes, the
agent rolls back to the commit that was live before (Coolify's rollback endpoint,
which reuses the kept image; on older Coolify versions it pins the commit and
deploys), reports it on GitHub and raises an alert. The next push deploys
normally again.

The installer keeps the last 5 images of each app (Coolify's
`docker_images_to_keep`), so rollbacks are usually instant; the Deployments tab
marks each rollback target *instant* or *rebuild*.
