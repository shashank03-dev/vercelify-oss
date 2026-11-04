# M8 software qualification and open physical gate

4 November 2026 (Asia/Kolkata). The software policy and evidence-bounded launch
material are implemented locally. **M8's physical laptop outage/recovery acceptance
gate remains open**: a read-only Tailscale check still reports `100.123.113.77` offline.
No deliberate outage, battery depletion, lid test or firmware power-on result is
invented or inferred from the VM. The saved pre-M3 laptop build settings still need
restoring when it reconnects. Nothing was pushed or sent to another person.

## Implemented behavior

Opt-in stages reuse the independent shell guard, not a new daemon. Configurable
50/40/30/20 percent thresholds hold builds, pause owned build work, sleep opted-in
previews, then opted-in production apps; the existing 15-percent clean shutdown
remains independent of the agent. Two-sample debounce, charge hysteresis, stable AC
and rollback of the recovery clock are checked. No/missing battery readings never
authorize recovery. Disabling policy holds existing ownership until safe recovery
without escalating new sleep tiers.

The guard filters platform ownership and exact installed core/builder names. It
never starts a stopped build or unpauses a builder already paused before its own
policy action. Boot restore starts core only; app/database desired state belongs to
the agent. The installer now ships the companion battery-policy library.

The agent persists power-sleep reason/generation before stopping. Restart completes
owned pending stops; recovery uses admitted wake and honors manual stops, PR close
and generation changes. A durable paused build remaining Building cannot prevent
conservation. Existing idle-sleeping workloads stay asleep. Visitors cannot wake a
power-held app, and disabling Sleep while held returns an explicit conflict. Worker
placement/build gate and cockpit status recognize battery/recovery holds. Doctor
reports firmware AC recovery as unverified and stops promising write survival after
abrupt power loss.

## Evidence

`checks.json` records **798 Rust passes**, fourteen Docker-gated ignores, **243 Linux
bats cases** (238 nonroot plus five root-only, all six installer cases rerun root),
**150 UI unit cases** and **33 browser cases**. Nine focused battery cases were
rerun after preserving entered-tier charge headroom when policy is turned off. Raw logs record all-feature Rust,
clippy/fmt, ShellCheck, Linux bats including root installer cases, types/build/UI
unit and browser checks. Binaries stay below 8 MiB; see `binaries.json`.

`real-power.txt` exercises a real private preview and opted-in noncritical app through
the actual agent/Traefik/engine in the owned nested daemon. Its first tier includes a
synthetic paused-build Building journal row, without a second runtime or deployment
job. It proves the preview tier excludes production, an authorized guest cannot wake
held work, an actual agent restart preserves generation-scoped ownership, stable
recovery restores matching preview state while manual production stop wins, and
PR close is never reversed. The independent shell guard pauses/resumes a real owned
Docker builder while continuous public-sentinel probes run. Simulated battery/AC
readings qualify software behavior only. The recovery-delay duration is shortened
to one second only for that live contract; Linux tests use a fake clock to verify the
full sixty-second default and bounce/clock-rollback cases.

`engine.txt` requalifies real deployment/rollout/rollback/recovery and admitted wake
storm (96.32 seconds). The M7b privacy, PostgreSQL and restore contracts remain
recorded in their own milestone; no unchanged source is relabelled as a new run.
`real-power-earlier.txt` records the earlier 679-probe zero-failure flow. A subsequent
fixture setup hit exhausted bridge pools after repeated contracts, before testing
candidate behavior (`lab-pool-exhaustion.txt`). Only unattached owned contract
networks were removed; attached and unrelated networks were preserved. The final
full-flow rerun is recorded separately with 736 probes and zero failures. A final
settings/recovery race fix was then checked by the focused persisted-state regression:
Sleep cannot be disabled after AC reports okay but before the automatic wake finishes.
The release binaries and installed idle sample include that fix. The live full-flow
result covers the unchanged staged-stop/wake path; it is not claimed as a rerun of the
new settings-conflict branch.

`lab-install.txt` records the installed schema13 candidate and rollback backup. The
power guard scripts were staged without enabling a service on this no-battery lab.
Matched original six-app memory samples follow two-minute settle with the disposable
daemon and BuildKit stopped. Peak battery/restore/build memory is a separate concern;
the idle sample does not establish feature overhead or energy savings. The final
median is **638.5 MiB** system used (609–653), **8.72 MiB** agent RSS and **3.11 MiB**
mesh RSS. All six original routes remain HTTP200. The earlier candidate sample
(639 MiB system / 9.66 MiB agent) is retained with its binary fingerprint, rather
than discarded or mislabelled as a final-candidate run.

## Remaining external qualification

The [physical matrix](../../POWER-RESILIENCE.md#physical-qualification) is entirely
unmeasured for the offline laptop: full shutdown/depletion plus AC return, lid,
unstable power, aged/missing battery, thermal load, reboot, router/ISP recovery and
independent off-device restore. Public guest Internet DNS/TLS/tunnel/IPv6 remain
separately unqualified. These are release restrictions, not passed cells.

Production-data backup-copy previews remain disabled. Quarantine/restore proof does
not approve production-data scrubbing, extension compatibility or hostile-code use.
[Release qualification](../../RELEASE-QUALIFICATION.md) and the
[pilot walkthrough](../../examples/OWNED-HARDWARE-PILOT.md) tie launch wording to
observed configurations and avoid availability, electricity-saving or adoption claims.
