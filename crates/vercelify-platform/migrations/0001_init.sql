-- Vercelify platform state, version 1. See docs/design/SYSTEM-DESIGN.md §4.
-- Timestamps are RFC 3339 UTC text. JSON columns are validated by json_valid().
-- Names that become hostnames or container names are DNS labels.

CREATE TABLE settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL CHECK (json_valid(value)),
  updated_at TEXT NOT NULL
);

CREATE TABLE nodes (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL UNIQUE,
  role         TEXT NOT NULL CHECK (role IN ('main', 'worker')),
  agent_url    TEXT NOT NULL,
  mesh_node_id TEXT REFERENCES mesh_nodes(id) ON DELETE SET NULL,
  arch         TEXT NOT NULL DEFAULT '',
  builds       INTEGER NOT NULL DEFAULT 1 CHECK (builds IN (0, 1)),
  labels       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(labels)),
  last_seen_at TEXT,
  created_at   TEXT NOT NULL
);

CREATE TABLE mesh_nodes (
  id              TEXT PRIMARY KEY,
  name            TEXT NOT NULL UNIQUE,
  kind            TEXT NOT NULL CHECK (kind IN ('server', 'device')),
  public_key      TEXT NOT NULL UNIQUE,
  ip_index        INTEGER NOT NULL UNIQUE CHECK (ip_index BETWEEN 1 AND 65534),
  role            TEXT NOT NULL DEFAULT 'owner' CHECK (role IN ('owner', 'deployer', 'viewer')),
  os              TEXT NOT NULL DEFAULT '',
  arch            TEXT NOT NULL DEFAULT '',
  token_hash      TEXT NOT NULL UNIQUE,
  endpoints       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(endpoints)),
  cert_expires_at TEXT,
  revoked_at      TEXT,
  last_seen_at    TEXT,
  created_at      TEXT NOT NULL
);

CREATE TABLE projects (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 80),
  description TEXT NOT NULL DEFAULT '',
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE environments (
  id         TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 40),
  created_at TEXT NOT NULL,
  UNIQUE (project_id, name)
);

-- App and database names share one namespace: both become hostnames
-- (<name>.<domain>) and container names.
CREATE TABLE resource_names (
  name       TEXT PRIMARY KEY
             CHECK (length(name) BETWEEN 1 AND 40
                    AND name NOT GLOB '*[^a-z0-9-]*'
                    AND name NOT GLOB '-*' AND name NOT GLOB '*-'),
  owner_kind TEXT NOT NULL CHECK (owner_kind IN ('app', 'database')),
  owner_id   TEXT NOT NULL UNIQUE
);

CREATE TABLE apps (
  id                    TEXT PRIMARY KEY,
  environment_id        TEXT NOT NULL REFERENCES environments(id) ON DELETE RESTRICT,
  name                  TEXT NOT NULL UNIQUE REFERENCES resource_names(name),
  kind                  TEXT NOT NULL CHECK (kind IN ('git', 'image', 'compose')),
  source                TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(source)),
  build                 TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(build)),
  runtime               TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(runtime)),
  previews              TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(previews)),
  node_id               TEXT REFERENCES nodes(id) ON DELETE SET NULL,
  desired               TEXT NOT NULL DEFAULT 'running' CHECK (desired IN ('running', 'stopped', 'sleeping')),
  current_deployment_id TEXT,
  created_at            TEXT NOT NULL,
  updated_at            TEXT NOT NULL
);
CREATE INDEX apps_environment ON apps(environment_id);

CREATE TABLE app_domains (
  id          TEXT PRIMARY KEY,
  app_id      TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  host        TEXT NOT NULL UNIQUE CHECK (length(host) BETWEEN 1 AND 253 AND host = lower(host)),
  https       INTEGER NOT NULL DEFAULT 1 CHECK (https IN (0, 1)),
  is_primary  INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1)),
  redirect_to TEXT,
  cert_status TEXT NOT NULL DEFAULT 'pending' CHECK (cert_status IN ('pending', 'issued', 'failed', 'edge', 'none')),
  created_at  TEXT NOT NULL
);
CREATE INDEX app_domains_app ON app_domains(app_id);

CREATE TABLE env_vars (
  id           TEXT PRIMARY KEY,
  scope        TEXT NOT NULL CHECK (scope IN ('project', 'environment', 'app')),
  scope_id     TEXT NOT NULL,
  key          TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 256 AND key NOT GLOB '*[^A-Za-z0-9_]*' AND key NOT GLOB '[0-9]*'),
  value        BLOB NOT NULL,
  build        INTEGER NOT NULL DEFAULT 0 CHECK (build IN (0, 1)),
  preview_only INTEGER NOT NULL DEFAULT 0 CHECK (preview_only IN (0, 1)),
  secret       INTEGER NOT NULL DEFAULT 1 CHECK (secret IN (0, 1)),
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL,
  UNIQUE (scope, scope_id, key, preview_only)
);

