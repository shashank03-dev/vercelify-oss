# AC loss while serving, battery depletion and sustained thermal load — 2026-11-05

Laptop: Dell Inspiron 5567, 4 threads, 7.8 GB RAM. The battery reported "100% Full"
while holding only 130 mAh (charge_now 130000 µAh, about 3.5% of design). Fresh
rollback copies (config, master key, control store via the SQLite backup API,
pg_dumpall of vfy-db-gwdb) were saved on the laptop before either test.
before.json records the boot ID, container set and control-store counts.

## Sustained thermal load

thermal.py ran one busy loop per CPU thread on the host, sampling every 5 s, with a
watchdog set to stop at any sensor ≥ 100 °C (the hardware's B0D4 critical trip)
or after 25 minutes. I stopped it after 7 minutes because the temperature had
plateaued (thermal.jsonl).

- The peak was **86 °C**. It settled at 83–84 °C with all cores still at about
  3.0 GHz (3.1 GHz at the start), so there was no thermal throttling.
- power-guard stayed `ok` throughout. Its pause (92 °C), freeze (95 °C) and
  shutdown (98 °C, three readings) thresholds were never reached.
- Production and PR1 answered 293 of 300 external probes (thermal-external.jsonl).
  The 7 failures were isolated 3 s timeouts (never two in a row). They happened
  both during the load and after it stopped (18:56:52, 18:58:25, 18:59:05,
  19:05:00), so they are not attributed to heat. Mesh probe flakiness is the
  likelier cause; see the open finding in ../2026-11-04-ac-return/.

**Result:** with the vents unobstructed, this laptop cannot reach power-guard's
thermal thresholds under full CPU load. The pause, freeze and thermal shutdown
paths remain qualified only by the simulated-sensor bats tests. Reaching them
physically would mean blocking airflow, which was deliberately not done.

## AC loss while serving → battery depletion

With all apps serving, the operator unplugged the charger at 18:58:04 UTC and left
it out. battery.jsonl (fsync'd every 2 s) and shutdown-journal.txt record the run:

| UTC | Event |
| --- | --- |
| 18:58:06 | AC 0, discharging, 96% |
| 18:58:16 | power-guard `ok → warning` (running on battery) |
| 18:58–19:08 | Steady drain, about 1% per 10 s, down to 38% |
| 19:08:2x | **Capacity fell from 38% to 5% between two readings** (worn-cell voltage cliff) |
| 19:08:48 | power-guard `warning → shutting_down` at 3%: stops 3 app/data containers, then 1 core container |
| 19:08:48 | PostgreSQL logs "database system is shut down" |
| 19:08:50 | `systemctl poweroff`; battery reads 2% / 3 mAh |
| 19:08:55 | systemd reaches System Power Off; filesystems synced |

**The graceful shutdown worked, with a very thin margin.** On battery the laptop ran
for 10 min 44 s. power-guard shut down at 3%, not 15%, because the reported
capacity collapsed between its 20 s samples and the guard waits for two
consecutive low readings. One more interval and this would have been a hard power
cut.

### Recovery

The operator reconnected AC and pressed the power button (the AC-return test
showed it does not start by itself). Boot at about 19:12:20 UTC (recovery.json,
boot-journal.txt):

- New boot ID. agent, mesh, power-guard and docker all active.
- PostgreSQL: "was shut down at 19:08:48" with no crash recovery, and accepting
  connections by 19:12:43.
- Control store integrity_check `ok`. Apps, deployments and databases unchanged
  (1 / 3 / 1).
- The proxy (protected) was restarted by power-guard. The database and media
  containers were running by 19:12:43, before the apps.
- The two app containers were stopped by `docker stop`, so their restart policy no
  longer applied. As designed, the agent reconciler started them at 19:13:21. Its
  first two passes (19:12:44 and 19:12:50) skipped work with "capacity: no
  capacity sample is available", which delayed that by about 40 s.
- Production and PR1 returned 200 from 19:13:22. The last 200 before the
  shutdown was 19:08:49, so total app downtime was **4 min 33 s**, most of it
  the laptop being off.

## Findings

1. **The battery percentage cannot be trusted on a worn cell.** A drop from 38% to
   5% inside one sampling interval left about 1% of margin. Shutdown on battery
   should not depend on two consecutive readings at a 20 s interval when the
   battery's full charge is a small fraction of its design capacity. Options:
   shut down on the first low reading, sample faster on battery, or shut down
   after a fixed time on battery when health is below a threshold.
2. After an outage longer than the battery lasts, the laptop needs a manual power
   button press. AC-return automatic start is absent in the current BIOS settings
   (../2026-11-04-ac-return/).
3. App recovery waits for the agent's first capacity sample, about 40 s after
   boot.

Not qualified: an abrupt hard power cut (the guard won this race), a missing or
healthy battery, and physically reached thermal thresholds.

Evidence: before.json, recovery.json, battery.jsonl, thermal.jsonl,
power-external.jsonl, thermal-external.jsonl, shutdown-journal.txt,
boot-journal.txt, thermal.py, batt.py and the operator-confirmation timestamps.
