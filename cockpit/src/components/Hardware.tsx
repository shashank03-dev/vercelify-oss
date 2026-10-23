import { Activity, CircuitBoard, Globe } from "lucide-react";
import { useMemo, useState } from "react";
import { agent } from "../lib/api";
import { hasCap } from "../lib/caps";
import { ago, daysUntil, mediaLabel, profileLabel, type Health } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { useLive } from "../lib/live";
import type { HardwareInfo, MetricPoint, SystemSnapshot, TailscaleStatus } from "../types";
import { Callout, Card, ErrorNote, Loading, Segmented, StatusDot, Tooltip } from "./ui";

/** Shown instead of a card's content when this server's agent is too old. */
export function NeedsUpdate({ what }: { what: string }) {
  return (
    <Callout tone="idle" title="Update this server">
      {what} needs a newer agent. Servers → Update (it keeps your apps and data).
    </Callout>
  );
}

interface Fact {
  label: string;
  value: string;
  health: Health;
  hint?: string;
}

/** What the machine is and how it copes: the facts that decide what can run on it. */
export function hardwareFacts(hw: HardwareInfo, ts?: TailscaleStatus): Fact[] {
  const out: Fact[] = [
    {
      label: "Machine",
      value: hw.model || profileLabel(hw.profile),
      health: "idle",
    },
    {
      label: "System disk",
      value: mediaLabel(hw.root_media) || "unknown",
      health: hw.root_media === "sd_card" ? "warn" : "ok",
      hint: hw.root_media === "sd_card" ? "SD cards wear out under a server's writes: boot from a USB SSD or NVMe, and keep backups on." : undefined,
    },
    {
      label: "Memory limits",
      value: hw.memory_cgroup ? "enforced" : "ignored by the kernel",
      health: hw.memory_cgroup ? "ok" : "bad",
      hint: hw.memory_cgroup ? undefined : "Re-run the installer (it enables the memory cgroup), then reboot.",
    },
  ];
  if (hw.throttling) {
    const t = hw.throttling;
    out.push({
      label: "Power supply",
      value: t.under_voltage_now ? "under-voltage now" : t.under_voltage_since_boot ? "dipped since boot" : t.throttled_since_boot ? "throttled since boot" : "OK",
      health: t.under_voltage_now ? "bad" : t.under_voltage_since_boot || t.throttled_since_boot ? "warn" : "ok",
      hint: t.under_voltage_now || t.under_voltage_since_boot ? "Use the official power supply (27 W for a Pi 5) and a short cable." : undefined,
    });
  }
  const mem = hw.pressure?.memory_some_avg10;
  if (mem != null) {
    out.push({
      label: "Memory pressure",
      value: `${mem.toFixed(1)}% stalled`,
      health: mem >= 40 ? "bad" : mem >= 15 ? "warn" : "ok",
      hint: "Share of the last 10 s that tasks waited for memory. Above ~15% apps feel slow.",
    });
  }
  if (hw.oom_kills != null) {
    out.push({
      label: "Out-of-memory kills",
      value: hw.oom_kills === 0 ? "none since boot" : `${hw.oom_kills} since boot`,
      health: hw.oom_kills === 0 ? "ok" : "warn",
      hint: hw.oom_kills ? "Builds are killed first, then previews, apps, and databases last." : undefined,
    });
  }
  out.push({ label: "Hang watchdog", value: hw.watchdog ? "armed" : "not available", health: hw.watchdog ? "ok" : "idle" });
  const expiry = daysUntil(ts?.self_node?.key_expiry);
  if (ts?.self_node) {
    out.push({
      label: "Tailscale key",
      value: expiry == null ? "never expires" : expiry < 0 ? "expired" : `expires in ${expiry} days`,
      health: expiry == null ? "ok" : expiry < 0 ? "bad" : expiry <= 14 ? "warn" : "ok",
      hint: expiry != null ? "Admin console → Machines → this machine → Disable key expiry." : undefined,
    });
  }
  return out;
}

export function HardwareCard({ s, ts }: { s?: SystemSnapshot; ts?: TailscaleStatus }) {
  const hw = s?.hardware;
  return (
    <Card title="Hardware" icon={<CircuitBoard />} description={hw ? profileLabel(hw.profile) : undefined} testId="hardware">
      {!s ? (
        <Loading rows={4} />
      ) : !hw ? (
        <NeedsUpdate what="Hardware checks (power supply, SD card, memory limits)" />
      ) : (
        <dl className="divide-y divide-line text-[13px]">
          {hardwareFacts(hw, ts).map((f) => (
            <div key={f.label} className="flex items-center gap-3 py-2">
              <dt className="w-40 shrink-0 text-fg-muted">{f.label}</dt>
              <dd className="flex min-w-0 items-center gap-2">
                <StatusDot health={f.health} />
                {f.hint ? (
                  <Tooltip content={f.hint}>
                    <span className="truncate underline decoration-line-strong decoration-dotted underline-offset-4">{f.value}</span>
                  </Tooltip>
                ) : (
                  <span className="truncate">{f.value}</span>
                )}
              </dd>
            </div>
          ))}
        </dl>
      )}
    </Card>
  );
}

