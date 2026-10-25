-- Attachments that can be downloaded, by position in their message's
-- `content` list, which describes them.
CREATE TABLE attachments (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    -- Where it is on Signal's servers, with its key (`AttachmentPointer`).
    pointer BLOB NOT NULL,
    -- `remote`, `downloading`, `ready`, `failed` or `expired`.
    state TEXT NOT NULL,
    -- Its encrypted copy in `attachments/`, once downloaded, and the key.
    file TEXT,
    file_key BLOB,
    UNIQUE (message_id, position)
);

-- Copies whose attachment is gone, still to delete from disk.
CREATE TABLE deleted_files (
    file TEXT PRIMARY KEY
);

CREATE TRIGGER attachment_deleted AFTER DELETE ON attachments
WHEN old.file IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO deleted_files (file) VALUES (old.file);
END;
