# Final local and physical-host qualification

Captured text logs have trailing whitespace/extra EOF blank lines normalized;
`log-whitespace-normalization.json` retains original and normalized fingerprints.

4 November 2026. This is evidence for the tested configurations, not a claim that
all physical/public/cloud release gates are closed. No push or deliberate physical
outage was performed. Physical-presence tests were deferred at the user's request.

## Findings fixed during qualification

1. A missing BuildKit container with retained buildx registration failed bounded
   builds. A unit regression first failed, then passed. Real Docker now confirms
   recreation with memory/swap/CPU limits, retained cache volume and a warm cache
   hit. Other inspection/bootstrap errors fail closed. The engine contract also
   passed after reproducing the failure in the first isolated run.
2. A physical x86 handoff over the userspace mesh returned HTTP 504 after exactly
   120 seconds. A paused-clock regression reproduced that production router
   timeout. Image uploads now use the hard build-lease ceiling outside the normal
   API timeout, retaining authentication, 30-second stall detection, 3-GiB bounds,
   resource reservation, lease/generation fencing and Docker-load limits.

See `builder-recovery-red.txt`, `builder-recovery-green.txt`,
`builder-idle.txt`, `engine.txt`, `upload-120s-physical.txt`,
`upload-timeout-red.txt` and `upload-timeout-green.txt`. Earlier failure records
are retained as investigation evidence; they are not final pass results. The first
installation harness incorrectly required HTTP 200 from a private preview on
loopback and rolled back. Baseline comparison was corrected to preserve production
200 and private-loopback 403; subsequent upgrade probes had no status changes.

## Isolation and protected resources

Destructive Rust contracts ran serially inside the exclusively owned
`vfy-final-qualification-docker` nested daemon in `vfybench`; the separate recovery
contract used `vfy-final-recovery-docker`. Neither used the serving Docker daemon
or a serving-host bypass. Real mesh/NAT tests used `vfy-final-mesh` and private
Linux network namespaces. The whole-memory matrix used a newly created,
exclusively owned Lima VM, `vfy-final-resources`, left stopped after testing.

The physical x86 laptop `100.123.113.77` was reached with Tailscale SSH. Its saved
pre-M3 build configuration was restored, retaining the subsequently corrected
mesh public URL. Both configurations were saved under
`/var/lib/vercelify/qualification-final-20261104T022650Z`. Only the identified M3
app `m3-offhost-next` and its qualification project were deleted. The final
qualification app/source database were likewise uniquely named and cleaned up.
Production, PostgreSQL 15, safe PR #1 and unrelated media retained their container
IDs and restart counts. Unrelated stopped Supabase/Redis/PostgreSQL containers
were inventoried and preserved. The serving PostgreSQL 15 instance is not claimed
as a supported independent PostgreSQL 17 restore target.

Agent/control-store rollback snapshots were taken before upgrades. Matching mesh,
backup helper and guard/helper files were installed, preserving configuration,
mesh identity and previously active service state. Battery conservation was not
enabled. No router, firmware, battery, AC or lid manipulation was performed.

## Rust and runtime qualification

The final ordinary suite passed **804 tests, zero failures, fourteen deliberate
ignores**. Clippy with warnings denied, formatting and generated-type consistency
checks passed. [Ignored-test coverage](IGNORED-TESTS.md) provides the per-test table.
`ignored-inventory.json` maps all fourteen ignored Rust tests to both historical
runtime evidence and fresh successful executions. `ignored-results.json` retains
binary fingerprints, durations, results and the initially failing engine attempt.
The final result for each test is the last result for its label. Six Docker tests
and the real macOS display test had no prior successful runtime record; these gaps
are now covered. The ordinary Rust suite still deliberately ignores these tests.

Real mesh: `mesh-runtime.txt` records 28 passes, including pairing, names,
WireGuard transport, firewall, userspace SOCKS/gateway/forwards, key rotation,
revocation and restart from a retained map with the coordinator unavailable.
`mesh-nat-runtime.txt` records 8 NAT/relay/revocation passes. These are Linux
namespace evidence, not physical router/ISP recovery.

## Restore safeguards and independent recovery

`restore-safeguards.txt` exercises real Docker writers/volumes, restic and rsync:
inspection failure and a failed stop abort before live replacement; exact-tree
replacement removes newer-only files, retains the prior tree, restores tokens,
restarts the owned writer and preserves an unrelated volume/writer. The helper
contract uses a process-control shim for systemctl in a non-systemd container;
it does not establish real systemd restart behavior.

`independent-backup-source.txt` records an encrypted custom-format PostgreSQL 17
fixture produced on the physical amd64 laptop. Its source container and volume
were removed before recovery. Only the encrypted repository and independently
retained recovery artifact crossed to the separate ARM host. The target product
agent had a fresh control store/master key and no original database row. Row count
128, migration `db1072b` and a selected value were asserted. Full-data integrity,
wrong/missing keys, ciphertext corruption, cancellation and cleanup passed; see
`independent-recovery.txt`. A cold image pull exceeded the initial harness wait;
the operation subsequently succeeded, and the complete cases were rerun with the
image available. This is separate-host recovery, not a physical disk-loss test.
No configured cloud repository endpoint was found; no cloud provider is qualified.
Other engines/major versions are not qualified by this restore evidence.

