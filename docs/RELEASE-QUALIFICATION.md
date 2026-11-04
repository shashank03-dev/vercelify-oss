# Roadmap release qualification

4 November 2026. These results describe local candidate code, not a published release.
No pilot/adoption outcome or competitor market claim follows from passing tests.

The qualified development environment is an ARM Linux VM with two vCPUs, four GiB,
Ubuntu 24 and Docker 29.9, with six fixed original app routes. Individual earlier
1/2 GiB and cross-build fixtures have their own limits and evidence; they do not
qualify all laptops, apps, database sizes or language/framework combinations.

| Capability | Evidence and limits |
| --- | --- |
| Private previews and first-deploy isolation | [M1](validation/2026-11-02-m1/VALIDATION.md); trusted repositories, Docker 28+; no hostile multi-tenant claim |
| Idle builder lifecycle | [M2](validation/2026-11-02-m2/VALIDATION.md); stops an idle builder, preserves cache and active builds |
| Explicit computer/CI builds | [M3](validation/2026-11-02-m3/VALIDATION.md); declared image architecture and target resource gates apply |
| Admission and memory profiles | [M4](validation/2026-11-02-m4/VALIDATION.md); sample freshness and reservations; small-host support is workload-specific |
| Fresh PostgreSQL previews | [M5](validation/2026-11-03-m5/VALIDATION.md); synthetic migration/seed, no production-data copying |
| Durable deploy guard | [M6](validation/2026-11-03-m6/VALIDATION.md); compatible verified rollback, durable cleanup and restart recovery |
| Restore drills | [M7a](validation/2026-11-03-m7a/VALIDATION.md) and [separate-host qualification](validation/2026-11-04-final-qualification/VALIDATION.md); encrypted PostgreSQL 17 fixture created on physical x86 and recovered on a separate ARM host after source deletion, using independently retained credentials and a fresh control store/key. Physical total-machine-loss and cloud-provider gates remain separate. |
| Guest invitations | [M7b](validation/2026-11-04-m7b/VALIDATION.md); main-node private previews, real TLS/Chromium lab; public Internet DNS/TLS/tunnel/IPv6 not qualified |
| Battery conservation | [M8 software evidence](validation/2026-11-04-m8/VALIDATION.md) and [configuration](POWER-RESILIENCE.md); opt-in staged policy; physical outage/recovery gate remains open |

Production-data backup-copy previews remain disabled. Restore quarantine alone does
not approve copying production data into executable preview code. Scrubbing,
credential/extension compatibility, resource bounds and independent security review
remain separate gates.

Start with an explicit image or supported repository configuration and a declared
resource limit. Keep production secrets out of previews, run synthetic migrations
and seed data, and verify a backup before relying on it. Use the existing
[platform guide](PLATFORM.md), [computer/CI build path](PLATFORM.md#build-on-your-computer-or-in-ci)
and [power matrix](POWER-RESILIENCE.md#physical-qualification). A deployed app needs
working host power, storage and networking; DNS and public HTTPS require operator
configuration. Record the actual outage/recovery results before advertising a laptop
model as qualified.

The [final qualification report](validation/2026-11-04-final-qualification/VALIDATION.md)
records physical x86 installation and image handoff, matched memory samples, real
Mac app-mode browser access, ignored-test coverage and remaining external gates.
Cold/warm cgroup-budget fixtures are not whole-machine memory qualifications.

No broad “five apps plus PostgreSQL on one GiB”, guaranteed lid-close/AC restart,
universal zero-downtime update, automatic failover or measured electricity-saving
claim is established by this evidence. Recruit and observe the small pilot described
in the competitive validation review before making adoption claims.

A concrete operator walkthrough is in [the owned-hardware pilot](examples/OWNED-HARDWARE-PILOT.md).
