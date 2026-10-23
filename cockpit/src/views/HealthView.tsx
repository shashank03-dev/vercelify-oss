import {
  BatteryCharging,
  BatteryWarning,
  Box,
  Cpu,
  HardDrive,
  MemoryStick,
  Network,
  Play,
  PlugZap,
  RefreshCw,
  RotateCcw,
  ScrollText,
  Server,
  AlertTriangle,
  ShieldCheck,
  Square,
  Thermometer,
} from "lucide-react";
import { useEffect, useState } from "react";
import { ActivityCard } from "../components/Activity";
import { HardwareCard, HistoryCard, UptimeCard } from "../components/Hardware";
import { useToast } from "../components/Toast";
import { Button, Card, cx, EmptyState, ErrorNote, IconButton, Loading, LogView, Meter, Modal, PageHeader, StatusDot, StatusText } from "../components/ui";
import { agent, local } from "../lib/api";
import { bytes, duration, mediaLabel, pct, type Health } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { inTauri } from "../lib/transport";
import type { ContainerInfo, PowerGuardState, SystemSnapshot, TailscaleNode, TailscaleStatus } from "../types";

export function HealthView() {
  const sys = usePoll(() => agent.system(), 3000);
  const ts = usePoll(() => agent.tailscale(), 10_000);
  const mine = usePoll(() => local.tailscale(), inTauri ? 15_000 : 0);
  // `docker stats` samples every container for ~2 s: keep it modest on a Pi.
  const containers = usePoll(() => agent.containers(true), 15_000);
  const services = usePoll(() => agent.services(), 10_000);
  const s = sys.data;
  const history = useHistory(s);

  return (
    <>
      <PageHeader
        title="Health"
        subtitle={
          s ? (
            <>
              {s.host.hostname} · {s.host.os} · kernel {s.host.kernel} · up {duration(s.host.uptime_secs)}
            </>
          ) : (
            "Live metrics from the host agent"
          )
        }
        actions={
          <IconButton label="Refresh" onClick={() => void Promise.all([sys.refresh(), ts.refresh(), containers.refresh(), services.refresh()])}>
            <RefreshCw />
          </IconButton>
        }
      />
      <ErrorNote error={sys.error} className="mb-4" />

      <Vitals s={s} history={history} />

      <div className="mb-6 grid grid-cols-1 gap-4 lg:grid-cols-2">
        <HardwareCard s={s} ts={ts.data} />
        <HistoryCard />
      </div>

      <div className="mb-6 grid grid-cols-1 gap-4 lg:grid-cols-[1.35fr_1fr]">
        <Card title="Machine" icon={<Cpu />}>
          {!s ? <Loading rows={4} /> : <Machine s={s} />}
        </Card>
        <PowerCard s={s} />
      </div>

      <div className="mb-6 grid grid-cols-1 gap-4 lg:grid-cols-2">
        <TailscaleCard title="Host on the tailnet" status={ts.data} error={ts.error} testId="ts-host" />
        <TailscaleCard
          title="This computer"
          status={mine.data}
          error={mine.error}
          compact
          testId="ts-local"
        />
      </div>

      <div className="mb-6 grid grid-cols-1 gap-6 xl:grid-cols-2">
        <UptimeCard />
        <ActivityCard />
      </div>

      <ContainersCard list={containers.data} error={containers.error} loading={containers.loading} refresh={containers.refresh} />

      <Card title="Services" icon={<ShieldCheck />} description="systemd units Vercelify depends on" className="mt-6" testId="services" flush>
        <ErrorNote error={services.error} className="m-4" />
        {services.loading && !services.data ? (
          <div className="p-4">
            <Loading rows={3} />
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-px bg-line lg:grid-cols-2 [&>*]:bg-surface">
            {(services.data ?? []).map((svc) => (
              <ServiceRow key={svc.name} name={svc.name} state={svc.active_state} enabled={svc.enabled} onDone={() => void services.refresh()} />
            ))}
          </div>
        )}
      </Card>
    </>
  );
}

/** 60 samples of the 3 s poll: the last three minutes. */
const MAX_SAMPLES = 60;

/** Last few minutes of CPU and memory, from the snapshots the page already polls. */
function useHistory(s?: SystemSnapshot) {
  const [h, setH] = useState<{ cpu: number[]; mem: number[] }>({ cpu: [], mem: [] });
  useEffect(() => {
    if (!s) return;
    const mem = pct(s.memory.used_bytes, s.memory.total_bytes);
    setH((p) => ({ cpu: [...p.cpu, s.cpu.usage_pct].slice(-MAX_SAMPLES), mem: [...p.mem, mem].slice(-MAX_SAMPLES) }));
  }, [s]);
  return h;
}

