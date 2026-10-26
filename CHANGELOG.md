# Changelog

## 0.2.0 - 2026-10-26

Attachments and new groups.

- **Attachments**: pictures, videos and files show in the chat. Pictures
  up to 10 MiB download on their own (not in message requests); others on
  request. A viewer for pictures and videos, with playback and sound
  (`←`/`→`, `Space`, `m`); save one or all of a message's attachments.
- **Sending attachments**: attach with `Ctrl/⌘+O` or the paperclip, drag
  and drop, or paste. Photos are sent upright without their location and
  other metadata, unless sent as originals.
- **New groups**: create a group from the new chat picker (its first
  entry), `Ctrl/⌘+Shift+N` or the palette: pick members from your
  contacts, then name it.
- **Ended groups**: a group an admin ended says so, and nothing more can
  be sent there.
- Group titles stand out in the chat header.
- Videos are decoded in a sandboxed helper process.

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
