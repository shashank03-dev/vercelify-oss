//! Wire types of the platform engine (`/v1/platform/…`, design doc §12).
//!
//! Shared by the agent (which serves them), the cockpit (generated TypeScript)
//! and `vfy`. Field names are the JSON names. Optional request fields mean
//! "leave as is" on PATCH and "use the default" on create.

use crate::model::StorageTier;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What a token may do (design §12).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Role {
    /// Read only.
    Viewer,
    /// Apps, deployments, variables, domains, previews, databases, logs.
    Deployer,
    /// Everything, including tokens, servers, the mesh and deletions.
    #[default]
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Deployer => "deployer",
            Role::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "deployer" => Some(Role::Deployer),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
}

// ----- projects and environments -------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Environment {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    pub environments: Vec<Environment>,
    pub app_count: u32,
    pub database_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ProjectRequest {
    #[ts(optional)]
    pub name: Option<String>,
    #[ts(optional)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct EnvironmentRequest {
    pub name: String,
}

// ----- apps -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum AppKind {
    /// Built from a git repository.
    Git,
    /// A ready image (`nginx:1.27`, `ghcr.io/me/app:v2`).
    Image,
    /// A Docker Compose file from a repository.
    Compose,
}

impl AppKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AppKind::Git => "git",
            AppKind::Image => "image",
            AppKind::Compose => "compose",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(default)]
