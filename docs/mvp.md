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
- Emoji: render as color glyphs, one to five alone drawn larger; OS
  emoji input works. The composer converts emoticons (`:)`, optional)
  and `:shortcodes:`, and `:` plus two letters lists matching emoji.
  `Ctrl+E` or the composer's `:)` opens a searchable picker by category,
  recently used first (kept in the database).
- Non-text content (stickers, reactions, calls): muted placeholder line,
  e.g. `[sticker] open on your phone`, above any caption.
- Received attachments (after the MVP, #87): images and GIFs drawn above
  the caption, albums as a grid of up to four; videos as their first
  frame once downloaded (Linux and macOS), before that their blurred
  preview, with `▶`, played with sound in the viewer; other files as
  cards: a page icon with the file's type (`PDF`), the name, then
  `size · type · download/save`.
  Not yet downloaded: the blurred preview with `↓ size`. Attachments that
  can't be downloaded keep a placeholder line.
- Sending attachments (#91): `Ctrl/⌘+O` or the composer's paperclip picks
  files; dropping files on the chat or pasting images or copied files adds
  them too. They wait in a tray above the composer (thumbnail, name,
  size, `original` for pictures, `✕`); `Enter` sends them with the
  composer's text as one message, up to 32 files of 100 MB each. Pictures
  go re-encoded without their metadata unless marked `original`.
- Group and timer changes: system lines, e.g. `-- alice added bob · 10:04`.
- Dark and light themes: follow the system by default (dark without a
  system preference), overridable in settings with a built-in theme
  (Catppuccin, Dracula, Everforest, Gruvbox, Kanagawa, Nord, One, Rosé Pine,
  Solarized, Tokyo Night), or a custom one in `config.toml`.
- Notifications showing the sender name only by default; message content is
  opt-in.
- Runs in the background when the window is closed, with a tray icon.
- Unlink and wipe local data.

### Out

- Sending attachments, reactions, stickers, calls, voice notes.
- Message history transfer: chats start empty at link time.
- Group creation and management.
- Base16 import, accent override.
- Global hotkey, notification quick reply.
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
- Compact (below 640 px): chat list hidden; the header shows the current
  chat, a count of other chats with unread messages and the connection
  unless online; switch chats through the palette (`Ctrl/⌘+F` opens it) or
  `Alt+↑/↓`. The chat list slides out and back while resizing; the log
  keeps its place (the end, or the top message).
- A failure to read or save local data shows in red above the connection
  (compact: `⚠` in the header) until clicked, as something may not have
  been saved.
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
  - **Blocks**: Slack-style name header with grouped messages; the time
    sits in the header.
  - **Bubbles**: incoming on the left, ours on the right, with the time and
    glyphs inside; the sender's name heads each group.
  - Avatars, when on, show in the chat list only.
  - Day separators, system lines, quotes and actions look alike in every
    style; switching keeps the scroll position.
- Status (`…` pending, `✓` sent, `✓✓` delivered, accent `✓✓` read), edited
  marker and disappearing timer shown as small muted glyphs.
- Typing: `alice is typing…` as a line after the last message, replaced by
  their message or removed when they stop; `typing…` in the chat list.
- Quoted replies render as a one-line muted quote above the message, e.g.
  `↳ alice: ship it?`, showing the quoted message as stored now, or
  `↳ original message not found`.
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

- Keyboard-first; mouse fully supported. `Tab` moves focus in forms, with
  a visible focus ring.
- Kinetic scrolling in the log: momentum, rubber band and bounce at the ends
  (native on macOS; off with reduced motion).
- `Ctrl/⌘+K` command palette, over the window like the contact picker:
  fuzzy search over other chats (most recent first, at most 5) and commands
  with their keys: new chat, settings, theme (opens the theme picker), quit. `Ctrl/⌘+K` again, `Esc` or a click outside
  closes it. One overlay shows at a time.
- In search, `↑`/`↓` move the highlight and `Enter` opens the chat.
- Other default bindings:

  | Key | Action |
  |---|---|
  | `Ctrl/⌘+N` | New chat |
  | `Ctrl/⌘+F` | Search chats |
  | `Alt+↑/↓` | Previous/next chat |
  | `Ctrl/⌘+,` | Settings (until #24: points to `config.toml`) |
  | `Esc` | Close overlay; in compact mode, then hide the window |
  | `Ctrl/⌘+Q` | Quit |

- `↑` in an empty composer edits the last own message.
- `Shift+↑` in an empty composer selects the last message; `↑`/`↓` move,
  `r` replies, `e` edits (own), `Del` deletes, `Enter` views its pictures
  (else downloads its attachments), `s` saves its attachments, `Esc`
  returns to the composer.
- The picture viewer covers the window: `←`/`→` move through the message's
  pictures and videos, `s` saves, `Esc` or a click outside closes. A video
  plays at once: `Space` (or a click on it) pauses, `←`/`→` move 5 s,
  `Shift+←`/`→` move between the message's pictures and videos, `m`
  mutes; the bar seeks. Clicking a picture or video opens it; clicking
  anything else downloads or saves it.
- Saving asks where: a file name for one attachment, a folder for several
  (numbered rather than replacing files), starting in Downloads.
- Hovering a message shows `reply · edit · download · save · delete` on a
  line under it (download and save when it has attachments to); delete
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
  explicit (palette, `Ctrl/⌘+Q`, `copiale --quit`). On Unix, SIGINT,
  SIGTERM and SIGHUP quit the same way; a second one ends the process at
  once.
- Hiding closes the window, as GPUI can't hide one; chats, drafts and
  onboarding carry over to the next. On Linux the app also goes headless
  while hidden, and reconnects with the requesting process's
  `XDG_ACTIVATION_TOKEN` so the new window takes focus on Wayland.
- On Wayland, an open but unfocused window is raised by reopening it:
  compositors may ignore activation requests (Hyprland by default) but
  focus new windows. With a token it reopens through a new connection.
- Single instance. The CLI talks to the running instance:
  - `copiale`: show the window.
  - `copiale --toggle`: show and focus the composer, or hide if focused.
  - `copiale --quit`.
  - `copiale --hidden`: start without showing the window, as if hidden at
    once (e.g. at login); does nothing if already running.
- `copiale --check-config [FILE]` lists problems in `FILE`, or in the
  config in effect, and fails if there are any; runs without an instance.
- Users bind `--toggle` to a key in their compositor/WM or OS.
- Notification click opens the window on that chat; on macOS, so does the
  Dock icon.
- Tray icon (placeholder artwork until there's an app icon):
  - Click: hides an open window, else shows it; clicking the tray takes
    focus, so focus doesn't count here.
  - Menu: `Show Copiale`/`Hide Copiale`, `Quit`.
  - A dot while any chat has unread messages.
  - Linux: StatusNotifierItem; GNOME needs the AppIndicator extension.
    Clicks carry no activation token, so Wayland may not focus the window.

### Notifications

- `notifications.content`: `off`, `name` (default: the chat's name and
  "New message", or "New message from alice" in groups) or `message` (the
  text, or its placeholder; never a disappearing message's). Changes apply
  live.
- One per chat, updated as messages arrive ("3 new messages"), closed once
  the chat is read or the message is deleted or disappears.
- None for the chat open in the focused window, for reactions and system
  lines, or for messages already read.
- Messages queued while offline notify once, after the queue drains:
  "5 new messages in 2 chats".
- Bodies are escaped: freedesktop servers may read them as markup.
- What the OS keeps: `docs/security.md`.

### Window chrome

- Custom minimal titlebar on macOS, Windows and GNOME-style desktops: a thin
  strip across the window with the app name; drag, double-click and window
  controls behave natively (traffic lights on macOS, native hit testing on
  Windows, window menu on right click on Linux).
- Linux asks for server-side decorations: KDE keeps its own titlebar, tiling
  WMs draw none. The titlebar shows only when the compositor leaves
  decorations to the client (GNOME).

### Settings

`config.toml` in the platform config dir (`~/.config/copiale/` on Linux) is
the source of truth (`copiale_core::config`).

- On Linux and BSDs, `copiale/config.toml` in each `$XDG_CONFIG_DIRS` dir
  (`/etc/xdg`) holds system-wide defaults (the NixOS module's): the user's
  file overrides them setting by setting, `[theme]` key by key. Read at
  start and with each reload of the user's file; never written.
- A read-only file (Home Manager's, a link into the Nix store) isn't
  written: settings UI changes last until restart, and its footer says so.

- The settings UI edits it with `toml_edit`, keeping comments and order;
  writes are atomic and go through symlinks (dotfile managers).
- External edits apply live.
- A bad value falls back to its default with a warning naming the setting;
  a syntax error means all defaults. Loading never fails.

```toml
[appearance]
theme = "system"          # system | copiale-dark | copiale-light | nord | …
                          # | custom (reads [theme])

# Only for theme = "custom": every token of a theme file, no name; see
# crates/copiale-ui/themes/dark.toml. Unusable: the system theme.
# [theme]
# appearance = "dark"
# [theme.colors]
# background = "#16181d"
# …

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

[composer]
convert_emoticons = true  # :) becomes 🙂 as you type

[window]
opacity = 1.0             # background, 0.5-1.0
blur = false              # compositor-dependent
capture_protection = false # Windows, macOS; see docs/security.md

[notifications]
content = "name"          # off | name | message
```

The settings UI also shows account settings, which live with the account,
not in the file:

| Group | Settings |
|---|---|
| Privacy | Read receipts and typing indicators (mirror account settings) |
| Account | Device name, unlink and wipe |

Unlinking and wiping:

- **Unlink and wipe** (settings, Account) asks first, shows "Unlinking…"
  while it removes the device from the account, then deletes the database,
  salt and keychain key, keeps `config.toml`, and returns to the link
  screen. If removing fails (e.g.
  offline, 10 s timeout), local data goes anyway and the link screen says to
  remove the device on the phone. Skipped once the server rejected the
  credentials: the phone already removed it. If the keychain can't be
  reached, the key stays there and the next screen says which entry to
  delete; so does "Start over" after a storage failure.
- When the server rejects this device's credentials (the phone unlinked
  it), the core stops reconnecting and the app shows "This device was
  unlinked": wipe and link again, or try again in case it was a glitch.
  Nothing is deleted until confirmed.

The settings UI (`Ctrl+,` or the palette) is one keyboard-driven list:

- Sections in file order, then Privacy and Account; typing filters rows.
- `↑`/`↓` move, `←`/`→` change, `Enter` toggles or cycles; clicks work too.
- Fonts open a fuzzy picker over installed fonts plus "Default"; moving
  previews live, `Enter` saves, `Esc` restores.
- Every change applies at once and is written to `config.toml`.
- Privacy and device name are read-only mirrors of the phone's settings.
- Fonts that aren't installed fall back to the default with a warning.

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
- Once connected, the core asks the phone for contacts (daily, hourly while
  none have come) and settings (until known), in the background.
- Each message is acknowledged to the server once stored (or dropped for
  good); the server sends unacknowledged ones again on the next connection.
  Losing the connection or quitting first stores what was already
  decrypted, as it won't decrypt again; what's still unread is left to the
  server. The receive task decrypts only a few messages ahead, never
  cancels a decryption, and gives the websocket a moment to send the
  acknowledgements.
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
  raw key with Argon2id (256 MiB, 3 passes, 4 lanes); salt and parameters live
  in `copiale.key` next to the database, which also marks passphrase mode.
  New passphrases need 8 characters and a zxcvbn score of 4 (over 10^10
  guesses).
- Files: `copiale.db` and `copiale.key` in the platform data dir
  (`~/.local/share/copiale` on Linux), restricted to the user.
- Messages live in Copiale's tables only, with edits, deletes and expiry
  applied. presage's own message saving (`thread_messages`) is turned off by
  a delegating `Store` wrapper.
- Decryption retries, as in Signal's clients: a message that fails to
  decrypt is asked for again (`DecryptionErrorMessage`) and shows as a
  placeholder until it arrives. Our sends are kept a day (`send_log`) with
  the devices they reached, to resend when one of those devices asks;
  deleted and disappeared messages are forgotten at once.
- `secure_delete` overwrites deleted rows rather than leaving them in free
  pages.
- Tests use temporary databases and an in-memory keychain; never real Signal
  servers or the OS keychain.

### Platform

- Config file watching: `notify`.
- Single-instance IPC (`copiale` crate): the first process holds
  `copiale.lock` and listens on `copiale.sock`, in the runtime dir
  (`$XDG_RUNTIME_DIR/copiale` on Linux, else the data dir), private to the
  user. On Windows, a named pipe named after the user's SID, owned by and
  open only to them; clients check the server runs as them. One request
  line per connection
  (`show`, `toggle`, `quit`, with an optional activation token), answered
  `ok` or `error`.
- Tray: `tray-icon`, with `ksni` (D-Bus, no GTK) on Linux.
- Notifications, on their own thread: `notify-rust` for the freedesktop
  D-Bus service (Linux) and WinRT toasts (Windows); `mac-usernotifications`
  for UNUserNotificationCenter (macOS, bundled app only). Replacing and
  closing work on Linux and macOS; Windows waits for #28.
- Logging (`copiale_core::logging`): `tracing` to stderr only, never to
  disk.
  - `COPIALE_LOG` sets the filter; default `info` for Copiale, `warn` for
    dependencies (`error` for `zbus`, which warns about missing D-Bus
    services on minimal desktops).
  - Logs go to stderr only, but the systemd journal may keep them: they
    must not identify contacts.
  - Dependencies are capped at `warn` regardless: presage and libsignal log
    message contents at `debug`.
  - Panics log their location only; string-literal messages are kept.
  - `COPIALE_UNSAFE_LOG=1` lifts the cap and logs panic messages, for test
    accounts only.
  - Identifiers go through redacting types (`ServiceId`, `PhoneNumber`,
    `GroupId`) showing a per-run tag (keyed hash, random key per run);
    bodies, keys and passphrases never print.
  - Backstop for dependency logs: all output is scrubbed of UUIDs, runs of
    32+ hex digits and E.164 numbers (replaced by the same tags), and of
    `Debug` byte lists of 16+ bytes, such as server responses.
  - SQLCipher's own logging is off (`cipher_log_level = NONE`).

### UI

- GPUI with GPUI Kit.
- Views are thin; state and logic live in view models covered by tests.
- Theme tokens loaded from built-in TOML themes or `[theme]`; system
  appearance observed live.

## Risks to verify early

- presage has no Storage Service support upstream; a vendored patch reads
  it, for accept and block state and verifications only. A linked device
  still learns about a group only when a message arrives in it.
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
28. Windows checks (IME, scaling, transparency, notifications).
79. macOS release: app bundle, GitHub CI, local signing and notarization,
    release command, checks (#80–#85).
