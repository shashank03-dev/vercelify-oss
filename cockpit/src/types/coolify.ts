// Subset of the Coolify v4 API (verified against Coolify 4.4).

/** "running:healthy", "exited:unhealthy", "restarting", ... */
export type CoolifyStatus = string;

export interface CoolifyApplication {
  uuid: string;
  name: string;
  description?: string | null;
  fqdn: string | null;
  status: CoolifyStatus;
  git_repository: string | null;
  git_branch: string | null;
  git_commit_sha?: string | null;
  build_pack: string;
  docker_registry_image_name?: string | null;
  docker_registry_image_tag?: string | null;
  created_at: string;
  updated_at: string;
  last_online_at?: string | null;
  environment_id: number;
  preview_url_template?: string | null;
}

export interface CoolifyDeployment {
  id: number;
  deployment_uuid: string;
  application_id?: string;
  application_name?: string;
  status: "queued" | "in_progress" | "finished" | "failed" | "cancelled-by-user" | string;
  commit: string | null;
  commit_message?: string | null;
  pull_request_id: number;
  is_webhook: boolean;
  is_api?: boolean;
  rollback?: boolean;
  created_at: string;
  updated_at: string;
  finished_at?: string | null;
  deployment_url?: string;
  server_name?: string;
  /** JSON-encoded array of log entries (only on /deployments/{uuid}). */
  logs?: string;
}

export interface CoolifyDeploymentLog {
  output: string;
  type: "stdout" | "stderr" | string;
  timestamp: string;
  hidden: boolean;
  batch?: number;
  order?: number;
}

/** An application environment variable (GET /applications/{uuid}/envs). */
export interface CoolifyEnv {
  uuid: string;
  key: string;
  value: string | null;
  is_preview: boolean;
  is_build_time?: boolean;
}

export interface CoolifyPreview {
  uuid: string;
  pull_request_id: number;
  pull_request_html_url: string | null;
  status: CoolifyStatus;
  fqdn: string | null;
  created_at: string;
}

export interface CoolifyDatabase {
  uuid: string;
  name: string;
  status: CoolifyStatus;
  database_type: string;
  image: string;
  internal_db_url?: string | null;
  external_db_url?: string | null;
  is_public: boolean;
  public_port: number | null;
  environment_id: number;
  created_at?: string;
}

export interface CoolifyProject {
  id?: number;
  uuid: string;
  name: string;
  description?: string | null;
  environments?: { id: number; name: string; uuid: string }[];
}

export interface CoolifyServer {
  uuid: string;
  name: string;
  ip: string;
  user?: string;
  port?: number;
  settings?: { is_reachable?: boolean; is_usable?: boolean; is_build_server?: boolean };
}

export type DatabaseKind = "postgresql" | "mysql" | "mariadb" | "mongodb" | "redis" | "keydb" | "dragonfly" | "clickhouse";
