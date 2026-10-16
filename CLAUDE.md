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
  - No telemetry.
  - Never log message content, phone numbers or keys; log redacted
    identifiers only (`ServiceId`, `PhoneNumber`, `GroupId`). Dependency
    logs are capped at `warn`, and all output is scrubbed of UUIDs, long
    hex runs, byte lists and phone numbers (`copiale_core::logging`).
  - Close every database connection before exit (`Database::close`):
    SQLCipher frees its global state at exit, crashing connections still
    closing (core dumps could hold keys).
- **MVP scope:** text-only 1:1 and group chats; see `docs/mvp.md`.
- **v1 scope:**
  - In: 1:1 and group chats, attachments, reactions, replies.
  - Out: calls, voice notes, stickers; accessibility after v1.
- **Packaging:**
  - Linux:
    - Nix flake: package plus NixOS module, as a first-class install path.
      nixpkgs later.
    - Flatpak, AUR.
  - macOS: notarized `.dmg`.
  - Windows: MSIX or winget.

## Code style

- **Rust**: format with `rustfmt`. Avoid import aliases (`use ... as ...`)
  unless necessary.

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
indicators (#14), incoming edits, deletes and quoted replies (#15), and
sending them (#16) done. Next: disappearing message timers (#17).
