-- Identity keys verified on the phone, synced from it.
CREATE TABLE verified_identities (
    -- ACI.
    peer BLOB PRIMARY KEY,
    -- The key the phone verified, as Signal encodes it.
    identity_key BLOB NOT NULL
);
