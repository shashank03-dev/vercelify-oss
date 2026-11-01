#!/usr/bin/env python3
"""Validate review findings against an isolated real agent and installer functions.

No installed services, user settings, real updates or real credentials are used.
The updater runs the real installer summary through a stub systemd launcher.
"""
import base64
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[3]
OUT = Path(__file__).with_name("host-results.json")
results = []


def wait_for(fn, seconds=15):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            value = fn()
            if value:
                return value
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    raise AssertionError("condition timed out")


with tempfile.TemporaryDirectory(prefix="vercelify-findings-") as raw:
    work = Path(raw)
    for directory in ("etc", "state", "run", "sys/class/power_supply", "storage", "bin"):
        (work / directory).mkdir(parents=True, exist_ok=True)
    def put(name, content):
        target = work / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)
        return target
    for name, value in {
        "AC/type": "Mains", "AC/online": "1", "BAT0/type": "Battery",
        "BAT0/present": "1", "BAT0/status": "Full", "BAT0/capacity": "100",
        "BAT0/energy_full": "1480000", "BAT0/energy_full_design": "42285714",
    }.items():
        put("sys/class/power_supply/" + name, value)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    stubs = ROOT / "agent/tests/stubs"
    binary = ROOT / "target/debug/vercelify-agent"
    assert binary.is_file(), "Build vercelify-agent first"
    cfg = put("etc/agent.toml", 'engine = "none"\nwake_listen = ""\n')
    cli = put("bin/vercelify", """#!/usr/bin/env bash
export VERCELIFY_SOURCE_ONLY=1
. "$REVIEW_ROOT/host/setup-host.sh"
ROLE=main
WITH_AI=0
NEEDS_REBOOT=0
mesh_ipv4() { echo 127.0.0.1; }
platform_engine() { return 1; }
summary
""")
    cli.chmod(0o755)
    token = "isolated-validation-admin-token-0123456789"
    env = {
        **os.environ, "REVIEW_ROOT": str(ROOT), "NO_COLOR": "1",
        "VERCELIFY_LISTEN": "127.0.0.1", "VERCELIFY_PORT": str(port),
        "VERCELIFY_TOKEN": token, "VERCELIFY_ENGINE": "none",
        "VERCELIFY_WATCH_INTERVAL": "0", "VERCELIFY_AUTODEPLOY_INTERVAL": "0",
        "VERCELIFY_WAKE_LISTEN": "", "VERCELIFY_ROLE": "main",
        "VERCELIFY_SYS_CLASS": str(work / "sys/class"),
        "VERCELIFY_SYS_BLOCK": str(work / "sys/block"),
        "VERCELIFY_STATE_DIR": str(work / "state"), "VERCELIFY_RUN_DIR": str(work / "run"),
        "VERCELIFY_ETC": str(work / "etc"), "VERCELIFY_LOG": str(work / "setup.log"),
        "VERCELIFY_TOKENS_FILE": str(work / "tokens.json"),
        "VERCELIFY_AUDIT_LOG": str(work / "state/audit.log"),
        "VERCELIFY_INSTALL_ENV": str(work / "etc/install.env"),
        "VERCELIFY_BACKUP_ENV": str(work / "etc/backup.env"),
        "VERCELIFY_MESH_DIR": str(work / "mesh"),
        "VERCELIFY_PLATFORM_DB": str(work / "state/platform.db"),
        "VERCELIFY_MASTER_KEY_FILE": str(work / "state/master.key"),
        "VERCELIFY_PROXY_DIR": str(work / "state/proxy"),
        "VERCELIFY_CLI_BIN": str(cli), "AGENT_BIN": str(binary),
        "AGENT_LOCAL_URL": f"http://127.0.0.1:{port}",
        "VERCELIFY_SYSTEMD_RUN_BIN": str(stubs / "systemd-run"),
        "VERCELIFY_SYSTEMCTL_BIN": str(stubs / "systemctl"),
        "VERCELIFY_DOCKER_BIN": str(stubs / "docker"),
        "VERCELIFY_REQUIRE_STORAGE_MOUNT": "true",
        "VERCELIFY_STORAGE_MOUNT": str(work / "storage"),
        "VERCELIFY_HDD_VOLUME_ROOT": str(work / "storage/volumes"),
        "VERCELIFY_FSTAB": str(work / "fstab"),
        "VERCELIFY_POWER_GUARD_STATE": str(work / "power-guard.state"),
        "VERCELIFY_POWER_GUARD_CONF": str(work / "power-guard.conf"),
        "VERCELIFY_POWER_GUARD_OVERRIDES": str(work / "power-guard.local.conf"),
        "VERCELIFY_SYSTEMD_DIR": str(work / "systemd"),
        "VERCELIFY_MAINTENANCE_CONF": str(work / "maintenance.conf"),
        "VERCELIFY_LOGIND_DROPIN": str(work / "logind.conf"),
        "STUB_LOG": str(work / "calls.log"),
    }
    def request(path, method="GET", body=None, credential=token):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=data, method=method,
            headers={"Authorization": f"Bearer {credential}", "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=20) as response:
            return json.load(response)
    with (work / "agent.log").open("w") as log:
        process = subprocess.Popen([str(binary), "--config", str(cfg), "serve"],
            env=env, stdout=log, stderr=log, umask=0o022)
        try:
            wait_for(lambda: request("/v1/health"))
            modes = []
            for existing in (False, True):
                update_log = work / "state/update.log"
                if existing:
                    update_log.write_text("previous update\n")
                    update_log.chmod(0o644)
                else:
                    update_log.unlink(missing_ok=True)
                request("/v1/update", "POST", {})
                wait_for(lambda: (work / "state/update.exit").exists())
                contents = update_log.read_text()
                codes = re.findall(r"vfy[12]\.([A-Za-z0-9_-]+)", contents)
                assert codes, "real installer summary did not issue a pairing"
                payload = json.loads(base64.urlsafe_b64decode(codes[0] + "=" * (-len(codes[0]) % 4)))
                accepted = request("/v1/system", credential=payload["agent_token"])
                mode = oct(update_log.stat().st_mode & 0o777)
                assert mode == "0o644" and "host" in accepted
                modes.append({"preexisting_log": existing, "mode": mode,
                    "pairing_credential_accepted_by_real_agent": True})
            results.append({"finding": "F1", "evidence": "real update endpoint + real installer summary; systemd launcher stubbed",
                "observed": modes, "secret_values_saved": False})

            system = request("/v1/system")
            doctor = request("/v1/doctor?refresh=true")
            battery = system["power"]["batteries"][0]
            assert battery["health_pct"] == 3.5
            worn = [c for c in doctor["checks"] if re.search(r"wear|design capacity|worn|battery health", json.dumps(c), re.I)]
            assert not worn
            results.append({"finding": "F18", "evidence": "real collector/doctor with synthetic sysfs battery",
                "observed": {"charge_pct": battery["capacity_pct"], "health_pct": battery["health_pct"],
                    "battery_wear_warnings": worn}, "physical_power_loss_tested": False})

            # The real HDD endpoint should reject a directory that is not mounted.
            try:
                request("/v1/volumes", "POST", {"service": "review", "name": "data", "tier": "hdd", "uid": None, "gid": None})
                raise AssertionError("unmounted HDD unexpectedly accepted")
            except urllib.error.HTTPError as error:
                text = error.read().decode()
                assert error.code >= 400 and "mount" in text.lower(), text
                results.append({"finding": "F10", "evidence": "real volume creation endpoint, unmounted temp directory",
                    "observed": {"status": error.code, "message": json.loads(text)}})
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    # Database creation is a different storage path from the volume assistant.
    # Run the actual engine with Docker stubbed; no real containers are touched.
    with (work / "platform-agent.log").open("w") as log:
        process = subprocess.Popen([str(binary), "--config", str(cfg), "serve"],
            env={**env, "VERCELIFY_ENGINE": "platform"}, stdout=log, stderr=log)
        try:
            wait_for(lambda: "platform" in request("/v1/health")["capabilities"])
            database = request("/v1/platform/databases", "POST", {"name": "validation-hdd", "kind": "redis", "tier": "hdd"})
            volume = next(v["volume"] for v in request("/v1/platform/volumes") if v["owner_name"] == "validation-hdd")
            folder = work / "storage/volumes" / volume["storage_name"]
            wait_for(lambda: folder.is_dir())
            assert folder.stat().st_dev == (work / "storage").stat().st_dev
            results.append({"finding": "F10", "evidence": "real database API/engine with Docker stubbed; unmounted storage directory",
                "observed": {"databaseCreationAccepted": True, "recordedTier": volume["tier"],
                    "createdBindDirectoryOnParentFilesystem": True},
                "correction": "Volume assistant is protected by a mount guard; initial database creation is not. Actual database container was not run."})
            sentinel = folder / "validation-sentinel.txt"
            sentinel.write_text("synthetic retained database data")
            deleted = request("/v1/platform/databases/" + database["id"], "DELETE")
            remaining = request("/v1/platform/volumes")
            assert deleted["data_kept"] and sentinel.exists()
            assert all(v["volume"]["id"] != volume["id"] for v in remaining)
            results.append({"finding": "F11", "evidence": "real database deletion API/SQLite metadata; synthetic on-disk sentinel; Docker stubbed",
                "observed": {"dataKept": deleted["data_kept"], "retainedDataStillOnDisk": sentinel.exists(),
                    "retainedVolumeStillInPlatformList": False}})
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    # Source installer functions without running installation or package commands.
    def shell(source):
        return subprocess.run(["bash", "-c", source], env={**env, "VERCELIFY_SOURCE_ONLY": "1"},
            capture_output=True, text=True, check=True).stdout.strip()
    backup = shell('. "$REVIEW_ROOT/host/setup-host.sh"; STORAGE_MOUNT="$VERCELIFY_STORAGE_MOUNT"; default_backup_repo')
    assert backup == "/var/backups/vercelify-restic"
    results.append({"finding": "F7", "evidence": "actual default_backup_repo function, no mounted storage disk",
        "observed": {"repository": backup}, "real_backup_or_restore_tested": False})
    defaults = shell('. "$REVIEW_ROOT/host/setup-host.sh"; parse_args; printf "remote=%s configure=%s storage=%s\\n" "$REMOTE" "$CONFIGURE" "$STORAGE_DEVICE"')
    results.append({"finding": "F13", "evidence": "actual installer defaults/parse_args", "observed": defaults})

OUT.write_text(json.dumps(results, indent=2) + "\n")
print(json.dumps(results, indent=2))
