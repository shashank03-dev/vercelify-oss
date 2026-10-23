// Demo backend for `VITE_DEMO=1 npm run dev`: realistic data for every
// screen, so the UI can be designed, reviewed and screenshotted without a
// host. Only loaded when VITE_DEMO is set (tree-shaken from real builds).

import type {
  AgentEvent,
  Job,
  PowerGuardSettings,
  ProjectInfo,
  ProjectSummary,
  SleepSetting,
  Alert,
  AuditEntry,
  AutoDeployWatch,
  BackupStatus,
  DeviceToken,
  MetricPoint,
  UptimeCheck,
  DoctorCheck,
  DoctorReport,
  UpdateStatus,
  ContainerInfo,
  CoolifyServer,
  DiscoveredServer,
  LlmModel,
  LlmStatus,
  LlmTask,
  CoolifyApplication,
  CoolifyDatabase,
  CoolifyDeployment,
  CoolifyEnv,
  CoolifyVolume,
  ServiceStatus,
  SystemSnapshot,
  TailscaleStatus,
  VolumeEntry,
  LlmChatMessage,
} from "../types";
import type { HttpRequest } from "./transport";
import { isExplainChat } from "./explain";
import failedBuildLog from "../../../cli/examples/failed-build.log?raw";

const GB = 1024 ** 3;
const now = Date.now();
const iso = (minsAgo: number) => new Date(now - minsAgo * 60_000).toISOString();

const apps: CoolifyApplication[] = [
  app("a1", "portfolio", "running:healthy", "https://shashank.dev", "shashank03-dev/portfolio", "main", "nixpacks", 4),
  app("a2", "notes-api", "running:healthy", "https://api.notes.shashank.dev", "shashank03-dev/notes-api", "main", "dockerfile", 38),
  app("a3", "blog", "running:healthy", "https://blog.shashank.dev", "shashank03-dev/blog", "main", "static", 220),
  app("a4", "dashboard", "restarting:unknown", "https://dash.shashank.dev", "shashank03-dev/dashboard", "dev", "nixpacks", 1),
  app("a5", "discord-bot", "exited:unhealthy", null, "shashank03-dev/discord-bot", "main", "dockerfile", 1440),
  app("a6", "umami", "running:healthy", "https://stats.shashank.dev", null, null, "dockerimage", 4300),
];

function app(
  uuid: string,
  name: string,
  status: string,
  fqdn: string | null,
  repo: string | null,
  branch: string | null,
  build_pack: string,
  minsAgo: number,
): CoolifyApplication {
  return {
    uuid,
    name,
    status,
    fqdn,
    git_repository: repo ? `https://github.com/${repo}` : null,
    git_branch: branch,
    build_pack,
    docker_registry_image_name: repo ? null : "ghcr.io/umami-software/umami",
    docker_registry_image_tag: repo ? null : "postgresql-latest",
    created_at: iso(60 * 24 * 30),
    updated_at: iso(minsAgo),
    environment_id: 1,
  };
}

const MESSAGES = [
  "Add dark mode toggle to the header",
  "Fix flaky image upload on Safari",
  "Bump next to 15.4",
  "Cache API responses for 60s",
  "Rewrite landing copy",
  "Initial commit",
];

// The seeded Next.js build that fails on Linux (cli/examples, also used by `vfy why --log`).
const FAILED_BUILD = failedBuildLog.trimEnd().split("\n");

function deployments(appUuid: string): CoolifyDeployment[] {
  const sha = (i: number) => (appUuid.charCodeAt(1) * 7919 * (i + 3)).toString(16).padEnd(40, "a3f9");
  const list: CoolifyDeployment[] = [];
  const building = appUuid === "a4";
  list.push(dep(appUuid, 0, building ? "in_progress" : "finished", sha(0), MESSAGES[0]!, 0, 2, building));
  list.push(dep(appUuid, 1, "finished", sha(1), "Preview: new pricing page", 42, 50));
  list.push(dep(appUuid, 2, "failed", sha(2), MESSAGES[1]!, 0, 180));
  list.push(dep(appUuid, 3, "finished", sha(3), MESSAGES[2]!, 0, 600));
  list.push(dep(appUuid, 4, "finished", sha(4), MESSAGES[3]!, 0, 2900));
  list.push(dep(appUuid, 5, "finished", sha(5), MESSAGES[4]!, 0, 8000));
  return list;
}

function dep(app: string, i: number, status: string, commit: string, msg: string, pr: number, minsAgo: number, live = false): CoolifyDeployment {
  const created = iso(minsAgo);
  return {
    id: i,
    deployment_uuid: `${app}-d${i}`,
    status,
    commit,
    commit_message: msg,
    pull_request_id: pr,
    is_webhook: i % 3 !== 2,
    is_api: i % 3 === 2,
    created_at: created,
    updated_at: created,
    finished_at: live ? null : new Date(Date.parse(created) + (48 + i * 17) * 1000).toISOString(),
    logs: JSON.stringify(
      (status === "failed" ? FAILED_BUILD : [
        "Starting deployment of shashank03-dev/portfolio:main",
        "Pulling repository… done",
        "Nixpacks detected Node.js 22 (npm)",
        "npm ci — added 412 packages in 9s",
        "npm run build",
        "  ✓ Compiled successfully in 14.2s",
        "Building docker image… done (sha256:9f2c…)",
        "Rolling update: starting new container",
        "Healthcheck passed",
        "Deployment finished.",
      ]).map((output, n) => ({ output, type: n === 3 && status !== "failed" ? "stderr" : "stdout", timestamp: created, hidden: false })),
    ),
  };
}

const dbs: CoolifyDatabase[] = [
  db("d1", "pg-main", "standalone-postgresql", "postgres:16-alpine", "running:healthy"),
  db("d2", "cache", "standalone-redis", "redis:7.4-alpine", "running:healthy"),
  db("d3", "analytics", "standalone-clickhouse", "clickhouse/clickhouse-server:24", "exited:unknown"),
];

function db(uuid: string, name: string, type: string, image: string, status: string): CoolifyDatabase {
  return {
    uuid,
    name,
    status,
    database_type: type,
    image,
    internal_db_url: `${type.replace("standalone-", "")}://user:••••@${uuid}:5432/${name}`,
    is_public: false,
    public_port: null,
    environment_id: 1,
  };
}

