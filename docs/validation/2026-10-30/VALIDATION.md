# Validation of all 18 review findings

Validated 30 October 2026 against `71971d8`, version 0.2.5. Product source was not
modified. Reproductions used temporary agent state, synthetic credentials, stubbed
system commands and browser-isolated demo fixtures. No live update, database restore,
physical power interruption or installed-app configuration was changed.

## Result

- **12 reproduced or externally verified observations:** F1, F4–F8, F10–F12, F14,
  F17 and the battery-warning portion of F18.
- **4 source/UI-supported limitations:** F2, F3, F9 and F13. Their code behavior is
  established, but clean-Mac networking, external-client and away-from-home failure
  claims have not been physically reproduced.
- **2 reclassified UX recommendations:** F15 and F16. They are not established
  functional failures; the original report overstated their status/severity.

This does **not** mean 18 end-to-end failures were reproduced. Several original
entries bundled an observed behavior with a broader risk or proposed improvement.
The table identifies exactly what the evidence supports.

## Finding-by-finding disposition

| ID | Disposition | Validation and boundaries |
| --- | --- | --- |
| **F1** | **Confirmed; P1 retained** | Started the real agent with umask 022; called its actual update endpoint; a stub systemd launcher executed the real installer `summary()`. Both a new and a pre-existing log were 0644. Parsed the synthetic pairing from each and successfully authenticated to `/v1/system`. This proves credential validity and file mode, not a complete live systemd upgrade. The existing physical field note independently records the readable production log. |
| **F2** | **Supported, conditional; P1 retained** | Installer defaults to userspace SOCKS; native agent HTTP uses that proxy; app-browser actions pass their private URL directly to the browser. A UI probe captured that unchanged URL. No app-browser forwarding/system proxy setup exists in this path. **Narrowed:** applies when no other host route/browser proxy reaches the mesh. A clean physical Mac/browser/private-server trip was not run; universal browser failure is not claimed. |
| **F3** | **Supported, conditional; P2 retained** | The rendered external-AI example uses a private `10.77.*` endpoint and supplies no proxy/local forward; native in-app requests use a separate proxy-aware client. **Narrowed:** the missing connection setup is established, but actual OpenAI Python/Ollama client failure on a clean Mac was not executed. Existing system routes or client proxy settings can make the snippet work. |
| **F4** | **Boundary failure reproduced; P1 retained** | In the actual UI, simulated successful callback storage plus a real-shaped `git-sources` event left the modal open and credentials absent after 11 seconds. This exceeds normal app polling. Separately reran the real-agent `github_app_and_webhooks` integration test with a stub GitHub API: successful callback stores the source, and replay of its state is rejected. Live GitHub approval/install was not performed. |
| **F5** | **Confirmed UI/documentation gap; P2 retained** | Inspected the rendered New app labels and default branch: repository, branch=`main`, build, name and port; no initial variables/database step. Source confirms create immediately requests deployment. README instructs variables before Deploy. **Boundary:** no arbitrary real repository was built to establish how many apps this affects; apps without build-time prerequisites can work normally. |
| **F6** | **Confirmed wording/workflow mismatch; P2 retained** | With a Cloudflare-mode fixture, per-app Domains still says to create an A record, while Settings correctly shows Cloudflare mode and points to a host CLI command. No mode-aware publish/setup/check flow exists in those controls. **Boundary:** no live DNS/Cloudflare/certificate failure was tested. The domain engine itself is not shown to be broken. |
| **F7** | **Confirmed default/visibility limitation; P2 retained** | Executed real `default_backup_repo()` with no storage mount: `/var/backups/vercelify-restic`. Rendered the normal success state with that target: a successful backup is shown with no separate off-device/disk-failure indication. **Boundary:** local backups remain useful for logical recovery; no backup corruption, restic failure or physical disk-loss restore was established. Desktop key-export/destination setup is absent; CLI instructions already exist. |
| **F8** | **Confirmed action behavior; P1 retained** | Clicked database Restore once. Instrumentation recorded the restore request immediately, with no additional dialog and no native confirmation. Engine source documents and implements restoration over current data. Only the demo backend received the request; no user's data was restored. Recommendation for a pre-restore backup is an additional safeguard, not a proven backend requirement. |
| **F9** | **Confirmed desktop capability gap; P2 retained** | UI offers Git, image and catalogue service with no arbitrary Compose import/project-environment controls. Source confirms omitted advanced create/editor settings; platform APIs expose the relevant fields. Reran actual Compose/build-plan test successfully. **Correction:** catalogue services already use Compose. Missing arbitrary Compose configuration is not missing Compose support. Repository configuration/CLI/API alternatives remain available. |
| **F10** | **Confirmed and strengthened; P2 retained** | Single-disk UI still enables HDD in the database form and Volume assistant. Real Volume assistant API returned 409 on an unmounted path. **Different database behavior:** real initial database API accepted HDD, recorded `tier=hdd` and created its bind directory on the unmounted parent filesystem, with Docker stubbed. Thus mount protection cannot be generalized to all creation paths. No real database container was launched. |
| **F11** | **Confirmed; P2 retained** | Real database deletion API returned `data_kept=true`; synthetic data on disk remained, but its volume record vanished from `/v1/platform/volumes`. Rendered deletion guidance says delete data on Storage; Storage has no delete/purge control. Docker cleanup was stubbed. Physical-data retention itself is intentional; the issue is inaccessible cleanup/misleading guidance. |
| **F12** | **Externally verified; P2 retained** | Fresh GitHub latest-release response lists v0.2.5's universal DMG and checksum. README still says no ready-made download exists and lists developer tools for normal installation. Asset metadata was checked, not installation/notarization of that DMG. The preference for one primary install path is a recommendation; the stale download claim is an objective mismatch. |
| **F13** | **Supported configuration/copy gap; P2 retained** | Sourced actual installer defaults and parsed no arguments: `remote=` empty, `configure=0`, `storage=none`. `remote_on` is guarded by `REMOTE==1`; normal wizard returns without optional questions. Onboarding says “Anywhere else” without checking remote readiness. **Correction:** README does include a later remote-access prerequisite. No separate-network/CGNAT failure was reproduced. The issue is inconsistent readiness messaging, not proof remote networking is broken. |
| **F14** | **Confirmed; P2 retained** | Opened real New app dialog, focused Repository and traversed with Tab. Focus reached the body while the dialog remained open. Shared Modal source has no focus trap/background inertness. This reproduces one dialog's focus escape; nested-dialog focus restoration and command-palette accessibility were not comprehensively audited. |
| **F15** | **Reclassified; P3 recommendation** | Confirmed that several recovery/setup actions require commands. **Withdrawn:** the broad assertion that users cannot tell which machine to use. Existing instructions often say “On the server,” “Change it on the server,” or “More detail on the server.” No nontechnical usability study was run. A host-admin graphical repair flow would reduce work, but absence alone is not a reproduced functional bug. |
| **F16** | **Reclassified; P3 recommendation** | Default Mac install omits FFmpeg/Moonlight, but `--with-display` already installs both via Homebrew; Linux installer includes mpv by default. Existing UI has separate mirroring/control sections, preflight, refresh and Sunshine setup. Targeted display-installer test passed. **Withdrawn:** any implication that these aids/separate sections are absent or that display functionality was proven broken. Physical capture/permissions/Wayland/latency remain untested. |
| **F17** | **Confirmed; root cause corrected; P2 retained** | Corrupt browser settings JSON produced first-time onboarding while original invalid data remained until save. Native `read_config` explicitly turns read/parse errors into default empty config. **Correction:** corruption is generally swallowed in the loader, not delivered to the shell rejection handler blamed originally. Native file-permission errors and a running installed Tauri app were not exercised. |
| **F18** | **Battery-warning gap reproduced; power-on claim qualified; P2 retained** | Real collector/doctor with synthetic sysfs: 100% charge, 3.5% design health, no battery-wear warning. UI separately verified to **already display health 3.5%**. The doctor is not wholly green in this fixture; only the absence of wear-specific checks was established. Automatic power-on after full shutdown is hardware-dependent and remains untested on the physical laptop. |

