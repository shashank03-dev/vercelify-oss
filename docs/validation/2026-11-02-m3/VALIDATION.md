# M3 qualification — explicit computer and CI builds

M3 implementation and lab qualification complete. Physical x86 qualification
and the offline laptop's settings restoration remain explicitly pending.

## Implementation and review

The developer-selected Docker runtime builds an exact Git commit using the shared
`vercelify-pack` detector and generated recipes. The CLI strips Git metadata from
its private checkout and keeps secrets in BuildKit mounts. Generated recipes add
an opaque per-deployment ARG after each FROM: changed build secrets cannot reuse
old RUN output, while package cache mounts persist. Repository Dockerfiles instead
use conservative `--no-cache`; their cache mounts reset. The real Docker regression
first failed with the old unconditional no-cache behavior, then passed with the
nonce (`local-cache-contract.txt`). No global Docker context or Git config changed. A retained builder with a different
endpoint is refused before job creation; a new builder gets the selected endpoint
explicitly. The CLI regression for this mismatch passes.

The agent persists owner/generation/deadlines, plan/hash/version, commit and
configuration/variable revision. An authenticated stream is bounded to 3 GiB and
sanitized to one job tag before Docker load. Runtime configuration, rootfs and
architecture are checked after load. Docker 29's image ID can identify an OCI
index; the protocol explicitly hashes the saved image configuration instead.
Revocation and lease/config/deployment validity are checked again before publish.
Interrupted jobs are fenced across restart. External jobs never fall back to a
serving-host build; `build_concurrency = 0` leaves ordinary builds queued.

Registry writes now bind only to the main server's loopback. Workers read through
an authenticated mesh proxy. Old workers need upgrading with the main server.

Self-review performed in the main chat; no sub-agents. Release asset generation is
implemented, but no release has been published or downloaded from a new release.

## Verified checks

- Rust workspace, all features: 714 passing tests, no failures (ignored destructive
  contracts are run separately). Fmt, clippy with warnings denied and ShellCheck pass.
- Linux bats: 231 cases (226 pass, five root-only skips), including installer,
  CLI/runtime failures and exact-checkout handling. Five privileged installer cases skip in the unprivileged suite; all six installer
  cases pass separately as root (`install-root-bats.txt`). A stale installer copy
  in the lab initially failed the new option test; syncing the candidate fixed
  the fixture (`bats-stale-fixture-failure.txt`).
- Cockpit: typecheck, 131 unit tests, Vite production build, 29 demo browser tests pass.
- `native-contract.txt`: real separate builder/target daemons, secret redaction,
  archive validation/load/serving, changed variables, duplicate/stale/superseded,
  wrong digest, cancel/expiry, interrupted upload and mid-upload token revocation.
- `builder-contract.txt`: three build/stop/restart cycles retain warm cache; active
  and paused builders are protected (M2 regression).
- `multinode-contract.txt`: 17 real worker checks pass, including authenticated
  registry reads, denied raw mesh access/device tokens/writes, routing, replicas,
  rollback, runtime operations and revocation.
- `next-cli-native.txt`: pinned Next.js fixture commit
  `90cfee0f74c25bb3688369a0587e435f8b8f114d`, real CLI ARM → ARM handoff,
  deployment `dpl_beaw2xvcs06amh5r` healthy; proxy response HTTP 200, 5609 bytes.
  Serving target had builds disabled and its forbidden Git/build wrappers were
  never called. This historical Next 13.5.6 fixture is for comparison only.
- `amd64-contract.txt`: the same real handoff/fencing failure contract passes with
  the final x86 agent under emulation and an actual amd64 runtime image.
- `next-helper-amd64.txt`, `next-amd64-handoff.txt`, `amd64-serving.txt`: ARM computer
  builds the pinned Next image for amd64; separate emulated x86 target verifies,
  imports and serves it, then rolls out another image under four clients paced
  at 250 ms. 948 requests in 60 seconds, zero failures, 42.99 ms p95. No target
  Git/build commands; actual loaded architecture is linux/amd64. This qualifies
  emulated lab operation, not unavailable physical x86 hardware.