let cpu = 18;
/** Demo servers: the same canned data, with a few differences per machine. */
const DEMO_HOSTS: Record<string, { hostname: string; cores: number; mem: number; load: number }> = {
  "thinkpad-t470": { hostname: "thinkpad-t470", cores: 4, mem: 8, load: 1 },
  "dell-e7450": { hostname: "dell-e7450", cores: 4, mem: 16, load: 0.5 },
  "pi5-worker": { hostname: "pi5-worker", cores: 4, mem: 8, load: 0.3 },
  "hp-elitebook": { hostname: "hp-elitebook", cores: 4, mem: 4, load: 0.2 },
};
function hostFor(server: unknown) {
  const name = typeof server === "string" ? (/\/\/([^.:/]+)/.exec(server)?.[1] ?? "") : "";
  return DEMO_HOSTS[name] ?? DEMO_HOSTS["thinkpad-t470"]!;
}
function system(server?: unknown): SystemSnapshot {
  cpu = Math.max(4, Math.min(92, cpu + (Math.random() - 0.48) * 14));
  const h = hostFor(server);
  const snap = systemBase();
  snap.host.hostname = h.hostname;
  snap.cpu.usage_pct = cpu * h.load;
  snap.memory.total_bytes = h.mem * GB;
  snap.memory.available_bytes = (h.mem - 4.8 * h.load) * GB;
  snap.memory.used_bytes = (4.8 * h.load + Math.random() * 0.3) * GB;
  if (h.hostname === "pi5-worker") {
    snap.host.arch = "aarch64";
    snap.cpu.model = "Cortex-A76";
    snap.disks = [{ ...snap.disks[0]!, device: "/dev/mmcblk0p2", media: "sd_card", total_bytes: 58 * GB, available_bytes: 31 * GB, used_bytes: 27 * GB }];
    snap.power = { ac_online: null, batteries: [], on_battery: false, lowest_capacity_pct: null };
    snap.hardware = {
      profile: "pi5",
      model: "Raspberry Pi 5 Model B Rev 1.0",
      root_media: "sd_card",
      memory_cgroup: true,
      throttling: { raw: 0x50000, under_voltage_now: false, throttled_now: false, soft_temp_limit_now: false, under_voltage_since_boot: true, throttled_since_boot: true },
      pressure: { cpu_some_avg10: 3.1, memory_some_avg10: 0.4, io_some_avg10: 6.2 },
      oom_kills: 0,
      watchdog: true,
      defaults_pause_c: 78,
    };
  }
  if (h.hostname === "hp-elitebook") snap.disks = snap.disks.slice(0, 1);
  return snap;
}
function systemBase(): SystemSnapshot {
  return {
    host: { hostname: "thinkpad-t470", os: "Debian GNU/Linux 12", kernel: "6.1.0-28-amd64", arch: "x86_64", uptime_secs: 9 * 86400 + 4 * 3600, agent_version: "0.1.0" },
    cpu: { model: "Intel(R) Core(TM) i5-7300U CPU @ 2.60GHz", cores: 4, usage_pct: cpu, load_avg: [0.82, 0.71, 0.64] },
    memory: { total_bytes: 8 * GB, used_bytes: (4.6 + Math.random() * 0.3) * GB, available_bytes: 3.2 * GB, swap_total_bytes: 2 * GB, swap_used_bytes: 0.18 * GB },
    disks: [
      { device: "/dev/nvme0n1p2", mount_point: "/", file_system: "ext4", total_bytes: 238 * GB, available_bytes: 141 * GB, used_bytes: 97 * GB, role: "system", media: "ssd", removable: false },
      { device: "/dev/sda1", mount_point: "/mnt/storage", file_system: "ext4", total_bytes: 931 * GB, available_bytes: 802 * GB, used_bytes: 129 * GB, role: "storage", media: "hdd", removable: false },
    ],
    power: {
      ac_online: true,
      batteries: [{ name: "BAT0", capacity_pct: 80, status: "Not charging", health_pct: 71, charge_stop_threshold: 80 }],
      on_battery: false,
      lowest_capacity_pct: 80,
    },
    thermal: {
      max_c: 54,
      sensors: [
        { name: "Package id 0", temp_c: 54 },
        { name: "Core 0", temp_c: 52 },
        { name: "Core 1", temp_c: 51 },
        { name: "nvme Composite", temp_c: 41 },
        { name: "acpitz", temp_c: 46 },
      ],
    },
    power_guard: { state: "ok", reason: "On AC power, battery held at 80% to protect it.", updated_at: iso(1) },
    hardware: {
      profile: "laptop",
      model: "",
      root_media: "ssd",
      memory_cgroup: true,
      throttling: null,
      pressure: { cpu_some_avg10: 1.2, memory_some_avg10: 0.1, io_some_avg10: 0.8 },
      oom_kills: 0,
      watchdog: true,
      defaults_pause_c: 92,
    },
  };
}

/** 24 hours of minute samples with a daily shape and a little noise. */
function metrics(server: unknown): MetricPoint[] {
  const h = hostFor(server);
  const out: MetricPoint[] = [];
  const end = Math.floor(now / 60_000) * 60;
  for (let i = 24 * 60 - 1; i >= 0; i--) {
    const t = end - i * 60;
    const day = Math.sin(((t / 3600) % 24) / 24 * Math.PI * 2 - 1.2);
    const wobble = Math.sin(t / 900) * 4 + Math.sin(t / 137) * 2;
    const build = i % 180 < 6 ? 35 : 0;
    out.push({
      t,
      cpu_pct: Math.max(2, (18 + day * 10 + wobble + build) * h.load),
      mem_used_pct: Math.min(96, 52 + day * 6 + (build ? 12 : 0) + h.load * 4),
      swap_used_pct: 4 + (build ? 6 : 0),
      disk_root_pct: 40.6 + (24 * 60 - i) / 8000,
      load1: (0.6 + day * 0.3 + (build ? 1.4 : 0)) * h.load,
      temp_c: 50 + day * 5 + (build ? 14 : 0),
      mem_pressure: build ? 8 : 0.2,
    });
  }
  return out;
}

const alerts: Alert[] = [
  { id: 4, at: iso(38), source: "deploy:a4", status: "bad", title: "dashboard: deploy of 9f2c1e0 failed", detail: "The previous version keeps running. See the build log in the app's Deployments.", resolved: false, notified: true },
  { id: 3, at: iso(190), source: "rollback:a2", status: "warn", title: "notes-api: rolled back to 4b1d2e7", detail: "c93a0f1 was running:unhealthy after deploying; the previous version is going back live.", resolved: false, notified: true },
  { id: 2, at: iso(60 * 9), source: "temperature", status: "ok", title: "Temperature: back to normal", detail: "61 °C (builds pause at 92 °C)", resolved: true, notified: true },
  { id: 1, at: iso(60 * 9 + 12), source: "temperature", status: "warn", title: "Temperature: warning", detail: "84 °C (builds pause at 92 °C) — clean the fan and vents", resolved: false, notified: true },
];
let notifyTarget: string | null = "https://ntfy.sh/•••";

