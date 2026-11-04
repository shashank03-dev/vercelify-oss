# M8 owned-work power resilience and qualification

The user requests direct execution of the existing roadmap, no sub-agents and no
push. These are implementation choices within that scope. Physical outage proof
cannot be replaced by virtual sensors. Cost metering remains outside this gate.

Extend the existing independent host guard rather than introduce a second daemon.
An opt-in battery policy stages: hold new builds at 50%, pause owned build work at
40%, sleep explicitly sleep-enabled previews at 30%, then explicitly sleep-enabled
noncritical apps at 20%; existing clean shutdown at 15% remains the final safety net.
Thresholds are configurable and ordered. Require two consistent readings, 5-point
hysteresis and 60 seconds stable AC before recovery. A guest with no battery never
enters battery stages. Invalid or absent samples never authorize recovery of paused
work. Clock rollback restarts the stable-AC delay.

Only guard-owned pauses may be resumed. Never act on unrelated Docker containers;
recognize managed labels and exact installed core/builder names. Persist why sleep
and pauses happened. Never wake a manually stopped app, closed PR, replaced generation
or previously idle-sleeping workload. Explicit sleep enablement is the noncritical
selection; explain this opt-in consequence in documentation. Active power sleep
refuses wake-on-visit. Recovery uses existing admitted engine wake, not raw container
start, and preserves durable deployment state and backup-copy gating.

Keep existing thermal safeguards independent. Guard state feeds existing BuildGate
and admission; no additional scheduling queue or duplicate build retries. Independent
clean shutdown must still work if the agent is broken. Legacy restore manifests must
not revive manually stopped platform apps; their declared state belongs to the agent.

Doctor and launch material describe vendor-dependent AC power-on as manual/unknown,
record lid/sleep, AC bounce, missing/aged battery, thermal load, shutdown/depletion,
boot, network/router recovery and backup recovery evidence per physical configuration.
No cloud SLA, universal laptop, power saving or off-device recovery claim is inferred
from the ARM Linux lab. Production-data backup-copy previews stay disabled until
separate quarantine, scrub, compatibility and resource gates pass.

Qualification: Linux simulated sysfs/Docker transitions, restart and clock cases;
store/engine wake fencing and sleep selection tests; existing shell/Rust/UI gates;
real isolated Docker pause/resume while sentinel probes run; matched six-app memory.
Physical laptop matrix stays open when no suitable device is reachable. Document
reproduction, exact tested configuration and clear remaining launch restrictions.
