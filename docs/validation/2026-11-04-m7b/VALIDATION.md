# M7b guest invitation validation

4 November 2026 (Asia/Kolkata). Local working-tree qualification; no push or public
PR comment. Main-node private previews only. Public Internet DNS/TLS, a real tunnel,
external IPv6 and the offline physical laptop remain unqualified.

SQL13 adds hash-only invitations/sessions, reserved namespaces, clock high-water
state and a sealed key-rotation probe. Owner-only management uses the existing
paired API. Public requests use the existing wake listener, before app routing or
wake admission. All backend aliases require service Basic Auth plus session
ForwardAuth, then internal-header removal. Advanced routes inherit the same chain.
A proxy file contains only the bridge password's bcrypt verifier; the random
bridge password itself remains in memory. App Authorization is preserved separately.

`checks.json` records 793 all-feature Rust passes, fourteen Docker-gated ignores,
and one subsequently added advanced-routing regression pass. Clippy, formatting,
ShellCheck, UI types/build, 150 UI unit cases, 234 Linux bats cases (229 unprivileged
plus five root-only installer cases; six installer cases rerun root), 32 existing
demo browser cases and one new sharing-dialog browser case pass. Both static agents
remain below 8 MiB; exact bytes and SHA-256 are in `binaries.json`.

`real-qualify.txt` exercises real TLS/Traefik, an actual Node private preview and a
separate public sentinel inside the owned disposable Docker daemon. Missing cookies,
forged bridge/forwarded headers and control-API paths fail closed. Redemption,
application Authorization preservation, internal-header/cookie stripping, no-store,
no-referrer and worker restrictions pass. Revocation closes open HTTP response and
WebSocket streams within three seconds; induced expiry also closes upgraded access.
All 80 concurrent public-sentinel probes succeed.

`real-browser.txt` uses Chromium outside the Linux guest, via an SSH port forward,
without a Vercelify mesh identity. A scanner navigation reaches no app, clears the
fragment before explicit confirmation and sets a host-only Secure/HttpOnly/Lax
cookie bounded to fifteen minutes. Service worker registration is refused. Opening
the invitation again with an existing session safely clears its fragment before
continuing; a separate browser cannot reuse the consumed invitation. Self-signed
TLS is accepted explicitly in this lab fixture; it is not public CA/DNS proof.
Invitation URLs/cookies are never printed. Synthetic handoff facts live mode 0600
inside the disposable daemon, outside tracked evidence.

`lifecycle.txt` confirms an actual agent restart preserves a valid session, actual
control-key replacement revokes it, toggling privacy/preview enablement cannot
resurrect it, and closing the PR ends real-proxy access. Store tests additionally
cover concurrent single-use redemption, eight-session caps, clock rollback,
permanent expiry, wrong host, generation replacement and manual-stop retirement.
`privacy-red.txt` records the failing privacy-toggle regression before its fix.

The real deployment/rollback/recovery, PostgreSQL preview, restore-drill and preview
network/privacy Docker contracts pass with this source. Restore fault qualification
records 578 unchanged production probes with zero failures; PostgreSQL preview
qualification records 367 with zero failures. These contracts run only in the
owned nested daemon, preserving unrelated Mac containers and original lab apps.

Qualification corrections: asynchronous Traefik file reload initially produced a
404 immediately after creation. Issuance now probes both redirect and protected
alias readiness before returning the invitation, and revokes on readiness timeout.
The live streaming test passes with the resulting candidate. An existing-session
invitation-page case and a privacy-toggle resurrection case were found and fixed.
An initial browser run against the earlier candidate fails; only the final candidate
run is counted. Dialog animation assertions now wait for visibility.

`lab-install.txt` records the schema13 lab install and rollback backup. The matched
six-app memory samples and research §6 comparison are recorded alongside this file;
the disposable daemon is stopped and BuildKit is absent during sampling. No guest
DNS domain is configured on the serving lab; this is the installed idle-feature cost.
The system-used median is **634.5 MiB** (625–648), agent **13.37 MiB**, mesh
**3.10 MiB**; all six original routes remain HTTP200. This is not a causal overhead
or guest peak-memory claim. The laptop's saved build settings still require restoration when it reconnects.
