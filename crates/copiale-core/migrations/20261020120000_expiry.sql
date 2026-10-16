-- Disappearing messages: the timer (seconds), and when the message is
-- deleted (ms); expires_at is NULL until the countdown starts.
ALTER TABLE messages ADD COLUMN expires_in INTEGER;
ALTER TABLE messages ADD COLUMN expires_at INTEGER;

CREATE INDEX messages_expiring ON messages (expires_at) WHERE expires_at IS NOT NULL;
