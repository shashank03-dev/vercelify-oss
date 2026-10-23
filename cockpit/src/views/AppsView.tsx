import {
  ArrowUpRight,
  BatteryMedium,
  Boxes,
  BrainCircuit,
  ChevronRight,
  CircleAlert,
  Database,
  ExternalLink,
  FileJson,
  GitBranch,
  GitPullRequest,
  Globe,
  History,
  KeyRound,
  LayoutDashboard,
  Play,
  Plug,
  Plus,
  RefreshCw,
  Rocket,
  RotateCcw,
  ScrollText,
  Search,
  Server,
  Sparkles,
  Square,
  Trash2,
  Zap,
} from "lucide-react";
import { motion } from "framer-motion";
import { useEffect, useMemo, useRef, useState } from "react";
import { AppDomains } from "../components/AppDomains";
import { AutoBadge, AutoDeployPanel, NewAppModal } from "../components/AutoDeploy";
import { DeployTimeline } from "../components/DeployTimeline";
import { EnvPanel } from "../components/EnvPanel";
import { GettingStarted } from "../components/GettingStarted";
import { ProjectPanel } from "../components/ProjectPanel";
import { useToast } from "../components/Toast";
import {
  Button,
  AutoHeight,
  Callout,
  Card,
  CopyButton,
  cx,
  EmptyState,
  ErrorNote,
  IconButton,
  InlineCode,
  Kbd,
  Loading,
  LogView,
  Meter,
  Modal,
  OrbitButton,
  PageHeader,
  PixelWave,
  Segmented,
  Select,
  ShimmerText,
  StatusDot,
  StatusText,
  Tabs,
  TextInput,
} from "../components/ui";
import { agent, coolify, defaultTarget, isWorker, local, parseDeploymentLogs } from "../lib/api";
import { ago, bytes, duration, hostOf, pct, primaryUrl, shortSha, statusHealth, statusLabel } from "../lib/format";
import { EXPLAIN_MAX_TOKENS, EXPLAIN_MODEL, explainMessages, pickExplainModel } from "../lib/explain";
import { usePoll } from "../lib/hooks";
import { tokensPerSecond } from "../lib/models";
import { useLive } from "../lib/live";
import { hasCap } from "../lib/caps";
import { useNav } from "../lib/nav";
import { openExternal } from "../lib/open";
import { useSession } from "../lib/session";
import { toCockpitError } from "../lib/transport";
import type { AutoDeployStatus, AutoDeployWatch, CoolifyApplication, CoolifyDatabase, CoolifyDeployment, CoolifyDeploymentLog, DatabaseKind } from "../types";

type Filter = "all" | "running" | "issues";

export function AppsView() {
  const { settings, servers, select } = useSession();
  if (isWorker(settings)) {
    const mains = servers.filter((s) => !isWorker(s));
    return (
      <>
        <PageHeader title="Apps" subtitle={`${settings.hostname} is a worker: it runs apps for a main server's Coolify.`} />
        <div className="rounded-xl border border-dashed border-line-strong bg-surface/50">
          <EmptyState
            icon={<Boxes />}
            title="Deploy from your main server"
            action={
              mains[0] && (
                <Button variant="primary" onClick={() => void select(mains[0]!.agent_url)}>
                  Switch to {mains[0].hostname}
                </Button>
              )
            }
          >
            Add this machine to a main server's pool on the Servers page, then pick it under “Run on” when you create an app there.
          </EmptyState>
        </div>
      </>
    );
  }
  return <MainApps />;
}

