-- Our own attachments are copied in when sent, before they're uploaded:
-- `pointer` is NULL until then.
CREATE TABLE attachments_new (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    -- Where it is on Signal's servers, with its key (`AttachmentPointer`).
    pointer BLOB,
    -- `remote`, `downloading`, `ready`, `failed`, `expired` or
    -- `unavailable`.
    state TEXT NOT NULL,
    -- Its encrypted copy in `attachments/`, and the key.
    file TEXT,
    file_key BLOB,
    -- A video's first frame: `wanted` until tried, then `ready` or
    -- `failed`; NULL for other attachments.
    preview TEXT,
    preview_file TEXT,
    preview_key BLOB,
    UNIQUE (message_id, position)
);

INSERT INTO attachments_new
SELECT id, message_id, position, pointer, state, file, file_key, preview, preview_file,
       preview_key
FROM attachments;

DROP TABLE attachments;
ALTER TABLE attachments_new RENAME TO attachments;

CREATE TRIGGER attachment_deleted AFTER DELETE ON attachments
BEGIN
    INSERT OR IGNORE INTO deleted_files (file) SELECT old.file WHERE old.file IS NOT NULL;
    INSERT OR IGNORE INTO deleted_files (file)
        SELECT old.preview_file WHERE old.preview_file IS NOT NULL;
END;
