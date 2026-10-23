import { Clock, FileJson, GitPullRequest, Play, RefreshCw, Trash2, Upload } from "lucide-react";
import { useRef, useState } from "react";
import { agent } from "../lib/api";
import { hasCap } from "../lib/caps";
import { ago, shortSha } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { useLive, useLiveTopic } from "../lib/live";
import { cronState, projectFileName, summaryLine, untilLabel } from "../lib/project";
import { toCockpitError } from "../lib/transport";
import type { CoolifyApplication, ProjectFile, ProjectInfo } from "../types";
import { NeedsUpdate } from "./Hardware";
import { useToast } from "./Toast";
import { Button, Callout, EmptyState, InlineCode, Loading, StatusText } from "./ui";

function sourceLabel(p: ProjectInfo): string {
  const when = ago(p.updated_at);
  if (p.source === "upload") return `uploaded ${when}`;
  const at = p.commit && /^[0-9a-f]{7,}$/.test(p.commit) ? shortSha(p.commit) : p.commit;
  return at ? `from the repository at ${at}, ${when}` : `from the repository, ${when}`;
}

function liveState(p: ProjectInfo): { health: "ok" | "warn" | "bad" | "idle"; label: string } {
  if (p.error) return { health: "bad", label: "not applied" };
  if (p.routes_active) return { health: "ok", label: "live in the proxy" };
  if (p.summary.crons.length) return { health: "ok", label: "cron jobs only" };
  return { health: "idle", label: "nothing to route" };
}

