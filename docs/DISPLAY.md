# Display streaming

Two directions, both over Vercelify's network ([MESH.md](MESH.md)) or your LAN:

| You want | Sender | Receiver | Typical latency |
| --- | --- | --- | --- |
| **The Linux laptop's panel as a second monitor for the Mac** | Mac: `ffmpeg` (AVFoundation capture → VideoToolbox H.264 → MPEG-TS) | Linux: `mpv` fullscreen, low-latency profile | 30–60 ms on a LAN / direct mesh path |
| **The Linux desktop on the Mac** | Linux: [Sunshine](https://github.com/LizardByte/Sunshine) | Mac: [Moonlight](https://moonlight-stream.org) | 10–20 ms on a LAN |

The cockpit's **Second screen** tab starts and stops both ends for you. Screen
sharing mirrors an existing screen; extension requires an additional Mac display.
The page checks missing tools before starting. **Refresh setup** retries discovery
after installing a tool or changing permissions. **Playing** appears only after
mpv reports that media has opened through its local IPC socket, rather than merely
because two processes exist.

## Linux panel as a second monitor

### One-time setup

- **Mac:** `brew install ffmpeg`. The first stream triggers macOS's Screen
  Recording prompt for Vercelify; allow it in System Settings → Privacy &
  Security → Screen Recording, then restart the cockpit. Updates keep the
  permission. If the prompt still comes back with Vercelify switched on (apps
  from before 0.2.4), run `tccutil reset ScreenCapture dev.vercelify.cockpit`,
  restart the app and allow it once more.
- **Host:** the installer installs `mpv` (skip with `--no-display`).
- **A real extended desktop:** choose **Extend** next to Screen. While it
  streams, the app adds a display of its own, as wide as your Mac's screen
  (at most 1080p for Smooth, 720p for Light; the laptop scales it to fill its
  screen, so the pointer crosses it as quickly as your own), to the right of
  your main screen: drag windows off the right
  edge onto it like any monitor, or rearrange it in System Settings →
  Displays. Stopping the stream removes it and its windows come
  back. **Mirror** shares a screen as it is.

### What happens when you press Start

1. The cockpit asks the agent to start the receiver:
   ```
   mpv --no-config --profile=low-latency --untimed --no-cache \
       --demuxer-lavf-format=mpegts --demuxer-lavf-o=fflags=+nobuffer \
       --demuxer-lavf-probesize=32 --demuxer-lavf-analyzeduration=0.1 \
       --framedrop=vo --hwdec=auto-safe --fullscreen --osc=no --audio=no \
       --loop-playlist=inf --idle=yes --vo=gpu --gpu-context=drm \
       "udp://0.0.0.0:5600?overrun_nonfatal=1&fifo_size=50000"
   ```
   `--gpu-context=drm` draws straight onto the panel, so Debian minimal
   without any desktop environment works. The agent answers with the address
   to send to (its mesh address). When the Mac's `vercelify-mesh` runs in app mode
   (the default, no root), the app adds a forward for the stream
   (`127.0.0.1:15600` → the server's `:5600`, through the daemon's
   `run/forwards` file) and points ffmpeg at it; with `--mesh-system` ffmpeg sends
   straight to the mesh address.
2. The cockpit starts the sender on the Mac:
   ```
   ffmpeg -f avfoundation -capture_cursor 1 -framerate 60 -pixel_format nv12 -i "<screen>:none" \
          -vf scale=1920:-2 -c:v h264_videotoolbox -realtime 1 -allow_sw 1 -profile:v main \
          -b:v 12000k -maxrate 12000k -bufsize 6000k -g 120 -bf 0 -an \
          -f mpegts -flush_packets 1 -muxdelay 0 -muxpreload 0 "udp://100.x.y.z:5600?pkt_size=1316"
   ```
3. **Stop** kills both. Closing the cockpit kills the sender too.

UDP port 5600–5610 is only accepted from the mesh (`vfy0`, firewall rules).

### Tuning

| Situation | Setting |
| --- | --- |
| Home Wi-Fi / same LAN | UDP, *Smooth* (1080p60, 12 Mbps) |
| Text-heavy work (code, docs) | *Sharp* (1440p30, 16 Mbps) |
| Phone hotspot / far away | SRT (retransmits lost packets, +120 ms), *Light* (720p30, 4 Mbps) |
| Laptop CPU is old | keep *Hardware* encoder on the Mac; decoding uses VA-API on the i5's iGPU via `--hwdec=auto-safe` |

The page warns when the Mac doesn't reach the server over your own network: through the
relay, or "direct" but to a public address (a phone hotspot that keeps its devices apart
sends everything out to the mobile network and back; measured: a quarter to over half
the packets lost, 180–210 ms). No setting makes a moving picture whole on such a path:
put both machines on the same Wi-Fi, or use *Light* (less to lose). SRT needs an ffmpeg
that can send it: Homebrew's `ffmpeg` is built without libsrt, and the page says so
rather than failing to start.

### Desktop running on the host?

With `display_backend = "auto"` (the default), the receiver detects an active local
X11 or Wayland session through logind and reads that user's display environment.
It runs mpv as that user, using the actual XAUTHORITY or Wayland runtime directory.
The service shares the X11 socket directory with its private temporary namespace;
authorization still belongs to the logged-in user. No desktop means DRM output.
An unavailable session produces a recovery message instead of silently choosing DRM.

Explicit `display_backend` and `display_env` values remain supported for unusual
setups. They take precedence over automatic detection. Test/null video sinks also
bypass desktop detection. Extend mode makes its display with macOS's own
`CGVirtualDisplay`; nothing is installed on the Mac.

## Linux desktop on the Mac (Moonlight + Sunshine)

Sunshine streams the logged-in desktop, so the host needs one (Ubuntu Desktop and the
like). Turn it on when needed (or choose it during `--configure` installation):

```bash
sudo vercelify desktop on       # install Sunshine and start it on the desktop
sudo vercelify desktop          # status
sudo vercelify desktop off      # stop starting it (Sunshine stays installed)
```

`desktop on` downloads Sunshine's package for your distribution (Ubuntu 22.04/24.04/26.04,
Debian 13, and Ubuntu-based ones like Linux Mint) from its GitHub release, checks it
against the SHA-256 GitHub records for the file, and installs it with apt, which sets
up its screen-capture permission and the input-device rules. Sunshine then runs as
upstream intends, as the desktop user's own service, started at every graphical login.
The agent shows its status in the cockpit and can restart it. On a headless server,
use the second-monitor mode above instead.

