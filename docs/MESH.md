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

**Your Mac.** `install-mac.sh --pair vfy2.…` (the command the server installer and
`sudo vercelify pair` print) installs the app and `vercelify-mesh`, and the app pairs by
itself when it opens. With the app already installed, pick
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

Joining needs the main server's control port (TCP 7443), and direct paths use WireGuard
(UDP 51820). With `sudo vercelify remote on` (the installer asks, and the answer defaults
to yes) there is nothing to set up, whatever the network:

1. **Your router forwards both ports** to the main server by itself, over UPnP or
   NAT-PMP. The public address goes into `/etc/vercelify/agent.toml`
   (`mesh_public_endpoints`, `mesh_public_url`).
2. **Otherwise a Cloudflare quick tunnel** carries the control port: a free
   `https://<random>.trycloudflare.com` address, with no account and no domain. This is
   what happens behind CGNAT, on a mobile hotspot, or with a router that won't forward.
   Joins and the relay go through it (`vercelify-quicktunnel.service`); once joined,
   machines still connect directly whenever their networks allow, and fall back to the
   relay otherwise.

A timer (`vercelify-remote.timer`, every 20 minutes) renews the forwards, follows a new
public address or tunnel, and switches back to a direct forward when the router allows
it. Each time the address changes, the agent signs it with the mesh's coordinator key and
the server posts it to a public message drop (ntfy.sh, under a topic derived from that
key). Devices retry that lookup every 30 seconds while disconnected. A machine that hasn't heard from the main server for 30 seconds reads the
newest note there, checks the signature, and follows it, so a reboot that gives the
quick tunnel a new address doesn't strand your Mac. `sudo vercelify remote` shows the
current address; `sudo vercelify remote off` removes the forwards and the tunnel.

Other ways:

- **On your LAN** nothing is needed.
- **Your own Cloudflare domain** (`sudo vercelify domain example.com --cloudflare-token …`)
  gives a permanent `https://mesh.example.com` for the control port, and `remote on`
  uses it instead of a quick tunnel. Quick tunnels have no uptime guarantee from
  Cloudflare and a limit of 200 requests in flight, which joins and the relay stay far
  below, but a domain is the sturdier choice for a server you rely on.
- **By hand** (a router without UPnP/NAT-PMP, and no tunnel wanted): forward TCP 7443 and
  UDP 51820 to the main server, and list the public address in
  `/etc/vercelify/agent.toml` (`mesh_public_endpoints = ["203.0.113.7:51820"]`).

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
is no network interface. The app (and `vfy`) use the network through the
daemon's SOCKS proxy (`socks5h://127.0.0.1:7480`). For the display stream and Moonlight
the app adds forwards from `127.0.0.1` by itself. Any other program can use the SOCKS
proxy. To give the whole Mac a real interface and `*.vfy` names, install with
`install-mac.sh --mesh-system` (it asks for your password once).

Private apps also open in your normal browser through a loopback HTTP gateway
on `127.0.0.1:7481`. The cockpit opens
`http://blog.10.77.0.1.sslip.io.localhost:7481/` for
`http://blog.10.77.0.1.sslip.io/`, preserving the path and query. Previews and
private HTTP domains use the same mapping. The gateway restores the original
Host for Traefik, supports WebSockets, and only connects to mesh destinations.
Public HTTPS domains retain their public URL. Chrome and Firefox handle
`*.localhost` locally; Safari resolution has not been verified. If Safari cannot
open it, use Chrome/Firefox pending the Mac field check, or use system mode.

Ollama is served directly on the mesh at port 11434. There is no `ai.<server>`
Traefik route, so an `ai.<server>.localhost:7481/v1` snippet would fail. In app
mode, use SOCKS with clients that support it, for example:

```bash
curl --socks5-hostname 127.0.0.1:7480 http://10.77.0.1:11434/api/tags
```

For clients without SOCKS support (including the Ollama CLI), add a loopback TCP
forward to `~/Library/Application Support/Vercelify/run/forwards`, preserving its
other lines:

```text
127.0.0.1:11434=10.77.0.1:11434 # ollama
```

The daemon reads the file every second. Use `http://127.0.0.1:11434` as the client
base URL (`/v1` for OpenAI clients). Remove that line when finished. The direct
mesh URLs in the AI snippets require system mode or a SOCKS-aware client.

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
