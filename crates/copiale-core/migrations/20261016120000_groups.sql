-- no-transaction
-- Rebuilds both tables, which SQLite can't alter in place: conversations
-- gain groups, messages their author. Foreign keys are off so dropping the
-- old tables doesn't cascade.
PRAGMA foreign_keys = OFF;

BEGIN;

-- One row per 1:1 peer or group.
CREATE TABLE conversations_new (
    id INTEGER PRIMARY KEY,
    -- 1:1: peer's service ID, 16-byte ACI UUID.
    peer BLOB UNIQUE,
    -- Groups v2: master key (32 bytes, secret) and the public ID derived
    -- from it.
    group_master_key BLOB UNIQUE,
    group_id BLOB UNIQUE,
    -- Group title and state revision as last fetched; NULL until then.
    group_title TEXT,
    group_revision INTEGER,
    -- Local time (ms) of the newest message.
    last_activity INTEGER,
    CHECK ((peer IS NULL) != (group_master_key IS NULL))
);

INSERT INTO conversations_new (id, peer, last_activity)
SELECT id, peer, last_activity FROM conversations;

-- The only copy of messages; presage keeps none.
CREATE TABLE messages_new (
    id INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    -- 1 if sent by this account, from any of its devices.
    outgoing INTEGER NOT NULL,
    -- Sender's ACI; empty for this account.
    author BLOB NOT NULL,
    -- Sender's timestamp (ms); with the author, identifies the message.
    sent_at INTEGER NOT NULL,
    -- Local time (ms) this device stored it.
    received_at INTEGER NOT NULL,
    body TEXT NOT NULL,
    read INTEGER NOT NULL DEFAULT 0,
    -- Progress of outgoing messages: 'pending', 'sent' or 'failed'; NULL
    -- for incoming ones.
    status TEXT CHECK (status IN ('pending', 'sent', 'failed')),
    UNIQUE (conversation_id, author, sent_at)
);

INSERT INTO messages_new
    (id, conversation_id, outgoing, author, sent_at, received_at, body, read, status)
SELECT m.id, m.conversation_id, m.outgoing,
       CASE WHEN m.outgoing THEN x'' ELSE c.peer END,
       m.sent_at, m.received_at, m.body, m.read, m.status
FROM messages m JOIN conversations c ON c.id = m.conversation_id;

DROP TABLE messages;
DROP TABLE conversations;
ALTER TABLE conversations_new RENAME TO conversations;
ALTER TABLE messages_new RENAME TO messages;

CREATE INDEX messages_by_time ON messages (conversation_id, sent_at, id);
CREATE INDEX messages_unread ON messages (conversation_id) WHERE read = 0;

-- A group's other members, as last fetched.
CREATE TABLE group_members (
    conversation_id INTEGER NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    -- ACI.
    member BLOB NOT NULL,
    -- Contact or profile name, if known.
    name TEXT,
    -- Order within the group.
    position INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, member)
);

COMMIT;

PRAGMA foreign_keys = ON;
