#!/usr/bin/env python3
"""Stateful mock of the vercelify-agent API for the CLI tests.

Usage: mock_agent.py PORT_FILE LOG_FILE
Background jobs (install, pull) finish after two status polls.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

TOKEN = "test-agent-token"
state = {"installed": False, "models": [], "tasks": [], "watches": [], "sleep": []}


def task_tick():
    for t in state["tasks"]:
        if t["done"]:
            continue
        t["polls"] += 1
        if t["polls"] >= 2:
            t["done"] = True
            if t["target"].startswith("bad"):
                t["status"], t["error"] = "failed", "pull model manifest: file does not exist"
            else:
                t["status"], t["completed"] = "success", t["total"]
                if t["kind"] == "install":
                    state["installed"] = True
                else:
                    state["models"].append({"name": t["target"], "size_bytes": 2000000000, "parameter_size": "3.2B"})
        else:
            t["status"], t["completed"] = "pulling", (t["total"] or 0) // 2


def llm_status(tick=True):
    if tick:
        task_tick()
    return {
        "installed": state["installed"],
        "running": state["installed"],
        "ready": state["installed"],
        "image": "ollama/ollama:latest",
        "version": "0.12.3" if state["installed"] else None,
        "endpoints": ["http://laptop.tail1234.ts.net:11434"],
        "models": state["models"],
        "loaded": [],
        "tasks": [{k: v for k, v in t.items() if k != "polls"} for t in state["tasks"]],
    }


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def handle_any(self, method):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length).decode() if length else ""
        if self.path == "/v1/health":
            return self.reply(200, {"ok": True, "version": "0.2.0", "hostname": "laptop", "role": "main"})
        with open(sys.argv[2], "a") as f:
            f.write(f"{method} {self.path} auth={self.headers.get('Authorization')} {raw}\n")
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            return self.reply(401, {"error": "missing or invalid agent token"})
        body = json.loads(raw) if raw else {}
        p = self.path
        if method == "GET" and p.startswith("/v1/doctor"):
            return self.reply(200, {"role": "main", "agent_version": "0.2.0", "checks": [
                {"id": "docker", "label": "Docker", "status": "ok", "detail": "Docker 27.3.1", "fix": None},
                {"id": "sd-card", "label": "System disk type", "status": "warn", "detail": "running from an SD card", "fix": "boot from a USB SSD"},
                {"id": "memory-limits", "label": "Memory limits", "status": "bad", "detail": "ignored", "fix": "re-run the installer, then reboot"},
            ]})
        if method == "GET" and p == "/v1/llm":
            return self.reply(200, llm_status())
        if p == "/v1/llm/install":
            state["tasks"].append({"kind": "install", "target": "ollama/ollama:latest", "status": "downloading", "completed": None, "total": None, "done": False, "error": None, "polls": 0})
            return self.reply(200, llm_status(tick=False))
        if p == "/v1/llm/models/pull":
            if not state["installed"]:
                return self.reply(503, {"error": "Ollama is not running on this server — install or start it first"})
            state["tasks"].append({"kind": "pull", "target": body["model"], "status": "pulling manifest", "completed": 0, "total": 2000000000, "done": False, "error": None, "polls": 0})
            return self.reply(200, llm_status(tick=False))
        if p == "/v1/llm/models/delete":
            names = [m["name"] for m in state["models"]]
            if body["model"] not in names:
                return self.reply(404, {"error": f"{body['model']} is not downloaded"})
            state["models"] = [m for m in state["models"] if m["name"] != body["model"]]
            return self.reply(200, {"ok": True})
        if p == "/v1/llm/chat":
            return self.reply(200, {"message": {"role": "assistant", "content": "Hello from " + body["model"]}, "done": True})
        if p in ("/v1/llm/start", "/v1/llm/stop", "/v1/llm/restart"):
            return self.reply(200, llm_status())
        if method == "GET" and p == "/v1/autodeploy":
            return self.reply(200, {"interval_secs": 60, "coolify_configured": True, "watches": state["watches"]})
        if method == "POST" and p == "/v1/autodeploy":
            if body.get("branch", "").startswith("-"):
                return self.reply(400, {"error": f"`{body['branch']}` is not a valid branch name"})
            w = {"app_uuid": body["app_uuid"], "name": body.get("name") or body["app_uuid"], "repo": body["repo"], "branch": body["branch"],
                 "enabled": True, "has_token": bool(body.get("token")), "last_commit": None, "last_checked_at": None, "last_error": None, "deploys": 0}
            state["watches"] = [x for x in state["watches"] if x["app_uuid"] != w["app_uuid"]] + [w]
            return self.reply(200, w)
        if method == "POST" and p.startswith("/v1/autodeploy/") and p.endswith("/check"):
            uuid = p.split("/")[3]
            for w in state["watches"]:
                if w["app_uuid"] == uuid:
                    w["last_commit"], w["last_checked_at"] = "1" * 40, "2026-10-19T10:00:00Z"
                    return self.reply(200, w)
            return self.reply(404, {"error": "no auto-deploy for this application"})
        if method == "DELETE" and p.startswith("/v1/autodeploy/"):
            uuid = p.split("/")[3]
            if not any(w["app_uuid"] == uuid for w in state["watches"]):
                return self.reply(404, {"error": "no auto-deploy for this application"})
            state["watches"] = [w for w in state["watches"] if w["app_uuid"] != uuid]
            return self.reply(200, {"ok": True})
        if method == "GET" and p == "/v1/sleep":
            targets = [{"key": s["app_uuid"], "app_uuid": s["app_uuid"], "pull_request_id": 0, "hosts": ["blog.example.com"],
                        "state": "asleep", "last_request_at": None, "asleep_since": None} for s in state["sleep"]]
            return self.reply(200, {"proxy_metrics": True, "settings": state["sleep"], "targets": targets, "error": None})
        if method == "PUT" and p.startswith("/v1/sleep/"):
            uuid = p.split("/")[3]
            if body.get("idle_minutes", 30) < 5:
                return self.reply(400, {"error": "idle time must be 5 minutes to 24 hours"})
            state["sleep"] = [s for s in state["sleep"] if s["app_uuid"] != uuid]
            if body["enabled"]:
                state["sleep"].append({"app_uuid": uuid, "enabled": True, "idle_minutes": body.get("idle_minutes", 30), "previews": body.get("previews", False)})
            return self.reply(200, {"proxy_metrics": True, "settings": state["sleep"], "targets": [], "error": None})
        if method == "POST" and p == "/v1/mesh/invites":
            if body.get("role") not in (None, "owner", "deployer", "viewer"):
                return self.reply(400, {"error": f"unknown role `{body['role']}`"})
            return self.reply(200, {"token": f"vfyj1.{body.get('kind', 'server')}-{body.get('role', 'owner')}-{body.get('ttl_minutes', 60)}",
                                    "expires_at": "2026-10-25T13:00:00Z"})
        if method == "GET" and p == "/v1/mesh/nodes":
            return self.reply(200, [
                {"id": "mn_1", "name": "main", "kind": "server", "role": "owner", "ipv4": "10.77.0.1", "ipv6": "fd76:6679:6d65::1",
                 "public_key": "k1", "endpoints": ["192.168.1.20:51820"], "os": "linux", "arch": "x86_64", "coordinator": True,
                 "last_seen_at": "2026-10-25T11:00:00Z", "cert_expires_at": None, "revoked_at": None, "created_at": "t"},
                {"id": "mn_2", "name": "pi5", "kind": "server", "role": "owner", "ipv4": "10.77.0.2", "ipv6": "fd76:6679:6d65::2",
                 "public_key": "k2", "endpoints": [], "os": "linux", "arch": "aarch64", "coordinator": False,
                 "last_seen_at": None, "cert_expires_at": None, "revoked_at": "2026-10-25T10:00:00Z", "created_at": "t"},
            ])
        if method == "DELETE" and p.startswith("/v1/mesh/nodes/"):
            name = p.split("/")[4]
            if name not in ("pi5", "mn_2"):
                return self.reply(404, {"error": f"mesh node {name} not found"})
            return self.reply(200, {"revoked": "pi5"})
        return self.reply(404, {"error": "not found"})

    def do_PUT(self):
        self.handle_any("PUT")

    def do_GET(self):
        self.handle_any("GET")

    def do_POST(self):
        self.handle_any("POST")

    def do_DELETE(self):
        self.handle_any("DELETE")


srv = HTTPServer(("127.0.0.1", 0), H)
with open(sys.argv[1], "w") as f:
    f.write(str(srv.server_address[1]))
srv.serve_forever()
