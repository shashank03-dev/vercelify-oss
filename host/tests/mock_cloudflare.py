#!/usr/bin/env python3
"""Stateful mock of the Cloudflare v4 API endpoints used by lib/cloudflare.sh.

Usage: mock_cloudflare.py PORT_FILE STATE_FILE
Writes the listening port to PORT_FILE and dumps state to STATE_FILE after
every mutating request so tests can assert on it.
"""
import json
import re
import sys
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

TOKEN = "test-cf-token"
STATE = {
    "zones": [{"id": "zone123", "name": "example.com", "status": "active", "account": {"id": "acc456"}}],
    "tunnels": [],
    "configs": {},
    "dns": [{"id": "rec-mx", "type": "MX", "name": "example.com", "content": "mail.example.com"}],
}


def ok(result):
    return 200, {"success": True, "errors": [], "messages": [], "result": result}


def fail(code, msg, http=400):
    return http, {"success": False, "errors": [{"code": code, "message": msg}], "result": None}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def _send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}")

    def _handle(self, method):
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            return self._send(*fail(10000, "Authentication error", 403))
        u = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(u.query).items()}
        p = u.path.removeprefix("/client/v4")
        status, body = self.route(method, p, q)
        if method != "GET":
            with open(sys.argv[2], "w") as f:
                json.dump(STATE, f, indent=1)
        self._send(status, body)

    def route(self, method, p, q):
        if method == "GET" and p == "/zones":
            return ok([z for z in STATE["zones"] if z["name"] == q.get("name")])

        m = re.fullmatch(r"/accounts/(\w+)/cfd_tunnel", p)
        if m:
            if method == "GET":
                return ok([t for t in STATE["tunnels"] if t["name"] == q.get("name") and not t["deleted"]])
            b = self._body()
            if b.get("config_src") != "cloudflare" or not b.get("name"):
                return fail(1003, "bad tunnel request")
            t = {"id": str(uuid.uuid4()), "name": b["name"], "deleted": False, "account": m.group(1)}
            STATE["tunnels"].append(t)
            return ok(t)

        m = re.fullmatch(r"/accounts/(\w+)/cfd_tunnel/([\w-]+)/token", p)
        if m and method == "GET":
            return ok(f"eyJ-token-for-{m.group(2)}")

        m = re.fullmatch(r"/accounts/(\w+)/cfd_tunnel/([\w-]+)/configurations", p)
        if m and method == "PUT":
            b = self._body()
            ingress = b.get("config", {}).get("ingress", [])
            if not ingress or "hostname" in ingress[-1]:
                return fail(1056, "last ingress rule must be a catch-all")
            STATE["configs"][m.group(2)] = b["config"]
            return ok({"tunnel_id": m.group(2), "config": b["config"]})

        m = re.fullmatch(r"/zones/(\w+)/dns_records", p)
        if m:
            if method == "GET":
                return ok([r for r in STATE["dns"] if r["name"] == q.get("name")])
            b = self._body()
            address_types = {"A", "AAAA", "CNAME"}
            if b["type"] in address_types and any(
                r["name"] == b["name"] and r["type"] in address_types and "CNAME" in (r["type"], b["type"])
                for r in STATE["dns"]
            ):
                return fail(81053, "An A, AAAA, or CNAME record with that host already exists.")
            r = dict(b, id=str(uuid.uuid4()))
            STATE["dns"].append(r)
            return ok(r)

        m = re.fullmatch(r"/zones/(\w+)/dns_records/([\w-]+)", p)
        if m:
            idx = next((i for i, r in enumerate(STATE["dns"]) if r["id"] == m.group(2)), None)
            if idx is None:
                return fail(81044, "Record does not exist.")
            if method == "PUT":
                STATE["dns"][idx] = dict(self._body(), id=m.group(2))
                return ok(STATE["dns"][idx])
            if method == "DELETE":
                STATE["dns"].pop(idx)
                return ok({"id": m.group(2)})

        return fail(7000, f"No route for {method} {p}", 404)

    def do_GET(self):
        self._handle("GET")

    def do_POST(self):
        self._handle("POST")

    def do_PUT(self):
        self._handle("PUT")

    def do_DELETE(self):
        self._handle("DELETE")


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(sys.argv[1], "w") as f:
        f.write(str(srv.server_address[1]))
    srv.serve_forever()