const tokens: DeviceToken[] = [
  { id: "3f9a1c2e", name: "paired 2026-10-12 21:04 UTC", created_at: iso(60 * 24 * 8), last_used_at: iso(0.2), current: true },
  { id: "b71d08aa", name: "iPad (status only)", created_at: iso(60 * 24 * 30), last_used_at: iso(60 * 24 * 3), current: false },
];

const audit: AuditEntry[] = [
  { at: iso(4), actor: "3f9a1c2e", method: "POST", path: "/v1/autodeploy/a1/check", status: 200 },
  { at: iso(38), actor: "admin", method: "POST", path: "/v1/update", status: 200 },
  { at: iso(95), actor: "3f9a1c2e", method: "POST", path: "/v1/containers/redis-cache/restart", status: 200 },
];

const backup: BackupStatus = {
  configured: true,
  targets: ["/mnt/storage/backups/restic", "sftp:root@pi5-worker:/mnt/storage/backups/thinkpad-t470"],
  last_ok: true,
  last_finished_at: iso(60 * 7),
  last_error: null,
  running: false,
  log_tail: ["==> Backup 2026-10-20T03:22:41Z", "  • dumped 3 database(s)", "  ✓ snapshot 7c1e9a42 in /mnt/storage/backups/restic", "  ✓ backup complete"],
};

function uptime(): UptimeCheck[] {
  return [
    ...apps
      .filter((a) => a.fqdn && !a.status.startsWith("exited"))
      .map((a) => ({
        id: `app:${a.uuid}`,
        name: `App ${a.name}`,
        target: a.fqdn!.split(",")[0]!,
        up: !a.status.includes("unhealthy"),
        http_status: a.status.includes("unhealthy") ? 502 : 200,
        latency_ms: a.status.includes("unhealthy") ? 31 : 40 + a.name.length * 9,
        checked_at: iso(0.3),
        since: iso(a.status.includes("unhealthy") ? 12 : 60 * 26),
        error: null,
      })),
    { id: "server:pi5-worker", name: "Server pi5-worker", target: "http://100.77.3.9:7420/v1/health", up: true, http_status: 200, latency_ms: 18, checked_at: iso(0.3), since: iso(60 * 50), error: null },
  ];
}

const node = (id: string, hostname: string, os: string, ip: string, online: boolean, direct: boolean) => ({
  id,
  hostname,
  dns_name: `${hostname}.tail1234.ts.net`,
  os,
  ips: [ip, "fd7a:115c:a1e0::1"],
  online,
  active: online,
  exit_node: false,
  relay: "blr",
  cur_addr: direct ? "192.168.1.20:41641" : "",
  rx_bytes: 0,
  tx_bytes: 0,
  last_seen: null,
  // Tagged/expiry-disabled nodes have none; this one shows the countdown.
  key_expiry: id === "1" ? iso(-60 * 24 * 132) : null,
});

const tailscale: TailscaleStatus = {
  installed: true,
  backend_state: "Running",
  version: "1.84.2-t1234",
  tailnet: "shashank03-dev.github",
  magic_dns_suffix: "tail1234.ts.net",
  auth_url: "",
  self_node: node("1", "thinkpad-t470", "linux", "100.101.102.103", true, true),
  peers: [
    node("2", "macbook-air", "macOS", "100.88.12.4", true, true),
    node("3", "pi5-worker", "linux", "100.77.3.9", true, false),
    node("4", "iphone", "iOS", "100.66.1.2", false, false),
  ],
  health: [],
  error: null,
};

const containers: ContainerInfo[] = [
  ["coolify", "ghcr.io/coollabsio/coolify:4.0.0", "running", 2.1, 0.41],
  ["coolify-proxy", "traefik:v3.3", "running", 0.4, 0.06],
  ["portfolio-a1", "a1:9f2c1e0", "running", 1.2, 0.12],
  ["notes-api-a2", "a2:77ab210", "running", 3.8, 0.21],
  ["pg-main-d1", "postgres:16-alpine", "running", 0.9, 0.18],
  ["cache-d2", "redis:7.4-alpine", "running", 0.2, 0.02],
  ["discord-bot-a5", "a5:1b9e4c2", "exited", null, null],
].map(([name, image, state, c, m]) => ({
  id: String(name),
  name: String(name),
  image: String(image),
  state: String(state),
  status: state === "running" ? "Up 9 days (healthy)" : "Exited (1) 1 day ago",
  ports: "",
  cpu_pct: c as number | null,
  mem_bytes: m == null ? null : (m as number) * GB,
  mem_pct: null,
  coolify_name: String(name).startsWith("coolify") ? null : String(name).replace(/-[ad]\d$/, ""),
  coolify_managed: !String(name).startsWith("coolify"),
}));

const services: ServiceStatus[] = [
  { name: "docker", active_state: "active", enabled: true },
  { name: "tailscaled", active_state: "active", enabled: true },
  { name: "vercelify-agent", active_state: "active", enabled: true },
  { name: "vercelify-power-guard", active_state: "active", enabled: true },
  { name: "cloudflared", active_state: "active", enabled: true },
  { name: "sunshine", active_state: "not-found", enabled: null },
];

const volumes: CoolifyVolume[] = [
  vol(1, "postgres-data-d1", "/var/lib/postgresql/data", "hdd", "StandalonePostgresql", "pg-main", "d1"),
  vol(2, "redis-data-d2", "/data", "ssd", "StandaloneRedis", "cache", "d2"),
  vol(3, "uploads-a2", "/app/uploads", "ssd", "Application", "notes-api", "a2"),
  vol(4, "clickhouse-data-d3", "/var/lib/clickhouse", "hdd", "StandaloneClickhouse", "analytics", "d3"),
];

