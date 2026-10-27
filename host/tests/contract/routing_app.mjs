// A static app for the routing contract test, run as an app on Vercelify's
// engine (node:22-slim, no dependencies).
//
// Serves a folder like a static host (a folder without its trailing slash
// redirects to it, a missing file is a 404), and records every request to
// /api/cron* as one JSON line (path, Authorization, User-Agent) so the test
// can see what the agent's cron runner sent.
//
//     node routing_app.mjs PORT DIR LOG
import { appendFileSync, readFileSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize } from "node:path";

const [port, root, log] = process.argv.slice(2);
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };

function send(res, code, body, headers = {}) {
  res.writeHead(code, { "Content-Length": Buffer.byteLength(body), ...headers });
  res.end(body);
}

createServer((req, res) => {
  const url = new URL(req.url, "http://app");
  if (url.pathname.startsWith("/api/cron")) {
    appendFileSync(log, JSON.stringify({
      path: url.pathname + url.search,
      authorization: req.headers.authorization ?? null,
      user_agent: req.headers["user-agent"] ?? null,
    }) + "\n");
    return send(res, 200, "ok");
  }
  if (req.method !== "GET" && req.method !== "HEAD") return send(res, 405, "method not allowed");
  let path = join(root, normalize(decodeURIComponent(url.pathname)));
  if (!path.startsWith(root)) return send(res, 404, "not found");
  try {
    if (statSync(path).isDirectory()) {
      if (!url.pathname.endsWith("/")) return send(res, 301, "", { Location: url.pathname + "/" + url.search });
      path = join(path, "index.html");
    }
    const body = readFileSync(path);
    res.writeHead(200, { "Content-Type": types[extname(path)] ?? "application/octet-stream", "Content-Length": body.length });
    res.end(req.method === "HEAD" ? undefined : body);
  } catch {
    send(res, 404, "not found");
  }
}).listen(Number(port), "0.0.0.0");
