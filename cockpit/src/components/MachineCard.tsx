import { ArrowUpRight, BatteryMedium, Plug } from "lucide-react";
import { IconButton, Loading, Meter } from "./ui";
import { bytes, duration, pct } from "../lib/format";
import { useLive } from "../lib/live";
import { useNav } from "../lib/nav";

/** The machine behind the apps: how much headroom it has left. */
export function MachineCard() {
  const { system: sys } = useLive();
  const { go } = useNav();
  const s = sys.data;
  const ssd = s?.disks.find((d) => d.role === "system");
  const hdd = s?.disks.find((d) => d.role === "storage");
  return (
    <section className="rounded-xl border border-line bg-surface" data-testid="machine-card">
      <header className="flex items-center justify-between gap-2 border-b border-line px-4 py-2.5">
        <div className="min-w-0">
          <div className="truncate text-[13px] font-medium">{s?.host.hostname ?? "This machine"}</div>
          <div className="mt-0.5 truncate text-[11px] text-fg-dim">{s ? `${s.host.os} · up ${duration(s.host.uptime_secs)}` : "…"}</div>
        </div>
        <IconButton label="Health" onClick={() => go("health")}>
          <ArrowUpRight />
        </IconButton>
      </header>
      <div className="space-y-3 px-4 py-3.5">
        {!s ? (
          <Loading rows={3} />
        ) : (
          <>
            <Usage label="CPU" value={s.cpu.usage_pct} detail={`${s.cpu.cores} threads`} />
            <Usage label="Memory" value={pct(s.memory.used_bytes, s.memory.total_bytes)} detail={`${bytes(s.memory.used_bytes)} / ${bytes(s.memory.total_bytes)}`} />
            {ssd && <Usage label="SSD" value={pct(ssd.used_bytes, ssd.total_bytes)} detail={`${bytes(ssd.available_bytes, 0)} free`} />}
            {hdd && <Usage label="HDD" value={pct(hdd.used_bytes, hdd.total_bytes)} detail={`${bytes(hdd.available_bytes, 0)} free`} />}
          </>
        )}
      </div>
      {s && (
        <div className="flex items-center gap-2 border-t border-line px-4 py-2.5 text-[11px] text-fg-muted">
          {s.power.on_battery ? <BatteryMedium className="size-3.5 text-warn" /> : <Plug className="size-3.5 text-fg-dim" />}
          {s.power.ac_online === null
            ? "Mains powered"
            : `${s.power.on_battery ? "On battery" : "On AC power"}${s.power.lowest_capacity_pct != null ? ` · ${Math.round(s.power.lowest_capacity_pct)}%` : ""}`}
          <span className="ml-auto tabular-nums text-fg-dim">load {s.cpu.load_avg[0].toFixed(2)}</span>
        </div>
      )}
    </section>
  );
}

function Usage({ label, value, detail }: { label: string; value: number; detail: string }) {
  return (
    <div>
      <div className="mb-1.5 flex items-baseline justify-between text-xs">
        <span className="text-fg-muted">{label}</span>
        <span className="text-fg-dim tabular-nums">
          <span className="text-fg">{Math.round(value)}%</span> · {detail}
        </span>
      </div>
      <Meter value={value} label={label} />
    </div>
  );
}
