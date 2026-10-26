-- A video's first frame, from the sandboxed helper: `wanted` until tried,
-- then `ready` or `failed`; NULL for other attachments.
ALTER TABLE attachments ADD COLUMN preview TEXT;
-- Its encrypted copy in `attachments/`, and the key.
ALTER TABLE attachments ADD COLUMN preview_file TEXT;
ALTER TABLE attachments ADD COLUMN preview_key BLOB;

UPDATE attachments SET preview = 'wanted'
WHERE EXISTS (
    SELECT 1 FROM messages m
    WHERE m.id = attachments.message_id AND (
        json_extract(m.content, '$.text.attachments[' || attachments.position || '].kind')
            = 'video'
        OR json_extract(
            m.content, '$.text.attachments[' || attachments.position || '].content_type'
        ) LIKE 'video/%'
    )
);

DROP TRIGGER attachment_deleted;
CREATE TRIGGER attachment_deleted AFTER DELETE ON attachments
BEGIN
    INSERT OR IGNORE INTO deleted_files (file) SELECT old.file WHERE old.file IS NOT NULL;
    INSERT OR IGNORE INTO deleted_files (file)
        SELECT old.preview_file WHERE old.preview_file IS NOT NULL;
END;
