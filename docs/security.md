# Secrets in memory

How Copiale keeps the database key and passphrase from leaking out of
process memory, and the copies it can't control.

## Protections

- **No core dumps** (`copiale_core::hardening`), first thing at startup:
  - Unix: core size limit set to zero.
  - Linux: also non-dumpable (`PR_SET_DUMPABLE`), which covers dump handlers
    that ignore the limit, and blocks `ptrace` and `perf` from other
    processes of the user.
  - macOS: the limit only.
  - Windows: nothing yet; Windows Error Reporting may keep a dump.
  - `COPIALE_UNSAFE_DEBUG=1` skips all of it, for `gdb` and `perf -p`.
- **Database closed on every exit from the core**, panics included:
  SQLCipher frees its global state at exit, crashing connections still
  closing.
- **Zeroed on drop**: `DatabaseKey`, `Passphrase`, the keychain secret, and
  Argon2's working memory.

## Remaining copies

Not zeroed; freed memory keeps them until reused.

- **Key, in sqlx.** `SqliteConnectOptions::pragma` only takes an owned
  string, with no hook before the pragmas run, so the hex key stays in:
  - each pool's connect options (presage's and Copiale's), for the life of
    the pool: new connections need it;
  - a `PRAGMA` statement string sqlx builds for each new connection, and
    SQLite's parsed copy of it.
- **Key, in SQLCipher**: per connection, as encryption needs it.
- **Key, in the keychain client** (`keyring`, the OS service and D-Bus on
  Linux).
- **Passphrase, in GPUI Kit's input**: the rope holding the text, and each
  edit's old chunks. Clearing the input after sending drops the text and
  its undo history; Copiale reads it straight from the rope into a zeroed
  string, skipping `InputState::value`'s copy.
