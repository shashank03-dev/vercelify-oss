# Vercelify's private network

Your Mac, your phone and every server you add talk to each other over Vercelify's own
network: `vercelify-mesh`, a small WireGuard daemon on each machine, coordinated by your
main server. Nothing needs an account anywhere, and the agent, AI models and database
ports are never on the internet. The design is in
[SYSTEM-DESIGN.md §13](design/SYSTEM-DESIGN.md#13-mesh).

## How it fits together

- The **main server coordinates** the network as `10.77.0.1` (`sudo vercelify mesh init`,
  which the installer runs). It hands out addresses (`10.77.x.x`) and signed
  certificates, and tells every machine who its peers are.
- **Every machine** runs `vercelify-mesh`, which keeps a WireGuard interface (`vfy0` on
  Linux; in the app's session on a Mac). Traffic goes straight between machines: on your
  LAN directly, across the internet by hole punching, and through the main server's relay
  when neither works (strict NAT, CGNAT on both ends).
- **Names**: `<machine>.vfy` resolves to its mesh address on every server and on a Mac
  with the system-wide daemon.

## Adding machines

**Your Mac.** Install the app (`install-mac.sh` installs `vercelify-mesh` too), then pick
your server and type the 8-character code from `sudo vercelify pair`. On the same Wi-Fi
the app finds the server by itself; elsewhere, paste the full pairing code. The code is
checked with a password-authenticated exchange: it never crosses the network, and
someone guessing gets a handful of tries before it expires (15 minutes).

**Another server or a Raspberry Pi.** In the app: **Add a server… → A worker for a main
server**, which gives an install command carrying a single-use invite. Or on the main
server:

```bash
sudo vercelify mesh invite              # a server (worker)
sudo vercelify mesh invite --device     # a laptop or phone of yours
```

and run what it prints on the new machine.

## Reaching your servers from anywhere

Joining needs the main server's control port (TCP 7443) and WireGuard (UDP 51820):

- **On your LAN** nothing is needed.
- **With a Cloudflare tunnel** (`sudo vercelify domain example.com --cloudflare-token …`)
  `mesh.example.com` reaches the control port through the tunnel, so you can join and
  use the relay from any network, with no port forwarding.
- **Without a tunnel** forward TCP 7443 and UDP 51820 on your router to the main server,
  and list the public address in `/etc/vercelify/agent.toml`
  (`mesh_public_endpoints = ["203.0.113.7:51820"]`).

Once joined, machines find their own paths (direct or relayed); a server behind CGNAT
works as a worker too.

## What can reach what

- **Servers** reach each other on any port (the main server's proxy forwards visitors to
  apps on workers; workers pull images from its registry).
- **Your devices** reach only the agent (7420), the proxy (80/443), Ollama (11434), the
  display stream (5600–5610/udp), Sunshine for Moonlight (47984–48010) and the
  databases' public ports.
- **Containers** can't reach the mesh at all, except each server's proxy and the
  databases' public ports: an app (or a pull request preview) never becomes a way into
  your other machines.

## On a Mac: app mode and system mode

By default `vercelify-mesh` runs in your login session without root ("app mode"): there
is no network interface, and only the app (and `vfy`) use the network, through the
daemon's SOCKS proxy (`socks5h://127.0.0.1:7480`). For the display stream and Moonlight
the app adds forwards from `127.0.0.1` by itself. Any other program can use the SOCKS
proxy. To give the whole Mac a real interface and `*.vfy` names, install with
`install-mac.sh --mesh-system` (it asks for your password once).

## Day to day

```bash
vercelify mesh status          # this machine: address, peers, path (lan/direct/relay), last handshake
sudo vercelify mesh nodes      # on the main server: every machine and when it was last seen
sudo vercelify mesh revoke NAME   # a lost laptop or a retired Pi: dropped by every peer within seconds
sudo vercelify logs mesh
```

Certificates renew by themselves (30 days, renewed after 20); `sudo vercelify mesh
rotate` changes a machine's key now. The Servers page in the app shows the same: each
machine's path and round trip, invites and revoking.

## Coming from Tailscale

Servers installed before the mesh existed used Tailscale. `sudo vercelify migrate` sets the
mesh up next to it (Tailscale keeps working meanwhile); pair your devices again with
`sudo vercelify pair`, then `sudo vercelify migrate --tailscale-off` stops the agent
listening on Tailscale once every machine has been seen on the mesh. You can uninstall
Tailscale afterwards.