function MainApps() {
  const apps = usePoll(() => coolify.applications(), 5000);
  const auto = usePoll(() => agent.autodeploy(), 15_000);
  const { asleep } = useLive();
  const [newApp, setNewApp] = useState(false);
  const [newDb, setNewDb] = useState(false);
  const dbs = usePoll(() => coolify.databases(), 8000);
  const [selected, setSelected] = useState<{ app: CoolifyApplication; tab?: DetailTab } | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const { intent, clearIntent } = useNav();
  const toast = useToast();

  // "New app" from the command palette.
  useEffect(() => {
    if (!intent?.newApp) return;
    setNewApp(true);
    clearIntent();
  }, [intent, clearIntent]);

  // "Open <app>" from the command palette.
  useEffect(() => {
    if (!intent?.app || !apps.data) return;
    const a = apps.data.find((x) => x.uuid === intent.app);
    if (a) setSelected({ app: a });
    clearIntent();
  }, [intent, apps.data, clearIntent]);

  const sorted = useMemo(() => [...(apps.data ?? [])].sort((a, b) => a.name.localeCompare(b.name)), [apps.data]);
  // A sleeping app is stopped on purpose: not a problem.
  const healthOf = (a: CoolifyApplication) => (asleep.has(a.uuid) ? "idle" : statusHealth(a.status));
  const running = sorted.filter((a) => healthOf(a) === "ok").length;
  const failing = sorted.filter((a) => healthOf(a) === "bad");
  const issues = sorted.filter((a) => ["bad", "warn"].includes(healthOf(a))).length;
  const visible = sorted.filter((a) => {
    const h = healthOf(a);
    if (filter === "running" && h !== "ok") return false;
    if (filter === "issues" && h !== "bad" && h !== "warn") return false;
    const q = query.trim().toLowerCase();
    return !q || [a.name, a.fqdn, a.git_repository, a.git_branch].some((s) => s?.toLowerCase().includes(q));
  });
  const tokenProblem = apps.error && (apps.error.status === 401 || /token/i.test(apps.error.message));

  return (
    <>
      <PageHeader
        title="Apps"
        subtitle="Push to deploy, preview every pull request and roll back in one click, all on your own machine."
        actions={
          <>
            <IconButton label="Refresh" onClick={() => void Promise.all([apps.refresh(), dbs.refresh()])}>
              <RefreshCw />
            </IconButton>
            <Button icon={<LayoutDashboard />} onClick={() => local.openCoolify().catch((e) => toast.bad(e.message))}>
              Coolify
            </Button>
            <Button variant="primary" icon={<Plus />} onClick={() => setNewApp(true)} data-testid="new-app">
              New app
            </Button>
          </>
        }
      />

      {tokenProblem ? (
        <Callout className="mb-6" title="Coolify API not connected">
          {apps.error?.message}. On the host run <InlineCode>sudo vercelify pair --refresh</InlineCode> and paste the new code in
          Settings.
        </Callout>
      ) : (
        <ErrorNote error={apps.error} className="mb-6" />
      )}

      {failing.length > 0 && (
        <div className="mb-6 flex flex-wrap items-center gap-3 rounded-xl border border-bad/25 bg-bad/[0.06] px-4 py-3" role="status" data-testid="apps-attention">
          <CircleAlert className="size-4 shrink-0 text-bad" />
          <span className="min-w-0 flex-1 text-[13px]">
            <span className="font-medium text-fg">
              {failing.length === 1 ? `${failing[0]!.name} is ${appState(failing[0]!.status).toLowerCase()}` : `${failing.length} apps are stopped or failing`}
            </span>
            <span className="text-fg-muted"> · its logs and deployments show why.</span>
          </span>
          {failing.slice(0, 3).map((a) => (
            <Button key={a.uuid} size="sm" onClick={() => setSelected({ app: a, tab: "logs" })}>
              Open {a.name}
            </Button>
          ))}
        </div>
      )}

      <GettingStarted apps={apps.data} onNewApp={() => setNewApp(true)} onDomains={(app) => setSelected({ app, tab: "domains" })} />

      <div className="grid items-start gap-5 xl:grid-cols-[minmax(0,1fr)_280px]">
        <div className="min-w-0 space-y-5">
          <section className="@container overflow-hidden rounded-xl border border-line bg-surface" data-testid="apps-card">
            <header className="flex flex-wrap items-center justify-between gap-3 border-b border-line px-3 py-2">
              <Segmented
                label="app status filter"
                value={filter}
                onChange={setFilter}
                options={[
                  { value: "all", label: <>All <Count n={sorted.length} /></> },
                  { value: "running", label: <>Running <Count n={running} /></> },
                  { value: "issues", label: <>Issues <Count n={issues} /></> },
                ]}
              />
              <div className="relative">
                <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-fg-dim" />
                <TextInput
                  placeholder="Filter by name, domain, repo…"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  className="h-8 w-64 pl-8"
                  spellCheck={false}
                  data-testid="app-filter"
                />
              </div>
            </header>

            {apps.loading && !apps.data ? (
              <div className="p-4">
                <Loading rows={4} />
              </div>
            ) : sorted.length === 0 ? (
              <EmptyState
                icon={<Rocket />}
                title="No applications yet"
                action={
                  <Button variant="primary" icon={<Plus />} onClick={() => setNewApp(true)}>
                    Deploy from Git
                  </Button>
                }
              >
                Paste a repository URL and pick a branch. Vercelify builds it, gives it a URL and deploys again every time you push to that
                branch.
              </EmptyState>
            ) : visible.length === 0 ? (
              <div className="py-10 text-center text-[13px] text-fg-muted">
                No apps match.{" "}
                <button className="text-fg underline-offset-2 hover:underline" onClick={() => (setQuery(""), setFilter("all"))}>
                  Clear filters
                </button>
              </div>
            ) : (
              <div role="table" aria-label="Applications">
                <div role="row" className={cx(ROW_GRID, "h-8 border-b border-line bg-fg/[0.015] text-[11px] font-medium text-fg-dim")}>
                  <span role="columnheader">Name</span>
                  <span role="columnheader">Status</span>
                  <span role="columnheader" className="hidden @3xl:block">
                    Domain
                  </span>
                  <span role="columnheader" className="hidden @5xl:block">
                    Updated
                  </span>
                  <span />
                </div>
                <div className="divide-y divide-line">
                  {visible.map((a) => (
                    <AppRow
                      key={a.uuid}
                      app={a}
                      watch={auto.data?.watches.find((w) => w.app_uuid === a.uuid)}
                      asleep={asleep.has(a.uuid)}
                      onOpen={() => setSelected({ app: a })}
                      onChanged={() => void apps.refresh()}
                    />
                  ))}
                </div>
              </div>
            )}
          </section>

          <DatabasesCard
            dbs={dbs.data}
            loading={dbs.loading}
            error={dbs.error}
            refresh={dbs.refresh}
            creating={newDb}
            setCreating={setNewDb}
          />
        </div>

        <aside className="space-y-5">
          <MachineCard />
          <Shortcuts onNewApp={() => setNewApp(true)} onNewDb={() => setNewDb(true)} />
        </aside>
      </div>

      {selected && (
        <AppDetail
          key={`${selected.app.uuid}|${selected.tab ?? ""}`}
          app={sorted.find((a) => a.uuid === selected.app.uuid) ?? selected.app}
          initialTab={selected.tab}
          auto={auto.data}
          refreshAuto={auto.refresh}
          onClose={() => setSelected(null)}
        />
      )}
      <NewAppModal
        open={newApp}
        onClose={() => setNewApp(false)}
        onCreated={() => {
          setNewApp(false);
          void Promise.all([apps.refresh(), auto.refresh()]);
        }}
      />
    </>
  );
}

