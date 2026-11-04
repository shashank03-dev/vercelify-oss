# Named Cloudflare tunnel and personal-domain qualification

The user authorized testing with shashankdev03.me. Cloudflare's cf CLI was found
at ~/.bun/bin/cf, outside the session PATH; its existing OAuth profile authenticated
successfully. No new login or credentials were requested. The active zone was verified.

An isolated synthetic app (128 MiB, no production secrets) on the physical x86 host
was exposed at vfy-qual-20261104.shashankdev03.me through a newly owned remotely
managed named tunnel. Only that exact new CNAME was created; a trailing ingress
rule returned 404 for unmatched hosts. The original site/records were not repointed.
The connector used HTTP/2 and a private token file; no token appears in this evidence.

## Results

- Real public DNS and trusted HTTPS: 200, certificate validation passed.
- Vercel cloud function outside LAN/Tailscale: page, assets, streaming HTTP and WSS
  echo passed. First streaming chunk arrived in 1032 ms; WebSocket echo in 1467 ms.
- Chromium, Firefox and WebKit: trusted HTTPS, expected content, streams and WebSocket
  echo all passed.
- Independent Internet.nl client: IPv6 address, IPv6 reachability, and identical
  IPv4/IPv6 content passed. The Vercel function itself has no IPv6 egress and returned
  EADDRNOTAVAIL; it is not the source of IPv6 qualification.
- The old Quick Tunnel streaming failure is not erased. This named-tunnel path
  passes with the synthetic fixture; no application streaming-code fix was needed.

This is a public synthetic application path, not a complete qualification of public
per-recipient invitation lifecycles, scanner safety, expiry/revocation, generation
replacement, wake or private-host bypass controls. Prior isolated guest/private
runtime evidence remains distinct. Direct Internet ingress to the physical host,
router recovery and active physical power-policy transitions were not exercised.

## Cloud backup provider

cf r2 buckets list returned HTTP 403 / error 10042, explicitly requiring R2 to be
enabled in the Cloudflare Dashboard. No R2 bucket or cloud backup was created.
Existing account/domain OAuth authorization does not activate an R2 subscription.
The independent encrypted PG17 recovery evidence remains valid separately; R2 is
**not qualified**. No paid subscription or payment method was added.

## Cleanup

The named tunnel connector was stopped, its local token removed, the owned app and
project deleted, and the test DNS record and tunnel removed. All other DNS records
were compared after removal and unchanged. The external probe project was paused
again and returned HTTP 503. The original personal site still returned HTTP 200
with certificate validation. A reconciler cleanup check verifies owned fixture
containers are gone; protected originals are preserved. Test URLs no longer serve.

Evidence: cloud-result.json, public-browsers.json, ipv6-validator.json,
backup-provider.json, fixture.json and cleanup.json. No master key, provider secret,
invitation token or Vercel share-link credential is included.

References: https://developers.cloudflare.com/cf/get-started/ and
https://developers.cloudflare.com/r2/get-started/.
