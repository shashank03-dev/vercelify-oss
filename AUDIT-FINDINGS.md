# Audit findings: rules implemented only in part

Date: 1 November 2026. Starting HEAD: `17547d68f0a778856042a5d145db6d096e4354f1`.

**9 confirmed findings: 3 high, 6 medium.** No fixes, source edits, commits, or pushes were made by this audit. Speculative candidates are omitted.

## Verification and tree state

Each finding below follows a reachable code path and has a temporary executable reproduction. A separate Cargo harness outside the repository imported the actual agent project/routing modules and linked the platform/core crates. It ran **34 tests successfully: 9 bug-presence reproductions and 25 existing tests from the imported modules**. These reproductions assert the observed faulty behavior; their passing does not mean the bugs are fixed. The role-policy reproduction compiled an exact extraction of `allowed`, whose body was checked against the current source.

Additional verification: `cargo test --offline -p vercelify-mesh --lib` passed **29/29**. `bats host/tests/firewall.bats host/tests/platform.bats host/tests/vfy-platform.bats` passed **21/22**; the remaining test invokes GNU `stat -c` on macOS and fails on that unsupported option. It is not counted as a product finding. Compose verification used the installed Compose 5.6.1 binary's `config` command, without starting containers. Deployment race tests used a private Docker stub and an in-memory store. No live application, production data, real credential, or deployed image was used. The temporary harness was deleted after verification.

The checkout was already dirty: `docs/PRODUCT-AUDIT.md` was modified and several untracked paths existed. Additional tracked edits appeared concurrently during the audit, including engine/auth/API/node files. They were preserved. Consequently, a globally clean `git status` cannot be asserted; the audit's only non-ignored repository addition is this untracked report. Finding-relevant source hashes were unchanged across the final reproduction run. Locations refer to the working tree inspected, including concurrent edits, rather than pristine HEAD.

## 1. A symlinked build context escapes the checkout and can publish host secrets — high

**Where:** `crates/vercelify-platform/src/detect.rs:107`, `crates/vercelify-platform/src/validate.rs:153`, `crates/vercelify-platform/src/build.rs:349`, `crates/vercelify-platform/src/build.rs:234`, `crates/vercelify-platform/src/detect.rs:669`.

**Claim versus implementation:** `validate.rs` describes `repo_path` as “A path inside the repository: relative, no `..`, no leading `-`.” `BuildPlan.context` is described as “Build context relative to the repository root”. The validation only checks the spelling. `root.join(context).is_dir()` follows symlinks, and the resulting path is passed directly to BuildKit. There is no canonical-path containment check. Static builds generate `COPY . /srv` and serve `/srv` through Caddy. The final-file symlink check in `read_project_files` also does not catch a symlink in a parent directory.

**User scenario:**
1. A deployed static Git app uses build context `site`, static pack, and the default output directory `.`.
2. A same-repository PR replaces the `site` directory with a Git symlink to `/var/lib/vercelify` on the server. This does not require a fork PR to be built.
3. The preview's context passes lexical validation and resolves to the host state directory. The root agent can read it.
4. The generated static Dockerfile copies that directory into the preview image, where files such as `master.key` and state backups can be served as ordinary static files.

**Verification:** `symlinked_context_escapes_checkout` created a checkout and a separate synthetic host directory, linked `site` to the latter, and called the actual planner and `Builder::build_dir`. It confirmed outside project files were read, the generated Dockerfile contained `COPY . /srv`, and the Docker invocation received the outside directory as its context. The test intercepted the BuildKit command; it did not build or serve a real image. The production Git builder uses the same planner/context construction and `buildx` helper.

**Fix:** Resolve the checkout and every host-read build path canonically and reject paths outside the checkout before reading or invoking Docker. Apply the rule consistently to context directories, Dockerfiles, Compose files, and project-rule reads, including parent symlinks.

**Test that would catch it:** A PR fixture replaces an otherwise valid configured context with a symlink to a directory containing a synthetic secret. Planning/building must reject it before any outside file is read or Docker is invoked.

## 2. DNS names bypass the private-origin rewrite restriction — high

