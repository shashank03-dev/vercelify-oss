#!/usr/bin/env python3
"""Mock of an agent on the platform engine (`/v1/platform`) for `vfy` tests.

Usage: mock_platform.py PORT_FILE LOG_FILE
Each request is logged as `METHOD PATH auth=… BODY`.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

APPS = {
    "blog": {"id": "app_blog0000000000001", "name": "blog", "desired": "running", "current_deployment_id": "dpl_3",
             "last_status": "healthy", "url": "https://blog.example.com"},
    "api": {"id": "app_api00000000000001", "name": "api", "desired": "running", "current_deployment_id": None,
            "last_status": "failed", "url": None},
}


def dep(n, status, sha, current=False, pr=None, image="vfy/blog:x"):
    return {"id": f"dpl_{n}", "app_id": APPS["blog"]["id"], "number": n, "kind": "preview" if pr else "production",
            "pr_number": pr, "branch": "main", "commit_sha": sha, "commit_message": f"commit {n}", "trigger": "push",
            "rollback_of": None, "image": image, "node_id": None, "status": status, "error": "",
            "current": current, "created_at": "2026-10-22T08:00:00Z", "started_at": None, "finished_at": None}


DEPLOYMENTS = [
    dep(4, "healthy", "4b1d2e7a9c0b1d2e3f4a5b6c7d8e9f0a1b2c3d4e", pr=12),
    dep(3, "healthy", "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432", current=True),
    dep(2, "failed", "abcdef1234567890abcdef1234567890abcdef12", image=""),
    dep(1, "superseded", "1234567890abcdef1234567890abcdef12345678"),
]
FAILED = dict(dep(2, "failed", "abcdef1234567890abcdef1234567890abcdef12", image=""), app_id=APPS["api"]["id"],
              commit_message="Add notes", error="build failed")
BUILD = [{"seq": 1, "at": "t", "stream": "info", "text": "Fetching https://github.com/me/blog (main)"},
         {"seq": 2, "at": "t", "stream": "stdout", "text": "Detected Next.js 15"},
         {"seq": 3, "at": "t", "stream": "info", "text": "Deployment is live"}]
FAILED_BUILD = [{"seq": 1, "at": "t", "stream": "stdout", "text": "npm run build"},
                {"seq": 2, "at": "t", "stream": "error", "text": "Module not found: Can't resolve '../components/Header'"}]
ENV = [{"id": "env_1", "key": "DATABASE_URL", "value": "postgres://u:p@db/x", "build": False, "preview_only": False,
        "secret": True, "scope": "app", "updated_at": "t"},
       {"id": "env_2", "key": "PREVIEW_ONLY", "value": "x", "build": False, "preview_only": True,
        "secret": False, "scope": "app", "updated_at": "t"}]
TEMPLATES = [{"id": "umami", "name": "Umami", "description": "Website analytics.", "category": "analytics",
              "website": "https://umami.is", "min_memory_mb": 512, "compose": "services:\n"},
             {"id": "plausible", "name": "Plausible", "description": "Analytics with ClickHouse.", "category": "analytics",
              "website": "https://plausible.io", "min_memory_mb": 2048, "compose": "services:\n"}]
ANSWER = ["**Cause:** ", "the import uses `Header` but the file is `header.tsx`.\n", "**Fix:** rename it.\n", "**Check:** `npm run build`"]


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def send(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else body.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def reply(self, code, obj):
        self.send(code, json.dumps(obj))

    def lines(self, items):
        self.send(200, "".join(json.dumps(i) + "\n" for i in items), "application/x-ndjson")

    def handle_any(self, method):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length).decode() if length else ""
        with open(sys.argv[2], "a") as f:
            f.write(f"{method} {self.path} auth={self.headers.get('Authorization', '')} {raw}\n")
        if self.headers.get("Authorization") != "Bearer agent-secret":
            return self.reply(401, {"error": "missing or invalid agent token"})
        path, _, query = self.path.partition("?")
        body = json.loads(raw) if raw else {}
        seg = path.strip("/").split("/")
        if path == "/v1/llm":
            return self.reply(200, {"installed": True, "ready": True, "models": [{"name": "gemma4:e2b"}]})
        if path == "/v1/llm/chat":
            chunks = [{"message": {"role": "assistant", "content": c}, "done": False} for c in ANSWER]
            return self.lines(chunks + [{"done": True, "eval_count": 40, "eval_duration": 4_000_000_000}])
        if seg[:2] != ["v1", "platform"]:
            return self.reply(404, {"error": "not found"})
        seg = seg[2:]
        if seg == ["apps"]:
            return self.reply(200, list(APPS.values()))
        if seg[0] == "apps":
            app = APPS.get(seg[1]) or next((a for a in APPS.values() if a["id"] == seg[1]), None)
            if not app:
                return self.reply(404, {"error": f"app {seg[1]} not found"})
            sub = seg[2] if len(seg) > 2 else ""
            if sub == "":
                return self.reply(200, app)
            if sub == "deploy":
                if app["name"] == "api":
                    return self.reply(200, dict(FAILED, id="dpl_9", number=9, status="queued"))
                return self.reply(200, dict(DEPLOYMENTS[1], id="dpl_5", number=5, status="queued", current=False))
            if sub == "deployments":
                return self.reply(200, [FAILED] if app["name"] == "api" else DEPLOYMENTS)
            if sub == "rollback":
                return self.reply(200, dict(DEPLOYMENTS[1], id="dpl_6", number=6, status="queued", trigger="rollback"))
            if sub == "restart":
                return self.reply(200, {"restarted": 2})
            if sub == "logs":
                out = [{"at": "2026-10-22T08:00:00Z", "stream": "stdout", "text": "ready on :3000"},
                       {"at": "2026-10-22T08:00:01Z", "stream": "stderr", "text": "warn: slow query"}]
                if "follow=true" in query:
                    out.append({"at": "2026-10-22T08:00:02Z", "stream": "stdout", "text": "GET / 200"})
                return self.lines(out)
            if sub == "env" and len(seg) > 3 and seg[3] == "import":
                n = len([l for l in body.get("content", "").splitlines() if "=" in l and not l.startswith("#")])
                return self.reply(200, {"saved": n})
            if sub == "env" and method == "PUT":
                return self.reply(200, {"saved": len(body["vars"])})
            if sub == "env":
                if "reveal=true" not in query:
                    return self.reply(200, [dict(v, value=None if v["secret"] else v["value"]) for v in ENV])
                return self.reply(200, ENV)
        if seg[0] == "deployments":
            d = {"dpl_5": dict(DEPLOYMENTS[1], id="dpl_5", status="healthy"),
                 "dpl_9": dict(FAILED, id="dpl_9", status="failed", error="build failed: exit status 1"),
                 "dpl_2": FAILED}.get(seg[1])
            if not d:
                return self.reply(404, {"error": "no deployment"})
            if len(seg) > 2 and seg[2] == "logs":
                return self.lines(FAILED_BUILD if d["status"] == "failed" else BUILD)
            return self.reply(200, d)
        if seg == ["templates"]:
            return self.reply(200, TEMPLATES)
        if seg[0] == "templates":
            if seg[1] not in [t["id"] for t in TEMPLATES]:
                return self.reply(404, {"error": f"template {seg[1]} not found"})
            return self.reply(200, {"id": "app_new", "name": body.get("name") or seg[1]})
        return self.reply(404, {"error": "not found"})

    def do_GET(self):
        self.handle_any("GET")

    def do_POST(self):
        self.handle_any("POST")

    def do_PUT(self):
        self.handle_any("PUT")


srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
with open(sys.argv[1], "w") as f:
    f.write(str(srv.server_port))
srv.serve_forever()
