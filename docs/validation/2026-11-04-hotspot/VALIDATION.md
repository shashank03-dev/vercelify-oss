# Physical hotspot off/on recovery — 2026-11-04

The operator confirmed readiness, reported turning the phone hotspot off, and then
confirmed it was back on. Instructions were to leave it off for twenty seconds,
keep the laptop on AC with lid open, and preserve hotspot credentials. Exact
operator action timestamps/duration were not measured, so this is not a precise
20-second outage timing claim.

External Mac app-mode probes and an independent laptop-local HTTP observer were
running before the action. The laptop re-associated, obtained a changed DHCP
address (10.90.182.207 to 192.168.49.207), and recovered both production and safe PR1
routes without manual service restart. The DHCP event at 18:12:53 UTC correlates
with the last external outage episode, 18:12:41–18:13:21 UTC (about 40 seconds).
Two earlier brief external drops (about 20 and 5 seconds) are retained in summary.json;
their causes are not assigned to the operator action without exact timestamps.
Both routes remained HTTP 200 in subsequent samples through the observation end.

All 117 laptop-local HTTP samples succeeded. The boot ID and original five container
identities were unchanged; all three Vercelify services remained active and
PostgreSQL accepted connections. No containers, keys, production data, profile
credentials or persistent network settings were changed by this observer.
Only the owned local and Mac monitoring processes were stopped afterward.

This qualifies recovery from the observed physical hotspot interruption and DHCP
address change. It does not qualify a separate router reboot, ISP outage, arbitrary
DNS failure or high-load network resilience. Earlier Wi-Fi-interface recovery
has its own evidence in ../2026-11-04-guest-public/.

Battery/AC interruptions, depletion, missing-battery configurations, firmware
AC-return boot and critical thermal policy remain unqualified. The severely aged
battery has not become safe for unattended power-disconnect tests because this
network test passed. Firefox's earlier initial public guest-browser failure also
remains recorded separately despite its successful retry.

Evidence: before.json, after.json, external.jsonl and summary.json.
