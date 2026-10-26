import { Sparkles, Square, Zap } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button, Callout, InlineCode, PixelWave, ShimmerText } from "./ui";
import { agentOn } from "../lib/api";
import { EXPLAIN_MAX_TOKENS, EXPLAIN_MODEL, explainMessages, pickExplainModel, type ExplainLine } from "../lib/explain";
import { tokensPerSecond } from "../lib/models";
import { useNav } from "../lib/nav";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";

type Explanation = { model?: string; text: string; tps?: number | null; error?: { title: string; detail: string } };

/** "Why did my deploy fail?": the server's own model (Gemma 4 E2B by default) reads the log. */
export function ExplainFailure({
  deployment,
  lines,
}: {
  deployment: { application_name: string; commit: string | null; commit_message: string | null };
  lines: ExplainLine[];
}) {
  const { settings } = useSession();
  const agent = agentOn(settings.agent_url);
  const { go } = useNav();
  const [ex, setEx] = useState<Explanation>();
  const [streaming, setStreaming] = useState<{ cancel: () => void } | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => () => streaming?.cancel(), [streaming]);
  useEffect(() => {
    // Newer browsers return a Promise here; an effect may only return a cleanup.
    if (ex) void panelRef.current?.scrollIntoView({ block: "nearest" });
  }, [ex]);

  async function explain() {
    setEx({ text: "" });
    const fail = (title: string, detail: string) => setEx((e) => ({ ...e, text: e?.text ?? "", error: { title, detail } }));
    let model: string | undefined;
    try {
      const s = await agent.llm();
      // Model discovery may finish after navigation or closing the log dialog.
      if (!mounted.current) return;
      if (!s.installed) return fail("No local AI on this server yet", `Install it in AI models, then download ${EXPLAIN_MODEL}. Or on the server: sudo vercelify llm install`);
      if (!s.ready) return fail("Ollama isn't answering", "Start it from AI models, or on the server: sudo vercelify llm start");
      model = pickExplainModel(s.models.map((m) => m.name));
      if (!model) return fail("No model downloaded yet", `Download ${EXPLAIN_MODEL} in AI models, or on the server: sudo vercelify llm pull ${EXPLAIN_MODEL}`);
    } catch (e) {
      return fail("Can't reach the agent", toCockpitError(e).message);
    }
    const messages = explainMessages(lines, { app: deployment.application_name, commit: deployment.commit, commitMessage: deployment.commit_message });
    setEx({ model, text: "" });
    const handle = agent.llmChat({ model, messages, max_tokens: EXPLAIN_MAX_TOKENS, temperature: 0.2 }, (line) => {
      let v: { message?: { content?: string }; done?: boolean; error?: string; eval_count?: number; eval_duration?: number };
      try {
        v = JSON.parse(line);
      } catch {
        return;
      }
      if (v.error) {
        const missing = /not found/i.test(v.error);
        fail(v.error, missing ? `Download it in AI models, or on the server: sudo vercelify llm pull ${model}` : "Try again, or check AI models on the server.");
      }
      if (v.message?.content) setEx((e) => ({ ...e, text: (e?.text ?? "") + v.message!.content }));
      if (v.done) setEx((e) => ({ ...e, text: e?.text ?? "", tps: tokensPerSecond(v) }));
    });
    setStreaming(handle);
    handle.done.catch((e) => fail("The answer stopped", toCockpitError(e).message)).finally(() => setStreaming(null));
  }

  return (
    <div ref={panelRef} className="mt-4 rounded-lg border border-line bg-surface-2 p-3.5" data-testid="explain-panel">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 text-[13px] font-medium text-fg">
            <Sparkles className="size-3.5 text-accent" /> Why did this deploy fail?
          </div>
          <p className="text-[12px] text-fg-dim">
            An open model on {settings.hostname} reads the log. Nothing leaves your machines.
          </p>
        </div>
        {streaming ? (
          <Button size="sm" icon={<Square />} onClick={() => streaming.cancel()}>
            Stop
          </Button>
        ) : (
          <Button size="sm" variant="primary" icon={<Sparkles />} onClick={() => void explain()} data-testid="explain-failure">
            {ex ? "Explain again" : "Explain with AI"}
          </Button>
        )}
      </div>
      {ex && (
        <div className="mt-3 border-t border-line pt-3" data-testid="explain-answer">
          {ex.text ? (
            <ExplainText text={ex.text} />
          ) : (
            streaming && (
              <span className="inline-flex items-center gap-2 py-0.5 text-[13px]">
                <PixelWave size={3} pixel={3} gap={1.5} />
                <ShimmerText>{ex.model ? `${ex.model} is reading the log…` : "Looking for a model…"}</ShimmerText>
              </span>
            )
          )}
          {ex.error && (
            <Callout tone="bad" title={ex.error.title} className={ex.text ? "mt-3" : undefined}>
              {ex.error.detail}{" "}
              <button type="button" className="text-accent hover:underline" onClick={() => go("ai")}>
                Open AI models
              </button>
            </Callout>
          )}
          {ex.model && !ex.error && (ex.tps != null || !streaming) && ex.text && (
            <div className="mt-2 flex items-center gap-1 text-[11px] text-fg-dim" data-testid="explain-meta">
              <Zap className="size-3" /> {ex.model} on {settings.hostname}
              {ex.tps != null && ` · ${ex.tps.toFixed(1)} tokens/s`}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** The model's Cause / Fix / Check answer: bold labels and `code`, nothing else. */
function ExplainText({ text }: { text: string }) {
  return (
    <div className="selectable space-y-1.5 text-[13px] leading-relaxed text-fg">
      {text.split("\n").map((line, i) => {
        const m = /^\s*\*\*([^*]+):?\*\*:?\s*(.*)$/.exec(line);
        return (
          <p key={i} className={line.trim() ? undefined : "hidden"}>
            {m && <span className="font-medium text-accent">{m[1]!.replace(/:$/, "")}: </span>}
            {(m ? m[2]! : line).split(/(`[^`]+`)/).map((part, j) =>
              part.length > 2 && part.startsWith("`") && part.endsWith("`") ? <InlineCode key={j}>{part.slice(1, -1)}</InlineCode> : part,
            )}
          </p>
        );
      })}
    </div>
  );
}