/** Small line of the last few minutes, no axes: the trend matters, not the scale. */
function MiniSpark({ values, max = 100 }: { values: number[]; max?: number }) {
  const W = 120;
  const H = 28;
  // One sample is drawn as a flat line, so the cell never looks empty while history builds up.
  if (values.length === 1) values = [values[0]!, values[0]!];
  const n = values.length;
  if (n < 2) return <div className="h-7" />;
  const pts = values.map((v, i) => [(i / (n - 1)) * W, H - 1 - (Math.min(max, Math.max(0, v)) / max) * (H - 2)] as const);
  const line = pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join(" ");
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="block h-7 w-full" aria-hidden="true">
      <path d={`${line} L${W} ${H} L0 ${H} Z`} fill="var(--fg)" fillOpacity="0.05" />
      <path d={line} fill="none" stroke="var(--fg-muted)" strokeWidth="1.5" strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

/**
 * The four numbers that tell you whether the old laptop is coping: CPU and
 * memory with the last three minutes drawn under them, the system disk, and
 * the hottest sensor. Same strip as the Apps page metrics.
 */
function Vitals({ s, history }: { s?: SystemSnapshot; history: { cpu: number[]; mem: number[] } }) {
  const ssd = s?.disks.find((d) => d.role === "system") ?? s?.disks[0];
  const temp = s?.thermal.max_c ?? null;
  const cells: { label: string; icon: React.ReactNode; value: string; hint: string; foot: React.ReactNode }[] = s
    ? [
        { label: "CPU", icon: <Cpu />, value: `${Math.round(s.cpu.usage_pct)}%`, hint: `${s.cpu.cores} threads`, foot: <MiniSpark values={history.cpu} /> },
        {
          label: "Memory",
          icon: <MemoryStick />,
          value: `${Math.round(pct(s.memory.used_bytes, s.memory.total_bytes))}%`,
          hint: `${bytes(s.memory.used_bytes)} of ${bytes(s.memory.total_bytes)}`,
          foot: <MiniSpark values={history.mem} />,
        },
        {
          label: "System disk",
          icon: <HardDrive />,
          value: ssd ? `${Math.round(pct(ssd.used_bytes, ssd.total_bytes))}%` : "—",
          hint: ssd ? `${bytes(ssd.available_bytes)} free` : "",
          foot: <div className="flex h-7 items-end pb-1">{ssd && <Meter value={pct(ssd.used_bytes, ssd.total_bytes)} label="system disk" />}</div>,
        },
        {
          label: "Hottest sensor",
          icon: <Thermometer />,
          value: temp !== null ? `${Math.round(temp)}°C` : "—",
          hint: temp === null ? "no sensors" : temp >= 85 ? "running hot" : temp >= 75 ? "warm" : "cool",
          foot: <div className="flex h-7 items-end pb-1">{temp !== null && <Meter value={Math.min(100, temp)} label="temperature" />}</div>,
        },
      ]
    : [];
  return (
    <div className="mb-6 grid grid-cols-2 overflow-hidden rounded-xl border border-line bg-surface lg:grid-cols-4" data-testid="resources">
      {!s
        ? [0, 1, 2, 3].map((i) => (
            <div key={i} className={cx("px-4 py-3.5", i > 0 && "border-l border-line")}>
              <Loading rows={1} />
            </div>
          ))
        : cells.map((c, i) => (
            <div key={c.label} className={cx("px-4 pt-3.5 pb-2", i > 0 && "border-l border-line", i === 2 && "max-lg:border-t max-lg:border-l-0", i === 3 && "max-lg:border-t")}>
              <div className="flex items-center gap-1.5 text-xs text-fg-muted">
                <span className="text-fg-dim [&>svg]:size-3.5">{c.icon}</span>
                {c.label}
              </div>
              <div className="mt-2 flex items-baseline gap-2">
                <span className="text-[24px] leading-none font-semibold tracking-tight tabular-nums">{c.value}</span>
                <span className="truncate text-[11px] text-fg-dim">{c.hint}</span>
              </div>
              <div className="mt-2">{c.foot}</div>
            </div>
          ))}
    </div>
  );
}

function Machine({ s }: { s: SystemSnapshot }) {
  return (
    <div className="space-y-4">
      <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-xs">
        <Fact k="Processor" v={s.cpu.model || "—"} />
        <Fact k="Load (1 / 5 / 15 min)" v={s.cpu.load_avg.map((l) => l.toFixed(2)).join("  ")} mono />
        <Fact k="System" v={`${s.host.os} · ${s.host.arch}`} />
        <Fact k="Kernel" v={s.host.kernel} mono />
        <Fact k="Swap" v={`${bytes(s.memory.swap_used_bytes)} of ${bytes(s.memory.swap_total_bytes)}`} />
        <Fact k="Agent" v={`v${s.host.agent_version} · up ${duration(s.host.uptime_secs)}`} />
      </dl>
      <div className="space-y-3 border-t border-line pt-4">
        {s.disks.map((d) => (
          <div key={d.mount_point}>
            <div className="mb-1.5 flex items-baseline justify-between gap-3 text-xs">
              <span className="flex min-w-0 items-center gap-2">
                <span className="font-medium text-fg">{d.role === "system" ? "System disk" : d.role === "storage" ? "Storage disk" : d.mount_point}</span>
                <span className="truncate font-mono text-[11px] text-fg-dim">
                  {d.device} · {mediaLabel(d.media) || d.file_system}
                </span>
              </span>
              <span className="shrink-0 text-fg-dim tabular-nums">
                <span className="text-fg">{bytes(d.available_bytes)}</span> free of {bytes(d.total_bytes)}
              </span>
            </div>
            <Meter value={pct(d.used_bytes, d.total_bytes)} label={d.mount_point} />
          </div>
        ))}
      </div>
    </div>
  );
}

function Fact({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <dt className="text-fg-dim">{k}</dt>
      <dd className={cx("selectable mt-0.5 truncate text-fg", mono && "font-mono text-[11.5px]")} title={v}>
        {v}
      </dd>
    </div>
  );
}

const GUARD_TONE: Record<string, Health> = { ok: "ok", warning: "warn", thermal_paused: "warn", shutting_down: "bad" };

function PowerCard({ s }: { s?: SystemSnapshot }) {
  const p = s?.power;
  const guard: PowerGuardState | null | undefined = s?.power_guard;
  const bat = p?.batteries[0];
  const temp = s?.thermal.max_c ?? null;
  return (
    <Card title="Power & thermal guard" icon={<PlugZap />} testId="power">
      {!s ? (
        <Loading rows={4} />
      ) : (
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <div className="flex size-10 items-center justify-center rounded-lg bg-fg/[0.06] text-fg-muted">
                {p?.on_battery ? <BatteryWarning className="size-5 text-warn" /> : <BatteryCharging className="size-5" />}
              </div>
              <div>
                <div className="font-medium" data-testid="power-source">
                  {p?.ac_online === null && !bat ? "No battery" : p?.on_battery ? "On battery" : "On AC power"}
                </div>
                <div className="text-xs text-fg-muted">
                  {bat ? `${bat.capacity_pct ?? "?"}% · ${bat.status}${bat.health_pct ? ` · health ${bat.health_pct}%` : ""}` : "desktop / Raspberry Pi"}
                  {bat?.charge_stop_threshold ? ` · charge limit ${bat.charge_stop_threshold}%` : ""}
                </div>
              </div>
            </div>
            {guard && <StatusText health={GUARD_TONE[guard.state] ?? "idle"} label={`Guard ${guard.state.replace("_", " ")}`} className="shrink-0 text-[12px] text-fg-muted" />}
          </div>
          {bat?.capacity_pct != null && <Meter value={bat.capacity_pct} tone="battery" label="battery" />}
          {guard && <div className="selectable text-xs text-fg-muted">{guard.reason}</div>}
          <div className="border-t border-line pt-3">
            <div className="mb-2 flex items-center justify-between text-sm">
              <span className="flex items-center gap-2">
                <Thermometer className="size-4 text-fg-dim" /> Hottest sensor
              </span>
              <span className={`font-semibold tabular-nums ${temp !== null && temp >= 85 ? "text-bad" : temp !== null && temp >= 75 ? "text-warn" : ""}`}>
                {temp !== null ? `${temp.toFixed(0)}°C` : "—"}
              </span>
            </div>
            <div className="max-h-28 space-y-1 overflow-auto text-xs text-fg-muted">
              {s.thermal.sensors.slice(0, 12).map((t) => (
                <div key={t.name} className="flex justify-between">
                  <span className="truncate">{t.name}</span>
                  <span className="tabular-nums">{t.temp_c.toFixed(1)}°C</span>
                </div>
              ))}
              {!s.thermal.sensors.length && <div className="text-fg-dim">no sensors exposed</div>}
            </div>
          </div>
        </div>
      )}
    </Card>
  );
}

function TailscaleCard({
  title,
  status,
  error,
  compact,
  testId,
}: {
  title: string;
  status?: TailscaleStatus;
  error?: { message: string };
  compact?: boolean;
  testId?: string;
}) {
  const me = status?.self_node;
  const ok = status?.backend_state === "Running";
  return (
    <Card
      title={title}
      icon={<Network />}
      testId={testId}
      actions={status && <StatusText health={ok ? "ok" : status.installed ? "warn" : "idle"} label={status.backend_state} className="text-[12px] text-fg-muted" />}
    >
      <ErrorNote error={error} />
      {!status ? (
        <Loading rows={2} />
      ) : !ok ? (
        <div className="text-[13px] text-fg-muted">
          {status.error ?? (status.backend_state === "NeedsLogin" ? "Run `sudo tailscale up` and sign in." : "Tailscale is not running.")}
          {status.auth_url && (
            <div className="mt-1">
              Login link: <span className="selectable font-mono text-fg">{status.auth_url}</span>
            </div>
          )}
        </div>
      ) : (
        <>
          <div className="mb-3 grid grid-cols-2 gap-3 text-xs">
            <KV k="MagicDNS" v={me?.dns_name ?? "—"} />
            <KV k="Tailscale IP" v={me?.ips.find((i) => i.includes(".")) ?? "—"} />
            <KV k="Tailnet" v={status.tailnet || status.magic_dns_suffix} />
            <KV k="Version" v={status.version.split("-")[0] ?? ""} />
          </div>
          {!compact && (
            <div className="-mx-2 max-h-52 space-y-0.5 overflow-auto border-t border-line pt-2">
              {status.peers.map((p) => (
                <Peer key={p.id || p.hostname} p={p} />
              ))}
              {!status.peers.length && <div className="px-2 text-xs text-fg-dim">No other devices yet.</div>}
            </div>
          )}
          {status.health.length > 0 && (
            <div className="mt-3 space-y-1 text-[11px] text-warn">
              {status.health.map((h) => (
                <div key={h} className="flex gap-1.5">
                  <AlertTriangle className="mt-px size-3 shrink-0" />
                  {h}
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </Card>
  );
}

function KV({ k, v }: { k: string; v: string }) {
  return (
    <div className="min-w-0">
      <div className="text-fg-dim">{k}</div>
      <div className="selectable truncate font-mono text-fg" title={v}>
        {v}
      </div>
    </div>
  );
}

function Peer({ p }: { p: TailscaleNode }) {
  const direct = !!p.cur_addr;
  return (
    <div className="flex items-center gap-3 rounded-md px-2 py-1.5 text-[13px] hover:bg-fg/[0.03]" data-testid="ts-peer">
      <StatusDot health={p.online ? "ok" : "idle"} />
      <span className="min-w-0 flex-1 truncate">{p.hostname}</span>
      <span className="text-[11px] text-fg-dim">{p.os}</span>
      <span className="w-28 truncate text-right font-mono text-[11px] text-fg-muted">{p.ips.find((i) => i.includes(".")) ?? ""}</span>
      <span className="w-20 text-right text-[11px] text-fg-dim">{p.online ? (direct ? "direct" : `relay · ${p.relay}`) : "offline"}</span>
    </div>
  );
}

const CONTAINER_GRID = "grid items-center gap-4 px-4 grid-cols-[minmax(0,1.3fr)_minmax(0,1fr)_64px_80px_96px]";

function ContainersCard({
  list,
  error,
  loading,
  refresh,
}: {
  list?: ContainerInfo[];
  error?: { message: string };
  loading: boolean;
  refresh: () => Promise<void>;
}) {
  const toast = useToast();
  const [logsOf, setLogsOf] = useState<ContainerInfo | null>(null);
  const act = (c: ContainerInfo, a: "start" | "stop" | "restart") =>
    agent.containerAction(c.id, a).then(
      () => {
        toast.ok(`${c.name}: ${a} done`);
        void refresh();
      },
      (e) => toast.bad(e.message),
    );
  return (
    <Card
      title="Containers"
      description={list ? `${list.filter((c) => c.state === "running").length} of ${list.length} running` : undefined}
      icon={<Box />}
      testId="containers"
      flush
    >
      <ErrorNote error={error} className="m-4" />
      {loading && !list ? (
        <div className="p-4">
          <Loading />
        </div>
      ) : !list?.length ? (
        <EmptyState icon={<Server />} title="No containers" />
      ) : (
        <div role="table" aria-label="Containers" className="text-[13px]">
          <div role="row" className={cx(CONTAINER_GRID, "h-8 border-b border-line bg-fg/[0.015] text-[11px] font-medium text-fg-dim")}>
            <span>Name</span>
            <span>Image</span>
            <span className="text-right">CPU</span>
            <span className="text-right">Memory</span>
            <span />
          </div>
          <div className="divide-y divide-line">
            {list.map((c) => (
              <div role="row" key={c.id} data-testid={`container-${c.name}`} className={cx(CONTAINER_GRID, "group py-2.5 transition-colors hover:bg-fg/[0.025]")}>
                <div className="min-w-0 leading-tight">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-medium" title={c.name}>
                      {c.coolify_name ?? c.name}
                    </span>
                    {c.coolify_managed && <span className="shrink-0 text-[11px] text-fg-dim">via Coolify</span>}
                  </div>
                  <StatusText
                    health={c.state === "running" ? "ok" : c.state === "paused" ? "warn" : "idle"}
                    label={c.status}
                    className="mt-1 text-[11px] text-fg-dim"
                  />
                </div>
                <span className="truncate font-mono text-[12px] text-fg-muted" title={c.image}>
                  {c.image}
                </span>
                <span className="text-right text-fg-muted tabular-nums">{c.cpu_pct != null ? `${c.cpu_pct.toFixed(1)}%` : "—"}</span>
                <span className="text-right text-fg-muted tabular-nums">{bytes(c.mem_bytes)}</span>
                <span className="flex justify-end gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
                  <IconButton label="Logs" onClick={() => setLogsOf(c)}>
                    <ScrollText />
                  </IconButton>
                  <IconButton label="Restart" onClick={() => void act(c, "restart")}>
                    <RotateCcw />
                  </IconButton>
                  {c.state === "running" ? (
                    <IconButton label="Stop" onClick={() => void act(c, "stop")}>
                      <Square />
                    </IconButton>
                  ) : (
                    <IconButton label="Start" onClick={() => void act(c, "start")}>
                      <Play />
                    </IconButton>
                  )}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
      {logsOf && <ContainerLogs c={logsOf} onClose={() => setLogsOf(null)} />}
    </Card>
  );
}

function ContainerLogs({ c, onClose }: { c: ContainerInfo; onClose: () => void }) {
  const logs = usePoll(() => agent.containerLogs(c.id, 400), 3000, [c.id]);
  return (
    <Modal open onClose={onClose} wide title={`Logs · ${c.coolify_name ?? c.name}`} description={<span className="font-mono">{c.image}</span>}>
      <ErrorNote error={logs.error} />
      <LogView testId="container-logs">{logs.data?.lines.join("\n") || (logs.loading ? "Loading…" : "No output.")}</LogView>
    </Modal>
  );
}

function ServiceRow({ name, state, enabled, onDone }: { name: string; state: string; enabled: boolean | null; onDone: () => void }) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const health: Health = state === "active" ? "ok" : state === "not-found" ? "idle" : state === "activating" ? "warn" : "bad";
  return (
    <div className="group flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-fg/[0.025]" data-testid={`service-${name}`}>
      <div className="min-w-0 flex-1 leading-tight">
        <div className="truncate font-mono text-[12.5px] text-fg">{name}</div>
        <StatusText
          health={health}
          label={`${state === "not-found" ? "not installed" : state}${enabled === false ? " · disabled" : ""}`}
          className="mt-1 text-[11px] text-fg-dim"
        />
      </div>
      {state !== "not-found" && (
        <Button
          size="sm"
          variant="ghost"
          busy={busy}
          icon={<RotateCcw />}
          className="opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100"
          onClick={async () => {
            setBusy(true);
            try {
              await agent.serviceAction(name, "restart");
              toast.ok(`${name} restarted`);
              onDone();
            } catch (e) {
              toast.bad((e as Error).message);
            } finally {
              setBusy(false);
            }
          }}
        >
          Restart
        </Button>
      )}
    </div>
  );
}
