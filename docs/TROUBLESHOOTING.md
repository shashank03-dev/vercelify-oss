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
| Tailscale step waits forever | Open the printed link, or use `--tailscale-authkey tskey-auth-…` (Tailscale admin → Settings → Keys). |
| Coolify never becomes healthy | `docker logs coolify --tail 50`. If you see `socket() [::]:8080 failed (97: Address family not supported)`, IPv6 is disabled in your kernel (`ipv6.disable=1` on the kernel command line). Remove it, reboot, re-run. |
| Docker Hub `429 Too Many Requests` | Add a mirror: `"registry-mirrors": ["https://mirror.gcr.io"]` in `/etc/docker/daemon.json`, `sudo systemctl restart docker`. |

## Cockpit can't connect

1. Both machines on the same tailnet? `tailscale status` on the Mac must
   list the host as online.
2. `curl http://<host>.<tailnet>.ts.net:7420/v1/health` from the Mac should
   print `{"ok":true,…}`. If not, check `sudo systemctl status vercelify-agent` and
   `journalctl -u vercelify-agent -n 50` on the host.
3. "missing or invalid agent token": run `sudo vercelify pair` and paste the new
   code in Settings → Pair again.
4. Apps tab says "Coolify API not connected": run `sudo vercelify pair --refresh`
   (creates a new Coolify token) and pair again.
5. The server isn't in the "Servers on your tailnet" list: discovery only lists online
   Linux peers whose agent answers on port 7420. Use **Not listed? Enter its address**
   with its Tailscale name or IP, or paste the full code.
6. *wrong pairing code* / *no pairing code is waiting*: codes work once, for 15
   minutes, and five wrong tries discard them. Run `sudo vercelify pair` for a new one.

## Deployments

| Symptom | Fix |
| --- | --- |
| Every deployment fails instantly with `usage: ssh …` | Coolify's localhost server has an empty SSH user. Re-run the installer, or `docker exec coolify-db psql -U coolify -d coolify -c "update servers set \"user\"='root' where id=0"`. |
| Git push doesn't trigger a deploy | GitHub can't reach Coolify's webhook: see [COOLIFY.md §3](COOLIFY.md#3-connect-github-push-to-deploy--pr-previews). |
| Build killed / very slow | Out of RAM. Keep concurrent builds at 1, add memory limits to apps, and check `free -h` (swap should exist). |
| `pr-42-app.example.com` has no certificate | Make sure the preview template is `pr-{{pr_id}}-{{domain}}` (one label deep). Apps created before the installer changed the default keep `{{pr_id}}.{{domain}}`: change it in the app's Preview Deployments settings. |
| Disk full | `docker system prune -af` (keeps volumes), and Coolify → Servers → localhost → Docker Cleanup. |

## Domains

| Symptom | Fix |
| --- | --- |
| `vercelify domain` says "no active Cloudflare zone found" | Add the domain to Cloudflare first (nameservers switched, status Active). |
| "already has an A record — left untouched" | An existing website uses that name. Use a subdomain (`vercelify domain apps.example.com …`) or `--force-dns`. |
| Cloudflare error 1033 | The tunnel is down: `sudo systemctl status cloudflared`, `journalctl -u cloudflared -n 50`. |
| Redirect loop with Cloudflare | Use `http://` URLs for apps in Coolify (TLS ends at Cloudflare) and turn off "Force HTTPS" in the app. |

## AI models

| Symptom | Fix |
| --- | --- |
| *needs about 12 GB free on the system disk* | `docker system prune -af` (keeps volumes), or remove old images in Coolify → Servers → Docker cleanup. |
| A model download fails at once | The model name is wrong (check ollama.com/library) or the server has no internet. `sudo vercelify llm` shows the error. |
| *model requires more system memory* | Pick a smaller model, stop other apps, or use a server with more RAM. The app marks models that don't fit. |
| First answer is slow | Loading the model from disk (slower from the HDD). Later answers within 5 minutes are faster. |
| Mac can't reach port 11434 | Use the Tailscale name/IP; check `sudo vercelify doctor` on the server. |

## Auto-deploy

| Symptom | Fix |
| --- | --- |
| *could not read Username … add a read-only access token* | The repository is private: add a token in the app's Auto-deploy tab. |
| *branch … not found* | Check the branch name (it is case-sensitive). |
| *Coolify refused the deployment* | Run `sudo vercelify pair --refresh` on the server if the token expired, then check the app in Coolify. |
| Pushed but nothing happens | Checks run every 60 s; press **Check now**. While the power guard is cooling the machine, deploys wait. |

## Storage

| Symptom | Fix |
| --- | --- |
| Database won't start after a reboot: "read-only file system"/"operation not permitted" under `/mnt/storage` | The HDD isn't mounted (the empty mount point is immutable on purpose). Check the cable, then `sudo mount /mnt/storage`; `dmesg | tail` shows disk errors. |
| "Move to HDD" is disabled | No disk is mounted at `/mnt/storage` (single-disk mode). |
| Move says "started while its data was being copied" | Something restarted the resource during the copy; nothing was changed. Stop it in Coolify and retry. |

## Display streaming

| Symptom | Fix |
| --- | --- |
| Receiver stops immediately with a DRM error | A desktop session owns the GPU. Set `display_backend = "x11"` (or `"wayland"`) and `display_env` in `/etc/vercelify/agent.toml`. |
| Black screen, sender running | macOS Screen Recording permission missing: System Settings → Privacy & Security → Screen Recording → enable Vercelify, restart the app. |
| Laggy or blocky | Check that Node Health shows the peer as **direct**, not **relay**. Lower the quality, or switch to SRT on lossy Wi-Fi. |

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
| Doctor: *Coolify version — missing …* | Coolify changed a table Vercelify uses. `sudo vercelify update` (a newer Vercelify may support it). Auto-update is off so this only happens after `vercelify coolify upgrade`. |
| An app on a worker isn't reachable on my domain | `sudo vercelify routes sync` on the main server; the worker's Coolify proxy must be running. |
| An app needs to reach a database on another server over Tailscale | Containers are kept off the tailnet. Allow the port in `/etc/vercelify/firewall.conf`: `CONTAINER_TAILNET_ALLOW_TCP="22 5000 8000 5432"`, then `sudo systemctl restart vercelify-firewall`. |
| A visitor sees *Waking up …* | The app was asleep (no visits for its idle time) and is starting; the page reloads by itself. Turn sleep off in the app's Domains tab or `sudo vercelify sleep off APP_UUID`. |
| *Waking up* never ends | The app's container doesn't become healthy: open its logs. If it was stopped in Coolify on purpose, start it there (the agent only starts containers it put to sleep). |
| Sleep is on but the app never sleeps | `sudo vercelify sleep` shows why (e.g. *the proxy's request counters are unreachable*): turn sleep off and on again in the app to set up the proxy, or check Coolify → Servers → Proxy is running. A build in progress also postpones sleeping. |
| The server rebooted at night | The reboot window applied security updates (`sudo vercelify maintenance` shows the last run). Change the time with `sudo vercelify maintenance on 03:15`, or turn it off. |
| Doctor: *Security updates — skipped last night* | Something was running (a build, a backup, battery). It retries the next night; `sudo reboot` now if you prefer. |
| Power guard thresholds don't match `power-guard.conf` | Values changed in the app win: Settings → Power guard → **Use this machine's defaults**, or delete `/var/lib/vercelify/power-guard.local.conf`. |
