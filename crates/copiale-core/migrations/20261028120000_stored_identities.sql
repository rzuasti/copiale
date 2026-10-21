-- Verifications last read from Storage Service: a record applies only
-- when it changes, so a key change confirmed here stays confirmed.
CREATE TABLE stored_identities (
    -- ACI.
    peer BLOB PRIMARY KEY,
    -- The record's key, as Signal encodes it.
    identity_key BLOB NOT NULL,
    verified INTEGER NOT NULL
);