- `small-vm-*`: one-GiB VM (956.1 MiB visible), swap disabled, one Next app, no DB,
  container memory uncapped, builds disabled. Initial image import healthy in
  15.92 seconds, second rollout healthy in 10.59 seconds under four paced clients.
  940 requests/60 seconds, zero failures; 9.22 ms p95. After a real VM stop/start,
  Next response validated and 948 requests/60 seconds, zero failures; 10.86 ms p95.
  1,168 samples at 200 ms over 239.9 seconds: minimum available RAM 326.4 MiB,
  OOM-kill counter delta zero, swap disabled throughout. `small-vm-summary.json`
  captures the boundary; this one synthetic fixture remains experimental.
- Final static agents: ARM 7,197,128 bytes; x86 8,364,632 bytes, both below
  8,388,608 bytes. The x86 budget has only 23,976 bytes remaining.

## Environment and remaining qualification

The selected Mac runtime is Colima, ARM64 Docker 29.5.2 (~1.9 GiB guest). Its
missing Buildx plugin was installed through Homebrew and the standard CLI plugin
symlink. The pinned BuildKit image was seeded from the existing lab cache after
registry resets. Public Git fetches use HTTP/1.1 only within qualification command
environments. The unchanged fixture Yarn cache was seeded from the previous
benchmark; this is a warm-cache run, not a cold-install speed claim. Failed fetch
and cancelled build logs are retained and are not successes.

An earlier M3 candidate was installed on the physical x86 laptop with user approval. Existing GW production,
PostgreSQL, the new safe PR #1 preview and unrelated media container are preserved.
It went offline before final-binary installation and x86 handoff qualification.
The user explicitly chose to continue in the lab VM. Physical laptop memory and
final-binary qualification remain unmeasured. Qualification uses its own project/app
and a private local settings file; credentials
are never included in this evidence. Serving-host builds are temporarily disabled
for this test; restore its saved configuration afterward.

The Ubuntu QEMU 8.2.2 registration reproduced an internal SIGSEGV even with
`node -e 'console.log(process.arch)'`; this was independent of Vercelify and
persisted with jitless/CPU variants. The identical image and command worked with
the QEMU 7.1.0 interpreter extracted from the already pinned BuildKit image, which
also performed the successful amd64 compilation. The lab temporarily registered
that interpreter for the successful runtime test, then restored the original
registration. The initial failure is retained (`next-amd64-qemu82-failure.txt`).
This does not qualify arbitrary emulation versions or make an emulator recommendation.

## Memory and cleanup

`vm-steady.txt/json`: matched M2 six-app workload at four GiB, two-minute settle,
twelve samples, BuildKit and disposable daemon stopped. Used median 685.5 MiB
(674–705), agent RSS 8.38 MiB, mesh 2.90 MiB, BuildKit zero. M2 used median was
656.5 MiB (638–689); ranges overlap, so no fixed cost/saving is inferred.
Research §6 contains the component table. No M3 laptop memory result is claimed.

The one-GiB test's owned app/project were deleted. Four-GiB VM configuration,
original agent configuration, four-GiB swap and all six original apps were restored
(`vm-restored.txt`). Disposable daemon stopped and temporary emulator registration
removed. Only named fixture builders/caches/images on the selected Mac runtime
were used; unrelated containers and contexts were preserved.

The offline laptop still has `build_concurrency = 0` from qualification. Restore
`/var/lib/vercelify/qualification-m3-before/pre-crossbuild-agent.toml` to
`/etc/vercelify/agent.toml` and restart the agent when reachable; then remove only
the owned `m3-offhost-qualification` project/app and any owned temporary receiver.
Production, PostgreSQL, safe PR #1 preview and unrelated media container must stay.
The final x86 candidate was prepared but not installed after the laptop went
online=false. Do not report that pending restoration or physical gate as complete.

Generated TS is committed exactly as emitted by ts-rs. Its known trailing space
on `LocalBuildPlan.ts` is the sole `git diff --check` exception; it is not edited
by hand.
