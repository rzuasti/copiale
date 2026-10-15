-- Sent time of the latest edit; NULL if never edited.
ALTER TABLE messages ADD COLUMN edited_at INTEGER;

-- Edits and deletes that arrived before their message; applied when it
-- does, dropped after a day.
CREATE TABLE pending_changes (
    id INTEGER PRIMARY KEY,
    -- The message's author's ACI; empty for this account.
    author BLOB NOT NULL,
    -- The message's sent time.
    target INTEGER NOT NULL,
    -- New content as JSON (`Content`) and the edit's sent time; NULL
    -- content for a delete.
    content TEXT,
    edited_at INTEGER,
    -- Local time (ms) it arrived.
    received_at INTEGER NOT NULL
);

CREATE INDEX pending_changes_by_target ON pending_changes (author, target);
