-- Non-text content as JSON (`Content`); NULL for plain text, which is in
-- `body`.
ALTER TABLE messages ADD COLUMN content TEXT;
