CREATE TABLE capacity_samples (
    node_id TEXT PRIMARY KEY,
    sample TEXT NOT NULL
);
CREATE TABLE resource_reservations (
    operation TEXT NOT NULL,
    node_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    memory_mb INTEGER NOT NULL CHECK(memory_mb >= 0),
    disk_mb INTEGER NOT NULL CHECK(disk_mb >= 0),
    heavy INTEGER NOT NULL CHECK(heavy IN (0,1)),
    settled_at INTEGER,
    recovering INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(operation,node_id,owner)
);
CREATE TABLE resource_placements (
    operation TEXT PRIMARY KEY,
    need TEXT NOT NULL,
    slots TEXT NOT NULL
);

-- Preserve existing containers and their spec hashes on upgrade.
ALTER TABLE databases ADD COLUMN memory_mb INTEGER CHECK(memory_mb IS NULL OR memory_mb > 0);
