# Backups and recovery

The main server backs itself up every night with [restic](https://restic.net):
encrypted, deduplicated, with a retention policy (7 daily, 4 weekly, 3 monthly).
The installer turns it on with a repository on the storage disk; add an
off-site copy with one command.

## What is saved

| Item | Why it matters |
| --- | --- |
| The engine's state (`platform.db`) | Projects, apps, deployments, environment variables, domains, databases, servers. Copied with SQLite's online backup (or its 5-minute copy, `platform.db.bak`): the live file is never read while it is written. |
| The master key (`/var/lib/vercelify/master.key`) | Unseals every stored secret (variables, database passwords, Git tokens). Without it a restored state can't be read. |
| `/etc/vercelify`, `/var/lib/vercelify` | Agent tokens, the network's keys, alerts, the Cloudflare token. |
| A dump of every database container | Postgres (`pg_dumpall`), MySQL/MariaDB (`--single-transaction`), MongoDB (`mongodump`); Redis-like stores are flushed (`SAVE`) before their files are copied. Dumps are consistent even while apps write. |
| App volumes on both tiers | `/mnt/storage/volumes` and Docker's named volumes (AI models excluded). |

## Commands

```bash
sudo vercelify backup setup                                   # local repository on the HDD
sudo vercelify backup setup --to /mnt/storage/backups/restic \
    --to sftp:root@pi5-worker:/mnt/storage/backups/$(hostname -s)   # + another server in your pool
sudo vercelify backup setup --to s3:https://<account>.r2.cloudflarestorage.com/backups \
    --s3-key KEY --s3-secret SECRET                           # + any S3-compatible bucket
sudo vercelify backup run          # now (also: Mac app → Settings → Back up now, or `vfy backup`)
sudo vercelify backup              # last result and next run
sudo vercelify backup list         # snapshots
sudo vercelify logs backup
```

The setup prints the **backup password**. Keep it in your password manager:
without it nobody, you included, can restore. It is stored on the server in
`/etc/vercelify/backup.env` (mode 0600), which is also inside the backup, so
the copy you keep is what lets you restore after losing the disk.

Backups run with idle I/O priority and nice 15, so apps don't notice. Each run
writes `/var/lib/vercelify/backup.json`; the doctor warns when the last backup
is older than two days or failed, and the agent sends an alert.

## Restoring

Files only (safe, nothing on the live server changes):

```bash
sudo vercelify backup restore              # latest snapshot → /var/lib/vercelify/restore-<date>
sudo vercelify backup restore 7c1e9a42 --target /tmp/r
```

A whole server, onto a fresh machine after a disk failure:

1. Run the normal installer on the new machine (same domain options).
2. Connect to the same repository with the password you kept:
   `sudo VERCELIFY_BACKUP_PASSWORD='<password>' vercelify backup setup --to <the same repository>`
   (an off-site one: the local disk is gone).
3. `sudo vercelify backup restore latest --instance` — stops the agent and the apps,
   puts back the master key, the engine's state and the network's keys, then the app
   volumes, and starts everything again. The engine brings every app and database back
   as it was when the backup ran; database dumps are under
   `<restore dir>/var/lib/vercelify/backup-stage/databases` if you want to load one by
   hand.
4. Workers and your devices keep their place on the network. They find the new machine
   by itself through a Cloudflare tunnel, or when it gets the old one's LAN address;
   otherwise re-pair them (`sudo vercelify pair`, `sudo vercelify mesh invite`).

With a worker to spare, a [warm standby](PLATFORM.md#6-more-servers-workers-and-a-standby)
takes over in minutes instead (`sudo vercelify promote`).

A snapshot taken while the server still ran Coolify is restored onto a server running
Coolify, then moved with `sudo vercelify migrate`; `--instance` refuses to put it over
the engine.

Practice it once on a spare machine: a backup you have never restored is a hope,
not a plan.
