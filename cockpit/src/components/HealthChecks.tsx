import { ChevronDown } from "lucide-react";
import { useState } from "react";
import type { CheckStatus, DoctorReport } from "../types";
import { cx, InlineCode, PixelWave, ShimmerText, StatusDot } from "./ui";

const HEALTH: Record<CheckStatus, "ok" | "warn" | "bad"> = { ok: "ok", warn: "warn", bad: "bad" };

export function overall(r: DoctorReport): CheckStatus {
  if (r.checks.some((c) => c.status === "bad")) return "bad";
  if (r.checks.some((c) => c.status === "warn")) return "warn";
  return "ok";
}

export function summary(r: DoctorReport): string {
  const bad = r.checks.filter((c) => c.status === "bad").length;
  const warn = r.checks.filter((c) => c.status === "warn").length;
  if (!bad && !warn) return `All ${r.checks.length} checks pass`;
  return [bad && `${bad} problem${bad > 1 ? "s" : ""}`, warn && `${warn} warning${warn > 1 ? "s" : ""}`].filter(Boolean).join(", ");
}

/** The server's `vercelify doctor`, as a list with fixes. Collapsed unless something is wrong or `open`. */
export function HealthChecks({ report, loading, open: initiallyOpen, testId }: { report?: DoctorReport; loading?: boolean; open?: boolean; testId?: string }) {
  const [open, setOpen] = useState(initiallyOpen ?? false);
  if (!report) {
    return (
      <div className="flex items-center gap-2 text-xs text-fg-muted" data-testid={testId}>
        {loading ? (
          <>
            <PixelWave size={3} pixel={3} gap={1.5} /> <ShimmerText>Running checks…</ShimmerText>
          </>
        ) : (
          <>
            <StatusDot health="idle" /> Checks unavailable
          </>
        )}
      </div>
    );
  }
  const o = overall(report);
  const problems = report.checks.filter((c) => c.status !== "ok");
  const shown = open ? report.checks : problems;
  return (
    <div data-testid={testId}>
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-center gap-2 text-left text-xs" aria-expanded={open}>
        <StatusDot health={HEALTH[o]} />
        <span className={cx("font-medium", o === "ok" ? "text-fg" : o === "warn" ? "text-warn" : "text-bad")}>{summary(report)}</span>
        <ChevronDown className={cx("ml-auto size-3.5 text-fg-dim transition-transform", open && "rotate-180")} />
      </button>
      {shown.length > 0 && (
        <ul className="mt-2 space-y-1.5">
          {shown.map((c) => (
            <li key={c.id} className="flex gap-2 text-xs" data-testid={`check-${c.id}`}>
              <span className="pt-1">
                <StatusDot health={HEALTH[c.status]} />
              </span>
              <div className="min-w-0">
                <span className="font-medium text-fg">{c.label}</span> <span className="text-fg-muted">— {c.detail}</span>
                {c.fix && (
                  <div className="selectable mt-0.5 text-fg-dim">
                    Fix: <InlineCode>{c.fix}</InlineCode>
                  </div>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
