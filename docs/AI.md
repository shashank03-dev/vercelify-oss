# Local AI models

Vercelify can run open models (Llama, Qwen, Gemma, Phi, DeepSeek R1, embeddings…)
on any of your servers with [Ollama](https://ollama.com), and you use them from your
Mac, from your apps, or from any tool that speaks the OpenAI API. Nothing leaves your
machines and there is no API bill.

## Set it up

Pick one:

- **Mac app → AI models → Install Ollama.** Choose where models live (SSD loads faster,
  HDD has more room).
- **During install:** `curl … install.sh | sudo bash -s -- --ai` (or answer *yes* when
  the installer asks). `--ai qwen2.5:3b` also downloads that model.
- **Later, on the host:** `sudo vercelify llm install`, then `sudo vercelify llm pull llama3.2:3b`.

The Ollama image is about a **4 GB download and ~9.5 GB on disk** (it bundles GPU
libraries); the agent refuses to start the install with less than 12 GB free on the
system disk. Models are extra: 0.3–5 GB each.

## Why did my deploy fail?

The first built-in use of the local models: a failed build log goes to
[Gemma 4 E2B](https://ai.google.dev/gemma) (`gemma4:e2b`, ~2.3B effective parameters,
[Apache 2.0](https://ai.google.dev/gemma/apache_2)) and comes back as three lines,
**Cause**, **Fix** and **Check**, in under 120 words.

| Where | What runs the model |
| --- | --- |
| Cockpit: a failed deployment → **Build logs** → **Explain with AI** | The Ollama on the server that built the app, through `POST /v1/llm/chat` |
| `vfy why APP` | The same (`--provider server`, the default) |
| `vfy why --log FILE` (or `-` for stdin) | The Ollama on your own machine (`VFY_OLLAMA_URL`, default `http://localhost:11434`); no server needed |
| `vfy why … --provider gemini` | Gemma 4 on Google's Gemini API (`GEMINI_API_KEY`; `VFY_GEMINI_MODEL`, default `gemma-4-26b-a4b-it`) |

How it keeps an old CPU fast enough:

- **Small input.** The log is trimmed to lines that look like errors (2 before, 3 after)
  plus the last 30 lines, with `…` for the gaps, and at most 5000 characters (the tail wins).
  Build-log `stderr` lines are marked `[stderr] `. With `--vercel-json`, the first 1500
  characters of vercel.json go along.
- **Small output.** `max_tokens` 400, temperature 0.2, one fixed system prompt
  (`EXPLAIN_SYSTEM` in `cli/vfy`, identical to `cockpit/src/lib/explain.ts`, and a unit
  test fails if they drift).
- **Model choice.** `gemma4:e2b` if installed, then any other `gemma4*`, then any model
  that isn't an embedding model. `VFY_AI_MODEL` overrides it in the CLI.

Memory: Ollama's default `gemma4:e2b` is about a 7 GB download and wants about that much
free memory while it answers (the app's model list shows whether it fits). On an 8 GB
server that is also running your apps, prefer running it on your Mac with
`vfy why --log`, or use the Gemini API path. `gemma4:e4b` is smarter but needs roughly 10 GB.

Try it without a server: `ollama pull gemma4:e2b && ./cli/vfy why --log cli/examples/failed-build.log`.
The answer is advice, not a guarantee: check it against the log before changing code.

## Tuned for old laptops

The agent runs Ollama in a container named `vercelify-ollama` with:

| Setting | Why |
| --- | --- |
| `OLLAMA_MAX_LOADED_MODELS=1`, `OLLAMA_NUM_PARALLEL=1` | One model and one answer at a time fits in 8 GB next to your apps. |
| `OLLAMA_KEEP_ALIVE=5m` | The model leaves memory five idle minutes after the last request. |
| `--cpu-shares 512` | Half the CPU weight of other containers: web apps stay responsive while a model thinks. |
| Answers capped at 1,024 tokens | Small models can loop until the context fills, which takes many minutes on an old CPU. Send `max_tokens` (up to 8192) to raise it. Stopping a chat (or disconnecting) cancels generation on the server. |
| Power guard | When the CPU passes 92 °C the guard pauses non-core containers, Ollama included. |

The **Download a model** list in the app shows which models fit the server's free
memory right now (*fits*, *tight* if other apps must give some back, *too big*).
Good starting points: `gemma4:e2b` to explain failed deploys (16 GB machines), `llama3.2:3b` (8 GB laptops), `gemma3:1b` or `qwen2.5:1.5b`
(4 GB), `qwen2.5-coder:1.5b` for code, `nomic-embed-text` for search/RAG.

## Use it from your Mac

Port 11434 is published on the server and kept private to your Vercelify network by
its firewall (the app warns if the firewall is off). Replace `laptop` with your
server's name on the network (`laptop.vfy`, or its `10.77.x.x` address; see
[MESH.md](MESH.md)):

```bash
# Ollama's own API
curl http://laptop:11434/api/chat -d '{"model":"llama3.2:3b","messages":[{"role":"user","content":"Hi"}],"stream":false}'

# The Ollama CLI on the Mac, running models on the server
brew install ollama
OLLAMA_HOST=http://laptop:11434 ollama run llama3.2:3b
```

OpenAI-compatible clients (Python/JS SDKs, Cursor, Continue, Open WebUI, LangChain…)
use `http://laptop:11434/v1` as the base URL with any API key:

```python
from openai import OpenAI
client = OpenAI(base_url="http://laptop:11434/v1", api_key="ollama")
print(client.chat.completions.create(model="llama3.2:3b", messages=[{"role": "user", "content": "Hi"}]).choices[0].message.content)
```

The app's **Chat** panel streams answers token by token and shows tokens per second.

## Use it from your apps

The container joins the proxy's network (`vfy-proxy`), so apps deployed on the same
server reach it at `http://vercelify-ollama:11434` (`/v1` for OpenAI clients).

Containers can't reach other machines on the network (so an app can't become a way
in). To let apps on a worker use the main server's models, allow the port on that
worker: `CONTAINER_TAILNET_ALLOW_TCP="11434"` in `/etc/vercelify/firewall.conf`, then
`sudo systemctl restart vercelify-firewall`; the apps call `http://10.77.0.1:11434`.

## Several servers

Each server runs its own Ollama. Put big models on the machine with the most memory
and switch to it in the sidebar; the Servers page shows which machines have models.

## CLI

```
sudo vercelify llm                      # status, endpoints, models
sudo vercelify llm install [ssd|hdd]
sudo vercelify llm pull MODEL
sudo vercelify llm rm MODEL
sudo vercelify llm chat MODEL "prompt"
sudo vercelify llm stop|start|restart   # stop frees all model memory
```

Agent API (bearer token): `GET /v1/llm`, `POST /v1/llm/install {tier}`,
`POST /v1/llm/models/pull {model}`, `POST /v1/llm/models/delete {model}`,
`POST /v1/llm/{start|stop|restart|remove}`, `POST /v1/llm/chat {model, messages, stream?, temperature?, max_tokens?}`.