/** An app's vercel.json: what the proxy and the cron runner do with it. */
export function ProjectPanel({ app }: { app: CoolifyApplication }) {
  const toast = useToast();
  const { health } = useLive();
  const withProjects = hasCap(health.data, "projects");
  const status = usePoll(() => (withProjects ? agent.projects() : Promise.resolve(null)), withProjects ? 30_000 : 0, [withProjects]);
  useLiveTopic("projects", () => void status.refresh());
  const [busy, setBusy] = useState<string | null>(null);
  const file = useRef<HTMLInputElement>(null);

  const info = status.data?.projects.find((p) => p.key === app.uuid);
  const previews = (status.data?.projects ?? []).filter((p) => p.app_uuid === app.uuid && p.pull_request_id > 0);

  async function run(what: string, fn: () => Promise<unknown>, ok?: string) {
    setBusy(what);
    try {
      await fn();
      await status.refresh();
      if (ok) toast.ok(ok);
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(null);
    }
  }

  async function upload(list: FileList | null) {
    if (!list?.length) return;
    const files: ProjectFile[] = [];
    for (const f of Array.from(list)) {
      const name = projectFileName(f.name);
      if (!name) {
        toast.bad(`${f.name}: choose a vercel.json or vercelify.json`);
        return;
      }
      files.push({ name, content: await f.text() });
    }
    await run("upload", () => agent.uploadProject(app.uuid, files), `${files.map((f) => f.name).join(" and ")} applied to ${app.name}`);
  }

  if (!health.data || (withProjects && status.loading && !status.data)) return <Loading rows={3} />;
  if (!withProjects) return <NeedsUpdate what="vercel.json support" />;

  const st = info ? liveState(info) : null;
  return (
    <div className="space-y-6">
      <section className="space-y-3" data-testid="app-project">
        <h3 className="flex items-center gap-2 text-[13px] font-medium">
          <FileJson className="size-4 text-fg-muted" /> vercel.json
        </h3>
        {info && st ? (
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
              <StatusText health={st.health} label={st.label} className="text-[13px]" testId="project-state" />
              <span className="text-xs text-fg-dim">{sourceLabel(info)}</span>
            </div>
            <p className="text-[13px] text-fg-muted" data-testid="project-summary">
              {summaryLine(info.summary)}
              {info.hosts.length > 0 && (
                <span className="text-fg-dim">
                  {" "}
                  on <span className="font-mono text-xs">{info.hosts.join(", ")}</span>
                </span>
              )}
            </p>
            {info.error && <Callout tone="bad">{info.error}</Callout>}
            {info.summary.warnings.length > 0 && (
              <Callout tone="idle" title="Not applied exactly as on Vercel">
                <ul className="list-disc space-y-0.5 pl-4" data-testid="project-warnings">
                  {info.summary.warnings.map((w, i) => (
                    <li key={i}>{w}</li>
                  ))}
                </ul>
              </Callout>
            )}
          </div>
        ) : (
          <EmptyState icon={<FileJson />} title="No vercel.json for this app yet">
            Redirects, rewrites, headers, <InlineCode>cleanUrls</InlineCode>, cron jobs and a firewall from your repository’s{" "}
            <InlineCode>vercel.json</InlineCode> (and an optional <InlineCode>vercelify.json</InlineCode>) run in the proxy on this server. With auto-deploy on
            GitHub they follow every deploy by themselves.
          </EmptyState>
        )}
        <div className="flex flex-wrap items-center gap-1.5">
          <Button size="sm" icon={<RefreshCw />} busy={busy === "sync"} onClick={() => void run("sync", () => agent.syncProject(app.uuid), "Read vercel.json from the repository")} data-testid="project-sync">
            Read from repository
          </Button>
          <Button size="sm" variant="ghost" icon={<Upload />} busy={busy === "upload"} onClick={() => file.current?.click()} data-testid="project-upload">
            Upload…
          </Button>
          <input
            ref={file}
            type="file"
            accept=".json,application/json"
            multiple
            className="hidden"
            data-testid="project-file"
            onChange={(e) => {
              void upload(e.target.files);
              e.target.value = "";
            }}
          />
          {info && (
            <Button
              size="sm"
              variant="ghost"
              icon={<Trash2 />}
              busy={busy === "remove"}
              onClick={() => void run("remove", () => agent.removeProject(app.uuid), `${app.name} is served as Coolify configures it`)}
              data-testid="project-remove"
            >
              Remove rules
            </Button>
          )}
        </div>
        {status.data?.error && <Callout tone="warn">{status.data.error}</Callout>}
      </section>

      {info && info.summary.crons.length > 0 && (
        <section className="space-y-3 border-t border-line pt-5" data-testid="app-crons">
          <h3 className="flex items-center gap-2 text-[13px] font-medium">
            <Clock className="size-4 text-fg-muted" /> Cron jobs
          </h3>
          <ul className="divide-y divide-line rounded-lg border border-line text-[13px]">
            {info.summary.crons.map((c, i) => {
              const cs = cronState(c);
              return (
                <li key={`${c.schedule} ${c.path}`} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
                  <span className="min-w-0 flex-1 truncate font-mono text-xs">{c.path}</span>
                  <span className="shrink-0 font-mono text-xs text-fg-muted">{c.schedule}</span>
                  <StatusText health={cs.health} label={cs.label} className="shrink-0 text-xs text-fg-muted" testId={`cron-state-${i}`} />
                  <span className="shrink-0 text-xs text-fg-dim" title={c.next_run_at ?? undefined}>
                    {c.next_run_at ? `next ${untilLabel(c.next_run_at)}` : ""}
                  </span>
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={<Play />}
                    busy={busy === `cron-${i}`}
                    disabled={c.running}
                    onClick={() => void run(`cron-${i}`, () => agent.runCron(app.uuid, c.path), `Running ${c.path}`)}
                    data-testid={`cron-run-${i}`}
                  >
                    Run now
                  </Button>
                </li>
              );
            })}
          </ul>
          <p className="text-xs text-fg-dim">
            Times are UTC. Each run is a GET to the app with <InlineCode>Authorization: Bearer $CRON_SECRET</InlineCode> when the app has that variable, like on
            Vercel.
          </p>
        </section>
      )}

      {previews.length > 0 && (
        <section className="space-y-3 border-t border-line pt-5" data-testid="app-project-previews">
          <h3 className="flex items-center gap-2 text-[13px] font-medium">
            <GitPullRequest className="size-4 text-fg-muted" /> Previews
          </h3>
          <ul className="divide-y divide-line rounded-lg border border-line text-[13px]">
            {previews.map((p) => {
              const ps = liveState(p);
              return (
                <li key={p.key} className="flex items-center gap-3 px-3 py-2">
                  <span className="w-12 shrink-0 font-mono text-xs text-fg-muted">#{p.pull_request_id}</span>
                  <StatusText health={ps.health} label={ps.label} className="shrink-0 text-xs text-fg-muted" />
                  <span className="min-w-0 flex-1 truncate text-fg-muted">{summaryLine(p.summary)}</span>
                  <span className="shrink-0 text-xs text-fg-dim">{p.commit ? shortSha(p.commit) : ""}</span>
                </li>
              );
            })}
          </ul>
        </section>
      )}
    </div>
  );
}
