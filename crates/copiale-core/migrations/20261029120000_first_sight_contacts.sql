-- 1:1 disappearing message timers, for every peer; presage keeps them
-- only for contacts.
CREATE TABLE peer_timers (
    -- ACI.
    peer BLOB PRIMARY KEY,
    -- Seconds; 0 if off.
    timer INTEGER NOT NULL,
    version INTEGER NOT NULL
);

INSERT INTO peer_timers (peer, timer, version)
SELECT uuid, expire_timer, expire_timer_version FROM contacts;

-- presage saved message senders as contacts, named by their profiles;
-- synced contacts have no profile key. Their names stay as profile names.
INSERT OR IGNORE INTO profiles (uuid, given_name)
SELECT uuid, name FROM contacts
WHERE length(profile_key) > 0 AND name != ''
  AND uuid IN (SELECT uuid FROM profile_keys);

DELETE FROM contacts WHERE length(profile_key) > 0;

-- Asks the phone for contacts at once, restoring those a sender replaced.
DELETE FROM app_meta WHERE key = 'contacts_requested_at';
