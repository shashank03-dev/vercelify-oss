# Attended network recovery procedure — window completed

Status: operator approved the ten-minute window. The automatic Wi-Fi reconnect
test completed successfully. Hotspot off/on completion was not confirmed; that
portion is unqualified. The window is over; no new disconnect is authorized.

Original procedure: ten-minute window on 100.123.113.77. Keep AC connected, lid open, hotspot phone
nearby. Preserve all production containers, PostgreSQL, PR1 and media. No battery,
firmware, thermal-stress, router factory-reset or payment actions.

Before: save network state privately, record Wi-Fi interface/profile/default route,
start Mac app-route probes and laptop-local production probes. Schedule an owned
local systemd recovery timer to reconnect wlp1s0 after 20 seconds; verify that the
timer is armed before disconnecting the interface. Disconnect only the Wi-Fi
interface once. The recovery timer reconnects the same saved profile. Observe
Tailscale and application-mesh recovery, original routes, database readiness,
container/deployment identities. Abort further actions if recovery takes >2 minutes;
operator reconnects the original hotspot locally if needed. Remove only the owned
recovery timer after confirming recovery. This qualifies host link recovery, not
physical router/ISP recovery.

If the operator can control the hotspot phone, a separate observed 20-second
hotspot off/on action can test the actual access point path. The operator must
confirm ability to turn it back on before that action. Do not change passwords,
phone network settings, DNS servers, ISP settings or other router configuration.
A successful hotspot test still does not qualify an unavailable separate router
or deliberate ISP outage. Confirm a specific window before any disconnect.

Battery facts remain charge_full/design 130000/3684000 (~3.5%). Do not unplug or
attempt depletion/removal based on the misleading 100% charge display. Missing/
healthy-battery configurations and firmware AC-return behavior require appropriate
hardware and a separately safe attended procedure, not a simulated result.