**Where:** `agent/src/project.rs:917`, `agent/src/project.rs:954`, `agent/src/projects.rs:76`, `agent/src/routing.rs:381`.

**Claim versus implementation:** `docs/PROJECT-CONFIG.md:40` says addresses on this machine or a private network “are refused” because “the proxy can reach them, and the rewrite would put them on your public domain.” `private_origin` checks IP literals and selected hostname suffixes. A fully qualified public DNS name that resolves to a private IP passes. The renderer then installs that name as a Traefik external service without an address-level restriction.

**User scenario:**
1. A main server has Ollama on its mesh address `10.77.0.1:11434`, and an app or preview has a public domain.
2. A repository rule rewrites `/ai/:path*` to `http://10.77.0.1.sslip.io:11434/:path*`.
3. With `rewrite_private_targets` still false, the guard accepts the DNS name and publishes the external rewrite.
4. Public requests reach the server's private Ollama API through Traefik. The same pattern can expose other reachable private HTTP services; services with their own authentication still retain that authentication.

**Verification:** `dns_rewrite_survives_guard` ran the real parser, private-rewrite filter, and renderer and confirmed this private-address DNS origin remained in the emitted service. An actual system DNS lookup resolved `10.77.0.1.sslip.io` to `10.77.0.1`; `127.0.0.1.sslip.io` similarly resolved to loopback. No private service was called.

**Fix:** Enforce private-address restrictions on resolved destinations at connection time, including IPv4/IPv6 and DNS changes. A one-time DNS validation alone leaves a rebinding gap; use enforced egress controls or a resolving proxy that validates every connection.

**Test that would catch it:** Point an ordinary-looking DNS name at a synthetic private HTTP server and assert that a rewrite is refused with the default setting. Include a DNS answer changing from public to private after configuration is applied.

## 3. Compose previews can mount the production volume despite preview isolation — high

**Where:** `crates/vercelify-platform/src/engine.rs:710`, `crates/vercelify-platform/src/compose.rs:58`, `crates/vercelify-platform/src/compose.rs:179`, `crates/vercelify-platform/src/deploy.rs:102`.

**Claim versus implementation:** `deploy.rs` says “Previews never touch production data: they get volumes of their own.” That protection exists in the ordinary container deployer, which adds a PR-specific physical volume name. The Compose branch bypasses that mount construction. It changes the Compose project name and adds labels/env/networks, but preserves explicit top-level volume names, external volumes, and host bind mounts from the source file.

**User scenario:**
1. A production Compose app has `web` mounting `data:/data`, with top-level `volumes.data.name: production-data`.
2. Enable previews for that Compose Git app and open a same-repository PR.
3. The preview runs under project `vfy-<app>-pr<n>`, but Compose's explicit `production-data` name wins over project namespacing.
4. The preview writes, migrates, or deletes files in the exact volume mounted by production.

**Verification:** `compose_preview_preserves_production_volume_name` fed the actual generated production and preview override files into real Compose `config --format json`. Both resolved the volume name to the same synthetic `audit-production-data`. No containers or volumes were created. The engine passes these original/override files directly to Compose `up`.

**Fix:** Before accepting a Compose preview, replace its storage bindings with isolated PR-specific storage or reject configurations whose explicit names, external volumes, or bind mounts cannot be safely isolated. Changing only the Compose project name is insufficient.

**Test that would catch it:** Resolve production and preview Compose configurations containing an explicit named volume, an external volume, and a bind mount. Assert that no writable storage source is shared, or that preview deployment is rejected before startup.

## 4. Closing a PR cleans up the old preview but lets an in-flight deployment publish afterward — medium

**Where:** `crates/vercelify-platform/src/engine.rs:1198`, `crates/vercelify-platform/src/engine.rs:836`, `agent/src/platform_git.rs:292`.

**Claim versus implementation:** `docs/PLATFORM.md:130` says previews are “deleted when the pull request is merged or closed.” `remove_preview` promises “container(s), route, its own volumes, and the preview record (closed).” It cancels only deployments whose status is `Queued`. A `Building` or `Deploying` deployment keeps its cancellation handle untouched, and the publish path never checks whether the preview has closed.

