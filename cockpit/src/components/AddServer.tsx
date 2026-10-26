import { ArrowLeft, ArrowRight, BrainCircuit, Check, Layers, Server, Terminal } from "lucide-react";
import { useEffect, useState } from "react";
import { agentOn, isWorker } from "../lib/api";
import { usePoll } from "../lib/hooks";
import { installCommand, validHostname } from "../lib/pool";
import { toCockpitError } from "../lib/transport";
import type { CockpitSettings } from "../types";
import { HealthChecks } from "./HealthChecks";
import { PairForm } from "./PairForm";
import { Button, Callout, CodeBlock, cx, Field, InlineCode, Select, StatusDot, TextInput } from "./ui";

type Kind = "server" | "worker";
type Step = "choose" | "install" | "finish";

const KINDS: { value: Kind; title: string; text: string; icon: typeof Server }[] = [
  {
    value: "server",
    title: "A separate server",
    text: "Runs its own apps and databases. Best with 8 GB of RAM or more. Switch to it from the sidebar.",
    icon: Server,
  },
  {
    value: "worker",
    title: "A worker for a main server",
    text: "Docker and the agent only — fine on 2–4 GB or a Raspberry Pi. Your main server places apps on it.",
    icon: Layers,
  },
];

/**
 * Adding a machine, start to finish: choose its role, run the generated
 * command on it (a worker's carries its main server's invite, so it joins
 * that server's network and runs apps for it), wait for it to show up,
 * type its code and see its health checks.
 */
