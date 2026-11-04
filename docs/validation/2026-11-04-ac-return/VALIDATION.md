# Attended AC-return observation — 2026-11-04

Procedure: PROCEDURE.md. The laptop (Dell Inspiron 5567, BIOS 1.1.7, no firmware
changes) was shut down normally on AC. With it confirmed fully off, the operator
unplugged the charger for twenty seconds and reconnected it.

**Automatic start on AC return: not observed.** The screen stayed dark for 118 s
of remote observation (automatic-start.json). Under the current firmware
configuration this machine does not power on when AC returns; an operator must
press the power button after an outage that outlasts the battery.

After one brief button press the laptop booted (new boot ID) and recovered
without manual service restarts (recovery.json): vercelify-agent, vercelify-mesh
and vercelify-power-guard active; the original five containers unchanged; the
control store passed integrity_check; deployments and jobs unchanged; PostgreSQL
logged a clean shutdown (18:18:45 UTC) and was accepting connections by 18:33:22.
Production and PR1 routes returned 200 from 18:33:27 UTC.

## Open finding: post-boot mesh outage

The external probes (through the Vercelify mesh, 10.77.0.1) failed again for about
40 s, 18:34:23–18:35:05 UTC, plus one sample at 18:36:18, after the routes had
already recovered. No container or service restarted in that window. The laptop
journal shows the Mac joining over the mesh relay at 18:34:00, `vercelify-remote`
falling back to a Cloudflare quick tunnel at 18:34:10–20, and a new mesh peer map
at 18:35:29. The outage most likely comes from the mesh path changing shortly
after boot; the Mac-side mesh logs were not captured, so the cause is unconfirmed.

This qualifies AC return while already powered off. It does not qualify AC loss
while serving or battery depletion (see ../2026-11-05-power-thermal/).

Evidence: PROCEDURE.md, preflight.json, external.jsonl, automatic-start.json,
recovery.json and the operator-confirmation timestamps.