**User scenario:**
1. Open a PR and let its preview deployment reach container creation or health checking.
2. Close the PR before that deployment finishes.
3. The poller calls `remove_preview`; it removes any previously serving deployment, removes the route/storage it knows about, and marks the preview closed.
4. The running deployment finishes and writes a new route, marks itself healthy, and updates the closed preview's serving pointer. Storage created after teardown can also escape that teardown.
5. A later reconciler can remove the unwanted route/container, but PR-close cleanup itself has already returned success and the preview can become reachable again in the interval.

**Verification:** `close_while_deploying_still_publishes_route` ran the real engine dispatcher with a Docker stub blocked at creation. After `remove_preview` returned, it released creation and observed a healthy deployment, a serving pointer on a still-closed preview, and the recreated route file.

**Fix:** Serialize preview closure with deployment publication, cancel and await all in-flight deployments for that PR, and check the preview's active generation at the commit point. Teardown should run after those operations can no longer create resources.

**Test that would catch it:** Pause a deployment before creation and again just before route publication, close its PR, then resume. It must never publish a route or leave containers/storage after close completes.

## 5. Cancellation does not interrupt a build waiting on the power guard — medium

**Where:** `crates/vercelify-platform/src/engine.rs:299`, `crates/vercelify-platform/src/engine.rs:598`, `crates/vercelify-platform/src/gate.rs:58`.

**Claim versus implementation:** `Engine::cancel` says “Cancel a queued or running deployment.” For a running deployment it signals cancellation, but `steps` awaits `BuildGate::acquire` before inspecting that signal. The gate loops while the power guard holds builds and also waits for a semaphore permit without observing cancellation.

**User scenario:**
1. A deployment enters `Building` while the server is thermally paused, shutting down, or waiting for another build slot.
2. The user cancels it.
3. It remains `Building`, and its app remains busy, for as long as the hold or semaphore wait lasts. A newer deployment of the same app cannot start.
4. Only after the gate opens does the engine notice cancellation and release the app.

**Verification:** `cancel_does_not_interrupt_power_guard_wait` used the real dispatcher and a `thermal_paused` state file. After cancellation and multiple gate polls, the deployment remained `Building`. Removing the hold caused it to transition to `Cancelled` without invoking Git or Docker.

**Fix:** Make both power-state and semaphore waits cancellable, and release the app's operation ownership promptly when cancellation arrives. Cancellation should not depend on the condition the user is trying to stop waiting for.

**Test that would catch it:** Cancel while thermally paused and while queued for the only build slot; assert timely terminal status and that a replacement deployment is no longer blocked by the cancelled operation.

## 6. Conditional redirects lose to later unconditional redirects — medium

**Where:** `agent/src/routing.rs:268`, `agent/src/routing.rs:350`, `agent/src/routing.rs:505`.

**Claim versus implementation:** `docs/PROJECT-CONFIG.md:7` says “A project moving from Vercel keeps its configuration as it is”; the supported redirects include `has`/`missing`. The renderer gives earlier conditional rules higher router priority (`P_REDIRECT - i`), but injects every unconditional redirect into the selected router's middleware chain *before* that router's own conditional redirect. Thus array order is preserved within the two categories but fails across them.

**User scenario:**
1. Put a redirect for `/offer` to `/special` first, conditional on query parameter `vip` being present.
2. Put an unconditional `/offer` to `/ordinary` redirect second, as the fallback.
3. Request `/offer?vip=1`.
4. The first rule's conditional router is selected, but its chain runs the second rule's redirect first. The visitor goes to `/ordinary`, and the intended special rule never executes.

**Verification:** `conditional_redirect_loses_to_later_unconditional` parsed those exact two rules and rendered the actual config. The conditional router's middleware order was `["vfy-audit-r1", "vfy-audit-r0"]`, proving the fallback redirect precedes the matching first rule. This verified generated ordering, not an additional live Traefik deployment.

**Fix:** Preserve one redirect ordering across conditional and unconditional rules, ensuring a selected conditional rule is not preempted by later unconditional middleware. Earlier matching unconditional rules must still win.

**Test that would catch it:** Test both permutations of conditional and unconditional redirects matching the same path, with the condition true and false. Assert the resulting location and query string through the proxy.

