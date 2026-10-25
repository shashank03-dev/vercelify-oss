// Demo backend of the platform engine (`/v1/platform/…`) for the demo server
// `optiplex-7050`: an in-memory stand-in for the agent's API, so the platform
// screens can be designed and tested without a host. Deployments move through
// queued → building → deploying → healthy on timers, with a streamed log.

import type {
  App,
  AppCreate,
  AppDomain,
  AppPatch,
  AppVolume,
  Database,
  DatabaseBackup,
  DatabaseCreate,
  DatabaseKind,
  DeployHook,
  Deployment,
  DeploymentStatus,
  EnvVar,
  EnvVarsPut,
  GitSource,
  GitWatchStatus,
  Job,
  AppInstance,
  PlatformDomain,
  PlatformNode,
  Project,
  StandbyStatus,
  ScheduledTask,
  ScheduledTaskRequest,
  ServiceTemplate,
} from "../types";
import type { HttpRequest } from "./transport";
import type { LogLine, Preview } from "./platform";

const t0 = Date.now();
const iso = (minsAgo: number) => new Date(t0 - minsAgo * 60_000).toISOString();
const isoNow = () => new Date().toISOString();
let counter = 0;
const id = (prefix: string) => `${prefix}_demo${(++counter).toString(36).padStart(12, "0")}`;
const sha = (n: number) => (n * 2654435761).toString(16).padStart(8, "0").repeat(5);

/** Set by demo.ts: tells the cockpit something changed (the live event stream). */
let changed: (topic: string) => void = () => {};
let jobStarted: (kind: string, title: string, target: string) => Job = (kind, title, target) => ({
  id: id("job"),
  kind,
  title,
  status: "running",
  started_at: isoNow(),
  finished_at: null,
  detail: null,
  progress: null,
  target,
});
let jobFinished: (j: Job, detail: string) => void = () => {};
export function wirePlatformDemo(hooks: {
  changed: (topic: string) => void;
  startJob: (kind: string, title: string, target: string) => Job;
  finishJob: (j: Job, detail: string) => void;
}) {
  changed = hooks.changed;
  jobStarted = hooks.startJob;
  jobFinished = hooks.finishJob;
}

const WILDCARD = "apps.shashank.dev";
const ENV_ID = "env_demoproduction00";
const project: Project = {
  id: "proj_demodefault0000",
  name: "Default",
  description: "",
  environments: [{ id: ENV_ID, project_id: "proj_demodefault0000", name: "production", created_at: iso(60 * 24 * 40) }],
  app_count: 0,
  database_count: 0,
  created_at: iso(60 * 24 * 40),
  updated_at: iso(60 * 24 * 40),
};

function domain(host: string, primary = true): AppDomain {
  return { id: id("dom"), host, https: true, is_primary: primary, redirect_to: null, cert_status: "issued", created_at: iso(60 * 24 * 20) };
}

function newApp(name: string, kind: App["kind"], over: Partial<App> = {}): App {
  const domains = [domain(`${name}.${WILDCARD}`)];
  return {
    id: id("app"),
    name,
    project_id: project.id,
    environment_id: ENV_ID,
    environment: "production",
    kind,
    source: { repository: null, branch: null, git_source_id: null, image: null, compose_file: null, compose_content: null, pinned_sha: null },
    build: { pack: "auto", dockerfile: null, context: null, install_command: null, build_command: null, output_directory: null, watch_paths: [] },
    runtime: { port: null, command: null, replicas: 1, memory_mb: null, cpus: null, health: null, health_timeout_secs: 120, drain_secs: 10, service: null, failover: false },
    previews: { enabled: kind === "git", max: 3 },
    node_id: "node_demomain00000000",
    desired: "running",
    current_deployment_id: null,
    last_status: null,
    domains,
    url: `https://${domains[0]!.host}`,
    created_at: iso(60 * 24 * 20),
    updated_at: iso(60 * 24 * 20),
    ...over,
  };
}

const git = (repo: string) => ({ repository: `https://github.com/shashank03-dev/${repo}`, branch: "main", git_source_id: null, image: null, compose_file: null, compose_content: null, pinned_sha: null });
const apps: App[] = [
  newApp("shop", "git", { source: git("shop") }),
  newApp("api", "git", {
    source: git("notes-api"),
    build: { pack: "dockerfile", dockerfile: "Dockerfile", context: null, install_command: null, build_command: null, output_directory: null, watch_paths: [] },
    node_id: "node_demopi5000000000",
    runtime: { port: null, command: null, replicas: 2, memory_mb: 256, cpus: null, health: null, health_timeout_secs: 120, drain_secs: 10, service: null, failover: true },
  }),
  newApp("umami", "image", { source: { repository: null, branch: null, git_source_id: null, image: "ghcr.io/umami-software/umami:postgresql-latest", compose_file: null, compose_content: null, pinned_sha: null } }),
  newApp("wiki", "git", { source: git("wiki"), desired: "sleeping" }),
];

const deployments = new Map<string, Deployment[]>();
const logs = new Map<string, LogLine[]>();

const MESSAGES = ["Checkout: Apple Pay button", "Fix cart total rounding", "Bump next to 15.4", "Cache product pages for 60 s", "Initial commit"];
const GOOD_LOG = [
  "Fetching https://github.com/shashank03-dev/shop (main)…",
  "Checked out 9f2c1e0",
  "Detected Next.js 15 (npm, standalone output)",
  "Building with BuildKit (memory limit 2 GiB)",
  "#7 [deps 3/3] RUN npm ci",
  "#7 added 412 packages in 9s",
  "#9 [build 4/4] RUN npm run build",
  "#9   ✓ Compiled successfully in 14.2s",
  "#12 exporting to image",
  "Built vfy/shop:9f2c1e0 in 1m 12s",
  "Starting the new container",
  "Health check passed (GET / → 200)",
  "Switched shop.apps.shashank.dev to the new version",
  "Draining the old version for 10 s",
  "Deployment is live",
];
const BAD_LOG = [
  "Fetching https://github.com/shashank03-dev/notes-api (main)…",
  "Checked out 4c1a77b",
  "Building Dockerfile",
  "#8 [build 3/4] RUN go build -o /out/api ./cmd/api",
  "#8 cmd/api/main.go:42:2: undefined: handlers.Notes",
  "#8 ERROR: process \"/bin/sh -c go build -o /out/api ./cmd/api\" did not complete successfully: exit code: 1",
  "Build failed: exit status 1",
];

