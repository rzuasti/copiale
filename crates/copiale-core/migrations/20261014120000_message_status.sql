-- Progress of outgoing messages: 'pending', 'sent' or 'failed'; NULL for
-- incoming ones.
ALTER TABLE messages ADD COLUMN status TEXT
    CHECK (status IN ('pending', 'sent', 'failed'));

UPDATE messages SET status = 'sent' WHERE outgoing = 1;
