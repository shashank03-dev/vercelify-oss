// Client of the platform engine's API (`/v1/platform/…` on the agent), for
// servers whose `/v1/health` lists the `platform` capability.

import type { Health } from "./format";
import type { CockpitSettings } from "../types";
import { call, streamLines, type HttpRequest } from "./transport";
import type {
  App,
  AppCreate,
  AppInstance,
  AppDomain,
  AppPatch,
  AppVolume,
  AppVolumeRequest,
  Database,
  DatabaseBackup,
  DatabaseCreate,
  DatabasePatch,
  DeployHook,
  Deployment,
  DeployRequest,
  DomainRequest,
  EnvVar,
  EnvVarsPut,
  GitSource,
  GitSourceRequest,
  GitWatchStatus,
  GithubAppStart,
  Job,
  PlatformDomain,
  NodePatch,
  PlatformNode,
  Project,
  ProjectRequest,
  ScheduledTask,
  ScheduledTaskRequest,
  ServiceTemplate,
  StandbyStatus,
  StorageTier,
  TemplateCreate,
  TerminalRequest,
  TerminalSession,
  VolumeInfo,
  WebhookSetup,
} from "../types";

const q = encodeURIComponent;

/** A line of a deployment's log. */
export interface LogLine {
  seq: number;
  at: string;
  /** stdout / stderr / info / error */
  stream: string;
  text: string;
}

/** A line of an app's own output (`docker logs`). */
export interface RuntimeLine {
  at: string;
  /** stdout / stderr */
  stream: string;
  text: string;
}

/** What a terminal's output stream carries, line by line. */
export type TerminalEvent = { data: string } | { exit: number };

/** A preview deployment of a pull request. */
export interface Preview {
  id: string;
  app_id: string;
  pr_number: number;
  head_sha: string;
  title: string;
  url: string;
  deployment_id: string | null;
  status: string;
  asleep: boolean;
  created_at: string;
  closed_at: string | null;
}