#[ts(export)]
pub struct AppSource {
    /// `https://github.com/me/blog` or `git@github.com:me/blog.git`.
    pub repository: Option<String>,
    /// Branch deployed to production (default: the repository's default branch, `main`).
    pub branch: Option<String>,
    /// Credentials for a private repository (`git_…`).
    pub git_source_id: Option<String>,
    /// Image apps: the image reference.
    pub image: Option<String>,
    /// Compose apps: path of the compose file in the repository.
    pub compose_file: Option<String>,
    /// Compose apps without a repository (services from the catalogue): the
    /// compose file itself.
    pub compose_content: Option<String>,
    /// Set by a rollback so auto-deploy doesn't redeploy the branch head;
    /// cleared by the next deploy.
    pub pinned_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum BuildPack {
    /// Dockerfile if present, else detect the language (design §6.3).
    #[default]
    Auto,
    Dockerfile,
    Static,
    Node,
    Python,
    Go,
    Rust,
    Deno,
    Bun,
    Ruby,
    Php,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(default)]
#[ts(export)]
pub struct AppBuild {
    pub pack: BuildPack,
    /// Dockerfile path relative to the context (default `Dockerfile`).
    pub dockerfile: Option<String>,
    /// Build context (and app root in a monorepo), relative to the repository.
    pub context: Option<String>,
    pub install_command: Option<String>,
    pub build_command: Option<String>,
    /// Static output directory (`dist`, `out`, `build`…).
    pub output_directory: Option<String>,
    /// Only pushes that change these paths (globs) are built.
    pub watch_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[serde(default)]
#[ts(export)]
pub struct HealthCheck {
    /// HTTP path that must answer 2xx/3xx (`/healthz`).
    pub path: Option<String>,
    /// Or a command run in the container that must exit 0.
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(default)]
#[ts(export)]
pub struct AppRuntime {
    /// Port the app listens on inside the container (default: detected, else 3000).
    pub port: Option<u16>,
    /// Override the image's start command.
    pub command: Option<Vec<String>>,
    pub replicas: u8,
    pub memory_mb: Option<u32>,
    pub cpus: Option<f32>,
    pub health: Option<HealthCheck>,
    pub health_timeout_secs: u32,
    /// How long the old version keeps serving after the switch.
    pub drain_secs: u32,
    /// Compose apps: the service the domains route to.
    pub service: Option<String>,
    /// Stateless apps may be started on another server when theirs is offline.
    pub failover: bool,
}

impl Default for AppRuntime {
    fn default() -> Self {
        Self {
            port: None,
            command: None,
            replicas: 1,
            memory_mb: None,
            cpus: None,
            health: None,
            health_timeout_secs: 180,
            drain_secs: 10,
            service: None,
            failover: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(default)]
#[ts(export)]
pub struct PreviewSettings {
    pub enabled: bool,
    /// Previews running at once; the oldest stops when a new one starts.
    pub max: u8,
}

impl Default for PreviewSettings {
    fn default() -> Self {
        Self { enabled: false, max: 3 }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Desired {
    #[default]
    Running,
    Stopped,
    Sleeping,
}

impl Desired {
    pub fn as_str(self) -> &'static str {
        match self {
            Desired::Running => "running",
            Desired::Stopped => "stopped",
            Desired::Sleeping => "sleeping",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum CertStatus {
    Pending,
    Issued,
    Failed,
    /// TLS ends at Cloudflare (tunnel mode).
    Edge,
    /// Plain HTTP.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AppDomain {
    pub id: String,
    pub host: String,
    pub https: bool,
    pub is_primary: bool,
    /// Redirect this host to another one (`www` → apex).
    pub redirect_to: Option<String>,
    pub cert_status: CertStatus,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DomainRequest {
    pub host: String,
    #[ts(optional)]
    pub https: Option<bool>,
    #[ts(optional)]
    pub primary: Option<bool>,
    #[ts(optional)]
    pub redirect_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct App {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub environment_id: String,
    pub environment: String,
    pub kind: AppKind,
    pub source: AppSource,
    pub build: AppBuild,
    pub runtime: AppRuntime,
    pub previews: PreviewSettings,
    /// Server it runs on (`node_…`); None = placed automatically.
    pub node_id: Option<String>,
    pub desired: Desired,
    pub current_deployment_id: Option<String>,
    /// Status of its newest production deployment (lists show build state
    /// without a request per app).
    pub last_status: Option<DeploymentStatus>,
    pub domains: Vec<AppDomain>,
    /// Primary URL, if it has a domain.
    pub url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AppCreate {
    pub name: String,
    pub kind: AppKind,
    /// Environment to create it in; or `project_id` for its `production`
    /// environment; neither = the `Default` project.
    #[ts(optional)]
    pub environment_id: Option<String>,
    #[ts(optional)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub source: AppSource,
    #[serde(default)]
    #[ts(optional)]
    pub build: Option<AppBuild>,
    #[serde(default)]
    #[ts(optional)]
    pub runtime: Option<AppRuntime>,
    #[serde(default)]
    #[ts(optional)]
    pub previews: Option<PreviewSettings>,
    #[ts(optional)]
    pub node_id: Option<String>,
    /// Extra domains; the generated `<name>.<wildcard>` is added when a
    /// wildcard domain is set.
    #[serde(default)]
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AppPatch {
    #[ts(optional)]
    pub source: Option<AppSource>,
    #[ts(optional)]
    pub build: Option<AppBuild>,
    #[ts(optional)]
    pub runtime: Option<AppRuntime>,
    #[ts(optional)]
    pub previews: Option<PreviewSettings>,
    /// `""` clears the placement (automatic).
    #[ts(optional)]
    pub node_id: Option<String>,
    #[ts(optional)]
    pub desired: Option<Desired>,
}

// ----- environment variables ------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum VarScope {
    Project,
    Environment,
    App,
}

impl VarScope {
    pub fn as_str(self) -> &'static str {
        match self {
            VarScope::Project => "project",
            VarScope::Environment => "environment",
            VarScope::App => "app",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct EnvVar {
    pub id: String,
    pub key: String,
    /// None for a secret unless it was asked for with `?reveal=true`.
    pub value: Option<String>,
    /// Available while building (`--build-arg` / build secret).
    pub build: bool,
    /// Only for previews (overrides the normal value there).
    pub preview_only: bool,
    pub secret: bool,
    pub scope: VarScope,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct EnvVarInput {
    pub key: String,
    pub value: String,
    #[serde(default)]
    #[ts(optional)]
    pub build: Option<bool>,
    #[serde(default)]
    #[ts(optional)]
    pub preview_only: Option<bool>,
    /// Default true: values are hidden unless revealed.
    #[serde(default)]
    #[ts(optional)]
    pub secret: Option<bool>,
}

/// Upsert variables. With `replace`, variables of this scope (and the same
/// `preview_only`) that aren't listed are deleted: importing a `.env`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct EnvVarsPut {
    pub vars: Vec<EnvVarInput>,
    #[serde(default)]
    pub replace: bool,
}

// ----- deployments ----------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum DeploymentKind {
    Production,
    Preview,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum DeploymentStatus {
    Queued,
    Building,
    Deploying,
    Healthy,
    Failed,
    Cancelled,
    Superseded,
}

impl DeploymentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Building => "building",
            Self::Deploying => "deploying",
            Self::Healthy => "healthy",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "building" => Self::Building,
            "deploying" => Self::Deploying,
            "healthy" => Self::Healthy,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "superseded" => Self::Superseded,
            _ => return None,
        })
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Building | Self::Deploying)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DeploymentTrigger {
    Push,
    Manual,
    Hook,
    Webhook,
    Rollback,
    AutoRollback,
    Migration,
    /// The reconciler re-created a missing container from its image.
    Recover,
}

impl DeploymentTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Manual => "manual",
            Self::Hook => "hook",
            Self::Webhook => "webhook",
            Self::Rollback => "rollback",
            Self::AutoRollback => "auto-rollback",
            Self::Migration => "migration",
            Self::Recover => "recover",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "push" => Self::Push,
            "manual" => Self::Manual,
            "hook" => Self::Hook,
            "webhook" => Self::Webhook,
            "rollback" => Self::Rollback,
            "auto-rollback" => Self::AutoRollback,
            "migration" => Self::Migration,
            "recover" => Self::Recover,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Deployment {
    pub id: String,
    pub app_id: String,
    /// 1, 2, 3… per app.
    pub number: u32,
    pub kind: DeploymentKind,
    pub pr_number: Option<u32>,
    pub branch: String,
    pub commit_sha: String,
    pub commit_message: String,
    pub trigger: DeploymentTrigger,
    pub rollback_of: Option<String>,
    pub image: String,
    pub node_id: Option<String>,
    pub status: DeploymentStatus,
    pub error: String,
    /// Serving traffic now.
    pub current: bool,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DeployRequest {
    /// Commit to deploy (default: the branch head).
    #[ts(optional)]
    pub commit_sha: Option<String>,
    /// Rebuild even if an image for this commit exists.
    #[serde(default)]
    pub force_rebuild: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct RollbackRequest {
    pub deployment_id: String,
}

// ----- volumes ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum VolumeOwner {
    App,
    Database,
}

/// A ready-made service from the catalogue (`/v1/platform/templates`):
/// a compose project with its secrets generated on creation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ServiceTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    /// analytics / storage / monitoring / security / flags
    pub category: String,
    pub website: String,
    /// Memory it needs to run comfortably, for the hardware check.
    pub min_memory_mb: u32,
    /// The compose file (secrets as `${VAR}`).
    pub compose: String,
}

/// Create an app from a template.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct TemplateCreate {
    /// App name (default: the template's id).
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub project_id: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub environment_id: Option<String>,
}

/// A volume with the name of the app or database it belongs to (the
/// Storage page lists every volume of the server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct VolumeInfo {
    pub volume: AppVolume,
    pub owner_name: String,
}

/// Open a terminal in a container.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct TerminalRequest {
    #[serde(default)]
    #[ts(optional)]
    pub cols: Option<u16>,
    #[serde(default)]
    #[ts(optional)]
    pub rows: Option<u16>,
    /// Databases: their own shell (`psql`…) instead of `sh` (default true).
    #[serde(default)]
    #[ts(optional)]
    pub console: Option<bool>,
}

/// An open terminal; read `/v1/platform/terminals/{id}/output`, write `…/input`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct TerminalSession {
    pub id: String,
    /// The container it runs in.
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AppVolume {
    pub id: String,
    pub owner_kind: VolumeOwner,
    pub owner_id: String,
    pub name: String,
    /// Where it is mounted in the container.
    pub mount_path: String,
    pub tier: StorageTier,
    /// Host directory for HDD volumes; None = a Docker named volume.
    pub host_path: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct AppVolumeRequest {
    pub name: String,
    pub mount_path: String,
    #[ts(optional)]
    pub tier: Option<StorageTier>,
}

// ----- servers ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct PlatformNode {
    pub id: String,
    pub name: String,
    /// main / worker
    pub role: String,
    pub agent_url: String,
    pub arch: String,
    /// Builds may run here.
    pub builds: bool,
    pub last_seen_at: Option<String>,
    pub created_at: String,
}

// ----- git sources, hooks, auth, tasks ------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum GitProvider {
    Github,
    Gitlab,
    Gitea,
    Generic,
}

impl GitProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Gitea => "gitea",
            Self::Generic => "generic",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum GitAuthKind {
    None,
    Token,
    DeployKey,
}

/// Credentials for private repositories. The secret is never returned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct GitSource {
    pub id: String,
    pub name: String,
    pub provider: GitProvider,
    pub auth: GitAuthKind,
    /// Deploy keys: the public key to add to the repository (read-only).
    pub public_key: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct GitSourceRequest {
    pub name: String,
    pub provider: GitProvider,
    pub auth: GitAuthKind,
    /// A personal access token (auth = token).
    #[ts(optional)]
    pub token: Option<String>,
}

/// Auto-deploy state of an app (its branch head as last seen).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct GitWatchStatus {
    pub app_id: String,
    pub enabled: bool,
    pub last_head: String,
    pub last_checked_at: Option<String>,
    pub last_error: String,
    pub skipped: u32,
    pub last_skip: String,
}

/// A deploy hook URL (`POST /v1/hooks/deploy/<secret>`); shown once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DeployHook {
    pub id: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    /// Only in the response that created it.
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct BasicAuthRequest {
    /// Empty user removes the password protection.
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ScheduledTask {
    pub id: String,
    pub app_id: String,
    pub name: String,
    /// Cron syntax, UTC (`0 3 * * *` = 03:00 every day).
    pub schedule: String,
    /// Run in the app's container (`sh -c`).
    pub command: String,
    pub enabled: bool,
    pub last_run_at: Option<String>,
    pub last_status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct ScheduledTaskRequest {
    pub name: String,
    pub schedule: String,
    pub command: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// The server's domain setting for new apps and previews.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct PlatformDomain {
    /// `example.com` (apps get `<name>.example.com`), or `<ip>.sslip.io`.
    pub wildcard: Option<String>,
    /// Let's Encrypt certificates on this server.
    pub https: bool,
    /// HTTPS ends at Cloudflare (tunnel mode).
    pub edge_tls: bool,
}

// ----- databases --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum DatabaseKind {
    Postgresql,
    Mysql,
    Mariadb,
    Mongodb,
    Redis,
    Keydb,
    Dragonfly,
    Clickhouse,
}

impl DatabaseKind {
    pub const ALL: [DatabaseKind; 8] = [
        Self::Postgresql,
        Self::Mysql,
        Self::Mariadb,
        Self::Mongodb,
        Self::Redis,
        Self::Keydb,
        Self::Dragonfly,
        Self::Clickhouse,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgresql => "postgresql",
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Mongodb => "mongodb",
            Self::Redis => "redis",
            Self::Keydb => "keydb",
            Self::Dragonfly => "dragonfly",
            Self::Clickhouse => "clickhouse",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct Database {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub environment_id: String,
    pub kind: DatabaseKind,
    pub version: String,
    pub image: String,
    pub node_id: Option<String>,
    /// `postgres://user:password@name:5432/db` for apps of the same project
    /// (password only with `?reveal=true`).
    pub internal_url: Option<String>,
    /// Reachable from the mesh (or everywhere) on this port.
    pub public_port: Option<u16>,
    /// `mesh` or `all`.
    pub public_bind: String,
    pub desired: Desired,
    pub container: String,
    /// Cron schedule of its own backups (empty: only the nightly server backup).
    pub backup_schedule: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DatabaseCreate {
    pub name: String,
    pub kind: DatabaseKind,
    /// Default: the version Vercelify tests with.
    #[ts(optional)]
    pub version: Option<String>,
    #[ts(optional)]
    pub environment_id: Option<String>,
    #[ts(optional)]
    pub project_id: Option<String>,
    #[ts(optional)]
    pub public_port: Option<u16>,
    /// Data on the HDD instead of the SSD.
    #[serde(default)]
    #[ts(optional)]
    pub tier: Option<crate::model::StorageTier>,
    /// Set `DATABASE_URL` (or `REDIS_URL`…) on this app to reference it.
    #[ts(optional)]
    pub link_app: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DatabasePatch {
    #[ts(optional)]
    pub desired: Option<Desired>,
    /// 0 closes the public port.
    #[ts(optional)]
    pub public_port: Option<u16>,
    #[ts(optional)]
    pub public_bind: Option<String>,
    #[ts(optional)]
    pub backup_schedule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[ts(export)]
pub struct DatabaseBackup {
    pub id: String,
    pub database_id: String,
    /// running / succeeded / failed
    pub status: String,
    pub bytes: u64,
    pub snapshot: String,
    pub error: String,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_use_their_wire_names() {
        for s in [
            DeploymentStatus::Queued,
            DeploymentStatus::Building,
            DeploymentStatus::Deploying,
            DeploymentStatus::Healthy,
            DeploymentStatus::Failed,
            DeploymentStatus::Cancelled,
            DeploymentStatus::Superseded,
        ] {
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
            assert_eq!(DeploymentStatus::parse(s.as_str()), Some(s));
        }
        for t in [
            DeploymentTrigger::Push,
            DeploymentTrigger::Manual,
            DeploymentTrigger::Hook,
            DeploymentTrigger::Webhook,
            DeploymentTrigger::Rollback,
            DeploymentTrigger::AutoRollback,
            DeploymentTrigger::Migration,
            DeploymentTrigger::Recover,
        ] {
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
            assert_eq!(DeploymentTrigger::parse(t.as_str()), Some(t));
        }
        for r in [Role::Viewer, Role::Deployer, Role::Owner] {
            assert_eq!(serde_json::to_value(r).unwrap(), r.as_str());
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert!(Role::Owner > Role::Deployer && Role::Deployer > Role::Viewer);
    }

    #[test]
    fn json_columns_tolerate_missing_fields() {
        let r: AppRuntime = serde_json::from_str("{}").unwrap();
        assert_eq!(r, AppRuntime::default());
        assert_eq!(r.replicas, 1);
        let b: AppBuild = serde_json::from_str(r#"{"pack":"node"}"#).unwrap();
        assert_eq!(b.pack, BuildPack::Node);
        let p: PreviewSettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!(p.max, 3);
    }
}
