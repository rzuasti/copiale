-- Groups an admin ended: nothing more is sent there. `group_end_checked`
-- once the group was fetched since ends were tracked, as earlier ones
-- left no trace.
ALTER TABLE conversations ADD COLUMN group_ended INTEGER NOT NULL DEFAULT 0;
ALTER TABLE conversations ADD COLUMN group_end_checked INTEGER NOT NULL DEFAULT 0;
