import { ArrowRightLeft, ArrowUpCircle, Battery, BrainCircuit, Cpu, Layers, Plus, RefreshCw, Server, Thermometer, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { HealthChecks } from "../components/HealthChecks";
import { useToast } from "../components/Toast";
import { Button, Callout, Card, EmptyState, ErrorNote, IconButton, Meter, Modal, PageHeader, PixelWave, Select, ShimmerText, StatusDot, StatusText } from "../components/ui";
import { agentOn, coolifyOn, isWorker } from "../lib/api";
import { isPlatform } from "../lib/platform";
import { compareVersions, newestVersion } from "../lib/caps";
import { bytes, duration, pct } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { useNav } from "../lib/nav";
import { attachWorker, POOL_STEPS } from "../lib/pool";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import type { CockpitSettings, UpdateStatus } from "../types";

export function ServersView() {
  const { servers, settings, addServer } = useSession();
  const toast = useToast();
  const [attach, setAttach] = useState<CockpitSettings | null>(null);
  // Bumped by "Update all" so every card starts following its update.
  const [updateAll, setUpdateAll] = useState(0);
  const [updatingAll, setUpdatingAll] = useState(false);
  const mains = servers.filter((s) => !isWorker(s));
  // Agents of different versions: the older ones get an "update available" note.
  const [versions, setVersions] = useState<Record<string, string>>({});
  const newest = newestVersion(Object.values(versions));

  async function startAll() {
    setUpdatingAll(true);
    const failed: string[] = [];
    for (const s of servers) {
      try {
        await agentOn(s.agent_url).updateStart();
      } catch (e) {
        failed.push(`${s.hostname}: ${toCockpitError(e).message}`);
      }
    }
    setUpdatingAll(false);
    setUpdateAll((n) => n + 1);
    if (failed.length) toast.bad(`Could not start every update — ${failed.join("; ")}`);
    else toast.ok(`Updating ${servers.length} server${servers.length === 1 ? "" : "s"}; apps keep running`);
  }

  return (
    <>
      <PageHeader
        title="Servers"
        subtitle="Every old laptop and PC you've connected. Switch to one to deploy, run AI models or check its health, or pool workers into a main server so one dashboard deploys to all of them."
        actions={
          <>
            <Button icon={<ArrowUpCircle />} busy={updatingAll} onClick={() => void startAll()} data-testid="update-all">
              Update all
            </Button>
            <Button variant="primary" icon={<Plus />} onClick={addServer} data-testid="add-server-page">
              Add a machine
            </Button>
          </>
        }
      />
      <div className="grid grid-cols-1 gap-4 xl:grid-cols-2" data-testid="server-grid">
        {servers.map((s) => (
          <ServerCard
            key={s.agent_url}
            s={s}
            active={s.agent_url === settings.agent_url}
            follow={updateAll}
            newest={newest}
            onVersion={(v) => setVersions((p) => (p[s.agent_url] === v ? p : { ...p, [s.agent_url]: v }))}
            onAttach={mains.some((m) => m.agent_url !== s.agent_url) ? () => setAttach(s) : undefined}
          />
        ))}
        <button
          type="button"
          onClick={addServer}
          data-testid="add-server-card"
          className="group flex min-h-48 flex-col items-center justify-center gap-2 rounded-xl border border-dashed border-line-strong p-6 text-center transition hover:border-fg/30 hover:bg-surface"
        >
          <span className="flex size-10 items-center justify-center rounded-xl bg-fg/[0.06] text-fg-muted transition group-hover:bg-fg group-hover:text-inverse">
            <Plus className="size-5" />
          </span>
          <span className="text-sm font-medium">Add a machine</span>
          <span className="max-w-xs text-[13px] text-fg-muted">
            A separate server with its own Coolify, or a worker your main server deploys to. One command on the machine, a code here.
          </span>
        </button>
      </div>

      {attach && <AttachWorker worker={attach} mains={mains.filter((m) => m.agent_url !== attach.agent_url)} onClose={() => setAttach(null)} />}
    </>
  );
}

function ServerCard({
  s,
  active,
  follow,
  newest,
  onVersion,
  onAttach,
}: {
  s: CockpitSettings;
  active: boolean;
  follow: number;
  newest?: string;
  onVersion: (v: string) => void;
  onAttach?: () => void;
}) {
  const { select, remove, revision } = useSession();
  const { go } = useNav();
  const toast = useToast();
  const [confirmRemove, setConfirmRemove] = useState(false);
  const api = agentOn(s.agent_url);
  const sys = usePoll(() => api.system(), 10_000, [s.agent_url]);
  const llm = usePoll(() => api.llm(), 30_000, [s.agent_url]);
  const worker = isWorker(s);
  // The pool is Coolify's server list; platform servers get theirs with M9.
  const noPool = worker || isPlatform(s);
  const pool = usePoll(() => (noPool ? Promise.resolve([]) : coolifyOn(s.agent_url).servers()), noPool ? 0 : 30_000, [s.agent_url, revision]);
  const doctor = usePoll(() => api.doctor(), 60_000, [s.agent_url, revision]);
  const upd = useUpdate(s, follow, () => void Promise.all([sys.refresh(), doctor.refresh()]));
  const d = sys.data;
  const online = !!d && !sys.error;
  const version = d?.host.agent_version;
  useEffect(() => {
    if (version) onVersion(version);
    // onVersion is a new closure each render; the version is what matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version]);
  const behind = !!version && !!newest && compareVersions(version, newest) < 0;
  const ssd = d?.disks.find((x) => x.role === "system");
  const hdd = d?.disks.find((x) => x.role === "storage");
  const battery = d?.power.lowest_capacity_pct;

  return (
    <Card flush className={active ? "border-line-strong" : undefined} testId={`server-${s.hostname}`}>
      <div className="p-4">
        <div className="flex items-start gap-3">
          <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-fg/[0.06] text-fg-muted">
            {worker ? <Layers className="size-4" /> : <Server className="size-4" />}
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-2">
              <span className="truncate text-[14px] font-semibold tracking-tight">{d?.host.hostname ?? s.hostname}</span>
              <span className="shrink-0 text-[12px] text-fg-dim">{worker ? "Worker" : "Main server"}</span>
              {active && <span className="shrink-0 text-[12px] font-medium text-fg-muted">· Current</span>}
            </div>
            <div className="mt-0.5 truncate text-[12px] text-fg-dim" title={s.agent_url}>
              {d ? `${d.host.os} · ${d.cpu.cores} threads · up ${duration(d.host.uptime_secs)}` : s.agent_url}
            </div>
          </div>
          <StatusText
            health={online ? "ok" : sys.loading ? "idle" : "bad"}
            label={online ? "Online" : sys.loading ? "Checking" : "Unreachable"}
            className="text-[12px] text-fg-muted"
          />
        </div>

        {sys.error && !d && <ErrorNote error={sys.error} className="mt-3" />}
        {d && (
          <div className="mt-4 grid grid-cols-2 gap-x-5 gap-y-3 text-xs">
            <Metric label="CPU" icon={<Cpu />} value={`${Math.round(d.cpu.usage_pct)}%`}>
              <Meter value={d.cpu.usage_pct} label="CPU" />
            </Metric>
            <Metric label="Memory" value={`${bytes(d.memory.used_bytes)} / ${bytes(d.memory.total_bytes)}`}>
              <Meter value={pct(d.memory.used_bytes, d.memory.total_bytes)} label="Memory" />
            </Metric>
            {ssd && (
              <Metric label="SSD" value={`${bytes(ssd.available_bytes)} free`}>
                <Meter value={pct(ssd.used_bytes, ssd.total_bytes)} label="SSD" />
              </Metric>
            )}
            {hdd ? (
              <Metric label="HDD" value={`${bytes(hdd.available_bytes)} free`}>
                <Meter value={pct(hdd.used_bytes, hdd.total_bytes)} label="HDD" />
              </Metric>
            ) : (
              <div />
            )}
            <div className="col-span-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-fg-muted">
              {d.thermal.max_c != null && (
                <span className="inline-flex items-center gap-1">
                  <Thermometer className="size-3.5 text-fg-dim" /> {Math.round(d.thermal.max_c)} °C
                </span>
              )}
              {battery != null && (
                <span className="inline-flex items-center gap-1">
                  <Battery className="size-3.5 text-fg-dim" /> {battery}% {d.power.on_battery ? "on battery" : "plugged in"}
                </span>
              )}
              {llm.data?.installed && (
                <span className="inline-flex items-center gap-1.5">
                  <BrainCircuit className="size-3.5 text-fg-dim" />
                  <StatusDot health={llm.data.ready ? "ok" : "warn"} />
                  {llm.data.ready ? `${llm.data.models.length} model${llm.data.models.length === 1 ? "" : "s"}` : "AI stopped"}
                </span>
              )}
            </div>
          </div>
        )}

        <div className="mt-4 border-t border-line pt-3">
          <HealthChecks report={doctor.data} loading={doctor.loading} testId={`checks-${s.hostname}`} />
        </div>

        {upd.status && (upd.status.running || upd.status.exit_code != null) && upd.following && (
          <div className="mt-3 rounded-lg bg-fg/[0.04] px-3 py-2 text-xs" data-testid={`update-${s.hostname}`}>
            <div className="flex items-center gap-2">
              {upd.status.running ? (
                <>
                  <PixelWave size={3} pixel={3} gap={1.5} />
                  <ShimmerText className="font-medium">Updating…</ShimmerText>
                </>
              ) : (
                <StatusText
                  health={upd.status.exit_code === 0 ? "ok" : "bad"}
                  label={upd.status.exit_code === 0 ? "Updated" : `Update failed (exit ${upd.status.exit_code})`}
                  className="font-medium"
                />
              )}
            </div>
            {upd.status.log_tail.length > 0 && <div className="mt-1 truncate font-mono text-[11px] text-fg-dim">{upd.status.log_tail[upd.status.log_tail.length - 1]}</div>}
          </div>
        )}

        {!worker && !!pool.data?.length && (
          <div className="mt-4 border-t border-line pt-3">
            <div className="mb-1.5 text-[11px] font-medium text-fg-dim">Deploys to</div>
            <div className="flex flex-wrap gap-x-4 gap-y-1" data-testid="pool">
              {pool.data.map((p) => (
                <span key={p.uuid} className="inline-flex items-center gap-1.5 text-[12px] text-fg-muted">
                  <StatusDot health={p.settings?.is_reachable === false ? "bad" : p.settings?.is_usable === false ? "warn" : "ok"} />
                  {p.name}
                </span>
              ))}
            </div>
          </div>
        )}
      </div>

      <div className="flex items-center justify-between gap-2 border-t border-line px-3 py-2">
        <div className="flex items-center gap-1">
          {confirmRemove ? (
            <>
              <Button size="sm" variant="danger" onClick={() => void remove(s.agent_url).then(() => toast.ok(`Removed ${s.hostname}`), (e: Error) => toast.bad(e.message))}>
                Remove {s.hostname}?
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setConfirmRemove(false)}>
                Keep
              </Button>
            </>
          ) : (
            <IconButton label="Remove from this Mac" onClick={() => setConfirmRemove(true)}>
              <Trash2 />
            </IconButton>
          )}
          <IconButton label="Refresh" onClick={() => void Promise.all([sys.refresh(), llm.refresh(), pool.refresh(), doctor.refresh()])}>
            <RefreshCw />
          </IconButton>
          {behind && (
            <span className="text-[11px] text-warn" data-testid={`update-available-${s.hostname}`}>
              {version} → {newest}
            </span>
          )}
          <IconButton label={`Update Vercelify (agent ${d?.host.agent_version ?? "?"})`} disabled={upd.busy || !!upd.status?.running} onClick={() => void upd.start()}>
            <ArrowUpCircle />
          </IconButton>
        </div>
        <div className="flex items-center gap-1.5">
          {worker && onAttach && (
            <Button size="sm" icon={<Layers />} onClick={onAttach} data-testid={`attach-${s.hostname}`}>
              Add to a pool
            </Button>
          )}
          {active ? (
            <Button size="sm" variant="primary" onClick={() => go(worker ? "health" : "apps")}>
              Open
            </Button>
          ) : (
            <Button
              size="sm"
              variant="primary"
              icon={<ArrowRightLeft />}
              data-testid={`switch-to-${s.hostname}`}
              onClick={() => void select(s.agent_url).then(() => toast.info(`Switched to ${s.hostname}`), (e: Error) => toast.bad(e.message))}
            >
              Switch to
            </Button>
          )}
        </div>
      </div>
    </Card>
  );
}

function Metric({ label, value, icon, children }: { label: string; value: string; icon?: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="mb-1 flex items-center justify-between gap-2 text-fg-muted">
        <span className="inline-flex items-center gap-1 [&>svg]:size-3.5 [&>svg]:text-fg-dim">
          {icon}
          {label}
        </span>
        <span className="truncate text-fg tabular-nums">{value}</span>
      </div>
      {children}
    </div>
  );
}

/** Follow `vercelify update` on one server; the agent restarts midway, so polls may fail for a moment. */
function useUpdate(s: CockpitSettings, follow: number, onFinished: () => void) {
  const toast = useToast();
  const api = agentOn(s.agent_url);
  const [status, setStatus] = useState<UpdateStatus>();
  // Each start (here or "Update all") bumps this and restarts the polling.
  const [run, setRun] = useState(0);
  const [busy, setBusy] = useState(false);
  const done = useRef(onFinished);
  done.current = onFinished;

  useEffect(() => {
    if (follow) setRun((r) => r + 1);
  }, [follow]);

  useEffect(() => {
    if (!run) return;
    let stop = false;
    const tick = async () => {
      try {
        const st = await api.update();
        if (stop) return;
        setStatus(st);
        if (!st.running && st.exit_code != null) {
          stop = true;
          if (st.exit_code === 0) toast.ok(`${s.hostname} is up to date`);
          else toast.bad(`Updating ${s.hostname} failed — see its log: sudo vercelify logs setup`);
          done.current();
        }
      } catch {
        /* the agent restarts during the update */
      }
    };
    void tick();
    const id = window.setInterval(() => !stop && void tick(), 3000);
    return () => {
      stop = true;
      window.clearInterval(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [run, s.agent_url]);

  async function start() {
    setBusy(true);
    try {
      setStatus(await api.updateStart());
      setRun((r) => r + 1);
      toast.info(`Updating ${s.hostname}; apps keep running`);
    } catch (e) {
      toast.bad(toCockpitError(e).message);
    } finally {
      setBusy(false);
    }
  }
  return { status, following: run > 0, busy, start };
}

/** Let a main server's Coolify deploy to this worker over SSH (on the tailnet). */
function AttachWorker({ worker, mains, onClose }: { worker: CockpitSettings; mains: CockpitSettings[]; onClose: () => void }) {
  const toast = useToast();
  const [main, setMain] = useState(mains[0]?.agent_url ?? "");
  const [step, setStep] = useState(-1);
  const [error, setError] = useState<string>();
  const [done, setDone] = useState(false);
  const mainS = mains.find((m) => m.agent_url === main);

  async function run() {
    if (!mainS) return;
    setError(undefined);
    setDone(false);
    try {
      const added = await attachWorker(worker, mainS, setStep);
      setStep(POOL_STEPS.length);
      setDone(true);
      toast.ok(added ? `${worker.hostname} joined ${mainS.hostname}'s pool — Coolify is validating it` : `${worker.hostname} is already in ${mainS.hostname}'s pool`);
    } catch (e) {
      setError(toCockpitError(e).message);
    }
  }

  return (
    <Modal open onClose={onClose} title={`Add ${worker.hostname} to a pool`} description="The main server's Coolify will build and run apps on this machine over Tailscale.">
      {mains.length === 0 ? (
        <EmptyState icon={<Server />} title="No main server paired">
          Pair a server that runs Coolify first.
        </EmptyState>
      ) : (
        <div className="space-y-4">
          <label className="flex flex-col gap-1.5 text-xs font-medium text-fg-muted">
            Main server
            <Select value={main} onChange={(e) => setMain(e.target.value)} disabled={step >= 0 && !error && !done}>
              {mains.map((m) => (
                <option key={m.agent_url} value={m.agent_url}>
                  {m.hostname}
                </option>
              ))}
            </Select>
          </label>
          <ol className="space-y-2 text-[13px]" data-testid="attach-steps">
            {POOL_STEPS.map((label, i) => (
              <li key={label} className="flex items-center gap-2.5">
                <span className="flex w-3 justify-center">
                  {step === i && !error ? (
                    <PixelWave size={2} pixel={3} gap={1.5} />
                  ) : (
                    <StatusDot health={step > i ? "ok" : step === i ? "bad" : "idle"} />
                  )}
                </span>
                <span className={step >= i ? "text-fg" : "text-fg-muted"}>{label}</span>
              </li>
            ))}
          </ol>
          {error && <Callout tone="bad">{error}</Callout>}
          {done && (
            <Callout tone="idle" title="Done">
              When you create an app, pick <span className="font-medium text-fg">{worker.hostname}</span> as its server. Coolify needs a minute to validate
              the connection and check Docker.
            </Callout>
          )}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={onClose}>
              {done ? "Close" : "Cancel"}
            </Button>
            {!done && (
              <Button variant="primary" busy={step >= 0 && step < POOL_STEPS.length && !error} onClick={() => void run()} data-testid="attach-run">
                {error ? "Try again" : "Add to pool"}
              </Button>
            )}
          </div>
        </div>
      )}
    </Modal>
  );
}
