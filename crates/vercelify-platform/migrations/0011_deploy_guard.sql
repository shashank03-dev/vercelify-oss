-- Control metadata only; no environment values or credentials are stored here.
CREATE TABLE deployment_configs (
    deployment_id TEXT PRIMARY KEY REFERENCES deployments(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL
);
CREATE TABLE deployment_guards (
    deployment_id TEXT PRIMARY KEY REFERENCES deployments(id) ON DELETE CASCADE,
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    previous_id TEXT NOT NULL,
    deadline INTEGER NOT NULL,
    grace_until INTEGER NOT NULL,
    failure_limit INTEGER NOT NULL,
    failures INTEGER NOT NULL DEFAULT 0,
    baseline TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'watching'
      CHECK(state IN ('watching','requested','verified','failed','cancelled','expired')),
    reason TEXT NOT NULL DEFAULT '',
    rollback_id TEXT,
    requested_at INTEGER,
    finished_at INTEGER
);
CREATE INDEX deployment_guards_active ON deployment_guards(state);
-- A process restart must not lose a scheduled drain/retirement.
CREATE TABLE deployment_retirements (
    node_id TEXT NOT NULL,
    container TEXT NOT NULL,
    deployment_id TEXT NOT NULL,
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    not_before INTEGER NOT NULL,
    PRIMARY KEY(node_id,container)
);
