# Physical test window — approval required

Target: user-inspiron-5567-1 / 100.123.113.77.

Status: the revised first window below was explicitly approved and executed. See
../2026-11-04-attended/VALIDATION.md for results. Other windows remain unapproved.

## Attended first window (revised 2026-11-04, executed)

The operator confirmed physical presence and access to the charger. Current checks
show all three Vercelify services active, all five protected containers running,
81 GiB free, AC connected, and reported thermal readings of 20–58°C. BAT0 reports
100% but charge_full/charge_full_design is 130000/3684000 (about 3.5%): percentage
alone is insufficient grounds to disconnect AC. This revised first window
supersedes Window A below: **no AC disconnects or bounces**.

Allow up to 30 minutes, with interruption of every service possible. Keep the
charger connected. Record external Mac reachability and original HTTP routes;
perform one ordinary reboot, allowing at most ten minutes for recovery. Confirm
the original services, containers and routes recover before continuing. Then ask
the operator to close the lid for two minutes and reopen it. If this suspends the
host, the operator reopens it and presses the power button briefly if necessary;
stop further actions if recovery fails. Record whether lid closure actually
suspends the host rather than assuming either behavior. Do not change firmware,
power policy, networking or production configuration. No shutdown/depletion,
battery removal or thermal stress is included. AC and router tests need separate
procedures and approval. External Internet recovery is not established by the
Mac's mesh probes.

Fresh private rollback material is at
`/var/lib/vercelify/qualification-attended-20261104`: configuration, consistent
SQLite copy (integrity_check: ok), master key, protected container inventory and
PostgreSQL cluster dump with a completion marker. Dump generation is verified;
this new production dump has not been restore-qualified. The existing independent
PG17 fixture recovery evidence remains distinct from this production PG15 dump.
Do not restore any of these files over live state automatically.

Historical planning text below was approved for this revised first window only.
The reboot and lid actions are complete; do not reuse that approval for further
outages.

Prerequisites: authenticated Tailscale SSH, operator physically present with charger,
verified independently readable PostgreSQL 17 fixture backup and retained credentials,
current configuration/binary/SQLite rollback copies, available disk and thermal headroom.
Inventory production, gw PR #1, PostgreSQL and voxbento; record IDs, generations,
restart counts, volumes and health. Do not delete or reconfigure these resources.
Mac is external monitor: timestamp Tailscale reachability, agent health and production/
preview HTTP every second, retain errors and recovery times. An Internet observer is
also required for public paths; a Mac using mesh is insufficient for that claim.

Window A: 30 minutes, possible whole-host interruption. First ordinary reboot
(10-minute recovery limit), then operator lid close/open at idle (2 minutes), brief
AC disconnect/reconnect (30 seconds, only battery >60%), and three AC bounces
(10 seconds apart). Disposable opted-in fixture only. Stop immediately on production
health errors beyond reboot, temperature above vendor safe range, missing battery
telemetry, unexpected pause ownership or SSH loss >2 minutes outside reboot.
Never disconnect AC with missing/unsafe battery. Preserve manual-stop intent; capture
journal generations, build IDs, pause ownership, debounce and recovery times before/
after each action. Verify no duplicate jobs and only matching generations recover.

Window B: separate 60-minute approval, whole-host outage expected. Graceful shutdown,
operator confirms off, AC removal/return, document exact firmware setting and observed
automatic/manual boot. Router/ISP/DNS changes require network-owner approval and a
local rollback path. No router factory resets or firmware changes. Restore original
network configuration immediately on unexpected impact. Battery depletion, battery
removal/aged battery and sustained thermal load need separate model-specific safe
procedures and available hardware; they are not authorized by either window.

Recovery: operator powers on manually if needed; restore saved candidate binary/config
and consistent control-store copy only if candidate rollback is needed, preserving the
newer state separately. Verify original app routes, PostgreSQL data assertions, PR #1,
media container, manual stops and absence of duplicate jobs. Retain logs even on failure.
Actual outage actions remain blocked until user approves a specific window and confirms
physical presence, backups and recovery access. Approval is not inferred from silence.
