# Public-network and native Mac qualification follow-up

4 November 2026. Follow-up to the local/physical qualification at `e8fb366`.
These are actual runtime checks. Unavailable or failing cases remain identified.
No physical outage test, domain purchase, cloud-storage purchase or Git push occurred.

## Results

| Area | Observed result | Evidence |
| --- | --- | --- |
| Internet HTTPS/tunnel | Passed through a real Cloudflare Quick Tunnel running on the physical laptop, to an isolated synthetic Vercelify app. Public DNS and a trusted certificate were used; no DNS override or certificate bypass. | `public-fixture.json`, `cloud-result-after.json` |
| Independent external client | A newly created, protected Vercel function outside the LAN and Tailscale reached the fixture over public IPv4 HTTPS and echoed a WebSocket message. | `cloud-result-after.json`, `cloud-probe.js` |
| Browser compatibility | Chromium, Firefox and WebKit each returned HTTP 200 with the expected content and a successful secure WebSocket echo. | `public-browsers.json` |
| External IPv6 ingress | Internet.nl independently confirmed reachable IPv6 web servers and matching IPv4/IPv6 content through Cloudflare. Neither the Mac nor the Vercel probe had outbound native IPv6, so they are not relabelled IPv6 clients. | `ipv6-validator.json`, [independent report](https://en.internet.nl/site/requested-alliance-vernon-gene.trycloudflare.com/4389868/) |
| Private routes from outside | The external Vercel client was denied at the private fixture's page, asset, WebSocket and API paths. Forged forwarding headers were denied; a private Host-header/domain-fronting attempt was rejected at the edge. | `cloud-private-result.json` |
| Native Mac system mode | Passed actual utun creation, system DNS and normal Chromium access to the raw private `.vfy` URL and production URL. Private mesh IPv6 transport to the production HTTP route also passed. Original resolver restored and temporary interface removed. | `native-result.json`, `native-browser.json`, `native-ipv6-summary.json` |
| Cloud repository provider | Unavailable. The saved AWS credentials were rejected (`InvalidClientTokenId`); the operator explicitly confirmed no cloud storage account is available. No simulated provider is called a cloud deployment. | Operator response; no existing credentials changed |
| Small continuous HTTP stream | Failed through the temporary HTTP/2 Quick Tunnel: source produced chunks immediately, but the external probe timed out and all three browser engines aborted after receiving no stream data within their deadline. Explicit header flush and `no-store, no-transform` did not make it pass. | `cloud-result-before.json`, `cloud-result-after.json`, `public-browsers.json` |

The public tunnel used a Cloudflare-assigned temporary hostname, not a permanent
operator-owned domain. Stable wildcard DNS/custom-domain configuration, the named
tunnel service and real public per-recipient invitation flows still need domain/
zone access. The successful IPv6 check covers public Cloudflare ingress and HTTP
content comparison; it does not prove native public IPv6 on this IPv4-only laptop,
or IPv6 WebSockets, nor direct private-preview IPv6 ingress.

The stream failure is retained as a qualification restriction, not converted to a
pass because ordinary HTTP and WebSockets work. [Cloudflare documents](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/)
that Quick Tunnels do not support SSE; these fixture checks used streaming text, and
only the observed temporary path is assessed here. A named production tunnel is
not qualified by these temporary-tunnel results.

## Fixes found by the native test

1. The Mac system resolver pointed at the daemon's own point-to-point mesh IP.
   TCP to the server worked but DNS to that self-address timed out. Native macOS
   now binds its signed-map DNS zone on an ephemeral loopback UDP port, writes the
   actual port to `/etc/resolver/vfy`, and makes new public resolver directories/
   files system-readable (0755/0644), independently of a private umask. Private
   mesh keys/state retain their restrictive modes. Linux still uses its mesh IP
   and port 53. `native-dns-unit-red.txt` and `native-dns-unit-green.txt` record the
   regression, with actual native before/after results retained separately.
2. Wildcard app hostnames advertised a node IPv6 address even though this Docker
   private-preview ingress does not preserve the IPv6 client identity. Browsers
   chose that address and received 403, while IPv4 reached the same preview.
   Application wildcard names now advertise the supported IPv4 ingress only;
   node hostnames retain A/AAAA records. This is an explicit direct-private-app
   IPv6 limitation, not a claim that IPv6 private previews were repaired.
   `app-dns-unit-red.txt`, `mesh-tests.txt` and the real native browser rerun record
   failure and success. The engineering design and mesh guide describe the scope.

The final ordinary Rust suite passed **806 tests, zero failures, fourteen
intentional ignores**. The fourteen ignored runtime cases were qualified in the
preceding report. Clippy with all targets/features and warnings denied passed,
as did formatting and generated-type consistency. All **28 Linux mesh runtime
checks** passed after the DNS changes, covering actual namespaces, WireGuard,
DNS, pairing, firewall, key rotation, revocation and saved-map recovery.

## Native test isolation and cleanup

A separate short-lived mesh identity was used; the active app-mode identity and
LaunchAgent were not replaced. Administrator authorization was used only for the
requested native test. The helper and its dependencies were staged in a private
temporary directory so the authorization helper did not need Desktop-folder access;
no macOS privacy or authentication policy was weakened. The supplied administrator
credential was not written to a script, configuration file or evidence log.

The helper backed up/restored the existing resolver, removed its temporary utun,
and retained structured results. The test identity was revoked and temporary key
copies removed. A failed first cleanup revealed that the authorization runner's
background child ignored TERM; the helper now bounds shutdown and kills only its
own temporary process if necessary. The failed run's interface/resolver cleanup
was completed before retrying.

The final Linux mesh binary was atomically installed on the physical laptop,
with a private rollback copy, unchanged node identity/key and unchanged production,
PostgreSQL, PR #1 and media container identities/restart counts. See
`linux-mesh-install.json`. The Mac test used the final source-built native binary;
it restored the user's usual app-mode operation afterward.

## External test isolation and cleanup

The physical public fixture used its own project/environment and no production
variables. Only its synthetic content was exposed. The negative private fixture
ran in an exclusively owned nested daemon, not the serving Docker daemon. Neither
the existing production app, PostgreSQL data, PR #1 nor unrelated media was used
as an Internet exposure target.

The physical public app/project was removed and all temporary tunnels stopped;
the test mesh identity was revoked. The negative private fixture's agent/apps
and isolated test daemons were stopped; their synthetic state remains inside the
stopped, exclusively owned daemon for review. The
external Vercel probe project was paused and verified to return HTTP 503 with
`DEPLOYMENT_PAUSED`; it remains as a protected, paused test artifact because the
connected tool offers no project-deletion operation. No existing Vercel project
or domain was modified. The source scripts contain no provider/account secrets.

Temporary tunnel URLs in these artifacts no longer serve the fixtures after cleanup.
The independent IPv6 report is a recorded result from when the fixture was live.
