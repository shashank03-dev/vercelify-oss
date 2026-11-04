CREATE TABLE guest_domains(domain TEXT PRIMARY KEY);
CREATE TABLE guest_security(id INTEGER PRIMARY KEY CHECK(id=1),clock INTEGER NOT NULL DEFAULT 0,key_probe BLOB);
INSERT INTO guest_security(id) VALUES(1);
CREATE TABLE guest_grants(
 id TEXT PRIMARY KEY,
 app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
 pr_number INTEGER NOT NULL,
 deployment_id TEXT NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
 host TEXT NOT NULL UNIQUE,
 token_hash TEXT NOT NULL UNIQUE,
 created_by TEXT NOT NULL,
 created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 single_use INTEGER NOT NULL DEFAULT 1,
 redeemed INTEGER NOT NULL DEFAULT 0,
 revoked_at INTEGER
);
CREATE INDEX guest_grants_preview ON guest_grants(app_id,pr_number);
CREATE TABLE guest_sessions(
 token_hash TEXT PRIMARY KEY,
 grant_id TEXT NOT NULL REFERENCES guest_grants(id) ON DELETE CASCADE,
 created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL
);
CREATE INDEX guest_sessions_grant ON guest_sessions(grant_id);
