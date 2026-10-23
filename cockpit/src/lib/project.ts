import type { CronInfo, ProjectSummary } from "../types";
import { ago } from "./format";

/** The two files the agent reads. A downloaded copy may be renamed (`vercel (1).json`). */
export function projectFileName(name: string): "vercel.json" | "vercelify.json" | null {
  const n = name.toLowerCase();
  if (!n.endsWith(".json")) return null;
  if (n.includes("vercelify")) return "vercelify.json";
  if (n.includes("vercel") || n === "now.json") return "vercel.json";
  return null;
}

export function cronState(c: CronInfo, now = Date.now()): { health: "ok" | "warn" | "bad" | "idle"; label: string } {
  if (c.running) return { health: "warn", label: "running" };
  if (!c.last_run_at) return { health: "idle", label: "not run yet" };
  const when = ago(c.last_run_at, now);
  if (c.last_status != null && c.last_status >= 200 && c.last_status < 300) {
    return { health: "ok", label: `HTTP ${c.last_status} · ${when}` };
  }
  return { health: "bad", label: `${c.last_error ?? `HTTP ${c.last_status}`} · ${when}` };
}

/** "3 redirects · 1 rewrite · clean URLs". */
export function summaryLine(s: ProjectSummary): string {
  const n = (count: number, one: string, many = `${one}s`) => (count ? `${count} ${count === 1 ? one : many}` : null);
  const parts = [
    n(s.redirects, "redirect"),
    n(s.rewrites, "rewrite"),
    n(s.headers, "header rule"),
    n(s.firewall_rules, "firewall rule"),
    n(s.crons.length, "cron job"),
    s.clean_urls ? "clean URLs" : null,
    s.trailing_slash === true ? "trailing slash" : s.trailing_slash === false ? "no trailing slash" : null,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : "no rules";
}

/** "in 12m", "in 5h", "in 3d" until a future time. */
export function untilLabel(ts: string | null | undefined, now = Date.now()): string {
  if (!ts) return "";
  const s = Math.round((Date.parse(ts) - now) / 1000);
  if (Number.isNaN(s)) return "";
  if (s < 60) return "in under a minute";
  if (s < 3600) return `in ${Math.floor(s / 60)}m`;
  if (s < 86400) return `in ${Math.floor(s / 3600)}h`;
  return `in ${Math.floor(s / 86400)}d`;
}