## Reproducible evidence

- [Host/agent harness](findings-host.py) and [results](host-results.json): real update
  API/summary, credential validation, collector/doctor, mounted-volume rejection,
  database creation/deletion, default backup repository and installer flags.
- [UI harness](findings-ui.mjs) and [results](ui-results.json): actual React components
  using demo fixtures. Fixture changes are limited to a private domain, single disk,
  local backup target and worn battery. Instrumentation records requests and emits a
  `git-sources` event; browser opens are captured to avoid external actions.
- [Source evidence](source-evidence.json): source hashes, paths and line excerpts for
  conditional findings and implementation boundaries. An empty `environment_id`
  match records its absence from the desktop form; it is not a failed runtime test.
- [Latest-release metadata](release-results.json).
- [Real-agent GitHub callback integration output](github-callback-test.txt).
- [Compose/build-plan test output](compose-test.txt).
- [Optional display-installer test output](display-installer-test.txt).

From the repository root:

```bash
# Build the isolated host harness's agent if needed.
cargo build -p vercelify-agent --locked
python3 docs/validation/2026-10-30/findings-host.py

# In one terminal, serve the demo; all browser state is isolated.
cd cockpit
VITE_DEMO=1 npm run dev -- --port 1431 --strictPort

# In another terminal, from repository root:
node docs/validation/2026-10-30/findings-ui.mjs

cargo test -p vercelify-agent --test e2e github_app_and_webhooks --locked -- --exact
cargo test -p vercelify-platform --lib detect::tests::explicit_packs_contexts_images_and_compose --locked -- --exact
bats --filter 'display' host/tests/install-mac.bats
```

Results contain synthetic test values only; the host fixture is removed after
execution. The systemd and Docker stubs are the existing agent-test tools. They
allow actual HTTP/SQLite/filesystem paths to run without updating this machine or
launching production containers. They do not establish real container health,
systemd upgrade behavior or media performance.

## Corrections to the original review

1. F2/F3 are conditional networking integration findings, not fresh demonstrations
   of failure on a clean physical Mac.
2. F9 concerns arbitrary Compose configuration; catalogue Compose services exist.
3. F10 has two backend behaviors: Volume assistant rejects an absent mount; initial
   database creation can use a directory on the parent disk while labeling it HDD.
4. F13's prerequisite is documented in README, but not reflected in onboarding.
5. F15/F16 are now P3 improvements. Existing machine labels, display setup automation,
   separate controls and preflight were under-credited in the original language.
6. F17's primary cause is the loader's fallback, not the shell's rejection handler.
7. F18's missing wear warning is confirmed; health display already exists, and
   physical power-on behavior has not been validated.

The earlier broad test-suite results remain historical results from the review
turn. This validation relies on the targeted evidence above; it does not claim
that every previously listed suite was rerun or that all 84 feature families are
now certified end to end.
