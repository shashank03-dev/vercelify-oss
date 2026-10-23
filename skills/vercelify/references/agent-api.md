# Vercelify agent API

`vercelify-agent` listens on port 7420 of each server, reachable over Tailscale
(`http://SERVER.TAILNET.ts.net:7420`). Everything under `/v1` except `/v1/health` and
`/v1/pair/claim` needs `Authorization: Bearer TOKEN`. The token is in
`/etc/vercelify/agent.token` on the server (mode 600) and in the desktop app's
settings after pairing. Pass it to curl on stdin so it never shows in `ps`:

```bash
TOKEN=$(sudo cat /etc/vercelify/agent.token)   # on the server
printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" |
  curl -sS -K - http://laptop:7420/v1/system | jq .
```

## Endpoints an agent usually needs

| Method and path | What it does |
| --- | --- |
| `GET /v1/health` | Liveness, no token. |
| `GET /v1/system` | CPU, memory, disks (role `system`/`storage`), power, thermal. |
| `GET /v1/doctor` | Health checks, as `vfy status` shows them. |
| `GET /v1/containers`, `GET /v1/containers/{id}/logs` | Containers and their logs. |
| `GET /v1/projects`, `POST /v1/projects/check` | vercel.json state per app, and a dry run of a config. |
| `GET /v1/jobs`, `GET /v1/jobs/{id}` | Background jobs (backups, moves, pulls). |
| `GET /v1/alerts`, `GET /v1/events` | Recent alerts; a live NDJSON event stream. |
| `GET /v1/llm` | Local AI: `installed`, `ready`, `models[].name`, `endpoints`, running `tasks`. |
| `POST /v1/llm/install` `{"tier":"ssd"\|"hdd"}` | Install Ollama (container `vercelify-ollama`, port 11434). |
| `POST /v1/llm/{start\|stop\|restart\|remove}` | Control the runtime. |
| `POST /v1/llm/models/pull` `{"model":"gemma4:e2b"}` | Download a model in the background (watch `GET /v1/llm` → `tasks`). |
| `POST /v1/llm/models/delete` `{"model":"…"}` | Remove a model. |
| `POST /v1/llm/chat` | Chat with an installed model (below). |

Deployments, apps, env vars and rollbacks are Coolify's API (`http://SERVER:8000/api/v1`,
its own token); `vfy` wraps both.

## POST /v1/llm/chat

```json
{
  "model": "gemma4:e2b",
  "messages": [{"role": "system", "content": "…"}, {"role": "user", "content": "…"}],
  "max_tokens": 400,
  "temperature": 0.2,
  "stream": true
}
```

- `max_tokens` defaults to 1024 and is clamped to 16–8192 (small models can otherwise
  ramble for minutes on an old CPU). `temperature` is clamped to 0–2.
- With `"stream": true` the body is Ollama's NDJSON: one
  `{"message":{"content":"…"},"done":false}` per chunk, then a final line with
  `"done":true`, `eval_count` and `eval_duration` (ns). Tokens/s =
  `eval_count / (eval_duration / 1e9)`.
- An error arrives as `{"error":"model 'x' not found"}`: tell the user to pull it.
- Closing the connection cancels generation on the server.

This is what `vfy why APP` and the cockpit's **Explain with AI** button call. The prompt
they send is `EXPLAIN_SYSTEM` in `cli/vfy` (identical to `cockpit/src/lib/explain.ts`).

## Ollama directly

Port 11434 is open to the tailnet only (Vercelify firewall). OpenAI-compatible clients
use `http://SERVER:11434/v1` with any API key; apps deployed on the same server use
`http://vercelify-ollama:11434`.
