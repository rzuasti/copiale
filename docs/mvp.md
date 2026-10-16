# MVP plan

Scope, UI design, architecture and work items for the first usable release.

## Scope

### In

- Link as a secondary device by scanning a QR code with the phone.
- 1:1 and group chats: send and receive text.
- Start a new 1:1 chat from synced contacts.
- Delivery/read receipts and typing indicators, following the account's
  settings as synced from the phone (off until known). Groups count as
  delivered or read once every member has.
- Edits, remote deletes and quoted replies: render incoming, send own.
- Disappearing messages: honor timers on incoming and outgoing messages.
- Emoji: render incoming as color glyphs; OS emoji input works; no picker.
- Non-text content (images, files, stickers, reactions, calls): muted
  placeholder line, e.g. `[image] open on your phone`, above any caption.
- Group and timer changes: system lines, e.g. `-- alice added bob · 10:04`.
- Dark and light themes: follow the system by default (dark without a
  system preference), overridable in settings.
- Notifications showing the sender name only by default; message content is
  opt-in.
- Runs in the background when the window is closed.
- Unlink and wipe local data.

### Out

- Sending or viewing attachments, emoji picker, reactions, stickers, calls,
  voice notes.
- Message history transfer: chats start empty at link time.
- Group creation and management.
- Custom themes, base16 import, accent override.
- Tray icon, global hotkey, notification quick reply.
- Multiple accounts.
- CI.

## UI design

### Principles

- Minimalist, "terminal on steroids": dense, aligned, keyboard-first.
- References: Plexamp, Slack. Anti-references: Teams, Signal Desktop.
- One adaptive window serves both full-screen use and quick widget-style
  interactions.

### Layout

- Full size: two panes.
  - Left: chat list with unread markers and relative times, account and
    settings button at the bottom. Search is hidden until the pointer reaches
    the sidebar's top edge or `Ctrl/⌘+F`.
  - Right: conversation header, message log, composer.
- Compact (below a width breakpoint): chat list hidden; the header shows the
  current chat; switch chats through the palette.
- Settings, new chat and linking open as overlays, not separate windows.

```
┌──────────────┬──────────────────────────────┐
│              │ # Alice                    ⋯ │
│ ● Alice   2m │──────────────────────────────│
│   Bob    1h  │ alice @ 10:02 > hey          │
│ ● Dev group  │                 you around?  │
│   Mum    3d  │ you @ 10:03 > yep            │
│              │                              │
│              │──────────────────────────────│
│ ⚙  you       │ > message…                   │
└──────────────┴──────────────────────────────┘
```

### Messages

- Styles (setting):
  - **Log** (default): `name @ time > text`, by prominence: text, then name
    (per-contact colors), then time. Consecutive messages from one sender
    are grouped, indented to the first message's text.
  - **Blocks**: Slack-style name header with grouped messages.
  - **Bubbles**: left/right bubbles.
- Status (`…` pending, `✓` sent, `✓✓` delivered, accent `✓✓` read), edited
  marker and disappearing timer shown as small muted glyphs.
- Typing: `alice is typing…` as a line after the last message, replaced by
  their message or removed when they stop; `typing…` in the chat list.
- Quoted replies render as a one-line muted quote above the message, e.g.
  `↳ alice: ship it?`.
- Edits replace the text and add a muted `edited`; remote deletes leave a
  muted `this message was deleted`. "Delete for me" on another device removes
  the message, or the whole chat, here too.
- Disappearing messages show the time left, e.g. `◷ 4m`, or the whole timer
  (`◷ 1w`) until the countdown starts: when an incoming message is read,
  here or on another device, and when an outgoing one is sent. Timer
  changes stay as system lines. Setting timers is out of scope.

### Typography

- Monospace for chrome: timestamps, names, headers, chat list metadata.
- Proportional sans for message bodies.
- Fonts configurable per role (UI, mono, body), plus base size and line
  height. Mono defaults to the system's monospace font (fontconfig on Linux).

### Interaction

- Keyboard-first; mouse fully supported.
- Kinetic scrolling in the log: momentum, rubber band and bounce at the ends
  (native on macOS; off with reduced motion).
- `Ctrl/⌘+K` command palette: switch chat, new chat, settings, theme, quit.
- Other default bindings:

  | Key | Action |
  |---|---|
  | `Ctrl/⌘+N` | New chat |
  | `Ctrl/⌘+F` | Search chats |
  | `Alt+↑/↓` | Previous/next chat |
  | `Ctrl/⌘+,` | Settings |
  | `Esc` | Close overlay; in compact mode, hide window |
  | `Ctrl/⌘+Q` | Quit |