function vol(id: number, name: string, mount: string, tier: "ssd" | "hdd", type: string, res: string, uuid: string): CoolifyVolume {
  return {
    id,
    name,
    mount_path: mount,
    host_path: tier === "hdd" ? `/mnt/storage/volumes/${res}` : null,
    tier,
    data_path: tier === "hdd" ? `/mnt/storage/volumes/${res}` : `/var/lib/docker/volumes/${name}/_data`,
    resource_type: type,
    resource_name: res,
    resource_uuid: uuid,
    movable: true,
    in_use: true,
  };
}

const hostVolumes: VolumeEntry[] = [
  ...volumes.map((v) => ({ tier: v.tier, name: v.name, host_path: v.data_path, size_bytes: null })),
  { tier: "ssd", name: "coolify-db", host_path: "/var/lib/docker/volumes/coolify-db/_data", size_bytes: null },
];

const idle = { running: false, pid: null, command: null, started_at: null, last_output: [], exit_code: null };

// --- AI models -----------------------------------------------------------------

const llmTasks: LlmTask[] = [];
const llmModels: LlmModel[] = [
  { name: "gemma4:e2b", size_bytes: 7.2 * GB, modified_at: iso(60 * 6), family: "gemma4", parameter_size: null, quantization: null },
  { name: "llama3.2:3b", size_bytes: 2.0 * GB, modified_at: iso(60 * 30), family: "llama", parameter_size: "3.2B", quantization: "Q4_K_M" },
  { name: "qwen2.5-coder:1.5b", size_bytes: 0.96 * GB, modified_at: iso(60 * 50), family: "qwen2", parameter_size: "1.5B", quantization: "Q4_K_M" },
];

function llm(server?: unknown): LlmStatus {
  const h = hostFor(server);
  const installed = h.hostname !== "pi5-worker";
  // advance fake downloads
  for (const t of llmTasks) {
    if (t.done) continue;
    t.completed = Math.min(t.total ?? 0, (t.completed ?? 0) + 180 * 1024 ** 2);
    t.status = `pulling ${t.target}`;
    if (t.completed === t.total) {
      t.done = true;
      t.status = "success";
      llmModels.push({ name: t.target, size_bytes: t.total ?? 0, modified_at: iso(0), family: null, parameter_size: null, quantization: "Q4_K_M" });
    }
  }
  return {
    installed,
    running: installed,
    ready: installed,
    container_state: installed ? "running" : null,
    image: "ollama/ollama:latest",
    version: installed ? "0.12.3" : null,
    port: 11434,
    endpoints: [`http://${h.hostname}.tail1234.ts.net:11434`, "http://100.101.102.103:11434"],
    data_path: "/mnt/storage/volumes/ollama/models",
    models: installed ? llmModels.map((m) => ({ ...m })) : [],
    loaded: installed ? [{ name: "llama3.2:3b", size_bytes: 2.9 * GB, expires_at: iso(-4) }] : [],
    tasks: llmTasks.map((t) => ({ ...t })),
    firewall_active: true,
    memory_total_bytes: h.mem * GB,
    memory_available_bytes: (h.mem - 4.8 * h.load) * GB,
    error: null,
  };
}

const watches: AutoDeployWatch[] = [
  watch("a1", "portfolio", "shashank03-dev/portfolio", "main", 7, null),
  watch("a2", "notes-api", "shashank03-dev/notes-api", "main", 3, null),
  watch("a4", "dashboard", "shashank03-dev/dashboard", "dev", 12, "git ls-remote failed: could not read Username — is the repository private? Add a read-only access token."),
];

function watch(app_uuid: string, name: string, repo: string, branch: string, deploys: number, last_error: string | null): AutoDeployWatch {
  return {
    app_uuid,
    name,
    repo: `https://github.com/${repo}.git`,
    branch,
    enabled: true,
    has_token: false,
    last_commit: "9f2c1e0a3f9a3f9a3f9a3f9a3f9a3f9a3f9a3f9a",
    last_checked_at: iso(0.4),
    last_deployed_commit: "9f2c1e0a3f9a3f9a3f9a3f9a3f9a3f9a3f9a3f9a",
    last_deployed_at: iso(4),
    last_error,
    deploys,
    previews: app_uuid === "a1",
    auto_rollback: app_uuid !== "a4",
    github_feedback: app_uuid === "a1",
    open_previews:
      app_uuid === "a1"
        ? [{ number: 42, title: "Dark mode for the blog", head_sha: "c4e1f7a9b2c4e1f7a9b2c4e1f7a9b2c4e1f7a9b2", url: "https://pr-42-portfolio.example.com", deployed_at: iso(25) }]
        : [],
    rollbacks: app_uuid === "a2" ? 1 : 0,
    skipped: app_uuid === "a1" ? 3 : 0,
    last_skip: app_uuid === "a1" ? "4b1d2e7: no changed file is in the app's watch paths" : null,
  };
}

const coolifyServers: CoolifyServer[] = [
  { uuid: "s0", name: "localhost", ip: "host.docker.internal", settings: { is_reachable: true, is_usable: true } },
  { uuid: "s1", name: "pi5-worker", ip: "100.77.3.9", settings: { is_reachable: true, is_usable: true } },
];

const DISCOVERED: DiscoveredServer[] = [
  { hostname: "dell-e7450", dns_name: "dell-e7450.tail1234.ts.net", ip: "100.90.4.2", os: "linux", agent_url: "http://100.90.4.2:7420", version: "0.2.0", role: "main", pairing_open: true },
  { hostname: "pi5-worker", dns_name: "pi5-worker.tail1234.ts.net", ip: "100.77.3.9", os: "linux", agent_url: "http://100.77.3.9:7420", version: "0.2.0", role: "worker", pairing_open: false },
  { hostname: "thinkpad-t470", dns_name: "thinkpad-t470.tail1234.ts.net", ip: "100.101.102.103", os: "linux", agent_url: "http://100.101.102.103:7420", version: "0.2.0", role: "main", pairing_open: false },
];

// A machine that comes online while the "Add a machine" wizard waits for it.
let discoverCalls = 0;
const NEW_MACHINE: DiscoveredServer = {
  hostname: "hp-elitebook",
  dns_name: "hp-elitebook.tail1234.ts.net",
  ip: "100.70.8.1",
  os: "linux",
  agent_url: "http://100.70.8.1:7420",
  version: "0.2.0",
  role: "worker",
  pairing_open: true,
};

