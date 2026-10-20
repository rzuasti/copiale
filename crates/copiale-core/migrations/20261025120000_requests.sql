-- Accounts and groups the user accepted or blocked, here, on the phone or
-- in Storage Service. A conversation with neither, and messages from the
-- other side, is a message request.
CREATE TABLE chat_states (
    -- A 1:1 peer's ACI (16 bytes), or a group's public ID (32 bytes).
    chat BLOB PRIMARY KEY,
    accepted INTEGER NOT NULL DEFAULT 0,
    blocked INTEGER NOT NULL DEFAULT 0
);

-- Conversations from before message requests stay as they were.
INSERT INTO chat_states (chat, accepted)
SELECT coalesce(peer, group_id), 1 FROM conversations;

-- Phone numbers the phone blocked without an ACI; kept to sync the list
-- back whole.
CREATE TABLE blocked_numbers (
    number TEXT PRIMARY KEY
);