- `↑` in an empty composer edits the last own message.
- `Shift+↑` in an empty composer selects the last message; `↑`/`↓` move,
  `r` replies, `e` edits (own), `Del` deletes, `Esc` returns to the composer.
- Hovering a message shows `reply · edit · delete` on a line under it; delete
  asks `for everyone · for me · cancel` there. Actions use the theme's
  `action` color, kept for things to click; labels like `edited` stay muted. Edits and deletes for everyone only for
  own messages, within 24 hours.
- Replies and edits show a line above the composer (`↳ replying to alice: …`,
  `editing your message`); `Esc` cancels.
- `Ctrl/⌘+N` opens a contact picker over the window: fuzzy search by name
  or number, `↑`/`↓` and `Enter` or a click to pick, `Esc` or a click
  outside to close. Picking opens the existing chat, or a new one; chats
  without messages are listed only while open.

### Quick mode and background

- Closing the window keeps the app running and connected; quitting is
  explicit (palette, `Ctrl/⌘+Q`, `copiale --quit`).
- Single instance. The CLI talks to the running instance:
  - `copiale --toggle`: show and focus the composer, or hide if focused.
  - `copiale --quit`.
- Users bind `--toggle` to a key in their compositor/WM or OS.
- Notification click opens the window on that chat.

### Window chrome

- Custom minimal titlebar on macOS, Windows and GNOME-style desktops.
- Server-side or no decorations on tiling WMs.

### Settings

`config.toml` in the platform config dir (`~/.config/copiale/` on Linux) is
the source of truth (`copiale_core::config`).

- The settings UI edits it with `toml_edit`, keeping comments and order;
  writes are atomic and go through symlinks (dotfile managers).
- External edits apply live.
- A bad value falls back to its default with a warning naming the setting;
  a syntax error means all defaults. Loading never fails.

```toml
[appearance]
theme = "system"          # system | dark | light

[typography]
# ui_font, mono_font, body_font: font names; unset = built-in default
size = 14                 # px, 9-32
line_height = 1.4         # 1.0-2.0

[layout]
density = "cozy"          # compact | cozy | roomy
message_style = "log"     # log | blocks | bubbles
timestamps = "always"     # always | hover | never
clock = "24h"             # 24h | 12h
avatars = false

[window]
opacity = 1.0             # background, 0.5-1.0
blur = false              # compositor-dependent

[notifications]
content = "name"          # off | name | message
```

The settings UI also shows account settings, which live with the account,
not in the file:

| Group | Settings |
|---|---|
| Privacy | Read receipts and typing indicators (mirror account settings) |
| Account | Device name, unlink and wipe |

## Architecture

### Crates

```
crates/
  copiale-core/     Signal adapter, store, config, keychain, actor, models
  copiale-ui/       views, view models, theme, settings UI
  copiale-testkit/  fake Signal backend, fixtures (dev-dependency)
  copiale/          binary: CLI, single-instance IPC, wiring
```

### Core

- Runs as an actor on its own thread: current-thread tokio runtime with a
  `LocalSet`, since presage's futures are not `Send`.
- UI sends `Command`s and receives `Event`s over async channels; the UI side
  drains events with `cx.spawn`.
- A `SignalBackend` trait wraps presage's `Manager`; `copiale-testkit`
  provides a fake implementation with scripted network responses.
- Models are plain data (`ConversationSummary`, `Message`, ...), shared
  with the UI as snapshots (`Conversations`, `Messages`) and updates
  (`MessageAdded`, `ConversationUpdated`).
- Startup: unlock storage, load or link the account, then serve.
- Serving keeps the connection open, reconnecting with backoff (1 s doubling
  to 60 s; reset once the queue drains); `Reconnect` retries at once.
  Connecting runs in the background and gives up after 30 s.
- A dead network can leave the connection looking open for minutes, so the
  core asks the server who it is every 20 s and reconnects if there is no
  answer within 10 s.
- Sending stores the message as pending, then sends in a background task,
  one message at a time and in order, so receiving never waits on it.
  Messages wait while disconnected; a network error mid-send reconnects and
  sends again with the same timestamp, so recipients drop duplicates. Other
  errors, or 10 minutes without sending, mark the message failed; failed
  sends are retried on request. Sends cut off by quitting are marked failed
  on the next start. libsignal-service copies each sent message to the
  account's other devices.
- Disappearing messages carry their sender's timer; ours take the
  conversation's timer from presage, which also applies it when sending.
  The core sleeps until the next one is due, checking the wall clock at
  least every minute (timers may not count suspend), deletes it and reports
  it removed. Overdue ones go at startup; ones sent from the phone and
  already gone there are never stored.
