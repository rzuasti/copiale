# Secrets in memory

How Copiale keeps the database key and passphrase from leaking out of
process memory, and the copies it can't control. Last, what notifications
leave with the OS, and keeping the window out of screen captures.

## Protections

- **No core dumps** (`copiale_core::hardening`), first thing at startup:
  - Unix: core size limit set to zero, which systemd-coredump and apport
    honor too.
  - Not `PR_SET_DUMPABLE` on Linux: it makes `/proc/<pid>` root-owned,
    and xdg-desktop-portal then rejects every request ("Unable to open
    /proc/<pid>/root"). It wouldn't stop piped dumps with
    `fs.suid_dumpable = 2` anyway.
  - Windows: the executable is excluded from Windows Error Reporting for
    the user (`WerAddExcludedApplication`), and reports still made leave
    out the heap. Dumps an administrator enables through `LocalDumps` are
    still written.
  - `COPIALE_UNSAFE_CORE_DUMPS=1` skips it, for debugging crashes. On
    Windows the exclusion stays in the registry; use `LocalDumps`.
- **Database closed on every exit from the core**, panics included:
  SQLCipher frees its global state at exit, crashing connections still
  closing.
- **Key kept on this machine**: on Windows, a Local credential;
  Credential Manager's default, Enterprise, roams with domain profiles.
- **Kept out of swap** (`hardening::LockedBytes`): `DatabaseKey` and
  `Passphrase`, including the UI's copy read from the input, sit on pages
  of their own, locked in RAM (`mlock`, `VirtualLock`) and, on Linux, left
  out of core dumps (`MADV_DONTDUMP`). New keys come straight from the
  OS's random number generator; keychain keys and derived keys are written
  straight into locked memory.
  - Best effort: when locking fails (e.g. `RLIMIT_MEMLOCK` reached), a
    warning is logged and the secret stays unlocked.
  - Hibernation images hold all memory, locked or not: only disk
    encryption protects them.
- **Zeroed on drop**: `DatabaseKey`, `Passphrase`, the keychain secret, and
  Argon2's working memory.

## Remaining copies

Not zeroed; freed memory keeps them until reused. Not locked either, so
they can also reach swap.

- **Key, in sqlx.** `SqliteConnectOptions::pragma` only takes an owned
  string, with no hook before the pragmas run, so the hex key stays in:
  - each pool's connect options (presage's and Copiale's), for the life of
    the pool: new connections need it;
  - a `PRAGMA` statement string sqlx builds for each new connection, and
    SQLite's parsed copy of it.
- **Key, in SQLCipher**: per connection, as encryption needs it.
- **Key, in the keychain client** (`keyring`, the OS service and D-Bus on
  Linux), and the secret it returns, zeroed but not locked, until it's
  copied into a `DatabaseKey`.
- **Key and passphrase, in Argon2**: its working memory (256 MiB, zeroed,
  too large to lock) and hashing state.
- **Passphrase, in GPUI Kit's input**: the rope holding the text, and each
  edit's old chunks. Clearing the input after sending drops the text and
  its undo history; Copiale reads it straight from the rope into a zeroed
  string, skipping `InputState::value`'s copy.
- **New passphrase, in zxcvbn**: the strength check copies it and pieces
  of it (lowercased, matched tokens).

## Notifications

Shown notifications belong to the OS, outside the encrypted database.

- **What they reveal** (`notifications.content`): nothing (`off`), chat
  and sender names (`name`, the default), or message text too (`message`).
  Never the text of disappearing messages, nor a request's name or text.
- **Linux**: the freedesktop server holds them. Copiale closes a chat's
  notification once it's read or its message is deleted or disappears;
  some servers keep closed ones in a history of their own.
- **macOS**: Notification Center keeps them in an unencrypted database in
  the user's profile.
- **Windows**: Action Center keeps them in an unencrypted database
  (`%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db`).
- **macOS and Windows**: not yet replaced or closed (#28), so read,
  deleted and expired messages stay listed until dismissed.

## Screen capture

`window.capture_protection` (off by default) asks the OS to leave the
window out of screenshots and screen shares. Notifications stay visible.

- **Windows**: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, from
  Windows 10 2004; earlier versions log a warning and capture as usual.
- **macOS**: `NSWindow.sharingType = .none`. Since macOS 15, ScreenCaptureKit
  doesn't reliably honor it.
- **Linux**: Wayland and X11 offer apps no way to ask, so the setting shows
  as unavailable. Hyprland (0.50+) can do it with a window rule, blacking
  out the window in shares and screenshots:

  ```lua
  hl.window_rule({ match = { class = "page.codeberg.korba.Copiale" }, no_screen_share = true })
  ```

  Before 0.55, see the hyprlang syntax in the 0.54 wiki.
- Nothing stops a camera or a compromised OS.
