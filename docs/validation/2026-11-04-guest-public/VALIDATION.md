# Public guest access and physical network/thermal qualification

The guest test uses only the owned nested daemon vfy-final-qualification-docker,
with a synthetic sentinel and private PR7 fixture. Nothing on the physical serving
Docker daemon is exposed as a guest or used for lifecycle mutation.

The actual Cloudflare named tunnel terminates trusted public HTTPS for exact
shr-*.shashankdev03.me invitation names. Individual DNS records are created; no
zone wildcard record or existing personal-site record is replaced. At the origin,
cloudflared verifies a short-lived wildcard certificate using an explicitly supplied
owned CA. No certificate validation bypass, browser DNS override or ignored TLS
error is used for the public checks. The origin reaches the real TLS proxy over
an owned SSH forward into the isolated lab; the control API is not exposed.

## Verified results so far

- Trusted public-domain Chromium invitation scanner navigation never reaches the
  app, clears the fragment before explicit confirmation, sets host-only Secure/
  HttpOnly/SameSite=Lax session cookies, refuses service workers and prevents reuse
  of a single-use invitation in a separate browser context.
- An external Vercel function outside the LAN/mesh denies unauthenticated pages,
  assets and advanced routes, including forged internal-control headers.
- External redemption, cookie policy, streaming HTTP, app-Authorization preservation
  and internal-header/guest-cookie stripping pass.
- An established external WebSocket echoes data, then closes after revocation;
  a subsequent request with its cookie is rejected. Induced server-side expiry
  likewise closes an established WebSocket and rejects reuse.
- A queued replacement preview generation is admitted, started and published by
  the real agent; the prior guest session then fails closed. PR close similarly
  invalidates a valid external session.

PR fixture records are seeded only in the owned lab, matching source-observer
inputs; tests do not forge a healthy deployment or disable admission. Initial wake
setup omitted the preview active flag when reopening its fixture; the queued job
was correctly cancelled. A corrected active fixture was used, without a product
change. The fixture watcher is intentionally disabled; wake qualification invokes
normal sleep-controller passes with the actual configured idle interval unchanged.
New guest DNS propagation and protected cloud-probe session renewal are setup
considerations, not silently counted as application failures or fixes.

## Physical Wi-Fi window

The operator explicitly approved a ten-minute recovery window. AC stayed connected,
lid stayed open. A local systemd reconnect timer was armed/verified before one
Wi-Fi disconnect; it reconnected the saved interface after twenty seconds.
External app routes went offline and recovered; local production HTTP continued
succeeding. Final checks preserve boot ID, original five container identities,
all three services and PostgreSQL readiness. The actual hotspot off/on action
was requested, but no operator completion was received before the window ended:
**not qualified**. No router factory reset, ISP/DNS configuration, battery, firmware
or additional power action was performed. Host link recovery is distinct from
physical router/ISP recovery.

## Bounded physical thermal observation

Real sysfs sensors were sampled for eight seconds of baseline, thirty seconds of
quarter-core owned CPU work, and forty seconds after the worker exited. The worker
was bounded in time and would abort at 70C; load was skipped if baseline reached 65C.
The observation completed with 78 samples: baseline max 58C, load max 59C, post-load
max 65C. The maximum occurring after load is retained; this is not a claim that every
sensor cooled below baseline. High-temperature policy stages, hysteresis, sustained
stress and critical shutdown are not qualified by this low-load observation.

## Hardware limits

Battery charge_full/design remains 130000/3684000 (~3.5%) despite its 100% display.
No AC unplug/bounce, depletion, missing-battery or firmware auto-power-on evidence
is invented. Those need a separately safe attended procedure and appropriate
hardware. Existing software policy tests are not physical evidence.

## Real idle and public guest wake

wake.json records a 326-second idle wait with normal five-minute policy semantics.
The isolated fixture disables its background watcher; normal API sleep-controller
passes were invoked at regular intervals without changing the clock or seconds
per minute. The product stopped the real preview container. An unauthenticated
public request returned 401 and left it asleep. The authenticated cloud guest
request woke it and reached HTTP 200 in 8.42 seconds as observed by the multi-request
probe. This is end-to-end observed recovery time, not a pure container boot metric.

## Public stream revocation and browser compatibility

stream-revocation.json records a streaming HTTP response that had delivered data
before revocation; the external client observed closure 0.69 seconds after the
revoke call. Subsequent use of the revoked cookie was denied. Expected tunnel
EOF/context-cancel messages accompanied these intentional connection terminations.
Firefox and WebKit completed the same public invitation/cookie/scanner flow as
Chromium. The initial Firefox assertion failed with only a generic suppressed-URL
error; after adding secret-safe diagnostics, the fresh-invitation rerun passed
without a product change. Its initial cause is unresolved and retained in
browser-firefox-initial.txt; it is not evidence of a consistently flawless first
attempt. New-host DNS availability is now checked before launching further browsers.

## Public security lifecycle across restart

restart-security.json verifies public session continuity across a real owned-agent
restart, permanent session rejection after the isolated control master key was
replaced, and inability to revive old guest access by toggling preview privacy or
enablement off and back on. Only the isolated lab was restarted or rekeyed.

## Cleanup and remaining qualification gates

Cleanup removes the exact owned DNS records and named tunnel, stops the connector
and SSH forward, removes the owned lab apps/proxy and private fixture state, then
stops its dedicated nested daemon. Original DNS records are compared with the
pre-test snapshot. The external probe project is paused again, not deleted.
Original personal-site HTTPS and physical production/PR1 remain separate checks.
No invitation URL, cookie, tunnel token, origin private key or Vercel share-link
credential is retained in these committed artifacts.

The physical Wi-Fi reconnect, low-load real thermal observation and listed public
guest scenarios now have runtime evidence. This is not a completion claim for:
actual hotspot/router/ISP power-cycle recovery (operator action not confirmed),
AC interruption/bounce, healthy/missing/aged-battery outage matrix, depletion,
firmware automatic restart, or thermal policy thresholds/critical shutdown.
The badly worn battery and absent physical actions cannot be replaced by VM or
synthetic-sensor evidence. Do not claim the whole project completely qualified.
