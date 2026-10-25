-- Multi-node (design §8.2-8.3): workers register over the mesh and the main
-- server places app instances on them.

-- The token the main server calls a worker's runtime API with (sealed).
ALTER TABLE nodes ADD COLUMN token BLOB;
ALTER TABLE nodes ADD COLUMN mesh_ip TEXT NOT NULL DEFAULT '';
-- The last heartbeat's NodeStatus.
ALTER TABLE nodes ADD COLUMN status TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(status));
-- No new placements; what runs keeps running.
ALTER TABLE nodes ADD COLUMN drained INTEGER NOT NULL DEFAULT 0 CHECK (drained IN (0, 1));

-- The containers of a deployment and where they run (replicas: several).
CREATE TABLE instances (
  id            TEXT PRIMARY KEY,
  deployment_id TEXT NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
  node_id       TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  container     TEXT NOT NULL,
  replica       INTEGER NOT NULL DEFAULT 0 CHECK (replica BETWEEN 0 AND 31),
  created_at    TEXT NOT NULL,
  UNIQUE (deployment_id, container)
);
CREATE INDEX instances_node ON instances(node_id);