function doctor(server?: unknown): DoctorReport {
  const h = hostFor(server);
  const worker = h.hostname === "pi5-worker" || h.hostname === "hp-elitebook";
  const c = (id: string, label: string, status: DoctorCheck["status"], detail: string, fix: string | null = null): DoctorCheck => ({ id, label, status, detail, fix });
  const checks = [
    c("docker", "Docker", "ok", "Docker 27.3.1"),
    c("tailscale", "Tailscale", "ok", `${h.hostname}.tail1234.ts.net`),
    ...(worker ? [] : [c("coolify", "Coolify", "ok", "dashboard and API are up"), c("coolify-token", "Coolify API token", "ok", "accepted")]),
    c("disk", "System disk", "ok", "41% used, 141.0 GB free"),
    worker
      ? c("storage", "Storage disk", "warn", "single disk: everything lives on the system disk", "add an HDD and re-run the installer to get a storage tier")
      : c("storage", "Storage disk", "ok", "mounted"),
    c("memory", "Memory", "ok", `${(h.mem - 4.8 * h.load).toFixed(1)} GB free of ${h.mem}.0 GB`),
    c("temperature", "Temperature", "ok", "54 °C"),
    c("power-guard", "Power guard", "ok", "ok"),
    c("firewall", "Firewall", "ok", "dashboard and AI API private to the tailnet"),
  ];
  return { role: worker ? "worker" : "main", agent_version: "0.2.0", checks };
}

const updates: Record<string, { polls: number }> = {};
function update(server: unknown, start: boolean): UpdateStatus {
  const key = String(server);
  if (start) updates[key] = { polls: 0 };
  const u = updates[key];
  if (!u) return { running: false, started_at: null, exit_code: null, log_tail: [] };
  u.polls++;
  const steps = ["==> Downloading Vercelify (main)", "==> Base packages", "==> Docker Engine", "==> Vercelify agent", "  ✓ agent 0.2.0 running on :7420", "✔ Vercelify is ready"];
  const n = Math.min(steps.length, u.polls);
  return { running: n < steps.length, started_at: iso(0), exit_code: n < steps.length ? null : 0, log_tail: steps.slice(0, n) };
}