## 7. Go builds silently ignore both app and vercel.json commands — medium

**Where:** `crates/vercelify-platform/src/detect.rs:158`, `crates/vercelify-platform/src/detect.rs:178`, `crates/vercelify-platform/src/detect.rs:856`.

**Claim versus implementation:** `docs/PLATFORM.md:123` says `buildCommand`, `installCommand`, `outputDirectory` and `framework` in `vercel.json` “are honoured”. The planner says “The app's own settings win over vercel.json” and merges the commands accordingly. But the Go branch calls `go(&dir)` without the merged build settings. Its generated Dockerfile runs its built-in commands regardless and emits no ignored-setting note. The precedence merge is implemented only for packs that receive and use `b`.

**User scenario:**
1. Deploy a Go repository requiring a code-generation step before `go build`.
2. Set `buildCommand` in `vercel.json`, or override it in the app's build settings, to run generation and then compile.
3. Select the Go pack or let Go detection choose it.
4. The engine silently runs its default build instead. The deployment fails on missing generated code or produces an unintended binary.

**Verification:** `commands_silently_ignored_in_go` constructed a Go fixture with repository commands and distinct app-level overrides. The real planner returned a generated Dockerfile containing none of the configured commands and an empty notes list. The direct Go dispatch and lack of settings parameter establish the path.

**Fix:** Pass merged settings to the Go generator and honor supported command overrides. For unsupported settings/packs, reject or explicitly report the limitation rather than claiming they were applied.

**Test that would catch it:** For each pack, test repository-only commands and conflicting app overrides together. Assert that the chosen command appears in the generated build steps or produces an explicit unsupported-setting error/note.

## 8. Deployers can edit app variables but cannot edit inherited variables — medium

**Where:** `agent/src/auth.rs:18`, `agent/src/auth.rs:28`, `agent/src/platform_api.rs:45`, `agent/src/platform_api.rs:54`.

**Claim versus implementation:** `docs/design/SYSTEM-DESIGN.md:569` assigns deployers “apps, deployments, env, domains, previews”. `auth.rs` likewise permits deployers to change “variables”. The allowlist grants writes below `/v1/platform/apps/...`, but has no cases for `/v1/platform/projects/{id}/env` or `/v1/platform/environments/{id}/env`. These fall through to denial even though they are variable operations. Project creation/deletion can remain owner-only independently.

**User scenario:**
1. An owner defines a shared variable or preview override at project/environment scope.
2. A CI operator or teammate has a deployer token, the documented role for changing variables and shipping code.
3. The operator updates an app variable successfully, then tries to rotate the shared variable or update the shared preview URL through the corresponding env endpoint.
4. The second request returns 403 solely because that variable is inherited from a different scope.

**Verification:** `deployer_cannot_write_inherited_vars` compiled the actual `allowed` function and compared PUT requests to all three real routed endpoints: app scope was allowed; project and environment scopes were denied. All endpoints pass through `require_token` before their handlers.

**Fix:** Grant deployers the specific project/environment env operations, while retaining owner checks on project/environment lifecycle operations. Keep secret-reveal checks consistent across all three scopes.

**Test that would catch it:** An HTTP role matrix covering GET, reveal, PUT, and DELETE for the same variable at app, environment, and project scopes, separately from infrastructure creation/deletion.

## 9. `vfy env pull` exports only one scope instead of the app's production variables — medium

**Where:** `cli/vfy:9`, `cli/vfy:451`, `agent/src/platform_api.rs:305`, `crates/vercelify-platform/src/env.rs:34`, `crates/vercelify-platform/src/env.rs:165`.

**Claim versus implementation:** The CLI promises to “write the app's production variables to FILE”. `docs/PLATFORM.md:139` explains that variables live on project, environment, or app, with app values winning. But `env pull` calls `/apps/{app}/env?reveal=true`, whose handler lists `VarScope::App` only. Filtering out preview-only entries does not perform the project/environment/app merge that production deployments actually use.

**User scenario:**
1. Set `DATABASE_URL` at project scope, `API_URL` at environment scope, and `GREETING` on the app.
2. Deploy: the app receives all three after precedence resolution.
3. Run `vfy env pull APP` for local development or migration.
4. The command succeeds and exports only `GREETING`; the two inherited production variables are missing, without a warning.

