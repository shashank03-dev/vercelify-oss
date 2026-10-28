#!/usr/bin/env python3
"""Decode real MPEG-TS packets through the agent's mpv and verify playback IPC.

AGENT_BIN=/path/to/vercelify-agent STREAM_FIXTURE=/path/to/pattern.ts python3 display.py
Generate the fixture with ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30
-t 4 -c:v libx264 -preset ultrafast -f mpegts pattern.ts. No GPU is needed.
The agent has its own temporary state, no engine, and a null video sink.
As root, set DISPLAY_TEST_USER to test automatic X11 session detection and
unprivileged mpv rendering under Xvfb (requires xauth and Xvfb).
"""
import json
import os
import pwd
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
from pathlib import Path


def main():
    fixture = Path(os.environ["STREAM_FIXTURE"]).read_bytes()
    assert len(fixture) > 1316, "fixture must contain MPEG-TS packets"
    with tempfile.TemporaryDirectory(prefix="vfy-display-") as work:
        env = os.environ | {
            "VERCELIFY_CONFIG": "/nonexistent", "VERCELIFY_ENGINE": "none",
            "VERCELIFY_LISTEN": "127.0.0.1", "VERCELIFY_PORT": "17425",
            "VERCELIFY_TOKEN": "display-contract-token-0123456789",
            "VERCELIFY_STATE_DIR": work, "VERCELIFY_RUN_DIR": work + "/run",
            "VERCELIFY_MESH_DIR": work + "/mesh", "VERCELIFY_INSTALL_ENV": work + "/install.env",
            "VERCELIFY_TOKENS_FILE": work + "/tokens.json", "VERCELIFY_AUDIT_LOG": work + "/audit.log",
            "VERCELIFY_REQUIRE_STORAGE_MOUNT": "false", "VERCELIFY_MPV_EXTRA_ARGS": "--vo=null",
        }
        # Optional root→desktop integration: use real Xvfb/mpv and a real
        # unprivileged user, with isolated logind/user-manager responses.
        # The fixture does not require a physical monitor or a graphical login.
        xvfb = None
        user = os.environ.get("DISPLAY_TEST_USER")
        if user:
            account = pwd.getpwnam(user)
            os.chmod(work, 0o755)
            auth = work + "/Xauthority"
            subprocess.run(["xauth", "-f", auth, "add", ":92", ".", secrets.token_hex(16)], check=True, capture_output=True)
            os.chown(auth, account.pw_uid, account.pw_gid)
            xvfb = subprocess.Popen(["runuser", "-u", user, "--", "Xvfb", ":92", "-screen", "0", "1280x720x24", "-auth", auth], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            stubs = Path(work) / "bin"
            stubs.mkdir()
            (stubs / "loginctl").write_text("#!/bin/sh\nif [ \"$1\" = list-sessions ]; then echo '1'; else printf 'Active=yes\\nRemote=no\\nType=x11\\nName=" + user + "\\nUser=" + str(account.pw_uid) + "\\nDisplay=:92\\n'; fi\n")
            (stubs / "systemctl").write_text("#!/bin/sh\nprintf '%s\\n' 'DISPLAY=:92' 'XAUTHORITY=" + auth + "' 'HOME=" + account.pw_dir + "'\n")
            for command in stubs.iterdir():
                command.chmod(0o755)
            env["PATH"] = str(stubs) + ":" + env["PATH"]
            env["VERCELIFY_SYSTEMCTL_BIN"] = str(stubs / "systemctl")
            env["VERCELIFY_MPV_EXTRA_ARGS"] = ""
            for _ in range(40):
                if Path("/tmp/.X11-unix/X92").exists():
                    break
                time.sleep(.1)
        with open(work + "/agent.log", "w+") as log:
            agent = subprocess.Popen([os.environ["AGENT_BIN"], "--config", "/nonexistent"], env=env, stdout=log, stderr=log)
            def request(path, body=None):
                req = urllib.request.Request("http://127.0.0.1:17425/v1/" + path,
                    data=json.dumps(body).encode() if body is not None else None,
                    headers={"Authorization": "Bearer " + env["VERCELIFY_TOKEN"], "Content-Type": "application/json"})
                with urllib.request.urlopen(req, timeout=4) as response:
                    return json.load(response)
            try:
                for _ in range(80):
                    try:
                        request("health")
                        break
                    except (OSError, TimeoutError):
                        if agent.poll() is not None:
                            log.seek(0)
                            raise AssertionError(log.read())
                        time.sleep(.1)
                assert not request("display")["playback_active"]
                for iteration in range(2):
                    status = request("display/receiver/start", {"transport": "udp", "port": 5608, "latency_ms": None})
                    assert status["receiver"]["running"], status
                    assert not status["playback_active"], "process alone must not confirm playback"
                    done = threading.Event()
                    def send():
                        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
                            while not done.is_set():
                                for offset in range(0, len(fixture), 1316):
                                    if done.is_set():
                                        return
                                    udp.sendto(fixture[offset:offset + 1316], ("127.0.0.1", 5608))
                                    time.sleep(.002)
                    sender = threading.Thread(target=send)
                    sender.start()
                    try:
                        for _ in range(100):
                            status = request("display")
                            if status["playback_active"]:
                                break
                            time.sleep(.1)
                        assert status["playback_active"], status
                    finally:
                        done.set()
                        sender.join(timeout=2)
                    status = request("display/receiver/stop", {})
                    assert not status["receiver"]["running"] and not status["playback_active"]
                    print(f"PASS receiver start → decode → playback confirmed → stop (run {iteration + 1})")
            finally:
                agent.terminate()
                agent.wait(timeout=10)
                if xvfb is not None:
                    xvfb.terminate()
                    xvfb.wait(timeout=10)


if __name__ == "__main__":
    main()
