# Guest links implementation plan

Execute inline in the main chat; no sub-agents. The user opted for direct execution
of the existing roadmap. Spec: `docs/superpowers/specs/2026-11-04-guest-links.md` and
companion competitive validation §7.3. No push or external message is authorized.

1. Store red/green: SQL13 domains/grants/sessions/clock, hash-only secrets,
   generation/private-preview validation, expiry/revoke/single-use/rotation and caps.
   New `crates/vercelify-platform/src/guest_shares.rs` and tests own these rules.
2. Proxy integration: explicit protected guest aliases in RouteHost/hosts_for,
   matching middleware on advanced rules, namespace reservation, wake exclusion,
   fail-closed global guest routing. Pin routing/security tests before changes.
3. Agent gateway: owner API, invitation fragment/confirmation/cookie handling,
   bounded streaming proxy and upgrades with live revocation. Reuse wake listener,
   preview wake admission and existing HTTP primitives; no new daemon.
4. Cockpit/demo: capability-gated private versus guest sharing, one-time secret,
   expiry/session limits, revocation, public-domain prerequisite and no public comments.
5. Run store/HTTP/browser/real Docker privacy and socket contracts, relevant
   regressions, all package checks, static size and matched six-app memory. Update
   SYSTEM-DESIGN/PLATFORM/research and evidence; exact-file commit; continue M8.

Review focus: same-host invitation with an existing session; forged headers after
revoke; old generation becoming active again; clock rollback across restart;
WebSockets/streaming bodies already in flight. Each needs a direct regression.
