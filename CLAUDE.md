# Copiale

Native desktop chat client for the Signal messaging network, targeting Linux,
macOS and Windows.

Named after the Copiale cipher, an 18th-century secret-society manuscript that
went unsolved until 2011.

## Goals

- **Easy to use**: familiar chat UX, minimal setup.
- **Slick and minimalist**: clean, uncluttered interface.
- **Performant**: native app, fast startup, low memory use.
- **Secure**: privacy-first defaults; never weaken Signal's end-to-end
  encryption guarantees.

## Decisions

- **Language:** Rust.
- **Signal layer:** presage (on libsignal); links as a secondary device.
- **License:** AGPL-3.0 (required by libsignal/presage).
- **UI:**
  - Custom-drawn; native feel, not native widgets.
  - Minimalist, keyboard-first "terminal on steroids"; one adaptive window
    for full-screen and quick widget-style use. Details in `docs/mvp.md`.
  - No web-based or Electron-style frameworks.
  - User-themeable: TOML themes, base16 import, follows system dark/light and
    accent.
- **UI toolkit:** GPUI with GPUI Kit (`gpui-kit`, pinned exactly); see
  `docs/spike-ui-toolkit-results.md`.
  - Known gap: wrapped RTL text renders incorrectly; accepted until RTL
    locales are targeted.
  - Emoji: bundled Noto Color Emoji as every window's font fallback on
    Linux, patched for GPUI (`crates/copiale-ui/fonts/README.md`, limits
    there); macOS and Windows use the system's (Windows has no colour
    bitmap glyphs in GPUI).
- **Workspace:**
  - `copiale-core`: Signal, models, storage, async; no UI dependencies.
  - `copiale-ui`: views and theming.
  - `copiale-testkit`: fake Signal backend and fixtures (dev-dependency).
  - `copiale`: binary.
  - `vendor/`: temporary patched dependencies, excluded from the workspace;
    see `vendor/README.md`. Don't reformat or lint them.
  - Core runs as an actor on its own thread (current-thread tokio runtime
    plus `LocalSet`; presage futures are not `Send`); UI talks to it via
    `Command`/`Event` channels.