type Series = "cpu" | "mem" | "temp";
const SERIES: { value: Series; label: string }[] = [
  { value: "cpu", label: "CPU" },
  { value: "mem", label: "Memory" },
  { value: "temp", label: "Temperature" },
];

function pick(p: MetricPoint, s: Series): number | null {
  if (s === "cpu") return p.cpu_pct;
  if (s === "mem") return p.mem_used_pct;
  return p.temp_c;
}

/** Downsample to at most `n` points (max of each bucket, so spikes survive). */
export function bucket(values: (number | null)[], n: number): (number | null)[] {
  if (values.length <= n) return values;
  const size = values.length / n;
  const out: (number | null)[] = [];
  for (let i = 0; i < n; i++) {
    const part = values.slice(Math.floor(i * size), Math.floor((i + 1) * size)).filter((v): v is number => v != null);
    out.push(part.length ? Math.max(...part) : null);
  }
  return out;
}

/** The last 24 hours, one line, recorded by the server itself every minute. */
export function HistoryCard() {
  const { health } = useLive();
  const supported = hasCap(health.data, "metrics");
  const m = usePoll(() => (supported ? agent.metrics() : Promise.resolve(null)), supported ? 60_000 : 0, [supported]);
  const [series, setSeries] = useState<Series>("cpu");
  const points = m.data?.points ?? [];
  const values = useMemo(() => bucket(points.map((p) => pick(p, series)), 240), [points, series]);
  const nums = values.filter((v): v is number => v != null);
  const max = series === "temp" ? Math.max(100, ...nums) : 100;
  const peak = nums.length ? Math.max(...nums) : null;
  const unit = series === "temp" ? "°C" : "%";
  const W = 480;
  const H = 160;
  let path = "";
  values.forEach((v, i) => {
    if (v == null) return;
    const x = values.length > 1 ? (i / (values.length - 1)) * W : 0;
    const y = H - 2 - (Math.min(max, Math.max(0, v)) / max) * (H - 4);
    path += `${path && values[i - 1] != null ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)} `;
  });
  const hours = points.length ? Math.max(1, Math.round((points[points.length - 1]!.t - points[0]!.t) / 3600)) : 0;

  return (
    <Card
      title="Last 24 hours"
      icon={<Activity />}
      description={points.length ? `${hours} h recorded on the server, one sample a minute` : undefined}
      actions={supported ? <Segmented value={series} onChange={setSeries} options={SERIES} label="Metric" /> : undefined}
      testId="history"
    >
      {!health.data ? (
        <Loading rows={2} />
      ) : !supported ? (
        <NeedsUpdate what="History" />
      ) : (
        <>
          <ErrorNote error={m.error} className="mb-3" />
          {points.length < 2 ? (
            <p className="text-[13px] text-fg-muted">The server records a sample every minute; the chart fills in from here.</p>
          ) : (
            <>
              <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="block h-40 w-full" role="img" aria-label={`${series} over the last ${hours} hours`}>
                <line x1="0" x2={W} y1={H / 2} y2={H / 2} stroke="var(--line)" strokeDasharray="2 4" vectorEffect="non-scaling-stroke" />
                <path d={path} fill="none" stroke="var(--fg-muted)" strokeWidth="1.5" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
              </svg>
              <div className="mt-2 flex justify-between text-[11px] text-fg-dim tabular-nums">
                <span>{hours} h ago</span>
                <span>peak {peak != null ? `${Math.round(peak)}${unit}` : "—"}</span>
                <span>now</span>
              </div>
            </>
          )}
        </>
      )}
    </Card>
  );
}

/** Every app URL and every pooled server, checked by the server each minute. */
export function UptimeCard() {
  const { health } = useLive();
  const supported = hasCap(health.data, "uptime");
  const u = usePoll(() => (supported ? agent.uptime() : Promise.resolve([])), supported ? 30_000 : 0, [supported]);
  const list = u.data ?? [];
  return (
    <Card title="Uptime" icon={<Globe />} description="Checked from the server every minute; changes are alerted" testId="uptime" flush>
      {!health.data ? (
        <div className="p-4">
          <Loading rows={2} />
        </div>
      ) : !supported ? (
        <div className="p-4">
          <NeedsUpdate what="Uptime checks" />
        </div>
      ) : list.length === 0 ? (
        <p className="p-4 text-[13px] text-fg-muted">No app URLs to check yet (stopped apps are skipped, workers check nothing).</p>
      ) : (
        <ul className="divide-y divide-line">
          {list.map((c) => (
            <li key={c.id} className="flex items-center gap-3 px-4 py-2.5 text-[13px]">
              <StatusDot health={c.up ? "ok" : "bad"} />
              <span className="w-40 shrink-0 truncate font-medium">{c.name.replace(/^(App|Server) /, "")}</span>
              <span className="min-w-0 flex-1 truncate font-mono text-xs text-fg-muted">{c.target}</span>
              <span className="shrink-0 text-xs text-fg-dim tabular-nums">
                {c.up ? `${c.latency_ms ?? "—"} ms · up ${ago(c.since).replace(" ago", "")}` : `down ${ago(c.since).replace(" ago", "")}${c.http_status ? ` · HTTP ${c.http_status}` : ""}`}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}
