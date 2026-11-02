-- Previews no longer inherit production variables (design §5.1). A normal
-- variable reaches previews only when shared deliberately; every existing one
-- starts as production-only, so an upgrade never widens what pull-request
-- code can read. The cockpit lists the keys previews now go without.
ALTER TABLE env_vars ADD COLUMN share_with_previews INTEGER NOT NULL DEFAULT 0
  CHECK (share_with_previews IN (0, 1));

-- A running preview still holds its old credentials and network attachments.
-- Persist the retirement list across crashes; startup cleans it before work
-- resumes. Owners redeploy after reviewing preview values.
INSERT INTO settings (key, value, updated_at)
SELECT 'preview_safety_retire', json_group_array(json_array(app_id, pr_number)),
       strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM previews WHERE status = 'active';