export function AddServerWizard({
  servers,
  onAdded,
  onClose,
}: {
  servers: CockpitSettings[];
  onAdded: (s: CockpitSettings) => Promise<void>;
  onClose: () => void;
}) {
  const mains = servers.filter((s) => !isWorker(s));
  const [step, setStep] = useState<Step>("choose");
  const [kind, setKind] = useState<Kind>(mains.length ? "worker" : "server");
  const [hostname, setHostname] = useState("");
  const [ai, setAi] = useState(false);
  const [installed, setInstalled] = useState(false);
  const [added, setAdded] = useState<CockpitSettings | null>(null);
  const [note, setNote] = useState<string>();
  // A worker's main server, and the single-use invite it joins with.
  const [main, setMain] = useState(mains[0]?.agent_url ?? "");
  const [invite, setInvite] = useState<{ token?: string; error?: string }>({});
  const mainS = mains.find((m) => m.agent_url === main);
  useEffect(() => {
    if (step !== "install" || kind !== "worker" || !mainS) return;
    let live = true;
    setInvite({});
    agentOn(mainS.agent_url)
      .meshInvite("server")
      .then(
        (i) => live && setInvite({ token: i.token }),
        (e) => live && setInvite({ error: toCockpitError(e).message }),
      );
    return () => {
      live = false;
    };
  }, [step, kind, mainS]);

  const reset = () => {
    setStep("choose");
    setAdded(null);
    setNote(undefined);
    setInstalled(false);
    setHostname("");
  };

  return (
    <div data-testid="add-server-wizard">
      <ol className="mb-5 flex items-center gap-2 text-xs" aria-label="Steps">
        {(["choose", "install", "finish"] as Step[]).map((s, i) => {
          const idx = ["choose", "install", "finish"].indexOf(step);
          return (
            <li key={s} className="flex items-center gap-2">
              {i > 0 && <span className="h-px w-6 bg-line-strong" />}
              <span
                className={cx(
                  "flex size-5 items-center justify-center rounded-full border text-[10px] font-semibold tabular-nums",
                  i < idx ? "border-ok/40 bg-ok/10 text-ok" : i === idx ? "border-fg/40 text-fg" : "border-line-strong text-fg-dim",
                )}
              >
                {i < idx ? <Check className="size-3" /> : i + 1}
              </span>
              <span className={i === idx ? "text-fg" : "text-fg-dim"}>{["Choose", "Install & connect", "Finish"][i]}</span>
            </li>
          );
        })}
      </ol>

      {step === "choose" && (
        <div className="space-y-4">
          <div role="radiogroup" aria-label="Kind of machine" className="grid grid-cols-2 gap-2.5">
            {KINDS.map((k) => {
              const active = k.value === kind;
              const Icon = k.icon;
              const disabled = k.value === "worker" && mains.length === 0;
              return (
                <button
                  key={k.value}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  disabled={disabled}
                  data-testid={`kind-${k.value}`}
                  onClick={() => setKind(k.value)}
                  className={cx(
                    "rounded-lg border p-3 text-left transition disabled:opacity-45",
                    active ? "border-fg/50 bg-surface-2 ring-3 ring-fg/10" : "border-line hover:border-line-strong",
                  )}
                >
                  <div className="flex items-center gap-2 text-[13px] font-medium">
                    <Icon className="size-4 text-fg-muted" />
                    {k.title}
                  </div>
                  <p className="mt-1 text-xs leading-relaxed text-fg-muted">{disabled ? "Pair a main server first." : k.text}</p>
                </button>
              );
            })}
          </div>
          <div className="grid grid-cols-2 gap-3">
            <Field label="Name (optional)" hint={validHostname(hostname.trim().toLowerCase()) ? "Defaults to the machine's hostname" : "Lowercase letters, digits and dashes"}>
              <TextInput value={hostname} onChange={(e) => setHostname(e.target.value)} placeholder="old-dell" spellCheck={false} data-testid="new-hostname" />
            </Field>
            <label className="flex cursor-pointer items-start gap-2.5 self-end rounded-lg border border-line bg-surface-2 px-3 py-2.5 text-[13px]">
              <input type="checkbox" checked={ai} onChange={(e) => setAi(e.target.checked)} className="mt-0.5 accent-[var(--accent)]" data-testid="new-ai" />
              <span>
                <span className="inline-flex items-center gap-1.5 font-medium text-fg">
                  <BrainCircuit className="size-3.5" /> Also set up AI models
                </span>
                <span className="mt-0.5 block text-xs text-fg-muted">Ollama plus a model sized for its memory (~4 GB download)</span>
              </span>
            </label>
          </div>
          {kind === "worker" && mains.length > 1 && (
            <label className="flex flex-col gap-1.5 text-xs font-medium text-fg-muted">
              Main server it works for
              <Select value={main} onChange={(e) => setMain(e.target.value)} data-testid="worker-main">
                {mains.map((m) => (
                  <option key={m.agent_url} value={m.agent_url}>
                    {m.hostname}
                  </option>
                ))}
              </Select>
            </label>
          )}
          <div className="flex items-center justify-between gap-2">
            <button
              type="button"
              className="text-xs text-fg-dim underline-offset-2 hover:text-fg-muted hover:underline"
              onClick={() => (setInstalled(true), setStep("install"))}
              data-testid="already-installed"
            >
              It's already installed — just connect it
            </button>
            <Button
              variant="primary"
              icon={<ArrowRight />}
              disabled={!validHostname(hostname.trim().toLowerCase())}
              onClick={() => (setInstalled(false), setStep("install"))}
              data-testid="wizard-next"
            >
              Next
            </Button>
          </div>
        </div>
      )}

      {step === "install" && (
        <div className="space-y-4">
          {!installed && (
            <div className="space-y-2">
              <div className="flex items-center gap-2 text-[13px] font-medium">
                <Terminal className="size-3.5 text-fg-dim" /> On the new machine (Debian 12/13 or Ubuntu 22.04/24.04), run:
              </div>
              <CodeBlock code={installCommand({ worker: kind === "worker", hostname, ai, join: invite.token })} prompt testId="install-command" />
              {kind === "worker" && invite.error && (
                <Callout tone="bad">
                  {mainS?.hostname} didn't make an invite ({invite.error}). On it, <InlineCode>sudo vercelify mesh invite --platform</InlineCode> prints
                  one; add <InlineCode>--join TOKEN</InlineCode> to the command.
                </Callout>
              )}
              <ul className="space-y-1 text-xs text-fg-muted">
                {kind === "worker" && mainS && <li>• It joins {mainS.hostname}'s private network with a single-use invite (valid for an hour).</li>}
                <li>
                  • It takes 5–15 minutes, then prints an 8-character code. You can always get a new one with <InlineCode>sudo vercelify pair</InlineCode>.
                </li>
                <li>• This window keeps looking for it; as soon as it's online, type the code below.</li>
              </ul>
            </div>
          )}
          <PairForm
            hidePaste={!installed}
            autoScanMs={4000}
            paired={servers.map((s) => s.hostname)}
            onPaired={(s, n) => {
              setAdded(s);
              setNote(n);
              setStep("finish");
              void onAdded(s);
            }}
          />
          <div className="flex justify-start">
            <Button variant="ghost" icon={<ArrowLeft />} onClick={() => setStep("choose")}>
              Back
            </Button>
          </div>
        </div>
      )}

      {step === "finish" && added && <Finish server={added} main={mainS} note={note} onAnother={reset} onClose={onClose} />}
    </div>
  );
}

function Finish({
  server,
  main,
  note,
  onAnother,
  onClose,
}: {
  server: CockpitSettings;
  main?: CockpitSettings;
  note?: string;
  onAnother: () => void;
  onClose: () => void;
}) {
  const worker = isWorker(server);
  const doctor = usePoll(() => agentOn(server.agent_url).doctor(), 0, [server.agent_url]);
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2.5 rounded-lg border border-line bg-surface-2 px-3.5 py-3">
        <StatusDot health="ok" />
        <div className="text-[13px]">
          <span className="font-medium text-fg">{server.hostname}</span> <span className="text-fg-muted">is connected as a {worker ? "worker" : "server"}.</span>
        </div>
      </div>
      {note && <Callout>{note}</Callout>}

      {worker && (
        <div className="rounded-lg border border-line p-3.5 text-xs text-fg-muted" data-testid="wizard-pool">
          {main ? (
            <>
              It runs apps for <span className="font-medium text-fg">{main.hostname}</span>: pick it under “Run on” when you create an app there, or
              let {main.hostname} place apps on it.
            </>
          ) : (
            <>On the main server, <InlineCode>sudo vercelify mesh invite --platform</InlineCode> makes the invite it joins with.</>
          )}
        </div>
      )}

      <div className="rounded-lg border border-line p-3.5">
        <div className="mb-2 flex items-center justify-between text-[13px] font-medium">
          Health checks
          <Button size="sm" variant="ghost" onClick={() => void doctor.refresh()}>
            Run again
          </Button>
        </div>
        <HealthChecks report={doctor.data} loading={doctor.loading} open testId="wizard-checks" />
      </div>

      <div className="flex justify-between gap-2">
        <Button variant="ghost" onClick={onAnother} data-testid="wizard-another">
          Add another machine
        </Button>
        <Button variant="primary" onClick={onClose} data-testid="wizard-done">
          Done
        </Button>
      </div>
    </div>
  );
}
