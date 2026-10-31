import { call, streamLines, type HttpRequest } from "./transport";
import type {
  AgentEvent,
  Alert,
  AlertsStatus,
  AuditEntry,
  BackupStatus,
  CaptureScreen,
  CockpitConfig,
  CockpitSettings,
  ContainerInfo,
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
  SunshinePairRequest,
  SunshinePairResult,
  SystemSnapshot,
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
  /** A code `install-mac.sh --pair` left for the first start (read once). */
  pendingPairing: () => call<string | null>("pair_pending"),
  /** Once: why the saved servers are gone (their file couldn't be read). */
  configNotice: () => call<string | null>("config_notice"),
  test: (s: CockpitSettings) =>
    call<{ hostname: string }>("test_connection", { settings: s }),
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
  /** Start (or restart) this computer's vercelify-mesh. */
  start: () => call<void>("mesh_start"),
  /** Servers on this LAN (mDNS). */
  discover: () => call<LanServer[]>("mesh_discover"),
  /** A short code → the pairing, over the LAN; joins the mesh. */
  pair: (code: string, server?: string) => call<Pairing>("mesh_pair", { code, server }),
};

export function pairingToSettings(p: Pairing): CockpitSettings {
  return {
    agent_url: p.agent_url,
    agent_token: p.agent_token,
    hostname: p.hostname,
    role: p.role ?? "main",
  };
}

/** Workers deploy nothing themselves (pairings without a role are main servers). */
export const isWorker = (s: CockpitSettings) => s.role === "worker";

const q = encodeURIComponent;

/** Agent API of one server (`server` = its agent URL; default: the selected one). */
function makeAgent(server?: string) {
  const req = <T>(method: HttpRequest["method"], path: string, body?: unknown, timeout?: number) =>
    call<T>("agent_request", { request: { method, path, body, timeout }, server });
  return {
    system: () => req<SystemSnapshot>("GET", "/v1/system"),
    containers: (stats = true) => req<ContainerInfo[]>("GET", `/v1/containers?stats=${stats}`),
    containerAction: (id: string, action: "start" | "stop" | "restart" | "pause" | "unpause") =>
      req<{ ok: boolean }>("POST", `/v1/containers/${q(id)}/${action}`, {}, 120),
    containerLogs: (id: string, tail = 300) => req<{ lines: string[] }>("GET", `/v1/containers/${q(id)}/logs?tail=${tail}`),
    volumes: () => req<VolumeEntry[]>("GET", "/v1/volumes"),
    createVolume: (r: VolumeRequest) => req<VolumeResult>("POST", "/v1/volumes", r),
    display: () => req<DisplayStatus>("GET", "/v1/display"),
    receiverStart: (cfg: ReceiverConfig) => req<DisplayStatus>("POST", "/v1/display/receiver/start", cfg),
    receiverStop: () => req<DisplayStatus>("POST", "/v1/display/receiver/stop", {}),
    /** Hands Sunshine the PIN Moonlight was started with; waits up to 25 s for Moonlight to ask. */
    sunshinePair: (pair: SunshinePairRequest) => req<SunshinePairResult>("POST", "/v1/display/sunshine/pair", pair, 60),
    services: () => req<ServiceStatus[]>("GET", "/v1/services"),
    serviceAction: (name: string, action: "start" | "stop" | "restart") =>
      req<{ ok: boolean }>("POST", `/v1/services/${q(name)}/${action}`, {}, 90),

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
    /** The coordinator's view; `reachable_from_anywhere` once `vercelify remote on` gave it a public address. */
    meshStatus: () => req<{ coordinator: { reachable_from_anywhere?: boolean } | null }>("GET", "/v1/mesh/status"),
    /** Take a node off the mesh; its peers drop it within seconds. */
    meshRevoke: (id: string) => req<{ revoked: string }>("DELETE", `/v1/mesh/nodes/${q(id)}`, undefined, 20),
    meshInvite: (kind: "server" | "device", role = "owner") =>
      req<MeshInvite>("POST", "/v1/mesh/invites", { kind, role, ttl_minutes: 60 }),
    audit: (limit = 50) => req<AuditEntry[]>("GET", `/v1/audit?limit=${limit}`),
    backup: () => req<BackupStatus>("GET", "/v1/backup"),
    backupRun: () => req<BackupStatus>("POST", "/v1/backup/run", {}),
    /** Owner-only: the backup password and repositories, to keep off the server. */
    backupRecoveryKit: () => req<{ filename: string; text: string }>("POST", "/v1/backup/recovery-kit", {}),
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
    updateStart: (channel?: "edge" | "stable") => req<UpdateStatus>("POST", "/v1/update", channel ? { channel } : {}),
    /** `vercelify desktop on` on the server (Sunshine for remote desktop). */
    desktopOn: () => req<UpdateStatus>("POST", "/v1/desktop/on", {}),
    desktopOnStatus: () => req<UpdateStatus>("GET", "/v1/desktop/on"),
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

export const local = {
  system: () => call<SystemSnapshot>("local_system"),
  tools: () => call<ToolStatus[]>("tools_status"),
  /** Save a secret file for the user (0600 in Downloads); returns where it went. */
  savePrivateFile: (filename: string, text: string) => call<string>("save_private_file", { filename, text }),
  captureScreens: () => call<CaptureScreen[]>("capture_screens"),
  senderStart: (config: SenderConfig) => call<ProcessStatus>("sender_start", { config }),
  senderStop: () => call<ProcessStatus>("sender_stop"),
  /** A new display for an extended desktop (macOS); removed when the stream stops. */
  extendDisplayStart: (width: number, height: number, fps: number) => call<CaptureScreen>("extend_display_start", { width, height, fps }),
  extendDisplayStop: () => call<void>("extend_display_stop"),
  senderStatus: () => call<ProcessStatus>("sender_status"),
  moonlightSetup: (host: string) => call<string>("moonlight_setup", { host }),
  moonlightStart: (launch: MoonlightLaunch) => call<ProcessStatus>("moonlight_start", { launch }),
  moonlightPair: (host: string, pin: string) => call<ProcessStatus>("moonlight_pair", { host, pin }),
  moonlightStop: () => call<ProcessStatus>("moonlight_stop"),
  moonlightStatus: () => call<ProcessStatus>("moonlight_status"),
};
