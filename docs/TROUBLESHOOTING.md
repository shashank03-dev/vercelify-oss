# Troubleshooting

Start with:

```bash
sudo vercelify doctor     # checks everything and prints a fix for each failure
sudo vercelify status
```

## Installer

| Symptom | Fix |
| --- | --- |
| `setup failed … near line N` | Read the last lines of `/var/log/vercelify-setup.log`, fix the cause, **run the installer again**. Finished steps are skipped. |
| "refusing to format /dev/sdX" | The disk has no filesystem (or NTFS/exFAT). Re-run with `--format-storage` if you really want it erased, or `--storage-device none`. |
| "… is mounted at /media/…" | Desktop auto-mounted the disk: `sudo umount /media/…` and re-run. |
| A worker says *joining failed* | Invites work once and expire (an hour by default): make a new one on the main server (`sudo vercelify mesh invite`, or the app's **Add a server…**). The worker must reach the main server's port 7443: on the same LAN, or through its Cloudflare tunnel (`mesh.<domain>`). Then `sudo vercelify mesh join <token> --platform`. |
| *the network isn't set up yet* on a main server | `sudo vercelify mesh init`; `sudo vercelify logs mesh` shows why it failed (a missing `/dev/net/tun`, or UDP 51820 taken). |
| Docker Hub `429 Too Many Requests` | Add a mirror: `"registry-mirrors": ["https://mirror.gcr.io"]` in `/etc/docker/daemon.json`, `sudo systemctl restart docker`. |

## Cockpit can't connect

1. Is the Mac on the network? `vercelify-mesh status` (in the app: Servers → network)
   lists each server with its path (`lan`, `direct`, `relay`) and last handshake. No
   handshake for minutes: the server is off, or neither the LAN nor its tunnel reaches
   it (see [MESH.md](MESH.md#reaching-your-servers-from-anywhere)).
2. On the server, `curl http://127.0.0.1:7420/v1/health` should print `{"ok":true,…}`.
   If not, check `sudo systemctl status vercelify-agent` and
   `journalctl -u vercelify-agent -n 50`.
3. "missing or invalid agent token": run `sudo vercelify pair` and pair again.
4. The server isn't in the list: discovery lists servers on the same LAN (multicast
   DNS) and on your network. On another network, paste the full code (`vfy2.…`) instead;
   that needs the server reachable from outside: `sudo vercelify remote on` (or a
   Cloudflare domain). `sudo vercelify remote` says why it isn't (no UPnP/NAT-PMP on the
   router, or CGNAT).
5. *wrong pairing code* / *no pairing code is waiting*: codes work once, for 15
   minutes, and five wrong tries discard them. Run `sudo vercelify pair` for a new one.

## Deployments

| Symptom | Fix |
| --- | --- |
| A push deploys only after a minute | That's the polling interval. For instant deploys set up a webhook or a GitHub App: [PLATFORM.md §2](PLATFORM.md#2-git-private-repositories-and-instant-deploys). |
| Build killed / very slow | Out of RAM. Builds run one at a time on 8 GB; let idle apps sleep, and check `free -h` (swap should exist). |
| *health check failed* and the old version keeps serving | The new container didn't answer on its port in time: check the port in the app's settings and its runtime logs. Nothing switched, so visitors saw no downtime. |
| Disk full | `docker system prune -af` (keeps volumes and the images rollbacks need are rebuilt or pulled again). The engine also removes old images itself. |

## Domains

| Symptom | Fix |
| --- | --- |
| `vercelify domain` says "no active Cloudflare zone found" | Add the domain to Cloudflare first (nameservers switched, status Active). |
| "already has an A record — left untouched" | An existing website uses that name. Use a subdomain (`vercelify domain apps.example.com …`) or `--force-dns`. |
| Cloudflare error 1033 | The tunnel is down: `sudo systemctl status cloudflared`, `journalctl -u cloudflared -n 50`. |
| Redirect loop with Cloudflare | TLS ends at Cloudflare in tunnel mode: in the app's **Domains** card the domain must not ask for HTTPS at the server (new apps get it right; `sudo vercelify domain` sets it for new ones). |

## AI models

| Symptom | Fix |
| --- | --- |
| *needs about 12 GB free on the system disk* | `docker system prune -af` (keeps volumes), or install it on the HDD: `sudo vercelify llm install hdd`. |
| A model download fails at once | The model name is wrong (check ollama.com/library) or the server has no internet. `sudo vercelify llm` shows the error. |
| *model requires more system memory* | Pick a smaller model, stop other apps, or use a server with more RAM. The app marks models that don't fit. |
| First answer is slow | Loading the model from disk (slower from the HDD). Later answers within 5 minutes are faster. |
| Mac can't reach port 11434 | Use the server's network address (`10.77.x.x` or `<name>.vfy`); check `sudo vercelify doctor` on the server. In the app's default mode only the app itself is on the network: other programs reach it through `socks5h://127.0.0.1:7480`, or install with `--mesh-system` for a real interface. |

## Auto-deploy

| Symptom | Fix |
| --- | --- |
| *could not read Username … add a read-only access token* | The repository is private: `sudo vercelify git-source add NAME`, or connect a GitHub App, then pick it as the app's credentials. |
| *branch … not found* | Check the branch name (it is case-sensitive). |
| Pushed but nothing happens | Checks run every 60 s; press **Check now**. While the power guard is cooling the machine, deploys wait. |

## Storage

| Symptom | Fix |
| --- | --- |
| Database won't start after a reboot: "read-only file system"/"operation not permitted" under `/mnt/storage` | The HDD isn't mounted (the empty mount point is immutable on purpose). Check the cable, then `sudo mount /mnt/storage`; `dmesg | tail` shows disk errors. |
| "Move to HDD" is disabled | No disk is mounted at `/mnt/storage` (single-disk mode). |
| A move fails halfway | The data stays where it was and the app or database starts there again; the job's log says why (often: not enough free space on the target). |

## Display streaming

| Symptom | Fix |
| --- | --- |
| Receiver stops immediately with a DRM error | A desktop session owns the GPU. Set `display_backend = "x11"` (or `"wayland"`) and `display_env` in `/etc/vercelify/agent.toml`. |
| Black screen, sender running | macOS Screen Recording permission missing: System Settings → Privacy & Security → Screen Recording → enable Vercelify, restart the app. |
| Laggy or blocky | Check that `vercelify-mesh status` shows the server as `lan` or `direct`, not `relay`. Lower the quality, or switch to SRT on lossy Wi-Fi. |

## Power guard

- Test without unplugging: `sudo POWER_SUPPLY_DIR=/tmp/fake /usr/local/lib/vercelify/power-guard.sh --status`.
- Current state: `cat /run/vercelify/power-guard.state`; logs: `sudo vercelify logs guard`.
- Thresholds: `/etc/vercelify/power-guard.conf`, then `sudo systemctl restart vercelify-power-guard`.

## Raspberry Pi

| Symptom | Fix |
| --- | --- |
| Doctor: *Memory limits — ignored by the kernel* | `sudo reboot` once after the installer (it enabled `cgroup_enable=memory`). Still there? Check `cat /boot/firmware/cmdline.txt`. |
| *Power supply — under-voltage* | Use the official supply (27 W for a Pi 5, 15 W for a Pi 4) and a short cable; unplug USB disks that draw power, or give them a powered hub. |
| *System disk type — running from an SD card* | Boot from a USB SSD or an NVMe HAT (Raspberry Pi Imager → copy the card), and keep backups on. |
| Builds fail or take forever on a Pi | Build on your strongest machine: see [RASPBERRY-PI.md](RASPBERRY-PI.md#building-for-a-pi). |

## Alerts, backups, tokens, updates

| Symptom | Fix |
| --- | --- |
| No notifications | App → Settings → Alerts → **Test**. ntfy topics are public by name: pick a long random one. |
| *the notification could not be delivered* | The URL is wrong or the server has no internet; `curl -d test <url>` on the server. |
| Doctor: *Backups — not set up* | `sudo vercelify backup setup` (add `--to sftp:root@<other server>:/path` for an off-site copy). |
| *cannot open … with that password* | The repository was created with another password: use the one from your password manager. |
| The app says *missing or invalid agent token* after a reinstall or revoke | `sudo vercelify pair` and pair again (device tokens are per pairing). |
| `vercelify update` says it rolled back | Read the reason in `sudo vercelify logs setup`; fix it, then update again. `sudo vercelify rollback` returns to the previous version by hand. |
| An app on a worker isn't reachable | The main server's proxy forwards it over the network: the worker must be online (Servers page) and `vercelify-mesh status` on the main server must show a handshake with it. |
| An app needs a service on another server | Containers can't reach other machines on the network except their proxies and databases' public ports. Allow a port in `/etc/vercelify/firewall.conf` on the app's server: `CONTAINER_MESH_ALLOW_TCP="6379"`, then `sudo systemctl restart vercelify-firewall`. |
| A visitor sees *Waking up …* | The app was asleep (no visits for its idle time) and is starting; the page reloads by itself. Turn sleep off in the app's **Sleep when idle** card or `sudo vercelify sleep off APP`. |
| *Waking up* never ends | The app's container doesn't become healthy: open its logs. An app you stopped yourself isn't woken by visits: start it in the app. |
| Sleep is on but the app never sleeps | `sudo vercelify sleep` shows why (e.g. *the proxy's request counters are unreachable*: check `docker ps` shows `vercelify-proxy` running). A build in progress also postpones sleeping. |
| Doctor: *Platform* is red (the engine couldn't open its state database) | `journalctl -u vercelify-agent -n 50` says why; if the database is damaged, `sudo vercelify restore-state` puts back its last good copy (at most 5 minutes old). |
| The server rebooted at night | The reboot window applied security updates (`sudo vercelify maintenance` shows the last run). Change the time with `sudo vercelify maintenance on 03:15`, or turn it off. |
| Doctor: *Security updates — skipped last night* | Something was running (a build, a backup, battery). It retries the next night; `sudo reboot` now if you prefer. |
| Power guard thresholds don't match `power-guard.conf` | Values changed in the app win: Settings → Power guard → **Use this machine's defaults**, or delete `/var/lib/vercelify/power-guard.local.conf`. |
