import { AnimatePresence, motion } from "framer-motion";
import { Eye, EyeOff, FileText, KeyRound, Upload, X } from "lucide-react";
import { useRef, useState, type DragEvent } from "react";
import { coolify } from "../lib/api";
import { parseDotenv, type EnvVar } from "../lib/env";
import { usePoll } from "../lib/hooks";
import type { CoolifyApplication } from "../types";
import { useToast } from "./Toast";
import { Button, cx, EmptyState, ErrorNote, IconButton, Loading, OrbitButton } from "./ui";

type Phase = "idle" | "review" | "saving" | "done";

/**
 * Environment variables of an app, plus a drop zone for a .env file (after
 * morphin.dev's gradient file upload). Dropping or choosing a file parses it
 * locally and shows what will be added or changed; saving sends one bulk
 * update to Coolify while a beam runs along the progress track. While a file
 * is dragged over it, an ember arc orbits the card's border.
 */
export function EnvPanel({ app }: { app: CoolifyApplication }) {
  const list = usePoll(() => coolify.envs(app.uuid), 0, [app.uuid]);
  const toast = useToast();
  const input = useRef<HTMLInputElement>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [over, setOver] = useState(false);
  const [file, setFile] = useState<{ name: string; vars: EnvVar[] } | null>(null);
  const [error, setError] = useState<string>();
  const [shown, setShown] = useState<Set<string>>(new Set());

  const existing = new Map((list.data ?? []).filter((e) => !e.is_preview).map((e) => [e.key, e.value ?? ""]));
  const added = file?.vars.filter((v) => !existing.has(v.key)).length ?? 0;
  const changed = file?.vars.filter((v) => existing.has(v.key) && existing.get(v.key) !== v.value).length ?? 0;

  async function read(f: File | undefined) {
    if (!f) return;
    setError(undefined);
    if (f.size > 256 * 1024) return setError("That file is larger than 256 KB; a .env file is usually a few lines.");
    const vars = parseDotenv(await f.text());
    if (!vars.length) return setError(`No KEY=value lines found in ${f.name}.`);
    setFile({ name: f.name, vars });
    setPhase("review");
  }

  async function save() {
    if (!file) return;
    setPhase("saving");
    try {
      await coolify.saveEnvs(app.uuid, file.vars);
      setPhase("done");
      toast.ok(`${file.vars.length} variables saved to ${app.name}. Redeploy to use them.`);
      void list.refresh();
    } catch (e) {
      setPhase("review");
      setError((e as Error).message);
      throw e;
    }
  }

  function reset() {
    setFile(null);
    setPhase("idle");
    setError(undefined);
  }

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setOver(false);
    if (phase === "saving") return;
    void read(e.dataTransfer.files[0]);
  };

  const busy = phase === "saving";
  const lit = over || busy || phase === "done";

  return (
    <div className="space-y-4" data-testid="env-panel">
      <div
        onDragOver={(e) => {
          e.preventDefault();
          setOver(true);
        }}
        onDragLeave={() => setOver(false)}
        onDrop={onDrop}
        className={cx(
          "relative rounded-xl border bg-surface transition-colors duration-300",
          over ? "border-transparent" : "border-line",
        )}
        data-testid="env-drop"
      >
        {over && (
          <>
            <span className="orbit-ring" data-glow="" />
            <span className="orbit-ring" />
          </>
        )}
        {/* Ember wash that fades in while a file is over the card, saving or saved. */}
        <motion.span
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 overflow-hidden rounded-[inherit]"
          animate={{ opacity: lit ? 1 : 0 }}
          transition={{ duration: 0.5 }}
          style={{
            background:
              "radial-gradient(120% 140% at 0% 0%, color-mix(in srgb, var(--accent) 13%, transparent), transparent 55%), radial-gradient(90% 120% at 100% 100%, color-mix(in srgb, var(--accent-soft) 9%, transparent), transparent 60%)",
          }}
        />
        <div className="relative flex items-center gap-3.5 p-3.5">
          <motion.span
            layout
            className="flex size-12 shrink-0 flex-col items-center justify-center gap-0.5 rounded-[10px] border border-line-strong bg-panel text-fg-muted"
          >
            <FileText className="size-4" />
            <span className="text-[8.5px] font-bold tracking-wider text-fg">ENV</span>
          </motion.span>
          <div className="min-w-0 flex-1">
            <AnimatePresence mode="popLayout" initial={false}>
              <motion.div
                key={phase === "idle" ? (over ? "over" : "idle") : phase}
                initial={{ opacity: 0, y: 6 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -6 }}
                transition={{ duration: 0.18 }}
              >
                <div className="truncate text-[13px] font-medium text-fg">
                  {phase === "idle" ? (over ? "Release to read it" : "Drop a .env file") : file?.name}
                </div>
                <div className="mt-0.5 truncate text-[12px] text-fg-muted">
                  {phase === "idle" && "or choose one. It's read here; only the values you save go to your server."}
                  {phase === "review" && `${file?.vars.length} variables · ${added} new · ${changed} changed`}
                  {phase === "saving" && `Saving ${file?.vars.length} variables to Coolify…`}
                  {phase === "done" && "Saved. Redeploy the app to use them."}
                </div>
              </motion.div>
            </AnimatePresence>
          </div>
          {phase === "idle" ? (
            <button
              type="button"
              onClick={() => input.current?.click()}
              aria-label="Choose a .env file"
              className="flex size-9 shrink-0 items-center justify-center rounded-full bg-fg text-inverse transition hover:bg-fg/90 active:scale-95"
            >
              <Upload className="size-4" />
            </button>
          ) : (
            <IconButton label={phase === "done" ? "Done" : "Discard"} onClick={reset} disabled={busy}>
              <X />
            </IconButton>
          )}
          <input
            ref={input}
            type="file"
            accept=".env,.txt,text/plain"
            className="hidden"
            data-testid="env-file"
            onChange={(e) => {
              void read(e.target.files?.[0]);
              e.target.value = "";
            }}
          />
        </div>

        {(busy || phase === "done") && (
          <div className="relative mx-3.5 mb-3.5 h-1 overflow-hidden rounded-full bg-fg/[0.08]">
            <motion.div
              className="h-full rounded-full"
              style={{ background: "linear-gradient(90deg, var(--accent), var(--accent-soft))" }}
              initial={{ width: "4%" }}
              animate={{ width: phase === "done" ? "100%" : "88%" }}
              transition={phase === "done" ? { duration: 0.35 } : { duration: 2.4, ease: [0.1, 0.6, 0.3, 1] }}
            />
            {busy && (
              <span
                className="absolute inset-y-0 left-0 w-1/4"
                style={{ background: "linear-gradient(90deg, transparent, rgb(255 255 255 / 0.55), transparent)", animation: "beam 1.3s ease-in-out infinite" }}
              />
            )}
          </div>
        )}
      </div>

      <ErrorNote error={error ? { message: error } : undefined} />

      {phase === "review" && file && (
        <motion.div initial={{ opacity: 0, y: 6 }} animate={{ opacity: 1, y: 0 }} className="rounded-xl border border-line bg-surface">
          <div className="max-h-56 divide-y divide-line overflow-y-auto">
            {file.vars.map((v) => {
              const isNew = !existing.has(v.key);
              const isChanged = !isNew && existing.get(v.key) !== v.value;
              return (
                <div key={v.key} className="flex items-center gap-3 px-3.5 py-2 text-[12px]">
                  <code className="min-w-0 flex-1 truncate font-mono text-fg">{v.key}</code>
                  <span className={cx("shrink-0", isNew ? "text-accent-fg" : isChanged ? "text-fg-muted" : "text-fg-dim")}>
                    {isNew ? "new" : isChanged ? "changed" : "same"}
                  </span>
                </div>
              );
            })}
          </div>
          <div className="flex items-center justify-end gap-2 border-t border-line px-3.5 py-2.5">
            <Button variant="ghost" size="sm" onClick={reset}>
              Cancel
            </Button>
            <OrbitButton size="sm" icon={<KeyRound />} busyLabel="Saving" doneLabel="Saved" onRun={save} testId="env-save">
              Save {file.vars.length} variables
            </OrbitButton>
          </div>
        </motion.div>
      )}

      <div className="overflow-hidden rounded-xl border border-line bg-surface">
        <div className="flex h-8 items-center border-b border-line bg-fg/[0.015] px-3.5 text-[11px] font-medium text-fg-dim">
          <span className="flex-1">Variable</span>
          <span className="w-1/2">Value</span>
        </div>
        <ErrorNote error={list.error} className="m-3" />
        {list.loading && !list.data ? (
          <div className="p-3.5">
            <Loading rows={2} />
          </div>
        ) : !list.data?.length ? (
          <EmptyState icon={<KeyRound />} title="No environment variables">
            Drop your .env above. Coolify passes these to the build and the running container.
          </EmptyState>
        ) : (
          <div className="divide-y divide-line" data-testid="env-list">
            {list.data
              .filter((e) => !e.is_preview)
              .map((e) => {
                const visible = shown.has(e.key);
                return (
                  <div key={e.uuid} className="group flex items-center gap-3 px-3.5 py-2 text-[12px]">
                    <code className="min-w-0 flex-1 truncate font-mono text-fg">{e.key}</code>
                    <span className="flex w-1/2 min-w-0 items-center gap-1">
                      <code className={cx("selectable min-w-0 flex-1 truncate font-mono", visible ? "text-fg-muted" : "text-fg-dim")}>
                        {visible ? e.value || "—" : "••••••••••"}
                      </code>
                      <IconButton
                        label={visible ? "Hide" : "Show"}
                        className="opacity-0 group-hover:opacity-100"
                        onClick={() =>
                          setShown((s) => {
                            const n = new Set(s);
                            if (visible) n.delete(e.key);
                            else n.add(e.key);
                            return n;
                          })
                        }
                      >
                        {visible ? <EyeOff /> : <Eye />}
                      </IconButton>
                    </span>
                  </div>
                );
              })}
          </div>
        )}
      </div>
    </div>
  );
}
