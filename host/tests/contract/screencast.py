#!/usr/bin/env python3
"""Screencast contract test: the real sender, the real receiver, a real network.

Two network namespaces stand in for the Mac and the Linux server, joined by a
veth link that `tc netem` turns into home Wi-Fi, a lossy link, a cellular
uplink or an outage. The agent and its mpv receiver run in the server's
namespace; this script acts as the Mac: it calls the agent's API over the
impaired link and streams with ffmpeg, using the exact arguments the cockpit
builds (`vercelify-core`'s `sender_args`, printed by its example). Playback
smoothness (speed against real time, the longest freeze) are read from
mpv's own IPC socket, not inferred from processes.

  cargo build -p vercelify-agent && cargo build -p vercelify-core --example sender_args
  sudo python3 host/tests/contract/screencast.py   # needs root, iproute2 (tc), mpv, ffmpeg

AGENT_BIN and SENDER_ARGS_BIN override the binaries (default: target/debug).
ONLY=name,name runs some scenarios; KEEP=1 leaves the work directory;
MPV_EXTRA adds receiver options (to try a change without a rebuild);
SENDER_LOGLEVEL sets ffmpeg's (e.g. verbose) to debug a sender.
mpv's null video sink decodes every frame without a GPU or a screen.
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
AGENT = os.environ.get("AGENT_BIN", str(TARGET / "debug/vercelify-agent"))
SENDER_ARGS = os.environ.get("SENDER_ARGS_BIN", str(TARGET / "debug/examples/sender_args"))
NS_MAC, NS_HOST = "vfys-mac", "vfys-host"
MAC_IP, HOST_IP = "10.88.0.2", "10.88.0.1"
PORT, API_PORT = 5600, 17426
TOKEN = "screencast-contract-token-0123456789"
RESETS = []

# Link conditions (netem), applied in both directions of the veth link.
CLEAN = ""
WIFI = "delay 4ms 3ms distribution normal loss 0.3%"
LOSSY = "delay 8ms 4ms loss 2% duplicate 1% reorder 10% 50%"
CELLULAR = "delay 45ms 15ms distribution normal loss 1% rate 10mbit"
OUTAGE = "loss 100%"
# The longest a lost UDP packet may freeze the picture: one keyframe interval.
KEYFRAME_FREEZE = 2.2


def sh(*args, check=True, **kw):
    return subprocess.run(args, check=check, capture_output=True, text=True, **kw)


def in_host(*args):
    return ["ip", "netns", "exec", NS_HOST, *args]


def link(conditions):
    """Set the netem conditions on both ends of the link."""
    for ns, dev in ((NS_MAC, "eth0"), (NS_HOST, "eth0")):
        sh("ip", "netns", "exec", ns, "tc", "qdisc", "del", "dev", dev, "root", check=False)
        if conditions:
            sh("ip", "netns", "exec", ns, "tc", "qdisc", "add", "dev", dev, "root", "netem", *conditions.split())


def wait_for(what, predicate, timeout, interval=0.2):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        last = predicate()
        if last:
            return last
        time.sleep(interval)
    raise AssertionError(f"timed out after {timeout}s waiting for {what} (last: {last!r})")


class Agent:
    """A vercelify-agent in the server's namespace with its own state."""

    def __init__(self, work, name="agent", port=API_PORT, **env):
        self.work, self.port = Path(work) / name, port
        self.work.mkdir(parents=True)
        self.env = os.environ | {
            "VERCELIFY_CONFIG": "/nonexistent", "VERCELIFY_ENGINE": "none",
            "VERCELIFY_LISTEN": "0.0.0.0", "VERCELIFY_PORT": str(port),
            "VERCELIFY_TOKEN": TOKEN, "VERCELIFY_STATE_DIR": str(self.work),
            "VERCELIFY_RUN_DIR": str(self.work / "run"), "VERCELIFY_MESH_DIR": str(self.work / "mesh"),
            "VERCELIFY_INSTALL_ENV": str(self.work / "install.env"),
            "VERCELIFY_TOKENS_FILE": str(self.work / "tokens.json"),
            "VERCELIFY_AUDIT_LOG": str(self.work / "audit.log"),
            "VERCELIFY_REQUIRE_STORAGE_MOUNT": "false",
            "VERCELIFY_MPV_EXTRA_ARGS": "--vo=null " + os.environ.get("MPV_EXTRA", ""),
        } | env
        self.log = open(self.work / "agent.log", "w+")
        self.proc = subprocess.Popen(in_host(AGENT, "--config", "/nonexistent"), env=self.env,
                                     stdout=self.log, stderr=self.log, start_new_session=True)
        wait_for("the agent to answer", self._healthy, 20)

    def _healthy(self):
        if self.proc.poll() is not None:
            self.log.seek(0)
            raise AssertionError("agent exited:\n" + self.log.read())
        try:
            return self.call("GET", "health", timeout=1)[0] == 200
        except OSError:
            return False

    def call(self, method, path, body=None, token=TOKEN, timeout=10):
        # netem's jitter reorders TCP segments, which can reset a connection
        # now and then. The cockpit polls again; a reset change must not pass.
        try:
            return self._call(method, path, body, token, timeout)
        except ConnectionResetError:
            RESETS.append(f"{method} {path}")
            if method != "GET":
                raise AssertionError(f"{method} {path}: the agent reset the connection")
            return self._call(method, path, body, token, timeout)

    def _call(self, method, path, body, token, timeout):
        req = urllib.request.Request(
            f"http://{HOST_IP}:{self.port}/v1/{path}", method=method,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Content-Type": "application/json"} | ({"Authorization": "Bearer " + token} if token else {}))
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return r.status, json.load(r)
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                return e.code, json.loads(raw)
            except ValueError:
                return e.code, raw.decode(errors="replace")

    def display(self):
        status, body = self.call("GET", "display")
        assert status == 200, body
        return body

    def start(self, transport="udp", port=PORT, latency_ms=None):
        return self.call("POST", "display/receiver/start",
                         {"transport": transport, "port": port, "latency_ms": latency_ms}, timeout=20)

    def stop(self):
        status, body = self.call("POST", "display/receiver/stop", {})
        assert status == 200, body
        return body

    def sockets(self):
        d = self.work / "run/display"
        return sorted(d.glob("*.sock")) if d.exists() else []

    def mpv(self, *command):
        """Ask the receiver's mpv over its IPC socket (None if unavailable)."""
        socks = self.sockets()
        if not socks:
            return None
        try:
            with socket.socket(socket.AF_UNIX) as s:
                s.settimeout(1)
                s.connect(str(socks[-1]))
                s.sendall(json.dumps({"command": list(command), "request_id": 7}).encode() + b"\n")
                buf = b""
                while True:
                    chunk = s.recv(65536)
                    if not chunk:
                        return None
                    buf += chunk
                    while b"\n" in buf:
                        line, buf = buf.split(b"\n", 1)
                        msg = json.loads(line)
                        if msg.get("request_id") == 7:
                            return msg.get("data") if msg.get("error") == "success" else None
        except (OSError, ValueError):
            return None

    def position(self):
        """The decoded stream's position (its timestamps), in seconds."""
        pos = self.mpv("get_property", "time-pos")
        return pos if isinstance(pos, (int, float)) else None

    def close(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log.close()


def mpv_pids(work):
    """mpv processes started for this run (found by their IPC socket path)."""
    out = sh("pgrep", "-f", f"input-ipc-server={work}", check=False).stdout.split()
    return sorted(int(p) for p in out)


class Sender:
    """ffmpeg on the "Mac", with the cockpit's own arguments."""

    def __init__(self, work, url, width=1280, fps=30, kbps=4000, name="sender"):
        config = {"url": url, "source": "test_pattern", "screen_index": 0, "fps": fps,
                  "bitrate_kbps": kbps, "width": width, "encoder": "software", "show_cursor": True}
        self.args = json.loads(sh(SENDER_ARGS, json.dumps(config)).stdout)
        if os.environ.get("SENDER_LOGLEVEL"):
            self.args[self.args.index("-loglevel") + 1] = os.environ["SENDER_LOGLEVEL"]
        self.log_path = Path(work) / f"{name}-{time.monotonic_ns()}.log"
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(["ffmpeg", *self.args], stdin=subprocess.DEVNULL,
                                     stdout=subprocess.DEVNULL, stderr=self.log)

    def alive(self):
        return self.proc.poll() is None

    def output(self):
        return self.log_path.read_text(errors="replace")

    def stop(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        self.log.close()


class Supervised:
    """A sender the cockpit starts again when it exits on its own
    (`cockpit/src-tauri/src/supervise.rs`: first retry after a second)."""

    def __init__(self, work, url, **kw):
        self.work, self.url, self.kw = work, url, kw
        self.sender, self.restarts, self.done = Sender(work, url, **kw), [], threading.Event()
        self.thread = threading.Thread(target=self._watch, daemon=True)
        self.thread.start()

    def _watch(self):
        while not self.done.wait(0.25):
            if not self.sender.alive():
                self.restarts.append(" | ".join(self.sender.output().strip().splitlines()[-1:]))
                self.sender.stop()
                if self.done.wait(1):
                    return
                self.sender = Sender(self.work, self.url, **self.kw)

    def alive(self):
        return self.sender.alive()

    def output(self):
        return self.sender.output()

    def stop(self):
        self.done.set()
        self.thread.join(5)
        self.sender.stop()


def udp_url(port=PORT):
    return f"udp://{HOST_IP}:{port}?pkt_size=1316"


def srt_url(latency_ms):
    """What the agent hands the Mac (`display::sender_url`, pinned by its test)."""
    return f"srt://{HOST_IP}:{PORT}?mode=caller&latency={latency_ms * 1000}&connect_timeout=10000&pkt_size=1316"


def playing(agent):
    return agent.display().get("playback_active") is True


class Flow:
    """What a viewer sees over a window: how far the picture moved per second
    of wall time (1.0 = real time) and the longest freeze (no new frame)."""

    def __init__(self, speed, freeze):
        self.speed, self.freeze = speed, freeze

    def __str__(self):
        return f"{self.speed:.2f}× real time, longest freeze {self.freeze * 1000:.0f} ms"


def flow_over(agent, seconds, every=0.1):
    t0 = time.monotonic()
    first = last = agent.position()
    changed = t0
    freeze = 0.0
    while time.monotonic() - t0 < seconds:
        time.sleep(every)
        pos, now = agent.position(), time.monotonic()
        if pos is not None and pos != last:
            freeze = max(freeze, now - changed)
            changed, last = now, pos
        if first is None:
            first = pos
    freeze = max(freeze, time.monotonic() - changed)
    if first is None or last is None:
        return Flow(0.0, seconds)
    return Flow((last - first) / (time.monotonic() - t0), freeze)


def smooth(agent, seconds, speed, freeze, what):
    f = flow_over(agent, seconds)
    assert f.speed >= speed and f.freeze <= freeze, f"{what}: {f} (want ≥ {speed}×, freezes ≤ {freeze * 1000:.0f} ms)"
    return f


# --------------------------------------------------------------- scenarios

def api_validation(ctx):
    a = ctx.agent
    status, _ = a.call("GET", "display", token=None)
    assert status == 401, f"display without a token: {status}"
    status, _ = a.call("POST", "display/receiver/start", {"transport": "udp", "port": PORT}, token="wrong-token-0123456789")
    assert status == 401, f"start with a wrong token: {status}"
    for body, why in [({"transport": "udp", "port": 80}, "privileged port"),
                      ({"transport": "udp", "port": 0}, "port 0"),
                      ({"transport": "udp", "port": 70000}, "port out of range"),
                      ({"transport": "tcp", "port": PORT}, "unknown transport"),
                      ({"port": PORT}, "no transport"),
                      ({"transport": "srt", "port": PORT, "latency_ms": -1}, "negative latency"),
                      ({"transport": "srt", "port": PORT, "latency_ms": 600_000}, "absurd SRT latency")]:
        status, body = a.call("POST", "display/receiver/start", body)
        assert 400 <= status < 600 and status != 500, f"{why}: {status} {body}"
        assert not a.display()["receiver"]["running"], f"{why} started a receiver"
    assert mpv_pids(ctx.work) == []
    idle = a.stop()
    assert not idle["receiver"]["running"] and idle["playback_active"] is False
    d = a.display()
    assert d["mpv_installed"] is True and d["receiver_url"] is None, d


def udp_clean(ctx):
    a = ctx.agent
    link(CLEAN)
    status, st = a.start()
    assert status == 200 and st["receiver"]["running"], st
    assert st["playback_active"] is False, "a process alone must not count as playing"
    assert len(mpv_pids(ctx.work)) == 1
    s = ctx.sender(udp_url())
    wait_for("playback", lambda: playing(a), 15)
    ctx.note(f"clean: {smooth(a, 4, 0.9, 0.3, 'clean link')}")
    assert a.mpv("get_property", "video-params/w") == 1280
    s.stop()
    # The Mac stopped sending (or its network dropped): no longer playing.
    wait_for("playback to stop being reported", lambda: not playing(a), 8)
    assert a.display()["receiver"]["running"], "the receiver must keep listening"
    s = ctx.sender(udp_url())
    wait_for("playback after the sender restarted", lambda: playing(a), 15)
    smooth(a, 3, 0.9, 0.3, 'after the sender restarted')
    stopped = a.stop()
    assert not stopped["receiver"]["running"] and stopped["playback_active"] is False
    assert mpv_pids(ctx.work) == [] and a.sockets() == [], "stop left mpv or its socket behind"
    time.sleep(0.5)
    assert s.alive(), "a UDP sender keeps running when the receiver goes away"


def network(name, conditions, speed, freeze, width=1280, kbps=4000):
    def scenario(ctx):
        a = ctx.agent
        link(conditions)
        status, st = a.start()
        assert status == 200 and st["receiver"]["running"], st
        ctx.sender(udp_url(), width=width, kbps=kbps)
        wait_for(f"playback on {name}", lambda: playing(a), 25)
        f = smooth(a, 6, speed, freeze, name)
        d = a.display()
        assert d["receiver"]["running"] and d["playback_active"], d
        ctx.note(f"{name}: {f}")
        a.stop()
    scenario.__name__ = f"udp_{name}"
    return scenario


def outage(ctx):
    a = ctx.agent
    link(WIFI)
    a.start()
    s = ctx.sender(udp_url())
    wait_for("playback", lambda: playing(a), 15)
    link(OUTAGE)
    wait_for("the outage to show as not playing", lambda: not playing_local(a), 10)
    time.sleep(4)
    assert a.mpv("get_property", "pid") is not None or mpv_pids(ctx.work), "mpv died during the outage"
    assert s.alive(), "ffmpeg died during the outage"
    link(WIFI)
    wait_for("playback after the outage", lambda: playing(a), 15)
    smooth(a, 3, 0.85, 0.5, 'after the outage')
    a.stop()


def playing_local(agent):
    """Playback, asked from the server side (the API is unreachable mid-outage)."""
    return agent.display_local().get("playback_active") is True


def srt(ctx):
    a = ctx.agent
    link(CELLULAR)
    status, st = a.start("srt", latency_ms=200)
    assert status == 200 and st["receiver"]["running"], st
    assert st["receiver_config"] == {"transport": "srt", "port": PORT, "latency_ms": 200}, st
    url = srt_url(200)
    ctx.sender(url)
    wait_for("SRT playback over cellular", lambda: playing(a), 25)
    ctx.note(f"SRT on cellular: {smooth(a, 6, 0.9, 0.5, 'SRT on cellular')}")
    link("delay 30ms 5ms loss 5%")
    ctx.note(f"SRT with 5% loss: {smooth(a, 8, 0.9, 0.5, 'SRT retransmitting 5% loss')}")
    # The Mac reconnects (app restarted, network changed): the listener accepts again.
    ctx.stop_senders()
    wait_for("SRT playback to stop", lambda: not playing(a), 10)
    t0 = time.monotonic()
    ctx.sender(url)
    wait_for("SRT playback after the sender reconnected", lambda: playing(a), 25)
    ctx.note(f"reconnected in {time.monotonic() - t0:.1f}s")
    a.stop()
    link(CLEAN)


def srt_reconnects(ctx):
    """The Mac drops and rejoins again and again (sleep, a Wi-Fi roam) on a
    lossy link: every reconnect must play again, quickly. A handshake that
    half-completes under loss ends the sender; the cockpit starts it again."""
    a = ctx.agent
    link("delay 30ms 5ms loss 5%")
    a.start("srt", latency_ms=120)
    url = srt_url(120)
    times, restarts = [], []
    for i in range(8):
        t0 = time.monotonic()
        s = ctx.sender(url, supervised=True)
        try:
            wait_for(f"SRT reconnect {i + 1}", lambda: playing(a), 15)
        except AssertionError:
            tail = " | ".join(s.output().strip().splitlines()[-2:])
            raise AssertionError(f"reconnect {i + 1} never played (sender: {tail}; restarts: {s.restarts}); earlier: {times}")
        times.append(round(time.monotonic() - t0, 1))
        restarts += s.restarts
        ctx.stop_senders()
        wait_for("playback to stop after the sender left", lambda: not playing(a), 10)
    ctx.note(f"reconnects in {times} s, sender restarted {len(restarts)}×")
    a.stop()


def srt_sender_first(ctx):
    """The cockpit starts the receiver and then the sender right away; on a
    slow server mpv may not listen yet. The SRT caller must not give up."""
    a = ctx.agent
    link(CLEAN)
    url = srt_url(120)
    s = ctx.sender(url)
    time.sleep(1.5)
    a.start("srt", latency_ms=120)
    wait_for("SRT playback when the sender started first", lambda: playing(a), 25)
    assert s.alive()
    a.stop()


def replace_and_races(ctx):
    a = ctx.agent
    link(CLEAN)
    a.start()
    first = mpv_pids(ctx.work)
    a.start("srt", latency_ms=120)
    second = mpv_pids(ctx.work)
    assert len(first) == 1 and len(second) == 1 and first != second, (first, second)
    assert a.display()["receiver_config"]["transport"] == "srt"
    assert len(a.sockets()) == 1
    # Many clients (two cockpits, a retrying UI) at once.
    results = []
    def call(i):
        results.append(a.start() if i % 3 else (200, a.stop()))
    threads = [threading.Thread(target=call, args=(i,)) for i in range(12)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(60)
    assert all(code == 200 for code, _ in results), results
    time.sleep(1)
    d = a.display()
    pids = mpv_pids(ctx.work)
    assert len(pids) == (1 if d["receiver"]["running"] else 0), (pids, d["receiver"])
    assert len(a.sockets()) == len(pids)
    a.start()
    ctx.sender(udp_url())
    wait_for("playback after the storm", lambda: playing(a), 15)
    a.stop()
    assert mpv_pids(ctx.work) == [] and a.sockets() == []


def receiver_killed(ctx):
    a = ctx.agent
    link(CLEAN)
    a.start()
    [pid] = mpv_pids(ctx.work)
    os.kill(pid, signal.SIGKILL)
    d = wait_for("the agent to notice mpv died", lambda: (lambda d: d if not d["receiver"]["running"] else None)(a.display()), 5)
    assert d["playback_active"] is False and d["receiver"]["exit_code"] is not None, d
    assert a.sockets() == [], "a dead receiver's socket was left behind"
    status, st = a.start()
    assert status == 200 and st["receiver"]["running"], st
    ctx.sender(udp_url())
    wait_for("playback after restarting a killed receiver", lambda: playing(a), 15)
    a.stop()


def port_taken(ctx):
    """Another program holds the port: say so instead of a receiver that
    runs forever and never plays."""
    a = ctx.agent
    holder = subprocess.Popen(in_host("python3", "-c",
        f"import socket,time; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.bind(('0.0.0.0',{PORT})); print('bound',flush=True); time.sleep(120)"),
        stdout=subprocess.PIPE, text=True)
    try:
        assert holder.stdout.readline().strip() == "bound"
        status, body = a.start()
        running = status == 200 and body["receiver"]["running"]
        assert not running, f"receiver claims to run on a taken port: {status} {body}"
        assert str(PORT) in json.dumps(body), f"the error should name the port: {body}"
        assert mpv_pids(ctx.work) == []
    finally:
        holder.kill()
        holder.wait()
    status, body = a.start()
    assert status == 200 and body["receiver"]["running"], body
    a.stop()


def noise_then_stream(ctx):
    """Junk on the port (a port scan, a stale sender) must not kill mpv."""
    a = ctx.agent
    link(CLEAN)
    a.start()
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        for i in range(3000):
            udp.sendto(os.urandom(1316 if i % 2 else 188), (HOST_IP, PORT))
    time.sleep(1)
    assert a.display()["receiver"]["running"], "noise killed the receiver"
    assert not playing(a), "noise must not count as playback"
    ctx.sender(udp_url())
    wait_for("playback after noise", lambda: playing(a), 15)
    a.stop()


def resolution_change(ctx):
    """Switching quality restarts the sender at another size: mpv follows."""
    a = ctx.agent
    link(WIFI)
    a.start()
    ctx.sender(udp_url(), width=1280)
    wait_for("720p playback", lambda: a.mpv("get_property", "video-params/w") == 1280, 15)
    ctx.stop_senders()
    ctx.sender(udp_url(), width=1920, kbps=12000)
    wait_for("1080p playback after the switch", lambda: a.mpv("get_property", "video-params/w") == 1920, 20)
    assert playing(a)
    smooth(a, 3, 0.85, 0.5, '1080p after the switch')
    ctx.stop_senders()
    ctx.sender(udp_url(), width=640, fps=15, kbps=1000)
    wait_for("640 px playback after the switch", lambda: a.mpv("get_property", "video-params/w") == 640, 20)
    a.stop()


def sender_validation(ctx):
    """The cockpit's arguments reject what ffmpeg would choke on."""
    def args(**over):
        config = {"url": udp_url(), "source": "test_pattern", "screen_index": 0, "fps": 30,
                  "bitrate_kbps": 4000, "width": 1280, "encoder": "software", "show_cursor": True} | over
        return sh(SENDER_ARGS, json.dumps(config), check=False)
    for over, why in [({"url": "file:///etc/passwd"}, "file URL"), ({"url": "udp://x:1 -y /tmp/o"}, "spaces"),
                      ({"fps": 0}, "0 fps"), ({"fps": 500}, "500 fps"), ({"bitrate_kbps": 10}, "tiny bitrate"),
                      ({"width": 100}, "tiny width"), ({"width": 1365}, "odd width")]:
        r = args(**over)
        assert r.returncode != 0, f"{why} was accepted: {r.stdout}"
    # Every accepted extreme must really encode.
    for over in [{"width": 320}, {"width": 3840, "bitrate_kbps": 20000}, {"fps": 1}, {"fps": 120, "width": 640},
                 {"width": None}, {"bitrate_kbps": 500, "width": 640}]:
        out = Path(ctx.work) / "probe.ts"
        a = json.loads(args(**over, url=f"udp://127.0.0.1:{PORT + 9}?pkt_size=1316").stdout)
        a = [x for x in a if x != "-re"]
        a[-1] = str(out)
        r = subprocess.run(["ffmpeg", "-y", *a[:a.index("-f", a.index("-i"))], "-frames:v", "10",
                            *a[a.index("-f", a.index("-i")):]], capture_output=True, text=True, timeout=60)
        assert r.returncode == 0 and out.stat().st_size > 1316, f"{over}: {r.stderr[-400:]}"


def mpv_missing(ctx):
    b = Agent(ctx.work, "no-mpv", port=API_PORT + 1, VERCELIFY_MPV_BIN="/nonexistent/mpv")
    try:
        assert b.display()["mpv_installed"] is False
        status, body = b.start()
        assert status == 503 and "apt install mpv" in json.dumps(body), (status, body)
    finally:
        b.close()


def mpv_fails_fast(ctx):
    """mpv refusing to start (DRM busy, a broken GPU driver): the reason reaches the UI."""
    stub = Path(ctx.work) / "broken-mpv"
    stub.write_text("#!/bin/sh\necho 'Failed to open DRM device: Permission denied' >&2\n"
                    "echo 'Video: no video' \nexit 2\n")
    stub.chmod(0o755)
    b = Agent(ctx.work, "broken", port=API_PORT + 2, VERCELIFY_MPV_BIN=str(stub))
    try:
        for attempt in range(5):
            status, body = b.start()
            assert status == 200, body
            r = body["receiver"]
            assert not r["running"], f"attempt {attempt}: {r}"
            assert r["exit_code"] == 2, f"attempt {attempt}: {r}"
            assert any("DRM" in line for line in r["last_output"]), f"attempt {attempt}: output lost: {r}"
            assert body["playback_active"] is False
        assert b.sockets() == []
    finally:
        b.close()


def agent_shutdown(ctx):
    """Stopping the agent (update, reboot) takes the receiver with it."""
    b = Agent(ctx.work, "shutdown", port=API_PORT + 3)
    b.start(port=PORT + 3)
    assert len(mpv_pids(b.work)) == 1
    b.close()
    wait_for("mpv to exit with the agent", lambda: mpv_pids(b.work) == [], 5)
    assert b.sockets() == []


SCENARIOS = [api_validation, sender_validation, udp_clean,
             # Lost UDP packets are not resent: a damaged keyframe can hold the
             # picture until the next one (the sender's -g is two seconds).
             network("wifi", WIFI, 0.9, KEYFRAME_FREEZE), network("lossy", LOSSY, 0.8, KEYFRAME_FREEZE),
             network("cellular", CELLULAR, 0.85, 0.6, width=1280, kbps=4000),
             outage, srt, srt_reconnects, srt_sender_first, replace_and_races, receiver_killed, port_taken,
             noise_then_stream, resolution_change, mpv_missing, mpv_fails_fast, agent_shutdown]


class Context:
    def __init__(self, work, agent):
        self.work, self.agent, self.senders, self.notes = work, agent, [], []

    def sender(self, url, supervised=False, **kw):
        s = (Supervised if supervised else Sender)(self.work, url, **kw)
        self.senders.append(s)
        return s

    def stop_senders(self):
        for s in self.senders:
            s.stop()
        self.senders.clear()

    def note(self, text):
        self.notes.append(text)


def setup_network():
    for ns in (NS_MAC, NS_HOST):
        sh("ip", "netns", "del", ns, check=False)
        sh("ip", "netns", "add", ns)
        sh("ip", "netns", "exec", ns, "ip", "link", "set", "lo", "up")
    sh("ip", "link", "add", "vfys-a", "type", "veth", "peer", "name", "vfys-b")
    for dev, ns, ip in (("vfys-a", NS_MAC, MAC_IP), ("vfys-b", NS_HOST, HOST_IP)):
        sh("ip", "link", "set", dev, "netns", ns)
        sh("ip", "netns", "exec", ns, "ip", "link", "set", dev, "name", "eth0")
        sh("ip", "netns", "exec", ns, "ip", "addr", "add", f"{ip}/24", "dev", "eth0")
        sh("ip", "netns", "exec", ns, "ip", "link", "set", "eth0", "up")
    # This script is the Mac from here on.
    with open(f"/run/netns/{NS_MAC}") as f:
        os.setns(f.fileno(), os.CLONE_NEWNET)


def main():
    if os.geteuid() != 0:
        sys.exit("run as root (network namespaces and tc)")
    for tool in ("ip", "tc", "mpv", "ffmpeg"):
        if not shutil.which(tool):
            sys.exit(f"missing {tool}")
    for b in (AGENT, SENDER_ARGS):
        if not os.access(b, os.X_OK):
            sys.exit(f"missing {b} (see the build line at the top of this file)")
    only = set(filter(None, os.environ.get("ONLY", "").split(",")))
    work = tempfile.mkdtemp(prefix="vfy-screencast-")
    os.chmod(work, 0o755)
    setup_network()
    agent = Agent(work)
    # Mid-outage the API is unreachable from the "Mac": ask on the server.
    agent.display_local = lambda: json.loads(sh(*in_host(
        "curl", "-sf", "-H", f"Authorization: Bearer {TOKEN}", f"http://127.0.0.1:{API_PORT}/v1/display")).stdout)
    failed = []
    try:
        for scenario in SCENARIOS:
            if only and scenario.__name__ not in only:
                continue
            ctx = Context(work, agent)
            t0 = time.monotonic()
            try:
                scenario(ctx)
                notes = f" — {'; '.join(ctx.notes)}" if ctx.notes else ""
                print(f"  \033[32m✓\033[0m {scenario.__name__} ({time.monotonic() - t0:.1f}s){notes}", flush=True)
            except Exception as e:  # report every scenario, then fail
                failed.append(scenario.__name__)
                print(f"  \033[31m✗\033[0m {scenario.__name__}: {e}", flush=True)
                if os.environ.get("VERBOSE"):
                    traceback.print_exc()
                for s in ctx.senders:
                    tail = s.output().strip().splitlines()[-3:]
                    if tail:
                        print("      sender: " + " | ".join(tail))
                out = agent.display_local()["receiver"]["last_output"][-4:]
                if out:
                    print("      receiver: " + " | ".join(out))
            finally:
                ctx.stop_senders()
                link(CLEAN)
                try:
                    agent.stop()
                except Exception:
                    pass
    finally:
        agent.close()
        for ns in (NS_MAC, NS_HOST):
            sh("ip", "netns", "del", ns, check=False)
        if os.environ.get("KEEP") == "1":
            print(f"kept {work}")
        else:
            shutil.rmtree(work, ignore_errors=True)
    total = len([s for s in SCENARIOS if not only or s.__name__ in only])
    if RESETS:
        print(f"connections reset (and retried if GET): {', '.join(RESETS)}")
    print(f"{total - len(failed)}/{total} scenarios passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
