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

## Tuned for old laptops

The agent runs Ollama in a container named `vercelify-ollama` with:

| Setting | Why |
| --- | --- |
| `OLLAMA_MAX_LOADED_MODELS=1`, `OLLAMA_NUM_PARALLEL=1` | One model and one answer at a time fits in 8 GB next to Coolify and your apps. |
| `OLLAMA_KEEP_ALIVE=5m` | The model leaves memory five idle minutes after the last request. |
| `--cpu-shares 512` | Half the CPU weight of other containers: web apps stay responsive while a model thinks. |
| Answers capped at 1,024 tokens | Small models can loop until the context fills, which takes many minutes on an old CPU. Send `max_tokens` (up to 8192) to raise it. Stopping a chat (or disconnecting) cancels generation on the server. |
| Power guard | When the CPU passes 92 °C the guard pauses non-core containers, Ollama included. |

The **Download a model** list in the app shows which models fit the server's free
memory right now (*fits*, *tight* if other apps must give some back, *too big*).
Good starting points: `llama3.2:3b` (8 GB laptops), `gemma3:1b` or `qwen2.5:1.5b`
(4 GB), `qwen2.5-coder:1.5b` for code, `nomic-embed-text` for search/RAG.

## Use it from your Mac

Port 11434 is published on the server and kept private to your tailnet by the
Vercelify firewall (the app warns if the firewall is off). Replace `laptop` with your
server's Tailscale name:

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

The container joins Coolify's `coolify` network, so apps deployed on the same server
reach it at `http://vercelify-ollama:11434` (`/v1` for OpenAI clients). Apps on other
servers in the pool use the server's Tailscale address.

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
