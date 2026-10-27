#!/usr/bin/env python3
"""Stateful mock of the vercelify-agent API for the CLI tests.

Usage: mock_agent.py PORT_FILE LOG_FILE
Background jobs (install, pull) finish after two status polls.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

TOKEN = "test-agent-token"
state = {"installed": False, "models": [], "tasks": [], "sleep": [], "sources": []}
APPS = [{"id": "app1", "name": "blog"}, {"id": "app2", "name": "shop"}]


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
        "endpoints": ["http://10.77.0.1:11434"],
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
        if method == "PUT" and p == "/v1/platform/settings/domain":
            return self.reply(200, body)
        if method == "GET" and p == "/v1/platform/git-sources":
            return self.reply(200, state["sources"])
        if method == "POST" and p == "/v1/platform/git-sources":
            if body.get("auth") == "token" and not body.get("token"):
                return self.reply(400, {"error": "a token is needed"})
            g = {"id": f"git_{len(state['sources']) + 1}", "name": body["name"], "provider": body["provider"], "auth": body["auth"],
                 "public_key": "ssh-ed25519 AAAAdeploykey vercelify" if body["auth"] == "deploy_key" else "",
                 "created_at": "2026-10-27T00:00:00Z", "webhook": False}
            state["sources"].append(g)
            return self.reply(200, g)
        if method == "DELETE" and p.startswith("/v1/platform/git-sources/"):
            sid = p.split("/")[4]
            state["sources"] = [g for g in state["sources"] if g["id"] != sid]
            return self.reply(200, {"deleted": True})
        if method == "GET" and p == "/v1/platform/apps":
            return self.reply(200, APPS)
        if method == "GET" and p.startswith("/v1/platform/apps/"):
            want = p.split("/")[4]
            for a in APPS:
                if want in (a["id"], a["name"]):
                    return self.reply(200, a)
            return self.reply(404, {"error": f"no app named {want}"})
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
                {"id": "mn_3", "name": "cabin", "kind": "server", "role": "owner", "ipv4": "10.77.0.3", "ipv6": "fd76:6679:6d65::3",
                 "public_key": "k3", "endpoints": ["203.0.113.23:4123"], "os": "linux", "arch": "aarch64", "coordinator": False,
                 "last_seen_at": "2026-10-25T11:00:00Z", "cert_expires_at": None, "revoked_at": None, "created_at": "t",
                 "relay_connected": True, "relayed_bytes": 52224},
            ])
        if method == "POST" and p == "/v1/platform/github-app":
            org = body.get("org")
            return self.reply(200, {"start_url": f"https://mesh.example.com/v1/github-app/start?state=s{'-' + org if org else ''}",
                                    "state": "s", "expires_at": "2026-10-25T12:30:00Z"})
        if method == "POST" and p.startswith("/v1/platform/git-sources/") and p.endswith("/webhook"):
            src = p.split("/")[4]
            if src != "github":
                return self.reply(404, {"error": f"git source {src} not found"})
            return self.reply(200, {"url": "https://mesh.example.com/v1/webhooks/github/git_1", "secret": "S" * 40,
                                    "lan_urls": [], "content_type": "application/json", "events": ["push", "pull_request"]})
        if p == "/v1/platform/settings/standby":
            if method == "PUT":
                node = body.get("node")
                if node not in (None, "", "pi5"):
                    return self.reply(404, {"error": f"server {node} not found"})
                state["standby"] = {"node_id": "node_pi5" if node else None, "node": node or None,
                                    "last_sent_at": None, "bytes": 0, "last_error": None}
            st = state.get("standby") or {"node_id": None, "node": None, "last_sent_at": None, "bytes": 0, "last_error": None}
            if st["node"] and method == "GET":
                st = dict(st, last_sent_at="2026-10-25T11:55:00Z", bytes=204800)
            return self.reply(200, st)
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
