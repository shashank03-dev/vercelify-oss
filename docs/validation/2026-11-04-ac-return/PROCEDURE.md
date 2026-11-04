# Attended AC-return observation — fifteen minutes, approval pending

Target: Dell Inspiron 5567, 100.123.113.77. No firmware changes. Current BIOS 1.1.7
has no exposed Linux firmware-attributes power-control interface. The battery
reports only 130000/3684000 of design charge (~3.5%). Never unplug while running.

Prerequisite: operator beside laptop with charger and power button, lid open,
fresh private config/control-store/master-key and database rollback files saved,
original five protected containers inventoried, external monitoring running.
The new production PG15 dump is complete but not restore-qualified; independent
PG17 recovery evidence is distinct. Do not count it as a production restore test.

1. Recheck AC and original health; start external route/reachability observations.
2. Execute one normal shutdown. Operator confirms screen dark and fan stopped.
3. Only after confirmed fully off, operator disconnects the charger for twenty
   seconds and reconnects it. No battery removal, case opening or depletion.
4. Operator reports whether the laptop starts without pressing its power button.
   Observe for ninety seconds. If not, record automatic AC-return start as absent
   under the current configuration; ask operator to press the button briefly once.
5. Allow five minutes for boot. Verify new boot ID, original services/containers,
   production and PR1 routes, PostgreSQL readiness, control-store integrity and
   deployment/job state. Preserve shutdown/boot logs. Close the window.

Abort on failure to shut down normally, abnormal charger/battery behavior, inability
to reconnect power or recover manually. Do not force power off, change BIOS or
restore over live state automatically. Production, PostgreSQL, safe PR1 preview
and unrelated media will be unavailable during shutdown/boot. This observes AC
return while already powered off, not resilience to AC loss during live workloads.

Depletion, missing/healthy-battery permutations, AC bounce while serving and
critical thermal-policy thresholds remain unqualified. They need suitable hardware
and separately scoped procedures. Do not force critical temperatures or silently
label lowered thresholds/simulated sensor readings physical evidence.
