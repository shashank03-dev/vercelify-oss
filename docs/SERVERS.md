# Several servers

Every old laptop or PC you install Vercelify on can be added to the Mac app. Switch
between them in the sidebar (or ⌘K → *Switch to …*); the **Servers** page shows them
side by side with CPU, memory, disks, temperature, battery and AI models.

## Two ways to add a machine

| | Separate server | Worker for a main server |
| --- | --- | --- |
| Install | `curl … install.sh \| sudo bash` | `curl … install.sh \| sudo bash -s -- --worker --join vfyj1.…` |
| Runs | its own engine, proxy, agent, firewall, power guard | Docker, agent, firewall, power guard; the main server places apps on it |
| Good for | 8 GB+ machines, separate projects | 2–4 GB machines, Raspberry Pi, more capacity for one main server |
| Deploy to it | switch to it and use **Apps** | create the app on the main server; it goes to the server with the most room, or pin it in the app's **Where it runs** card |

Both kinds can run local AI models.

## Adding a machine, step by step

Servers page → **Add a machine** (or the server switcher → **Add a server…**) opens a
three-step guide:

1. **Choose** a separate server or a worker (and, with several main servers, which one
   it works for), optionally its name and whether to set up AI models too.
2. **Install & connect.** Copy the generated command (for example
   `curl … install.sh | sudo bash -s -- --worker --join vfyj1.… --hostname old-dell --ai`)
   and run it on the new machine. A worker's command carries a single-use invite to its
   main server's network, so it joins and starts taking apps by itself. The guide keeps
   looking for the machine; when the installer finishes, it appears with *code ready*
   and the code field opens. Type the 8-character code the installer printed. Already
   installed? Pick *It's already installed — just connect it*.
3. **Finish.** The machine's health checks run, so you see at once whether anything
   needs fixing.

## Health checks and updates

Every server card shows the machine's health checks (`GET /v1/doctor`, the same checks as
`sudo vercelify doctor`): Docker, the engine and its proxy (main servers), the network,
disk space, the storage disk, memory, temperature, battery, power guard, firewall,
backups, AI models and deploy errors. Anything not OK comes with the command that fixes
it.

**Update** on a card (or **Update all**) runs `vercelify update` on the machine as its own
systemd unit, so it survives the agent restarting midway; the card follows its progress.
Apps and data keep running.

A main server's card lists its workers under **Deploys to**.

## Pairing

At the end of the install, and any time with `sudo vercelify pair`, the server shows
an 8-character code such as `K7QF-M2XD`. In the app (onboarding, or the server
switcher → **Add a server…**) the servers on your LAN and on your Vercelify network are
listed; pick one and type its code. The code works once, for 15 minutes (an hour right
after install), and five wrong tries throw it away. It is checked with a
password-authenticated exchange, so it never crosses the network. The full `vfy2.…`
code still works for pasting, from anywhere ([MESH.md](MESH.md)).

Discovery asks the LAN (multicast DNS) and each server on your network for
`/v1/health` on port 7420; nothing is exposed beyond them.

Settings for all servers live in one file on the Mac (`settings.json`, mode 0600),
which older single-server settings are migrated to automatically.
