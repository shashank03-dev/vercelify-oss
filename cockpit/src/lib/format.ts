const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

/** 1536 -> "1.5 KB" (binary multiples, labelled the way Finder does). */
export function bytes(n: number | null | undefined, digits = 1): string {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n < 1024) return `${n} B`;
  let i = 0;
  let v = n;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : digits)} ${UNITS[i]}`;
}

export function pct(part: number, whole: number): number {
  if (!whole) return 0;
  return Math.min(100, Math.max(0, (part / whole) * 100));
}

export function duration(secs: number): string {
  if (secs < 60) return `${Math.floor(secs)}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

/** "3 minutes ago". Accepts ISO strings and Coolify's "YYYY-MM-DD HH:MM:SS" (UTC). */
export function ago(ts: string | null | undefined, now = Date.now()): string {
  if (!ts) return "never";
  const iso = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(ts) ? ts.replace(" ", "T") + "Z" : ts;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return ts;
  const s = Math.round((now - t) / 1000);
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

export type Health = "ok" | "warn" | "bad" | "idle";

/** Map Coolify/Docker status strings to a traffic light. */
export function statusHealth(status: string | null | undefined): Health {
  const s = (status ?? "").toLowerCase();
  if (s.startsWith("running")) return s.includes("unhealthy") ? "warn" : "ok";
  if (s.startsWith("restarting") || s.startsWith("starting") || s.includes("progress") || s === "queued") return "warn";
  if (s.startsWith("exited") || s.startsWith("dead") || s === "failed" || s.includes("degraded")) return "bad";
  if (s === "finished") return "ok";
  return "idle";
}

export function statusLabel(status: string | null | undefined): string {
  const s = status ?? "unknown";
  const [state, health] = s.split(":");
  if (!health || health === "unknown") return state ?? s;
  return `${state} · ${health}`;
}

export function shortSha(sha: string | null | undefined): string {
  if (!sha || sha === "HEAD") return sha ?? "";
  return sha.slice(0, 7);
}

/** First URL of a Coolify comma-separated fqdn list. */
export function primaryUrl(fqdn: string | null | undefined): string | null {
  const first = (fqdn ?? "").split(",").map((s) => s.trim()).find(Boolean);
  return first ?? null;
}

export function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** Disk media as people say it. */
export function mediaLabel(media: string | null | undefined): string {
  switch (media) {
    case "ssd":
      return "SSD";
    case "hdd":
      return "HDD";
    case "sd_card":
      return "SD card";
    default:
      return "";
  }
}

/** Hardware profile from the agent → a name. */
export function profileLabel(profile: string | null | undefined): string {
  switch (profile) {
    case "pi5":
      return "Raspberry Pi 5";
    case "pi4":
      return "Raspberry Pi 4";
    case "pi":
      return "Raspberry Pi";
    case "laptop":
      return "Laptop";
    case "pc":
      return "Desktop PC";
    default:
      return "Unknown machine";
  }
}

/** Days until an ISO time (negative when past, rounded down). */
export function daysUntil(ts: string | null | undefined, now = Date.now()): number | null {
  if (!ts) return null;
  const t = Date.parse(ts);
  return Number.isFinite(t) ? Math.floor((t - now) / 86_400_000) : null;
}
