<p align="center">
  <img src="docs/brand/vercelify-icon.svg" width="112" alt="Vercelify logo" />
</p>

<h1 align="center">Vercelify</h1>

<p align="center">
  <b>Deploy on hardware you own. Understand failures with AI you can run yourself.</b><br/>
  A self-hosted deployment platform for spare laptops and Raspberry Pis, with a Mac app and an open-weight AI build-log assistant.
</p>

<p align="center">
  <a href="https://github.com/shashank03-dev/vercelify-oss/actions/workflows/ci.yml"><img src="https://github.com/shashank03-dev/vercelify-oss/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="CONTRIBUTING.md"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs welcome"></a>
</p>

Vercelify turns a spare Linux computer into a place to deploy websites, APIs and databases.
It brings Git-based deployments, pull request previews, rollbacks and server management
into a desktop app. When a build fails, an open-weight model reads the relevant log and
suggests a **cause**, a **fix** and a **command to check the result**.

The goal is to make self-hosting practical for students and independent developers who
already have usable hardware. The core workflow requires no paid inference API or hosting
subscription. Electricity, internet, hardware, and optional domains or cloud services still
have their own costs.

**Hacktoberfest Hack Day Bengaluru 2026 · Track 1: Best Open-Source AI Project**

[Try the AI assistant](#try-the-ai-assistant) · [Explore the demo](#explore-the-interface) ·
[Install Vercelify](#install-on-your-own-hardware) · [Contribute](#contributing)

## Why this project exists

Deploying a small project should not require a complicated hosting setup. A spare laptop
can run it, but managing builds, networking, storage and failures takes work. Build errors
add another obstacle: a project that works on macOS may fail inside a Linux container
because an import has different capitalization or an environment variable is missing.

Vercelify combines a deployment workflow with help at that point of failure. It uses
Coolify for the hosting engine, adds controls for running on modest hardware, and connects
failed build logs to a model that can run on the developer's computer or server.

## Open-source AI that does useful work

The AI feature answers one concrete question: **“Why did my deploy fail?”**

1. Read a failed deployment's build log, or supply a log file through the CLI.
2. Keep error lines with nearby context and the last 30 lines, within a 5,000-character budget.
3. Send that context to an installed chat model through Ollama.
4. Ask for a short explanation in the same three-part format each time.
5. Review the suggestion, make the change and check it before redeploying.

For the included [example build log](cli/examples/failed-build.log), the output is intended
to look like this:

```text
Cause: The Linux build cannot resolve ../components/Header from app/page.tsx.
Fix: Check the component's filename and make the import match its capitalization.
Check: Run npm run build after correcting the path.
```

This is an illustrative explanation, not a recorded model response. The model only sees
the supplied context; it cannot inspect the repository or prove a fix is correct. The
assistant offers advice and does not edit code or redeploy automatically.

### Model and implementation

| Part | Implementation |
| --- | --- |
| Preferred model | **Gemma 4 E2B**, Ollama tag `gemma4:e2b`. Gemma 4 has open weights under [Apache 2.0](https://ai.google.dev/gemma/apache_2). |
| Local runtime | [Ollama](https://github.com/ollama/ollama), on your computer or the Vercelify server. Local inference keeps the supplied log on your machines. |
| CLI | [`cli/vfy`](cli/vfy): `vfy why APP` reads a failed deployment; `vfy why --log FILE` accepts an existing log without a Vercelify server. |
| Desktop UI | Failed deployment → **Build logs** → **Explain with AI**, implemented in [`AppsView.tsx`](cockpit/src/views/AppsView.tsx). Live responses stream from the server's Ollama. |
| Prompt and context | [`explain.ts`](cockpit/src/lib/explain.ts) defines the prompt, log selection and model preference. The CLI carries the same prompt; [tests](cockpit/src/lib/explain.test.ts) check that they stay in sync. |
| Generation settings | Local explanations use temperature `0.2` and a 400-token output cap. The prompt requests **Cause**, **Fix** and **Check** in fewer than 120 words. |
| Model selection | Prefer `gemma4:e2b`, then another installed Gemma 4 model, then a non-embedding model. Set `VFY_AI_MODEL` to choose a specific model in the CLI. |

The integration uses an existing model; it does not train a new one. Vercelify's work is
the deployment context, log preparation, shared prompt, provider integration and the CLI
and desktop workflows around them. See [the AI guide](docs/AI.md) for details.

### Agent Skill

The repository also includes [`skills/vercelify`](skills/vercelify/SKILL.md), an MIT-licensed
skill packaged with metadata and reference files in the
[Agent Skills format](https://agentskills.io/specification).

It gives skill-aware coding agents instructions for installing Vercelify, using `vfy`,
interpreting a failed build with `vfy why`, and helping a developer fix and redeploy an app.
Copy the `skills/vercelify` directory into your agent's skills location; for example,
`~/.claude/skills/vercelify` for Claude Code. The skill supplies instructions to an existing
agent; it does not include its own model or agent runtime.

## Try the AI assistant

You can run a real model against the example log without installing a hosting server.
You need Git, Bash, `curl`, `jq`, and [Ollama](https://docs.ollama.com/quickstart) installed
and running on your computer.

```bash
git clone https://github.com/shashank03-dev/vercelify-oss.git
cd vercelify-oss

ollama pull gemma4:e2b
./cli/vfy why --log cli/examples/failed-build.log
```

The first model download can take time. Allow several gigabytes of disk space and enough
free memory for inference. The project's AI guide estimates roughly 7 GB for Ollama's
default E2B package and a similar amount of free memory while answering; actual requirements
vary with the model package, quantization and context. An 8 GB server already running apps
may have too little memory available. Use a computer with more free memory or select a
smaller installed chat model:

```bash
VFY_AI_MODEL=YOUR_INSTALLED_MODEL ./cli/vfy why --log cli/examples/failed-build.log
```

On a paired server with Ollama and a chat model installed, use the installed CLI:

```bash
vfy why my-app
```

This fetches a recent failed deployment and runs the explanation on the server. See
[local AI setup](docs/AI.md) for installation and configuration.

### Optional hosted inference

The CLI can also call Gemma 4 through the Gemini API. This is optional; local Ollama is
the default path for the open-source track demo. The hosted path sends the selected log
context to Google and requires an API key and access to the chosen model.

From the repository root:

```bash
cp .env.example .env
# Edit .env and set GEMINI_API_KEY using a key from https://aistudio.google.com/apikey.
set -a
. ./.env
set +a
./cli/vfy why --log cli/examples/failed-build.log --provider gemini
```

The default hosted model is `gemma-4-26b-a4b-it`; override it with `VFY_GEMINI_MODEL`.
The integration is in `why_gemini` in [`cli/vfy`](cli/vfy). Keep `.env` private; it is
excluded from Git. Inspect logs for secrets before sending them to any model provider.

## Explore the interface

To explore the app without a Linux server, use the browser demo. You need Node.js 22
and npm, plus Git to clone the repository if you have not already done so.

```bash
# From the repository root:
cd cockpit
npm ci
VITE_DEMO=1 npm run dev
```

Open [localhost:1420](http://localhost:1420), select `dell-e7450`, and pair using
`K7QF-M2XD`. Open `notes-api` → **Deployments & logs** → the **Error** deployment →
**Build logs** → **Explain with AI**.

**Demo mode uses simulated server data and a canned AI explanation.** It demonstrates
the interface; use the CLI example above to demonstrate real inference.

<p align="center">
  <img src="docs/screenshots/explain-failure.png" width="860" alt="Vercelify's demo interface showing a failed build explanation with Cause, Fix and Check" />
  <br/><sub>Explain with AI in demo mode. A connected server uses live Ollama inference.</sub>
</p>

| Apps | Deployments |
| --- | --- |
| ![Apps interface](docs/screenshots/apps.png) | ![Deployment history](docs/screenshots/deployments.png) |
| **Storage** | **Health** |
| ![Storage management](docs/screenshots/storage.png) | ![Server health](docs/screenshots/health.png) |

## Hack Day: open-source track

This README focuses on **Track 1: Best Open-Source AI Project** at Hacktoberfest Hack Day
Bengaluru, held on 23 October 2026 at Atria Institute of Technology and organized by the
AWS Student Builder Group at Atria Institute of Technology.

The project addresses the track through an open-weight model doing a practical task:
explaining deployment failures. The source, prompt and integration are available in this
repository under the [MIT License](LICENSE), alongside the reusable Agent Skill.

A reproducible walkthrough for reviewers:

1. Open the example log and identify the build failure.
2. Run `./cli/vfy why --log cli/examples/failed-build.log` with Ollama and a model ready.
3. Compare the generated **Cause**, **Fix** and **Check** with the log.
4. Show the shared prompt and log-selection code, then the included Agent Skill.
5. Use the browser demo to show where the feature fits in the deployment workflow,
   identifying the demo response as canned.

Relevant event resources: [Open-Source AI track brief](https://mlh.com/opensource-ai),
[Agent Skills specification](https://agentskills.io/specification),
[Gemma documentation](https://ai.google.dev/gemma/docs/core), and
[Ollama quickstart](https://docs.ollama.com/quickstart).

## What Vercelify provides

| Capability | What it does |
| --- | --- |
| Git-based deployments | Deploy a repository branch and watch for updates. GitHub pull requests can receive preview deployments. |
| Deployment recovery | Inspect build and runtime logs, roll back a deployment, and explain failed builds with a local model. |
| Applications and databases | Manage apps and database services through Coolify, including Dockerfile-based projects. |
| Domains and routing | Use private app URLs, or configure a domain and public HTTPS through Cloudflare Tunnel or direct ingress. |
| Project configuration | Apply supported `vercel.json` redirects, rewrites, headers and cron jobs; add firewall rules with `vercelify.json`. |
| Server care | Monitor CPU, memory, storage, battery and temperature; configure alerts, backups and hardware protections. |
| Multiple machines | Manage connected servers and add Raspberry Pi workers. |
| Local AI | Install and manage Ollama models for chat and deployment explanations. |

Vercelify builds on **Coolify**, rather than replacing its deployment engine. It adds the
desktop control app, host agent, hardware management, configuration integration and AI
workflow. It supports part of Vercel's configuration format; consult the
[feature comparison](docs/VERCEL-PARITY.md) and [configuration guide](docs/PROJECT-CONFIG.md)
for differences and unsupported options.

## Install on your own hardware

### Requirements

- A spare 64-bit Linux laptop or PC. Ubuntu 24.04 LTS is the recommended starting point;
  see the [Raspberry Pi guide](docs/RASPBERRY-PI.md) for Pi setup.
- At least 4 GB RAM and 20 GB free disk space as a starting point for hosting. Builds,
  databases and AI models need additional capacity; 8 GB or more is preferable for hosting.
- A Mac for the desktop control app and a free [Tailscale account](https://tailscale.com).
- An internet connection. Ethernet is preferable for a server.

Back up files before replacing an operating system. The server installer changes system
configuration and installs services; a separate storage disk can be erased if you explicitly
choose to format it. The command below keeps storage on the system disk.

### 1. Prepare the Linux server

Install `curl`, then run the server installer:

```bash
sudo apt update
sudo apt install -y curl
curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install.sh | sudo bash -s -- --storage-device none
```

Follow the prompts and approve the Tailscale sign-in link. Save the Coolify login details
and pairing code printed when installation finishes. Leave the server powered on and
connected to the network. AI installation is optional and can be added later.

### 2. Install the Mac app

```bash
curl -fsSL https://raw.githubusercontent.com/shashank03-dev/vercelify-oss/main/install-mac.sh | bash
```

Sign in to the same Tailscale account, open Vercelify, select the server and enter the
pairing code. If it expires, run `sudo vercelify pair` on the Linux server to issue a new one.

If a downloadable app release is unavailable, the Mac installer offers a source build.
That requires Rust, Node.js 22 and Xcode Command Line Tools. See
[development setup](docs/DEVELOPMENT.md) and [troubleshooting](docs/TROUBLESHOOTING.md).

### 3. Deploy an app

In the Mac app, choose **New app**, enter a repository URL, select a branch and deploy.
Configure any required build settings and environment variables for your project.
Private repositories require repository access configuration.

Apps start with private access through Tailscale. To publish an app, configure a domain
and ingress using the [domain setup guide](docs/COOLIFY.md). GitHub previews and automatic
deployments are covered in the [auto-deploy guide](docs/AUTODEPLOY.md).

## Architecture

```text
Mac app / vfy CLI
       |
       | private connection over Tailscale
       v
Linux server
  ├── Vercelify agent  → system health, storage, services and AI requests
  ├── Coolify         → builds, deployments, containers and databases
  ├── Ollama          → optional local model inference
  └── Traefik         → application routing
       |
       └── Optional public access through Cloudflare Tunnel or direct ingress
```

The desktop app uses Tauri v2, React and TypeScript. The host agent is written in Rust;
installation and administration use shell scripts. Deployment builds use Coolify's
Nixpacks or Docker workflows. See [architecture](docs/ARCHITECTURE.md) for the full design.

The management connection is private to the tailnet. Paired clients authenticate to the
agent with revocable bearer tokens. Public app access is configured separately. See
[security reporting](SECURITY.md) and the [backup guide](docs/BACKUPS.md) before relying
on a server for important data.

## Development and verification

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites and setup. To build the
Mac app from the repository root:

```bash
cd cockpit
npm ci
npm run tauri build
```

The repository includes the following checks. These commands describe the available
suites; they do not imply a particular checkout has passed them.

| Check | Command from repository root |
| --- | --- |
| Rust workspace | `cargo test --workspace --all-features` |
| Host and CLI scripts | `bats host/tests` |
| Frontend types | `cd cockpit && npm run typecheck` |
| Frontend unit tests | `cd cockpit && npm test` |
| Browser demo flows | `cd cockpit && npm run e2e:demo` |
| Coolify contract | `sudo host/tests/contract/coolify.sh` |
| Routing contract | `sudo host/tests/contract/routing.sh` |

Rust workspace checks require the Tauri prerequisites and a `cockpit/dist` directory;
see [development instructions](docs/DEVELOPMENT.md). Contract and installer integration
tests require a suitable Docker environment. The development guide also covers live,
desktop and arm64 tests. CI configuration is in [`.github/workflows`](.github/workflows).

### Repository layout

```text
agent/                  Rust host agent
crates/vercelify-core/  Shared types and host probes
cockpit/                Tauri desktop app and React interface
cli/vfy                 Mac/Linux command-line client
cli/examples/           Sample build log for the AI walkthrough
host/                   Installer, admin commands, services and tests
skills/vercelify/       Agent Skill and reference documentation
website/                Project website source
docs/                   Architecture, setup and feature guides
install.sh              Linux server bootstrap
install-mac.sh          Mac app installer
```

## Documentation

| Guide | Covers |
| --- | --- |
| [AI](docs/AI.md) | Ollama setup, deployment explanations, model selection and API access |
| [Coolify and domains](docs/COOLIFY.md) | Hosting setup and public ingress |
| [Auto-deploy](docs/AUTODEPLOY.md) | Branch updates and GitHub previews |
| [Project configuration](docs/PROJECT-CONFIG.md) | `vercel.json`, firewall rules and cron jobs |
| [Vercel comparison](docs/VERCEL-PARITY.md) | Supported features and differences |
| [Backups](docs/BACKUPS.md) | Backup configuration and restore workflows |
| [Servers](docs/SERVERS.md) · [Raspberry Pi](docs/RASPBERRY-PI.md) | Additional machines and workers |
| [Second screen](docs/DISPLAY.md) | Optional display features |
| [Troubleshooting](docs/TROUBLESHOOTING.md) | Diagnosis and recovery |
| [Development](docs/DEVELOPMENT.md) | Local setup and test environments |

## Contributing

Bug reports, documentation improvements and code contributions are welcome. Read
[CONTRIBUTING.md](CONTRIBUTING.md) for setup, conventions and checks. For a larger change,
open an issue first to discuss the approach. Please follow the
[Code of Conduct](CODE_OF_CONDUCT.md), and report vulnerabilities privately as described
in [SECURITY.md](SECURITY.md).

## License and acknowledgments

Vercelify's source code and included Agent Skill are released under the [MIT License](LICENSE).
Gemma 4 model weights are provided separately under [Apache 2.0](https://ai.google.dev/gemma/apache_2).
Other dependencies retain their respective licenses; see their upstream repositories.

Vercelify builds on the work of [Coolify](https://github.com/coollabsio/coolify),
[Ollama](https://github.com/ollama/ollama), [Tailscale](https://github.com/tailscale/tailscale),
[Traefik](https://github.com/traefik/traefik), [Tauri](https://github.com/tauri-apps/tauri),
and the wider open-source community. Vercelify is an independent project and is not
affiliated with Vercel.
