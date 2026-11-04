# Attended reboot and lid qualification — 2026-11-04

Operator explicitly approved the revised 30-minute reboot/lid window and confirmed
physical presence. AC remained connected. No AC/battery/router/firmware actions.

One physical reboot completed: boot ID changed from
6330a3ca-a8bf-4569-bca5-6da51a80a4dc to
7b8344a4-1aca-4ad7-9a7c-f66252932edf. First failed external HTTP probe was
13:53:56 UTC; first subsequent HTTP 200 was 13:55:48 UTC (about 111 seconds).
This was initial recovery, not continuous availability: later drops occurred.
All three services active, original five container IDs preserved, PostgreSQL
accepting connections, and control-store integrity_check returned ok.

Operator closed and reopened the physical lid; journal recorded lid closed at
19:27:27 IST. Network access was unavailable during much of this period and
returned following reopening. Host boot ID and original container start times
remained unchanged. No kernel suspend/resume events were found. Lid switch policy
is ignore on AC. This first attempt did **not qualify** closed-lid availability;
the later short retest below supersedes that result for its tested conditions.
Earlier post-reboot drops confound attribution; physical Wi-Fi, routing and mesh
need further diagnosis. Do not describe this as a successful suspend test.

The host also has an AppArmor service failure: the unrelated Discord profile
references missing abi/4.0. No profile or security policy was modified.

Fresh private rollback material lives under
/var/lib/vercelify/qualification-attended-20261104 on the laptop. The fresh PG15
dump has a completion marker but was not restore-tested. No production restore
was attempted. No claims about physical policy thresholds, duplicate-job fencing,
AC bounce, depletion, thermal stress or public Internet recovery follow from
this window. Mac probes use existing app-mode mesh gateways and Tailscale;
monitor intervals are about 1 second on success and about 2 seconds on timeout.

See monitor.jsonl, transitions.json, reboot-recovery.json and lid-recovery.json.

## Lid-open follow-up

The bounded follow-up was interrupted; recorded sample counts and failures are
in lid-open-summary.json. Local production HTTP and hotspot gateway probes had
no failures in the retained samples; remote routes had a timeout with the lid
open. This narrows the investigation to the network path but does not isolate
Wi-Fi, upstream connectivity, Tailscale or the application mesh. Wi-Fi signal
was about -78 to -79 dBm and power saving was on. Neither setting was changed.

## Hotspot moved closer

The phone was moved closer by the operator. Signal improved to about -46 to -47
dBm, and the default gateway also changed to 192.168.127.182. This is a changed
network baseline, not an isolated distance-only experiment. See
hotspot-closer-summary.json for completed sample counts. The first collector
used the old fixed gateway; discard those gateway failures. A separate collector
checks the current route gateway. No persistent settings changed and no further
lid closure was requested. This short lid-open run cannot qualify closed-lid
reliability or the remaining physical power matrix.

## Timed closed-lid retest after hotspot repositioning

Operator confirmed lid closed; timed interval began at 14:21:37 UTC. Reopening
was requested after two minutes. The summary timestamp records when that request
was saved, and samples extend through that recording time. See
lid-retest-summary.json for exact retained sample counts: both remote routes
returned HTTP 200 throughout the timed samples; local HTTP also passed, AC
remained online and boot ID did not change. This establishes a successful short
closed-lid availability test on the stronger hotspot connection, not suspend
recovery or long-duration thermal/network qualification. Earlier connectivity
drops remain historical failures; hotspot signal/route changes confound a precise
root-cause claim. No persistent host configuration was changed.

## Final post-reopening state

Operator confirmed reopening. window-final.json records all three services
active, the original five protected containers running, PostgreSQL accepting
connections, AC connected and both remote HTTP routes returning 200. No further
outage actions are approved by this completed window.

## Post-window read-only audit

state-comparison.json confirms identical deployment IDs/statuses/container mappings
and zero build jobs before and after the idle window. Both SQLite integrity checks
returned ok. This observes no new jobs; it does not exercise in-flight generation
fencing or claim duplicate-job prevention under active workloads. Rollback files
have private permissions, saved config is present, and the PG dump completion marker
is present (still not a production restore qualification). The previous boot also
records the same Discord abi/4.0 AppArmor error, establishing it predates this reboot.
