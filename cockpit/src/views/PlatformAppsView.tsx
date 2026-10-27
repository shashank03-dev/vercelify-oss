// Apps and databases on a server running Vercelify's own platform engine
// (`/v1/platform`, docs/design/SYSTEM-DESIGN.md): a list, a detail sheet per
// app, databases underneath.

import {
  Ban,
  Box,
  Clock,
  Copy,
  Database as DatabaseIcon,
  ExternalLink,
  FileJson,
  Eye,
  EyeOff,
  GitBranch,
  GitPullRequest,
  Globe,
  HardDrive,
  History,
  KeyRound,
  Link2,
  LayoutGrid,
  Lock,
  Moon,
  Play,
  Plus,
  RefreshCw,
  Rocket,
  RotateCcw,
  RotateCw,
  ScrollText,
  Server,
  Search,
  Settings2,
  Square,
  SquareTerminal,
  Trash2,
  Upload,
  Webhook,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { GithubAppModal, WebhookModal } from "../components/GitIntegrations";
import { TerminalView } from "../components/Terminal";
import { useToast } from "../components/Toast";
import {
  Button,
  Callout,
  Card,
  CopyButton,
  cx,
  EmptyState,
  ErrorNote,
  Field,
  IconButton,
  InlineCode,
  Loading,
  LogView,
  Modal,
  Segmented,
  Select,
  StatusDot,
  StatusText,
  Tabs,
  TextInput,
  Toggle,
} from "../components/ui";
import { ago, bytes, hostOf, shortSha, type Health } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { agent } from "../lib/api";
import { hasCap } from "../lib/caps";
import { useLive, useLiveTopic } from "../lib/live";
import { useNav } from "../lib/nav";
import { openExternal } from "../lib/open";
import {
  appState,
  deploymentHealth,
  parseDotenv,
  platformOn,
  slug,
  STATUS_LABEL,
  TRIGGER_LABEL,
  validName,
  validRepo,
  type LogLine,
  type PlatformClient,
  type RuntimeLine,
} from "../lib/platform";
import { useSession } from "../lib/session";
import type { App, AppKind, BuildPack, Database, DatabaseKind, Deployment, SleepTarget, StorageTier } from "../types";
import { ExplainFailure } from "../components/ExplainFailure";
import { MachineCard } from "../components/MachineCard";
import { ProjectPanel } from "../components/ProjectPanel";

const RUNNING: Deployment["status"][] = ["queued", "building", "deploying"];
const isRunning = (d: Deployment) => RUNNING.includes(d.status);

function useClient(): PlatformClient {
  const { settings } = useSession();
  return useMemo(() => platformOn(settings.agent_url), [settings.agent_url]);
}

/** Run `fn`, toast the outcome, return whether it worked. */
function useAct() {
  const toast = useToast();
  const [busy, setBusy] = useState<string>();
  async function act(key: string, fn: () => Promise<unknown>, ok?: string): Promise<boolean> {
    setBusy(key);
    try {
      await fn();
      if (ok) toast.ok(ok);
      return true;
    } catch (e) {
      toast.bad((e as Error).message);
      return false;
    } finally {
      setBusy(undefined);
    }
  }
  return { busy, act };
}

type Filter = "all" | "live" | "issues";
type DetailTab = "deployments" | "logs" | "terminal" | "env" | "domains" | "previews" | "project" | "settings";

export function PlatformApps() {
  const client = useClient();
  const apps = usePoll(() => client.apps(), 10_000, [client]);
  const dbs = usePoll(() => client.databases(), 15_000, [client]);
  useLiveTopic("apps", () => void apps.refresh());
  useLiveTopic("deployments", () => void apps.refresh());
  useLiveTopic("databases", () => void dbs.refresh());
  const [newApp, setNewApp] = useState(false);
  const [newDb, setNewDb] = useState(false);
  const [selected, setSelected] = useState<{ id: string; tab?: DetailTab } | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const { intent, clearIntent } = useNav();

  useEffect(() => {
    if (!intent?.newApp) return;
    setNewApp(true);
    clearIntent();
  }, [intent, clearIntent]);
  useEffect(() => {
    if (!intent?.app || !apps.data) return;
    if (apps.data.some((a) => a.id === intent.app)) setSelected({ id: intent.app });
    clearIntent();
  }, [intent, apps.data, clearIntent]);

  const list = apps.data ?? [];
  const health = (a: App) => appState(a).health;
  const live = list.filter((a) => health(a) === "ok").length;
  const issues = list.filter((a) => ["bad", "warn"].includes(health(a))).length;
  const visible = list.filter((a) => {
    if (filter === "live" && health(a) !== "ok") return false;
    if (filter === "issues" && !["bad", "warn"].includes(health(a))) return false;
    const q = query.trim().toLowerCase();
    return !q || [a.name, a.url, a.source.repository, a.source.image, a.source.branch].some((s) => s?.toLowerCase().includes(q));
  });
  const open = list.find((a) => a.id === selected?.id);

  return (
    <>
      <PageHeader
        onRefresh={() => void Promise.all([apps.refresh(), dbs.refresh()])}
        onNewApp={() => setNewApp(true)}
      />
      <ErrorNote error={apps.error} className="mb-6" />

      <div className="grid items-start gap-5 xl:grid-cols-[minmax(0,1fr)_280px]">
        <div className="min-w-0 space-y-5">
          <section className="overflow-hidden rounded-xl border border-line bg-surface" data-testid="platform-apps">
            <header className="flex flex-wrap items-center justify-between gap-3 border-b border-line px-3 py-2">
              <Segmented
                label="app status filter"
                value={filter}
                onChange={setFilter}
                options={[
                  { value: "all", label: <>All <Count n={list.length} /></> },
                  { value: "live", label: <>Live <Count n={live} /></> },
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
            ) : list.length === 0 ? (
              <EmptyState
                icon={<Rocket />}
                title="No apps yet"
                action={
                  <Button variant="primary" icon={<Plus />} onClick={() => setNewApp(true)}>
                    Deploy from Git
                  </Button>
                }
              >
                Paste a repository URL. Vercelify detects the framework, builds it on this machine, gives it a URL and deploys again on
                every push.
              </EmptyState>
            ) : visible.length === 0 ? (
              <div className="py-10 text-center text-[13px] text-fg-muted">No apps match.</div>
            ) : (
              <div className="divide-y divide-line" role="list" aria-label="Apps">
                {visible.map((a) => (
                  <AppRow key={a.id} app={a} client={client} onOpen={(tab) => setSelected({ id: a.id, tab })} onChanged={() => void apps.refresh()} />
                ))}
              </div>
            )}
          </section>

          <DatabasesCard client={client} dbs={dbs.data} apps={list} loading={dbs.loading} error={dbs.error} refresh={dbs.refresh} creating={newDb} setCreating={setNewDb} />
        </div>
        <aside className="space-y-5">
          <MachineCard />
        </aside>
      </div>

      {open && (
        <AppDetail
          key={`${open.id}|${selected?.tab ?? ""}`}
          app={open}
          client={client}
          initialTab={selected?.tab}
          onChanged={() => void apps.refresh()}
          onClose={() => setSelected(null)}
        />
      )}
      <NewAppModal
        open={newApp}
        client={client}
        onClose={() => setNewApp(false)}
        onCreated={(a) => {
          setNewApp(false);
          void apps.refresh();
          setSelected({ id: a.id });
        }}
      />
    </>
  );
}

function PageHeader({ onRefresh, onNewApp }: { onRefresh: () => void; onNewApp: () => void }) {
  return (
    <div className="mb-6 flex flex-wrap items-center justify-between gap-4">
      <div className="min-w-0">
        <h1 className="text-[20px] leading-7 font-semibold tracking-[-0.02em]">Apps</h1>
        <p className="mt-0.5 max-w-2xl text-[13px] text-fg-muted">
          Push to deploy, preview every pull request and roll back in one click. Built and served by this machine.
        </p>
      </div>
      <div className="flex items-center gap-2">
        <IconButton label="Refresh" onClick={onRefresh}>
          <RefreshCw />
        </IconButton>
        <Button variant="primary" icon={<Plus />} onClick={onNewApp} data-testid="new-app">
          New app
        </Button>
      </div>
    </div>
  );
}

function Count({ n }: { n: number }) {
  return <span className="text-[10px] text-fg-dim tabular-nums">{n}</span>;
}

function Monogram({ name }: { name: string }) {
  return (
    <div className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-line-strong bg-surface-2 text-[11px] font-semibold tracking-wide text-fg-muted uppercase">
      {name.replace(/[^a-z0-9]/gi, "").slice(0, 2) || "?"}
    </div>
  );
}

function sourceLabel(a: App): string {
  if (a.kind === "image") return a.source.image ?? "image";
  const repo = (a.source.repository ?? "").replace(/^https?:\/\/(www\.)?github\.com\//, "").replace(/\.git$/, "");
  return `${repo}${a.source.branch ? `@${a.source.branch}` : ""}`;
}

function AppRow({ app, client, onOpen, onChanged }: { app: App; client: PlatformClient; onOpen: (tab?: DetailTab) => void; onChanged: () => void }) {
  const { busy, act } = useAct();
  const state = appState(app);
  const deploying = !!app.last_status && RUNNING.includes(app.last_status);
  return (
    <div role="listitem" className="flex items-center gap-3 px-4 py-3 transition-colors hover:bg-fg/[0.02]" data-testid={`papp-${app.name}`}>
      <button type="button" className="flex min-w-0 flex-1 items-center gap-3 text-left" onClick={() => onOpen()}>
        <Monogram name={app.name} />
        <div className="min-w-0">
          <div className="truncate text-[13px] font-medium text-fg">{app.name}</div>
          <div className="truncate font-mono text-[11px] text-fg-dim">{sourceLabel(app)}</div>
        </div>
      </button>
      <StatusText health={state.health} label={state.label} className="hidden w-44 shrink-0 sm:flex" testId={`status-${app.name}`} />
      <div className="hidden w-56 min-w-0 shrink-0 lg:block">
        {app.url ? (
          <button type="button" className="flex max-w-full items-center gap-1 truncate text-[12px] text-fg-muted hover:text-fg" onClick={() => void openExternal(app.url!)}>
            <span className="truncate">{hostOf(app.url)}</span>
            <ExternalLink className="size-3 shrink-0" />
          </button>
        ) : (
          <span className="text-[12px] text-fg-dim">no domain</span>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <Button
          size="sm"
          icon={<Rocket />}
          busy={busy === "deploy"}
          disabled={deploying}
          data-testid={`deploy-${app.name}`}
          onClick={() =>
            void act("deploy", () => client.deploy(app.id), `Deploying ${app.name}`).then((ok) => {
              if (ok) {
                onChanged();
                onOpen("deployments");
              }
            })
          }
        >
          Deploy
        </Button>
        <IconButton label="Deployments" onClick={() => onOpen("deployments")}>
          <History />
        </IconButton>
      </div>
    </div>
  );
}

// ----- new app ---------------------------------------------------------------------

const PACKS: { value: BuildPack; label: string }[] = [
  { value: "auto", label: "Detect automatically" },
  { value: "dockerfile", label: "Dockerfile" },
  { value: "static", label: "Static site" },
  { value: "node", label: "Node.js" },
  { value: "bun", label: "Bun" },
  { value: "deno", label: "Deno" },
  { value: "python", label: "Python" },
  { value: "go", label: "Go" },
  { value: "rust", label: "Rust" },
  { value: "ruby", label: "Ruby" },
  { value: "php", label: "PHP" },
];

function NewAppModal({ open, client, onClose, onCreated }: { open: boolean; client: PlatformClient; onClose: () => void; onCreated: (a: App) => void }) {
  const [kind, setKind] = useState<AppKind | "service">("git");
  const templates = usePoll(() => (open && kind === "service" ? client.templates() : Promise.resolve(null)), 0, [open, kind, client]);
  const [template, setTemplate] = useState<string>();
  const [repo, setRepo] = useState("");
  const [branch, setBranch] = useState("main");
  const [image, setImage] = useState("");
  const [name, setName] = useState("");
  const [named, setNamed] = useState(false);
  const [pack, setPack] = useState<BuildPack>("auto");
  const [port, setPort] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const sources = usePoll(() => (open ? client.gitSources() : Promise.resolve([])), 0, [open, client]);
  const [ghApp, setGhApp] = useState(false);
  const [gitSource, setGitSource] = useState("");
  const toast = useToast();

  useEffect(() => {
    if (!open) return;
    setError(undefined);
  }, [open]);
  // The name follows the repository or image until it is edited.
  useEffect(() => {
    if (named) return;
    const from =
      kind === "service" ? template : kind === "image" ? image.split("/").pop()?.split(":")[0] : repo.replace(/\.git$/, "").split(/[/:]/).pop();
    setName(slug(from ?? ""));
  }, [repo, image, kind, named, template]);

  const sourceOk = kind === "service" ? !!template : kind === "image" ? image.trim().length > 0 : validRepo(repo);
  const portNum = port ? Number(port) : null;
  const portOk = portNum === null || (Number.isInteger(portNum) && portNum > 0 && portNum < 65536);
  const ok = sourceOk && validName(name) && portOk;

  async function create() {
    setBusy(true);
    setError(undefined);
    try {
      if (kind === "service") {
        const a = await client.createFromTemplate(template!, { name });
        setTemplate(undefined);
        setNamed(false);
        onCreated(a);
        return;
      }
      const a = await client.createApp({
        name,
        kind,
        source: {
          repository: kind === "git" ? repo.trim() : null,
          branch: kind === "git" ? branch.trim() || null : null,
          git_source_id: kind === "git" && gitSource ? gitSource : null,
          image: kind === "image" ? image.trim() : null,
          compose_file: null,
          compose_content: null,
          pinned_sha: null,
        },
        build:
          kind === "git"
            ? { pack, dockerfile: null, context: null, install_command: null, build_command: null, output_directory: null, watch_paths: [] }
            : undefined,
        runtime: portNum
          ? { port: portNum, command: null, replicas: 1, memory_mb: null, cpus: null, health: null, health_timeout_secs: 120, drain_secs: 10, service: null, failover: false }
          : undefined,
        domains: [],
      });
      // The agent only creates the app: its first deployment is asked here.
      await client.deploy(a.id).catch((e: Error) => toast.bad(`Created ${a.name}, but its deployment didn't start: ${e.message}`));
      setRepo("");
      setImage("");
      setNamed(false);
      onCreated(a);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open={open} onClose={onClose} title="New app" description="Built and run on this machine, with its own URL and HTTPS.">
      <div className="space-y-4 overflow-y-auto p-5">
        <Segmented
          label="app source"
          value={kind}
          onChange={setKind}
          options={[
            { value: "git", label: <><GitBranch /> Git repository</>, testId: "kind-git" },
            { value: "image", label: <><Box /> Docker image</>, testId: "kind-image" },
            { value: "service", label: <><LayoutGrid /> Service</>, testId: "kind-service" },
          ]}
        />
        {kind === "service" ? (
          <div className="space-y-2" data-testid="templates">
            {templates.loading && !templates.data && <Loading rows={3} />}
            <ErrorNote error={templates.error} />
            <div className="grid max-h-[40vh] grid-cols-1 gap-2 overflow-y-auto sm:grid-cols-2">
              {(templates.data ?? []).map((t) => (
                <button
                  key={t.id}
                  type="button"
                  data-testid={`template-${t.id}`}
                  aria-pressed={template === t.id}
                  onClick={() => setTemplate(t.id)}
                  className={cx(
                    "rounded-lg border p-3 text-left transition-colors",
                    template === t.id ? "border-accent/60 bg-accent/[0.06]" : "border-line hover:bg-fg/[0.03]",
                  )}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-[13px] font-medium text-fg">{t.name}</span>
                    <span className="text-[10px] text-fg-dim">{t.min_memory_mb >= 1024 ? `${t.min_memory_mb / 1024} GB` : `${t.min_memory_mb} MB`}</span>
                  </div>
                  <p className="mt-1 line-clamp-3 text-[11.5px] leading-snug text-fg-muted">{t.description}</p>
                </button>
              ))}
            </div>
            <p className="text-[11px] text-fg-dim">Passwords and keys are generated and kept as the app's secret variables.</p>
          </div>
        ) : kind === "git" ? (
          <>
            <Field label="Repository" hint="https://github.com/you/app, or git@github.com:you/app.git for a private one">
              <TextInput value={repo} onChange={(e) => setRepo(e.target.value)} placeholder="https://github.com/you/app" spellCheck={false} data-testid="new-repo" autoFocus />
            </Field>
            <div className="grid grid-cols-2 gap-3">
              <Field label="Branch">
                <TextInput value={branch} onChange={(e) => setBranch(e.target.value)} spellCheck={false} data-testid="new-branch" />
              </Field>
              <Field label="Build">
                <Select value={pack} onChange={(e) => setPack(e.target.value as BuildPack)} data-testid="new-pack">
                  {PACKS.map((p) => (
                    <option key={p.value} value={p.value}>
                      {p.label}
                    </option>
                  ))}
                </Select>
              </Field>
            </div>
            {!!sources.data?.length && (
              <Field label="Credentials" hint="For private repositories">
                <Select value={gitSource} onChange={(e) => setGitSource(e.target.value)} data-testid="new-git-source">
                  <option value="">None (public repository)</option>
                  {sources.data.map((g) => (
                    <option key={g.id} value={g.id}>
                      {g.name}
                    </option>
                  ))}
                </Select>
              </Field>
            )}
            {/github\.com/.test(repo) && !sources.data?.some((g) => g.auth === "github_app") && (
              <button type="button" className="text-xs text-accent hover:underline" onClick={() => setGhApp(true)} data-testid="github-app-open">
                Private GitHub repositories: connect a GitHub App
              </button>
            )}
            {ghApp && (
              <GithubAppModal
                client={client}
                onClose={() => setGhApp(false)}
                onCreated={(g) => {
                  setGitSource(g.id);
                  void sources.refresh();
                }}
              />
            )}
          </>
        ) : (
          <Field label="Image" hint="Pulled from Docker Hub, GHCR or any registry">
            <TextInput value={image} onChange={(e) => setImage(e.target.value)} placeholder="ghcr.io/umami-software/umami:postgresql-latest" spellCheck={false} data-testid="new-image" autoFocus />
          </Field>
        )}
        <div className="grid grid-cols-2 gap-3">
          <Field label="Name" hint="Lowercase letters, digits and dashes">
            <TextInput
              value={name}
              onChange={(e) => {
                setNamed(true);
                setName(e.target.value);
              }}
              spellCheck={false}
              data-testid="new-name"
            />
          </Field>
          <Field label="Port" hint={kind === "service" ? "Set by the service" : portOk ? "Leave empty to detect it" : "From 1 to 65535"}>
            <TextInput disabled={kind === "service"} value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))} placeholder="3000" inputMode="numeric" data-testid="new-port" />
          </Field>
        </div>
        {error && <Callout tone="bad">{error}</Callout>}
      </div>
      <div className="flex justify-end gap-2 border-t border-line px-5 py-3">
        <Button variant="ghost" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" icon={<Rocket />} disabled={!ok} busy={busy} onClick={() => void create()} data-testid="create-app">
          Create and deploy
        </Button>
      </div>
    </Modal>
  );
}

// ----- detail ----------------------------------------------------------------------

function AppDetail({
  app,
  client,
  initialTab,
  onChanged,
  onClose,
}: {
  app: App;
  client: PlatformClient;
  initialTab?: DetailTab;
  onChanged: () => void;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<DetailTab>(initialTab ?? "deployments");
  const deps = usePoll(() => client.deployments(app.id, 30), 5000, [app.id, client]);
  const previews = usePoll(() => client.previews(app.id), 20_000, [app.id, client]);
  useLiveTopic("deployments", () => void deps.refresh());
  const state = appState(app);
  const { busy, act } = useAct();
  const deploying = (deps.data ?? []).some(isRunning);

  return (
    <Modal
      open
      sheet
      onClose={onClose}
      title={
        <span className="flex items-center gap-2.5" data-testid="app-detail">
          {app.name}
          <StatusText health={state.health} label={state.label} className="text-xs font-normal" />
        </span>
      }
      description={
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="font-mono">{sourceLabel(app)}</span>
          {app.url && (
            <button type="button" className="inline-flex items-center gap-1 text-fg-muted hover:text-fg" onClick={() => void openExternal(app.url!)}>
              {hostOf(app.url)} <ExternalLink className="size-3" />
            </button>
          )}
        </span>
      }
      actions={
        <>
        {app.current_deployment_id && app.desired === "running" && (
          <Button
            size="sm"
            icon={<RotateCw />}
            busy={busy === "restart"}
            data-testid="app-restart"
            onClick={() => void act("restart", () => client.restart(app.id), `Restarted ${app.name}`)}
          >
            Restart
          </Button>
        )}
        <Button
          variant="primary"
          size="sm"
          icon={<Rocket />}
          busy={busy === "deploy"}
          disabled={deploying}
          data-testid="deploy-now"
          onClick={() =>
            void act("deploy", () => client.deploy(app.id), `Deploying ${app.name}`).then((ok) => {
              if (ok) {
                setTab("deployments");
                void deps.refresh();
                onChanged();
              }
            })
          }
        >
          {deploying ? "Deploying…" : "Deploy"}
        </Button>
        </>
      }
    >
      <div className="border-b border-line px-5">
        <Tabs
          label="app sections"
          value={tab}
          onChange={setTab}
          items={[
            { value: "deployments", label: "Deployments", icon: <History />, testId: "tab-deployments" },
            { value: "logs", label: "Logs", icon: <ScrollText />, testId: "tab-logs" },
            { value: "terminal", label: "Terminal", icon: <SquareTerminal />, testId: "tab-terminal" },
            { value: "env", label: "Variables", icon: <KeyRound />, testId: "tab-env" },
            { value: "domains", label: "Domains", icon: <Globe />, testId: "tab-domains", count: app.domains.length },
            { value: "previews", label: "Previews", icon: <GitPullRequest />, testId: "tab-previews", count: previews.data?.filter((p) => !p.closed_at).length },
            { value: "project", label: "vercel.json", icon: <FileJson />, testId: "tab-project" },
            { value: "settings", label: "Settings", icon: <Settings2 />, testId: "tab-settings" },
          ]}
        />
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto p-5">
        {tab === "deployments" && <Deployments app={app} client={client} deps={deps.data} loading={deps.loading} error={deps.error} refresh={deps.refresh} onChanged={onChanged} />}
        {tab === "logs" && <RuntimeLogs app={app} client={client} />}
        {tab === "terminal" &&
          (app.current_deployment_id && app.desired !== "stopped" ? (
            <TerminalView client={client} open={(cols, rows) => client.openTerminal(app.id, { cols, rows })} />
          ) : (
            <Callout tone="idle">The terminal opens a shell in the running app: deploy or start {app.name} first.</Callout>
          ))}
        {tab === "env" && <EnvTab app={app} client={client} />}
        {tab === "domains" && <DomainsTab app={app} client={client} onChanged={onChanged} />}
        {tab === "previews" && <PreviewsTab app={app} client={client} previews={previews} onChanged={onChanged} />}
        {tab === "project" && <ProjectPanel app={app} />}
        {tab === "settings" && <SettingsTab app={app} client={client} onChanged={onChanged} onDeleted={onClose} />}
      </div>
    </Modal>
  );
}

function Deployments({
  app,
  client,
  deps,
  loading,
  error,
  refresh,
  onChanged,
}: {
  app: App;
  client: PlatformClient;
  deps: Deployment[] | undefined;
  loading: boolean;
  error?: { message: string };
  refresh: () => Promise<void>;
  onChanged: () => void;
}) {
  const { busy, act } = useAct();
  const list = deps ?? [];
  // The log shown: the one picked, else the newest deployment's.
  const [picked, setPicked] = useState<string>();
  const shown = list.find((d) => d.id === picked) ?? list[0];

  if (loading && !deps) return <Loading rows={5} />;
  return (
    <div className="space-y-4">
      <ErrorNote error={error} />
      {app.source.pinned_sha && (
        <Callout tone="idle" title="Pinned by a rollback">
          Pushes to {app.source.branch ?? "the branch"} don't deploy until the next deploy from here.
        </Callout>
      )}
      {list.length === 0 ? (
        <EmptyState icon={<Rocket />} title="Not deployed yet">
          Press Deploy to build {app.name} from {app.kind === "image" ? "its image" : `the head of ${app.source.branch ?? "its branch"}`}.
        </EmptyState>
      ) : (
        <>
          {shown && <DeploymentLog key={shown.id} client={client} d={shown} app={app} />}
          <div className="divide-y divide-line overflow-hidden rounded-xl border border-line" data-testid="deployments">
            {list.map((d) => {
              const canRollback = !d.current && (d.status === "healthy" || d.status === "superseded") && !!d.image && !d.pr_number;
              return (
                <div
                  key={d.id}
                  data-testid={`deployment-${d.number}`}
                  className={cx("flex items-center gap-3 px-3.5 py-2.5", d.id === shown?.id && "bg-fg/[0.03]")}
                >
                  <button type="button" className="flex min-w-0 flex-1 items-center gap-3 text-left" onClick={() => setPicked(d.id)}>
                    <span className="w-8 shrink-0 font-mono text-[12px] text-fg-dim">#{d.number}</span>
                    <StatusText health={deploymentHealth(d)} label={d.current ? "Live" : STATUS_LABEL[d.status]} className="w-28 shrink-0" />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] text-fg">{d.commit_message || (app.kind === "image" ? d.image : "Deploy")}</span>
                      <span className="block truncate text-[11px] text-fg-dim">
                        {[d.commit_sha && shortSha(d.commit_sha), d.branch, TRIGGER_LABEL[d.trigger], ago(d.created_at)].filter(Boolean).join(" · ")}
                      </span>
                    </span>
                  </button>
                  {isRunning(d) && d.status !== "deploying" && (
                    <IconButton label="Cancel" onClick={() => void act(`cancel-${d.id}`, () => client.cancel(d.id), "Cancelled").then(() => void refresh())}>
                      <Ban />
                    </IconButton>
                  )}
                  {canRollback && (
                    <Button
                      size="sm"
                      icon={<RotateCcw />}
                      busy={busy === `rb-${d.id}`}
                      data-testid={`rollback-${d.number}`}
                      onClick={() =>
                        void act(`rb-${d.id}`, () => client.rollback(app.id, d.id), `Rolling back to #${d.number}`).then((ok) => {
                          if (ok) {
                            setPicked(undefined);
                            void refresh();
                            onChanged();
                          }
                        })
                      }
                    >
                      Roll back
                    </Button>
                  )}
                </div>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}

/** A deployment's log: streamed while it runs, loaded once afterwards. */
function DeploymentLog({ client, d, app }: { client: PlatformClient; d: Deployment; app: App }) {
  const [lines, setLines] = useState<LogLine[]>([]);
  const [error, setError] = useState<string>();
  const running = isRunning(d);
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  useEffect(() => {
    setError(undefined);
    if (!running) {
      let alive = true;
      client.logs(d.id).then(
        (l) => alive && setLines(l),
        (e: Error) => alive && setError(e.message),
      );
      return () => void (alive = false);
    }
    setLines([]);
    const s = client.followLogs(d.id, (l) => setLines((x) => (x.some((y) => y.seq === l.seq) ? x : [...x, l])));
    s.done.catch((e: Error) => setError(e.message));
    return () => s.cancel();
  }, [client, d.id, running]);

  // Follow the end while the reader is at the end.
  useEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  const explainable = d.status === "failed" && lines.length > 0;
  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2 text-xs text-fg-muted">
        <span className="flex items-center gap-1.5">
          <ScrollText className="size-3.5" />
          Log of #{d.number}
          {running && <span className="text-fg-dim">· live</span>}
        </span>
        {d.error && <span className="truncate text-bad">{d.error}</span>}
      </div>
      {error && <Callout tone="bad">{error}</Callout>}
      <div
        ref={box}
        className="max-h-[38vh] overflow-auto rounded-lg"
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        <LogView testId="deploy-log" className="max-h-none!">
          {lines.map((l) => (
            <div key={l.seq} className={l.stream === "error" || l.stream === "stderr" ? "text-bad" : l.stream === "info" ? "text-fg" : undefined}>
              {l.text}
            </div>
          ))}
          {lines.length === 0 && (running ? "Waiting for the builder…" : "No log output.")}
        </LogView>
      </div>
      {explainable && (
        <ExplainFailure
          deployment={{ application_name: app.name, commit: d.commit_sha || null, commit_message: d.commit_message || null }}
          lines={lines.map((l) => ({ output: l.text, type: l.stream === "error" ? "stderr" : l.stream, timestamp: l.at, hidden: false }))}
        />
      )}
    </div>
  );
}

/** The app's own output, live: what `console.log` and crashes print. */
function RuntimeLogs({ app, client }: { app: App; client: PlatformClient }) {
  const [lines, setLines] = useState<RuntimeLine[]>([]);
  const [error, setError] = useState<string>();
  const [follow, setFollow] = useState(true);
  const [query, setQuery] = useState("");
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const deployed = !!app.current_deployment_id;

  useEffect(() => {
    setError(undefined);
    setLines([]);
    if (!deployed) return;
    if (!follow) {
      let alive = true;
      client.runtimeLogs(app.id, 500).then(
        (l) => alive && setLines(l),
        (e: Error) => alive && setError(e.message),
      );
      return () => void (alive = false);
    }
    // Lines arrive one by one; render them in batches.
    let queue: RuntimeLine[] = [];
    const tick = window.setInterval(() => {
      if (!queue.length) return;
      const add = queue;
      queue = [];
      setLines((x) => [...x, ...add].slice(-2000));
    }, 250);
    const s = client.followRuntimeLogs(app.id, (l) => queue.push(l), 500);
    s.done.catch((e: Error) => setError(e.message));
    return () => {
      window.clearInterval(tick);
      s.cancel();
    };
  }, [client, app.id, follow, deployed]);

  useEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  if (!deployed) return <Callout tone="idle">{app.name} isn't running yet: its output shows here once it is deployed.</Callout>;
  const q = query.trim().toLowerCase();
  const shown = q ? lines.filter((l) => l.text.toLowerCase().includes(q)) : lines;
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Segmented
          label="log mode"
          value={follow ? "live" : "last"}
          onChange={(v) => setFollow(v === "live")}
          options={[
            { value: "live", label: "Live", testId: "logs-live" },
            { value: "last", label: "Last 500 lines", testId: "logs-last" },
          ]}
        />
        <div className="relative">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-fg-dim" />
          <TextInput placeholder="Filter lines…" value={query} onChange={(e) => setQuery(e.target.value)} className="h-8 w-56 pl-8" spellCheck={false} data-testid="logs-filter" />
        </div>
      </div>
      {error && <Callout tone="bad">{error}</Callout>}
      <div
        ref={box}
        className="max-h-[56vh] overflow-auto rounded-lg"
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        <LogView testId="runtime-log" className="max-h-none!">
          {shown.map((l, i) => (
            <div key={i} className={l.stream === "stderr" ? "text-bad" : undefined}>
              {l.at && <span className="mr-2 text-fg-dim select-none">{l.at.slice(11, 19)}</span>}
              {l.text}
            </div>
          ))}
          {shown.length === 0 && (q ? "No lines match." : follow ? "Waiting for output…" : "No output.")}
        </LogView>
      </div>
    </div>
  );
}

// ----- variables -------------------------------------------------------------------

function EnvTab({ app, client }: { app: App; client: PlatformClient }) {
  const [reveal, setReveal] = useState(false);
  const vars = usePoll(() => client.env(app.id, reveal), 0, [app.id, client, reveal]);
  const { busy, act } = useAct();
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");
  const [secret, setSecret] = useState(true);
  const [build, setBuild] = useState(false);
  const [importing, setImporting] = useState(false);
  const [dotenv, setDotenv] = useState("");
  const keyOk = /^[A-Za-z_][A-Za-z0-9_]*$/.test(key);
  const parsed = parseDotenv(dotenv);

  async function add() {
    if (await act("add", () => client.putEnv(app.id, { vars: [{ key, value, secret, build }], replace: false }), `Saved ${key}. Deploy to use it.`)) {
      setKey("");
      setValue("");
      void vars.refresh();
    }
  }

  return (
    <div className="space-y-4">
      <Card
        flush
        title="Environment variables"
        description="Apps read them at runtime; those marked Build are also available while building. References like ${{db.name.url}} are filled in on deploy."
        actions={
          <>
            <IconButton label={reveal ? "Hide values" : "Show values"} onClick={() => setReveal(!reveal)}>
              {reveal ? <EyeOff /> : <Eye />}
            </IconButton>
            <Button size="sm" icon={<Upload />} onClick={() => setImporting(true)} data-testid="env-import">
              Import .env
            </Button>
          </>
        }
        testId="env-card"
      >
        <ErrorNote error={vars.error} className="m-4" />
        {vars.loading && !vars.data ? (
          <div className="p-4">
            <Loading rows={3} />
          </div>
        ) : !vars.data?.length ? (
          <p className="px-4 py-6 text-center text-[13px] text-fg-muted">No variables yet.</p>
        ) : (
          <div className="divide-y divide-line">
            {vars.data.map((v) => (
              <div key={`${v.key}${v.preview_only}`} className="flex items-center gap-3 px-4 py-2" data-testid={`env-${v.key}`}>
                <span className="w-56 shrink-0 truncate font-mono text-[12px] text-fg">{v.key}</span>
                <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-fg-muted">{v.value ?? "••••••••"}</span>
                <span className="flex shrink-0 gap-1">
                  {v.build && <Tag>Build</Tag>}
                  {v.preview_only && <Tag>Previews</Tag>}
                  {v.scope !== "app" && <Tag>{v.scope}</Tag>}
                </span>
                {v.scope === "app" && (
                  <IconButton
                    label={`Delete ${v.key}`}
                    onClick={() => void act(`del-${v.key}`, () => client.deleteEnv(app.id, v.key, v.preview_only), `Deleted ${v.key}`).then(() => void vars.refresh())}
                  >
                    <Trash2 />
                  </IconButton>
                )}
              </div>
            ))}
          </div>
        )}
        <div className="flex flex-wrap items-end gap-2 border-t border-line p-4">
          <div className="w-56">
            <Field label="Key">
              <TextInput value={key} onChange={(e) => setKey(e.target.value.toUpperCase().replace(/[^A-Z0-9_]/g, "_"))} placeholder="API_URL" spellCheck={false} data-testid="env-key" />
            </Field>
          </div>
          <div className="min-w-48 flex-1">
            <Field label="Value">
              <TextInput value={value} onChange={(e) => setValue(e.target.value)} spellCheck={false} data-testid="env-value" />
            </Field>
          </div>
          <label className="flex h-8 items-center gap-1.5 text-xs text-fg-muted">
            <input type="checkbox" checked={secret} onChange={(e) => setSecret(e.target.checked)} /> Secret
          </label>
          <label className="flex h-8 items-center gap-1.5 text-xs text-fg-muted">
            <input type="checkbox" checked={build} onChange={(e) => setBuild(e.target.checked)} /> Build
          </label>
          <Button variant="primary" icon={<Plus />} disabled={!keyOk} busy={busy === "add"} onClick={() => void add()} data-testid="env-add">
            Add
          </Button>
        </div>
      </Card>
      <Modal open={importing} onClose={() => setImporting(false)} title="Import a .env file" description="Existing keys are overwritten; others stay.">
        <div className="space-y-3 p-5">
          <textarea
            value={dotenv}
            onChange={(e) => setDotenv(e.target.value)}
            rows={10}
            spellCheck={false}
            placeholder={"DATABASE_URL=postgres://…\nAPI_KEY=…"}
            className="w-full rounded-lg border border-line-strong bg-bg p-2.5 font-mono text-[12px] text-fg outline-none focus:border-accent/60"
            data-testid="env-import-text"
          />
          <p className="text-xs text-fg-dim">{parsed.length} variable{parsed.length === 1 ? "" : "s"} found</p>
        </div>
        <div className="flex justify-end gap-2 border-t border-line px-5 py-3">
          <Button variant="ghost" onClick={() => setImporting(false)}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!parsed.length}
            busy={busy === "import"}
            data-testid="env-import-run"
            onClick={() =>
              void act("import", () => client.importEnv(app.id, dotenv), `Imported ${parsed.length} variables. Deploy to use them.`).then((ok) => {
                if (ok) {
                  setImporting(false);
                  setDotenv("");
                  void vars.refresh();
                }
              })
            }
          >
            Import {parsed.length || ""}
          </Button>
        </div>
      </Modal>
    </div>
  );
}

function Tag({ children }: { children: ReactNode }) {
  return <span className="rounded-md border border-line px-1.5 py-px text-[10px] text-fg-muted">{children}</span>;
}

// ----- domains ---------------------------------------------------------------------

const CERT: Record<string, { health: "ok" | "warn" | "bad" | "idle"; label: string }> = {
  issued: { health: "ok", label: "HTTPS" },
  pending: { health: "warn", label: "Getting a certificate" },
  failed: { health: "bad", label: "Certificate failed" },
  edge: { health: "ok", label: "HTTPS at Cloudflare" },
  none: { health: "idle", label: "HTTP only" },
};

function DomainsTab({ app, client, onChanged }: { app: App; client: PlatformClient; onChanged: () => void }) {
  const { busy, act } = useAct();
  const [host, setHost] = useState("");
  const setting = usePoll(() => client.domain(), 0, [client]);
  const hostOk = /^(\*\.)?([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}$/.test(host.trim().toLowerCase());
  // Pending certificates turn into issued ones within a minute or so.
  const pending = app.domains.some((d) => d.cert_status === "pending");
  useEffect(() => {
    if (!pending) return;
    const id = window.setInterval(onChanged, 4000);
    return () => window.clearInterval(id);
  }, [pending, onChanged]);

  return (
    <div className="space-y-4">
      <Card flush title="Domains" description="Point a DNS A record at this server, add the name here, and HTTPS follows." testId="domains-card">
        {app.domains.length === 0 ? (
          <p className="px-4 py-6 text-center text-[13px] text-fg-muted">No domains. Add one below, or set a wildcard domain in Settings for automatic ones.</p>
        ) : (
          <div className="divide-y divide-line">
            {app.domains.map((d) => {
              const c = CERT[d.cert_status] ?? CERT.none!;
              return (
                <div key={d.id} className="flex items-center gap-3 px-4 py-2.5" data-testid={`domain-${d.host}`}>
                  <span className="min-w-0 flex-1 truncate text-[13px] text-fg">
                    {d.host}
                    {d.is_primary && <span className="ml-2 text-[11px] text-fg-dim">primary</span>}
                    {d.redirect_to && <span className="ml-2 text-[11px] text-fg-dim">→ {d.redirect_to}</span>}
                  </span>
                  <StatusText health={d.https ? c.health : "idle"} label={d.https ? c.label : "HTTP only"} className="w-44 shrink-0" />
                  <IconButton label={`Open ${d.host}`} onClick={() => void openExternal(`${d.https ? "https" : "http"}://${d.host}`)}>
                    <ExternalLink />
                  </IconButton>
                  <IconButton
                    label={`Remove ${d.host}`}
                    onClick={() => void act(`rm-${d.host}`, () => client.removeDomain(app.id, d.host), `Removed ${d.host}`).then(onChanged)}
                  >
                    <Trash2 />
                  </IconButton>
                </div>
              );
            })}
          </div>
        )}
        <form
          className="flex items-end gap-2 border-t border-line p-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (!hostOk || busy) return;
            void act("add", () => client.addDomain(app.id, { host: host.trim().toLowerCase(), https: setting.data?.https ?? true }), `Added ${host.trim()}`).then((ok) => {
              if (ok) {
                setHost("");
                onChanged();
              }
            });
          }}
        >
          <div className="flex-1">
            <Field label="Add a domain" hint={setting.data?.wildcard ? `Apps also get <name>.${setting.data.wildcard} automatically.` : undefined}>
              <TextInput value={host} onChange={(e) => setHost(e.target.value)} placeholder="www.example.com" spellCheck={false} data-testid="domain-input" />
            </Field>
          </div>
          <Button type="submit" variant="primary" icon={<Plus />} disabled={!hostOk} busy={busy === "add"} data-testid="domain-add">
            Add
          </Button>
        </form>
      </Card>
    </div>
  );
}

// ----- previews --------------------------------------------------------------------

function PreviewsTab({
  app,
  client,
  previews,
  onChanged,
}: {
  app: App;
  client: PlatformClient;
  previews: ReturnType<typeof usePoll<Awaited<ReturnType<PlatformClient["previews"]>>>>;
  onChanged: () => void;
}) {
  const { busy, act } = useAct();
  const open = (previews.data ?? []).filter((p) => !p.closed_at);
  if (app.kind !== "git") {
    return <Callout tone="idle">Previews build pull requests, so they need an app deployed from a Git repository.</Callout>;
  }
  return (
    <div className="space-y-4">
      <Card
        title="Pull request previews"
        description={`Each open pull request gets its own URL, rebuilt on every push. Up to ${app.previews.max} run at once.`}
        actions={
          <Button
            size="sm"
            busy={busy === "toggle"}
            data-testid="previews-toggle"
            onClick={() =>
              void act("toggle", () => client.patchApp(app.id, { previews: { ...app.previews, enabled: !app.previews.enabled } }), app.previews.enabled ? "Previews off" : "Previews on").then(
                onChanged,
              )
            }
          >
            {app.previews.enabled ? "Turn off" : "Turn on"}
          </Button>
        }
        flush
        testId="previews-card"
      >
        {!app.previews.enabled ? (
          <p className="px-4 py-6 text-center text-[13px] text-fg-muted">Previews are off for {app.name}.</p>
        ) : open.length === 0 ? (
          <p className="px-4 py-6 text-center text-[13px] text-fg-muted">No open pull requests with a preview.</p>
        ) : (
          <div className="divide-y divide-line">
            {open.map((p) => (
              <div key={p.id} className="flex items-center gap-3 px-4 py-2.5" data-testid={`preview-${p.pr_number}`}>
                <span className="w-12 shrink-0 font-mono text-[12px] text-fg-dim">#{p.pr_number}</span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] text-fg">{p.title || `Pull request ${p.pr_number}`}</span>
                  <span className="block truncate text-[11px] text-fg-dim">
                    {shortSha(p.head_sha)} · {ago(p.created_at)}
                  </span>
                </span>
                <StatusText health={p.asleep ? "idle" : p.status === "healthy" ? "ok" : p.status === "failed" ? "bad" : "warn"} label={p.asleep ? "Asleep" : STATUS_LABEL[p.status as Deployment["status"]] ?? p.status} className="w-28 shrink-0" />
                {p.url && (
                  <IconButton label="Open preview" onClick={() => void openExternal(p.url)}>
                    <ExternalLink />
                  </IconButton>
                )}
                <IconButton
                  label={`Remove preview #${p.pr_number}`}
                  onClick={() => void act(`rm-${p.pr_number}`, () => client.removePreview(app.id, p.pr_number), `Removed the preview of #${p.pr_number}`).then(() => void previews.refresh())}
                >
                  <Trash2 />
                </IconButton>
              </div>
            ))}
          </div>
        )}
      </Card>
    </div>
  );
}

// ----- settings --------------------------------------------------------------------

const IDLE_OPTIONS = [15, 30, 60, 120, 240, 720];

function sleepLabel(t: SleepTarget | undefined): { health: Health; label: string } {
  if (!t) return { health: "idle", label: "not reached yet" };
  if (t.state === "asleep") return { health: "idle", label: `asleep since ${ago(t.asleep_since)}` };
  if (t.state === "waking") return { health: "warn", label: "waking up" };
  return { health: "ok", label: t.last_request_at ? `awake · last visit ${ago(t.last_request_at)}` : "awake" };
}

/** Stop the app when nobody visits it; the next visit wakes it. */
function SleepCard({ app }: { app: App }) {
  const toast = useToast();
  const { health } = useLive();
  const withSleep = hasCap(health.data, "sleep");
  const sleep = usePoll(() => (withSleep ? agent.sleep() : Promise.resolve(null)), withSleep ? 60_000 : 0, [withSleep]);
  useLiveTopic("sleep", () => void sleep.refresh());
  const [busy, setBusy] = useState(false);
  if (!withSleep) return null;
  const setting = sleep.data?.settings.find((s) => s.app_uuid === app.id);
  const st = sleepLabel(sleep.data?.targets.find((t) => t.key === app.id));

  async function save(enabled: boolean, idle = setting?.idle_minutes ?? 30, previews = setting?.previews ?? false) {
    setBusy(true);
    try {
      await agent.setSleep(app.id, { enabled, idle_minutes: idle, previews });
      await sleep.refresh();
      toast.ok(enabled ? `${app.name} sleeps after ${idle} idle minutes` : `${app.name} stays awake`);
    } catch (e) {
      toast.bad((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card icon={<Moon />} title="Sleep when idle" testId="app-sleep">
      {sleep.loading && !sleep.data ? (
        <Loading rows={2} />
      ) : (
        <div className="space-y-3">
          <Toggle
            checked={!!setting?.enabled}
            disabled={busy}
            onChange={(v) => void save(v)}
            label="Stop this app when nobody visits it"
            hint="Gives its memory back to the server. The next visit starts it again; that visitor sees “waking up” for a second or two."
            testId="sleep-toggle"
          />
          {setting?.enabled && (
            <div className="flex flex-wrap items-end gap-4 pl-6">
              <Field label="After">
                <Select value={String(setting.idle_minutes)} disabled={busy} onChange={(e) => void save(true, Number(e.target.value))} data-testid="sleep-minutes">
                  {[...new Set([...IDLE_OPTIONS, setting.idle_minutes])].sort((a, b) => a - b).map((m) => (
                    <option key={m} value={m}>
                      {m < 60 ? `${m} minutes` : `${m / 60} hour${m === 60 ? "" : "s"}`} without visits
                    </option>
                  ))}
                </Select>
              </Field>
              <Toggle
                checked={setting.previews}
                disabled={busy}
                onChange={(v) => void save(true, setting.idle_minutes, v)}
                label="Pull request previews too"
                hint="They are rarely visited after review."
              />
              <StatusText health={st.health} label={st.label} className="text-xs text-fg-muted" testId="sleep-state" />
            </div>
          )}
          {sleep.data?.error && setting?.enabled && <Callout tone="warn">{sleep.data.error}</Callout>}
        </div>
      )}
    </Card>
  );
}

function SettingsTab({ app, client, onChanged, onDeleted }: { app: App; client: PlatformClient; onChanged: () => void; onDeleted: () => void }) {
  const { settings } = useSession();
  const { busy, act } = useAct();
  const git = usePoll(() => (app.kind === "git" ? client.git(app.id) : Promise.resolve(null)), 0, [app.id, client]);
  const hooks = usePoll(() => client.hooks(app.id), 0, [app.id, client]);
  const tasks = usePoll(() => client.tasks(app.id), 0, [app.id, client]);
  const sources = usePoll(() => (app.source.git_source_id ? client.gitSources() : Promise.resolve([])), 0, [app.id, client]);
  const source = sources.data?.find((g) => g.id === app.source.git_source_id);
  const [hook, setHook] = useState(false);
  const [newHook, setNewHook] = useState<string>();
  const [user, setUser] = useState("");
  const [password, setPassword] = useState("");
  const [task, setTask] = useState({ name: "", schedule: "0 3 * * *", command: "" });
  const [confirm, setConfirm] = useState("");
  const stopped = app.desired === "stopped";

  return (
    <div className="space-y-4">
      {app.kind === "git" && (
        <Card
          icon={<GitBranch />}
          title="Deploy on push"
          description={`New commits on ${app.source.branch ?? "the default branch"} are built and deployed.`}
          actions={
            <Button
              size="sm"
              busy={busy === "git"}
              data-testid="autodeploy-toggle"
              onClick={() => void act("git", () => client.setGit(app.id, !git.data?.enabled), git.data?.enabled ? "Auto-deploy off" : "Auto-deploy on").then(() => void git.refresh())}
            >
              {git.data?.enabled ? "Turn off" : "Turn on"}
            </Button>
          }
        >
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-fg-muted">
            <StatusText
              health={git.data?.enabled ? (git.data.last_error ? "warn" : git.data.waiting ? "idle" : "ok") : "idle"}
              label={git.data?.enabled ? (git.data.waiting ? "Waiting" : "Watching") : "Off"}
              testId="autodeploy-state"
            />
            {git.data?.last_head && <span className="font-mono">head {shortSha(git.data.last_head)}</span>}
            {git.data?.last_checked_at && <span>checked {ago(git.data.last_checked_at)}</span>}
            {git.data?.last_error && <span className="text-warn">{git.data.last_error}</span>}
            {git.data?.waiting && <span data-testid="autodeploy-waiting">{git.data.waiting}</span>}
            <Button size="sm" variant="ghost" icon={<RefreshCw />} busy={busy === "check"} onClick={() => void act("check", () => client.checkGit(app.id), "Checked").then(() => void git.refresh())}>
              Check now
            </Button>
            {source && (
              <Button size="sm" variant="ghost" icon={<Webhook />} onClick={() => setHook(true)} data-testid="webhook-open">
                {source.webhook ? "Webhook on" : "Faster: a webhook"}
              </Button>
            )}
          </div>
          {hook && source && <WebhookModal client={client} source={source} onClose={() => setHook(false)} />}
        </Card>
      )}

      <SleepCard app={app} />

      <Card icon={<Lock />} title="Password protection" description="Visitors need a user name and password (HTTP Basic Auth). Leave both empty to remove it.">
        <div className="flex flex-wrap items-end gap-2">
          <div className="w-44">
            <Field label="User">
              <TextInput value={user} onChange={(e) => setUser(e.target.value)} autoComplete="off" data-testid="basic-user" />
            </Field>
          </div>
          <div className="w-56">
            <Field label="Password">
              <TextInput type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" data-testid="basic-password" />
            </Field>
          </div>
          <Button
            busy={busy === "basic"}
            disabled={!!user !== !!password}
            data-testid="basic-save"
            onClick={() =>
              void act("basic", () => client.setBasicAuth(app.id, user, password), user ? "Password protection on" : "Password protection removed").then((ok) => {
                if (ok) setPassword("");
              })
            }
          >
            Save
          </Button>
        </div>
      </Card>

      <Card
        flush
        icon={<Webhook />}
        title="Deploy hooks"
        description="A secret URL that deploys this app when something POSTs to it (a CMS, a cron job, CI)."
        actions={
          <Button
            size="sm"
            icon={<Plus />}
            busy={busy === "hook"}
            data-testid="hook-create"
            onClick={() =>
              void act("hook", async () => {
                const h = await client.createHook(app.id);
                setNewHook(`${settings.agent_url.replace(/\/$/, "")}/v1/hooks/deploy/${h.secret}`);
                await hooks.refresh();
              })
            }
          >
            New hook
          </Button>
        }
      >
        {newHook && (
          <div className="space-y-2 border-b border-line p-4">
            <p className="text-xs text-fg-muted">Copy it now: it isn't shown again.</p>
            <div className="flex items-center gap-2">
              <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-bg px-2 py-1.5 font-mono text-[12px]" data-testid="hook-url">
                {newHook}
              </code>
              <CopyButton text={newHook} />
            </div>
          </div>
        )}
        {!hooks.data?.length ? (
          <p className="px-4 py-4 text-[13px] text-fg-muted">No deploy hooks.</p>
        ) : (
          <div className="divide-y divide-line">
            {hooks.data.map((h) => (
              <div key={h.id} className="flex items-center gap-3 px-4 py-2 text-[13px]">
                <Link2 className="size-3.5 text-fg-dim" />
                <span className="flex-1 text-fg-muted">
                  Created {ago(h.created_at)} · {h.last_used_at ? `last used ${ago(h.last_used_at)}` : "never used"}
                </span>
                <IconButton label="Delete hook" onClick={() => void act(`dh-${h.id}`, () => client.deleteHook(app.id, h.id), "Hook deleted").then(() => void hooks.refresh())}>
                  <Trash2 />
                </IconButton>
              </div>
            ))}
          </div>
        )}
      </Card>

      <Card flush icon={<Clock />} title="Scheduled tasks" description="Commands run in the app's container on a cron schedule (UTC)." testId="tasks-card">
        {!!tasks.data?.length && (
          <div className="divide-y divide-line border-b border-line">
            {tasks.data.map((t) => (
              <div key={t.id} className="flex items-center gap-3 px-4 py-2" data-testid={`task-${t.name}`}>
                <span className="w-36 shrink-0 truncate text-[13px] text-fg">{t.name}</span>
                <span className="w-28 shrink-0 font-mono text-[12px] text-fg-muted">{t.schedule}</span>
                <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-fg-dim">{t.command}</span>
                <span className="w-32 shrink-0 text-right text-[11px] text-fg-dim">{t.last_run_at ? `${t.last_status} ${ago(t.last_run_at)}` : "not run yet"}</span>
                <IconButton label={`Run ${t.name} now`} onClick={() => void act(`run-${t.id}`, () => client.runTask(app.id, t.id), `${t.name} ran`).then(() => void tasks.refresh())}>
                  <Play />
                </IconButton>
                <IconButton label={`Delete ${t.name}`} onClick={() => void act(`dt-${t.id}`, () => client.deleteTask(app.id, t.id), `Deleted ${t.name}`).then(() => void tasks.refresh())}>
                  <Trash2 />
                </IconButton>
              </div>
            ))}
          </div>
        )}
        <div className="flex flex-wrap items-end gap-2 p-4">
          <div className="w-36">
            <Field label="Name">
              <TextInput value={task.name} onChange={(e) => setTask({ ...task, name: e.target.value })} data-testid="task-name" />
            </Field>
          </div>
          <div className="w-32">
            <Field label="Schedule">
              <TextInput value={task.schedule} onChange={(e) => setTask({ ...task, schedule: e.target.value })} className="font-mono" spellCheck={false} data-testid="task-schedule" />
            </Field>
          </div>
          <div className="min-w-48 flex-1">
            <Field label="Command">
              <TextInput value={task.command} onChange={(e) => setTask({ ...task, command: e.target.value })} className="font-mono" spellCheck={false} placeholder="npm run cleanup" data-testid="task-command" />
            </Field>
          </div>
          <Button
            icon={<Plus />}
            disabled={!task.name.trim() || !task.command.trim() || task.schedule.trim().split(/\s+/).length !== 5}
            busy={busy === "task"}
            data-testid="task-add"
            onClick={() =>
              void act("task", () => client.createTask(app.id, { ...task, enabled: true }), `Scheduled ${task.name}`).then((ok) => {
                if (ok) {
                  setTask({ name: "", schedule: "0 3 * * *", command: "" });
                  void tasks.refresh();
                }
              })
            }
          >
            Add
          </Button>
        </div>
      </Card>

      <PlacementCard app={app} client={client} onChanged={onChanged} />

      <VolumesCard app={app} client={client} />

      <Card title="Danger zone">
        <div className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="text-[13px] text-fg-muted">{stopped ? `${app.name} is stopped and serves nothing.` : `Stop ${app.name}: its containers are removed until you start it again.`}</p>
            <Button
              icon={stopped ? <Play /> : <Square />}
              busy={busy === "desired"}
              data-testid="app-stop"
              onClick={() =>
                void act("desired", () => client.patchApp(app.id, { desired: stopped ? "running" : "stopped" }), stopped ? `Starting ${app.name}` : `Stopped ${app.name}`).then(onChanged)
              }
            >
              {stopped ? "Start" : "Stop"}
            </Button>
          </div>
          <div className="flex flex-wrap items-end justify-between gap-3 border-t border-line pt-4">
            <div className="w-72">
              <Field label={`Type ${app.name} to delete it`} hint="Removes its containers, images, previews and settings. Volumes are kept.">
                <TextInput value={confirm} onChange={(e) => setConfirm(e.target.value)} spellCheck={false} data-testid="delete-confirm" />
              </Field>
            </div>
            <Button
              variant="danger"
              icon={<Trash2 />}
              disabled={confirm !== app.name}
              busy={busy === "delete"}
              data-testid="app-delete"
              onClick={() =>
                void act("delete", () => client.deleteApp(app.id), `Deleted ${app.name}`).then((ok) => {
                  if (ok) {
                    onChanged();
                    onDeleted();
                  }
                })
              }
            >
              Delete app
            </Button>
          </div>
        </div>
      </Card>
    </div>
  );
}

const instanceHealth = (state: string): Health => (state === "running" ? "ok" : state === "unknown" ? "idle" : "bad");

/**
 * Which servers run the app (design §8.3): its server (or automatic: the
 * one with the most memory to spare), replicas, and failover. Takes effect
 * with the next deployment.
 */
function PlacementCard({ app, client, onChanged }: { app: App; client: PlatformClient; onChanged: () => void }) {
  const { busy, act } = useAct();
  const nodes = usePoll(() => client.nodes(), 30_000, [client]);
  const instances = usePoll(() => client.instances(app.id), 15_000, [app.id, app.current_deployment_id, client]);
  const [nodeId, setNodeId] = useState(app.node_id ?? "");
  const [replicas, setReplicas] = useState(String(app.runtime.replicas));
  const [failover, setFailover] = useState(app.runtime.failover);
  useEffect(() => {
    setNodeId(app.node_id ?? "");
    setReplicas(String(app.runtime.replicas));
    setFailover(app.runtime.failover);
  }, [app.node_id, app.runtime.replicas, app.runtime.failover]);
  const servers = nodes.data ?? [];
  const n = Number(replicas);
  const validReplicas = Number.isInteger(n) && n >= 1 && n <= 16;
  const dirty = nodeId !== (app.node_id ?? "") || n !== app.runtime.replicas || failover !== app.runtime.failover;
  const compose = app.kind === "compose";

  return (
    <Card
      icon={<Server />}
      title="Where it runs"
      description={
        servers.length > 1
          ? "Its server, and how many copies run. Replicas spread over your servers; the main server balances visitors between them."
          : "On this server. Add one with sudo vercelify mesh invite (then join it) to run apps on more machines."
      }
      testId="placement-card"
    >
      <div className="space-y-3">
        {!!instances.data?.length && (
          <ul className="space-y-1" data-testid="instances">
            {instances.data.map((i) => (
              <li key={i.container} className="flex items-center gap-2 text-[13px]" data-testid={`instance-${i.container}`}>
                <StatusDot health={instanceHealth(i.state)} />
                <span className="font-medium text-fg">{i.node}</span>
                <span className="font-mono text-[12px] text-fg-dim">{i.container}</span>
                <span className="text-[12px] text-fg-muted">{i.state}</span>
              </li>
            ))}
          </ul>
        )}
        {servers.length > 1 && !compose && (
          <div className="flex flex-wrap items-end gap-2">
            <div className="w-56">
              <Field label="Server">
                <Select value={nodeId} onChange={(e) => setNodeId(e.target.value)} data-testid="placement-node">
                  <option value="">Automatic</option>
                  {servers.map((s) => (
                    <option key={s.id} value={s.id}>
                      {s.name}
                      {s.role === "main" ? " (main)" : ""}
                      {!s.online ? " — offline" : s.drained ? " — drained" : ""}
                    </option>
                  ))}
                </Select>
              </Field>
            </div>
            <div className="w-24">
              <Field label="Replicas">
                <TextInput type="number" min={1} max={16} value={replicas} onChange={(e) => setReplicas(e.target.value)} data-testid="placement-replicas" />
              </Field>
            </div>
            <label className="flex h-9 items-center gap-2 text-[13px] text-fg-muted">
              <input type="checkbox" checked={failover} onChange={(e) => setFailover(e.target.checked)} data-testid="placement-failover" />
              Move it when its server is offline
            </label>
            <Button
              busy={busy === "placement"}
              disabled={!dirty || !validReplicas}
              data-testid="placement-save"
              onClick={() =>
                void act(
                  "placement",
                  () => client.patchApp(app.id, { node_id: nodeId, runtime: { ...app.runtime, replicas: n, failover } }),
                  "Saved: the next deployment uses it",
                ).then((ok) => ok && onChanged())
              }
            >
              Save
            </Button>
          </div>
        )}
        {compose && servers.length > 1 && <p className="text-xs text-fg-muted">Compose apps run on the main server.</p>}
      </div>
    </Card>
  );
}

/** Persistent folders of the app: kept across deploys (and rollbacks). */
function VolumesCard({ app, client }: { app: App; client: PlatformClient }) {
  const vols = usePoll(() => client.volumes(app.id), 0, [app.id, client]);
  const { busy, act } = useAct();
  const [name, setName] = useState("data");
  const [path, setPath] = useState("/data");
  const nameOk = /^[a-z0-9][a-z0-9_.-]{0,62}$/.test(name);
  const pathOk = /^\/[^\s]*$/.test(path) && path !== "/";
  return (
    <Card
      flush
      icon={<HardDrive />}
      title="Volumes"
      description="Folders that keep their files across deploys. New ones are used from the next deploy; move them between disks on the Storage page."
      testId="volumes-card"
    >
      {!!vols.data?.length && (
        <div className="divide-y divide-line border-b border-line">
          {vols.data.map((v) => (
            <div key={v.id} className="flex items-center gap-3 px-4 py-2" data-testid={`volume-${v.name}`}>
              <span className="w-36 shrink-0 truncate font-mono text-[12px] text-fg">{v.name}</span>
              <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-fg-muted">{v.mount_path}</span>
              <Tag>{v.tier.toUpperCase()}</Tag>
              <IconButton
                label={`Remove ${v.name}`}
                onClick={() => void act(`rm-${v.id}`, () => client.removeVolume(app.id, v.name), `Removed ${v.name}; its data is kept until the app is deleted`).then(() => void vols.refresh())}
              >
                <Trash2 />
              </IconButton>
            </div>
          ))}
        </div>
      )}
      <div className="flex flex-wrap items-end gap-2 p-4">
        <div className="w-36">
          <Field label="Name">
            <TextInput value={name} onChange={(e) => setName(e.target.value.toLowerCase())} spellCheck={false} data-testid="volume-name" />
          </Field>
        </div>
        <div className="min-w-48 flex-1">
          <Field label="Mount path">
            <TextInput value={path} onChange={(e) => setPath(e.target.value)} className="font-mono" spellCheck={false} data-testid="volume-path" />
          </Field>
        </div>
        <Button
          icon={<Plus />}
          disabled={!nameOk || !pathOk}
          busy={busy === "add"}
          data-testid="volume-add"
          onClick={() =>
            void act("add", () => client.addVolume(app.id, { name, mount_path: path }), `Added ${name}. Deploy to mount it.`).then(() => void vols.refresh())
          }
        >
          Add
        </Button>
      </div>
    </Card>
  );
}

// ----- databases -------------------------------------------------------------------

const DB_KINDS: { value: DatabaseKind; label: string }[] = [
  { value: "postgresql", label: "PostgreSQL" },
  { value: "mysql", label: "MySQL" },
  { value: "mariadb", label: "MariaDB" },
  { value: "mongodb", label: "MongoDB" },
  { value: "redis", label: "Redis (Valkey)" },
  { value: "keydb", label: "KeyDB" },
  { value: "dragonfly", label: "Dragonfly" },
  { value: "clickhouse", label: "ClickHouse" },
];
const kindLabel = (k: DatabaseKind) => DB_KINDS.find((x) => x.value === k)?.label ?? k;

function DatabasesCard({
  client,
  dbs,
  apps,
  loading,
  error,
  refresh,
  creating,
  setCreating,
}: {
  client: PlatformClient;
  dbs: Database[] | undefined;
  apps: App[];
  loading: boolean;
  error?: { message: string };
  refresh: () => Promise<void>;
  creating: boolean;
  setCreating: (v: boolean) => void;
}) {
  const [open, setOpen] = useState<string>();
  const db = dbs?.find((d) => d.id === open);
  return (
    <>
      <Card
        flush
        icon={<DatabaseIcon />}
        title="Databases"
        description="Run next to your apps; apps reach them by name, and backups go where server backups go."
        actions={
          <Button size="sm" icon={<Plus />} onClick={() => setCreating(true)} data-testid="new-db">
            New database
          </Button>
        }
        testId="platform-dbs"
      >
        <ErrorNote error={error} className="m-4" />
        {loading && !dbs ? (
          <div className="p-4">
            <Loading rows={2} />
          </div>
        ) : !dbs?.length ? (
          <p className="px-4 py-6 text-center text-[13px] text-fg-muted">No databases yet.</p>
        ) : (
          <div className="divide-y divide-line">
            {dbs.map((d) => (
              <button key={d.id} type="button" className="flex w-full items-center gap-3 px-4 py-2.5 text-left transition-colors hover:bg-fg/[0.02]" onClick={() => setOpen(d.id)} data-testid={`db-${d.name}`}>
                <Monogram name={d.name} />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[13px] font-medium text-fg">{d.name}</span>
                  <span className="block truncate text-[11px] text-fg-dim">
                    {kindLabel(d.kind)} {d.version}
                    {d.public_port ? ` · public on :${d.public_port}` : ""}
                  </span>
                </span>
                <StatusText health={d.desired === "running" ? "ok" : "idle"} label={d.desired === "running" ? "Running" : "Stopped"} className="w-28 shrink-0" />
              </button>
            ))}
          </div>
        )}
      </Card>
      {db && <DatabaseDetail db={db} apps={apps} client={client} onChanged={() => void refresh()} onClose={() => setOpen(undefined)} />}
      <NewDatabaseModal
        open={creating}
        client={client}
        apps={apps}
        onClose={() => setCreating(false)}
        onCreated={(d) => {
          setCreating(false);
          void refresh();
          setOpen(d.id);
        }}
      />
    </>
  );
}

function NewDatabaseModal({ open, client, apps, onClose, onCreated }: { open: boolean; client: PlatformClient; apps: App[]; onClose: () => void; onCreated: (d: Database) => void }) {
  const [name, setName] = useState("");
  const [kind, setKind] = useState<DatabaseKind>("postgresql");
  const [link, setLink] = useState("");
  const [tier, setTier] = useState<StorageTier>("ssd");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  async function create() {
    setBusy(true);
    setError(undefined);
    try {
      const d = await client.createDatabase({ name, kind, tier, link_app: link || undefined });
      setName("");
      setLink("");
      onCreated(d);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal open={open} onClose={onClose} title="New database" description="Created with a generated password; apps reference it by name.">
      <div className="space-y-4 p-5">
        <div className="grid grid-cols-2 gap-3">
          <Field label="Name">
            <TextInput value={name} onChange={(e) => setName(slug(e.target.value))} placeholder="shop-db" spellCheck={false} data-testid="db-name" autoFocus />
          </Field>
          <Field label="Engine">
            <Select value={kind} onChange={(e) => setKind(e.target.value as DatabaseKind)} data-testid="db-kind">
              {DB_KINDS.map((k) => (
                <option key={k.value} value={k.value}>
                  {k.label}
                </option>
              ))}
            </Select>
          </Field>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <Field label="Connect to app" hint="Sets DATABASE_URL (or REDIS_URL…) on it">
            <Select value={link} onChange={(e) => setLink(e.target.value)} data-testid="db-link">
              <option value="">None</option>
              {apps.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="Storage">
            <Segmented label="database storage" value={tier} onChange={setTier} options={[{ value: "ssd", label: "SSD" }, { value: "hdd", label: "HDD" }]} />
          </Field>
        </div>
        {error && <Callout tone="bad">{error}</Callout>}
      </div>
      <div className="flex justify-end gap-2 border-t border-line px-5 py-3">
        <Button variant="ghost" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" disabled={!validName(name)} busy={busy} onClick={() => void create()} data-testid="db-create">
          Create
        </Button>
      </div>
    </Modal>
  );
}

function DatabaseDetail({ db, apps, client, onChanged, onClose }: { db: Database; apps: App[]; client: PlatformClient; onChanged: () => void; onClose: () => void }) {
  const { busy, act } = useAct();
  const [reveal, setReveal] = useState(false);
  const full = usePoll(() => (reveal ? client.database(db.id, true) : Promise.resolve(null)), 0, [db.id, reveal, client]);
  const backups = usePoll(() => client.backups(db.id), 10_000, [db.id, client]);
  useLiveTopic("databases", () => void backups.refresh());
  const [link, setLink] = useState("");
  const [confirm, setConfirm] = useState("");
  const [console_, setConsole] = useState(false);
  const url = (reveal ? full.data?.internal_url : db.internal_url) ?? "";
  const reference = `\${{db.${db.name}.url}}`;

  return (
    <Modal
      open
      sheet
      onClose={onClose}
      title={<span data-testid="db-detail">{db.name}</span>}
      description={`${kindLabel(db.kind)} ${db.version} · ${db.image}`}
      actions={
        <Button size="sm" icon={<SquareTerminal />} onClick={() => setConsole(true)} data-testid="db-console">
          Console
        </Button>
      }
    >
      <Modal open={console_} onClose={() => setConsole(false)} wide title={`${db.name} console`} description={`${kindLabel(db.kind)}'s own shell, logged in as the app user`}>
        <div className="p-4">
          {console_ && <TerminalView client={client} testId="db-terminal" open={(cols, rows) => client.openDatabaseTerminal(db.id, { cols, rows })} />}
        </div>
      </Modal>
      <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-5">
        <Card title="Connect" description="Apps of this project reach it by name. Use the reference in a variable so the password never sits in your app's settings.">
          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <code className="min-w-0 flex-1 truncate rounded-md border border-line bg-bg px-2 py-1.5 font-mono text-[12px]" data-testid="db-url">
                {url}
              </code>
              <IconButton label={reveal ? "Hide password" : "Show password"} onClick={() => setReveal(!reveal)}>
                {reveal ? <EyeOff /> : <Eye />}
              </IconButton>
              {reveal && url && <CopyButton text={url} />}
            </div>
            <div className="flex items-center gap-2 text-xs text-fg-muted">
              Reference: <InlineCode>{reference}</InlineCode>
              <IconButton label="Copy reference" onClick={() => void navigator.clipboard?.writeText(reference)}>
                <Copy />
              </IconButton>
            </div>
            <div className="flex items-end gap-2 border-t border-line pt-3">
              <div className="w-56">
                <Field label="Connect to an app">
                  <Select value={link} onChange={(e) => setLink(e.target.value)} data-testid="db-link-app">
                    <option value="">Pick an app…</option>
                    {apps.map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.name}
                      </option>
                    ))}
                  </Select>
                </Field>
              </div>
              <Button
                disabled={!link}
                busy={busy === "link"}
                data-testid="db-link-run"
                onClick={() =>
                  void act("link", async () => {
                    const r = await client.linkDatabase(db.id, link);
                    const name = apps.find((a) => a.id === link)?.name;
                    return r.variable && name;
                  }, `Set the variable on ${apps.find((a) => a.id === link)?.name ?? "the app"}. Deploy it to connect.`)
                }
              >
                Connect
              </Button>
            </div>
          </div>
        </Card>

        <Card
          flush
          title="Backups"
          description={db.backup_schedule ? `Scheduled ${db.backup_schedule} (UTC), plus the nightly server backup.` : "Taken with the nightly server backup."}
          actions={
            <Button size="sm" icon={<History />} busy={busy === "backup"} data-testid="db-backup" onClick={() => void act("backup", () => client.backup(db.id), `Backing up ${db.name}`)}>
              Back up now
            </Button>
          }
          testId="db-backups"
        >
          <ErrorNote error={backups.error} className="m-4" />
          {!backups.data?.length ? (
            <p className="px-4 py-5 text-center text-[13px] text-fg-muted">No backups yet.</p>
          ) : (
            <div className="divide-y divide-line">
              {backups.data.map((b) => (
                <div key={b.id} className="flex items-center gap-3 px-4 py-2">
                  <StatusText health={b.status === "succeeded" ? "ok" : b.status === "failed" ? "bad" : "warn"} label={b.status === "succeeded" ? ago(b.created_at) : b.status} className="w-28 shrink-0" />
                  <span className="flex-1 font-mono text-[12px] text-fg-muted">{b.snapshot || "—"}</span>
                  <span className="w-20 text-right text-[12px] text-fg-dim tabular-nums">{b.bytes ? bytes(b.bytes) : ""}</span>
                  {b.status === "succeeded" && (
                    <Button size="sm" variant="ghost" icon={<RotateCcw />} busy={busy === `restore-${b.id}`} onClick={() => void act(`restore-${b.id}`, () => client.restore(db.id, b.snapshot), `Restoring ${db.name} from ${b.snapshot}`)}>
                      Restore
                    </Button>
                  )}
                </div>
              ))}
            </div>
          )}
        </Card>

        <Card title="Danger zone">
          <div className="flex flex-wrap items-end justify-between gap-3">
            <div className="w-72">
              <Field label={`Type ${db.name} to delete it`} hint="Removes the container. Its data volume is kept unless you delete it on the Storage page.">
                <TextInput value={confirm} onChange={(e) => setConfirm(e.target.value)} spellCheck={false} data-testid="db-delete-confirm" />
              </Field>
            </div>
            <Button
              variant="danger"
              icon={<Trash2 />}
              disabled={confirm !== db.name}
              busy={busy === "delete"}
              data-testid="db-delete"
              onClick={() =>
                void act("delete", () => client.deleteDatabase(db.id), `Deleted ${db.name}`).then((ok) => {
                  if (ok) {
                    onChanged();
                    onClose();
                  }
                })
              }
            >
              Delete database
            </Button>
          </div>
        </Card>
      </div>
    </Modal>
  );
}
