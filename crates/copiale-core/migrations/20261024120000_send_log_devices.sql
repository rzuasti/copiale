-- Retry requests are answered only from devices a send reached; sends
-- logged before kept no devices, so they're dropped.
DROP TABLE send_log_recipients;
DELETE FROM send_log;

-- Devices a logged send reached.
CREATE TABLE send_log_recipients (
    log_id INTEGER NOT NULL REFERENCES send_log (id) ON DELETE CASCADE,
    -- ACI.
    recipient BLOB NOT NULL,
    device INTEGER NOT NULL,
    PRIMARY KEY (log_id, recipient, device)
);
