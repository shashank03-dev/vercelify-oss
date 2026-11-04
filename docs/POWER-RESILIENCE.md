# Power resilience on owned hardware

The independent Linux power guard can shut down a machine cleanly when its battery
is low even if the agent is unavailable. This reduces risk; it cannot guarantee
availability or preserve writes after sudden loss of power. Keep independent,
verified backups and allow time for shutdown before battery exhaustion.

## Opt-in battery conservation

In `/etc/vercelify/power-guard.conf`, set `BATTERY_POLICY_ENABLED=1`, then restart
`vercelify-power-guard`. Existing installs retain their configuration; the new keys
have defaults even when absent. Install the agent and guard from the same candidate.
Before downgrading to an agent without battery-stage support, disable policy and let
pending recovery finish. The installed guard needs its companion
`/usr/local/lib/vercelify/lib/battery-policy.sh`, installed by the host installer.

| Battery at or below | Action after two consistent readings |
| --- | --- |
| 50% | Hold admission of new builds |
| 40% | Pause this installation's BuildKit/AI work |
| 30% | Sleep previews explicitly opted into app Sleep **and** its previews toggle |
| 20% | Also sleep production apps explicitly opted into Sleep |
| 15% | Existing clean shutdown: owned workloads first, then core, then power off |

An app with Sleep enabled is a noncritical workload under this optional policy.
Keep Sleep disabled for an app that should continue serving until shutdown. Critical
apps still become unavailable when the machine loses power. Database services are
not independently put to sleep by these battery tiers. Sleeping a Compose app stops
its sidecars through the existing engine lifecycle.

Thresholds, debounce readings, recovery delay and charge headroom are configurable.
Recovery requires sixty seconds continuous AC and charge at least five percentage
points above the tier that was entered. AC bounce or backward clock movement resets
the interval. Missing/invalid readings cannot release held work. No battery reported
by a Linux VM means no battery evidence; it does not inherit its host's battery.

The guard touches only platform-labelled containers and exact installed core/builder
names. It resumes only its own paused work and does not start a stopped build.
Power-sleep intent, reason and deployment identity persist before the engine stops
containers. Stable recovery wakes only matching generations through normal admission.
Manual app stop, PR close and generation replacement take precedence. Previously
idle-sleeping apps stay asleep. Visiting a power-slept app shows “Waiting for power”;
it does not override conservation. Keep sleep settings until automatic recovery finishes; turning
them off while a power-sleep journal entry remains is refused with an explanation.

Thermal safeguards take priority. A failed or stale guard sample does not authorize
a power-slept app to wake. The existing build journal/gate retains ownership of jobs;
this feature does not enqueue replacement builds. Boot restore starts core services;
the agent restores platform workloads from declared state, avoiding resurrection of
a manually stopped app from an old raw container-ID manifest.

## Physical qualification

Firmware AC recovery is vendor-dependent. Doctor reports it as unverified and offers
manual guidance; Linux cannot promise that firmware will power on after shutdown or
a depleted battery. Read the vendor's settings for this exact model and test both
conditions. A setting working after an ordinary reboot proves neither condition.

Use a disposable app and database with a verified backup. Record model/firmware,
OS/kernel, battery design/full capacity, shutdown thresholds, cooling, lid settings,
network equipment/UPS and power source. Observe externally, without depending on
the tested laptop for monitoring. Record timestamps, serving gaps, errors, durable
writes and recovery outcomes for each row:

| Scenario | Current physical result |
| --- | --- |
| Lid closed, running and under load | Unmeasured |
| Unstable AC / repeated unplug and reconnect | Unmeasured |
| Missing or aged battery | Unmeasured |
| Thermal load and cooldown | Unmeasured |
| Graceful shutdown then AC return | Unmeasured |
| Full battery depletion then AC return | Unmeasured |
| Reboot and restore of declared app state | Unmeasured for the laptop; lab coverage is separate |
| Router, DNS, mesh/tunnel and ISP recovery | Unmeasured |
| Independent off-device backup recovery | Unmeasured physically; logical lab recovery is separate |

The field-test laptop at `100.123.113.77` was reached through Tailscale on
4 November 2026. Its saved pre-M3 build settings have been restored, retaining
its subsequently corrected mesh public URL; both configurations were backed up.
Production, PostgreSQL, PR #1 and the unrelated media container retained their
container identities. Physical outage tests remain deferred until an operator
can attend; the prepared [test window](validation/2026-11-04-final-qualification/OUTAGE-PROCEDURE.md)
is not approved for unattended execution. No physical qualification is inferred
from the ARM lab's simulated sysfs, real containers or controlled process restarts.

Router and ISP loss can make a healthy laptop unreachable. This is not a cloud SLA.
A UPS/network redundancy helps only when independently configured and tested. Use
wall-meter measurements for electricity claims; no savings number is supplied here.
