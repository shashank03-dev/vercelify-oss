# Several servers

Every old laptop or PC you install Vercelify on can be added to the Mac app. Switch
between them in the sidebar (or ⌘K → *Switch to …*); the **Servers** page shows them
side by side with CPU, memory, disks, temperature, battery and AI models.

## Two ways to add a machine

| | Separate server | Worker in a pool |
| --- | --- | --- |
| Install | `curl … install.sh \| sudo bash` | `curl … install.sh \| sudo bash -s -- --worker` |
| Runs | its own Coolify, agent, firewall, power guard | Docker, agent, firewall, power guard (no Coolify) |
| Good for | 8 GB+ machines, separate projects | 2–4 GB machines, Raspberry Pi, more capacity for one dashboard |
| Deploy to it | switch to it and use **Apps** | create the app on the main server and pick the worker under **Run on** |

Both kinds can run local AI models.

## Adding a machine, step by step

Servers page → **Add a machine** (or the server switcher → **Add a server…**) opens a
three-step guide:

1. **Choose** a separate server or a worker, optionally its Tailscale name and whether
   to set up AI models too.
2. **Install & connect.** Copy the generated command (for example
   `curl … install.sh | sudo bash -s -- --worker --hostname old-dell --ai`) and run it on
   the new machine. The guide keeps scanning your tailnet; when the installer finishes,
   the machine appears with *code ready* and the code field opens by itself. Type the
   8-character code the installer printed. Already installed? Pick *It's already
   installed — just connect it*.
3. **Finish.** A worker can join a main server's pool right there (adding a worker keeps
   your current server selected), and the machine's health checks run so you see at once
   whether anything needs fixing.

## Health checks and updates

Every server card shows the machine's health checks (`GET /v1/doctor`, the same checks as
`sudo vercelify doctor`): Docker, Tailscale, Coolify and its API token (main servers),
disk space, the storage disk, memory, temperature, battery, power guard, firewall, AI
models and auto-deploy errors. Anything not OK comes with the command that fixes it.

**Update** on a card (or **Update all**) runs `vercelify update` on the machine as its own
systemd unit, so it survives the agent restarting midway; the card follows its progress.
Apps and data keep running.

When you create an app on a main server, **Run on** shows the free memory of each machine
in its pool and marks the one with the most room.

## Pairing

At the end of the install, and any time with `sudo vercelify pair`, the server shows
an 8-character code such as `K7QF-M2XD`. In the app (onboarding, or the server
switcher → **Add a server…**) the machines on your tailnet are listed; pick one and
type its code. The code works once, for 15 minutes (an hour right after install),
and five wrong tries throw it away. The full `vfy1.…` code still works for pasting.

Discovery asks each online Linux peer on your tailnet for `/v1/health` on port 7420;
nothing is exposed beyond the tailnet.

## Pooling a worker into a main server

On the Servers page, a worker card has **Add to a pool**. The app then:

1. reads the main server's Coolify SSH public key (`GET /v1/coolify/worker-key` on the main agent),
2. authorises it for root on the worker (`POST /v1/ssh/authorized-keys` on the worker agent),
3. looks up the worker's Tailscale IP, and
4. registers it in the main server's Coolify (`POST /api/v1/servers`, validated over SSH).

The main server card then lists it under **Deploys to**. The same is available on the
host as `sudo vercelify add-worker root@<worker>`.

Settings for all servers live in one file on the Mac (`settings.json`, mode 0600),
which older single-server settings are migrated to automatically.
