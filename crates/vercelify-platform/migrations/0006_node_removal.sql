-- Removing a server from hosting (nodes::remove) leaves its mesh identity,
-- which the owner revokes separately. Without a mark here its next heartbeat
-- registered it again as a new, undrained worker. Set on removal, cleared
-- when the machine pairs again.
ALTER TABLE mesh_nodes ADD COLUMN hosting_removed_at TEXT;
