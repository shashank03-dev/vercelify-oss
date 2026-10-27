---
name: vercelify
description: Deploy, inspect and debug apps on a self-hosted Vercelify server (an old x86 laptop or Raspberry Pi running the Vercelify agent and its private network), and explain failed builds with Gemma 4 running locally in Ollama. Use when the user mentions Vercelify, the vfy CLI, self-hosting a Vercel-style PaaS on their own hardware, a failed or broken deploy or build log, "why did my deploy fail", vercel.json on their own server, rolling back, pulling env vars, or running Gemma or other open models with Ollama on a home server.
license: MIT
compatibility: Needs bash, curl and jq. vfy APP commands need a server paired in the Vercelify desktop app (reached over Vercelify's private network). vfy why --log needs Ollama on the same machine, or a GEMINI_API_KEY for the Gemini API.
metadata:
  repository: https://github.com/shashank03-dev/vercelify-oss
  version: "0.2.0"
---

# Vercelify

Vercelify turns a spare Linux laptop into a free, self-hosted Vercel: push-to-deploy,
PR previews, rollbacks, databases and HTTPS domains. `vercelify-agent` (port 7420,
bearer token) builds and runs the apps with its own engine (BuildKit, Traefik), reports
the machine's state and runs a local Ollama for AI; `vercelify-mesh` (WireGuard) gives
private access to it from the user's devices.

You drive it with three tools:

| Tool | Where it runs | Use it for |
| --- | --- | --- |
| `vfy` (`cli/vfy` in the repo) | The developer's Mac or Linux machine | Apps, deploys, logs, rollbacks, env, vercel.json, **`vfy why`** |
| `vercelify` | On the server, with `sudo` | Health, backups, the local AI runtime (`vercelify llm …`) |
| Agent HTTP API | `http://SERVER:7420/v1/…` over the private network | Anything scripted; see [references/agent-api.md](references/agent-api.md) |

## Getting vfy

`vfy` is one bash script (bash 3.2 compatible) that needs `curl` and `jq`:

```bash
git clone https://github.com/shashank03-dev/vercelify-oss && cd vercelify-oss
./cli/vfy --help
# optional: put it on PATH
ln -s "$PWD/cli/vfy" /usr/local/bin/vfy
```

It reads the servers paired in the Vercelify desktop app (`VFY_SETTINGS` overrides the
settings path). Commands that name an app need a paired server; `vfy why --log` does not.

## Workflow: a deploy failed

1. Find the app: `vfy apps` (name, status, URL). The APP argument can be the name or the uuid.
2. Ask why it failed: `vfy why APP`. This fetches the app's last failed
   deployment, trims the build log, and asks the model on the server that built it
   (Gemma 4 E2B when installed) for an answer in three parts:
   - **Cause:** the failing step and the root cause
   - **Fix:** the exact change (file and line, setting, or command)
   - **Check:** one command to confirm the fix locally before pushing again
3. Apply the fix in the user's repository, run the **Check** command, and commit.
4. Redeploy: `vfy deploy APP` (or push to the watched branch), then `vfy logs APP`.
5. If the new version is worse, roll back: `vfy rollback APP COMMIT`.

Treat the model's answer as a lead, not as ground truth: confirm it against the log
before you edit code. Common causes on a Linux build box are file-name case mismatches
(`Header` vs `header.tsx`), env vars missing from the app's settings, and lockfile or
Node version drift.

### A log with no server

When the user has a build log as a file (a CI log, or a Vercel log they copied):

```bash
vfy why --log build.log                       # local Ollama at http://localhost:11434
cat build.log | vfy why --log -               # from stdin
vfy why --log build.log --vercel-json vercel.json   # include routing config as context
vfy why --log build.log --provider gemini     # Gemma 4 via the Gemini API
```

The repository ships a seeded example: `cli/examples/failed-build.log` (a Next.js build
that fails on Linux because of a case-sensitive import).

### Providers and settings

| Flag / env | Default | Meaning |
| --- | --- | --- |
| `--provider server` | (default for `vfy why APP`) | The paired server's Ollama, through the agent. Nothing leaves the user's machines. |
| `--provider ollama` | (default for `--log`) | Ollama on this machine, `VFY_OLLAMA_URL` (default `http://localhost:11434`). |
| `--provider gemini` | | Google's Gemini API. Needs `GEMINI_API_KEY`; model `VFY_GEMINI_MODEL` (default `gemma-4-26b-a4b-it`). |
| `VFY_AI_MODEL` | `gemma4:e2b` | The Ollama model to ask. Unset with `--provider server`: `gemma4:e2b` if the server has it, else another `gemma4*`, else any non-embedding model. |

Answers are capped at 400 tokens at temperature 0.2, and the log is trimmed to error
lines with context plus the last 30 lines (at most 5000 characters), so it stays quick on
an old CPU. The footer names the model, where it ran, and tokens per second.

### When vfy why fails

| Message | Do this |
| --- | --- |
| `Ollama isn't running at …` | Start the Ollama app or `ollama serve`, then `ollama pull gemma4:e2b`. |
| `model '…' not found — download it first` | `ollama pull gemma4:e2b` (local), or on the server `sudo vercelify llm pull gemma4:e2b`. |
| `… has no local AI yet` | Server side: `sudo vercelify llm install`, or the app's **AI models → Install**. |
| `Ollama on … isn't answering` | `sudo vercelify llm start` (or **AI models → Restart**). |
| `GEMINI_API_KEY is not set` | Create a key at https://aistudio.google.com/apikey and export it. Never commit it. |
| `no failed deployments of APP lately` | Nothing to explain; check `vfy logs APP` for runtime errors instead. |

## Setting up Gemma 4 with Ollama

On the developer's machine:

```bash
# macOS: brew install ollama (or the Ollama app). Linux: see https://ollama.com/download
ollama serve &               # skip if the Ollama app is running
ollama pull gemma4:e2b       # Gemma 4 E2B, Apache 2.0
./cli/vfy why --log cli/examples/failed-build.log
```

On a Vercelify server (models are shared by the cockpit, `vfy why APP` and the user's apps):

```bash
sudo vercelify llm install [ssd|hdd]   # Ollama in a container, port 11434, private network only
sudo vercelify llm pull gemma4:e2b
sudo vercelify llm                     # status, endpoints, installed models
```

Gemma 4 E2B is the default because it is the smallest Gemma 4. Ollama's default build is
still about a 7 GB download and needs about that much free memory while it answers, and a
server loads one model at a time next to the user's apps. On an 8 GB server,
prefer `vfy why --log` on the user's own computer, or `--provider gemini` if they agree.
`gemma4:e4b` needs roughly 10 GB.

## Everyday vfy commands

```text
vfy apps                     every app with its status and URL
vfy deploy APP               deploy now
vfy logs APP [LINES]         runtime logs
vfy rollback APP COMMIT      put an earlier commit back live
vfy env pull APP [FILE]      write production variables to FILE (default .env, private; never overwrites)
vfy open APP                 open the app's URL
vfy status | alerts | backup server health checks, recent alerts, start a backup
vfy sleep APP [MINUTES|off]  stop APP after MINUTES without visits
vfy config check [DIR]       what DIR's vercel.json would do on Vercelify
vfy config push APP [DIR]    apply vercel.json to APP; sync | show | rm also exist
vfy crons APP [run PATH]     cron jobs from vercel.json, and run one now
vfy servers                  paired servers; -s NAME picks another one for a command
```

The full list with examples and exit behaviour is in [references/vfy.md](references/vfy.md).

## Installing a server (when the user has none yet)

On the old laptop, after a fresh Ubuntu 24.04 (this erases it):

```bash
curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash
# with local AI too:
curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash -s -- --ai gemma4:e2b
```

On the Mac: `curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install-mac.sh | bash -s -- --pair vfy2.…`
(the command the server installer prints) installs the desktop app, `vercelify-mesh` and
`vfy`, and pairs with the server.
Always tell the user before running an installer: it changes the machine.

## Rules

- Never print, log or commit tokens or `GEMINI_API_KEY`. vfy passes them to curl on stdin.
- Prefer `--provider server` or local Ollama. Use `--provider gemini` only when the user
  agrees that the trimmed log may be sent to Google.
- Don't run `vfy deploy`, `rollback`, `sleep`, `config push` or `env pull` without the
  user asking for it: they change a live server or write secrets to disk.
