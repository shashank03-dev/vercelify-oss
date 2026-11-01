# Repository audit — 1 November 2026

Audited release **1.0.0**, commit **6407cf7**, on `fix/m7-verification`.
`git fetch --prune origin` completed and `git merge --ff-only origin/main`
reported **Already up to date**; HEAD and origin/main have zero divergence.
Pre-existing edits and untracked work were preserved. No product fixes,
commits, pushes, public vulnerability reports, or live-server changes were made.

**17 actionable findings: 0 P0, 6 P1, 10 P2, 1 P3.** Fix the host trust-boundary
and restore problems first. Passing regression suites do not cover these cases.

> **Status (1 November 2026, later the same day):** all 17 findings are addressed —
> 16 by code/documentation changes with regression tests, A15 by the local film work
> itself. See [Resolution](#resolution--1-november-2026) for each fix, its tests and
> what is still unverified.

## Resolution — 1 November 2026

Fixed on `fix/m7-verification` on top of `6407cf7`, P1 first. Each regression test
named below fails on the audited code and passes now.

| ID | Status | Fix | Regression tests |
| --- | --- | --- | --- |
| A01 | Fixed | `compose::up` writes the env file and override with `create_new`/0600 into a fresh private directory under the agent's work dir (outside the checkout), removed after `up`. Before anything runs, `check_tree` refuses any symbolic link in the checkout that leads outside it or nowhere (Compose follows them as root for `.env`, env files, mounts, configs, secrets, build contexts). `detect::plan` refuses a build context, Dockerfile or compose file that resolves outside the checkout. | `compose::tests::generated_files_stay_out_of_the_checkout` (symlinked `.vercelify/app.env` and symlinked `.vercelify`, host file untouched, nothing left in checkout or scratch), `compose::tests::tree_links`, `detect::tests::contexts_and_files_stay_in_the_repository` |
| A02 | Fixed | `compose::check_model` validates the model real Compose resolves from the repository file plus our override (`config --no-env-resolution`) before `up`, for every role (no unrestricted mode exists, so there is nothing for a deployer to escalate to): no privileged, extra capabilities, devices, host/`container:` namespaces, `cgroup_parent`, unsafe `security_opt`, Docker API socket, provider services, published ports, external/driver-option volumes, foreign external networks, or files outside the checkout (binds, env files, configs, secrets, build contexts/Dockerfiles, build SSH/host network/entitlements). The override resets every service's `ports` (`!reset`). Inline compose files get a textual pre-check when saved, so the API refuses them with 400. | `compose::tests::model_policy` (29 denied, 8 allowed shapes + top-level cases), `compose::tests::real_compose_model_is_checked` (real Compose), `compose::tests::refused_model_is_not_started`, `compose::tests::saved_files_are_prechecked`, agent e2e: deployer and owner `POST /v1/platform/apps` with a privileged/docker.sock compose file → 400 |
| A03 | Fixed | `project::pin_rewrites` resolves each external rewrite's name on every reconcile (and in `config check`), drops it when any address is private, loopback, link-local, CGNAT, reserved, multicast, IPv4-mapped/NAT64-private, or when it doesn't resolve, and records the checked addresses. `routing.rs` gives Traefik those IPs (Host header and TLS `serverName` kept), never the name, so DNS rebinding can't redirect the proxy. Traefik does not follow upstream redirects. Verified against real Traefik v3.7: a pinned route to a public HTTPS site returned its page (200, correct title). | `project::tests::rewrite_names_are_resolved_and_pinned` (`127.0.0.1.sslip.io`, LAN, mixed, unresolvable, IPv6), `routing::tests::pinned_rewrites_connect_to_checked_addresses` |
| A04 | Fixed | Writable bind mounts are refused (data belongs in named volumes); read-only binds and configs must come from the checkout. The engine moves a compose checkout to `<work>/compose/<project>/<deployment>` and keeps it while containers may read it; older ones are pruned only after a newer deployment is healthy, all on app/preview removal. | `compose::tests::model_policy` (writable `./persist:/data` refused), `real_compose_model_is_checked`; `tests/engine_docker.rs` now mounts `./suffix.txt:ro`, checks the page after checkout cleanup, after `docker restart` and after a redeploy (old checkout pruned), and that app deletion removes it — **run and passed on real Docker** (Colima Linux VM) |
| A05 | Fixed | `--instance` builds a manifest (Vercelify volumes `vfy-*`/`vfyv2-*` and the storage disk's; others need `--all-volumes`), stops only Vercelify's containers plus any container using a listed volume, re-queries and aborts if any writer still runs — then restarts everything it stopped and the agent, having changed no live file. Agent stop failure also aborts. | bats: “a writer that won't stop means nothing changes, and everything starts again”; “each of Vercelify's volumes comes back exactly; others are left alone” (unrelated container never stopped; `--all-volumes` scope) |
| A06 | Fixed | Each volume is copied with `rsync --delete` into an empty staging tree beside it and switched into place; the previous tree is kept as `<volume>.before-restore-<stamp>` for recovery. No `--delete` on the global Docker volume directory. `docker volume create` registers volumes on a fresh machine. | bats (real rsync): file written after the snapshot is gone from the live volume and kept in the recovery copy |
| A07 | Fixed | `backup setup` keeps AWS/B2/Azure/Google credentials, region and retention from the existing `backup.env` unless given anew; writes `backup.env.new`, validates/creates repositories with it, and only then renames it into place. | bats: “setup again … credentials and retention are kept unless given anew” (no flags, new destination, new key only, failing setup leaves the file byte-identical) |
| A08 | Fixed | Instance restore puts back the token registry (`tokens_file`, default `/var/lib/vercelify/tokens.json`) and `agent.token` before the agent starts; machine settings (`agent.toml`, install/tunnel files) are kept. Policy: device access is as of the backup; tokens revoked after it must be revoked again (printed, documented). | bats: “paired devices' tokens come back; this machine's settings stay” (content, mode 0600, ordered before agent start) |
| A09 | Fixed | `write_private` uses a per-write unique temp file (`create_new`, removed on failure). A `state.lock` (`flock`, readable 0644 so a user-run pairing and a root daemon share it) is held across load-check-write in `save_own` and every `State::save`. | `state::tests::concurrent_writes_each_land_whole` (8×100 concurrent writes, no errors, no temp files left), `state::tests::the_lock_is_exclusive`, `daemon::tests::its_own_update_waits_for_a_pairing_in_progress` |
| A10 | Fixed | Values with `'`, `\`, CR or LF use double quotes with `\`, `"`, `$`, newline escapes; single quotes only for values with none of them (real Compose also treats `\'` as an escape inside single quotes, a second case found while fixing this). | `compose::tests::env_values_stay_literal_in_real_compose` (apostrophes, `${…}`, `$$`, `\$`, newlines, CRLF, quotes, trailing backslash through real Compose) |
| A11 | Fixed | HTTP readiness passes only on 200–399, matching the `HealthCheck` contract; a 4xx keeps the previous deployment serving. | `health::tests::readiness_needs_2xx_or_3xx` (real HTTP server: 404/401/403/429 fail; 200/204/302 pass) |
| A12 | Fixed | `volume_remove` checks the exit status: “no such volume” is success, a refusal is a `Conflict` naming Docker's reason, so the retained-volume endpoint keeps its record. | `docker::tests::volume_remove_reports_refusal` |
| A13 | Fixed | `pick` never guesses a database (by image or by port); one non-database service with a port wins, else a web-named one, else the only one; several plausible services require `runtime.service`. The routed service's single declared port is used when `runtime.port` is unset. | `compose::tests::routed_service_is_not_the_database`, updated `routed_service` |
| A14 | Fixed | In auto mode a repository with no Dockerfile and no detectable language deploys its compose file (Compose's filename order); the app is converted to a compose app (`apps::adopt_compose`), so placement, previews and cleanup treat it as one. Repositories whose language is detected keep their compose file for local development. Guide corrected. | `detect::tests::repositories_with_only_a_compose_file`, `apps::tests::repository_app_adopts_its_compose_file` |
| A15 | Resolved locally (not by this change) | The untracked `launch-film` sources were reconciled by separate local work earlier on 1 November; `npm run check` now passes (tsc, lint-film 0 errors, grid-check pass with 1 warning). Left untouched and uncommitted. | `npm run check` |
| A16 | Fixed | README states that the second screen needs FFmpeg on the Mac (`brew install ffmpeg` or `install-mac.sh --with-display`) and that Moonlight is only for desktop control. | Documentation |
| A17 | Fixed | Integration triggers cover whole crates (`crates/vercelify-platform/**`, core, mesh, `agent/**`, manifests, host CLI, installer). New `platform-docker` job runs the `VERCELIFY_DOCKER_TESTS` suites (compose, engine, databases, storage). CI gains a website build job. | Workflow YAML parsed; the new jobs have not run on GitHub yet |

The design doc (§4.2, §6.3, §7.2, §9, §13.4, §14), `PLATFORM.md`, `BACKUPS.md` and
`PROJECT-CONFIG.md` describe the new behaviour.

### Checks after the fixes

| Check | Result |
| --- | --- |
| `cargo test --workspace --all-features` (macOS) | 599 passed, 0 failed, 8 ignored (the Docker suites, run below) |
| `VERCELIFY_DOCKER_TESTS=1 cargo test -p vercelify-platform -- --include-ignored` (root, Colima Linux VM, real Docker + Compose v5.1.4) | 151 passed, 0 failed: 149 unit (incl. `compose_up_and_down`), `databases_docker`, `engine_docker` |
| `cargo clippy --workspace --all-targets -D warnings`; mesh with `daemon` | Clean |
| `cargo fmt --all --check` | Clean |
| `bats host/tests` (Debian dev image) | 222 passed (backup suite 14, 5 new) |
| ShellCheck (all host/dev/installer scripts) | Clean |
| Generated TypeScript types | Unchanged |
| Real Traefik v3.7, pinned HTTPS rewrite shape | 200 with the origin's page |
| `launch-film` `npm run check` | Pass |

### Remaining limitations

- The new GitHub jobs (`platform-docker`, `website`) were not run on GitHub; the same
  Docker suites passed locally on Linux.
- `routing.sh`, `multinode.sh`, `promote.sh` and the mesh namespace contracts (sudo,
  Linux namespaces) were not rerun; the pinned-rewrite route was checked by hand against
  real Traefik, not through `routing.sh`.
- Compose features read while resolving the model (`include`/`extends` from an absolute
  path) are judged by their result; the files themselves are read by Compose before the
  check. `--no-env-resolution` needs Compose 2.30 or later; an older Compose fails the
  deployment with its error rather than skipping the check.
- The Compose boundary has no owner-only unrestricted mode: privileged compose apps are
  refused for every role.
- Restore was exercised with stub Docker/systemd and real rsync, not against a live
  database; previous volume trees are kept until the user removes them (disk use).
- Tokens revoked after the backup become valid again on restore (documented policy).
- The original `docs/audits/2026-11-01/platform-probes.rs` targets the old `compose::up`
  signature and no longer compiles; the regression tests above replace it.

## Severity scale

These are engineering risk scores, combining consequence, prerequisites and
likely exposure; they are **not CVSS scores**.

| Priority | Score | Meaning |
| --- | --- | --- |
| P0 | 9.5–10 | Emergency: broadly exploitable compromise, unavoidable catastrophic data loss, or an outage requiring immediate intervention. None established here. |
| P1 | 7.5–9.4 | High impact: host compromise, data loss/corruption, or a seriously unsafe recovery path under a concrete trigger. Fix before expanding use. |
| P2 | 4.0–7.4 | Material reliability, correctness, workflow, or verification failure. Schedule a fix. |
| P3 | 0.1–3.9 | Lower-impact documentation or usability defect with an available workaround. |
| — | 0 | No issue established. |

The absence of a P0 finding is not certification that none exists. The P1
security findings require a deployment/configuration action or control of a
relevant repository/token; no anonymous takeover of a default installation was
demonstrated.

## Findings at a glance

| ID | Priority | /10 | Issue |
| --- | --- | --- | --- |
| A01 | P1 | 9.2 | Compose writes follow repository symlinks into host files. |
| A02 | P1 | 9.0 | Compose deployment accepts host-level privileges and bypasses the owner/deployer boundary. |
| A03 | P1 | 8.8 | Private rewrite protection accepts DNS names resolving to private addresses. |
| A04 | P1 | 8.7 | Compose bind mounts can store persistent data inside the checkout that is deleted after deployment. |
| A05 | P1 | 8.6 | Instance restore overwrites data after container stop failure and includes unrelated containers. |
| A06 | P1 | 8.0 | Instance restore merges old and current database files instead of restoring an exact volume tree. |
| A07 | P2 | 7.2 | Re-running backup setup deletes saved S3 credentials and resets retention. |
| A08 | P2 | 6.8 | Instance restore omits paired-device tokens. |
| A09 | P2 | 6.8 | Concurrent mesh-state saves collide on a shared temporary file. |
| A10 | P2 | 6.2 | Compose changes literal secret/environment values through interpolation. |
| A11 | P2 | 6.4 | HTTP readiness accepts 4xx responses, contrary to the public model contract. |
| A12 | P2 | 6.0 | Docker volume deletion reports success when Docker refuses deletion. |
| A13 | P2 | 5.5 | Automatic Compose service selection can route to the database instead of the web app. |
| A14 | P2 | 5.0 | Importing a repository does not automatically detect Compose, despite the guide. |
| A15 | P2 | 4.5 | The local, untracked launch-film project fails its required check. |
| A16 | P3 | 2.5 | README says screen sharing needs nothing extra; the default Mac install omits FFmpeg. |
| A17 | P2 | 5.5 | Integration CI path filters miss major engine changes. |

### A01 — Repository symlinks redirect Compose's host writes

**Location:** `crates/vercelify-platform/src/compose.rs:184–204`.

**Trigger/evidence:** A checkout contains `.vercelify/app.env` as a symlink to a
synthetic file outside that checkout. Calling the actual `compose::up` with a
stub Docker CLI succeeds and replaces the outside file with `AUDIT='synthetic-only'`.
The writer uses `create(true).truncate(true)` without rejecting symlinks. The
generated override file has the same unsafe write pattern. `.vercelify` itself
can also be a symlink. No real host configuration was targeted.

**Why fix:** Repository content controls files written by the root agent, allowing
host-file corruption or more serious compromise wherever the agent's sandbox
allows writes. A container escape is unnecessary. The sandbox makes some system
paths read-only, but leaves agent state and application data writable.

**How:** Keep generated files in a newly created private directory outside the
checkout. Reject symlink components and validate canonical containment for all
repository-derived contexts and file paths. Use exclusive, unique file creation.
Add regressions for symlinks on both the directory and generated filenames.

**Evidence level:** Direct reproduction with real platform code, synthetic files,
and stub Docker. The same harness also verifies that `detect::plan` accepts an
out-of-checkout context symlink; no real image build of that context was run.

### A02 — Compose bypasses the deployment security boundary

**Locations:** `agent/src/auth.rs:27–39`,
`crates/vercelify-platform/src/apps.rs:165–177`,
`crates/vercelify-platform/src/compose.rs:108–135,178–233`.

**Trigger/evidence:** App creation accepts a Compose definition with
`privileged: true`. Rendering it with the actual generated override and real
Compose preserves that privilege and explicit published ports. The API permits
deployer tokens to create these apps. No dangerous container was started.

**Why fix:** A token intended to deploy apps can ask Docker for capabilities that
are effectively host administration. The override adds labels/networks but does
not enforce the security model's promise that apps do not mount the Docker socket.
Host binds, host namespaces and explicit port publication are likewise unrestricted.
An innocuous image with dangerous mounts is sufficient; this is separate from the
documented exclusion of a malicious image deliberately chosen by the owner.

**How:** Validate the fully resolved Compose model before any `up`. Enforce an
allowlist covering mounts, namespaces, privilege, capabilities, devices, sockets,
ports and included/extended definitions. Put unrestricted host-administration
Compose behind an explicit owner-only capability, rather than the general deployer
permission. Test rejected definitions through the HTTP authorization path.

**Evidence level:** Real app-record acceptance and effective Compose configuration;
host compromise itself was not executed.

### A03 — DNS aliases bypass private rewrite checks

**Locations:** `agent/src/project.rs:917–967`, `agent/src/projects.rs:76–77`.

**Trigger/evidence:** Compiling the exact `private_origin` function from current
source returns `false` for `http://127.0.0.1.sslip.io:11434`, while an actual DNS
lookup returns `127.0.0.1`. The check inspects IP literals and a few hostname suffixes;
it does not check resolved addresses.

**Why fix:** A rewrite can publish an otherwise private service through an app's
public route. Loopback is the proxy container's loopback, so a host service requires
a hostname resolving to an address the proxy can reach, such as the host's LAN or
bridge address. The same missing check applies to those names and to DNS rebinding.
This can expose unauthenticated internal HTTP services; it does not itself bypass
the agent API's bearer authentication.

**How:** Enforce destination restrictions at connection time, including IPv4/IPv6,
mapped addresses, redirects and DNS rebinding. A one-time DNS check followed by an
independent Traefik lookup is insufficient. Use pinned validated upstream addresses
or an egress policy that prevents the proxy from reaching prohibited destinations.

**Evidence level:** Exact validator and real DNS resolution. No public tunnel or
private production service was exposed during this audit.

### A04 — Compose checkout cleanup can erase persistent bind-mounted data

**Locations:** `crates/vercelify-platform/src/engine.rs:685–733,1087–1094`,
`crates/vercelify-platform/src/compose.rs:213–231`.

**Trigger/evidence:** A service uses a standard relative mount, `./persist:/data`.
Real Compose configuration resolves it under the deployment checkout. The engine
then drops `CheckoutGuard`, which recursively deletes that checkout after the
Compose deployment block. The override neither rejects nor relocates bind mounts.

**Why fix:** Files written by a database or upload service can be removed as soon
as deployment finishes. Other mounts to repository configuration files also lose
their durable source, breaking later restarts. A new deployment gets another
checkout rather than the previous data directory. Named Compose volumes avoid
this particular failure.

**How:** Separate disposable source/build paths from managed persistent storage.
Reject or explicitly map persistent relative binds to a stable app-owned path;
stage read-only configuration mounts for the container lifetime. Check the fully
resolved mounts before checkout cleanup. Add a real-container deployment/restart/
redeployment test that preserves a marker in a relative data mount.

**Evidence level:** Real Compose path resolution plus direct engine cleanup source;
no running database was subjected to deletion.

### A05 — Restore proceeds without stopping all data writers

**Location:** `host/lib/backup.sh:272–280`.

**Trigger/evidence:** In an isolated Linux container, Docker is a shell stub whose
`stop` fails. The real `backup_restore --instance` prints a warning, overwrites the
engine database, copies volume data and returns success. Its stop list is every
running container except two fixed infrastructure names, including a synthetic
unrelated production database.

**Why fix:** Copying database files while a process still writes them can corrupt
data. Restoring Vercelify also interrupts unrelated workloads on the same Docker
host. The backup includes the global named-volume directory, so selecting managed
restore targets matters as well as selecting which containers to stop.

**How:** Build a validated restore manifest for owned containers and volumes. Stop
all identified writers and verify they are stopped before changing any live file.
Abort on failure. Preserve restart state and provide a failure-recovery path. Require
a separate explicit scope for restoring unrelated Docker data.

**Evidence level:** Real restore function and filesystem operations, fake service/
Docker commands; no actual containers were stopped.

### A06 — Volume restore leaves files that were absent from the snapshot

**Location:** `host/lib/backup.sh:291–292`.

**Trigger/evidence:** A synthetic destination volume contains a file created after
the snapshot. Running the actual restore leaves that file in place because both
volume copies use `rsync -aHAX --numeric-ids` without replacement semantics.

**Why fix:** The result is a mixture of two database states, potentially including
stale WAL, table, Redis AOF or application files. Restore can say it succeeded
without restoring the requested point in time. This applies when restoring over
existing data, rather than an entirely empty new volume.

**How:** After verifying ownership and stopping writers, restore each volume into
an empty staging directory and switch it into place. Alternatively use deletion
semantics only inside validated, snapshot-owned volume roots. Do not add `--delete`
to the entire global Docker volume directory: that could erase unrelated volumes.
Keep a recovery copy until the restored service is checked.

**Evidence level:** Real rsync and restore code against synthetic directories.

### A07 — Backup setup discards existing cloud credentials and retention

**Location:** `host/lib/backup.sh:60–84`.

**Trigger/evidence:** Start with an existing `backup.env` containing S3 credentials
and `BACKUP_KEEP_DAILY=30`. Run the real `backup_setup` without new credential flags.
The rewritten file has no AWS credential assignments and resets daily retention
to seven. The harness checks only synthetic values.

**Why fix:** Re-running setup or changing backup repositories can break off-site
backups. Reset retention can also prune more history than the user intended. The
installer currently skips setup when the file already exists, so this is **not**
a claim that every normal update deletes cloud credentials.

**How:** Load and preserve existing credentials, supported provider settings and
retention values. Override only options explicitly supplied. Validate repositories
using the proposed configuration before atomically replacing the working file.
Test a second setup with no flags and a change of destination.

**Evidence level:** Real setup function with fake external tools.

### A08 — Whole-instance recovery does not restore paired-device tokens

**Locations:** `host/lib/backup.sh:279–298`, `agent/src/config.rs:225`,
`agent/src/tokens.rs`.

**Trigger/evidence:** A restored snapshot contains the old `state/tokens.json`, but
the new machine has a different registry. Actual instance restore copies the
master key, platform database, mesh directory and volume directories, leaving the
new token registry untouched. A synthetic marker test confirms the omission.

**Why fix:** Devices can regain mesh membership yet fail bearer authentication
with their saved agent tokens after disaster recovery. Recovery also omits other
saved configuration such as the old agent token and Cloudflare configuration;
those need an explicit restore-versus-preserve policy rather than a blanket copy.

**How:** Define and validate a restore manifest for device authorization and
portable configuration. Restore the token registry privately before starting the
agent; preserve machine-specific settings deliberately. Test that a token issued
before the backup authenticates after recovery on fresh state, and that previously
revoked tokens remain revoked according to the chosen recovery policy.

**Evidence level:** Real restore filesystem behavior and token-verification source;
an installed Mac was not re-paired during this audit.

### A09 — Mesh private-file writes are not safe with concurrent writers

**Locations:** `crates/vercelify-mesh/src/state.rs:41–60`,
`crates/vercelify-mesh/src/daemon.rs:633–639`.

**Trigger/evidence:** Eight threads make 100 calls each to the real
`state::write_private`. Hundreds fail because every writer uses the same
`path.with_extension("tmp")`; one writer renames another writer's temporary file.
The final JSON happened to be valid in the recorded run, which does not prove
intermediate writes were correct. Pairing and the running daemon are independent
writers. `save_own` checks identity before the write, leaving a check/write race.

**Why fix:** A fresh pairing or state/map update can fail or be overwritten,
disconnecting a device. The existing sequential re-pair regression does not cover
the filesystem race.

**How:** Use a unique temporary file per write. Also lock across identity checking
and replacement in all relevant processes; unique temporary filenames alone do
not prevent an old daemon from replacing a newly paired identity. Test overlapping
join and map/certificate updates with controlled barriers.

**Evidence level:** Direct concurrent execution of the real writer; live pairing
was not disturbed.

### A10 — Literal Compose environment values are expanded a second time

**Location:** `crates/vercelify-platform/src/compose.rs:37–49`.

**Trigger/evidence:** `env_line("AUDIT_VALUE", "it's ${AUDIT_EXPANSION}")` switches
to double quotes because of the apostrophe. Real Compose resolves the resulting
env file to `it's changed-by-host-environment` when that expansion exists. Dollar
signs are not escaped in this branch. Quoted values containing a newline have the
same exposure.

**Why fix:** Credentials and application settings silently change between what
the user saved and what reaches the container. Some services fail to authenticate;
other settings acquire unintended values.

**How:** Use a Compose-supported raw env-file format or a fully literal encoding,
and verify it with actual Compose rather than string-only tests. Cover apostrophes,
newlines, dollar signs, quotes and backslashes together.

**Evidence level:** Actual serializer and real Compose configuration.

### A11 — A broken HTTP readiness endpoint is accepted as healthy

**Locations:** `crates/vercelify-platform/src/health.rs:116–118`,
`crates/vercelify-core/src/platform.rs:169`.

**Trigger/evidence:** A real loopback HTTP server always answers `404` at
`/healthz`. With only Docker inspect stubbed, the actual `wait_healthy` passes
after two requests. The shared `HealthCheck` model promises 2xx/3xx; implementation
accepts every status below 500, including 401, 403, 404 and 429.

**Why fix:** A misconfigured or unavailable application can replace the healthy
production deployment. The configured readiness signal is ignored precisely when
it is meant to protect a rollout.

**How:** Match the documented 200–399 range, or expose an explicit accepted-status
policy. Add real HTTP tests proving 4xx responses prevent promotion and leave the
previous deployment serving.

**Evidence level:** Real HTTP exchange and actual readiness function.

### A12 — Failed Docker volume deletion is returned as success

**Locations:** `crates/vercelify-platform/src/docker.rs:435–438`,
`agent/src/platform_api.rs:1964–2000`.

**Trigger/evidence:** A synthetic Docker command exits 1 with `volume is in use`.
The actual `Docker::volume_remove` returns `Ok(())`, because it checks process
execution but not exit status. The retained-volume endpoint can then delete its
tracking record and report completion.

**Why fix:** Cleanup loses the inventory entry while the data still consumes disk.
Users can believe sensitive data was removed when it remains in Docker.

**How:** Check the exit status. Treat only a genuine missing-volume response as
idempotent success. Preserve the retained-volume record and return an actionable
error when deletion is refused. Verify absence before deleting metadata.

**Evidence level:** Actual wrapper with failing stub; caller traced in source.

### A13 — Compose selects a database as the routed service

**Location:** `crates/vercelify-platform/src/compose.rs:138–173`.

**Trigger/evidence:** Give `pick` two services: `db` exposes 5432; `web` exposes
3000. With no explicit runtime service, the real function selects `db`, because
alphabetic sorting plus “has any port” takes precedence over the web-name fallback.

**Why fix:** An otherwise valid multi-service app fails its HTTP deployment or
routes visitors to the wrong service. The default should not infer the public
frontend from a database's exposed port. Explicit `runtime.service` avoids this.

**How:** Prefer published web candidates or well-known web service names; where
multiple services are plausible, require a selection. Infer and validate the
selected service's target port instead of silently relying on a default of 80.

**Evidence level:** Direct invocation of the actual selector.

### A14 — Repository import does not auto-detect Compose

**Locations:** `crates/vercelify-platform/src/detect.rs:107–185,210–255`,
`cockpit/src/views/PlatformAppsView.tsx:417–429`, `docs/PLATFORM.md:119–122`.

**Trigger/evidence:** A repository contains only `docker-compose.yml`. Calling the
real planner as `AppKind::Git` fails detection. Compose planning is only selected
when the caller already chose `AppKind::Compose`; the normal repository form
creates Git apps. The guide says a compose file is used when present.

**Why fix:** Following the advertised repository-import path fails for a supported
app format. Pasting inline Compose or using the API's explicit Compose kind is a
workaround, not automatic repository detection.

**How:** Detect recognized Compose filenames in auto mode with documented precedence,
and carry the resulting kind/service/port through placement, preview and cleanup
logic. Alternatively expose repository-backed Compose explicitly and correct the
guide. Test the real UI import path with a Compose-only repository.

**Evidence level:** Direct planner reproduction and UI request source.

### A15 — Local launch-film sources cannot pass their check

**Locations:** `launch-film/src/Film.tsx:24`, `launch-film/src/Root.tsx:46`,
`launch-film/src/acts/Act1.tsx`, `launch-film/src/acts/Act2.tsx`,
`launch-film/src/sync.ts`, `launch-film/src/timeline.ts:45`.

**Trigger/evidence:** `cd launch-film && npm run check` fails with 15 TS2339
errors. The consumers refer to `ACT.statement` and cues including `drop`, `land`,
`depart`, `handoff`, `fly`, `pieceA` and `pieceB` that the current timeline does not
export. `ACT` now defines rent/own/push/live/proof/lid/anywhere/mark.

**Why fix:** The registered film composition reads missing timeline data and the
check never reaches its film/grid validation. This prevents a dependable render.
**This directory is untracked local work; it is not a released-app regression.**

**How:** Reconcile the composition, act implementations and audio cues with one
timeline schema; remove abandoned consumers or migrate them. Run `npm run check`,
then render a short preview and check picture/audio timing.

**Evidence level:** Fresh failing required check. No movie render was attempted.

### A16 — Default screen-sharing prerequisites are misstated

**Locations:** `README.md:271`, `install-mac.sh:27,394–406,430`.

**Trigger/evidence:** README says the second screen needs nothing extra. Mac setup
defaults `WITH_DISPLAY=0`, and installs FFmpeg/Moonlight only with `--with-display`.
Capture uses FFmpeg. The app already has tool detection and installation guidance.

**Why fix:** A fresh Mac following the README cannot simply press Start. This is
a setup/documentation mismatch, not evidence that configured screen sharing fails.

**How:** State the FFmpeg prerequisite beside the feature and show
`--with-display`, or provide an explicit guided installation path. Keep Moonlight's
remote-desktop dependency separate from FFmpeg's screen-sending dependency.

**Evidence level:** Installer defaults and capture/tool source. No clean Mac was
modified to reproduce installation.

### A17 — Critical engine changes can skip integration CI

**Location:** `.github/workflows/integration.yml:18–46`.

**Trigger/evidence:** The PR path allowlist names selected platform modules, but
does not cover changes confined to `compose.rs`, `build.rs`, `deploy.rs`,
`dbengine.rs`, `detect.rs`, `storage.rs`, `volumes.rs`, `proxy.rs`, platform
migrations or the crate manifest. Ordinary CI's Docker lifecycle/database/engine
tests are opt-in/ignored, so normal unit-test success does not fill this gap.

**Why fix:** Changes to the most consequential deployment and data paths can
merge without exercising the real routing/multi-node integration contracts. Weekly
scheduled runs happen after the change may already be released. The current
contracts also need new cases for the defects above; simply triggering them is
not proof that they cover Compose and recovery adequately.

**How:** Match `crates/vercelify-platform/**` and relevant agent/host dependencies
rather than maintaining a partial module list. Add disposable Compose and database
restore contract tests. Keep website build validation in CI as well; the existing
CI workflow has no website job.

**Evidence level:** Current workflow trigger configuration and test annotations;
no failing GitHub run is inferred.

## Verification and coverage

Fresh results from this checkout:

| Check | Result |
| --- | --- |
| Rust core/platform/mesh, daemon feature | 389 passed; 6 ignored. Includes 13 generated-binding tests. |
| Rust agent + native cockpit backend | 191 passed; 2 ignored. Includes 20 agent API integration tests. |
| Cockpit Vitest | 130 passed across 31 files. |
| Cockpit production build | Passed TypeScript and Vite build. |
| Website production build | Passed TypeScript and Vite build. |
| Host scripts through `./dev check host` | 218 bats tests passed on Linux; ShellCheck passed. |
| Additional ShellCheck including release scripts | Passed. |
| Rust formatting | Passed. |
| Workspace Clippy, all targets, warnings denied | Passed. |
| Browser demo Playwright | 27 passed, dark/light themes and workflow regressions included. |
| Local launch-video typecheck | Passed. |
| Local launch-film required check | Failed as A15. |
| Cockpit production npm advisory audit | Zero reported advisories. |
| Website production npm advisory audit | Two affected package entries, discussed below; not treated as a deployed-site exploit. |

**Total: 955 passing existing tests** (580 Rust + 218 host + 130 frontend + 27
browser); **8 ignored Rust tests remain unexecuted**. The isolated audit probes
are additional to that total. Generated TypeScript files remained unchanged.
The `ts-rs` warning about `deserialize_with = "runtime_port"` was present;
typechecks and tests nevertheless passed.

| Feature family | What this audit covered | Boundary |
| --- | --- | --- |
| Installation, update, rollback, uninstall | Shell source, systemd sandbox, release/edge workflows, 218 host regressions | No install/update/uninstall on the user's server; no release artifact signing/notarization run. |
| Authentication, pairing, roles, tokens | Auth/routes/token source; agent API regressions; pairing/mesh tests | No destructive revocation of an existing device. |
| Deployment/build packs, Compose, previews, rollback, replicas | Platform source and tests; effective Compose configuration; targeted filesystem/selection/readiness probes | Ignored Docker end-to-end tests and Linux network-namespace contracts not rerun. |
| GitHub App, private Git, webhooks, auto-deploy | Source plus GitHub callback/webhook API regressions and hardened-Git tests | No live GitHub app authorization or real private repository credentials used. |
| Routing, domains, HTTPS, rewrites, cron | Proxy/project rules, cron source, unit tests, targeted private-origin check | No public DNS/certificate/tunnel change; no full real-Traefik contract run. |
| Databases, storage, volumes, backup/recovery | Platform/host source, API regressions, isolated restore/setup probes with real file copying | No real database restore, disk format, disk removal or disk failure. |
| Mesh, relay, NAT, browser gateway, workers, standby | Rust sources/tests, gateway demo coverage, concurrent-write probe | No physical multi-network test or live standby promotion. |
| AI/Ollama and failure explanation | Install/cancellation/chat source, API tests, frontend/CLI regressions | No large model download or model-quality benchmark. |
| Power, thermal, sleep, maintenance, health and alerts | Source, collector/host/agent tests, readiness probe | No power cut, shutdown, thermal stress or physical lid test. |
| Desktop, screen sharing, Moonlight, virtual display | Native/frontend source and tests, prerequisites review | Hardware capture/extended-display ignored tests not run; no permission changes. |
| Website and launch assets | Frontend build, source/dependency review; both local motion projects' checks | No movie render or full website accessibility/performance certification. |
| Documentation, prior audits and field evidence | Compared older findings against current code and feature changes | Historical field results are context, not fresh proof. |

This is a repository-wide risk review with targeted source tracing and extensive
regression checks. It does **not** claim every line was manually inspected or
every implemented feature was physically exercised end to end.

## Dependency advisory triage

Website `npm audit --omit=dev` flags `vgpu@0.5.0` through
`@modelcontextprotocol/server@2.0.0`. The upstream advisories concern HTTP MCP
servers, including [cross-client delivery](https://github.com/advisories/GHSA-jm63-cq47-j5jp)
and [validation memory exhaustion](https://github.com/advisories/GHSA-46jr-4wx8-v453).
The website imports a browser graphics library and builds static assets; no
reachable MCP HTTP server was established in this application. These package
entries therefore are **not two additional P1 website vulnerabilities**.
Review/remove the unused tooling dependency chain or update it compatibly; do
not blindly apply npm's suggested major downgrade. The advisory results alone
do not certify either npm or Rust dependency safety.

## Previous reports rechecked

The older 30 October findings were not copied into this list as current defects.
Current source and fresh tests cover fixes for update-log permissions, short-code
consumption/token expiry, Mac browser gateway/opener scope, GitHub callback refresh,
initial app variables, restore confirmation/pre-restore backup, missing-HDD creation,
retained-volume visibility, modal focus, settings-corruption preservation, battery
wear warnings, relay reconnects, packet bursts and display preflight. This audit
does not re-certify all older physical field-test claims.

## Suggested fix order and acceptance checks

1. **A01–A03:** close host filesystem/Compose privilege/private-service boundaries.
   Prove malicious source cannot write outside its workspace or gain host capabilities,
   and rewrites cannot reach denied IPs through DNS aliases/rebinding.
2. **A04–A08:** make data lifetime and disaster recovery trustworthy. Preserve a
   marker across Compose deployment/restart; make failed-stop restore change no
   live files; prove exact snapshot restoration; preserve cloud credentials and
   paired-device authentication on fresh-machine recovery.
3. **A09–A14:** concurrency, literal values, readiness, cleanup errors and Compose
   import/routing correctness. Add regressions using real tool semantics.
4. **A17:** broaden integration triggers and add the missing contract cases while
   making the fixes, so they cannot regress unnoticed.
5. **A15–A16:** reconcile the local film timeline and correct the screen-sharing
   prerequisite text.

Reproduction source and fresh logs are in `docs/audits/2026-11-01/`. They use
synthetic values and disposable state. Security probes were kept local and have
not been filed publicly.