function agentRoute(req: HttpRequest, server?: unknown): unknown {
  const p = req.path.split("?")[0]!;
  if (p === "/v1/health") {
    const h = hostFor(server);
    return {
      ok: true,
      version: "0.2.0",
      hostname: h.hostname,
      role: h.hostname === "pi5-worker" || h.hostname === "hp-elitebook" ? "worker" : "main",
      pairing_open: false,
      api_version: 2,
      capabilities: [
        "doctor", "update", "rollback", "hardware", "alerts", "metrics", "uptime", "tokens", "audit", "backup", "previews",
        "auto-rollback", "images", "events", "jobs", "sleep", "power-guard", "domain", "projects",
      ],
    };
  }
  if (p === "/v1/jobs") return jobs.map((j) => ({ ...j }));
  if (p === "/v1/sleep" && req.method === "GET") return sleepStatus();
  if (p.startsWith("/v1/sleep/") && req.method === "PUT") {
    const uuid = p.split("/")[3]!;
    const b = req.body as { enabled: boolean; idle_minutes?: number; previews?: boolean };
    const i = sleepSettings.findIndex((x) => x.app_uuid === uuid);
    if (i >= 0) sleepSettings.splice(i, 1);
    if (b.enabled) sleepSettings.push({ app_uuid: uuid, enabled: true, idle_minutes: b.idle_minutes ?? 30, previews: b.previews ?? false });
    return sleepStatus();
  }
  if (p === "/v1/power-guard") {
    if (req.method === "PUT") pgSettings = { ...(req.body as PowerGuardSettings) };
    if (req.method === "DELETE") pgSettings = { ...PG_DEFAULTS };
    return {
      active: true,
      state: { state: "ok", reason: "AC power, CPU 52°C", updated_at: iso(0) },
      settings: pgSettings,
      defaults: PG_DEFAULTS,
      customized: JSON.stringify(pgSettings) !== JSON.stringify(PG_DEFAULTS),
    };
  }
  if (p === "/v1/projects") return { projects: projects.map((x) => structuredClone(x)), proxy_image: "traefik:v3.7", error: null };
  if (p === "/v1/projects/check") return demoSummary((req.body as { files: { content: string }[] }).files);
  let pm = /^\/v1\/projects\/([^/]+)\/crons\/run$/.exec(p);
  if (pm) {
    const info = projects.find((x) => x.key === pm![1]);
    const c = info?.summary.crons.find((x) => x.path === (req.body as { path: string }).path);
    if (!info || !c) throw new Error("no cron job for this path");
    const job = startJob("cron", `Run ${c.path}`, info.app_uuid);
    c.running = true;
    setTimeout(() => {
      Object.assign(c, { running: false, last_run_at: iso(0), last_status: 200, last_error: null, last_duration_ms: 182 });
      updateJob(job, { status: "ok", detail: "HTTP 200 in 182 ms", finished_at: iso(0) });
      emit("changed", { topic: "projects" });
    }, 1200);
    return job;
  }
  pm = /^\/v1\/projects\/([^/]+)\/sync$/.exec(p);
  if (pm) {
    const info = projects.find((x) => x.key === pm![1]);
    if (!info || info.source !== "repo") throw new Error("no vercel.json or vercelify.json on main of shashank03-dev/" + (apps.find((a) => a.uuid === pm![1])?.name ?? "app"));
    info.updated_at = iso(0);
    return structuredClone(info);
  }
  pm = /^\/v1\/projects\/([^/]+)$/.exec(p);
  if (pm && req.method === "DELETE") {
    const i = projects.findIndex((x) => x.key === pm![1]);
    if (i < 0) throw new Error("this app has no project configuration");
    projects.splice(i, 1);
    return { removed: true };
  }
  if (pm && req.method === "PUT") {
    const files = (req.body as { files: { name: string; content: string }[] }).files;
    const info: ProjectInfo = {
      key: pm[1]!,
      app_uuid: pm[1]!,
      pull_request_id: 0,
      source: "upload",
      files: files.map((f) => f.name),
      commit: null,
      updated_at: iso(0),
      summary: demoSummary(files),
      routes_active: true,
      hosts: [(apps.find((a) => a.uuid === pm![1])?.fqdn ?? "").replace(/^https?:\/\//, "")],
      error: null,
    };
    const i = projects.findIndex((x) => x.key === info.key);
    if (i >= 0) projects.splice(i, 1, info);
    else projects.push(info);
    return structuredClone(info);
  }
  if (p === "/v1/domain") return { mode: "cloudflare", domain: "shashank.dev", wildcard: "http://shashank.dev", tunnel_active: true };
  if (p === "/v1/alerts" || p === "/v1/alerts/check") return { notify_target: notifyTarget, alerts };
  if (p === "/v1/alerts/notify") {
    const url = String((req.body as { url?: string })?.url ?? "");
    notifyTarget = url ? url.replace(/^(https?:\/\/[^/]+).*$/, "$1/•••") : null;
    return { notify_target: notifyTarget, alerts };
  }
  if (p === "/v1/alerts/test") {
    const a: Alert = { id: alerts.length + 10, at: new Date().toISOString(), source: "test", status: "ok", title: "Test notification", detail: "Alerts from this server reach you.", resolved: true, notified: true };
    alerts.unshift(a);
    return a;
  }
  if (p === "/v1/metrics") return { interval_secs: 60, points: metrics(server) };
  if (p === "/v1/uptime") return uptime();
  if (p === "/v1/tokens" && req.method === "GET") return tokens;
  if (p.startsWith("/v1/tokens/") && req.method === "DELETE") {
    const i = tokens.findIndex((t) => p.endsWith(t.id));
    if (i >= 0) tokens.splice(i, 1);
    return { ok: true };
  }
  if (p === "/v1/audit") return audit;
  if (p.startsWith("/v1/images/")) return { repository: p.split("/").pop(), tags: ["9f2c1e0a3f9a3f9a3f9a3f9a3f9a3f9a3f9a3f9a", "4b1d2e7", "latest"] };
  if (p === "/v1/backup") return backup;
  if (p === "/v1/backup/run") {
    backup.running = true;
    const job = startJob("backup", "Backup", null);
    setTimeout(() => updateJob(job, { detail: "dumping 2 databases", progress: 40 }), 1500);
    setTimeout(() => {
      backup.running = false;
      backup.last_finished_at = new Date().toISOString();
      updateJob(job, { status: "ok", detail: "backed up", progress: 100, finished_at: iso(0) });
    }, 4000);
    return backup;
  }
  if (p === "/v1/doctor") return doctor(server);
  if (p === "/v1/update") return update(server, req.method === "POST");
  if (p === "/v1/system") return system(server);
  if (p === "/v1/llm") return llm(server);
  if (p === "/v1/llm/models/pull") {
    const model = String((req.body as { model?: string })?.model ?? "");
    const size = 1.9 * GB;
    llmTasks.push({ kind: "pull", target: model, status: "pulling manifest", completed: 0, total: size, done: false, error: null, started_at: iso(0) });
    return llm(server);
  }
  if (p === "/v1/llm/models/delete") {
    const model = String((req.body as { model?: string })?.model ?? "");
    const i = llmModels.findIndex((m) => m.name === model);
    if (i >= 0) llmModels.splice(i, 1);
    return { ok: true };
  }
  if (p.startsWith("/v1/llm")) return llm(server);
  if (p === "/v1/autodeploy" && req.method === "GET") return { interval_secs: 60, coolify_configured: true, watches: watches.map((w) => ({ ...w })) };
  if (p === "/v1/autodeploy") {
    const b = req.body as { app_uuid: string; name?: string; repo: string; branch: string; enabled?: boolean };
    const existing = watches.find((w) => w.app_uuid === b.app_uuid);
    const w = { ...(existing ?? watch(b.app_uuid, b.name ?? b.app_uuid, "", b.branch, 0, null)), repo: b.repo, branch: b.branch, enabled: b.enabled ?? true, last_error: null };
    if (existing) Object.assign(existing, w);
    else watches.push(w);
    return w;
  }
  if (/^\/v1\/autodeploy\/[^/]+\/check$/.test(p)) {
    const w = watches.find((x) => p.includes(x.app_uuid));
    if (w) w.last_checked_at = new Date().toISOString();
    return w;
  }
  if (p.startsWith("/v1/autodeploy/") && req.method === "DELETE") {
    const i = watches.findIndex((w) => p.endsWith(w.app_uuid));
    if (i >= 0) watches.splice(i, 1);
    return { ok: true };
  }
  if (p === "/v1/coolify/worker-key") return { public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHkLyFOm2v3pq6tYp2kQ0Q8Xc1sR2mJ3dZy7vC9p0Abc coolify", private_key_uuid: "k1" };
  if (p === "/v1/ssh/authorized-keys") return { added: true };
  if (p === "/v1/tailscale") return tailscale;
  if (p === "/v1/containers") return containers.map((c) => (c.cpu_pct == null ? c : { ...c, cpu_pct: c.cpu_pct * (0.7 + Math.random() * 0.6) }));
  if (p.endsWith("/logs")) return { lines: ["listening on :3000", "GET / 200 4ms", "GET /api/notes 200 11ms"] };
  if (p === "/v1/services") return services;
  if (p === "/v1/coolify/volumes") return volumes;
  if (p === "/v1/volumes" && req.method === "GET") return hostVolumes;
  if (p === "/v1/volumes") return { host_path: "/mnt/storage/volumes/demo/data", created: true };
  if (p === "/v1/display")
    return { receiver: idle, receiver_config: null, receiver_url: null, mpv_installed: true, sunshine: services[5] };
  return { ok: true };
}

const envs: Record<string, CoolifyEnv[]> = {};

function coolifyRoute(req: HttpRequest): unknown {
  const p = req.path.split("?")[0]!;
  if (p === "/servers" && req.method === "GET") return coolifyServers.map((x) => ({ ...x }));
  if (p === "/servers") {
    const b = req.body as { name: string; ip: string };
    coolifyServers.push({ uuid: `s${coolifyServers.length}`, name: b.name, ip: b.ip, settings: { is_reachable: true, is_usable: true } });
    return { uuid: `s${coolifyServers.length - 1}` };
  }
  if (p === "/applications/public") {
    const b = req.body as { name: string; git_repository: string; git_branch: string; build_pack: string };
    const uuid = `n${apps.length + 1}`;
    apps.push(app(uuid, b.name, "starting:unknown", `https://${b.name}.shashank.dev`, b.git_repository.replace(/^https:\/\/github\.com\//, "").replace(/\.git$/, ""), b.git_branch, b.build_pack, 0));
    return { uuid, domains: `https://${b.name}.shashank.dev` };
  }
  if (p === "/projects" && req.method === "GET") return [{ uuid: "p1", name: "default" }];
  if (p === "/projects/p1") return { uuid: "p1", name: "default", environments: [{ id: 1, name: "production", uuid: "e1" }] };
  // Fresh objects each time, like a real API (React skips identical state).
  if (p === "/applications") return apps.map((x) => ({ ...x }));
  if (p === "/databases") return dbs;
  let m = /^\/deployments\/applications\/(\w+)$/.exec(p);
  if (m) return { count: 6, deployments: deployments(m[1]!) };
  m = /^\/deployments\/(\w+)-d(\d+)$/.exec(p);
  if (m) return deployments(m[1]!)[Number(m[2])];
  m = /^\/applications\/(\w+)\/envs(\/bulk)?$/.exec(p);
  if (m) {
    const list = (envs[m[1]!] ??= [
      { uuid: `${m[1]}-e1`, key: "NODE_ENV", value: "production", is_preview: false },
      { uuid: `${m[1]}-e2`, key: "DATABASE_URL", value: "postgres://app:••••@pg-main:5432/app", is_preview: false },
    ]);
    if (m[2] && req.method === "PATCH") {
      for (const v of (req.body as { data: { key: string; value: string }[] }).data) {
        const found = list.find((e) => e.key === v.key);
        if (found) found.value = v.value;
        else list.push({ uuid: `${m[1]}-e${list.length + 1}`, key: v.key, value: v.value, is_preview: false });
      }
      return { message: "Environment variables updated." };
    }
    return list.map((e) => ({ ...e }));
  }
  m = /^\/applications\/(\w+)\/previews$/.exec(p);
  if (m)
    return m[1] === "a1"
      ? [{ uuid: "p1", pull_request_id: 42, pull_request_html_url: null, status: "running:healthy", fqdn: "https://pr-42-shashank.dev", created_at: iso(50) }]
      : [];
  if (/\/logs$/.test(p)) return { logs: "▲ Next.js 15.4.0\n- Local: http://localhost:3000\n✓ Ready in 412ms\nGET / 200 in 18ms\nGET /about 200 in 9ms" };
  if (/^\/applications\/\w+$/.test(p) && req.method === "PATCH") {
    const a = apps.find((x) => p.endsWith(x.uuid));
    const domains = (req.body as { domains?: string }).domains;
    if (a && domains !== undefined) a.fqdn = domains || null;
    return { uuid: a?.uuid };
  }
  if (p === "/version") return "4.0.0";
  return { message: "ok" };
}

export async function demoCall(cmd: string, args: Record<string, unknown>): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 120 + Math.random() * 180));
  const req = args.request as HttpRequest;
  switch (cmd) {
    case "agent_request":
      return agentRoute(req, args.server);
    case "coolify_request":
      return coolifyRoute(req);
    case "discover_servers":
      // The new machine "finishes installing" after a few scans.
      return ++discoverCalls >= 3 ? [NEW_MACHINE, ...DISCOVERED] : DISCOVERED;
    case "pair_claim": {
      const d = [NEW_MACHINE, ...DISCOVERED].find((x) => x.agent_url === args.agentUrl) ?? DISCOVERED[0]!;
      if (String(args.code).replace(/[\s-]/g, "").length !== 8) throw new Error("wrong pairing code (4 attempts left)");
      return {
        agent_url: `http://${d.dns_name}:7420`,
        agent_token: "demo-token-0123456789",
        coolify_url: d.role === "worker" ? "" : `http://${d.dns_name}:8000`,
        coolify_token: d.role === "worker" ? undefined : "demo",
        hostname: d.hostname,
        role: d.role,
      };
    }
    case "test_connection":
      return { hostname: "thinkpad-t470", coolify: { ok: true, version: "4.0.0" } };
    case "tools_status":
      return [
        { name: "ffmpeg", installed: true, path: "/opt/homebrew/bin/ffmpeg", install_hint: "brew install ffmpeg" },
        { name: "tailscale", installed: true, path: "/Applications/Tailscale.app", install_hint: "" },
        { name: "moonlight", installed: false, path: null, install_hint: "brew install --cask moonlight" },
      ];
    case "local_tailscale":
      return { ...tailscale, self_node: tailscale.peers[0], peers: [] };
    case "capture_screens":
      return [{ index: 1, name: "Built-in Retina Display" }];
    case "sender_status":
    case "moonlight_status":
      return idle;
    default:
      return null;
  }
}

/** Fake an Ollama chat stream, word by word. */
// --- jobs, live events, sleeping apps, power guard ---------------------------------

const jobs: Job[] = [
  { id: "j3", kind: "sleep", title: "pr-42-shashank.dev went to sleep", status: "ok", started_at: iso(35), finished_at: iso(35), detail: "no requests for 30 minutes; wakes on the next one", progress: null, target: "a1" },
  { id: "j2", kind: "deploy", title: "Deploy portfolio (9f2c1e0 on main)", status: "ok", started_at: iso(4), finished_at: iso(3), detail: "live at https://shashank.dev", progress: 100, target: "a1" },
  { id: "j1", kind: "backup", title: "Backup", status: "ok", started_at: iso(600), finished_at: iso(596), detail: "backed up", progress: 100, target: null },
];
const eventListeners = new Set<(e: AgentEvent) => void>();
let seq = 0;

function emit(kind: string, extra: Partial<AgentEvent>) {
  const e: AgentEvent = { seq: ++seq, at: new Date().toISOString(), kind, alert: null, job: null, topic: null, ...extra };
  eventListeners.forEach((fn) => fn(e));
}

function startJob(kind: string, title: string, target: string | null): Job {
  const j: Job = { id: `j${Date.now()}`, kind, title, status: "running", started_at: iso(0), finished_at: null, detail: null, progress: null, target };
  jobs.unshift(j);
  emit("job", { job: { ...j } });
  return j;
}

function updateJob(j: Job, patch: Partial<Job>) {
  Object.assign(j, patch);
  emit("job", { job: { ...j } });
}

/** A rough stand-in for the agent's parser: counts only. */
function demoSummary(files: { content: string }[]): ProjectSummary {
  const merged: Record<string, unknown> = {};
  for (const f of files) {
    let v: unknown;
    try {
      v = JSON.parse(f.content);
    } catch (e) {
      throw new Error(`vercel.json: ${(e as Error).message}`);
    }
    if (!v || typeof v !== "object" || Array.isArray(v)) throw new Error("vercel.json: must be a JSON object");
    Object.assign(merged, v);
  }
  const len = (k: string) => (Array.isArray(merged[k]) ? (merged[k] as unknown[]).length : 0);
  const fw = (merged.firewall ?? {}) as { rules?: unknown[]; blockIps?: unknown[]; trustedIps?: unknown[] };
  return {
    redirects: len("redirects"),
    rewrites: len("rewrites"),
    headers: len("headers"),
    firewall_rules: (fw.rules?.length ?? 0) + (fw.blockIps?.length ? 1 : 0) + (fw.trustedIps?.length ? 1 : 0),
    clean_urls: merged.cleanUrls === true,
    trailing_slash: typeof merged.trailingSlash === "boolean" ? merged.trailingSlash : null,
    crons: ((merged.crons as { path: string; schedule: string }[] | undefined) ?? []).map((c) => ({
      path: c.path,
      schedule: c.schedule,
      next_run_at: iso(-60),
      last_run_at: null,
      last_status: null,
      last_error: null,
      last_duration_ms: null,
      running: false,
    })),
    warnings: Object.keys(merged)
      .filter((k) => ["functions", "buildCommand", "regions"].includes(k))
      .map((k) => `\`${k}\` is not used here: your server runs the app as one container`),
  };
}

const projects: ProjectInfo[] = [
  {
    key: "a1",
    app_uuid: "a1",
    pull_request_id: 0,
    source: "repo",
    files: ["vercel.json"],
    commit: "9f2c1e0a3f9a3f9a3f9a3f9a3f9a3f9a3f9a3f9a",
    updated_at: iso(3),
    summary: {
      redirects: 4,
      rewrites: 1,
      headers: 2,
      firewall_rules: 0,
      clean_urls: true,
      trailing_slash: false,
      crons: [
        { path: "/api/cron/sitemap", schedule: "0 5 * * *", next_run_at: iso(-300), last_run_at: iso(1140), last_status: 200, last_error: null, last_duration_ms: 412, running: false },
        { path: "/api/cron/digest", schedule: "*/30 * * * *", next_run_at: iso(-12), last_run_at: iso(18), last_status: 500, last_error: "answered HTTP 500", last_duration_ms: 95, running: false },
      ],
      warnings: [
        "`functions` is not used here: your app runs as one container, with no per-function memory or duration limits",
        "cleanUrls: `.html` addresses redirect to clean ones; your server must answer the clean address (most static servers do)",
      ],
    },
    routes_active: true,
    hosts: ["shashank.dev"],
    error: null,
  },
  {
    key: "a1-pr-42",
    app_uuid: "a1",
    pull_request_id: 42,
    source: "repo",
    files: ["vercel.json"],
    commit: "c4e1f7a9b2c4e1f7a9b2c4e1f7a9b2c4e1f7a9b2",
    updated_at: iso(25),
    summary: { redirects: 5, rewrites: 1, headers: 2, firewall_rules: 0, clean_urls: true, trailing_slash: false, crons: [], warnings: [] },
    routes_active: true,
    hosts: ["pr-42-shashank.dev"],
    error: null,
  },
];

const sleepSettings: SleepSetting[] = [{ app_uuid: "a3", enabled: true, idle_minutes: 30, previews: false }];

function sleepStatus() {
  return {
    proxy_metrics: true,
    settings: sleepSettings.map((x) => ({ ...x })),
    targets: sleepSettings.map((x) => ({
      key: x.app_uuid,
      app_uuid: x.app_uuid,
      pull_request_id: 0,
      hosts: [(apps.find((a) => a.uuid === x.app_uuid)?.fqdn ?? "").replace(/^https?:\/\//, "")],
      state: x.app_uuid === "a3" ? "asleep" : "awake",
      last_request_at: iso(95),
      asleep_since: x.app_uuid === "a3" ? iso(65) : null,
    })),
    error: null,
  };
}

const PG_DEFAULTS: PowerGuardSettings = {
  battery_shutdown_pct: 15,
  battery_warn_pct: 30,
  thermal_pause_c: 92,
  thermal_freeze_c: 95,
  thermal_resume_c: 80,
  thermal_shutdown_c: 98,
  thermal_cpu_cap_pct: 50,
};
let pgSettings: PowerGuardSettings = { ...PG_DEFAULTS };

/** The live event stream: a hello, then whatever the demo backend does. */
async function demoEvents(onLine: (line: string) => void, signal: AbortSignal): Promise<void> {
  const fn = (e: AgentEvent) => onLine(JSON.stringify(e));
  eventListeners.add(fn);
  fn({ seq: ++seq, at: new Date().toISOString(), kind: "hello", alert: null, job: null, topic: null });
  await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }));
  eventListeners.delete(fn);
}

export async function demoStream(req: HttpRequest, onLine: (line: string) => void, signal: AbortSignal): Promise<void> {
  if (req.path === "/v1/events") return demoEvents(onLine, signal);
  const body = req.body as { model: string; messages: LlmChatMessage[] };
  const last = body.messages[body.messages.length - 1]?.content ?? "";
  const reply = isExplainChat(body.messages)
    ? DEMO_EXPLANATION
    : `Running ${body.model} on your old laptop. You asked: “${last.slice(0, 80)}”. This is demo mode, so the answer is canned — on a real server the tokens stream in as the CPU produces them.`;
  const words = reply.split(/(?<= )/);
  await new Promise((r) => setTimeout(r, 400));
  for (const w of words) {
    if (signal.aborted) return;
    onLine(JSON.stringify({ message: { role: "assistant", content: w }, done: false }));
    await new Promise((r) => setTimeout(r, 45));
  }
  onLine(JSON.stringify({ done: true, eval_count: words.length * 1.3, eval_duration: words.length * 45e6 * 1.3 }));
}

// Demo mode only: what Gemma 4 E2B is expected to say about cli/examples/failed-build.log.
// Not model output; a real server streams the model's own answer.
const DEMO_EXPLANATION = `(Demo mode: canned answer, no model ran.)

**Cause:** \`npm run build\` fails because \`app/page.tsx\` imports \`../components/Header\`, and Linux file names are case-sensitive, so that path doesn't match the file on disk (likely \`components/header.tsx\`).
**Fix:** make the import match the file name exactly: \`import Header from "../components/header"\` in \`app/page.tsx\` (or \`git mv components/header.tsx components/Header.tsx\`).
**Check:** \`git ls-files components\` then \`npm run build\`.`;

export const DEMO_SETTINGS = {
  agent_url: "http://thinkpad-t470.tail1234.ts.net:7420",
  agent_token: "demo",
  coolify_url: "http://thinkpad-t470.tail1234.ts.net:8000",
  coolify_token: "demo",
  hostname: "thinkpad-t470",
};