- **Security:**
  - One SQLCipher database (presage store plus app tables), raw key in the
    OS keychain (`keyring`); Argon2id passphrase fallback without a
    keychain. See `docs/spike-sqlcipher-results.md`.
  - Messages live in app tables only; presage's message saving is disabled
    through a delegating `Store` wrapper.
  - Identity keys: changes are accepted, as in Signal, and noted in every
    shared chat; sends to a key replacing one verified on the phone are
    refused until the user confirms it (`signal::protocol`). Verified state
    comes only from the phone: its verified syncs, and Storage Service
    contact records when they change (`stored_identities`).
  - Groups: messages, edits and deletes kept only from members of the
    group as last fetched, with this account in it (one fetch first on a
    mismatch); group change lines only from server-signed changes newer
    than the stored group. Edits are text only. New groups (name and
    members, made admin) through vendored upstream PRs (`vendor/README.md`);
    members without a known profile key are invited. Ended groups (from
    the server-signed change, or the fetched state; each group fetched
    once to catch earlier ends) take nothing more: no sends, edits,
    deletes for everyone or typing.
  - Others' edits and deletes apply only within 48h of the message, by
    server time (edits from the original, not the last edit); at most 100
    edits per message (`storage::changes`), 10 of our own sent.
  - Work others can cause is bounded: bodies clipped to 64 KiB
    (`copiale_core::text`), 100 kept changes per author, 3 group fetches
    per sender a minute, each member's profile fetched once an hour, 50
    attachments downloaded on their own per sender an hour.
  - Quotes: only which message a reply quotes is stored; its text is shown
    as stored in the same chat, else "original message not found".
  - Attachments: downloaded copies in `attachments/` beside the database,
    each AES-256-GCM encrypted under its own key in the database, deleted
    with their message (`storage::attachments`; `docs/security.md`).
    Images up to 10 MiB download on their own, never in message requests;
    others when asked. At most 32 per message and 100 MiB each, enforced
    while downloading. Pictures decoded by contents (JPEG, PNG, WebP, GIF)
    with size limits; saved only where the user picks, under safe names
    (`copiale_core::text::save_name`). Sent ones are copied in encrypted
    at once, still images re-encoded upright without their metadata
    unless sent as originals (`copiale_core::outgoing`), and uploaded two
    at a time before the message joins the outbox.
  - Videos: first frames (`copiale --video-frame`) and playback in the
    viewer (`copiale --video-play`, frames and sound streamed back) come
    from a child process sandboxed before it reads the video (Linux
    seccomp, threads only when playing; macOS pure computation profile;
    no Windows yet, #94), decoding only MOV/MP4 with H.264, HEVC, VP9 (and
    AV1 on Linux), and AAC sound, through FFmpeg, in software
    (`crates/copiale/src/video`, `copiale_core::video`).
  - Message requests: a chat the other side wrote in is a request until
    accepted (here, on the phone, in Storage Service, or by replying);
    requests send no read receipts or typing and notify without name or
    text. Blocked accounts and groups are dropped. Accept/block state
    syncs both ways with the phone (`chat_states`); Storage Service is
    read only.
  - Contacts: only the phone's synced ones are listed. Contacts presage
    saves on first sight (they carry a profile key) never replace them,
    and name chats only after profile names (`storage::contacts`). A
    complete contact sync read in full drops synced contacts missing
    from it.
  - Links: `http(s)://` and `www.` URLs (any case; `www.` over `https`)
    without bidi or invisible characters; a click asks first, showing the
    host in punycode.
  - Names and titles: bidi controls and invisible characters stripped
    (joiners and variation selectors kept; `copiale_core::text`), isolated
    (FSI…PDI) inside other text; file names show them as `<U+XXXX>`.
  - No telemetry.
  - Optional capture protection (`window.capture_protection`, off):
    Windows and macOS only; on Hyprland, a documented window rule.
  - No core dumps; keys and passphrases locked out of swap
    (`copiale_core::hardening`). Key and passphrase copies
    left in memory are listed in `docs/security.md`.
  - Never log message content, phone numbers or keys; log redacted
    identifiers only (`ServiceId`, `PhoneNumber`, `GroupId`), shown as
    per-run tags (the journal may keep stderr; Windows writes a log file,
    `logging::log_to_file`). Dependency logs are capped
    at `warn` (libsignal-protocol's off), panics log their location only,
    and all output is scrubbed of UUIDs, long hex runs, byte lists and
    phone numbers (`copiale_core::logging`).
  - Close every database connection before exit (`Database::close`):
    SQLCipher frees its global state at exit, crashing connections still
    closing (core dumps could hold keys).
- **MVP scope:** text-only 1:1 and group chats; see `docs/mvp.md`.
- **v1 scope:**
  - In: 1:1 and group chats, attachments, reactions, replies.
  - Out: calls, voice notes, stickers; accessibility after v1.
- **Packaging:**
  - Linux:
    - Nix flake: package, NixOS module (settings as system-wide defaults
      in `/etc/xdg`) and Home Manager module (settings as the user's
      read-only `config.toml`), as a first-class install path. Their
      settings come from `nix/config-schema.json`, which a test keeps in
      step with the app. `autostart` starts it hidden at login: an XDG
      autostart entry (NixOS), a systemd user service (Home Manager).
      nixpkgs later.
    - Flatpak, AUR.
  - FFmpeg (videos): minimal builds only, never system-wide
    extras: `nix/ffmpeg.nix` on Linux, a static build per architecture on
    macOS (`packaging/macos/ffmpeg.sh`, pinned and checksummed), and the
    same for Windows with MSVC (`packaging/windows/ffmpeg.sh`, in CI).
  - macOS: notarized universal `.dmg` (macOS 14+), built, signed and
    notarized by GitHub Actions on the mirror (github.com/rzuasti/copiale):
    `macos.yml` builds and attests `main` on demand, `macos-release.yml`
    signs that build for a `v*` tag. Signing secrets live only in the
    mirror's `release` environment (`v*` tags only). Releases:
    `nix run .#release -- <version>` (`docs/release-macos.md`); its
    changelog section is approved by the user before it's committed.
  - Windows: signed MSIX downloaded from Codeberg Releases (no Microsoft
    Store), signed through SignPath Foundation (Certum Open Source
    certificate if declined); updates through an `.appinstaller` on
    Codeberg Pages (#100). `windows.yml` on the mirror checks and builds
    `copiale.exe` on demand (static C runtime); tried in a Windows 11
    evaluation VM only, `nix run .#windows-vm` (x86_64-linux;
    `docs/windows-vm.md`).

## Code style

- **Rust**: format with `rustfmt`. Avoid import aliases (`use ... as ...`)
  unless necessary.
  - Prefer plain arguments over a struct made only to satisfy
    `clippy::too_many_arguments`; allow the lint on that function instead.

## Testing & linting

- Write/update tests for every new or modified component:
  - `copiale-core`: unit and integration tests.
  - `copiale-ui`: view and state tests.
- Tests never hit real Signal servers; use a mocked presage store and fake
  network responses.
- `spikes/` is exempt from testing rules.
- After any change, run all tests and do not continue until they pass.
- Run the linter after adding a significant chunk of code.
- Commands (run inside `nix develop`):
  - Format: `cargo fmt --all`
  - Lint: `cargo clippy --all-targets --all-features -- -D warnings`
  - Test: `cargo test --workspace`

## Workflow

- Keep formatter changes: never revert a file just because the formatter
  reformatted it (even files you didn't otherwise touch), and add such changes
  to the current commit. Revert only if the reformatting actually introduces a
  bug.
- Track follow-ups (upstream PRs to check, vendored crates to drop, things
  to revisit later) as Codeberg issues with a due date, never as memories:
  work spans multiple workstations.

## Naming

- Never use "Signal" in the product name, branding or package names (Signal
  trademark policy). Describe it as "a client for Signal" only.
- App ID: `page.codeberg.korba.Copiale` (`copiale_core::APP_ID`): Wayland app
  ID, macOS bundle ID, keychain service, Flatpak ID. Changing it means
  migrating user data.

## Status

Pre-development. MVP planned (`docs/mvp.md`, Codeberg issues #1–#28);
M0 (foundations) and M1 (walking skeleton, #7–#11) done. M2: group text
(#12), placeholders for non-text content (#13), receipts and typing
indicators (#14), incoming edits, deletes and quoted replies (#15), sending
them (#16), disappearing message timers (#17), new 1:1 chats from contacts
(#18), and notifications (#19) done: M2 complete. M3: command palette and
keyboard navigation (#20), adaptive compact layout (#21), and single
instance with background running and `--toggle`/`--quit` (#22), and custom
titlebar and decorations (#23), the settings UI (#24), blocks and bubbles
message styles (#25), and unlink and wipe (#26) done: M3 complete. M4: Nix
package and modules (#27) done. macOS release: first release v0.1.0
(#79–#84); left: macOS checks (#85). Next: Windows checks (#28).
