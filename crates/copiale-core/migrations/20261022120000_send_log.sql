-- Our recent sends as they went out, to send again when a recipient can't
-- decrypt them; kept a day, and forgotten with the message.
CREATE TABLE send_log (
    id INTEGER PRIMARY KEY,
    -- The send's timestamp (ms), which retry requests name.
    sent_at INTEGER NOT NULL,
    -- Sent time of the message it sent, edited or deleted.
    message_sent_at INTEGER NOT NULL,
    -- Encoded by the backend.
    content BLOB NOT NULL,
    -- Local time (ms) it was sent.
    logged_at INTEGER NOT NULL
);

CREATE INDEX send_log_by_sent_at ON send_log (sent_at);
CREATE INDEX send_log_by_message ON send_log (message_sent_at);
CREATE INDEX send_log_by_age ON send_log (logged_at);

-- Accounts a logged send reached.
CREATE TABLE send_log_recipients (
    log_id INTEGER NOT NULL REFERENCES send_log (id) ON DELETE CASCADE,
    -- ACI.
    recipient BLOB NOT NULL,
    PRIMARY KEY (log_id, recipient)
);

-- Deleted or disappeared messages of ours aren't sent again.
CREATE TRIGGER send_log_forget_removed AFTER DELETE ON messages
WHEN OLD.author = x''
BEGIN
    DELETE FROM send_log WHERE message_sent_at = OLD.sent_at;
END;

CREATE TRIGGER send_log_forget_deleted AFTER UPDATE OF content ON messages
WHEN OLD.author = x'' AND NEW.content = '"deleted"'
BEGIN
    DELETE FROM send_log WHERE message_sent_at = OLD.sent_at;
END;
