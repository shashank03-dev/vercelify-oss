import { call, streamLines, type HttpRequest } from "./transport";
import type {
  AgentEvent,
  Alert,
  AlertsStatus,
  AppImages,
  AuditEntry,
  AuthorizeKeyResult,
  AutoDeployRequest,
  AutoDeployStatus,
  AutoDeployWatch,
  BackupStatus,
  CaptureScreen,
  CockpitConfig,
  CockpitSettings,
  ContainerInfo,
  CoolifyApplication,
  CoolifyDatabase,
  CoolifyDeployment,
  CoolifyDeploymentLog,
  CoolifyEnv,
  CoolifyPreview,
  CoolifyProject,
  CoolifyServer,
  CoolifyVolume,
  CoolifyWorkerKey,
  DatabaseKind,
  DeviceToken,
  DeviceTokenCreated,
  DiscoveredServer,
  DisplayStatus,
  DoctorReport,
  DomainInfo,
  Health,
  Job,
  LanServer,
  LlmChatRequest,
  LlmStatus,
  MeshInvite,
  MeshLocal,
  MeshNode,
  MetricsResponse,
  MoonlightLaunch,
  MoveVolumeResult,
  Pairing,
  PowerGuardSettings,
  PowerGuardStatus,
  ProcessStatus,
  ProjectFile,
  ProjectInfo,
  ProjectSummary,
  ProjectsStatus,
  ProxyMetricsResult,
  ReceiverConfig,
  SenderConfig,
  ServiceStatus,
  SleepRequest,
  SleepStatus,
  StorageTier,
  SystemSnapshot,
  TailscaleStatus,
  ToolStatus,
  UpdateStatus,
  UptimeCheck,
  VolumeEntry,
  VolumeRequest,
  VolumeResult,
} from "../types";

export const settings = {
  load: () => call<CockpitSettings | null>("settings_load"),
  save: (s: CockpitSettings) => call<void>("settings_save", { settings: s }),
  clear: () => call<void>("settings_clear"),
  decodePairing: (code: string) => call<Pairing>("pair_decode", { code }),
  /** Decode a pasted code; a `vfy2.` one also joins this computer to the mesh. */
  joinPairing: (code: string) => call<Pairing>("pair_join", { code }),
  test: (s: CockpitSettings) =>
    call<{ hostname: string; coolify: { ok: boolean; version?: string; error?: string } }>("test_connection", {
      settings: s,
    }),
};

/** Every paired server, and switching between them. */
export const servers = {
  load: () => call<CockpitConfig>("servers_load"),
  select: (agentUrl: string) => call<CockpitConfig>("server_select", { agentUrl }),
  remove: (agentUrl: string) => call<CockpitConfig>("server_remove", { agentUrl }),
  discover: () => call<DiscoveredServer[]>("discover_servers"),
  /** Trade a short code from `sudo vercelify pair` for the full pairing. */
  claim: (agentUrl: string, code: string) => call<Pairing>("pair_claim", { agentUrl, code }),
};

/** This computer on Vercelify's own network (vercelify-mesh). */
export const mesh = {
  local: () => call<MeshLocal>("mesh_local"),
  /** Servers on this LAN (mDNS). */
  discover: () => call<LanServer[]>("mesh_discover"),
  /** A short code → the pairing, over the LAN; joins the mesh. */
  pair: (code: string, server?: string) => call<Pairing>("mesh_pair", { code, server }),
};

export function pairingToSettings(p: Pairing): CockpitSettings {
  return {
    agent_url: p.agent_url,
    agent_token: p.agent_token,
    coolify_url: p.coolify_url,
    coolify_token: p.coolify_token ?? null,
    hostname: p.hostname,
    role: p.role ?? (p.coolify_url ? "main" : "worker"),
  };
}

/** Workers deploy nothing themselves. A main server on the platform engine has
 * no Coolify URL but says `role: "main"`; older pairings carry no role. */
export const isWorker = (s: CockpitSettings) => s.role === "worker" || (!s.role && !s.coolify_url);

const q = encodeURIComponent;

