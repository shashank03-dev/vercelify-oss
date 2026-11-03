-- Ownership survives app/PR deletion until external cleanup succeeds. A service
-- cannot be deleted through the generic DB API while the journal still owns it.
CREATE TABLE preview_db_services (
    app_id TEXT PRIMARY KEY,
    database_id TEXT NOT NULL UNIQUE REFERENCES databases(id) ON DELETE RESTRICT,
    network TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('active','closing')),
    created_at TEXT NOT NULL
);
CREATE TABLE preview_databases (
    id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL,
    app_name TEXT NOT NULL,
    preview_id TEXT NOT NULL,
    pr_number INTEGER NOT NULL CHECK(pr_number > 0),
    service_id TEXT NOT NULL,
    database_name TEXT NOT NULL UNIQUE,
    username TEXT NOT NULL UNIQUE,
    password BLOB NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','provisioned','ready','closing','deleted')),
    deployment_id TEXT NOT NULL DEFAULT '',
    error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX preview_database_live ON preview_databases(app_id,pr_number)
    WHERE state != 'deleted';
