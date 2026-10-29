-- NULL preserves the exact storage locations used before this migration.
-- New volumes get an immutable identity outside the legacy vfy-* namespace.
ALTER TABLE volumes ADD COLUMN storage_name TEXT;
CREATE UNIQUE INDEX volumes_storage_name ON volumes(storage_name) WHERE storage_name IS NOT NULL;
