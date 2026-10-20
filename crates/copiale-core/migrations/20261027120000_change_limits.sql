-- When the server received others' messages and kept changes (ms); others'
-- edits and deletes apply only within a window of the message's. Older
-- rows use the local time they arrived.
ALTER TABLE messages ADD COLUMN server_at INTEGER;
UPDATE messages SET server_at = received_at WHERE outgoing = 0;

ALTER TABLE pending_changes ADD COLUMN server_at INTEGER;
UPDATE pending_changes SET server_at = received_at;

-- How many edits a message took; capped.
ALTER TABLE messages ADD COLUMN edits INTEGER NOT NULL DEFAULT 0;
UPDATE messages SET edits = 1 WHERE edited_at IS NOT NULL;