function lines(text: string[], at: string, failed: boolean): LogLine[] {
  return text.map((t, i) => ({
    seq: i + 1,
    at,
    stream: failed && i >= text.length - 2 ? "error" : i === 0 || t.startsWith("Switched") || t.startsWith("Deployment") ? "info" : "stdout",
    text: t,
  }));
}

function seed(app: App, statuses: DeploymentStatus[], minsAgo: number[]) {
  const list: Deployment[] = statuses.map((status, i) => {
    const n = statuses.length - i;
    const created = iso(minsAgo[i]!);
    const d: Deployment = {
      id: id("dep"),
      app_id: app.id,
      number: n,
      kind: "production",
      pr_number: null,
      branch: app.source.branch ?? "",
      commit_sha: app.kind === "image" ? "" : sha(n + app.name.length),
      commit_message: app.kind === "image" ? "" : MESSAGES[i % MESSAGES.length]!,
      trigger: i === statuses.length - 1 ? "manual" : "push",
      rollback_of: null,
      image: app.kind === "image" ? app.source.image! : `vfy/${app.name}:${sha(n + app.name.length).slice(0, 7)}`,
      node_id: app.node_id,
      status,
      error: status === "failed" ? "build failed: exit status 1" : "",
      current: false,
      created_at: created,
      started_at: created,
      finished_at: new Date(Date.parse(created) + 84_000).toISOString(),
    };
    logs.set(d.id, lines(status === "failed" ? BAD_LOG : GOOD_LOG, created, status === "failed"));
    return d;
  });
  const live = list.find((d) => d.status === "healthy");
  if (live) {
    live.current = true;
    app.current_deployment_id = live.id;
  }
  app.last_status = list[0]?.status ?? null;
  deployments.set(app.id, list);
}
seed(apps[0]!, ["healthy", "superseded", "failed", "superseded", "superseded"], [4, 180, 200, 1500, 9000]);
seed(apps[1]!, ["failed", "healthy", "superseded"], [12, 300, 4000]);
seed(apps[2]!, ["healthy"], [60 * 24 * 6]);
seed(apps[3]!, ["healthy", "superseded"], [60 * 24 * 2, 60 * 24 * 9]);

const env = new Map<string, EnvVar[]>();
const envVar = (key: string, value: string, secret: boolean, build = false): EnvVar => ({
  id: id("env"),
  key,
  value: secret ? null : value,
  build,
  preview_only: false,
  secret,
  scope: "app",
  updated_at: iso(600),
});
const secretValues = new Map<string, string>();
env.set(apps[0]!.id, [
  envVar("NEXT_PUBLIC_SITE_URL", "https://shop.apps.shashank.dev", false, true),
  envVar("STRIPE_SECRET_KEY", "", true),
  envVar("DATABASE_URL", "${{db.shop-db.url}}", false),
]);
secretValues.set(`${apps[0]!.id}/STRIPE_SECRET_KEY`, "sk_live_demo_51NzQ…");

const previews = new Map<string, Preview[]>([
  [
    apps[0]!.id,
    [
      {
        id: id("prv"),
        app_id: apps[0]!.id,
        pr_number: 42,
        head_sha: sha(42),
        title: "New pricing page",
        url: `https://pr-42-shop.${WILDCARD}`,
        deployment_id: null,
        status: "healthy",
        asleep: false,
        created_at: iso(50),
        closed_at: null,
      },
    ],
  ],
]);
const hooks = new Map<string, DeployHook[]>();
const tasks = new Map<string, ScheduledTask[]>([
  [
    apps[0]!.id,
    [{ id: id("task"), app_id: apps[0]!.id, name: "Clean carts", schedule: "0 3 * * *", command: "node scripts/clean-carts.js", enabled: true, last_run_at: iso(600), last_status: "succeeded", created_at: iso(60 * 24 * 10) }],
  ],
]);
const watches = new Map<string, GitWatchStatus>();
for (const a of apps) {
  if (a.kind === "git") {
    watches.set(a.id, { app_id: a.id, enabled: true, last_head: sha(5 + a.name.length), last_checked_at: iso(1), last_error: "", skipped: 0, last_skip: "" });
  }
}
const volumes = new Map<string, AppVolume[]>([
  [apps[2]!.id, [{ id: id("vol"), owner_kind: "app", owner_id: apps[2]!.id, name: "umami-data", mount_path: "/app/data", tier: "ssd", host_path: null, created_at: iso(60 * 24 * 6) }]],
]);

