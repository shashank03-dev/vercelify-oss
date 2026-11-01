# Hosting on a Mac or Windows computer

Linux machines host Vercelify apps directly (`install.sh`). A Mac or a Windows PC
hosts them inside **one Vercelify environment**: a small Linux virtual machine,
made once, that every app, database and build on that computer shares. Each app
still has its own container, network, secrets and data. Adding a second project
never makes a second environment.

The engineering design is `docs/design/SYSTEM-DESIGN.md` §19.

## Support status

| Computer | Manage servers (the app) | Host apps |
| --- | --- | --- |
| Linux | yes | yes, directly (`install.sh`) |
| macOS 13+ (Apple silicon or Intel) | yes | **yes**, Lima with Apple's Virtualization framework (verified on macOS 27, Apple M5) |
| macOS 11–12 | yes | Lima with QEMU and Hypervisor.framework: built, **not yet qualified** on real hardware |
| Windows 10 2004+ / 11, x64 | yes (installer built in CI) | QEMU with the Windows Hypervisor Platform: built and unit-tested, **in qualification**, so don't rely on it yet |
| Windows on ARM | yes | no |

## Set up a Mac

```bash
curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install-mac.sh | bash -s -- --host
```

This installs the app, Vercelify's network daemon, the host manager
(`vercelify-host`) and a pinned copy of Lima (checked against its published
checksum). Your own Lima or Colima, if you have one, is left alone. Then, in the
app: **Servers → Vercelify environment**.

## Set up a Windows PC

Install the Vercelify app, then in an elevated PowerShell:

```powershell
Set-ExecutionPolicy -Scope Process Bypass; .\install-windows.ps1
```

It asks before it turns on the Windows Hypervisor Platform (a restart follows) or
installs QEMU. It makes a startup task, so hosting runs before anyone signs in. It
also adds firewall rules for a main server's two mesh ports, on Private networks
only.

## Create the environment

The app measures the computer and recommends **Memory**, **Processing power** and a
**Storage limit**. It leaves at least 4 GB of memory, two processors and 10 % of the
disk to you. You can change the values within the limits shown. Choose how it joins:

- **Join my servers**: paste an invite from your main server
  (`sudo vercelify mesh invite --server`). The environment becomes a worker; nothing
  on your computer is opened to the network.
- **This is my first server**: the environment becomes your main server. Its two mesh
  ports (7443/tcp, 51820/udp) are opened on this computer, as on a Linux main server.
  **Connect this app** pairs the app with it; **Pair another computer** gives a
  one-use code.

Creation shows its steps (*Checking computer, Preparing environment, Starting hosting,
Checking connection*). The first one downloads about 350 MB and installs Vercelify
inside, which takes 5–15 minutes. It is ready only once Docker, the agent, the
isolation rule and the clock check out. If it is interrupted, **Try again** starts
cleanly.

The CLI does the same on the computer itself:

```bash
vercelify-host recommend
vercelify-host create --join vfyj1.… --memory 4 --cpus 2 --disk 40 --yes
vercelify-host status
vercelify-host resize --memory 6 --yes    # restarts the environment
vercelify-host stop --yes                 # stops every app on this computer
vercelify-host start
vercelify-host remove --yes               # deletes the environment and all its data
```

## What to expect

- **Closing the app doesn't stop hosting.** A sleeping or switched-off computer
  serves nothing. While the environment runs, the host manager keeps the computer
  from idle sleep (on a Mac, on power). A closed laptop on battery still sleeps.
- **Restarts.** The environment comes back on its own after a restart or a crash. On
  a Mac with FileVault (the default), nothing starts until someone logs in once after
  the restart. Windows Update restarts a PC from time to time.
- **Battery and heat.** The environment is told the real computer's battery and
  temperature. On battery below 50 %, or when hot, it takes no new apps.
- **Isolation.** Apps can't reach your computer's own services (a database on
  `localhost`, the host manager). The environment sees none of your files.
- **Resources.** Memory and processor changes restart the environment, and the app
  asks first. The storage limit can grow but not shrink. The disk image grows as it
  fills, up to the limit.
- **Deleting the environment** deletes every app, database, volume and backup stored
  in it. Back up what you need first.

## Troubleshooting

- `vercelify-host status` shows the state and the last error. The log is
  `~/Library/Application Support/Vercelify/run/host.log` (Mac) or the
  "Vercelify host" task's history (Windows).
- "another Vercelify host manager already manages this computer": another user
  account, or another copy, runs it. Only one can.
- "port 7443 (TCP) is used by another program": a main server needs it. Stop that
  program, or join an existing main server instead.
- A step that took too long (a stalled download) fails with "retrying is safe".
  Choose **Try again**.
