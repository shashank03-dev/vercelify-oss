# Raspberry Pi and other small machines

Vercelify treats the hardware as an input. The installer detects the board
(`/proc/device-tree/model`, batteries, memory) and picks a **profile**: `pi5`,
`pi4`, `pi`, `laptop` or `pc`. The profile decides the defaults below; every
value can still be changed in `/etc/vercelify/power-guard.conf`.

| | Laptop / PC | Raspberry Pi |
| --- | --- | --- |
| Builds pause / apps freeze / shutdown | 92 / 95 / 98 °C | 78 / 82 / 85 °C (the firmware throttles near 80 °C) |
| Swap | swapfile on the SSD | zram (compressed RAM, no flash writes) |
| Memory kept for the OS | 1 GB | 768 MB (Pi 5), 512 MB (Pi 4), 384 MB (≤ 2 GB) |
| Builds on the machine | yes | Pi 5 with 8 GB yes; smaller boards should pull images (see below) |

## What the installer does on a Pi

- **Memory limits that work.** Docker silently ignores memory limits when the
  kernel's memory cgroup is off, which Raspberry Pi kernels have shipped as the
  default. The installer adds `cgroup_enable=memory cgroup_memory=1` to
  `cmdline.txt` (backup kept as `cmdline.txt.vercelify.bak`) and asks for one
  reboot.
- **Power supply checks.** The firmware's throttling flags
  (`get_throttled`) are read on every health check. Under-voltage right now is a
  failure, under-voltage since boot is a warning, both with the fix: the
  official supply (27 W for a Pi 5) and a short cable.
- **SD card protection.** SD cards and eMMC are shown as *SD card*, not SSD.
  When the system runs from one: zram instead of a swapfile, bounded journald
  with batched writes, and a warning to boot from a USB SSD or an NVMe HAT.
- **Clock.** A Pi 4 has no battery-backed clock. Services wait (at most 90 s)
  for NTP after boot, so TLS, Git and backups don't fail on a 1970 clock.
- **Hang watchdog.** The SoC watchdog reboots the board after a 30 s hang
  instead of leaving it dead until someone walks over. Laptops with a
  watchdog get the same.
- **Wi-Fi.** Power saving is turned off (NetworkManager) and you are told that
  Ethernet is more reliable for a server.

## Reboots for security updates

Kernel and library updates install themselves (unattended-upgrades) but only
take effect after a reboot. A nightly window (04:30 by default) reboots when one
is needed: `/var/run/reboot-required`, or a replaced kernel, which is how
Raspberry Pi OS shows it. Never during a build, a backup or an update, on
battery, while hot, or within an hour of the last boot; skipped nights are
retried the next night and shown by the doctor.

```bash
sudo vercelify maintenance             # on/off, window, last result
sudo vercelify maintenance on 03:15    # another time
sudo vercelify maintenance off
```

## Sleeping apps free memory

On a 4 GB Pi, apps nobody visits can sleep: `sudo vercelify sleep on APP 30` on the
main server (or the app's **Sleep when idle** card in the Mac app). After 30 minutes without a request
the app stops; the next visitor sees "waking up" for a second or two while it
starts again.

## Never running out of memory

On every machine:

- All containers run in `vercelify.slice`, capped at *RAM − the OS reserve*. An
  app that leaks memory is stopped inside the slice; sshd, the network and the
  agent keep working, so you can still reach the machine.
- The agent sets OOM priorities every minute: build containers die first, then
  pull-request previews, then the AI model server, then apps; the proxy, the
  registry and databases last.
- Memory pressure (PSI) and out-of-memory kills show up in **Health → Hardware**
  and in the doctor, and raise alerts.

## Hot days

The power guard responds in steps instead of freezing everything:

1. **Pause** at `THERMAL_PAUSE_C`: builds and the AI server are paused, apps get
   half the CPUs (`docker update --cpus`) and keep serving.
2. **Freeze** at `THERMAL_FREEZE_C` (default: shutdown − 3 °C): apps are paused
   too. Databases, the proxy and the registry are never frozen; other services
   wait on them.
3. **Resume** at `THERMAL_RESUME_C`: everything is unpaused and CPU limits go
   back to what they were.
4. **Shut down** cleanly after three readings at `THERMAL_SHUTDOWN_C`.

## Building for a Pi

A 4 GB Pi cannot build a Next.js app next to everything else, and it doesn't have
to: the main server builds every image and keeps it in its own registry, which the
Pi pulls from over the network. For an app placed on a Pi the main server builds a
`linux/arm64` image too; on an x86 main server that needs emulation, once:

```bash
sudo vercelify registry multiarch        # on the main server: build arm64 images (QEMU)
```

Emulated builds are slower. If a build is too slow, pin the app to an x86 server in
its **Where it runs** card.

## Public apps on a worker

Visitors always arrive at the main server (its tunnel or its ports); its proxy sends
requests for an app on a worker to that worker over the network. Moving an app to the
Pi needs no DNS or Cloudflare changes.