const DB_IMAGES: Record<DatabaseKind, [string, string]> = {
  postgresql: ["17", "postgres:17-alpine"],
  mysql: ["8.4", "mysql:8.4"],
  mariadb: ["11.4", "mariadb:11.4"],
  mongodb: ["8.0", "mongo:8.0"],
  redis: ["8", "valkey/valkey:8-alpine"],
  keydb: ["6.3", "eqalpha/keydb:alpine"],
  dragonfly: ["1.25", "docker.dragonflydb.io/dragonflydb/dragonfly:v1.25"],
  clickhouse: ["24.8", "clickhouse/clickhouse-server:24.8-alpine"],
};
const DB_URL: Record<DatabaseKind, (name: string) => string> = {
  postgresql: (n) => `postgres://${n}:••••@vfy-db-${n}:5432/${n}`,
  mysql: (n) => `mysql://${n}:••••@vfy-db-${n}:3306/${n}`,
  mariadb: (n) => `mysql://${n}:••••@vfy-db-${n}:3306/${n}`,
  mongodb: (n) => `mongodb://${n}:••••@vfy-db-${n}:27017/${n}?authSource=admin`,
  redis: (n) => `redis://default:••••@vfy-db-${n}:6379`,
  keydb: (n) => `redis://default:••••@vfy-db-${n}:6379`,
  dragonfly: (n) => `redis://default:••••@vfy-db-${n}:6379`,
  clickhouse: (n) => `clickhouse://${n}:••••@vfy-db-${n}:8123/${n}`,
};
function newDb(name: string, kind: DatabaseKind, minsAgo = 60 * 24 * 12): Database {
  const [version, image] = DB_IMAGES[kind];
  return {
    id: id("db"),
    name,
    project_id: project.id,
    environment_id: ENV_ID,
    kind,
    version,
    image,
    node_id: "node_demomain00000000",
    internal_url: DB_URL[kind](name),
    public_port: null,
    public_bind: "mesh",
    desired: "running",
    container: `vfy-db-${name}`,
    backup_schedule: kind === "postgresql" ? "0 2 * * *" : "",
    created_at: iso(minsAgo),
    updated_at: iso(minsAgo),
  };
}
const databases: Database[] = [newDb("shop-db", "postgresql"), newDb("cache", "redis")];
const backups = new Map<string, DatabaseBackup[]>([
  [
    databases[0]!.id,
    [0, 1, 2].map((d) => ({
      id: id("bak"),
      database_id: databases[0]!.id,
      status: "succeeded",
      bytes: 18_400_000 - d * 220_000,
      snapshot: sha(d + 100).slice(0, 8),
      error: "",
      created_at: iso(60 * (24 * d + 7)),
      finished_at: iso(60 * (24 * d + 7) - 1),
    })),
  ],
]);
const gitSources: GitSource[] = [];
let domainSetting: PlatformDomain = { wildcard: WILDCARD, https: true, edge_tls: false };

// ----- deploying ------------------------------------------------------------------

function findApp(ref: string): App {
  const a = apps.find((x) => x.id === ref || x.name === ref);
  if (!a) throw new Error(`no app ${ref}`);
  return a;
}

function findDb(ref: string): Database {
  const d = databases.find((x) => x.id === ref || x.name === ref);
  if (!d) throw new Error(`no database ${ref}`);
  return d;
}

function findDeployment(depId: string): Deployment {
  for (const list of deployments.values()) {
    const d = list.find((x) => x.id === depId);
    if (d) return d;
  }
  throw new Error(`no deployment ${depId}`);
}

/** Log listeners of running deployments (the follow streams). */
const followers = new Map<string, Set<(l: LogLine | null) => void>>();

function log(d: Deployment, text: string, stream = "stdout") {
  const list = logs.get(d.id) ?? [];
  const line: LogLine = { seq: list.length + 1, at: isoNow(), stream, text };
  list.push(line);
  logs.set(d.id, list);
  followers.get(d.id)?.forEach((fn) => fn(line));
}

function settle(d: Deployment, status: DeploymentStatus) {
  const app = findApp(d.app_id);
  d.status = status;
  d.finished_at = isoNow();
  if (status === "healthy") {
    for (const o of deployments.get(app.id) ?? []) {
      if (o.current && o.id !== d.id) {
        o.current = false;
        o.status = "superseded";
      }
    }
    d.current = true;
    app.current_deployment_id = d.id;
    if (app.desired === "stopped") app.desired = "running";
  }
  app.last_status = status;
  followers.get(d.id)?.forEach((fn) => fn(null));
  followers.delete(d.id);
  changed("deployments");
}

function enqueue(app: App, trigger: Deployment["trigger"], rollbackOf: Deployment | null = null): Deployment {
  const running = (deployments.get(app.id) ?? []).find((x) => ["queued", "building", "deploying"].includes(x.status));
  if (running) throw new Error(`${app.name} is already deploying (#${running.number})`);
  const list = deployments.get(app.id) ?? [];
  const n = (list[0]?.number ?? 0) + 1;
  const commit = rollbackOf?.commit_sha ?? (app.kind === "image" ? "" : sha(n + app.name.length));
  const d: Deployment = {
    id: id("dep"),
    app_id: app.id,
    number: n,
    kind: "production",
    pr_number: null,
    branch: app.source.branch ?? "",
    commit_sha: commit,
    commit_message: rollbackOf ? `Rollback to #${rollbackOf.number}` : app.kind === "image" ? "" : "Latest on main",
    trigger,
    rollback_of: rollbackOf?.id ?? null,
    image: rollbackOf?.image ?? (app.kind === "image" ? app.source.image! : `vfy/${app.name}:${commit.slice(0, 7)}`),
    node_id: app.node_id,
    status: "queued",
    error: "",
    current: false,
    created_at: isoNow(),
    started_at: null,
    finished_at: null,
  };
  list.unshift(d);
  deployments.set(app.id, list);
  app.last_status = "queued";
  logs.set(d.id, []);
  changed("deployments");

  // A rollback reuses the image on disk: no build.
  const steps = rollbackOf || app.kind === "image" ? GOOD_LOG.slice(10) : GOOD_LOG.map((l) => l.replace(/shop/g, app.name));
  const fail = app.name === "api" && !rollbackOf;
  const script = fail ? BAD_LOG : steps;
  let i = 0;
  const tick = () => {
    if (d.status === "cancelled") return;
    if (i === 0) {
      d.status = rollbackOf || app.kind === "image" ? "deploying" : "building";
      d.started_at = isoNow();
      app.last_status = d.status;
      changed("deployments");
    }
    if (i < script.length) {
      const text = script[i]!;
      if (text.startsWith("Starting the new container") && d.status === "building") {
        d.status = "deploying";
        app.last_status = d.status;
        changed("deployments");
      }
      log(d, text, i === 0 || text.startsWith("Switched") ? "info" : fail && i >= script.length - 2 ? "error" : "stdout");
      i++;
      window.setTimeout(tick, 260);
      return;
    }
    if (fail) d.error = "build failed: exit status 1";
    settle(d, fail ? "failed" : "healthy");
  };
  window.setTimeout(tick, 600);
  return d;
}

