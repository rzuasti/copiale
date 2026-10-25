# Changelog

## 0.1.0 - 2026-10-25

First release: a fast, minimalist, keyboard-first desktop client for Signal,
linked to your phone as a secondary device. Text only for now.

- **Chats**: 1:1 and group text chats; start new chats from your phone's
  contacts. Chats start empty at link time.
- **Messages**: delivery and read receipts, typing indicators, replies, edits
  and deletes (incoming and your own), disappearing message timers.
  Attachments, reactions, stickers and calls show as placeholders to open on
  your phone.
- **Message requests and blocks**, synced with your phone.
- **Emoji**: colour emoji, `:shortcodes:`, emoticon conversion and a picker
  (`Ctrl/⌘+E`).
- **Keyboard first**: command palette (`Ctrl/⌘+K`), chat search, keyboard
  navigation through chats and messages.
- **One adaptive window**: two panes when wide, a compact single-chat view
  when narrow.
- **Look**: log, blocks or bubbles message styles; follows the system's dark
  and light mode; ten built-in themes or your own; configurable fonts.
- **Background**: keeps running with the window closed, with a tray/menu bar
  icon; single instance, with `copiale --toggle` to bind to a key.
- **Notifications**: the sender only by default; message text is opt-in.
- **Privacy**: messages stored in an encrypted database, its key in the
  system keychain; no telemetry; unlink and wipe from the settings.

Downloads: a signed and notarized universal `.dmg` for macOS 14 or later.
On Linux, install with the Nix flake (package, NixOS and Home Manager
modules).