**Verification:** `env_pull_endpoint_drops_inherited_production_values` populated an actual in-memory platform store. The function used by the pull endpoint listed only the app key, while the production resolver returned the inherited `DATABASE_URL` too. The CLI's call and export filter were traced directly. Existing shell tests use only app-scoped mock values and therefore miss this combination.

**Fix:** Provide an authorized effective-production-variable export using the same resolution semantics as deployments, and have `env pull` consume it. Retain a separate scope-specific listing API for editing inherited settings.

**Test that would catch it:** Pull an app with project-only and environment-only values, conflicting values at all three scopes, and preview-only overrides. Assert that the file contains the effective production values with correct precedence and excludes preview overrides.

## Places checked with no additional confirmed finding

- **Environment/secrets/database references:** The reported original preview precedence bug is fixed: normal values are merged across all scopes before preview-only values. Checked the existing cross-level regression test, sealed-cell AAD, masking/reveal paths, database reference expansion, and ordinary container PR-specific mount naming.
- **Git/webhooks/cron:** Traced GitHub fork exclusion including deleted forks, incomplete-PR-list handling, per-app check serialization, signed webhook verification and repository matching, push watch-path ordering, upload-versus-repository rule precedence, and production-only cron selection. No additional confirmed bypass in these paths.
- **Mesh and host firewall:** Checked peer visibility/port construction, certificate and map signatures, epoch/version ordering, enrollment sealing, worker-token origin checks, container egress versus device-port ACL generation, and firewall chain replacement. All 29 mesh library tests and the six selected firewall shell tests passed. This was not a live two-node network test.
- **Core and cockpit Tauri commands:** Checked role definitions, pairing decoding, volume/path normalization, active-versus-explicit server selection, request path construction, private settings/download writes, stream cancellation plumbing, and mesh forwarding selection. No additional confirmed finding from those checks; GUI behavior was not exercised.
- **Host and Mac CLIs:** Checked engine selection, domain setting construction, deploy/log/rollback paths, env export permissions/symlink refusal, mesh SOCKS selection, backup state/master-key handling, and selected remote-management logic. Shell verification limits are recorded above.

Excluded as requested: `cockpit/src/types/generated`, `launch-film/`, `launch-video/`, and `field-test/`. Coverage is a focused audit of the named rule boundaries, not a claim that every possible behavior in these directories has been exhaustively proven correct.

## Resolution (1 November 2026)

All nine findings were reproduced against the code and fixed. Each fix has a regression test
that fails when the fix is reverted (checked for 4, 5 and 6 by disabling each layer).

