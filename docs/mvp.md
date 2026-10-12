# MVP plan

Scope, UI design, architecture and work items for the first usable release.

## Scope

### In

- Link as a secondary device by scanning a QR code with the phone.
- 1:1 and group chats: send and receive text.
- Start a new 1:1 chat from synced contacts.
- Delivery/read receipts and typing indicators, following the account's
  settings.
- Edits, remote deletes and quoted replies: render incoming, send own.
- Disappearing messages: honor timers on incoming and outgoing messages.
- Emoji: render incoming as color glyphs; OS emoji input works; no picker.
- Non-text content (images, files, stickers, reactions, calls, group
  updates): muted placeholder line, e.g. `[image] open on your phone`.
- Dark and light themes: follow the system by default, overridable in
  settings.
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
  - Left: search, chat list with unread markers and relative times, account
    and settings button at the bottom.
  - Right: conversation header, message log, composer.
- Compact (below a width breakpoint): chat list hidden; the header shows the
  current chat; switch chats through the palette.
- Settings, new chat and linking open as overlays, not separate windows.

```
┌──────────────┬──────────────────────────────┐
│ ▸ search     │ # Alice                    ⋯ │
│──────────────│──────────────────────────────│
│ ● Alice   2m │ 10:02  alice │ hey           │
│   Bob    1h  │              │ you around?   │
│ ● Dev group  │ 10:03    you │ yep           │
│   Mum    3d  │                              │
│              │──────────────────────────────│
│ ⚙  you       │ > message…                   │
└──────────────┴──────────────────────────────┘
```

### Messages

- Styles (setting):
  - **Log** (default): timestamp, right-aligned name, rule, text; consecutive
    messages from one sender grouped.
  - **Blocks**: Slack-style name header with grouped messages.
  - **Bubbles**: left/right bubbles.
- Status (sent, delivered, read), edited marker and disappearing timer shown as
  small muted glyphs.
- Quoted replies render as a one-line muted quote above the message.

### Typography

- Monospace for chrome: timestamps, names, headers, chat list metadata.
- Proportional sans for message bodies.
- Fonts configurable per role (UI, mono, body), plus base size and line
  height.

### Interaction

- Keyboard-first; mouse fully supported.
- `Ctrl/⌘+K` command palette: switch chat, new chat, settings, theme, quit.
- Other default bindings:

  | Key | Action |
  |---|---|
  | `Ctrl/⌘+N` | New chat |
  | `Alt+↑/↓` | Previous/next chat |
  | `Ctrl/⌘+,` | Settings |
  | `Esc` | Close overlay; in compact mode, hide window |
  | `Ctrl/⌘+Q` | Quit |

- `↑` in an empty composer edits the last own message.

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

`config.toml` in the platform config dir is the source of truth. The
settings UI edits it with `toml_edit`, preserving comments; external edits
apply live.

| Group | Knobs |
|---|---|
| Appearance | Theme: system / dark / light |
| Typography | UI, mono and body fonts; base size; line height |
| Layout | Density (compact, cozy, roomy); message style; timestamp format and visibility; avatars on/off |
| Window | Background opacity; blur (compositor-dependent) |
| Notifications | Off / name only / name and message |
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
- Models are plain data (`Conversation`, `Message`, `Contact`, ...), shared
  with the UI as snapshots or diffs.

### Storage

- One SQLCipher database holding presage's SQLite store
  (`presage-store-sqlite`) and Copiale's own tables
  (`docs/spike-sqlcipher-results.md`).
- Copiale's migrations are tracked in `_copiale_migrations`, apart from
  presage's `_sqlx_migrations`.
- Key: random 256-bit key stored in the OS keychain (`keyring`), passed to
  SQLCipher as a raw key.
- No keychain available: prompt for a passphrase at startup and derive the
  raw key with Argon2id; the salt is stored next to the database.
- Tests use temporary databases; never real Signal servers.

### Platform

- Config file watching: `notify`.
- Single-instance IPC: Unix socket / named pipe (`interprocess`).
- Notifications: `notify-rust` (Linux/macOS), WinRT toasts (Windows).
- Logging: `tracing` with redacted identifiers only; no message content,
  phone numbers or keys.

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
