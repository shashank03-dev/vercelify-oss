import { BrainCircuit, Check, Download, HardDrive, Laptop, Play, RotateCcw, Send, Square, Trash2, Zap } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useToast } from "../components/Toast";
import {
  Button,
  Callout,
  Card,
  CodeBlock,
  cx,
  EmptyState,
  ErrorNote,
  IconButton,
  InlineCode,
  Loading,
  PageHeader,
  PixelWave,
  ProgressBar,
  Segmented,
  Select,
  ShimmerText,
  StatusDot,
  StatusText,
  TextInput,
} from "../components/ui";
import { agent } from "../lib/api";
import { bytes, pct } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { CATALOG, fit, memoryNeeded, tokensPerSecond, type Fit } from "../lib/models";
import { agentHost, useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import type { LlmChatMessage, LlmStatus, LlmTask, StorageTier } from "../types";

export function AiView() {
  const { settings } = useSession();
  const [fast, setFast] = useState(false);
  const status = usePoll(() => agent.llm(), fast ? 2500 : 8000);
  const sys = usePoll(() => agent.system(), 0);
  const s = status.data;
  const busy = !!s?.tasks.some((t) => !t.done);
  useEffect(() => setFast(busy), [busy]);

  return (
    <>
      <PageHeader
        title="AI models"
        subtitle={
          <>
            Run open models like Llama, Qwen and Gemma on <span className="text-fg">{settings.hostname}</span> and use them from this Mac, your
            apps and any tool that speaks the OpenAI API. Nothing leaves your machines.
          </>
        }
        actions={s?.installed ? <RuntimeControls s={s} refresh={status.refresh} /> : undefined}
      />
      <ErrorNote error={status.error} className="mb-4" />

      {!s ? (
        <Loading rows={4} />
      ) : !s.installed || (s.tasks.some((t) => t.kind === "install" && !t.done) && !s.ready) ? (
        <InstallCard s={s} hasHdd={!!sys.data?.disks.some((d) => d.role === "storage")} refresh={status.refresh} onStarted={() => setFast(true)} />
      ) : (
        <>
          {!s.firewall_active && (
            <Callout className="mb-4" title="Port 11434 is not firewalled">
              The Vercelify firewall isn't running on this server, so anyone on its local network could use the models. Run{" "}
              <InlineCode>sudo systemctl enable --now vercelify-firewall</InlineCode> there.
            </Callout>
          )}
          {s.error && <Callout className="mb-4">{s.error}</Callout>}
          <div className="grid grid-cols-1 gap-6 lg:grid-cols-[1.25fr_1fr]">
            <div className="min-w-0 space-y-6">
              <Chat s={s} />
              <UseFromMac s={s} host={agentHost(settings)} />
            </div>
            <div className="min-w-0 space-y-6">
              <Models s={s} refresh={status.refresh} onStarted={() => setFast(true)} />
            </div>
          </div>
        </>
      )}
    </>
  );
}

function RuntimeControls({ s, refresh }: { s: LlmStatus; refresh: () => Promise<void> }) {
  const toast = useToast();
  const [busy, setBusy] = useState<string>();
  const act = async (a: "start" | "stop" | "restart") => {
    setBusy(a);
    try {
      await agent.llmAction(a);
      toast.ok(a === "stop" ? "Ollama stopped" : "Ollama is starting");
      await refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(undefined);
    }
  };
  return (
    <>
      <StatusText
        health={s.ready ? "ok" : s.running ? "warn" : "bad"}
        label={s.ready ? `Ollama ${s.version ?? ""}`.trim() : s.running ? "Starting" : "Stopped"}
        testId="llm-status"
        className="mr-1 text-[12px] text-fg-muted"
      />
      <IconButton label="Restart Ollama" disabled={!!busy} onClick={() => void act("restart")}>
        <RotateCcw />
      </IconButton>
      {s.running ? (
        <IconButton label="Stop Ollama (frees memory)" disabled={!!busy} onClick={() => void act("stop")}>
          <Square />
        </IconButton>
      ) : (
        <Button variant="primary" icon={<Play />} busy={busy === "start"} onClick={() => void act("start")}>
          Start
        </Button>
      )}
    </>
  );
}

function InstallCard({ s, hasHdd, refresh, onStarted }: { s: LlmStatus; hasHdd: boolean; refresh: () => Promise<void>; onStarted: () => void }) {
  const toast = useToast();
  const [tier, setTier] = useState<StorageTier>(hasHdd ? "hdd" : "ssd");
  const [busy, setBusy] = useState(false);
  const task = s.tasks.find((t) => t.kind === "install");
  const running = !!task && !task.done;
  useEffect(() => setTier(hasHdd ? "hdd" : "ssd"), [hasHdd]);

  async function install() {
    setBusy(true);
    try {
      await agent.llmInstall(tier);
      onStarted();
      await refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card testId="llm-install">
      <EmptyState
        icon={<BrainCircuit />}
        title="Run AI models on this server"
        action={
          running ? (
            <div className="flex items-center gap-2 text-[13px] text-fg-muted" data-testid="llm-installing">
              <PixelWave size={3} pixel={4} gap={1.5} /> <ShimmerText>{`${task.status}…`}</ShimmerText>
            </div>
          ) : (
            <div className="flex flex-col items-center gap-3">
              <Segmented
                label="model storage"
                value={tier}
                onChange={setTier}
                options={[
                  { value: "ssd", label: "Models on SSD (fast to load)", testId: "llm-tier-ssd" },
                  ...(hasHdd ? [{ value: "hdd" as const, label: "Models on HDD (more room)", testId: "llm-tier-hdd" }] : []),
                ]}
              />
              <Button variant="primary" icon={<Download />} busy={busy} onClick={() => void install()} data-testid="llm-install-button">
                Install Ollama
              </Button>
            </div>
          )
        }
      >
        Vercelify runs <span className="text-fg">Ollama</span> in a container tuned for old hardware: one model in memory at a time, unloaded after 5
        idle minutes, answers capped at 1,024 tokens, and lower CPU priority than your apps. The Ollama image is about a 4 GB download (it
        bundles GPU libraries) and needs roughly 12 GB free on the system disk; models can live on the HDD.
        {task?.error && <span className="mt-2 block text-bad">Last attempt failed: {task.error}</span>}
      </EmptyState>
      {s.memory_total_bytes > 0 && (
        <p className="pb-2 text-center text-xs text-fg-dim">
          {bytes(s.memory_available_bytes)} of {bytes(s.memory_total_bytes)} memory free right now.
        </p>
      )}
    </Card>
  );
}

const FIT_LABEL: Record<Fit, { text: string; tone: "ok" | "warn" | "bad" }> = {
  ok: { text: "fits", tone: "ok" },
  tight: { text: "tight", tone: "warn" },
  "too-big": { text: "too big", tone: "bad" },
};

function PullProgress({ t }: { t: LlmTask }) {
  const p = t.total ? pct(t.completed ?? 0, t.total) : null;
  return (
    <div className="mt-2">
      <div className="mb-1 flex justify-between text-[11px] text-fg-dim">
        {t.done ? <span className="truncate">{t.status}</span> : <ShimmerText className="truncate">{t.status}</ShimmerText>}
        {p != null && (
          <span className="tabular-nums">
            {bytes(t.completed ?? 0)} / {bytes(t.total)}
          </span>
        )}
      </div>
      <ProgressBar value={p ?? 3} active={!t.done} label={`Downloading ${t.target}`} />
    </div>
  );
}

function Models({ s, refresh, onStarted }: { s: LlmStatus; refresh: () => Promise<void>; onStarted: () => void }) {
  const toast = useToast();
  const [custom, setCustom] = useState("");
  const have = new Set(s.models.map((m) => m.name));
  const loaded = new Set(s.loaded.map((m) => m.name));
  const pulls = s.tasks.filter((t) => t.kind === "pull");
  const pulling = (name: string) => pulls.find((t) => t.target === name && !t.done);

  async function pull(name: string) {
    try {
      await agent.llmPull(name.trim());
      onStarted();
      toast.info(`Downloading ${name}`);
      await refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    }
  }

  async function del(name: string) {
    try {
      await agent.llmDelete(name);
      toast.ok(`Deleted ${name}`);
      await refresh();
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    }
  }

  const failed = pulls.filter((t) => t.done && t.error);
  const extraPulls = pulls.filter((t) => !t.done && !CATALOG.some((c) => c.name === t.target) && !have.has(t.target));

  return (
    <>
      <Card title="On this server" icon={<HardDrive />} description={s.data_path ?? undefined} flush testId="llm-models">
        {s.models.length === 0 && extraPulls.length === 0 ? (
          <EmptyState icon={<BrainCircuit />} title="No models yet">
            Pick one below. <span className="text-fg">Llama 3.2 3B</span> is a good start on an 8 GB laptop.
          </EmptyState>
        ) : (
          <ul className="divide-y divide-line">
            {s.models.map((m) => (
              <li key={m.name} className="group flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-fg/[0.025]" data-testid={`model-${m.name}`}>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 font-mono text-[12.5px] text-fg">
                    {m.name}
                    {loaded.has(m.name) && (
                      <span className="inline-flex items-center gap-1.5 font-sans text-[11px] text-fg-muted">
                        <StatusDot health="ok" />
                        in memory
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 text-[11px] text-fg-dim">
                    {[m.parameter_size, m.quantization, bytes(m.size_bytes)].filter(Boolean).join(" · ")}
                  </div>
                </div>
                <IconButton label={`Delete ${m.name}`} className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100" onClick={() => void del(m.name)}>
                  <Trash2 />
                </IconButton>
              </li>
            ))}
            {extraPulls.map((t) => (
              <li key={t.target} className="px-4 py-2.5">
                <div className="font-mono text-[12.5px]">{t.target}</div>
                <PullProgress t={t} />
              </li>
            ))}
          </ul>
        )}
        {failed.map((t) => (
          <ErrorNote key={t.target} className="m-3" error={{ message: `${t.target}: ${t.error}` }} />
        ))}
      </Card>

      <Card title="Download a model" icon={<Download />} description="Sized for this machine's memory" flush testId="llm-catalog">
        <ul className="divide-y divide-line">
          {CATALOG.map((m) => {
            const f = fit(m.size, s.memory_available_bytes, s.memory_total_bytes);
            const t = pulling(m.name);
            return (
              <li key={m.name} className="px-4 py-2.5">
                <div className="flex items-center gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 text-[13px] font-medium">
                      {m.label}
                      <span className="font-mono text-[11px] font-normal text-fg-dim">{m.name}</span>
                    </div>
                    <div className="mt-0.5 text-xs text-fg-muted">{m.blurb}</div>
                  </div>
                  <span className="hidden text-right text-[11px] text-fg-dim sm:block" title={`about ${bytes(memoryNeeded(m.size))} of memory while in use`}>
                    {bytes(m.size)}
                    <br />
                    <span className={cx(FIT_LABEL[f].tone === "ok" ? "text-fg-dim" : FIT_LABEL[f].tone === "warn" ? "text-warn" : "text-bad")}>{FIT_LABEL[f].text}</span>
                  </span>
                  {have.has(m.name) ? (
                    <span className="inline-flex h-7 items-center gap-1 px-2.5 text-xs text-fg-muted">
                      <Check className="size-3.5" />
                      Ready
                    </span>
                  ) : (
                    <Button size="sm" icon={<Download />} busy={!!t} disabled={!s.ready} onClick={() => void pull(m.name)} data-testid={`pull-${m.name}`}>
                      {t ? "Getting" : "Get"}
                    </Button>
                  )}
                </div>
                {t && <PullProgress t={t} />}
              </li>
            );
          })}
        </ul>
        <form
          className="flex gap-2 border-t border-line p-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (custom.trim()) void pull(custom).then(() => setCustom(""));
          }}
        >
          <TextInput
            value={custom}
            onChange={(e) => setCustom(e.target.value)}
            placeholder="Any model from ollama.com/library, e.g. mistral:7b"
            spellCheck={false}
            className="font-mono"
            data-testid="custom-model"
          />
          <Button type="submit" disabled={!custom.trim() || !s.ready}>
            Download
          </Button>
        </form>
      </Card>
    </>
  );
}

interface Turn extends LlmChatMessage {
  tps?: number | null;
  error?: string;
}

function Chat({ s }: { s: LlmStatus }) {
  const [model, setModel] = useState(() => s.models[0]?.name ?? "");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState<{ cancel: () => void } | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const names = useMemo(() => s.models.map((m) => m.name), [s.models]);

  // Navigation and server switches must release the server's model generation.
  useEffect(() => () => streaming?.cancel(), [streaming]);
  useEffect(() => {
    if (!names.includes(model)) setModel(names[0] ?? "");
  }, [names, model]);
  useEffect(() => {
    // Newer browsers return a Promise here; an effect may only return a cleanup.
    void endRef.current?.scrollIntoView({ block: "nearest" });
  }, [turns]);

  function send() {
    const text = input.trim();
    if (!text || !model || streaming) return;
    const history: LlmChatMessage[] = [...turns.filter((t) => !t.error).map(({ role, content }) => ({ role, content })), { role: "user", content: text }];
    setTurns((t) => [...t, { role: "user", content: text }, { role: "assistant", content: "" }]);
    setInput("");
    const patchLast = (f: (t: Turn) => Turn) => setTurns((all) => [...all.slice(0, -1), f(all[all.length - 1]!)]);
    const handle = agent.llmChat({ model, messages: history }, (line) => {
      let v: { message?: { content?: string }; done?: boolean; error?: string; eval_count?: number; eval_duration?: number };
      try {
        v = JSON.parse(line);
      } catch {
        return;
      }
      if (v.error) patchLast((t) => ({ ...t, error: v.error }));
      if (v.message?.content) patchLast((t) => ({ ...t, content: t.content + v.message!.content }));
      if (v.done) patchLast((t) => ({ ...t, tps: tokensPerSecond(v) }));
    });
    setStreaming(handle);
    handle.done
      .catch((e) => patchLast((t) => ({ ...t, error: toCockpitError(e).message })))
      .finally(() => {
        setStreaming(null);
        patchLast((t) => (t.role === "assistant" && !t.content && !t.error ? { ...t, content: "(stopped)" } : t));
      });
  }

  return (
    <Card
      title="Chat"
      icon={<BrainCircuit />}
      description="Answers stream from the server as they are generated"
      testId="llm-chat"
      actions={
        <>
          <Select value={model} onChange={(e) => setModel(e.target.value)} disabled={!names.length || !!streaming} aria-label="Model" className="w-52" data-testid="chat-model">
            {names.length === 0 && <option value="">No models yet</option>}
            {names.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </Select>
          <IconButton label="Clear chat" disabled={!turns.length || !!streaming} onClick={() => setTurns([])}>
            <Trash2 />
          </IconButton>
        </>
      }
    >
      <div className="max-h-[46vh] min-h-40 space-y-3 overflow-y-auto pr-1" data-testid="chat-log">
        {turns.length === 0 && (
          <p className="py-10 text-center text-[13px] text-fg-dim">
            {names.length ? "Ask anything. The first answer takes a little longer while the model loads." : "Download a model to start chatting."}
          </p>
        )}
        {turns.map((t, i) => (
          <div key={i} className={cx("flex", t.role === "user" ? "justify-end" : "justify-start")}>
            <div
              className={cx(
                "selectable max-w-[85%] rounded-xl px-3.5 py-2 text-[13px] leading-relaxed whitespace-pre-wrap",
                t.role === "user" ? "bg-surface-3 text-fg" : "border border-line bg-surface-2 text-fg",
              )}
            >
              {t.content ||
                (t.error ? (
                  ""
                ) : streaming && i === turns.length - 1 ? (
                  <span className="inline-flex items-center gap-2 py-0.5">
                    <PixelWave size={3} pixel={3} gap={1.5} />
                    <ShimmerText>Thinking…</ShimmerText>
                  </span>
                ) : (
                  ""
                ))}
              {t.error && <div className="text-bad">{t.error}</div>}
              {t.tps != null && (
                <div className="mt-1 flex items-center gap-1 text-[10.5px] text-fg-dim">
                  <Zap className="size-3" /> {t.tps.toFixed(1)} tokens/s
                </div>
              )}
            </div>
          </div>
        ))}
        <div ref={endRef} />
      </div>
      <form
        className="mt-3 flex gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <TextInput
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder={model ? `Message ${model}…` : "Download a model first"}
          disabled={!model}
          className="h-9"
          data-testid="chat-input"
        />
        {streaming ? (
          <Button className="h-9" icon={<Square />} onClick={() => streaming.cancel()}>
            Stop
          </Button>
        ) : (
          <Button type="submit" variant="primary" className="h-9" icon={<Send />} disabled={!input.trim() || !model} data-testid="chat-send">
            Send
          </Button>
        )}
      </form>
    </Card>
  );
}

type Snippet = "openai" | "curl" | "ollama" | "apps";

function UseFromMac({ s, host }: { s: LlmStatus; host: string }) {
  const [kind, setKind] = useState<Snippet>("openai");
  const base = s.endpoints[0] ?? `http://${host}:${s.port}`;
  const model = s.models[0]?.name ?? "llama3.2:3b";
  const code: Record<Snippet, string> = {
    openai: `# Any OpenAI-compatible client (Python, JS, Cursor, Continue, Open WebUI…)
from openai import OpenAI

client = OpenAI(base_url="${base}/v1", api_key="ollama")  # key is ignored
reply = client.chat.completions.create(
    model="${model}",
    messages=[{"role": "user", "content": "Hello from my Mac"}],
)
print(reply.choices[0].message.content)`,
    curl: `curl ${base}/api/chat -d '{
  "model": "${model}",
  "messages": [{ "role": "user", "content": "Hello from my Mac" }],
  "stream": false
}'`,
    ollama: `# Use the Ollama CLI on your Mac with the models on ${host}
brew install ollama
OLLAMA_HOST=${base} ollama run ${model}`,
    apps: `# From apps deployed on this server (they share its vfy-proxy network)
OLLAMA_BASE_URL=http://vercelify-ollama:11434
OPENAI_BASE_URL=http://vercelify-ollama:11434/v1`,
  };
  return (
    <Card title="Use it from your Mac" icon={<Laptop />} description="Private to your Vercelify network — no API key, no cloud" testId="llm-use">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <Segmented
          label="snippet"
          value={kind}
          onChange={setKind}
          options={[
            { value: "openai", label: "OpenAI API" },
            { value: "curl", label: "curl" },
            { value: "ollama", label: "Ollama CLI" },
            { value: "apps", label: "Your apps" },
          ]}
        />
        <span className="font-mono text-[11.5px] text-fg-muted">{base}</span>
      </div>
      <CodeBlock code={code[kind]} testId="llm-snippet" />
    </Card>
  );
}