export function platformOn(server?: string) {
  const req = <T>(method: HttpRequest["method"], path: string, body?: unknown, timeout?: number) =>
    call<T>("agent_request", { request: { method, path: `/v1/platform${path}`, body, timeout }, server });
  return {
    projects: () => req<Project[]>("GET", "/projects"),
    createProject: (r: ProjectRequest) => req<Project>("POST", "/projects", r),
    deleteProject: (id: string) => req<unknown>("DELETE", `/projects/${q(id)}`),

    apps: (project?: string) => req<App[]>("GET", `/apps${project ? `?project=${q(project)}` : ""}`),
    app: (app: string) => req<App>("GET", `/apps/${q(app)}`),
    createApp: (r: AppCreate) => req<App>("POST", "/apps", r, 60),
    patchApp: (app: string, p: AppPatch) => req<App>("PATCH", `/apps/${q(app)}`, p),
    deleteApp: (app: string, purgeVolumes = false) =>
      req<{ deleted: string }>("DELETE", `/apps/${q(app)}${purgeVolumes ? "?purge_volumes=true" : ""}`),

    deploy: (app: string, r: DeployRequest = { force_rebuild: false }) => req<Deployment>("POST", `/apps/${q(app)}/deploy`, r),
    rollback: (app: string, deploymentId: string) =>
      req<Deployment>("POST", `/apps/${q(app)}/rollback`, { deployment_id: deploymentId }),
    deployments: (app: string, limit = 30) => req<Deployment[]>("GET", `/apps/${q(app)}/deployments?limit=${limit}`),
    deployment: (id: string) => req<Deployment>("GET", `/deployments/${q(id)}`),
    cancel: (id: string) => req<Deployment>("POST", `/deployments/${q(id)}/cancel`, {}),
    /** The log so far (one request). */
    logs: async (id: string, tail?: number): Promise<LogLine[]> => {
      const raw = await req<unknown>("GET", `/deployments/${q(id)}/logs${tail ? `?tail=${tail}` : ""}`);
      return parseLogLines(raw);
    },
    /** The log, then new lines until the deployment's log is complete. */
    followLogs: (id: string, onLine: (l: LogLine) => void) =>
      streamLines(
        { method: "GET", path: `/v1/platform/deployments/${q(id)}/logs?follow=true`, timeout: 3600 },
        (line) => {
          try {
            onLine(JSON.parse(line) as LogLine);
          } catch {
            /* a partial line */
          }
        },
        server,
      ),

    env: (app: string, reveal = false) => req<EnvVar[]>("GET", `/apps/${q(app)}/env${reveal ? "?reveal=true" : ""}`),
    putEnv: (app: string, r: EnvVarsPut) => req<{ saved: number }>("PUT", `/apps/${q(app)}/env`, r),
    importEnv: (app: string, content: string, replace = false) =>
      req<{ saved: number }>("POST", `/apps/${q(app)}/env/import`, { content, replace }),
    deleteEnv: (app: string, key: string, previewOnly = false) =>
      req<unknown>("DELETE", `/apps/${q(app)}/env/${q(key)}${previewOnly ? "?preview_only=true" : ""}`),

    domains: (app: string) => req<AppDomain[]>("GET", `/apps/${q(app)}/domains`),
    addDomain: (app: string, r: DomainRequest) => req<AppDomain>("POST", `/apps/${q(app)}/domains`, r),
    removeDomain: (app: string, host: string) => req<unknown>("DELETE", `/apps/${q(app)}/domains/${q(host)}`),

    volumes: (app: string) => req<AppVolume[]>("GET", `/apps/${q(app)}/volumes`),
    addVolume: (app: string, r: AppVolumeRequest) => req<AppVolume>("POST", `/apps/${q(app)}/volumes`, r),
    removeVolume: (app: string, name: string) => req<unknown>("DELETE", `/apps/${q(app)}/volumes/${q(name)}`),

    previews: (app: string) => req<Preview[]>("GET", `/apps/${q(app)}/previews`),
    removePreview: (app: string, pr: number) => req<unknown>("DELETE", `/apps/${q(app)}/previews/${pr}`),

    git: (app: string) => req<GitWatchStatus>("GET", `/apps/${q(app)}/git`),
    setGit: (app: string, enabled: boolean) => req<GitWatchStatus>("PUT", `/apps/${q(app)}/git`, { enabled }),
    checkGit: (app: string) => req<GitWatchStatus>("POST", `/apps/${q(app)}/git/check`, {}, 60),
    setBasicAuth: (app: string, user: string, password: string) =>
      req<{ protected: boolean }>("PUT", `/apps/${q(app)}/basic-auth`, { user, password }),
    hooks: (app: string) => req<DeployHook[]>("GET", `/apps/${q(app)}/hooks`),
    createHook: (app: string) => req<DeployHook>("POST", `/apps/${q(app)}/hooks`, {}),
    deleteHook: (app: string, id: string) => req<unknown>("DELETE", `/apps/${q(app)}/hooks/${q(id)}`),
    tasks: (app: string) => req<ScheduledTask[]>("GET", `/apps/${q(app)}/tasks`),
    createTask: (app: string, r: ScheduledTaskRequest) => req<ScheduledTask>("POST", `/apps/${q(app)}/tasks`, r),
    deleteTask: (app: string, id: string) => req<unknown>("DELETE", `/apps/${q(app)}/tasks/${q(id)}`),
    runTask: (app: string, id: string) => req<{ status: string }>("POST", `/apps/${q(app)}/tasks/${q(id)}/run`, {}, 900),

    databases: (reveal = false) => req<Database[]>("GET", `/databases${reveal ? "?reveal=true" : ""}`),
    database: (db: string, reveal = false) => req<Database>("GET", `/databases/${q(db)}${reveal ? "?reveal=true" : ""}`),
    createDatabase: (r: DatabaseCreate) => req<Database>("POST", "/databases", r, 60),
    patchDatabase: (db: string, p: DatabasePatch) => req<Database>("PATCH", `/databases/${q(db)}`, p),
    deleteDatabase: (db: string, purge = false) => req<unknown>("DELETE", `/databases/${q(db)}${purge ? "?purge=true" : ""}`),
    linkDatabase: (db: string, app: string) => req<{ variable: string }>("POST", `/databases/${q(db)}/link`, { app }),
    moveDatabase: (db: string, tier: StorageTier) => req<Job>("POST", `/databases/${q(db)}/move`, { tier }),
    backups: (db: string) => req<DatabaseBackup[]>("GET", `/databases/${q(db)}/backups`),
    backup: (db: string) => req<Job>("POST", `/databases/${q(db)}/backups`, {}),
    restore: (db: string, snapshot: string) => req<Job>("POST", `/databases/${q(db)}/restore`, { snapshot }),

    /** The app's own output so far (the last `tail` lines). */
    runtimeLogs: async (app: string, tail = 300): Promise<RuntimeLine[]> =>
      parseLines<RuntimeLine>(await req<unknown>("GET", `/apps/${q(app)}/logs?tail=${tail}`)),
    /** …and new lines as the app writes them. */
    followRuntimeLogs: (app: string, onLine: (l: RuntimeLine) => void, tail = 300) =>
      streamLines(
        { method: "GET", path: `/v1/platform/apps/${q(app)}/logs?follow=true&tail=${tail}`, timeout: 86_400 },
        (line) => {
          try {
            onLine(JSON.parse(line) as RuntimeLine);
          } catch {
            /* a partial line */
          }
        },
        server,
      ),
    restart: (app: string) => req<{ restarted: number }>("POST", `/apps/${q(app)}/restart`, {}, 120),

    openTerminal: (app: string, r: TerminalRequest = {}) => req<TerminalSession>("POST", `/apps/${q(app)}/terminal`, r),
    openDatabaseTerminal: (db: string, r: TerminalRequest = {}) => req<TerminalSession>("POST", `/databases/${q(db)}/terminal`, r),
    terminalOutput: (id: string, on: (e: TerminalEvent) => void) =>
      streamLines(
        { method: "GET", path: `/v1/platform/terminals/${q(id)}/output`, timeout: 86_400 },
        (line) => {
          try {
            on(JSON.parse(line) as TerminalEvent);
          } catch {
            /* a partial line */
          }
        },
        server,
      ),
    terminalInput: (id: string, data: string) => req<unknown>("POST", `/terminals/${q(id)}/input`, { data: toBase64(data) }),
    terminalResize: (id: string, cols: number, rows: number) => req<unknown>("POST", `/terminals/${q(id)}/resize`, { cols, rows }),
    closeTerminal: (id: string) => req<unknown>("DELETE", `/terminals/${q(id)}`),

    templates: () => req<ServiceTemplate[]>("GET", "/templates"),
    createFromTemplate: (id: string, r: TemplateCreate = {}) => req<App>("POST", `/templates/${q(id)}`, r, 60),
    allVolumes: () => req<VolumeInfo[]>("GET", "/volumes"),
    moveVolume: (id: string, tier: StorageTier) => req<Job>("POST", `/volumes/${q(id)}/move`, { tier }),

    gitSources: () => req<GitSource[]>("GET", "/git-sources"),
    createGitSource: (r: GitSourceRequest) => req<GitSource>("POST", "/git-sources", r),
    deleteGitSource: (id: string) => req<unknown>("DELETE", `/git-sources/${q(id)}`),
    webhook: (id: string) => req<WebhookSetup>("POST", `/git-sources/${q(id)}/webhook`),
    startGithubApp: (org?: string) => req<GithubAppStart>("POST", "/github-app", org ? { org } : {}),
    convertGithubApp: (code: string, state: string) => req<GitSource>("POST", "/github-app/convert", { code, state }),
    domain: () => req<PlatformDomain>("GET", "/settings/domain"),
    setDomain: (d: PlatformDomain) => req<PlatformDomain>("PUT", "/settings/domain", d),
    nodes: () => req<PlatformNode[]>("GET", "/nodes"),
    patchNode: (id: string, p: NodePatch) => req<PlatformNode>("PATCH", `/nodes/${q(id)}`, p),
    removeNode: (id: string) => req<unknown>("DELETE", `/nodes/${q(id)}`),
    instances: (app: string) => req<AppInstance[]>("GET", `/apps/${q(app)}/instances`),
    standby: () => req<StandbyStatus>("GET", "/settings/standby"),
    setStandby: (node: string | null) => req<StandbyStatus>("PUT", "/settings/standby", { node }),
  };
}