In the cockpit, press **Launch Moonlight**. The first time, the app pairs Moonlight with
the server by itself: there is no Sunshine login to create and no PIN to type.

1. The app picks a four-digit PIN and starts `moonlight pair` with it.
2. The agent gives Sunshine its first login (a random password the agent keeps in
   `/var/lib/vercelify/sunshine-login.json`, readable by root only), waits for
   Moonlight's request and hands Sunshine the same PIN.
3. Moonlight's pairing window closes and the desktop opens. Later launches connect at once;
   **Pair again** repeats the pairing (after resetting Moonlight, say).

When the Mac's `vercelify-mesh` runs in app mode (the default), the app first forwards
Sunshine's ports (TCP 47984, 47989, 47990, 48010 and UDP 47998–48000) from `127.0.0.1`
to the server and starts Moonlight on `127.0.0.1`; with `--mesh-system`, or on the same
LAN, Moonlight connects to the server's address directly. Your devices may reach these
ports on the mesh; Sunshine's login and its pairing protect them.

The agent talks to Sunshine's HTTPS page through curl, pinned to the public key of
Sunshine's own certificate, so nothing else listening on that port gets the login.

**Already made a Sunshine login by hand?** The agent leaves it alone and the app says
so. Either run `sudo vercelify desktop reset-login` on the server (Sunshine forgets its
login, paired devices stay paired; the next pairing sets the agent's), or pair by hand:
**Open Sunshine setup**, sign in, launch Moonlight and enter the PIN it shows on
Sunshine's **PIN** page. Servers from before this change show the manual steps too.