- Messages, including those sent from the phone, are stored once: Signal
  identifies a message by author and sent timestamp, so redeliveries are
  ignored. Order is by sent time; the chat list by local receive time.
- Contact sync is requested after linking, while no contacts are stored,
  and at startup a day after the last request; the phone also pushes
  changes. Synced names, else cached profile names, title conversations.

### Storage

- One SQLCipher database holding presage's SQLite store
  (`presage-store-sqlite`) and Copiale's own tables
  (`docs/spike-sqlcipher-results.md`).
- Copiale's migrations are tracked in `_copiale_migrations`, apart from
  presage's `_sqlx_migrations`.
- Key: random 256-bit key stored in the OS keychain (`keyring`), passed to
  SQLCipher as a raw key.
- No keychain available: prompt for a passphrase at startup and derive the
  raw key with Argon2id (64 MiB, 3 passes, 4 lanes); salt and parameters live
  in `copiale.key` next to the database, which also marks passphrase mode.
- Files: `copiale.db` and `copiale.key` in the platform data dir
  (`~/.local/share/copiale` on Linux), restricted to the user.
- Messages live in Copiale's tables only, with edits, deletes and expiry
  applied. presage's own message saving (`thread_messages`) is turned off by
  a delegating `Store` wrapper.
- `secure_delete` overwrites deleted rows rather than leaving them in free
  pages.
- Tests use temporary databases and an in-memory keychain; never real Signal
  servers or the OS keychain.

### Platform

- Config file watching: `notify`.
- Single-instance IPC: Unix socket / named pipe (`interprocess`).
- Notifications: `notify-rust` (Linux/macOS), WinRT toasts (Windows).
- Logging (`copiale_core::logging`): `tracing` to stderr only, never to
  disk.
  - `COPIALE_LOG` sets the filter; default `info` for Copiale, `warn` for
    dependencies (`error` for `zbus`, which warns about missing D-Bus
    services on minimal desktops).
  - Dependencies are capped at `warn` regardless: presage and libsignal log
    message contents at `debug`. `COPIALE_UNSAFE_LOG=1` lifts the cap, for
    test accounts only.
  - Identifiers go through redacting types (`ServiceId`, `PhoneNumber`,
    `GroupId`); bodies, keys and passphrases never print.
  - Backstop for dependency logs: all output is scrubbed of UUIDs, runs of
    32+ hex digits and E.164 numbers (kept: last 4 or 2 digits), and of
    `Debug` byte lists of 16+ bytes, such as server responses.
  - SQLCipher's own logging is off (`cipher_log_level = NONE`).

### UI

- GPUI with GPUI Kit.
- Views are thin; state and logic live in view models covered by tests.
- Theme tokens loaded from built-in dark/light TOML; system appearance
  observed live.

## Risks to verify early

- presage has no Storage Service support: a linked device learns about a
  group only when a message arrives in it.
- GPUI: transparent/blurred window background and client-side decorations per
  platform.
- Wayland focus stealing: `--toggle` needs an `xdg-activation` token to raise
  the window.
- macOS and Windows spike checks still pending (IME, scaling).

## Work items

Built as a walking skeleton: one thin end-to-end slice, then widen.

Tracked as [Codeberg issues](https://codeberg.org/korba/copiale/issues);
issue numbers match the item numbers below.

### M0: Foundations

1. Workspace scaffold: crates, flake build, fmt/clippy/test passing.
2. Spike: `presage-store-sqlite` on SQLCipher.
3. Core actor skeleton: `Command`/`Event`, `SignalBackend` trait, testkit
   fake.
4. Encrypted store: open, keychain key, passphrase fallback, migrations.
5. Config: TOML schema, defaults, validation, hot reload.
6. Logging with redaction.

### M1: Walking skeleton

7. Link device via QR (core and UI).
8. Receive 1:1 text: backend → store → UI.
9. Send 1:1 text.
10. App shell: two-pane layout, chat list, log-style conversation, composer.
11. Dark/light themes following system appearance.

### M2: Messaging

12. Groups: send and receive.
13. Placeholders for non-text content and group updates.
14. Receipts and typing indicators.
15. Incoming edits, deletes and quoted replies.
16. Sending edits, deletes and replies.
17. Disappearing message timers.
18. New 1:1 chat from contacts.
19. Notifications (name only by default).

### M3: Experience

20. Command palette and keyboard navigation.
21. Adaptive compact layout.
22. Single instance, background running, `--toggle`/`--quit`.
23. Custom titlebar and decorations.
24. Settings UI: theme override, typography, layout, window, notifications.
25. Blocks and bubbles message styles.
26. Unlink and wipe local data.

### M4: Release prep

27. Nix package and NixOS module.
28. macOS and Windows checks (IME, scaling, transparency, notifications).
