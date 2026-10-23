# Display streaming

Two directions, both over Tailscale:

| You want | Sender | Receiver | Typical latency |
| --- | --- | --- | --- |
| **The Linux laptop's panel as a second monitor for the Mac** | Mac: `ffmpeg` (AVFoundation capture → VideoToolbox H.264 → MPEG-TS) | Linux: `mpv` fullscreen, low-latency profile | 30–60 ms on a LAN / direct Tailscale path |
| **The Linux desktop on the Mac** | Linux: [Sunshine](https://github.com/LizardByte/Sunshine) | Mac: [Moonlight](https://moonlight-stream.org) | 10–20 ms on a LAN |

The cockpit's **Screencast** tab starts and stops both ends for you.

## Linux panel as a second monitor

### One-time setup

- **Mac:** `brew install ffmpeg`. The first stream triggers macOS's Screen
  Recording prompt for Vercelify; allow it in System Settings → Privacy &
  Security → Screen Recording, then restart the cockpit.
- **Host:** the installer installs `mpv` (skip with `--no-display`).
- **A real extended desktop:** macOS only lets ffmpeg capture screens that
  exist. Create a virtual display with [BetterDisplay](https://github.com/waydabber/BetterDisplay)
  (free tier) or plug in an HDMI dummy plug, arrange it in System Settings →
  Displays, then choose that screen in the cockpit. Match its resolution to the
  laptop panel (1920×1080 for most).

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
   to send to (its Tailscale IP).
2. The cockpit starts the sender on the Mac:
   ```
   ffmpeg -f avfoundation -capture_cursor 1 -framerate 60 -pixel_format nv12 -i "<screen>:none" \
          -vf scale=1920:-2 -c:v h264_videotoolbox -realtime 1 -allow_sw 1 -profile:v main \
          -b:v 12000k -maxrate 12000k -bufsize 6000k -g 120 -bf 0 -an \
          -f mpegts -flush_packets 1 -muxdelay 0 -muxpreload 0 "udp://100.x.y.z:5600?pkt_size=1316"
   ```
3. **Stop** kills both. Closing the cockpit kills the sender too.

UDP port 5600–5610 is only accepted on `tailscale0` (firewall rules).

### Tuning

| Situation | Setting |
| --- | --- |
| Home Wi-Fi / same LAN | UDP, *Smooth* (1080p60, 12 Mbps) |
| Text-heavy work (code, docs) | *Sharp* (1440p30, 16 Mbps) |
| Phone hotspot / far away | SRT (retransmits lost packets, +120 ms), *Light* (720p30, 4 Mbps) |
| Laptop CPU is old | keep *Hardware* encoder on the Mac; decoding uses VA-API on the i5's iGPU via `--hwdec=auto-safe` |

Check that Tailscale has a **direct** path (Node Health → peers shows "direct",
not "relay"); relays add a lot of latency. `tailscale ping <host>` shows it too.

### Desktop running on the host?

DRM output needs the GPU to be free. When a desktop session (GNOME/KDE) is running,
use it instead, in `/etc/vercelify/agent.toml`:

```toml
display_backend = "x11"            # or "wayland"
display_env = { DISPLAY = ":0", XAUTHORITY = "/home/you/.Xauthority" }
```

then `sudo systemctl restart vercelify-agent`.

## Linux desktop on the Mac (Moonlight + Sunshine)

Sunshine needs a graphical session on the host (it captures it). On a
headless Debian minimal install, use the second-monitor mode above instead.

Install Sunshine on the host (Debian 12 example; pick the package matching
your distro from the [releases](https://github.com/LizardByte/Sunshine/releases)):

```bash
cd /tmp
curl -fLO https://github.com/LizardByte/Sunshine/releases/latest/download/sunshine-debian-bookworm-amd64.deb
sudo apt install -y ./sunshine-debian-bookworm-amd64.deb
```

Then start it as your desktop user, either from the application menu or by following
[Sunshine's Linux setup guide](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2getting__started.html),
which also covers autostart and the `uinput` permissions it needs for mouse
and keyboard input. If you run it as a systemd unit named `sunshine`, the
cockpit shows its status and can restart it.

Open `https://<host>:47990` from the Mac (over Tailscale), create the Sunshine
admin user, then in Moonlight add the host by its Tailscale name and enter the
PIN Moonlight shows in Sunshine's **PIN** page. After that, **Launch Moonlight**
in the cockpit connects directly.