/** Agent API of one server (`server` = its agent URL; default: the selected one). */
function makeAgent(server?: string) {
  const req = <T>(method: HttpRequest["method"], path: string, body?: unknown, timeout?: number) =>
    call<T>("agent_request", { request: { method, path, body, timeout }, server });
  return {
    system: () => req<SystemSnapshot>("GET", "/v1/system"),
    tailscale: () => req<TailscaleStatus>("GET", "/v1/tailscale"),
    containers: (stats = true) => req<ContainerInfo[]>("GET", `/v1/containers?stats=${stats}`),
    containerAction: (id: string, action: "start" | "stop" | "restart" | "pause" | "unpause") =>
      req<{ ok: boolean }>("POST", `/v1/containers/${q(id)}/${action}`, {}, 120),
    containerLogs: (id: string, tail = 300) => req<{ lines: string[] }>("GET", `/v1/containers/${q(id)}/logs?tail=${tail}`),
    volumes: () => req<VolumeEntry[]>("GET", "/v1/volumes"),
    createVolume: (r: VolumeRequest) => req<VolumeResult>("POST", "/v1/volumes", r),
    coolifyVolumes: () => req<CoolifyVolume[]>("GET", "/v1/coolify/volumes"),
    moveVolume: (id: number, tier: StorageTier) => req<MoveVolumeResult>("POST", `/v1/coolify/volumes/${id}/move`, { tier }, 600),
    /** Agents with "jobs": answers at once; follow the job in the activity list. */
    moveVolumeInBackground: (id: number, tier: StorageTier) =>
      req<Job>("POST", `/v1/coolify/volumes/${id}/move?background=true`, { tier }, 30),
    display: () => req<DisplayStatus>("GET", "/v1/display"),
    receiverStart: (cfg: ReceiverConfig) => req<DisplayStatus>("POST", "/v1/display/receiver/start", cfg),
    receiverStop: () => req<DisplayStatus>("POST", "/v1/display/receiver/stop", {}),
    services: () => req<ServiceStatus[]>("GET", "/v1/services"),
    serviceAction: (name: string, action: "start" | "stop" | "restart") =>
      req<{ ok: boolean }>("POST", `/v1/services/${q(name)}/${action}`, {}, 90),

    autodeploy: () => req<AutoDeployStatus>("GET", "/v1/autodeploy"),
    autodeploySave: (r: AutoDeployRequest) => req<AutoDeployWatch>("POST", "/v1/autodeploy", r),
    autodeployRemove: (appUuid: string) => req<{ ok: boolean }>("DELETE", `/v1/autodeploy/${q(appUuid)}`),
    autodeployCheck: (appUuid: string) => req<AutoDeployWatch>("POST", `/v1/autodeploy/${q(appUuid)}/check`, {}, 60),

    llm: () => req<LlmStatus>("GET", "/v1/llm"),
    llmInstall: (tier: StorageTier) => req<LlmStatus>("POST", "/v1/llm/install", { tier }, 60),
    llmAction: (action: "start" | "stop" | "restart" | "remove") => req<LlmStatus>("POST", `/v1/llm/${action}`, {}, 120),
    llmPull: (model: string) => req<LlmStatus>("POST", "/v1/llm/models/pull", { model }),
    llmDelete: (model: string) => req<{ ok: boolean }>("POST", "/v1/llm/models/delete", { model }),
    llmChat: (r: LlmChatRequest, onLine: (line: string) => void) =>
      streamLines({ method: "POST", path: "/v1/llm/chat", body: { ...r, stream: true }, timeout: 900 }, onLine, server),

    health: () => req<Health>("GET", "/v1/health", undefined, 10),
    doctor: (refresh = false) => req<DoctorReport>("GET", `/v1/doctor${refresh ? "?refresh=true" : ""}`, undefined, 90),
    alerts: () => req<AlertsStatus>("GET", "/v1/alerts"),
    setNotifyUrl: (url: string) => req<AlertsStatus>("POST", "/v1/alerts/notify", { url }),
    testAlert: () => req<Alert>("POST", "/v1/alerts/test", {}, 30),
    checkNow: () => req<AlertsStatus>("POST", "/v1/alerts/check", {}, 120),
    metrics: (minutes = 24 * 60) => req<MetricsResponse>("GET", `/v1/metrics?minutes=${minutes}`),
    uptime: () => req<UptimeCheck[]>("GET", "/v1/uptime"),
    tokens: () => req<DeviceToken[]>("GET", "/v1/tokens"),
    createToken: (name: string) => req<DeviceTokenCreated>("POST", "/v1/tokens", { name }),
    revokeToken: (id: string) => req<{ ok: boolean }>("DELETE", `/v1/tokens/${q(id)}`),
    /** The mesh's nodes (on its coordinator). */
    meshNodes: () => req<MeshNode[]>("GET", "/v1/mesh/nodes"),
    /** Take a node off the mesh; its peers drop it within seconds. */
    meshRevoke: (id: string) => req<{ revoked: string }>("DELETE", `/v1/mesh/nodes/${q(id)}`, undefined, 20),
    meshInvite: (kind: "server" | "device", role = "owner") =>
      req<MeshInvite>("POST", "/v1/mesh/invites", { kind, role, ttl_minutes: 60 }),
    audit: (limit = 50) => req<AuditEntry[]>("GET", `/v1/audit?limit=${limit}`),
    images: (appUuid: string) => req<AppImages>("GET", `/v1/images/${q(appUuid)}`),
    backup: () => req<BackupStatus>("GET", "/v1/backup"),
    backupRun: () => req<BackupStatus>("POST", "/v1/backup/run", {}),
    jobs: () => req<Job[]>("GET", "/v1/jobs"),
    /** The live event stream (newline-delimited JSON); see `useLive`. */
    events: (onEvent: (e: AgentEvent) => void) =>
      streamLines(
        { method: "GET", path: "/v1/events", timeout: 900 },
        (line) => {
          try {
            onEvent(JSON.parse(line) as AgentEvent);
          } catch {
            /* a partial or unknown line */
          }
        },
        server,
      ),
    sleep: () => req<SleepStatus>("GET", "/v1/sleep"),
    setSleep: (appUuid: string, r: SleepRequest) => req<SleepStatus>("PUT", `/v1/sleep/${q(appUuid)}`, r, 200),
    enableProxyMetrics: () => req<ProxyMetricsResult>("POST", "/v1/sleep/proxy", {}, 200),
    powerGuard: () => req<PowerGuardStatus>("GET", "/v1/power-guard"),
    setPowerGuard: (s: PowerGuardSettings) => req<PowerGuardStatus>("PUT", "/v1/power-guard", s, 60),
    resetPowerGuard: () => req<PowerGuardStatus>("DELETE", "/v1/power-guard", undefined, 60),
    domain: () => req<DomainInfo>("GET", "/v1/domain"),
    projects: () => req<ProjectsStatus>("GET", "/v1/projects"),
    checkProject: (files: ProjectFile[]) => req<ProjectSummary>("POST", "/v1/projects/check", { files }),
    uploadProject: (appUuid: string, files: ProjectFile[]) => req<ProjectInfo>("PUT", `/v1/projects/${q(appUuid)}`, { files }, 60),
    syncProject: (appUuid: string) => req<ProjectInfo>("POST", `/v1/projects/${q(appUuid)}/sync`, {}, 60),
    removeProject: (appUuid: string) => req<{ removed: boolean }>("DELETE", `/v1/projects/${q(appUuid)}`),
    runCron: (appUuid: string, path: string) => req<Job>("POST", `/v1/projects/${q(appUuid)}/crons/run`, { path }),
    update: () => req<UpdateStatus>("GET", "/v1/update"),
    updateStart: () => req<UpdateStatus>("POST", "/v1/update", {}),

    workerKey: () => req<CoolifyWorkerKey>("GET", "/v1/coolify/worker-key"),
    authorizeKey: (publicKey: string) => req<AuthorizeKeyResult>("POST", "/v1/ssh/authorized-keys", { public_key: publicKey }),
  };
}

