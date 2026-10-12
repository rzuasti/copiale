# Spike results: presage store on SQLCipher

Issue #2. Prototype: `spikes/sqlcipher` (run instructions in its `NOTES.md`).

**Status:** Linux done. Storage plan confirmed with two adjustments (raw key,
separate migrations table).

## Decision

One SQLCipher database holding presage's store and Copiale's tables works.

- Open it through `SqliteStore::open_with_options` with a **raw key**
  (`PRAGMA key = "x'…'"`): no SQLCipher key derivation on open.
- Run Copiale's sqlx migrations with their own tracking table
  (`Migrator::dangerous_set_table_name("_copiale_migrations")`). Sharing
  `_sqlx_migrations` breaks presage on the next open.
- Copy presage's `[patch.crates-io]` for `libsqlite3-sys` (whisperfish fork)
  into the root workspace.

## Versions

| Component | Version |
|---|---|
| presage, presage-store-sqlite | git `94b3978` (2026-10-12), `0.8.0-dev` |
| sqlx | 0.9.0 |
| libsqlite3-sys | 0.36.0, whisperfish fork `2a42b33`, `bundled-sqlcipher` (presage default) |
| SQLCipher | 4.10.0 community, OpenSSL provider |

presage is not on crates.io; pin the git revision.

## Checks (Linux, release)

| Check | Result |
|---|---|
| SQLCipher linked | Pass: `cipher_version` 4.10.0, provider `openssl` |
| presage store opens with raw key | Pass |
| App migrations in `_copiale_migrations`, presage reopens | Pass |
| App migrations rerun after presage's | Pass |
| presage and app tables in one database | Pass (22 tables) |
| Control: app migrations in `_sqlx_migrations` | presage fails: `migration 20261013000000 was previously applied but is missing` |
| No SQLite header in the file | Pass |
| Plaintext marker absent from database and WAL bytes | Pass |
| Right key reads data back | Pass |
| Wrong key / no key / presage with wrong key | Fail as expected: `(code: 26) file is not a database` |
| Open cost, median of 5 | Raw key 0.3 ms; passphrase (PBKDF2) 77 ms |

## presage coverage

Pinned revision, read from source.

| Feature | Receive | Send |
|---|---|---|
| Edits | `EditMessage` content stored as-is; applying it is ours | `send_message` with `EditMessage` |
| Remote deletes | Applied to presage's message store | `DataMessage.delete` |
| Delete for me (sync) | Applied to presage's message store | Not needed |
| Receipts | `ReceiptMessage` in the content stream, stored | `send_message` with `ReceiptMessage` |
| Typing | Content stream only, not stored | `send_message` with `TypingMessage` |
| Disappearing timers | Timer and version tracked per contact and group | Applied automatically by `send_message` |
| Contact sync | `request_contacts`, then `Received::Contacts`; stored | — |
| Linking | `link_secondary_device` yields the provisioning URL over a oneshot channel (render as QR) | — |
| Group list | **Gap:** no Storage Service; a group appears only when a message arrives in it | — |

Expiring messages (deleting them at the deadline) is Copiale's job.

## Findings

- **Futures are not `Send`.** `Manager::receive_messages` holds `Rc` and
  `dyn Future` values, so presage must run on one thread: a current-thread
  tokio runtime with a `LocalSet`, as `presage-cli` does. The core actor
  owns that thread (#3).
- **presage stores messages itself** (`threads`, `thread_messages`), as raw
  protobuf content. Copiale's view of a conversation (applied edits, receipt
  state) needs its own tables; decide in #4/#8 whether to keep bodies in both
  or read presage's.
- **SQLCipher logs to stderr** on a wrong key (`ERROR CORE … hmac check
  failed`). No secrets, but route or silence it (`PRAGMA cipher_log_level`)
  in #6.
- **Crypto provider:** `bundled-sqlcipher` links system OpenSSL. Fine with
  Nix; macOS and Windows need OpenSSL available or
  `bundled-sqlcipher-vendored-openssl` (#28).
- **Passphrase fallback:** derive the raw key with Argon2id ourselves (#4)
  rather than giving SQLCipher a passphrase; avoids 77 ms of PBKDF2 per
  pooled connection.
- **CDSI** (phone number lookup) is behind presage's `cdsi` feature, which
  switches SQLCipher to custom crypto. Not needed for the MVP: synced
  contacts carry service IDs.

## Follow-ups

- #3: actor on a dedicated thread with a current-thread runtime and
  `LocalSet`.
- #4: raw key, `_copiale_migrations`, workspace `[patch]`.
- #6: SQLCipher log routing.
- #12: groups appear as messages arrive; no full group list until Storage
  Service support exists.