CREATE TABLE deployments (
  id             TEXT PRIMARY KEY,
  app_id         TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  number         INTEGER NOT NULL CHECK (number > 0),
  kind           TEXT NOT NULL CHECK (kind IN ('production', 'preview')),
  pr_number      INTEGER CHECK (pr_number IS NULL OR pr_number > 0),
  branch         TEXT NOT NULL DEFAULT '',
  commit_sha     TEXT NOT NULL DEFAULT '',
  commit_message TEXT NOT NULL DEFAULT '',
  trigger        TEXT NOT NULL CHECK (trigger IN ('push', 'manual', 'hook', 'webhook', 'rollback', 'auto-rollback', 'migration', 'recover')),
  rollback_of    TEXT REFERENCES deployments(id) ON DELETE SET NULL,
  image          TEXT NOT NULL DEFAULT '',
  -- Port the image listens on (detected at build), so a rollback needs no build.
  port           INTEGER CHECK (port IS NULL OR port BETWEEN 1 AND 65535),
  framework      TEXT NOT NULL DEFAULT '',
  node_id        TEXT REFERENCES nodes(id) ON DELETE SET NULL,
  status         TEXT NOT NULL CHECK (status IN ('queued', 'building', 'deploying', 'healthy', 'failed', 'cancelled', 'superseded')),
  error          TEXT NOT NULL DEFAULT '',
  container      TEXT NOT NULL DEFAULT '',
  created_at     TEXT NOT NULL,
  started_at     TEXT,
  finished_at    TEXT,
  UNIQUE (app_id, number),
  CHECK ((kind = 'preview') = (pr_number IS NOT NULL))
);
CREATE INDEX deployments_app ON deployments(app_id, number DESC);
CREATE INDEX deployments_active ON deployments(status) WHERE status IN ('queued', 'building', 'deploying');

CREATE TABLE previews (
  id            TEXT PRIMARY KEY,
  app_id        TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  pr_number     INTEGER NOT NULL CHECK (pr_number > 0),
  head_sha      TEXT NOT NULL DEFAULT '',
  title         TEXT NOT NULL DEFAULT '',
  url           TEXT NOT NULL DEFAULT '',
  deployment_id TEXT REFERENCES deployments(id) ON DELETE SET NULL,
  status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'closed')),
  created_at    TEXT NOT NULL,
  closed_at     TEXT,
  UNIQUE (app_id, pr_number)
);

CREATE TABLE databases (
  id              TEXT PRIMARY KEY,
  environment_id  TEXT NOT NULL REFERENCES environments(id) ON DELETE RESTRICT,
  name            TEXT NOT NULL UNIQUE REFERENCES resource_names(name),
  kind            TEXT NOT NULL CHECK (kind IN ('postgresql', 'mysql', 'mariadb', 'mongodb', 'redis', 'keydb', 'dragonfly', 'clickhouse')),
  version         TEXT NOT NULL,
  image           TEXT NOT NULL,
  node_id         TEXT REFERENCES nodes(id) ON DELETE SET NULL,
  credentials     BLOB NOT NULL,
  public_port     INTEGER CHECK (public_port IS NULL OR public_port BETWEEN 1024 AND 65535),
  public_bind     TEXT NOT NULL DEFAULT 'mesh' CHECK (public_bind IN ('mesh', 'all')),
  desired         TEXT NOT NULL DEFAULT 'running' CHECK (desired IN ('running', 'stopped')),
  container       TEXT NOT NULL DEFAULT '',
  backup_schedule TEXT NOT NULL DEFAULT '',
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);

CREATE TABLE database_backups (
  id          TEXT PRIMARY KEY,
  database_id TEXT NOT NULL REFERENCES databases(id) ON DELETE CASCADE,
  status      TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed')),
  bytes       INTEGER NOT NULL DEFAULT 0,
  snapshot    TEXT NOT NULL DEFAULT '',
  error       TEXT NOT NULL DEFAULT '',
  created_at  TEXT NOT NULL,
  finished_at TEXT
);
CREATE INDEX database_backups_db ON database_backups(database_id, created_at DESC);

CREATE TABLE volumes (
  id         TEXT PRIMARY KEY,
  owner_kind TEXT NOT NULL CHECK (owner_kind IN ('app', 'database')),
  owner_id   TEXT NOT NULL,
  name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 64 AND name NOT GLOB '*[^a-z0-9_.-]*'),
  mount_path TEXT NOT NULL CHECK (substr(mount_path, 1, 1) = '/'),
  tier       TEXT NOT NULL DEFAULT 'ssd' CHECK (tier IN ('ssd', 'hdd')),
  host_path  TEXT,
  created_at TEXT NOT NULL,
  UNIQUE (owner_kind, owner_id, name)
);

CREATE TABLE git_sources (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL UNIQUE,
  provider   TEXT NOT NULL CHECK (provider IN ('github', 'gitlab', 'gitea', 'generic')),
  base_url   TEXT NOT NULL DEFAULT '',
  auth       TEXT NOT NULL CHECK (auth IN ('none', 'token', 'deploy_key', 'github_app')),
  secret     BLOB,
  public_key TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL
);

CREATE TABLE deploy_hooks (
  id           TEXT PRIMARY KEY,
  app_id       TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  secret_hash  TEXT NOT NULL UNIQUE,
  created_at   TEXT NOT NULL,
  last_used_at TEXT
);

CREATE TABLE scheduled_tasks (
  id          TEXT PRIMARY KEY,
  app_id      TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
  name        TEXT NOT NULL,
  schedule    TEXT NOT NULL,
  command     TEXT NOT NULL,
  enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  last_run_at TEXT,
  last_status TEXT NOT NULL DEFAULT '',
  created_at  TEXT NOT NULL,
  UNIQUE (app_id, name)
);
