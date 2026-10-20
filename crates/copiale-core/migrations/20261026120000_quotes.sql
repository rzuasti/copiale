-- Quotes keep only which message they quote; what it says comes from
-- storage.
UPDATE messages
SET content = json_remove(content, '$.text.quote.text', '$.text.quote.attachment')
WHERE json_type(content, '$.text.quote') = 'object';

UPDATE pending_changes
SET content = json_remove(content, '$.text.quote.text', '$.text.quote.attachment')
WHERE json_type(content, '$.text.quote') = 'object';

-- Finds a message's replies.
CREATE INDEX messages_by_quote
ON messages (conversation_id, json_extract(content, '$.text.quote.sent_at'));