export const agent = makeAgent();

/** Follow a job until it ends (polls, so it works with or without the event stream). */
export async function waitForJob(id: string, onProgress?: (j: Job) => void, intervalMs = 2000, server?: string): Promise<Job> {
  const client = makeAgent(server);
  for (;;) {
    const j = (await client.jobs()).find((x) => x.id === id);
    if (j) {
      onProgress?.(j);
      if (j.status !== "running") return j;
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}
export const agentOn = (server: string) => makeAgent(server);

export interface NewApp {
  project_uuid: string;
  server_uuid: string;
  environment_name: string;
  environment_uuid?: string;
  git_repository: string;
  git_branch: string;
  build_pack: "nixpacks" | "static" | "dockerfile" | "dockercompose";
  ports_exposes: string;
  name: string;
  instant_deploy: boolean;
}

/** Coolify API of one server (default: the selected one). */
function makeCoolify(server?: string) {
  const req = <T>(method: HttpRequest["method"], path: string, body?: unknown, timeout?: number) =>
    call<T>("coolify_request", { request: { method, path, body, timeout }, server });
  return {
    version: () => req<string>("GET", "/version"),
    applications: () => req<CoolifyApplication[]>("GET", "/applications"),
    deployments: (appUuid: string, take = 10) =>
      req<{ count: number; deployments: CoolifyDeployment[] }>("GET", `/deployments/applications/${q(appUuid)}?skip=0&take=${take}`),
    deployment: (uuid: string) => req<CoolifyDeployment>("GET", `/deployments/${q(uuid)}`),
    deploy: (uuid: string, force = false) =>
      req<{ deployments: { deployment_uuid: string; message: string }[] }>("POST", `/deploy?uuid=${q(uuid)}&force=${force}`),
    appAction: (uuid: string, action: "start" | "stop" | "restart") =>
      req<{ message: string; deployment_uuid?: string }>("POST", `/applications/${q(uuid)}/${action}`),
    rollback: (uuid: string, commit: string) =>
      req<{ message: string; deployment_uuid?: string }>("POST", `/applications/${q(uuid)}/rollback`, { commit }),
    /** Comma-separated URLs (`https://app.example.com,https://www.example.com`). */
    setDomains: (uuid: string, domains: string) => req<{ uuid: string }>("PATCH", `/applications/${q(uuid)}`, { domains }, 60),
    appLogs: (uuid: string, lines = 200) => req<{ logs: string }>("GET", `/applications/${q(uuid)}/logs?lines=${lines}`),
    previews: (uuid: string) => req<CoolifyPreview[]>("GET", `/applications/${q(uuid)}/previews`),
    envs: (uuid: string) => req<CoolifyEnv[]>("GET", `/applications/${q(uuid)}/envs`),
    /** Create or update many variables at once (production, not previews). */
    saveEnvs: (uuid: string, vars: { key: string; value: string }[]) =>
      req<unknown>("PATCH", `/applications/${q(uuid)}/envs/bulk`, { data: vars.map((v) => ({ ...v, is_preview: false })) }),
    deletePreview: (uuid: string, pr: number) => req<{ message: string }>("DELETE", `/applications/${q(uuid)}/previews/${pr}`),
    createPublicApp: (body: NewApp) => req<{ uuid: string; domains?: string }>("POST", "/applications/public", body, 60),
    databases: () => req<CoolifyDatabase[]>("GET", "/databases"),
    dbAction: (uuid: string, action: "start" | "stop" | "restart") => req<{ message: string }>("POST", `/databases/${q(uuid)}/${action}`),
    createDatabase: (
      kind: DatabaseKind,
      body: {
      project_uuid: string;
      server_uuid: string;
      environment_name: string;
      environment_uuid?: string;
      name: string;
      instant_deploy: boolean;
    },
    ) => req<{ uuid: string; internal_db_url?: string }>("POST", `/databases/${kind}`, body),
    projects: () => req<CoolifyProject[]>("GET", "/projects"),
    project: (uuid: string) => req<CoolifyProject>("GET", `/projects/${q(uuid)}`),
    createProject: (name: string) => req<{ uuid: string }>("POST", "/projects", { name }),
    servers: () => req<CoolifyServer[]>("GET", "/servers"),
    createServer: (body: {
      name: string;
      description: string;
      ip: string;
      port: number;
      user: string;
      private_key_uuid: string;
      is_build_server: boolean;
      instant_validate: boolean;
    }) => req<{ uuid: string }>("POST", "/servers", body, 60),
  };
}

export const coolify = makeCoolify();
export const coolifyOn = (server: string) => makeCoolify(server);

/** First project + its first environment, creating a project when there is none. */
export async function defaultTarget(
  c = coolify,
): Promise<{ project_uuid: string; environment_name: string; environment_uuid?: string }> {
  const projects = await c.projects();
  let project = projects[0];
  if (!project) project = { uuid: (await c.createProject("default")).uuid, name: "default" };
  const full = await c.project(project.uuid);
  const env = full.environments?.[0];
  // Coolify accepts either; send both when we know the uuid.
  return { project_uuid: project.uuid, environment_name: env?.name ?? "production", ...(env?.uuid ? { environment_uuid: env.uuid } : {}) };
}

export function parseDeploymentLogs(d: CoolifyDeployment): CoolifyDeploymentLog[] {
  if (!d.logs) return [];
  try {
    return (JSON.parse(d.logs) as CoolifyDeploymentLog[]).filter((l) => !l.hidden);
  } catch {
    return [];
  }
}

export const local = {
  system: () => call<SystemSnapshot>("local_system"),
  tailscale: () => call<TailscaleStatus>("local_tailscale"),
  tools: () => call<ToolStatus[]>("tools_status"),
  captureScreens: () => call<CaptureScreen[]>("capture_screens"),
  senderStart: (config: SenderConfig) => call<ProcessStatus>("sender_start", { config }),
  senderStop: () => call<ProcessStatus>("sender_stop"),
  senderStatus: () => call<ProcessStatus>("sender_status"),
  moonlightStart: (launch: MoonlightLaunch) => call<ProcessStatus>("moonlight_start", { launch }),
  moonlightStop: () => call<ProcessStatus>("moonlight_stop"),
  moonlightStatus: () => call<ProcessStatus>("moonlight_status"),
  openCoolify: (path?: string) => call<void>("open_coolify", { path }),
};