// ----- routes ---------------------------------------------------------------------

const clone = <T>(v: T): T => structuredClone(v);

function revealEnv(app: App, list: EnvVar[], reveal: boolean): EnvVar[] {
  return list.map((v) => (v.secret && reveal ? { ...v, value: secretValues.get(`${app.id}/${v.key}`) ?? "" } : { ...v }));
}

function putEnv(app: App, r: EnvVarsPut) {
  const list = env.get(app.id) ?? [];
  const keep = r.replace ? [] : list.filter((v) => !r.vars.some((n) => n.key === v.key));
  for (const n of r.vars) {
    const secret = n.secret ?? true;
    keep.push({ ...envVar(n.key, n.value, secret, n.build ?? false), preview_only: n.preview_only ?? false, updated_at: isoNow() });
    if (secret) secretValues.set(`${app.id}/${n.key}`, n.value);
  }
  env.set(app.id, keep.sort((a, b) => a.key.localeCompare(b.key)));
  return { saved: r.vars.length };
}

function job(kind: string, title: string, target: string, detail: string, then?: () => void): Job {
  const j = jobStarted(kind, title, target);
  window.setTimeout(() => {
    then?.();
    jobFinished(j, detail);
    changed("databases");
  }, 1400);
  return j;
}

/** The demo fleet: the main server and a Raspberry Pi worker on the mesh. */
const nodes: PlatformNode[] = [
  {
    id: "node_demomain00000000",
    name: "optiplex-7050",
    role: "main",
    agent_url: "http://127.0.0.1:7420",
    arch: "x86_64",
    builds: true,
    last_seen_at: null,
    created_at: iso(60 * 24 * 40),
    mesh_ip: "10.77.0.1",
    online: true,
    drained: false,
    status: { cpus: 4, memory_total_mb: 7840, memory_available_mb: 3920, on_battery: false, battery_percent: null, thermal_paused: false, docker_ok: true, http_port: 80, agent_version: "0.2.0" },
    instances: 0,
  },
  {
    id: "node_demopi5000000000",
    name: "pi-5",
    role: "worker",
    agent_url: "http://10.77.0.2:7420",
    arch: "aarch64",
    builds: false,
    last_seen_at: null,
    created_at: iso(60 * 24 * 9),
    mesh_ip: "10.77.0.2",
    online: true,
    drained: false,
    status: { cpus: 4, memory_total_mb: 7900, memory_available_mb: 6100, on_battery: false, battery_percent: null, thermal_paused: false, docker_ok: true, http_port: 80, agent_version: "0.2.0" },
    instances: 0,
  },
];

let standby: StandbyStatus = { node_id: null, node: null, last_sent_at: null, bytes: 0, last_error: null };

/** Where an app's live deployment runs: its server first, replicas spread. */
function demoInstances(a: App): AppInstance[] {
  if (!a.current_deployment_id) return [];
  const first = nodes.find((n) => n.id === a.node_id) ?? nodes[0]!;
  const order = [first, ...nodes.filter((n) => n.id !== first.id && !n.drained)];
  const base = `vfy-${a.name}-${a.current_deployment_id.slice(-8).toLowerCase()}`;
  return Array.from({ length: Math.max(1, a.runtime.replicas) }, (_, r) => {
    const n = order[r % order.length]!;
    return { node_id: n.id, node: n.name, container: r ? `${base}-r${r}` : base, state: a.desired === "running" ? "running" : "exited" };
  });
}

function instanceCount(nodeId: string): number {
  return apps.flatMap(demoInstances).filter((i) => i.node_id === nodeId).length;
}

