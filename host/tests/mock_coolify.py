#!/usr/bin/env python3
"""Tiny Coolify + agent API mock for `vfy` (the Mac CLI) tests.

Usage: mock_coolify.py PORT_FILE LOG_FILE
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

APPS = [
    {"uuid": "a1", "name": "blog", "status": "running:healthy", "fqdn": "https://blog.example.com"},
    {"uuid": "a2", "name": "api", "status": "exited:unhealthy", "fqdn": None},
]


PROJECT = {
    "key": "a1", "app_uuid": "a1", "pull_request_id": 0, "source": "repo", "files": ["vercel.json"],
    "commit": "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432", "updated_at": "2026-10-22T08:00:00Z",
    "summary": {"redirects": 2, "rewrites": 1, "headers": 0, "firewall_rules": 0, "clean_urls": True, "trailing_slash": None,
                "crons": [{"path": "/api/cron", "schedule": "0 5 * * *", "next_run_at": "2026-10-23T05:00:00Z",
                           "last_run_at": "2026-10-22T05:00:00Z", "last_status": 500, "last_error": "answered HTTP 500",
                           "last_duration_ms": 120, "running": False}],
                "warnings": ["`functions` is not used here"]},
    "routes_active": True, "hosts": ["blog.example.com"], "error": None,
}


# A failed build (d2) and what a model answers about it, as Ollama streams it.
FAILED_LOG = json.dumps([
    {"output": "npm run build", "type": "stdout", "hidden": False, "timestamp": "t"},
    {"output": "Module not found: Can't resolve '../components/Header'", "type": "stderr", "hidden": False, "timestamp": "t"},
    {"output": "internal helper noise", "type": "stdout", "hidden": True, "timestamp": "t"},
    {"output": "Deployment failed.", "type": "stdout", "hidden": False, "timestamp": "t"},
])
ANSWER = ["**Cause:** ", "the import uses `Header` but the file is `header.tsx`.\n", "**Fix:** rename it.\n", "**Check:** `npm run build`"]

FLAKED = []


def ndjson(model):
    if model.startswith("missing"):
        return [{"error": f"model '{model}' not found"}]
    chunks = [{"model": model, "message": {"role": "assistant", "content": c}, "done": False} for c in ANSWER]
    return chunks + [{"model": model, "done": True, "eval_count": 40, "eval_duration": 4_000_000_000}]


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

    def stream(self, lines):
        body = "".join(json.dumps(l) + "\n" for l in lines).encode()
        self.send_response(404 if "error" in lines[0] else 200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def summary(self, raw):
        merged = {}
        for f in json.loads(raw)["files"]:
            merged.update(json.loads(f["content"]))
        warnings = [f"`{k}` is not used here" for k in merged if k in ("functions", "regions")]
        return {
            "redirects": len(merged.get("redirects", [])), "rewrites": len(merged.get("rewrites", [])),
            "headers": len(merged.get("headers", [])), "firewall_rules": len(merged.get("firewall", {}).get("rules", [])),
            "clean_urls": merged.get("cleanUrls", False), "trailing_slash": merged.get("trailingSlash"),
            "crons": [dict(c, next_run_at=None, last_run_at=None, last_status=None, last_error=None,
                           last_duration_ms=None, running=False) for c in merged.get("crons", [])],
            "warnings": warnings,
        }

    def check(self, raw):
        try:
            return self.reply(200, self.summary(raw))
        except (ValueError, KeyError) as e:
            return self.reply(400, {"error": f"vercel.json: {e}"})

    def any(self, method):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n).decode() if n else ""
        auth = self.headers.get("Authorization")
        with open(sys.argv[2], "a") as f:
            f.write(f"{method} {self.path} auth={auth} {raw}\n")
        p = self.path
        # The Ollama on this machine (vfy why --log): no auth.
        if p == "/api/version":
            return self.reply(200, {"version": "0.40.6"})
        if p == "/api/chat":
            return self.stream(ndjson(json.loads(raw)["model"]))
        # Gemma 4 through the Gemini API.
        if p.startswith("/v1beta/models/"):
            # "gem-flaky" gets one 500 first, as the real API sometimes answers.
            if self.headers.get("x-goog-api-key") == "gem-flaky" and not FLAKED:
                FLAKED.append(1)
                return self.reply(500, {"error": {"code": 500, "message": "Internal error encountered."}})
            if self.headers.get("x-goog-api-key") not in ("gem-key", "gem-flaky"):
                return self.reply(400, {"error": {"code": 400, "message": "API key not valid. Please pass a valid API key."}})
            if p != "/v1beta/models/gemma-4-26b-a4b-it:generateContent":
                return self.reply(404, {"error": {"code": 404, "message": "model not found"}})
            # Like the real API: without minimal thinking, thoughts use up the budget.
            if json.loads(raw)["generationConfig"].get("thinkingConfig") != {"thinkingLevel": "minimal"}:
                return self.reply(200, {"candidates": [{"finishReason": "MAX_TOKENS", "content": {"role": "model", "parts": [
                    {"text": "thinking about the import…", "thought": True}]}}]})
            return self.reply(200, {"candidates": [{"finishReason": "STOP", "content": {"role": "model", "parts": [
                {"text": "thinking about the import…", "thought": True}, {"text": "".join(ANSWER)}]}}]})
        if p.startswith("/v1/"):
            if auth != "Bearer agent-secret":
                return self.reply(401, {"error": "missing or invalid agent token"})
            if p == "/v1/llm":
                return self.reply(200, {"installed": True, "ready": True, "models": [
                    {"name": "nomic-embed-text:latest"}, {"name": "llama3.2:3b"}, {"name": "gemma4:e2b"}]})
            if p == "/v1/llm/chat":
                return self.stream(ndjson(json.loads(raw)["model"]))
            if p == "/v1/doctor":
                return self.reply(200, {"checks": [{"id": "disk", "label": "System disk", "status": "warn", "detail": "88% used", "fix": "docker system prune -af"}]})
            if p == "/v1/backup/run":
                return self.reply(200, {"configured": True, "running": True})
            if p == "/v1/sleep/a1" and method == "PUT":
                body = json.loads(raw)
                if body.get("idle_minutes", 30) < 5:
                    return self.reply(400, {"error": "idle time must be 5 minutes to 24 hours"})
                return self.reply(200, {"settings": [], "targets": []})
            if p == "/v1/projects/check":
                return self.check(raw)
            if p == "/v1/projects" and method == "GET":
                return self.reply(200, {"projects": [PROJECT], "proxy_image": "traefik:v3.7", "error": None})
            if p == "/v1/projects/a1" and method == "PUT":
                return self.reply(200, dict(PROJECT, source="upload", summary=self.summary(raw)))
            if p == "/v1/projects/a1/sync":
                return self.reply(200, PROJECT)
            if p == "/v1/projects/a1" and method == "DELETE":
                return self.reply(200, {"removed": True})
            if p == "/v1/projects/a1/crons/run":
                if json.loads(raw).get("path") != "/api/cron":
                    return self.reply(404, {"error": "no cron job for that path"})
                return self.reply(200, {"id": "j9", "kind": "cron", "title": "Run /api/cron", "status": "running"})
            return self.reply(404, {"error": "not found"})
        if auth != "Bearer coolify-secret":
            return self.reply(401, {"message": "Unauthenticated."})
        if p == "/api/v1/applications":
            return self.reply(200, APPS)
        if p == "/api/v1/applications/a1":
            return self.reply(200, APPS[0])
        if p.startswith("/api/v1/deploy?uuid=a1"):
            return self.reply(200, {"deployments": [{"message": "Deployment request queued.", "deployment_uuid": "d1"}]})
        if p == "/api/v1/applications/a1/envs":
            return self.reply(200, [
                {"key": "DATABASE_URL", "value": "postgres://u:p@db/x", "is_preview": False},
                {"key": "QUOTE", "value": 'say "hi"', "is_preview": False},
                {"key": "DATABASE_URL", "value": "postgres://preview", "is_preview": True},
            ])
        if p.startswith("/api/v1/deployments/applications/a1"):
            # As Coolify 4.4: newest first; only full SHAs.
            return self.reply(200, {"count": 4, "deployments": [
                {"deployment_uuid": "d4", "status": "finished", "pull_request_id": 0, "commit": "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432"},
                {"deployment_uuid": "d3", "status": "finished", "pull_request_id": 7, "commit": "4b1dffff00000000000000000000000000000000"},
                {"deployment_uuid": "d2", "status": "failed", "pull_request_id": 0, "commit": "4b1d0000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                 "commit_message": "Add header\n\nlong body", "created_at": "2026-10-23T08:00:00Z"},
                {"deployment_uuid": "d1", "status": "finished", "pull_request_id": 0, "commit": "4b1d2e7a9c0b1d2e3f4a5b6c7d8e9f0a1b2c3d4e"},
            ]})
        if p == "/api/v1/deployments/d2":
            return self.reply(200, {"deployment_uuid": "d2", "status": "failed", "logs": FAILED_LOG})
        if p == "/api/v1/applications/a1/rollback":
            return self.reply(200, {"message": "Rollback queued."})
        return self.reply(404, {"message": "Not found."})

    def do_GET(self):
        self.any("GET")

    def do_POST(self):
        self.any("POST")

    def do_PUT(self):
        self.any("PUT")

    def do_DELETE(self):
        self.any("DELETE")


srv = HTTPServer(("127.0.0.1", 0), H)
with open(sys.argv[1], "w") as f:
    f.write(str(srv.server_address[1]))
srv.serve_forever()
