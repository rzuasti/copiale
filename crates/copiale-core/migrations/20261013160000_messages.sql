-- One row per 1:1 peer; groups get their own key column later.
CREATE TABLE conversations (
    id INTEGER PRIMARY KEY,
    -- Peer's service ID: 16-byte ACI UUID.
    peer BLOB NOT NULL UNIQUE,
    -- Local time (ms) of the newest message.
    last_activity INTEGER
);

-- The only copy of messages; presage keeps none.
CREATE TABLE messages (
    id INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    -- 1 if sent by this account, from any of its devices.
    outgoing INTEGER NOT NULL,
    -- Sender's timestamp (ms); with the author, identifies the message.
    sent_at INTEGER NOT NULL,
    -- Local time (ms) this device stored it.
    received_at INTEGER NOT NULL,
    body TEXT NOT NULL,
    read INTEGER NOT NULL DEFAULT 0,
    UNIQUE (conversation_id, outgoing, sent_at)
);

CREATE INDEX messages_by_time ON messages (conversation_id, sent_at, id);
CREATE INDEX messages_unread ON messages (conversation_id) WHERE read = 0;
