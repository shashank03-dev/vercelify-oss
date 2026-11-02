-- A queued deployment can wait for an explicitly selected computer/CI image.
CREATE TABLE build_jobs (
  deployment_id TEXT PRIMARY KEY REFERENCES deployments(id) ON DELETE CASCADE,
  owner TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation > 0),
  platform TEXT NOT NULL CHECK (platform IN ('linux/arm64', 'linux/amd64')),
  revision TEXT NOT NULL,
  plan_hash TEXT NOT NULL,
  plan_version INTEGER NOT NULL DEFAULT 1 CHECK(plan_version = 1),
  plan TEXT NOT NULL CHECK (json_valid(plan)),
  phase TEXT NOT NULL CHECK (phase IN ('leased', 'uploading', 'verified', 'failed')),
  deadline INTEGER NOT NULL,
  hard_deadline INTEGER NOT NULL,
  image_digest TEXT NOT NULL DEFAULT ''
);
