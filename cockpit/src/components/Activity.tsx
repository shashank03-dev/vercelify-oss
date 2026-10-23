import { Activity as ActivityIcon } from "lucide-react";
import { hasCap } from "../lib/caps";
import { ago, type Health } from "../lib/format";
import { useLive } from "../lib/live";
import type { Job } from "../types";
import { NeedsUpdate } from "./Hardware";
import { Card, Loading, ProgressBar, StatusText } from "./ui";

export function jobHealth(j: Job): Health {
  return j.status === "ok" ? "ok" : j.status === "failed" ? "bad" : "idle";
}

export function jobLabel(j: Job): string {
  if (j.status === "running") return j.progress != null ? `${Math.round(j.progress)}%` : "running";
  return j.status === "ok" ? "done" : "failed";
}

/** Long-running work on the server (backups, moves, downloads, deploys, sleeping apps), live. */
export function ActivityCard({ limit = 8 }: { limit?: number }) {
  const { health, jobs, streaming } = useLive();
  const supported = hasCap(health.data, "jobs");
  return (
    <Card
      title="Activity"
      icon={<ActivityIcon />}
      description={streaming ? "Live from the server" : "Backups, moves, downloads and deploys"}
      testId="activity"
      flush
    >
      {!health.data ? (
        <div className="p-4">
          <Loading rows={2} />
        </div>
      ) : !supported ? (
        <div className="p-4">
          <NeedsUpdate what="The activity list" />
        </div>
      ) : jobs.length === 0 ? (
        <p className="p-4 text-[13px] text-fg-muted">Nothing has run yet.</p>
      ) : (
        <ul className="divide-y divide-line">
          {jobs.slice(0, limit).map((j) => (
            <li key={j.id} className="space-y-1.5 px-4 py-2.5 text-[13px]" data-testid={`job-${j.kind}`}>
              <div className="flex items-center gap-3">
                <span className="min-w-0 flex-1 truncate font-medium">{j.title}</span>
                <StatusText health={jobHealth(j)} label={jobLabel(j)} className="shrink-0 text-xs text-fg-muted" />
                <span className="w-16 shrink-0 text-right text-xs text-fg-dim tabular-nums">{ago(j.finished_at ?? j.started_at)}</span>
              </div>
              {j.status === "running" && j.progress != null && <ProgressBar value={j.progress} active label={j.title} />}
              {j.detail && <p className="truncate text-xs text-fg-muted">{j.detail}</p>}
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}
