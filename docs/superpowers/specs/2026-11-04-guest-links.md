# M7b guest-link privacy contract

Implements the already-authorized roadmap and companion validation §7.3 in the
main chat, with no sub-agents, no push and no messages/PR comments sent to others.
The user asked to proceed directly. These are implementation choices within that
scope, not an additional approval request.

## Audience and public route

Only an Owner may configure a dedicated guest HTTPS domain, create invitations,
revoke them or rotate guest security. Private preview hosts, IP allowlists and
production domains are unchanged. Only active private PR previews on the main node with a current
healthy generation can be shared. No configured public domain means the existing
private URL still requires the Vercelify network. This feature provisions no cloud
relay and does not configure the operator's DNS/tunnel. Public DNS/TLS reachability
is reported as an operator prerequisite, never inferred from a configured string.

Worker-hosted previews are outside this first guest release; old workers must never
receive a guest alias they could interpret without its authentication boundary.

The guest domain must be separate from application-domain namespaces. It is
reserved against app/custom-domain/wildcard claims, including after a grant is
revoked. Retained domain reservations are bounded (16). Generated public hosts are
unique per invitation; grants are bound to app/PR/current deployment generation.

## Credentials, redemption and lifetime

A 256-bit opaque random secret is shown once in an invitation URL fragment on a
reserved invitation path. Only its SHA-256 digest is stored. Metadata never exposes
the secret. The invitation page removes the fragment immediately and requires an
explicit confirmation POST; unauthenticated GET/link scanners cannot consume a
single-use invitation. The POST requires the exact HTTPS Origin for its host,
JSON and a bounded body. No token travels in a query string, log, redirect, public
PR comment or preview environment. No automatic PR comment is created.

The redemption page has a restrictive nonce CSP and no external resources. The
reserved invitation path is intercepted even when a session cookie already exists,
so preview JavaScript never receives a fresh invitation fragment. Guest app responses
add `worker-src 'none'`: worker/service-worker functionality is unavailable through
guest links in this release, preventing a previously loaded preview worker from
intercepting invitation navigation. Preview code remains untrusted for credential
exposure; bridge/session headers and guest cookies are stripped before the backend.

Redemption mints a separate random, hashed-at-rest session. Its cookie is host-only,
Secure, HttpOnly, SameSite=Lax, Path=/ with a `__Host-` name. Sessions last at most
15 minutes and never exceed the invitation expiry (default one hour, maximum seven
days). Single-session invites are the default; reusable invites are explicit and
bounded to eight live sessions. The global live session cap is 128; active grants
cap at 100. Expired sessions are pruned. A single-use invitation consumed once cannot
renew a session; recipients need a new invitation when that session ends.

Every request checks revocation, expiry, configured domain, active/private PR and
exact generation before serving assets, proxy rules, waking or upgrading sockets.
Closing/reopening a PR, changing preview privacy or enablement, and manually stopping an app permanently revoke its grants. A clock high-water mark persists;
a backward wall-clock jump fails closed. Owner rotation revokes grants/sessions and
resets the clock mark. A sealed constant detects control-master-key replacement and
revokes old grants. Normal restart preserves valid hashed grants/sessions.

## Ingress and bounded proxy

The existing wake listener hosts a dedicated guest gateway, not the agent control
API. Normal public guest requests are intercepted by a reserved-domain router above
all application `vercel.json` priorities. After cookie validation, the gateway sends
a loopback request to Traefik for that same host, with an ephemeral in-memory bridge
secret and the session identity in dedicated headers. A backend alias exists only
for an active private preview; its base and every higher-priority application router
require a random service Basic Auth capability (only its bcrypt verifier enters the proxy file), then forward-auth checking those headers/session. Traefik then strips them before
proxying. Forwarded identity headers from clients are ignored. Original mesh hosts
keep their existing source-address rules and cannot serve as public bypasses.

The public catch-all does not match bridge-header requests; such requests can reach
only an explicitly protected alias, or fail. Guest namespace reservation prevents a
lower application route from claiming a revoked/unknown guest host. An arbitrary
bridge header is not authority: forward-auth fails closed without the random bridge
secret and a live host/generation-scoped session. No general target URL, agent API,
registry or arbitrary host forwarding is accepted.

The gateway preserves application routing, cookies and WebSocket upgrades. Original application Authorization is restored only after bridge authentication. Existing
app Basic Auth remains an additional requirement. Authenticated wake invokes only
the selected preview's existing admitted wake path. Long responses/WebSockets use
streaming bounded buffers and poll authorization every second with a 750 ms inspection timeout;
revocation, PR close, generation change or expiry normally closes active access within two seconds (qualification allows three). A monotonic hard session deadline bounds streams even if wall time moves.
Global request/redemption token buckets and a 32-connection ceiling bound work;
request bodies cap at 16 MiB and response/socket transfer at 64 MiB. These first-release
limits are explicit, not a general unlimited reverse proxy.

Responses force no-store/private-cache behavior, no-referrer and noindex; protected
backend headers cannot relax those controls. Downloaded content cannot be revoked.
The owner API waits for the HTTPS redirect and protected alias to become ready before returning a new invitation; a timed-out invitation is revoked. Local loopback and host processes remain trusted like the existing agent control plane.

A failed/stopped gateway, invalid clock, absent alias or inspection failure denies
access. No new always-on service/container or managed relay is added.

## Qualification

Store/HTTP tests: wrong host/secret/cookie, expiry, revoke, single-use GET/POST,
concurrent redemption, PR close/reopen, generation change, restart/key rotation,
clock jumps, owner roles, bounded queues/session counts, no secret in metadata.
Routing/browser contracts: external non-mesh client, forged bridge/forwarded headers,
alternate hostname/direct IP, reserved-domain claims, base/advanced/static/rewrite
routes, wake-before-auth denial, cache headers, scanner GET, fragment removal,
Secure/HttpOnly cookies and live WebSocket/response closure. Public Internet DNS/TLS
and physical hardware remain explicitly unmeasured without suitable infrastructure.

Traefik reference: explicit `trustForwardHeader: false`, an allowlist of auth request
headers, and empty custom request headers to strip bridge material, per the official
v3.7 ForwardAuth and Headers documentation. Auth ignores all X-Forwarded identity.
