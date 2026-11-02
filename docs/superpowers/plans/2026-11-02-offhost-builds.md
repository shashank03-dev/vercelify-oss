# Explicit computer and CI builds implementation plan

Goal: M3, explicit off-host builds with immutable, fenced image handoff.
Spec: competitive roadmap §9 and companion engineering review §4.
Execution: main chat, one milestone commit, report and continue. No sub-agents.

Architecture: a local `vercelify-pack` binary reuses the detector on an exact-commit
checkout. The CLI uses the caller-selected Docker runtime/context without switching
it, builds the requested platform, and uploads a Docker image archive to the
existing authenticated agent API. The server owns a persisted deployment/job,
lease generation, owner, deadline, plan/config/variable revision and image digest.
An archive is streamed to bounded temporary storage and validated/sanitized before
Docker load; only the job's unique tag may be loaded. Registry credentials are not
exposed. No target build fallback on failed or missing imported images.

Contract: POST apps/{app}/build-jobs creates a waiting deployment and lease;
POST build-jobs/{id}/renew renews the current generation; PUT build-jobs/{id}/image
streams the tar with generation and expected image digest; GET returns metadata
without variables; cancellation uses the existing deployment cancel endpoint.
CI can supply a locally pulled digest-pinned image through the same artifact path.
Every mutation checks token identity and current lease. Complete uploads alone can
be marked deployable, after digest/platform/plan/revision/current-deployment checks.
Old jobs, closed PRs and interrupted uploads never publish over a newer deployment.

Review focus: stale generations and duplicate upload, revocation, cancellation/new
commit/PR close during upload, wrong image/platform, archive tag injection and tar
links/path tricks, disk exhaustion, runtime absence/emulation, source exactness,
secrets in logs/cache and target-side build fallback.

- [x] Add job state/fencing/expiry tests; observe red; implement SQLite migration,
      wire types and job repository, queue integration and config revision hashing.
- [x] Add malicious/truncated/wrong-digest archive tests; observe red; implement
      bounded streaming validation and job-only Docker load/publication.
- [x] Add real HTTP and Docker handoff tests, CLI bats/runtime failure cases;
      implement authenticated handlers, shared pack helper, explicit CLI flow,
      runtime inspection and helper installation/release packaging.
- [x] Run all standard checks, arm64→arm64 and arm64→amd64 contracts, musl size
      gates; remeasure both hosts with §6 method and document hardware boundaries.
- [x] Update platform/design/CLI docs, self-review, stage exact files, commit M3,
      report result and continue M4. Push requires separate authorization.