/** `/v1/platform/…` on the demo platform server. */
export function platformRoute(req: HttpRequest): unknown {
  const [path, query = ""] = req.path.replace(/^\/v1\/platform/, "").split("?");
  const params = new URLSearchParams(query);
  const seg = path!.split("/").filter(Boolean).map(decodeURIComponent);
  const m = req.method;
  const body = (req.body ?? {}) as Record<string, unknown>;
  const extra = runtimeRoute(seg, m, body, params);
  if (extra !== undefined) return extra;

  if (seg[0] === "projects" && m === "GET") {
    return [{ ...project, app_count: apps.length, database_count: databases.length }];
  }
  if (seg[0] === "nodes") {
    if (seg[1] && m === "PATCH") {
      const n = nodes.find((x) => x.id === seg[1] || x.name === seg[1]);
      if (!n) throw new Error(`server ${seg[1]} not found`);
      if (typeof body.drained === "boolean") n.drained = body.drained;
      if (typeof body.builds === "boolean") n.builds = body.builds;
      return clone(n);
    }
    if (seg[1] && m === "DELETE") {
      const n = nodes.find((x) => x.id === seg[1] || x.name === seg[1]);
      if (!n) throw new Error(`server ${seg[1]} not found`);
      if (n.role === "main") throw new Error("the main server can't be removed");
      const running = instanceCount(n.id);
      if (running > 0) throw new Error(`${n.name} still runs ${running} instance(s): drain it and redeploy them first`);
      nodes.splice(nodes.indexOf(n), 1);
      return { removed: true };
    }
    return clone(nodes.map((n) => ({ ...n, last_seen_at: isoNow(), instances: instanceCount(n.id) })));
  }
  if (seg[0] === "settings" && seg[1] === "standby") {
    if (m === "PUT") {
      const n = body.node ? nodes.find((x) => x.id === body.node || x.name === body.node) : null;
      if (body.node && !n) throw new Error(`server ${String(body.node)} not found`);
      if (n?.role === "main") throw new Error(`${n.name} is the main server: the standby is a worker`);
      standby = n ? { node_id: n.id, node: n.name, last_sent_at: isoNow(), bytes: 412_160, last_error: null } : { node_id: null, node: null, last_sent_at: null, bytes: 0, last_error: null };
    }
    return { ...standby };
  }
  if (seg[0] === "settings" && seg[1] === "domain") {
    if (m === "PUT") domainSetting = body as unknown as PlatformDomain;
    return { ...domainSetting };
  }
  if (seg[0] === "github-app") {
    if (seg[1] === "convert") {
      if (!body.code) throw new Error("that isn't a code from GitHub");
      const g: GitSource = {
        id: id("git"),
        name: "GitHub App vercelify-optiplex",
        provider: "github",
        auth: "github_app",
        public_key: "",
        created_at: isoNow(),
        webhook: true,
        app_url: "https://github.com/apps/vercelify-optiplex",
      };
      gitSources.push(g);
      return g;
    }
    return { start_url: "https://github.com/settings/apps/new?state=demo", state: "demo", expires_at: isoNow() };
  }
  if (seg[0] === "git-sources" && seg[2] === "webhook") {
    const g = gitSources.find((x) => x.id === seg[1]);
    if (!g) throw new Error(`git source ${seg[1]} not found`);
    g.webhook = true;
    return {
      url: `https://mesh.example.com/v1/webhooks/${g.provider === "generic" ? "github" : g.provider}/${g.id}`,
      secret: "Zq3demoWebhookSecretForTheCockpitDemo42",
      lan_urls: [`http://192.168.1.20:7443/v1/webhooks/github/${g.id}`],
      content_type: "application/json",
      events: ["push", "pull_request"],
    };
  }
  if (seg[0] === "git-sources") {
    if (m === "POST") {
      const g: GitSource = {
        id: id("git"),
        name: String(body.name ?? "GitHub"),
        provider: "github",
        auth: body.auth === "deploy_key" ? "deploy_key" : "token",
        public_key: body.auth === "deploy_key" ? "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDemoKeyForVercelifyOptiplex vercelify" : "",
        created_at: isoNow(),
        webhook: false,
      };
      gitSources.push(g);
      return g;
    }
    if (m === "DELETE") gitSources.splice(gitSources.findIndex((g) => g.id === seg[1]), 1);
    return clone(gitSources);
  }

  if (seg[0] === "deployments" && seg[1]) {
    const d = findDeployment(seg[1]);
    if (seg[2] === "cancel") {
      if (!["queued", "building"].includes(d.status)) throw new Error("only queued or building deployments can be cancelled");
      log(d, "Cancelled", "error");
      settle(d, "cancelled");
      return clone(d);
    }
    if (seg[2] === "logs") return clone(logs.get(d.id) ?? []);
    return clone(d);
  }

  if (seg[0] === "apps") {
    if (!seg[1]) {
      if (m === "POST") return createApp(body as unknown as AppCreate);
      return clone(apps);
    }
    const app = findApp(seg[1]);
    const sub = seg[2];
    if (!sub) {
      if (m === "PATCH") {
        Object.assign(app, body as AppPatch, { updated_at: isoNow() });
        // An empty server means "place it automatically".
        if (body.node_id === "") app.node_id = null;
        changed("apps");
      }
      if (m === "DELETE") {
        apps.splice(apps.indexOf(app), 1);
        changed("apps");
        return { deleted: app.id };
      }
      return clone(app);
    }
    switch (sub) {
      case "deploy":
        return clone(enqueue(app, "manual"));
      case "rollback": {
        const target = findDeployment(String(body.deployment_id));
        app.source.pinned_sha = target.commit_sha || null;
        return clone(enqueue(app, "rollback", target));
      }
      case "deployments":
        return clone((deployments.get(app.id) ?? []).slice(0, Number(params.get("limit") ?? 30)));
      case "env": {
        if (seg[3] === "import") {
          const vars = String(body.content ?? "")
            .split("\n")
            .map((l) => l.trim())
            .filter((l) => l && !l.startsWith("#") && l.includes("="))
            .map((l) => ({ key: l.slice(0, l.indexOf("=")).replace(/^export /, "").trim(), value: l.slice(l.indexOf("=") + 1).replace(/^["']|["']$/g, "") }));
          return putEnv(app, { vars, replace: !!body.replace });
        }
        if (seg[3] && m === "DELETE") {
          env.set(app.id, (env.get(app.id) ?? []).filter((v) => v.key !== seg[3]));
          return { deleted: seg[3] };
        }
        if (m === "PUT") return putEnv(app, body as unknown as EnvVarsPut);
        return revealEnv(app, env.get(app.id) ?? [], params.get("reveal") === "true");
      }
      case "domains": {
        if (m === "POST") {
          const host = String(body.host ?? "").toLowerCase();
          if (!/^[a-z0-9.-]+\.[a-z]{2,}$/.test(host)) throw new Error(`${host} is not a domain name`);
          if (apps.some((a) => a.domains.some((d) => d.host === host))) throw new Error(`${host} is already used`);
          const d: AppDomain = { ...domain(host, !!body.primary), cert_status: "pending", https: body.https !== false };
          if (d.is_primary) app.domains.forEach((x) => (x.is_primary = false));
          app.domains.push(d);
          window.setTimeout(() => {
            d.cert_status = "issued";
            changed("apps");
          }, 2500);
        }
        if (m === "DELETE" && seg[3]) app.domains = app.domains.filter((d) => d.host !== seg[3]);
        const primary = app.domains.find((d) => d.is_primary) ?? app.domains[0];
        app.url = primary ? `https://${primary.host}` : null;
        return clone(app.domains);
      }
      case "volumes": {
        const list = volumes.get(app.id) ?? [];
        if (m === "POST") {
          list.push({ id: id("vol"), owner_kind: "app", owner_id: app.id, name: String(body.name), mount_path: String(body.mount_path), tier: (body.tier as AppVolume["tier"]) ?? "ssd", host_path: null, created_at: isoNow() });
          volumes.set(app.id, list);
        }
        if (m === "DELETE") volumes.set(app.id, list.filter((v) => v.name !== seg[3]));
        return clone(volumes.get(app.id) ?? []);
      }
      case "previews": {
        if (m === "DELETE") previews.set(app.id, (previews.get(app.id) ?? []).filter((p) => p.pr_number !== Number(seg[3])));
        return clone(previews.get(app.id) ?? []);
      }
      case "git": {
        const w = watches.get(app.id) ?? { app_id: app.id, enabled: false, last_head: "", last_checked_at: null, last_error: "", skipped: 0, last_skip: "" };
        if (m === "PUT") w.enabled = !!body.enabled;
        if (seg[3] === "check") w.last_checked_at = isoNow();
        watches.set(app.id, w);
        return { ...w };
      }
      case "basic-auth":
        return { protected: !!body.user };
      case "hooks": {
        const list = hooks.get(app.id) ?? [];
        if (m === "POST") {
          const h: DeployHook = { id: id("hook"), created_at: isoNow(), last_used_at: null, secret: "vfyh_" + Math.random().toString(36).slice(2, 14) };
          list.push({ ...h, secret: null });
          hooks.set(app.id, list);
          return h;
        }
        if (m === "DELETE") hooks.set(app.id, list.filter((h) => h.id !== seg[3]));
        return clone(hooks.get(app.id) ?? []);
      }
      case "tasks": {
        const list = tasks.get(app.id) ?? [];
        if (m === "POST" && !seg[3]) {
          const r = body as unknown as ScheduledTaskRequest;
          if (r.schedule.trim().split(/\s+/).length !== 5) throw new Error(`invalid schedule \`${r.schedule}\`: use five cron fields`);
          const t: ScheduledTask = { id: id("task"), app_id: app.id, ...r, last_run_at: null, last_status: "", created_at: isoNow() };
          list.push(t);
          tasks.set(app.id, list);
          return t;
        }
        if (seg[4] === "run") {
          const t = list.find((x) => x.id === seg[3]);
          if (t) Object.assign(t, { last_run_at: isoNow(), last_status: "succeeded" });
          return { status: "succeeded" };
        }
        if (m === "DELETE") tasks.set(app.id, list.filter((t) => t.id !== seg[3]));
        return clone(tasks.get(app.id) ?? []);
      }
    }
  }

  if (seg[0] === "databases") {
    if (!seg[1]) {
      if (m === "POST") return createDatabase(body as unknown as DatabaseCreate);
      return clone(databases);
    }
    const db = findDb(seg[1]);
    switch (seg[2]) {
      case undefined:
        if (m === "PATCH") {
          Object.assign(db, body, { updated_at: isoNow() });
          changed("databases");
        }
        if (m === "DELETE") {
          databases.splice(databases.indexOf(db), 1);
          changed("databases");
          return { deleted: db.id };
        }
        return clone(params.get("reveal") === "true" ? { ...db, internal_url: db.internal_url?.replace("••••", "s3cr3t-demo-pw") ?? null } : db);
      case "link": {
        const app = findApp(String(body.app));
        const variable = db.kind === "postgresql" || db.kind === "mysql" || db.kind === "mariadb" ? "DATABASE_URL" : db.kind === "mongodb" ? "MONGODB_URL" : db.kind === "clickhouse" ? "CLICKHOUSE_URL" : "REDIS_URL";
        putEnv(app, { vars: [{ key: variable, value: `\${{db.${db.name}.url}}`, secret: false }], replace: false });
        return { variable };
      }
      case "move":
        return job("volume-move", `Move ${db.name} to the ${String(body.tier).toUpperCase()}`, db.id, `${db.name} now keeps its data on the ${String(body.tier).toUpperCase()}`);
      case "backups": {
        if (m === "POST") {
          return job("backup", `Back up ${db.name}`, db.id, "backed up", () => {
            const list = backups.get(db.id) ?? [];
            list.unshift({ id: id("bak"), database_id: db.id, status: "succeeded", bytes: 18_600_000, snapshot: sha(Date.now() % 997).slice(0, 8), error: "", created_at: isoNow(), finished_at: isoNow() });
            backups.set(db.id, list);
          });
        }
        return clone(backups.get(db.id) ?? []);
      }
      case "restore":
        return job("backup", `Restore ${db.name} from ${String(body.snapshot)}`, db.id, `${db.name} restored`);
    }
  }
  throw new Error(`demo: no platform route for ${m} ${req.path}`);
}

function createApp(r: AppCreate): App {
  if (!/^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$/.test(r.name)) throw new Error(`invalid name \`${r.name}\``);
  if (apps.some((a) => a.name === r.name) || databases.some((d) => d.name === r.name)) throw new Error(`the name \`${r.name}\` is already used`);
  const a = newApp(r.name, r.kind, {
    source: { ...r.source },
    ...(r.build ? { build: r.build } : {}),
    ...(r.runtime ? { runtime: r.runtime } : {}),
    created_at: isoNow(),
    updated_at: isoNow(),
  });
  for (const host of r.domains) a.domains.push(domain(host, false));
  apps.push(a);
  deployments.set(a.id, []);
  if (a.kind === "git") watches.set(a.id, { app_id: a.id, enabled: true, last_head: "", last_checked_at: null, last_error: "", skipped: 0, last_skip: "" });
  changed("apps");
  enqueue(a, "manual");
  return clone(a);
}

function createDatabase(r: DatabaseCreate): Database {
  if (!/^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$/.test(r.name)) throw new Error(`invalid name \`${r.name}\``);
  if (apps.some((a) => a.name === r.name) || databases.some((d) => d.name === r.name)) throw new Error(`the name \`${r.name}\` is already used`);
  const d = newDb(r.name, r.kind, 0);
  if (r.version) d.version = r.version;
  if (r.public_port) d.public_port = r.public_port;
  databases.push(d);
  if (r.link_app) platformRoute({ method: "POST", path: `/v1/platform/databases/${d.name}/link`, body: { app: r.link_app } });
  changed("databases");
  return clone(d);
}

/** Streams of the demo platform: deployment logs, app output, terminals. */
export async function followPlatformLogs(path: string, onLine: (line: string) => void, signal: AbortSignal): Promise<void> {
  const runtime = /\/apps\/([^/]+)\/logs/.exec(path)?.[1];
  if (runtime) return followRuntime(findApp(decodeURIComponent(runtime)), onLine, signal);
  const term = /\/terminals\/([^/]+)\/output/.exec(path)?.[1];
  if (term) return followTerminal(decodeURIComponent(term), onLine, signal);
  const depId = /\/deployments\/([^/]+)\/logs/.exec(path)?.[1];
  if (!depId) return;
  const d = findDeployment(decodeURIComponent(depId));
  for (const l of logs.get(d.id) ?? []) onLine(JSON.stringify(l));
  if (!["queued", "building", "deploying"].includes(d.status)) return;
  await new Promise<void>((resolve) => {
    const set = followers.get(d.id) ?? new Set();
    const fn = (l: LogLine | null) => {
      if (l) onLine(JSON.stringify(l));
      else done();
    };
    const done = () => {
      set.delete(fn);
      resolve();
    };
    set.add(fn);
    followers.set(d.id, set);
    signal.addEventListener("abort", done, { once: true });
  });
}

// ----- runtime: app output, restart, terminals, templates, volumes ----------------------

const RUNTIME = [
  "> shop@1.4.0 start",
  "> next start -p 3000",
  "  ▲ Next.js 15.4.2",
  "  - Local: http://localhost:3000",
  " ✓ Ready in 412ms",
  "GET / 200 in 38ms",
  "GET /api/products 200 in 12ms",
  "GET /cart 200 in 21ms",
];

function runtimeLine(text: string, minsAgo = 0, stream = "stdout") {
  return { at: new Date(Date.now() - minsAgo * 60_000).toISOString().replace(/\.\d+Z$/, "Z"), stream, text };
}

function runtimeLog(app: App) {
  const out = RUNTIME.map((t, i) => runtimeLine(t.replace("shop", app.name), RUNTIME.length - i));
  if (app.name === "api") out.push(runtimeLine("warn: slow query on /notes (812ms)", 1, "stderr"));
  return out;
}

async function followRuntime(app: App, onLine: (line: string) => void, signal: AbortSignal) {
  for (const l of runtimeLog(app)) onLine(JSON.stringify(l));
  const paths = ["/", "/api/products", "/cart", "/checkout", "/api/health"];
  let n = 0;
  await new Promise<void>((resolve) => {
    const id = window.setInterval(() => {
      const p = paths[n++ % paths.length]!;
      onLine(JSON.stringify(runtimeLine(`GET ${p} 200 in ${8 + ((n * 7) % 40)}ms`)));
    }, 900);
    signal.addEventListener("abort", () => (window.clearInterval(id), resolve()), { once: true });
  });
}

/** A pretend shell: echoes keys, knows a few commands. */
interface DemoTerm {
  id: string;
  target: string;
  line: string;
  listeners: Set<(e: { data?: string; exit?: number }) => void>;
  backlog: string;
  exited: number | null;
  db: boolean;
}
const terms = new Map<string, DemoTerm>();
const b64 = (t: string) => btoa(String.fromCharCode(...new TextEncoder().encode(t)));
const unb64 = (b: string) => new TextDecoder().decode(Uint8Array.from(atob(b), (c) => c.charCodeAt(0)));

function termOut(t: DemoTerm, text: string) {
  t.backlog += text;
  t.listeners.forEach((fn) => fn({ data: text }));
}

function prompt(t: DemoTerm) {
  return t.db ? `${t.target.replace("vfy-db-", "")}=> ` : `/app $ `;
}

function openTerm(target: string, db: boolean) {
  const t: DemoTerm = { id: id("term"), target, line: "", listeners: new Set(), backlog: "", exited: null, db };
  terms.set(t.id, t);
  window.setTimeout(() => termOut(t, (db ? "psql (17.2)\r\nType \"help\" for help.\r\n\r\n" : "") + prompt(t)), 150);
  return { id: t.id, target };
}

function runCommand(t: DemoTerm, cmd: string): string | null {
  const c = cmd.trim();
  if (c === "exit" || c === "\\q") return null;
  if (!c) return "";
  if (t.db) {
    if (/^select 1/i.test(c)) return " ?column? \r\n----------\r\n        1\r\n(1 row)\r\n";
    if (c === "\\dt") return "         List of relations\r\n Schema |   Name   | Type  \r\n--------+----------+-------\r\n public | orders   | table\r\n public | products | table\r\n(2 rows)\r\n";
    return `ERROR:  syntax error at or near "${c.split(" ")[0]}"\r\n`;
  }
  if (c === "pwd") return "/app\r\n";
  if (c === "ls") return "node_modules  package.json  public  server.js  .next\r\n";
  if (c === "whoami") return "node\r\n";
  if (c.startsWith("echo ")) return c.slice(5) + "\r\n";
  if (c === "node -v") return "v22.11.0\r\n";
  return `sh: ${c.split(" ")[0]}: not found\r\n`;
}

function termInput(t: DemoTerm, data: string) {
  for (const ch of data) {
    if (t.exited !== null) return;
    if (ch === "\r") {
      termOut(t, "\r\n");
      const res = runCommand(t, t.line);
      t.line = "";
      if (res === null) {
        t.exited = 0;
        t.listeners.forEach((fn) => fn({ exit: 0 }));
        return;
      }
      termOut(t, res + prompt(t));
    } else if (ch === "\u007f") {
      if (t.line) {
        t.line = t.line.slice(0, -1);
        termOut(t, "\b \b");
      }
    } else if (ch >= " ") {
      t.line += ch;
      termOut(t, ch);
    }
  }
}

async function followTerminal(tid: string, onLine: (line: string) => void, signal: AbortSignal) {
  const t = terms.get(tid);
  if (!t) throw new Error(`no terminal ${tid}`);
  if (t.backlog) onLine(JSON.stringify({ data: b64(t.backlog) }));
  if (t.exited !== null) return onLine(JSON.stringify({ exit: t.exited }));
  await new Promise<void>((resolve) => {
    const fn = (e: { data?: string; exit?: number }) => {
      if (e.data !== undefined) onLine(JSON.stringify({ data: b64(e.data) }));
      else {
        onLine(JSON.stringify({ exit: e.exit }));
        t.listeners.delete(fn);
        resolve();
      }
    };
    t.listeners.add(fn);
    signal.addEventListener("abort", () => (t.listeners.delete(fn), resolve()), { once: true });
  });
}

const TEMPLATE_INFO: ServiceTemplate[] = [
  { id: "umami", name: "Umami", description: "Privacy-friendly website analytics, a light alternative to Google Analytics.", category: "analytics", website: "https://umami.is", min_memory_mb: 512, compose: "services:\n  umami:\n    image: ghcr.io/umami-software/umami:postgresql-v2.15.1\n" },
  { id: "plausible", name: "Plausible", description: "Simple, cookie-free analytics with a fast dashboard. Runs ClickHouse: needs about 2 GB of memory.", category: "analytics", website: "https://plausible.io", min_memory_mb: 2048, compose: "services:\n  plausible:\n" },
  { id: "minio", name: "MinIO", description: "S3-compatible object storage for uploads and backups. The domain opens its console; apps reach the S3 API at minio:9000.", category: "storage", website: "https://min.io", min_memory_mb: 512, compose: "services:\n  minio:\n" },
  { id: "unleash", name: "Unleash", description: "Feature flags and gradual rollouts for your apps.", category: "flags", website: "https://www.getunleash.io", min_memory_mb: 512, compose: "services:\n  unleash:\n" },
  { id: "uptime-kuma", name: "Uptime Kuma", description: "Status pages and uptime monitoring for any website or service.", category: "monitoring", website: "https://uptime.kuma.pet", min_memory_mb: 256, compose: "services:\n  kuma:\n" },
  { id: "vaultwarden", name: "Vaultwarden", description: "A password manager server compatible with the Bitwarden apps. Keep it on HTTPS.", category: "security", website: "https://github.com/dani-garcia/vaultwarden", min_memory_mb: 128, compose: "services:\n  vaultwarden:\n" },
];

function runtimeRoute(seg: string[], m: string, body: Record<string, unknown>, params: URLSearchParams): unknown {
  if (seg[0] === "templates") {
    if (!seg[1]) return clone(TEMPLATE_INFO);
    const t = TEMPLATE_INFO.find((x) => x.id === seg[1]);
    if (!t) throw new Error(`no template ${seg[1]}`);
    const a = createApp({
      name: String(body.name ?? t.id),
      kind: "compose",
      source: { repository: null, branch: null, git_source_id: null, image: null, compose_file: null, compose_content: t.compose, pinned_sha: null },
      domains: [],
    });
    env.set(a.id, [envVar("POSTGRES_PASSWORD", "", true), envVar("APP_SECRET", "", true)]);
    return a;
  }
  if (seg[0] === "volumes") {
    if (seg[2] === "move") {
      const vol = [...volumes.values()].flat().find((v) => v.id === seg[1]) ?? null;
      const dbVol = databases.find((d) => `vol_${d.id}` === seg[1]);
      const label = vol ? vol.name : dbVol ? dbVol.name : seg[1]!;
      return job("volume-move", `Move ${label} to the ${String(body.tier).toUpperCase()}`, seg[1]!, `${label} is on the ${String(body.tier).toUpperCase()} now`, () => {
        if (vol) vol.tier = body.tier as AppVolume["tier"];
        if (dbVol) dbTier.set(dbVol.id, body.tier as AppVolume["tier"]);
        changed("volumes");
      });
    }
    const out = [];
    for (const a of apps) for (const v of volumes.get(a.id) ?? []) out.push({ volume: clone(v), owner_name: a.name });
    for (const d of databases) {
      out.push({
        volume: { id: `vol_${d.id}`, owner_kind: "database" as const, owner_id: d.id, name: "data", mount_path: d.kind === "postgresql" ? "/var/lib/postgresql/data" : "/data", tier: dbTier.get(d.id) ?? "ssd", host_path: null, created_at: d.created_at },
        owner_name: d.name,
      });
    }
    return out;
  }
  if (seg[0] === "terminals" && seg[1]) {
    const t = terms.get(seg[1]);
    if (!t) throw new Error(`no terminal ${seg[1]} (it ended, or another token opened it)`);
    if (m === "DELETE") {
      terms.delete(t.id);
      return { closed: true };
    }
    if (seg[2] === "input") termInput(t, unb64(String(body.data ?? "")));
    return { ok: true };
  }
  if (seg[0] === "databases" && seg[2] === "terminal") {
    const d = findDb(seg[1]!);
    return openTerm(d.container, body.console !== false);
  }
  if (seg[0] === "apps" && seg[1] && seg[2] === "instances") {
    const a = apps.find((x) => x.id === seg[1] || x.name === seg[1]);
    if (!a) throw new Error(`app ${seg[1]} not found`);
    return demoInstances(a);
  }
  if (seg[0] === "apps" && seg[1] && ["logs", "restart", "terminal"].includes(seg[2] ?? "")) {
    const a = findApp(seg[1]);
    if (!a.current_deployment_id) throw new Error(`${a.name} isn't deployed yet`);
    if (seg[2] === "logs") return runtimeLog(a).slice(-Number(params.get("tail") ?? 300));
    if (seg[2] === "restart") return { restarted: 1 };
    return openTerm(`vfy-${a.name}-1`, false);
  }
  return undefined;
}

const dbTier = new Map<string, AppVolume["tier"]>();
