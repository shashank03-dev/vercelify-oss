ALTER TABLE database_backups ADD COLUMN manifest TEXT;
CREATE TABLE restore_drills (
 id TEXT PRIMARY KEY,
 database_id TEXT REFERENCES databases(id) ON DELETE SET NULL,
 source_id TEXT NOT NULL,
 backup_id TEXT NOT NULL,
 snapshot TEXT NOT NULL,
 source_name TEXT NOT NULL,
 manifest TEXT NOT NULL,
 assertions BLOB NOT NULL,
 credential_source TEXT NOT NULL DEFAULT 'host',
 recovery BLOB,
 state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','deferred','running','verifying','cleanup','succeeded','failed','cancelled')),
 error TEXT NOT NULL DEFAULT '',
 integrity TEXT NOT NULL DEFAULT 'unknown',
 full_integrity INTEGER NOT NULL DEFAULT 0,
 cancel_requested INTEGER NOT NULL DEFAULT 0,
 attempts INTEGER NOT NULL DEFAULT 0,
 assertion_names TEXT NOT NULL DEFAULT '[]',
 verified_at TEXT,
 cleanup_pending INTEGER NOT NULL DEFAULT 0,
 bytes INTEGER NOT NULL,
 restore_seconds INTEGER,
 rpo_seconds INTEGER,
 created_at TEXT NOT NULL,
 started_at TEXT,
 finished_at TEXT
);
CREATE INDEX restore_drills_active ON restore_drills(state);
CREATE TABLE restore_drill_services (
 drill_id TEXT PRIMARY KEY REFERENCES restore_drills(id) ON DELETE RESTRICT,
 database_id TEXT NOT NULL UNIQUE REFERENCES databases(id) ON DELETE RESTRICT,
 network TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('active','closing'))
);
CREATE TABLE restore_drill_schedules (
 database_id TEXT PRIMARY KEY REFERENCES databases(id) ON DELETE CASCADE,
 enabled INTEGER NOT NULL DEFAULT 0,
 schedule TEXT NOT NULL DEFAULT '0 3 1 * *',
 assertions BLOB NOT NULL,
 created_at TEXT NOT NULL,
 last_due TEXT,
 last_job TEXT,
 error TEXT NOT NULL DEFAULT ''
);
