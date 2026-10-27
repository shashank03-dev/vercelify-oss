-- Why a new commit isn't deployed yet though nothing is wrong (a migration
-- from Coolify not finished, the power guard): set while it holds, cleared
-- by the next check that isn't held.
ALTER TABLE git_watch ADD COLUMN waiting TEXT NOT NULL DEFAULT '';
