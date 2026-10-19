-- Finds a message by author and sent time across conversations, as
-- receipts and reads on other devices do.
CREATE INDEX messages_by_author ON messages (author, sent_at);