function Count({ n }: { n: number }) {
  return <span className="text-[10px] text-fg-dim tabular-nums">{n}</span>;
}

/** Shared by the header and every row so the columns line up. */
const ROW_GRID =
  "grid items-center gap-4 px-4 grid-cols-[minmax(0,1fr)_120px_184px] @3xl:grid-cols-[minmax(0,1.4fr)_120px_minmax(0,1fr)_184px] @5xl:grid-cols-[minmax(0,1.4fr)_120px_minmax(0,1fr)_72px_184px]";

/** The machine behind the apps: how much headroom it has left. */
function MachineCard() {
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

/** Common next steps, Raycast-style: icon, label, key hint. */
function Shortcuts({ onNewApp, onNewDb }: { onNewApp: () => void; onNewDb: () => void }) {
  const { go, openPalette } = useNav();
  const { addServer } = useSession();
  const toast = useToast();
  const items: { icon: React.ReactNode; label: string; hint?: React.ReactNode; run: () => void }[] = [
    { icon: <GitBranch />, label: "Deploy a Git repository", run: onNewApp },
    { icon: <Database />, label: "Create a database", run: onNewDb },
    { icon: <BrainCircuit />, label: "Run a local AI model", run: () => go("ai") },
    { icon: <Server />, label: "Add another machine", run: addServer },
    { icon: <LayoutDashboard />, label: "Open Coolify", hint: <ExternalLink className="size-3" />, run: () => void local.openCoolify().catch((e: Error) => toast.bad(e.message)) },
  ];
  return (
    <section className="rounded-xl border border-line bg-surface p-1.5">
      <div className="flex items-center justify-between px-2.5 pt-1.5 pb-1 text-[11px] font-medium text-fg-dim">
        Shortcuts
        <button type="button" onClick={openPalette} className="flex items-center gap-1 hover:text-fg-muted">
          all actions <Kbd className="h-4 min-w-4 text-[10px]">⌘K</Kbd>
        </button>
      </div>
      {items.map((i) => (
        <button
          key={i.label}
          type="button"
          onClick={i.run}
          className="group flex h-8 w-full items-center gap-2.5 rounded-md px-2.5 text-left text-[13px] text-fg-muted transition hover:bg-fg/[0.05] hover:text-fg [&>svg]:size-4"
        >
          <span className="text-fg-dim group-hover:text-fg-muted [&>svg]:size-4">{i.icon}</span>
          <span className="flex-1 truncate">{i.label}</span>
          <span className="text-fg-dim opacity-0 transition group-hover:opacity-100">{i.hint ?? <ChevronRight className="size-3.5" />}</span>
        </button>
      ))}
    </section>
  );
}

/** "running:healthy" -> "Running", "running:unhealthy" -> "Unhealthy", "exited:…" -> "Exited". */
function appState(status: string): string {
  const [state = "unknown", health] = status.split(":");
  const word = state === "running" && health === "unhealthy" ? "unhealthy" : state;
  return word.charAt(0).toUpperCase() + word.slice(1);
}

function repoLabel(app: CoolifyApplication): string {
  if (app.git_repository && app.build_pack !== "dockerimage") {
    const repo = app.git_repository.replace(/^https?:\/\/(www\.)?github\.com\//, "").replace(/\.git$/, "");
    return `${repo}${app.git_branch ? `@${app.git_branch}` : ""}`;
  }
  if (app.docker_registry_image_name) return `${app.docker_registry_image_name}:${app.docker_registry_image_tag ?? "latest"}`;
  return "—";
}

function Monogram({ name }: { name: string }) {
  return (
    <div className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-line-strong bg-gradient-to-b from-surface-3 to-surface-2 text-[11px] font-semibold tracking-wide text-fg-muted uppercase">
      {name.replace(/[^a-z0-9]/gi, "").slice(0, 2) || "?"}
    </div>
  );
}

function AppRow({
  app,
  watch,
  asleep,
  onOpen,
  onChanged,
}: {
  app: CoolifyApplication;
  watch?: AutoDeployWatch;
  /** Stopped because nobody visited it; the next visit wakes it. */
  asleep?: boolean;
  onOpen: () => void;
  onChanged: () => void;
}) {
  const toast = useToast();
  const [busy, setBusy] = useState<string>();
  const url = primaryUrl(app.fqdn);
  const health = asleep ? "idle" : statusHealth(app.status);
  const isRunning = app.status.startsWith("running");

  async function act(name: string, fn: () => Promise<unknown>, okText: string) {
    setBusy(name);
    try {
      await fn();
      toast.ok(okText);
      onChanged();
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(undefined);
    }
  }

  return (
    <div role="row" className={cx(ROW_GRID, "group h-[60px] transition-colors hover:bg-fg/[0.025]")} data-testid={`app-${app.name}`}>
      <button type="button" onClick={onOpen} className="flex min-w-0 items-center gap-3 text-left" data-testid="app-open">
        <Monogram name={app.name} />
        <span className="min-w-0">
          <span className="flex items-center gap-2">
            <span className="truncate text-[13px] font-medium text-fg">{app.name}</span>
            <AutoBadge watch={watch} />
          </span>
          <span className="mt-0.5 flex items-center gap-1 text-[11.5px] text-fg-dim">
            <GitBranch className="size-3 shrink-0" />
            <span className="truncate font-mono">{repoLabel(app)}</span>
            <span className="hidden shrink-0 @4xl:inline">· {app.build_pack}</span>
          </span>
        </span>
      </button>

      <StatusText health={health} label={asleep ? "Sleeping" : appState(app.status)} testId="app-status" className={cx("text-[12px]", health === "ok" ? "text-fg-muted" : "text-fg")} />

      <span className="hidden min-w-0 @3xl:block">
        {url ? (
          <button
            type="button"
            onClick={() => void openExternal(url)}
            className="inline-flex max-w-full items-center gap-1 text-fg-muted hover:text-fg"
          >
            <span className="truncate font-mono text-[12px]">{hostOf(url)}</span>
            <ArrowUpRight className="size-3 shrink-0 text-fg-dim" />
          </button>
        ) : (
          <span className="text-[12px] text-fg-dim">—</span>
        )}
      </span>

      <span className="hidden text-[12px] text-fg-dim tabular-nums @5xl:block">{ago(app.updated_at)}</span>

      <div className="flex items-center justify-end gap-0.5">
        <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100">
          <IconButton label="Deployments & logs" onClick={onOpen}>
            <History />
          </IconButton>
          <IconButton
            label="Restart"
            disabled={!!busy}
            onClick={() => act("restart", () => coolify.appAction(app.uuid, "restart"), `Restarting ${app.name}`)}
          >
            <RotateCcw />
          </IconButton>
          {isRunning ? (
            <IconButton label="Stop" disabled={!!busy} onClick={() => act("stop", () => coolify.appAction(app.uuid, "stop"), `Stopping ${app.name}`)}>
              <Square />
            </IconButton>
          ) : (
            <IconButton label="Start" disabled={!!busy} onClick={() => act("start", () => coolify.appAction(app.uuid, "start"), `Starting ${app.name}`)}>
              <Play />
            </IconButton>
          )}
        </div>
        <Button
          size="sm"
          className="ml-1.5"
          busy={busy === "deploy"}
          icon={<Rocket />}
          onClick={() => {
            const t = toast.progress(`Queueing a deployment of ${app.name}`);
            setBusy("deploy");
            coolify
              .deploy(app.uuid)
              .then(
                () => (t.ok(`Deployment of ${app.name} queued`), onChanged()),
                (e: Error) => t.bad(e.message),
              )
              .finally(() => setBusy(undefined));
          }}
          data-testid={`deploy-${app.name}`}
        >
          Deploy
        </Button>
      </div>
    </div>
  );
}

/**
 * Newest first. The newest finished production deployment is live; older
 * finished production deployments of *other* commits can be rolled back to
 * (offered once per commit).
 */
export function rollbackTargets(list: CoolifyDeployment[]): { d: CoolifyDeployment; canRollback: boolean; live: boolean }[] {
  let live: string | null = null;
  const offered = new Set<string>();
  return list.map((d) => {
    let canRollback = false;
    let isLive = false;
    if (d.pull_request_id === 0 && d.status === "finished" && d.commit && d.commit !== "HEAD") {
      if (live === null) {
        live = d.commit;
        isLive = true;
      } else if (d.commit !== live && !offered.has(d.commit)) {
        canRollback = true;
        offered.add(d.commit);
      }
    }
    return { d, canRollback, live: isLive };
  });
}

type DetailTab = "deployments" | "previews" | "logs" | "env" | "domains" | "project" | "autodeploy";

function AppDetail({
  app,
  initialTab,
  auto,
  refreshAuto,
  onClose,
}: {
  app: CoolifyApplication;
  initialTab?: DetailTab;
  auto: AutoDeployStatus | undefined;
  refreshAuto: () => Promise<void>;
  onClose: () => void;
}) {
  const toast = useToast();
  const [tab, setTab] = useState<DetailTab>(initialTab ?? "deployments");
  const watch = auto?.watches.find((w) => w.app_uuid === app.uuid);
  const deps = usePoll(() => coolify.deployments(app.uuid, 15), 3000, [app.uuid]);
  const previews = usePoll(() => coolify.previews(app.uuid), 6000, [app.uuid]);
  const { health, apps } = useLive();
  const withImages = hasCap(health.data, "images");
  // Which earlier versions are still on disk (instant rollback) — apps on this server only.
  const images = usePoll(() => (withImages ? agent.images(app.uuid).catch(() => null) : Promise.resolve(null)), withImages ? 30_000 : 0, [app.uuid, withImages]);
  const runtime = usePoll(() => (tab === "logs" ? coolify.appLogs(app.uuid, 300) : Promise.resolve(null)), tab === "logs" ? 4000 : 0, [tab]);
  const [logFor, setLogFor] = useState<CoolifyDeployment | null>(null);
  const url = primaryUrl(app.fqdn);

  const deployments = rollbackTargets(deps.data?.deployments ?? []);

  async function rollback(d: CoolifyDeployment) {
    if (!d.commit) return;
    try {
      await coolify.rollback(app.uuid, d.commit);
      toast.ok(`Rolling back ${app.name} to ${shortSha(d.commit)}`);
      void deps.refresh();
    } catch (e) {
      toast.bad((e as Error).message);
    }
  }

  async function deploy() {
    try {
      await coolify.deploy(app.uuid);
      toast.ok(`Deployment of ${app.name} queued`);
      void deps.refresh();
    } catch (e) {
      toast.bad((e as Error).message);
      throw e;
    }
  }

  return (
    <Modal
      open
      sheet
      onClose={onClose}
      title={app.name}
      description={<span className="font-mono">{repoLabel(app)}</span>}
      actions={
        <>
          <Button size="sm" icon={<LayoutDashboard />} onClick={() => local.openCoolify().catch((e) => toast.bad(e.message))}>
            Open in Coolify
          </Button>
          <OrbitButton size="sm" icon={<Rocket />} busyLabel="Deploying" doneLabel="Queued" onRun={deploy} testId="redeploy">
            Redeploy
          </OrbitButton>
        </>
      }
    >
      <div className="mb-5 grid grid-cols-4 gap-px overflow-hidden rounded-lg border border-line bg-line">
        <Meta label="Status">
          <span className="inline-flex items-center gap-1.5 font-sans">
            <StatusDot health={statusHealth(app.status)} />
            {appState(app.status)}
          </span>
        </Meta>
        <Meta label="Domain">
          {url ? (
            <button type="button" className="inline-flex max-w-full items-center gap-1 truncate hover:underline" onClick={() => void openExternal(url)}>
              <span className="truncate">{hostOf(url)}</span>
              <ExternalLink className="size-3 shrink-0 text-fg-dim" />
            </button>
          ) : (
            <span className="text-fg-dim">none</span>
          )}
        </Meta>
        <Meta label="Auto-deploy">{watch?.enabled ? `on push to ${watch.branch}` : "off"}</Meta>
        <Meta label="Updated">{ago(app.updated_at)}</Meta>
      </div>

      <Tabs
        className="mb-4 border-b border-line"
        label="app detail"
        value={tab}
        onChange={setTab}
        items={[
          { value: "deployments", label: "Deployments", icon: <History />, testId: "detail-tab-deployments" },
          { value: "previews", label: "Previews", icon: <GitPullRequest />, count: previews.data?.length, testId: "detail-tab-previews" },
          { value: "logs", label: "Logs", icon: <ScrollText />, testId: "detail-tab-logs" },
          { value: "env", label: "Environment", icon: <KeyRound />, testId: "detail-tab-env" },
          { value: "domains", label: "Domains", icon: <Globe />, testId: "detail-tab-domains" },
          { value: "autodeploy", label: "Auto-deploy", icon: <GitBranch />, testId: "detail-tab-autodeploy" },
          { value: "project", label: "vercel.json", icon: <FileJson />, testId: "detail-tab-project" },
        ]}
      />

      <AutoHeight>
        <motion.div key={tab} initial={{ opacity: 0, y: 4 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.18 }}>
          {tab === "deployments" && (
            <div data-testid="deployments">
              <ErrorNote error={deps.error} className="mb-3" />
              {deps.loading && !deps.data && <Loading rows={4} />}
              {deployments.length === 0 && !deps.loading && <EmptyState icon={<History />} title="No deployments yet" />}
              {deployments.length > 0 && <DeployTimeline
                  rows={deployments}
                  app={app}
                  onLogs={setLogFor}
                  onRollback={(d) => void rollback(d)}
                  instant={images.data?.tags.length ? images.data.tags : undefined}
                />}
            </div>
          )}

          {tab === "previews" && (
            <div className="space-y-2">
              <ErrorNote error={previews.error} />
              {(previews.data ?? []).length === 0 && (
                <EmptyState icon={<GitPullRequest />} title="No preview deployments">
                  Turn on “Preview Deployments” for this app in Coolify. Every pull request then gets its own URL, like{" "}
                  <InlineCode>pr-42-{hostOf(primaryUrl(app.fqdn) ?? "app.example.com")}</InlineCode>, which is removed when the PR closes.
                </EmptyState>
              )}
              {(previews.data ?? []).map((p) => {
                const purl = primaryUrl(p.fqdn);
                return (
                  <div key={p.uuid} className="flex items-center gap-3 rounded-lg border border-line bg-surface px-3.5 py-2.5">
                    <span className="flex items-center gap-1.5 text-[13px] text-fg">
                      <GitPullRequest className="size-3.5 text-fg-dim" />#{p.pull_request_id}
                    </span>
                    <StatusDot health={statusHealth(p.status)} />
                    {purl ? (
                      <button type="button" className="truncate font-mono text-[12px] hover:underline" onClick={() => void openExternal(purl)}>
                        {hostOf(purl)}
                      </button>
                    ) : (
                      <span className="text-[13px] text-fg-dim">{statusLabel(p.status)}</span>
                    )}
                    <span className="ml-auto text-xs text-fg-dim">{ago(p.created_at)}</span>
                    <IconButton
                      label="Delete preview"
                      onClick={() =>
                        coolify.deletePreview(app.uuid, p.pull_request_id).then(
                          () => {
                            toast.ok(`Preview #${p.pull_request_id} is being removed`);
                            void previews.refresh();
                          },
                          (e) => toast.bad(e.message),
                        )
                      }
                    >
                      <Trash2 />
                    </IconButton>
                  </div>
                );
              })}
            </div>
          )}

          {tab === "logs" && (
            <>
              <ErrorNote error={runtime.error} />
              {runtime.loading && !runtime.data ? <Loading rows={6} /> : <LogView>{runtime.data?.logs || "No output yet."}</LogView>}
            </>
          )}

          {tab === "env" && <EnvPanel app={app} />}

          {tab === "domains" && <AppDomains app={app} onChanged={() => void apps.refresh()} />}
          {tab === "project" && <ProjectPanel app={app} />}
          {tab === "autodeploy" && <AutoDeployPanel app={app} status={auto} refresh={refreshAuto} />}
        </motion.div>
      </AutoHeight>

      {logFor && <DeploymentLog deployment={logFor} onClose={() => setLogFor(null)} />}
    </Modal>
  );
}

function Meta({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="min-w-0 bg-surface px-3.5 py-2.5">
      <div className="text-[11px] text-fg-dim">{label}</div>
      <div className="mt-0.5 truncate font-mono text-[12px] text-fg">{children}</div>
    </div>
  );
}

function DeploymentLog({ deployment, onClose }: { deployment: CoolifyDeployment; onClose: () => void }) {
  const live = deployment.status === "in_progress" || deployment.status === "queued";
  const d = usePoll(() => coolify.deployment(deployment.deployment_uuid), live ? 2000 : 0, [deployment.deployment_uuid]);
  const lines = d.data ? parseDeploymentLogs(d.data) : [];
  const failed = deployment.status === "failed";
  return (
    <Modal
      open
      onClose={onClose}
      wide
      title={`Build log · ${shortSha(deployment.commit) || deployment.deployment_uuid}`}
      description={deployment.commit_message?.split("\n")[0]}
    >
      <ErrorNote error={d.error} />
      {d.loading && !d.data ? (
        <Loading rows={8} />
      ) : (
        <LogView testId="build-log" className={failed ? "max-h-[34vh]!" : undefined}>
          {lines.map((l, i) => (
            <div key={i} className={l.type === "stderr" ? "text-bad" : undefined}>
              {l.output}
            </div>
          ))}
          {lines.length === 0 && "No log output."}
        </LogView>
      )}
      {failed && lines.length > 0 && <ExplainFailure deployment={deployment} lines={lines} />}
    </Modal>
  );
}

type Explanation = { model?: string; text: string; tps?: number | null; error?: { title: string; detail: string } };

/** "Why did my deploy fail?": the server's own model (Gemma 4 E2B by default) reads the log. */
function ExplainFailure({ deployment, lines }: { deployment: CoolifyDeployment; lines: CoolifyDeploymentLog[] }) {
  const { settings } = useSession();
  const { go } = useNav();
  const [ex, setEx] = useState<Explanation>();
  const [streaming, setStreaming] = useState<{ cancel: () => void } | null>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  useEffect(() => () => streaming?.cancel(), [streaming]);
  useEffect(() => {
    // Newer browsers return a Promise here; an effect may only return a cleanup.
    if (ex) void panelRef.current?.scrollIntoView({ block: "nearest" });
  }, [ex]);

  async function explain() {
    setEx({ text: "" });
    const fail = (title: string, detail: string) => setEx((e) => ({ ...e, text: e?.text ?? "", error: { title, detail } }));
    let model: string | undefined;
    try {
      const s = await agent.llm();
      if (!s.installed) return fail("No local AI on this server yet", `Install it in AI models, then download ${EXPLAIN_MODEL}. Or on the server: sudo vercelify llm install`);
      if (!s.ready) return fail("Ollama isn't answering", "Start it from AI models, or on the server: sudo vercelify llm start");
      model = pickExplainModel(s.models.map((m) => m.name));
      if (!model) return fail("No model downloaded yet", `Download ${EXPLAIN_MODEL} in AI models, or on the server: sudo vercelify llm pull ${EXPLAIN_MODEL}`);
    } catch (e) {
      return fail("Can't reach the agent", toCockpitError(e).message);
    }
    const messages = explainMessages(lines, { app: deployment.application_name, commit: deployment.commit, commitMessage: deployment.commit_message });
    setEx({ model, text: "" });
    const handle = agent.llmChat({ model, messages, max_tokens: EXPLAIN_MAX_TOKENS, temperature: 0.2 }, (line) => {
      let v: { message?: { content?: string }; done?: boolean; error?: string; eval_count?: number; eval_duration?: number };
      try {
        v = JSON.parse(line);
      } catch {
        return;
      }
      if (v.error) {
        const missing = /not found/i.test(v.error);
        fail(v.error, missing ? `Download it in AI models, or on the server: sudo vercelify llm pull ${model}` : "Try again, or check AI models on the server.");
      }
      if (v.message?.content) setEx((e) => ({ ...e, text: (e?.text ?? "") + v.message!.content }));
      if (v.done) setEx((e) => ({ ...e, text: e?.text ?? "", tps: tokensPerSecond(v) }));
    });
    setStreaming(handle);
    handle.done.catch((e) => fail("The answer stopped", toCockpitError(e).message)).finally(() => setStreaming(null));
  }

  return (
    <div ref={panelRef} className="mt-4 rounded-lg border border-line bg-surface-2 p-3.5" data-testid="explain-panel">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 text-[13px] font-medium text-fg">
            <Sparkles className="size-3.5 text-accent" /> Why did this deploy fail?
          </div>
          <p className="text-[12px] text-fg-dim">
            An open model on {settings.hostname} reads the log. Nothing leaves your machines.
          </p>
        </div>
        {streaming ? (
          <Button size="sm" icon={<Square />} onClick={() => streaming.cancel()}>
            Stop
          </Button>
        ) : (
          <Button size="sm" variant="primary" icon={<Sparkles />} onClick={() => void explain()} data-testid="explain-failure">
            {ex ? "Explain again" : "Explain with AI"}
          </Button>
        )}
      </div>
      {ex && (
        <div className="mt-3 border-t border-line pt-3" data-testid="explain-answer">
          {ex.text ? (
            <ExplainText text={ex.text} />
          ) : (
            streaming && (
              <span className="inline-flex items-center gap-2 py-0.5 text-[13px]">
                <PixelWave size={3} pixel={3} gap={1.5} />
                <ShimmerText>{ex.model ? `${ex.model} is reading the log…` : "Looking for a model…"}</ShimmerText>
              </span>
            )
          )}
          {ex.error && (
            <Callout tone="bad" title={ex.error.title} className={ex.text ? "mt-3" : undefined}>
              {ex.error.detail}{" "}
              <button type="button" className="text-accent hover:underline" onClick={() => go("ai")}>
                Open AI models
              </button>
            </Callout>
          )}
          {ex.model && !ex.error && (ex.tps != null || !streaming) && ex.text && (
            <div className="mt-2 flex items-center gap-1 text-[11px] text-fg-dim" data-testid="explain-meta">
              <Zap className="size-3" /> {ex.model} on {settings.hostname}
              {ex.tps != null && ` · ${ex.tps.toFixed(1)} tokens/s`}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** The model's Cause / Fix / Check answer: bold labels and `code`, nothing else. */
function ExplainText({ text }: { text: string }) {
  return (
    <div className="selectable space-y-1.5 text-[13px] leading-relaxed text-fg">
      {text.split("\n").map((line, i) => {
        const m = /^\s*\*\*([^*]+):?\*\*:?\s*(.*)$/.exec(line);
        return (
          <p key={i} className={line.trim() ? undefined : "hidden"}>
            {m && <span className="font-medium text-accent">{m[1]!.replace(/:$/, "")}: </span>}
            {(m ? m[2]! : line).split(/(`[^`]+`)/).map((part, j) =>
              part.length > 2 && part.startsWith("`") && part.endsWith("`") ? <InlineCode key={j}>{part.slice(1, -1)}</InlineCode> : part,
            )}
          </p>
        );
      })}
    </div>
  );
}

const DB_KINDS: { value: DatabaseKind; label: string }[] = [
  { value: "postgresql", label: "PostgreSQL" },
  { value: "mysql", label: "MySQL" },
  { value: "mariadb", label: "MariaDB" },
  { value: "mongodb", label: "MongoDB" },
  { value: "redis", label: "Redis" },
  { value: "keydb", label: "KeyDB" },
  { value: "dragonfly", label: "Dragonfly" },
  { value: "clickhouse", label: "ClickHouse" },
];

function DatabasesCard({
  dbs,
  loading,
  error,
  refresh,
  creating,
  setCreating,
}: {
  dbs: CoolifyDatabase[] | undefined;
  loading: boolean;
  error?: { message: string };
  refresh: () => Promise<void>;
  creating: boolean;
  setCreating: (v: boolean) => void;
}) {
  const toast = useToast();
  const [kind, setKind] = useState<DatabaseKind>("postgresql");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

  async function create() {
    setBusy(true);
    try {
      const servers = await coolify.servers();
      const server = servers.find((s) => s.ip === "host.docker.internal") ?? servers[0];
      if (!server) throw new Error("Coolify has no servers yet");
      const target = await defaultTarget();
      const dbName = name.trim() || `${kind}-${Math.random().toString(36).slice(2, 6)}`;
      await coolify.createDatabase(kind, {
        ...target,
        server_uuid: server.uuid,
        name: dbName,
        instant_deploy: true,
      });
      toast.ok(`${dbName} is starting. You can move it to the HDD tier from Storage.`);
      setCreating(false);
      setName("");
      void refresh();
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card
      title="Databases"
      icon={<Database />}
      description="Managed by Coolify, with generated credentials"
      testId="databases-card"
      flush
      actions={
        <Button size="sm" icon={<Plus />} onClick={() => setCreating(true)} data-testid="new-database">
          New database
        </Button>
      }
    >
      <ErrorNote error={error} className="m-4" />
      {loading && !dbs ? (
        <div className="p-4">
          <Loading rows={2} />
        </div>
      ) : !dbs?.length ? (
        <EmptyState icon={<Database />} title="No databases">
          One click gives you PostgreSQL, MySQL, Redis, MongoDB and more, with backups managed by Coolify.
        </EmptyState>
      ) : (
        <div className="divide-y divide-line">
          {dbs.map((d) => (
            <DbRow key={d.uuid} db={d} onChanged={() => void refresh()} />
          ))}
        </div>
      )}
      <Modal open={creating} onClose={() => setCreating(false)} title="New database" description="Created in your first Coolify project on this host.">
        <form
          className="space-y-4"
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <div className="grid grid-cols-2 gap-3">
            <Field2 label="Engine">
              <Select value={kind} onChange={(e) => setKind(e.target.value as DatabaseKind)} data-testid="db-kind">
                {DB_KINDS.map((k) => (
                  <option key={k.value} value={k.value}>
                    {k.label}
                  </option>
                ))}
              </Select>
            </Field2>
            <Field2 label="Name">
              <TextInput placeholder={`${kind}-main`} value={name} onChange={(e) => setName(e.target.value)} data-testid="db-name" />
            </Field2>
          </div>
          <p className="text-xs text-fg-dim">A password is generated for you. The connection string shows up in the list once it's running.</p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setCreating(false)}>
              Cancel
            </Button>
            <Button type="submit" variant="primary" busy={busy} data-testid="db-create">
              Create & start
            </Button>
          </div>
        </form>
      </Modal>
    </Card>
  );
}

function Field2({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="flex flex-col gap-1.5 text-xs font-medium text-fg-muted">
      {label}
      {children}
    </label>
  );
}

function DbRow({ db, onChanged }: { db: CoolifyDatabase; onChanged: () => void }) {
  const toast = useToast();
  const running = db.status.startsWith("running");
  const act = (a: "start" | "stop" | "restart") =>
    coolify.dbAction(db.uuid, a).then(
      () => {
        toast.ok(`${db.name}: ${a} requested`);
        onChanged();
      },
      (e) => toast.bad(e.message),
    );
  return (
    <div className="flex items-center gap-3 px-4 py-3 transition-colors hover:bg-fg/[0.02]" data-testid={`db-${db.name}`}>
      <div className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-line bg-surface-2 text-fg-muted">
        <Database className="size-4" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 text-[13px] font-medium">
          {db.name}
          <span className="text-xs font-normal text-fg-dim">{db.database_type.replace("standalone-", "")}</span>
        </div>
        <div className="mt-0.5 flex items-center gap-1.5 truncate text-xs text-fg-dim">
          <StatusDot health={statusHealth(db.status)} />
          {appState(db.status)} · <span className="font-mono">{db.image}</span>
        </div>
      </div>
      {db.internal_db_url && <CopyButton text={db.internal_db_url} label="Copy URL" />}
      <IconButton label="Restart" onClick={() => void act("restart")}>
        <RotateCcw />
      </IconButton>
      {running ? (
        <IconButton label="Stop" onClick={() => void act("stop")}>
          <Square />
        </IconButton>
      ) : (
        <IconButton label="Start" onClick={() => void act("start")}>
          <Play />
        </IconButton>
      )}
    </div>
  );
}