export const platform = platformOn();
export type PlatformClient = ReturnType<typeof platformOn>;

/** UTF-8 text → base64 (what the terminal input takes). */
export function toBase64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** base64 → bytes (terminal output, which may split a character). */
export function fromBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** JSON-lines answers (or, from the demo, an array). */
export function parseLines<T>(raw: unknown): T[] {
  if (Array.isArray(raw)) return raw as T[];
  if (typeof raw !== "string") return [];
  return raw
    .split("\n")
    .filter((l) => l.trim())
    .flatMap((l) => {
      try {
        return [JSON.parse(l) as T];
      } catch {
        return [];
      }
    });
}

/** The log endpoint answers JSON lines (or, from the demo, an array). */
export function parseLogLines(raw: unknown): LogLine[] {
  if (Array.isArray(raw)) return raw as LogLine[];
  if (typeof raw !== "string") return [];
  return raw
    .split("\n")
    .filter((l) => l.trim())
    .flatMap((l) => {
      try {
        return [JSON.parse(l) as LogLine];
      } catch {
        return [];
      }
    });
}

// ----- presentation helpers ------------------------------------------------------

/**
 * A main server: it runs Vercelify's engine (the agent serves
 * `/v1/platform`). One whose engine didn't start has no `platform`
 * capability in its health.
 */
