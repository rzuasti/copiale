-- Delivery of a sent message: 'delivered' or 'read'; NULL until receipts
-- arrive from the recipient, or from every member of a group.
ALTER TABLE messages ADD COLUMN delivery TEXT CHECK (delivery IN ('delivered', 'read'));

-- Receipts per recipient of a group message.
CREATE TABLE receipts (
    message_id INTEGER NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    -- Recipient's ACI.
    recipient BLOB NOT NULL,
    -- 1 once read, 0 if only delivered.
    read INTEGER NOT NULL,
    PRIMARY KEY (message_id, recipient)
);