## Browsers, guest access and public boundaries

`mac-browser.json` records real Chromium HTTP 200 for production and private PR #1
through the Mac userspace `.localhost:7481` gateway. The raw `.vfy` URL returns
`ERR_NAME_NOT_RESOLVED` on this userspace-only Mac. The historical F2 app-mode
browser gap is covered through the gateway; full native Mac system mode is not
configured and remains a separate gate.

`guest-qualify.txt` and `guest-lifecycle.txt` rerun the real lab TLS/Traefik/agent
flow for credential/header/cookie stripping, app authorization preservation,
cache/referrer/worker policies, WebSockets, streaming HTTP, revocation, induced
server-side expiry, agent restart, control-key replacement, generation/privacy
changes and PR close. All 76 continuous production-sentinel probes passed.
This lab uses local test DNS/certificates; it is not Internet certificate/DNS
qualification. `guest-browser.txt` adds fresh real Chromium scanner/fragment/cookie checks through
an owned localhost SSH forward, preserving an existing unrelated port forward.
Historical browser evidence also remains in M7b. No active cloudflared service, public HTTPS configuration or cloud backup
provider was found on the laptop. It uses a private sslip.io wildcard and local
backup repository. Genuine external non-mesh HTTPS/tunnel/IPv6/browser access
therefore remains unqualified; no endpoint or client was fabricated.

## Cold start and resource bounds

`whole-vm-matrix.json` records actual separate whole-VM RAM profiles with two CPUs,
no swap, ARM64 Ubuntu and Docker 29.1.3. Docker/containerd image stores were
recreated between rows in this exclusively owned VM. The declared fixture is a
small static HTTP Dockerfile built through the shared `vercelify-pack` CLI;
results do not establish arbitrary Next.js, database or multi-app capacity.
Each row records architecture, image/config digest, initial cache, load average,
cold download/import/build, warm cache hit, sampled system memory, latency and OOM
counter. Guest page cache was dropped before import; host disk cache is uncontrolled.
Each profile served 40 requests from two clients with zero errors and no OOM kills.

| RAM | Download s | Import s | Cold/warm build s | p95 ms | Sampled peak used MiB |
| --- | --- | --- | --- | --- | --- |
| 1 GiB | 8.980 | 0.089 | 0.254 / 0.147 | 0.436 | 393.44 |
| 2 GiB | 10.039 | 0.089 | 0.363 / 0.183 | 0.486 | 429.94 |
| 4 GiB | 7.558 | 0.085 | 0.281 / 0.161 | 0.649 | 473.13 |

`resource-matrix.json` separately records 1/2/4-GiB nested cgroup limits on a shared
four-GiB VM. Those rows are supplementary and not relabelled whole-machine results.
Physical x86 memory uses the same five serving containers and restored configuration,
12 samples six seconds apart after settling. The initial pre-upgrade median was
2,081 MiB system used and 10.50 MiB agent RSS; the initial db1072b median was
2,092 MiB and 11.04 MiB. System ranges overlap, so no fixed memory saving is claimed.
Final-binary samples are recorded separately rather than replacing earlier data.
The final median is **2087.5 MiB system used**
(range 2058–2149), **12.38 MiB agent RSS**
and **3.07 MiB mesh RSS**. These are idle samples of
this workload, not peak build/restore usage or a memory-savings guarantee.
`laptop-final-status.json` verifies final fingerprints, all three active services,
restored build settings, only the original `gw` app, empty upload staging and the
unchanged five protected container identities/restart counts.

`laptop-install-final.txt` records the installed native x86 binary and 66 route
probes with no status changes. `native-handoff.json` records a final physical amd64
handoff deliberately limited to 12000 bytes/s: 2,228,736 archive bytes, 197.55
seconds from build/import to healthy serving and a known body assertion. It exceeds
the old timeout, renews the lease during transfer, rejects an arm64 request with
HTTP 400 and cleans its owned app/upload staging. The exact installed agent/mesh
and cross-built ARM fingerprints and size-budget checks are in `final-binaries.json`.
The five protected container identities/restart counts remain the upgrade baseline.

## Open physical/external gates

The battery inventory reports full charge 130000 versus design 3684000 (about
3.5%); this is inventory evidence only, not battery endurance or power recovery.
An attended outage window with external monitoring and a verified recovery path
is required. See `OUTAGE-PROCEDURE.md`: no window is authorized by silence.
Lid, AC bounce, battery removal/depletion, thermal load/cooldown, firmware AC return,
shutdown/reboot and router/DNS/mesh/tunnel/ISP physical recovery remain unmeasured.
Physical total-machine-loss recovery, real remote cloud providers, public domain/TLS,
a non-mesh external client, Internet IPv6 and Mac native system mode remain open.
