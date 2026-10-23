#!/usr/bin/env python3
"""A static app for the routing contract test.

Serves a folder (404 for anything missing, like a static host), and records
every request to /api/cron* as one JSON line (path, Authorization, User-Agent)
so the test can see what the agent's cron runner sent.

    routing_app.py PORT DIR LOG
"""
import json
import sys
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class Handler(SimpleHTTPRequestHandler):
    log_file = ""

    def do_GET(self):
        if self.path.startswith("/api/cron"):
            with open(self.log_file, "a", encoding="utf-8") as f:
                f.write(json.dumps({
                    "path": self.path,
                    "authorization": self.headers.get("Authorization"),
                    "user_agent": self.headers.get("User-Agent"),
                }) + "\n")
            body = b"ok"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def log_message(self, *args):
        pass


def main():
    port, root, log = int(sys.argv[1]), sys.argv[2], sys.argv[3]
    Handler.log_file = log
    server = ThreadingHTTPServer(("127.0.0.1", port), partial(Handler, directory=root))
    server.serve_forever()


if __name__ == "__main__":
    main()
