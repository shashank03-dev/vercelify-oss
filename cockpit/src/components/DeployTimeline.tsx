import { motion } from "framer-motion";
import { GitBranch, GitCommitHorizontal, RotateCcw, ScrollText } from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";
import { ago, shortSha } from "../lib/format";
import type { CoolifyApplication, CoolifyDeployment } from "../types";
import { cx, IconButton, ShimmerText, StatusText } from "./ui";

export interface TimelineRow {
  d: CoolifyDeployment;
  canRollback: boolean;
  live: boolean;
}

const GRID = "grid items-center gap-4 grid-cols-[96px_104px_minmax(0,1fr)_76px_60px]";

/** "Today", "Yesterday" or "Mon, Oct 13" in the user's locale. */
function dayLabel(ts: string, now = new Date()): string {
  const iso = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(ts) ? ts.replace(" ", "T") + "Z" : ts;
  const d = new Date(iso);
  const start = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((start(now) - start(d)) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return d.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
}

export function deployState(d: CoolifyDeployment): string {
  if (d.status === "finished") return "Ready";
  if (d.status === "failed") return "Error";
  if (d.status === "in_progress") return "Building";
  if (d.status === "queued") return "Queued";
  if (d.status.startsWith("cancelled")) return "Canceled";
  return d.status;
}

function deployHealth(d: CoolifyDeployment) {
  if (d.status === "finished") return "ok" as const;
  if (d.status === "failed" || d.status.startsWith("cancelled")) return "bad" as const;
  return "warn" as const;
}

function deployDuration(d: CoolifyDeployment): string {
  const end = d.finished_at ?? (d.status === "finished" || d.status === "failed" ? d.updated_at : null);
  if (!end) return "";
  const s = Math.max(0, Math.round((Date.parse(end) - Date.parse(d.created_at)) / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

/** Path through the nodes; a change of column is an S-curve just above the node. */
function pathThrough(nodes: { x: number; y: number }[]): string {
  if (!nodes.length) return "";
  let d = `M${nodes[0]!.x} ${nodes[0]!.y}`;
  for (let i = 1; i < nodes.length; i++) {
    const a = nodes[i - 1]!;
    const b = nodes[i]!;
    if (a.x === b.x) {
      d += ` L${b.x} ${b.y}`;
      continue;
    }
    const top = Math.max(a.y + 4, b.y - 22);
    const bottom = b.y - 7;
    d += ` L${a.x} ${top} C${a.x} ${top + 9} ${b.x} ${bottom - 9} ${b.x} ${bottom} L${b.x} ${b.y}`;
  }
  return d;
}

/**
 * Deployments grouped by day with a curved rail in the gutter (after
 * morphin.dev's animated curved timeline). Day nodes sit on the outer line,
 * deployments on the inner one, and the line is drawn in ember from the top
 * down to the deployment that is live (or building), so you can see at a
 * glance how far production is from the newest push. Rows keep the Vercel
 * columns: environment, status, source and created.
 */
export function DeployTimeline({
  rows,
  app,
  onLogs,
  onRollback,
  instant,
}: {
  rows: TimelineRow[];
  app: CoolifyApplication;
  /** Image tags kept on the server: a rollback to one of these needs no rebuild. */
  instant?: string[];
  onLogs: (d: CoolifyDeployment) => void;
  onRollback: (d: CoolifyDeployment) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [geo, setGeo] = useState<{ all: string; lit: string; h: number }>({ all: "", lit: "", h: 0 });

  const groups: { day: string; rows: TimelineRow[] }[] = [];
  for (const r of rows) {
    const day = dayLabel(r.d.created_at);
    const g = groups[groups.length - 1];
    if (g?.day === day) g.rows.push(r);
    else groups.push({ day, rows: [r] });
  }
  const litUuid = (rows.find((r) => r.d.status === "in_progress" || r.d.status === "queued") ?? rows.find((r) => r.live))?.d.deployment_uuid;
  const key = rows.map((r) => r.d.deployment_uuid + r.d.status).join("|");

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => {
      const b = el.getBoundingClientRect();
      const nodes = Array.from(el.querySelectorAll<HTMLElement>("[data-node]")).map((n) => {
        const r = n.getBoundingClientRect();
        return { x: Math.round(r.left - b.left + r.width / 2), y: Math.round(r.top - b.top + r.height / 2), uuid: n.dataset.uuid };
      });
      const stop = nodes.findIndex((n) => n.uuid === litUuid);
      setGeo({ all: pathThrough(nodes), lit: stop > 0 ? pathThrough(nodes.slice(0, stop + 1)) : "", h: el.scrollHeight });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [key, litUuid]);

  return (
    <div className="overflow-hidden rounded-xl border border-line bg-surface" role="table" aria-label="Deployments">
      <div role="row" className={cx(GRID, "h-8 border-b border-line bg-fg/[0.015] pr-3 pl-11 text-[11px] font-medium text-fg-dim")}>
        <span role="columnheader">Environment</span>
        <span role="columnheader">Status</span>
        <span role="columnheader">Source</span>
        <span role="columnheader">Created</span>
        <span />
      </div>
      <div ref={box} className="relative py-2">
        <svg className="pointer-events-none absolute top-0 left-0 overflow-visible" width="48" height={geo.h} aria-hidden="true">
          <path d={geo.all} fill="none" stroke="var(--line-strong)" strokeWidth="1.5" strokeLinecap="round" />
          {geo.lit && (
            <motion.path
              key={geo.lit}
              d={geo.lit}
              fill="none"
              stroke="var(--accent)"
              strokeWidth="1.5"
              strokeLinecap="round"
              initial={{ pathLength: 0 }}
              animate={{ pathLength: 1 }}
              transition={{ duration: 0.9, ease: [0.22, 1, 0.36, 1] }}
            />
          )}
        </svg>
        {groups.map((g) => {
          return (
            <div key={g.day}>
              <div className="flex h-8 items-center gap-2 pl-2 text-[12px]">
                <span className="flex w-8 justify-start pl-[5px]">
                  <span data-node="day" className="size-[9px] rounded-full border-[1.5px] border-fg-dim bg-surface" />
                </span>
                <span className="font-medium text-fg">{g.day}</span>
                <span className="text-fg-dim">{g.rows.length}</span>
              </div>
              {g.rows.map(({ d, canRollback, live }) => {
                const isProd = d.pull_request_id === 0;
                const active = d.status === "in_progress" || d.status === "queued";
                const before = litUuid ? rows.findIndex((r) => r.d.deployment_uuid === d.deployment_uuid) <= rows.findIndex((r) => r.d.deployment_uuid === litUuid) : false;
                return (
                  <div key={d.deployment_uuid} role="row" data-testid="deployment-row" className="group relative mx-1.5 flex items-center rounded-lg py-2.5 pr-1.5 transition-colors hover:bg-fg/[0.03]">
                    <span className="flex w-[38px] shrink-0 justify-start pl-[19px]">
                      <span
                        data-node="row"
                        data-uuid={d.deployment_uuid}
                        className={cx(
                          "size-[7px] rounded-full transition-transform duration-200 group-hover:scale-[1.45]",
                          before ? "bg-accent" : "bg-fg-dim",
                          d.deployment_uuid === litUuid && "ring-4 ring-accent/15",
                        )}
                      />
                    </span>
                    <div className={cx(GRID, "min-w-0 flex-1")}>
                      <div className="min-w-0 leading-tight">
                        <div className="text-[13px] text-fg">{isProd ? "Production" : "Preview"}</div>
                        <div className="mt-1 text-[11px] text-fg-dim">
                          {live ? <span className="font-medium text-fg-muted">Current</span> : d.rollback ? "Rollback" : isProd ? "" : `PR #${d.pull_request_id}`}
                        </div>
                      </div>
                      <div className="min-w-0 leading-tight">
                        <StatusText health={deployHealth(d)} label={deployState(d)} className="text-[13px] text-fg" />
                        <div className="mt-1 text-[11px] text-fg-dim tabular-nums">{active ? <ShimmerText>working…</ShimmerText> : deployDuration(d)}</div>
                      </div>
                      <div className="min-w-0 leading-tight">
                        <div className="flex items-center gap-1.5 text-[13px] text-fg">
                          <GitBranch className="size-3.5 shrink-0 text-fg-dim" />
                          <span className="truncate">{isProd ? (app.git_branch ?? "main") : `pr-${d.pull_request_id}`}</span>
                        </div>
                        <div className="mt-1 flex min-w-0 items-center gap-1.5 text-[11px] text-fg-dim">
                          <GitCommitHorizontal className="size-3.5 shrink-0" />
                          <code className="shrink-0 font-mono text-fg-muted">{shortSha(d.commit) || "HEAD"}</code>
                          <span className="truncate">{d.commit_message?.split("\n")[0] || "Manual deploy"}</span>
                        </div>
                      </div>
                      <div className="min-w-0 leading-tight">
                        <div className="text-[13px] text-fg-muted">{ago(d.created_at)}</div>
                        <div className="mt-1 text-[11px] text-fg-dim">{d.is_webhook ? "git push" : d.is_api ? "API" : "manual"}</div>
                      </div>
                      <div className="flex items-center justify-end gap-0.5">
                        {canRollback && instant && d.commit && (
                          <span className="text-[11px] text-fg-dim" data-testid="rollback-kind">
                            {instant.some((t) => d.commit!.startsWith(t) || t.startsWith(d.commit!)) ? "instant" : "rebuild"}
                          </span>
                        )}
                        {canRollback && (
                          <IconButton
                            label={
                              instant && d.commit && !instant.some((t) => d.commit!.startsWith(t) || t.startsWith(d.commit!))
                                ? "Roll back to this (its image was cleaned up: it rebuilds)"
                                : "Roll back to this"
                            }
                            onClick={() => onRollback(d)}
                            data-testid="rollback"
                          >
                            <RotateCcw />
                          </IconButton>
                        )}
                        <IconButton label="Build logs" onClick={() => onLogs(d)}>
                          <ScrollText />
                        </IconButton>
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          );
        })}
      </div>
    </div>
  );
}