| # | Fix | Tests |
| --- | --- | --- |
| 1 (Compose) | Review follow-up: `compose::check_build_inputs` refuses a Compose `build.context`, Dockerfile or `additional_contexts` resolving outside the checkout (an inline Dockerfile reads only its context) for every compose app, before `up`. | `compose::builds_read_only_the_checkout`; real Compose in `compose_resolves_preview_volumes` |
| 1 | `detect::plan` resolves the context, Dockerfile, Compose file, output directory and every file it reads with symlinks followed, and refuses (or ignores) anything leading out of the checkout. `detect::context_dir` checks again where `Builder::build`/`build_dir` hand the context to BuildKit. | `detect::links_out_of_the_repository_are_refused`, `build::a_linked_context_never_reaches_docker` (Docker stub never invoked) |
| 2 | External rewrites by name are pinned: `projects.rs` looks the name up every reconcile, `project::pin_rewrites` refuses it if any address is private (or it doesn't resolve), and `routing.rs` connects Traefik to those addresses only (`Host` header + `serversTransports.serverName`), so a later DNS change can't reach a private address. `vfy config check` reports the same. | `project::rewrites_by_name_are_pinned_to_public_addresses`, `routing::pinned_rewrites_connect_to_checked_addresses`; verified on Traefik v3.7 (whoami saw the pinned IP, `Host: api.example.com`, rewritten path; the TLS transport config loaded) |
| 3 | Compose previews: the override renames every top-level volume to `vfy-<app>-pr<n>_<key>` (not external, labelled), the merged `docker compose config` is checked before `up`, and bind mounts resolving outside the checkout, driver options and `container:` volumes refuse the preview. Closing removes the labelled volumes. | `compose::preview_storage`, `compose::compose_resolves_preview_volumes` (real Compose 5.6.1) |
| 3 (files) | Review follow-up: `config` runs with `--no-env-resolution` (Compose never reads env files before the checks), and a preview is refused when an env file, secret/config file or SSH key resolves outside the checkout, or a secret/config is external. A Compose too old for the flag refuses compose previews. | `compose::previews_read_no_server_files`; real Compose: the outside env file isn't inlined and is refused, as is an outside secret file |
| 3 (implicit reads) | Second review: Compose read a `.env` symlinked out of the checkout for `${…}` (and, as found while fixing it, `include` and `extends.file` outside it). `compose::implicit_reads` parses the compose files (yaml-rust2, no encoding feature; +95 KiB, agent 8,250,096 B of the 8 MiB budget on x86_64 musl) and refuses any of those reads leading outside the checkout, or interpolated paths, before Compose is run. | `compose::implicit_reads_stay_in_the_checkout` (symlinked `.env`, nested `.env`, include/env_file/project_directory, extends in block, flow, `<<` merge and escaped-key spellings, nested include, linked include), `compose::a_preview_checks_before_compose_reads_anything` (Docker never run), real Compose in `compose_resolves_preview_volumes` |
| 3 (merges) | Third review: `extends.file` inside services brought in by a `<<` merge at the services level was missed. Merges are now resolved throughout each document before the scan, and every `extends.file` anywhere counts. While there: aliases are counted from parser events and a file expanding past 100,000 nodes (or over 1 MiB) is refused before it is loaded, since the loader copies aliases (an alias bomb would exhaust the agent's memory). | `implicit_reads_stay_in_the_checkout` (the review's example, list merges, nested merges, a merged include entry, include merged at the top, extends in an extension field; a merge's own key winning), `alias_bombs_are_refused_before_loading`; real Compose reads the outside file in the review's example and `implicit_reads` refuses it |
| 4 (cleanup) | Review follow-up: an instance that finished starting as the cancel arrived is kept in `started`, so the cancel path removes it. The engine tests' Docker stub now tracks containers, and the tests assert none of the closed preview is left (not just that `rm` ran). | `engine::a_closed_preview_never_goes_live` (fails without the fix) |
| 4 | `remove_preview` closes the row first under a commit lock, cancels and waits for the PR's running deployments, then tears down; the publish path re-checks the preview is open under the same lock. A failed teardown reopens the row for retry. | `engine::a_closed_preview_never_goes_live`, `engine::the_commit_point_checks_the_preview_is_open` |
| 5 | The build-gate wait races the cancel signal (`tokio::select!`), so a cancel ends the deployment and frees the app at once. | `engine::cancel_does_not_wait_for_the_build_gate` (power guard hold and taken slot) |
| 6 | A conditional redirect's router runs only the unconditional redirects listed before it. | `routing::redirect_order_across_conditions` (both orders, condition true/false) |
| 7 | Every pack honours install/build commands (app over `vercel.json`); Go/Rust compile after a generating build command unless it left the binary. Unusable settings (commands with a Dockerfile, an output directory for a server) are build-log notes. | `detect::commands_reach_every_pack` |
| 8 | Deployers may PUT/DELETE variables at project and environment scope; project/environment lifecycle stays owner-only. | `auth::variables_at_every_scope` |
| 9 | `GET /v1/platform/apps/{app}/env?resolved=true` returns the production merge (`env::effective`, same order as deployments, no preview overrides); `vfy env pull` uses it. | `env::effective_production_variables`, `vfy-platform.bats` "env pull, set and push" |

Checks run on macOS: `cargo test --workspace --all-features` (all pass), `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo fmt --check`, ShellCheck, and bats (the 7 failures left are
GNU-only `stat -c`/`sed -i`/`touch -d` uses in backup, hardware, maintenance and remote tests,
which pass on Linux CI). Not run here: the Linux-only `host/tests/contract/*.sh` suites.
