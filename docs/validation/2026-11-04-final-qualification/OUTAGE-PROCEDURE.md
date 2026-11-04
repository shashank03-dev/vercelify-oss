# Physical test window — approval required

Target: user-inspiron-5567-1 / 100.123.113.77. This procedure is prepared, not approved or executed.

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