export const isPlatform = (s: CockpitSettings) => s.role !== "worker";

/** An app as lists, the palette and the menu bar show it. */
export interface ListedApp {
  id: string;
  name: string;
  health: Health;
  url: string | null;
  /** What it is: a build pack for Git apps, else `image` or `compose`. */
  kind: string;
}

/** How an app is doing, from what it should do and its newest deployment. */
export function appState(app: App, latest: Deployment["status"] | null | undefined = app.last_status): { health: Health; label: string } {
  if (app.desired === "stopped") return { health: "idle", label: "Stopped" };
  if (app.desired === "sleeping") return { health: "idle", label: "Asleep" };
  if (latest === "queued" || latest === "building" || latest === "deploying") {
    return { health: "warn", label: STATUS_LABEL[latest] };
  }
  if (!app.current_deployment_id) {
    return latest === "failed" ? { health: "bad", label: "Failed" } : { health: "idle", label: "Not deployed" };
  }
  if (latest === "failed") return { health: "warn", label: "Live, last deploy failed" };
  return { health: "ok", label: "Live" };
}

/** The shape the sidebar, top bar and ⌘K list apps in. */
export function listedApp(a: App): ListedApp {
  return { id: a.id, name: a.name, health: appState(a).health, url: a.url, kind: a.kind === "git" ? a.build.pack : a.kind };
}

export function deploymentHealth(d: Deployment): Health {
  switch (d.status) {
    case "healthy":
      return "ok";
    case "failed":
      return "bad";
    case "queued":
    case "building":
    case "deploying":
      return "warn";
    default:
      return "idle";
  }
}

export const STATUS_LABEL: Record<Deployment["status"], string> = {
  queued: "Queued",
  building: "Building",
  deploying: "Deploying",
  healthy: "Healthy",
  failed: "Failed",
  cancelled: "Cancelled",
  superseded: "Replaced",
};

export const TRIGGER_LABEL: Record<Deployment["trigger"], string> = {
  push: "Push",
  manual: "Manual",
  hook: "Deploy hook",
  webhook: "Webhook",
  rollback: "Rollback",
  "auto-rollback": "Automatic rollback",
  recover: "Recovered",
};

/** "my blog" → "my-blog": the rule app and database names follow. */
export function slug(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, 40)
    .replace(/-+$/, "");
}

/** A domain as the agent stores it (`validate::host`): scheme, trailing
 * slash and dot dropped, lowercase. */
export const normalizeDomain = (raw: string) =>
  raw
    .trim()
    .toLowerCase()
    .replace(/^https?:\/\//, "")
    .replace(/\/+$/, "")
    .replace(/\.+$/, "");

/** Why the agent would refuse a domain (null: it accepts it). */
export function domainProblem(raw: string): string | null {
  const h = normalizeDomain(raw);
  if (!h) return null;
  if (h.startsWith("*.")) return "Wildcards are a server setting (Settings → Domain), not an app domain.";
  const labels = h.split(".");
  if (h.length > 253 || labels.length < 2) return "Use a full name such as app.example.com.";
  if (labels.some((l) => l.length > 63)) return "Each part between dots is at most 63 characters.";
  if (!labels.every((l) => /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(l))) return "Use letters, digits and dashes between the dots.";
  if (/^\d+$/.test(labels[labels.length - 1]!)) return "That's an IP address; use a name (sslip.io names work).";
  return null;
}

export const validName = (s: string) => /^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$/.test(s);

/** https or scp-style ssh git URLs (what the agent accepts). */
export const validRepo = (s: string) =>
  /^https:\/\/[^\s/]+\/\S+$/.test(s.trim()) || /^[\w.-]+@[\w.-]+:[\w./-]+$/.test(s.trim()) || /^ssh:\/\/\S+\/\S+$/.test(s.trim());

