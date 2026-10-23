//! Wire types shared by the host agent (server) and the cockpit (client).
//!
//! Every type here derives `TS`, so `cargo test -p vercelify-core` regenerates
//! the TypeScript definitions in `cockpit/src/types/generated/`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identity of the machine the agent runs on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub uptime_secs: u64,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CpuStats {
    pub model: String,
    pub cores: u32,
    pub usage_pct: f32,
    /// 1, 5 and 15 minute load averages.
    pub load_avg: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct MemoryStats {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

/// What a mount point is used for in the Vercelify layout.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DiskRole {
    /// `/` — OS, Docker engine, build caches (SSD tier).
    System,
    /// `/mnt/storage` — persistent databases and bulk volumes (HDD tier).
    Storage,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MediaKind {
    Ssd,
    Hdd,
    /// SD card or eMMC (`mmcblk*`): flash that wears out under constant writes.
    SdCard,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DiskInfo {
    pub device: String,
    pub mount_point: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub role: DiskRole,
    pub media: MediaKind,
    pub removable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Battery {
    pub name: String,
    pub capacity_pct: Option<u8>,
    /// Raw kernel status: Charging, Discharging, Full, Not charging, Unknown.
    pub status: String,
    /// Full-charge capacity as a percentage of design capacity.
    pub health_pct: Option<f32>,
    /// Charge stop threshold if the firmware supports one.
    pub charge_stop_threshold: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct PowerStatus {
    /// `None` when the machine has no mains adapter (desktop, Raspberry Pi).
    pub ac_online: Option<bool>,
    pub batteries: Vec<Battery>,
    /// True only when a battery exists and no AC adapter is online.
    pub on_battery: bool,
    pub lowest_capacity_pct: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ThermalSensor {
    pub name: String,
    pub temp_c: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct ThermalStatus {
    pub sensors: Vec<ThermalSensor>,
    pub max_c: Option<f32>,
}

/// Contents of `/run/vercelify/power-guard.state`, written by `power-guard.sh`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct PowerGuardState {
    /// ok | warning | thermal_paused | shutting_down
    pub state: String,
    pub reason: String,
    pub updated_at: String,
}

/// Raspberry Pi firmware throttling flags (`vcgencmd get_throttled`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Throttling {
    pub raw: u32,
    pub under_voltage_now: bool,
    pub throttled_now: bool,
    pub soft_temp_limit_now: bool,
    pub under_voltage_since_boot: bool,
    pub throttled_since_boot: bool,
}

/// Pressure stall information: % of the last 10 s some task waited.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Pressure {
    pub cpu_some_avg10: Option<f32>,
    pub memory_some_avg10: Option<f32>,
    pub io_some_avg10: Option<f32>,
}

/// What kind of machine this is and how it copes (see `hardware.rs`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct HardwareInfo {
    /// pi5 | pi4 | pi | laptop | pc
    pub profile: String,
    /// Device-tree model ("Raspberry Pi 5 Model B Rev 1.0"), empty on PCs.
    pub model: String,
    /// Media of the disk holding `/`.
    pub root_media: MediaKind,
    /// The kernel enforces container memory limits.
    pub memory_cgroup: bool,
    pub throttling: Option<Throttling>,
    pub pressure: Option<Pressure>,
    /// Processes the kernel's OOM killer has killed since boot.
    pub oom_kills: Option<u64>,
    /// A hardware watchdog device exists.
    pub watchdog: bool,
    /// The profile's default thermal pause threshold (°C).
    pub defaults_pause_c: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct SystemSnapshot {
    pub host: HostInfo,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    pub disks: Vec<DiskInfo>,
    pub power: PowerStatus,
    pub thermal: ThermalStatus,
    pub power_guard: Option<PowerGuardState>,
    /// Older agents don't send it.
    #[serde(default)]
    pub hardware: Option<HardwareInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct TailscaleNode {
    pub id: String,
    pub hostname: String,
    /// MagicDNS name without the trailing dot.
    pub dns_name: String,
    pub os: String,
    pub ips: Vec<String>,
    pub online: bool,
    pub active: bool,
    pub exit_node: bool,
    /// DERP relay region in use, empty if unknown.
    pub relay: String,
    /// Direct endpoint (ip:port) when a peer-to-peer path exists.
    pub cur_addr: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub last_seen: Option<String>,
    /// When this node's key expires (RFC 3339); None = never (tagged node or
    /// expiry disabled in the admin console).
    #[serde(default)]
    pub key_expiry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct TailscaleStatus {
    pub installed: bool,
    /// Running | NeedsLogin | Stopped | Starting | NoState | ...
    pub backend_state: String,
    pub version: String,
    pub tailnet: String,
    pub magic_dns_suffix: String,
    pub auth_url: String,
    pub self_node: Option<TailscaleNode>,
    pub peers: Vec<TailscaleNode>,
    pub health: Vec<String>,
    /// Set when the CLI could not be run or returned garbage.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running | exited | paused | restarting | created | dead
    pub state: String,
    pub status: String,
    pub ports: String,
    pub cpu_pct: Option<f32>,
    pub mem_bytes: Option<u64>,
    pub mem_pct: Option<f32>,
    /// Coolify resource name (from the `coolify.name` label) if managed by Coolify.
    pub coolify_name: Option<String>,
    pub coolify_managed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StorageTier {
    /// Docker-managed named volume under `/var/lib/docker/volumes` (fast SSD).
    Ssd,
    /// Bind mount under `/mnt/storage/volumes` (large HDD).
    Hdd,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct VolumeRequest {
    pub service: String,
    /// Optional sub-directory, e.g. `data` or `uploads/images`.
    #[serde(default)]
    pub name: Option<String>,
    pub tier: StorageTier,
    /// Owner for the created directory (HDD tier only), e.g. 999 for postgres.
    #[serde(default)]
    pub uid: Option<u32>,
    #[serde(default)]
    pub gid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct VolumeResult {
    pub tier: StorageTier,
    /// Named volume (SSD tier) or absolute host path (HDD tier).
    pub source: String,
    /// Directory on the host that will hold the data.
    pub host_path: String,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct VolumeEntry {
    pub tier: StorageTier,
    pub name: String,
    pub host_path: String,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StreamTransport {
    /// MPEG-TS over raw UDP. Lowest latency; fine over Tailscale/LAN.
    Udp,
    /// SRT with ARQ; tolerates packet loss (cellular, long distance).
    Srt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ReceiverConfig {
    pub transport: StreamTransport,
    pub port: u16,
    /// SRT receive latency in milliseconds (ignored for UDP).
    #[serde(default)]
    pub latency_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct ProcessStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub command: Option<String>,
    pub started_at: Option<String>,
    /// Last lines of stderr, useful when the process died immediately.
    pub last_output: Vec<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ServiceStatus {
    pub name: String,
    /// active | inactive | failed | activating | not-found | unknown
    pub active_state: String,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DisplayStatus {
    pub receiver: ProcessStatus,
    pub receiver_config: Option<ReceiverConfig>,
    /// Address the Mac should stream to, e.g. `udp://100.101.102.103:5600`.
    pub receiver_url: Option<String>,
    pub mpv_installed: bool,
    pub sunshine: ServiceStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ApiError {
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Health {
    pub ok: bool,
    pub version: String,
    pub hostname: String,
    /// `main` (runs Coolify) or `worker` (Docker + agent only).
    #[serde(default)]
    pub role: String,
    /// A short pairing code (`sudo vercelify pair`) is waiting to be claimed.
    #[serde(default)]
    pub pairing_open: bool,
    /// Bumped on breaking API changes. 0 = an agent from before versioning.
    #[serde(default)]
    pub api_version: u32,
    /// Optional features this agent offers (`alerts`, `metrics`, `tokens`…),
    /// so a cockpit can talk to servers running different versions.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// A persistent volume of a Coolify-managed resource (from Coolify's DB).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CoolifyVolume {
    pub id: u64,
    /// Docker volume name, e.g. `postgres-data-<uuid>`.
    pub name: String,
    /// Path inside the container.
    pub mount_path: String,
    /// Bind-mount source when not using the named volume.
    pub host_path: Option<String>,
    /// Where the data currently lives.
    pub tier: StorageTier,
    /// Effective directory on the host.
    pub data_path: String,
    /// e.g. `StandalonePostgresql`, `Application`.
    pub resource_type: String,
    pub resource_name: String,
    pub resource_uuid: String,
    /// Only standalone databases and applications can be moved safely
    /// (compose-based services regenerate their mounts from the compose file).
    pub movable: bool,
    /// A running container currently mounts this volume.
    pub in_use: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct MoveVolumeRequest {
    pub tier: StorageTier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct MoveVolumeResult {
    pub volume: CoolifyVolume,
    pub previous_path: String,
    pub copied: bool,
    /// Human-readable next step (start the resource, delete the old copy, ...).
    pub note: String,
}

/// An external program the cockpit can drive (ffmpeg, Moonlight, Tailscale).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ToolStatus {
    pub name: String,
    pub installed: bool,
    pub path: Option<String>,
    pub install_hint: String,
}

/// Cockpit connection settings (stored on the Mac with 0600 permissions).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CockpitSettings {
    pub agent_url: String,
    pub agent_token: String,
    pub coolify_url: String,
    #[serde(default)]
    pub coolify_token: Option<String>,
    pub hostname: String,
    /// `main` or `worker`; missing in settings written by older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<String>,
}

/// Every server this cockpit is paired with, and which one is selected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct CockpitConfig {
    pub servers: Vec<CockpitSettings>,
    /// `agent_url` of the selected server.
    #[serde(default)]
    pub active: Option<String>,
}

impl CockpitConfig {
    /// The selected server, falling back to the first one.
    pub fn active_server(&self) -> Option<&CockpitSettings> {
        self.active
            .as_deref()
            .and_then(|a| self.servers.iter().find(|s| s.agent_url == a))
            .or_else(|| self.servers.first())
    }

    pub fn find(&self, agent_url: &str) -> Option<&CockpitSettings> {
        self.servers.iter().find(|s| s.agent_url == agent_url)
    }

    /// Add or replace a server (matched by agent URL) and select it.
    pub fn upsert(&mut self, s: CockpitSettings) {
        self.active = Some(s.agent_url.clone());
        match self.servers.iter_mut().find(|x| x.agent_url == s.agent_url) {
            Some(existing) => *existing = s,
            None => self.servers.push(s),
        }
    }

    /// Remove a server; selects the first remaining one if it was active.
    pub fn remove(&mut self, agent_url: &str) {
        self.servers.retain(|s| s.agent_url != agent_url);
        if self.active.as_deref() == Some(agent_url) || self.active_server().is_none() {
            self.active = self.servers.first().map(|s| s.agent_url.clone());
        }
    }

    /// Accept both this format and the single-server `settings.json` of 0.1.
    pub fn from_json(raw: &str) -> Option<Self> {
        if let Ok(c) = serde_json::from_str::<CockpitConfig>(raw) {
            return Some(c);
        }
        let s: CockpitSettings = serde_json::from_str(raw).ok()?;
        Some(CockpitConfig {
            active: Some(s.agent_url.clone()),
            servers: vec![s],
        })
    }
}

/// A Vercelify host found on the tailnet (it answered `/v1/health`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DiscoveredServer {
    pub hostname: String,
    /// MagicDNS name, empty when MagicDNS is off.
    pub dns_name: String,
    pub ip: String,
    pub os: String,
    /// Base URL of the agent, e.g. `http://100.101.102.103:7420`.
    pub agent_url: String,
    pub version: String,
    pub role: String,
    pub pairing_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct PairClaimRequest {
    /// Short code shown by `sudo vercelify pair`, e.g. `K7QF-M2XD`.
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct PairClaimResponse {
    /// Full `vfy1.` pairing code.
    pub pairing: String,
}

// --- auto-deploy ----------------------------------------------------------------

/// A branch the agent watches; a new commit triggers a Coolify deployment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AutoDeployWatch {
    pub app_uuid: String,
    pub name: String,
    /// `https://…` clone URL.
    pub repo: String,
    pub branch: String,
    pub enabled: bool,
    /// An access token is stored for this repository (never returned).
    pub has_token: bool,
    /// Head of the branch at the last successful check.
    pub last_commit: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_deployed_commit: Option<String>,
    pub last_deployed_at: Option<String>,
    pub last_error: Option<String>,
    pub deploys: u32,
    /// Deploy a preview for every open pull request (GitHub).
    #[serde(default)]
    pub previews: bool,
    /// Roll back to the previous commit when a new deploy fails or the app
    /// is unhealthy right after it.
    #[serde(default)]
    pub auto_rollback: bool,
    /// Report deploys as GitHub commit statuses and comment preview URLs
    /// on pull requests (needs a token with write access).
    #[serde(default)]
    pub github_feedback: bool,
    /// Pull requests with a live preview.
    #[serde(default)]
    pub open_previews: Vec<PreviewWatch>,
    #[serde(default)]
    pub rollbacks: u32,
    /// Pushes not built because nothing in the app's watch paths changed or
    /// the commit said `[skip deploy]` (Vercel's "Ignored Build Step").
    #[serde(default)]
    pub skipped: u32,
    #[serde(default)]
    pub last_skip: Option<String>,
}

/// A pull request the agent deployed as a preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct PreviewWatch {
    pub number: u64,
    pub title: String,
    pub head_sha: String,
    pub url: Option<String>,
    pub deployed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AutoDeployRequest {
    pub app_uuid: String,
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
    pub repo: String,
    pub branch: String,
    /// Read-only token for private repositories. Omit to keep the stored
    /// one, send an empty string to remove it.
    #[serde(default)]
    #[ts(optional)]
    pub token: Option<String>,
    #[serde(default = "default_true")]
    #[ts(optional)]
    pub enabled: Option<bool>,
    /// Omit to keep the current setting.
    #[serde(default)]
    #[ts(optional)]
    pub previews: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub auto_rollback: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub github_feedback: Option<bool>,
}

fn default_true() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AutoDeployStatus {
    pub interval_secs: u64,
    /// The agent has a Coolify API token, so it can trigger deployments.
    pub coolify_configured: bool,
    pub watches: Vec<AutoDeployWatch>,
}

// --- local LLMs (Ollama) ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmModel {
    /// e.g. `llama3.2:3b`
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
}

/// A model currently loaded in memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmLoaded {
    pub name: String,
    pub size_bytes: u64,
    pub expires_at: Option<String>,
}

/// A long-running job: installing Ollama or downloading a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmTask {
    /// `install` | `pull`
    pub kind: String,
    /// Image or model name.
    pub target: String,
    pub status: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmStatus {
    /// The Ollama container exists.
    pub installed: bool,
    pub running: bool,
    /// Ollama answers on its API.
    pub ready: bool,
    pub container_state: Option<String>,
    pub image: String,
    pub version: Option<String>,
    pub port: u16,
    /// URLs other machines on the tailnet can use, e.g. `http://laptop.tail1234.ts.net:11434`.
    pub endpoints: Vec<String>,
    /// Where models are stored on the host.
    pub data_path: Option<String>,
    pub models: Vec<LlmModel>,
    pub loaded: Vec<LlmLoaded>,
    pub tasks: Vec<LlmTask>,
    /// The Vercelify firewall keeps port 11434 private to the tailnet.
    pub firewall_active: bool,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmInstallRequest {
    /// Where to keep model files.
    pub tier: StorageTier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmModelRequest {
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmChatMessage {
    /// `system` | `user` | `assistant`
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct LlmChatRequest {
    pub model: String,
    pub messages: Vec<LlmChatMessage>,
    #[serde(default)]
    #[ts(optional)]
    pub temperature: Option<f32>,
    /// Longest answer in tokens (default 1024, at most 8192). Small models
    /// can otherwise ramble until the context fills, for minutes on a slow CPU.
    #[serde(default)]
    #[ts(optional)]
    pub max_tokens: Option<u32>,
    /// Stream NDJSON chunks (Ollama's format) instead of one JSON reply.
    #[serde(default)]
    #[ts(optional)]
    pub stream: Option<bool>,
}

// --- server pools -----------------------------------------------------------------

/// What a worker needs to let the main host's Coolify in over SSH.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CoolifyWorkerKey {
    /// OpenSSH public key of Coolify's localhost key.
    pub public_key: String,
    /// Coolify `private_keys.uuid`, for `POST /api/v1/servers`.
    pub private_key_uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AuthorizeKeyRequest {
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AuthorizeKeyResult {
    /// False when the key was already authorised.
    pub added: bool,
}

// --- health checks and updates ------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CheckStatus {
    Ok,
    /// Works, but worth a look (single disk, on battery, firewall off…).
    Warn,
    /// Broken: deployments or the cockpit will not work properly.
    Bad,
}

/// One line of `GET /v1/doctor` (the same idea as `sudo vercelify doctor`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DoctorCheck {
    pub id: String,
    pub label: String,
    pub status: CheckStatus,
    pub detail: String,
    /// What to do about a warning or failure.
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DoctorReport {
    pub role: String,
    pub agent_version: String,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// The worst status of all checks.
    pub fn overall(&self) -> CheckStatus {
        if self.checks.iter().any(|c| c.status == CheckStatus::Bad) {
            CheckStatus::Bad
        } else if self.checks.iter().any(|c| c.status == CheckStatus::Warn) {
            CheckStatus::Warn
        } else {
            CheckStatus::Ok
        }
    }
}

/// State of `vercelify update` started from the cockpit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct UpdateStatus {
    pub running: bool,
    pub started_at: Option<String>,
    /// Exit code of the last finished update (0 = success).
    pub exit_code: Option<i32>,
    /// Last lines of the update's output.
    pub log_tail: Vec<String>,
}

// --- alerts, metrics and uptime -------------------------------------------------------

/// Something changed on a server that someone should know about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Alert {
    pub id: u64,
    pub at: String,
    /// The doctor check or uptime target it is about (`disk`, `app:<uuid>`…).
    pub source: String,
    pub status: CheckStatus,
    pub title: String,
    pub detail: String,
    /// A previous problem went away.
    pub resolved: bool,
    /// The notification was delivered (None = no notification target).
    pub notified: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AlertsStatus {
    /// Where notifications go, with secrets masked; None = nowhere.
    pub notify_target: Option<String>,
    /// Newest first.
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct NotifyConfigRequest {
    /// ntfy topic, Discord/Slack webhook or any URL taking a POSTed text body.
    /// Empty = stop sending.
    pub url: String,
}

/// One minute of a server's life.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct MetricPoint {
    /// Unix seconds.
    pub t: u64,
    pub cpu_pct: f32,
    pub mem_used_pct: f32,
    pub swap_used_pct: f32,
    pub disk_root_pct: f32,
    pub load1: f32,
    pub temp_c: Option<f32>,
    /// Memory pressure (PSI some avg10).
    pub mem_pressure: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct MetricsResponse {
    pub interval_secs: u64,
    pub points: Vec<MetricPoint>,
}

/// Is an app URL or another server answering?
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct UptimeCheck {
    /// `app:<uuid>` or `server:<name>`.
    pub id: String,
    pub name: String,
    pub target: String,
    pub up: bool,
    pub http_status: Option<u16>,
    pub latency_ms: Option<u32>,
    pub checked_at: String,
    /// When `up` last changed.
    pub since: String,
    pub error: Option<String>,
}

// --- device tokens -------------------------------------------------------------------

/// An agent token issued to one paired device (the secret is never shown again).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DeviceToken {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    /// The token making this request.
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DeviceTokenRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DeviceTokenCreated {
    pub token: DeviceToken,
    /// Shown once.
    pub secret: String,
}

/// One line of the audit log: a change made through the agent's API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AuditEntry {
    pub at: String,
    /// Token id (`admin` = the host's own token).
    pub actor: String,
    pub method: String,
    pub path: String,
    pub status: u16,
}

// --- images, backups ------------------------------------------------------------------

/// Image tags an app can roll back to without rebuilding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AppImages {
    pub repository: String,
    /// Usually commit SHAs (Coolify tags images with the commit).
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct BackupStatus {
    pub configured: bool,
    /// Where backups go (password and keys masked).
    pub targets: Vec<String>,
    pub last_ok: Option<bool>,
    pub last_finished_at: Option<String>,
    pub last_error: Option<String>,
    pub running: bool,
    pub log_tail: Vec<String>,
}

// --- jobs and live events ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum JobStatus {
    Running,
    Ok,
    Failed,
}

/// Long-running work on a server: a backup, a volume move, a model download,
/// an automatic deploy… One shape for all of them, so the cockpit can show
/// one activity list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Job {
    pub id: String,
    /// backup | update | volume-move | llm-install | llm-pull | deploy |
    /// rollback | sleep | wake | cleanup
    pub kind: String,
    pub title: String,
    pub status: JobStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// The latest progress line while running; the result afterwards.
    pub detail: Option<String>,
    /// 0–100 when known.
    pub progress: Option<f32>,
    /// What it is about (an app uuid, a volume id, a model), for the UI.
    pub target: Option<String>,
}

/// One line of `GET /v1/events` (newline-delimited JSON, kept open).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AgentEvent {
    pub seq: u64,
    pub at: String,
    /// hello | alert | job | changed | ping
    pub kind: String,
    pub alert: Option<Alert>,
    pub job: Option<Job>,
    /// For `changed`: what to fetch again (autodeploy, sleep, backup,
    /// power-guard, apps).
    pub topic: Option<String>,
}

// --- sleeping apps --------------------------------------------------------------------

/// Stop an app after `idle_minutes` without requests; the next request
/// starts it again (the visitor sees a short "waking up" page).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct SleepSetting {
    pub app_uuid: String,
    pub enabled: bool,
    pub idle_minutes: u32,
    /// Also its pull request previews.
    pub previews: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct SleepRequest {
    pub enabled: bool,
    pub idle_minutes: Option<u32>,
    pub previews: Option<bool>,
}

/// One app or preview that may sleep.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct SleepTarget {
    /// `<app uuid>` or `<app uuid>-pr-<n>`.
    pub key: String,
    pub app_uuid: String,
    pub pull_request_id: u64,
    pub hosts: Vec<String>,
    /// awake | asleep | waking
    pub state: String,
    pub last_request_at: Option<String>,
    pub asleep_since: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct SleepStatus {
    /// Traefik counts requests per app (needed to know an app is idle).
    pub proxy_metrics: bool,
    pub settings: Vec<SleepSetting>,
    pub targets: Vec<SleepTarget>,
    /// Why apps can't sleep right now, if anything is wrong.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ProxyMetricsResult {
    pub metrics: bool,
    /// The proxy was restarted to apply it (a few seconds without traffic).
    pub restarted: bool,
}

// --- power guard ----------------------------------------------------------------------

/// The power guard's thresholds. Battery values are percentages, thermal
/// ones °C.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct PowerGuardSettings {
    pub battery_shutdown_pct: u32,
    pub battery_warn_pct: u32,
    pub thermal_pause_c: u32,
    pub thermal_freeze_c: u32,
    pub thermal_resume_c: u32,
    pub thermal_shutdown_c: u32,
    /// Share of the CPUs each app keeps while hot.
    pub thermal_cpu_cap_pct: u32,
}

impl PowerGuardSettings {
    /// Ordered, in range, and safe: None when valid, else what is wrong.
    pub fn problem(&self) -> Option<String> {
        let s = self;
        if !(5..=50).contains(&s.battery_shutdown_pct) {
            return Some("shutdown battery level must be 5–50%".into());
        }
        if s.battery_warn_pct <= s.battery_shutdown_pct || s.battery_warn_pct > 90 {
            return Some("the warning level must be above the shutdown level (and at most 90%)".into());
        }
        if !(50..=110).contains(&s.thermal_shutdown_c) {
            return Some("shutdown temperature must be 50–110 °C".into());
        }
        if !(s.thermal_resume_c < s.thermal_pause_c
            && s.thermal_pause_c <= s.thermal_freeze_c
            && s.thermal_freeze_c < s.thermal_shutdown_c)
        {
            return Some("temperatures must rise: resume < pause ≤ freeze < shutdown".into());
        }
        if s.thermal_resume_c < 40 {
            return Some("resume temperature must be at least 40 °C".into());
        }
        if !(10..=100).contains(&s.thermal_cpu_cap_pct) {
            return Some("the CPU share while hot must be 10–100%".into());
        }
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct PowerGuardStatus {
    /// The guard's service is running.
    pub active: bool,
    pub state: Option<PowerGuardState>,
    pub settings: PowerGuardSettings,
    /// What the installer chose for this machine.
    pub defaults: PowerGuardSettings,
    /// Changed from the cockpit (kept across updates).
    pub customized: bool,
}

// --- domains --------------------------------------------------------------------------

/// How this server's apps are reached from the internet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DomainInfo {
    /// private (Tailscale only, sslip.io) | cloudflare (tunnel) | direct (A records)
    pub mode: String,
    pub domain: Option<String>,
    /// New apps get `<name>.<this>` (Coolify's wildcard domain).
    pub wildcard: Option<String>,
    /// The Cloudflare tunnel is running (cloudflare mode only).
    pub tunnel_active: Option<bool>,
}

/// A cron job from an app's `vercel.json`, run by the agent (UTC).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CronInfo {
    pub path: String,
    pub schedule: String,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    /// HTTP status of the last run (None: it didn't get an answer).
    pub last_status: Option<u16>,
    pub last_error: Option<String>,
    pub last_duration_ms: Option<u64>,
    pub running: bool,
}

/// What an app's `vercel.json` / `vercelify.json` turned into.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct ProjectSummary {
    pub redirects: u32,
    pub rewrites: u32,
    pub headers: u32,
    /// Firewall rules, blocked and trusted IP lists.
    pub firewall_rules: u32,
    pub clean_urls: bool,
    pub trailing_slash: Option<bool>,
    pub crons: Vec<CronInfo>,
    /// Rules skipped or approximated, and settings handled elsewhere.
    pub warnings: Vec<String>,
}

/// Project configuration applied to an app or one of its previews.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ProjectInfo {
    /// `<app uuid>` or `<app uuid>-pr-<n>`.
    pub key: String,
    pub app_uuid: String,
    pub pull_request_id: u64,
    /// repo (read at each deploy) | upload (sent from the app or `vfy`)
    pub source: String,
    pub files: Vec<String>,
    pub commit: Option<String>,
    pub updated_at: String,
    pub summary: ProjectSummary,
    /// The rules are live in the proxy.
    pub routes_active: bool,
    pub hosts: Vec<String>,
    /// Why the rules aren't (all) live, if so.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, TS)]
#[ts(export)]
pub struct ProjectsStatus {
    pub projects: Vec<ProjectInfo>,
    /// The proxy's image (e.g. `traefik:v3.7`).
    pub proxy_image: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ProjectFile {
    /// `vercel.json` or `vercelify.json`.
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ProjectUpload {
    pub files: Vec<ProjectFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct CronRunRequest {
    pub path: String,
}
