# Vendored dependencies

Temporary copies of dependencies carrying patches not yet upstream. Remove
each one once its patches are merged and the pin is updated.

## libsignal-service-rs

- **Base:** [whisperfish/libsignal-service-rs](https://github.com/whisperfish/libsignal-service-rs)
  at `fae0679db1659bb2decb650fd07437c5dfd72f37`, the revision presage pins.
  Copied without `.github/`, `examples/`, `AGENTS.md`, `.gitignore` and
  `rust-toolchain`.
- **Patches**, applied in this order:
  - `patches/libsignal-service-contacts-bounds.patch`. Parsing a contacts
    sync whose avatar length runs past the end of the data panicked
    (`copy_to_bytes`), killing the receive connection mid-batch: messages
    already decrypted but not yet acknowledged came back and failed to
    decrypt again. The patch ends the contact list with
    `ParseContactError::Truncated` instead, keeps the contacts parsed so
    far, and logs sizes only (no contact data) to help find the cause. It
    also strips the attachment padding to the declared size; each padding
    byte otherwise parsed as an empty contact.
    Upstream PR: [whisperfish/libsignal-service-rs#483](https://github.com/whisperfish/libsignal-service-rs/pull/483).
  - `patches/libsignal-service-retry-requests.patch`. Decryption failures
    carry what a retry request needs (`ServiceError::DecryptionFailed`);
    `MessageSender::send_retry_request` sends one as unencrypted content,
    and `MessageSender::resend_content` answers one.
    Upstream PR: [whisperfish/libsignal-service-rs#484](https://github.com/whisperfish/libsignal-service-rs/pull/484).
  - `patches/libsignal-service-sent-devices.patch`. `SentMessage::devices`
    lists the devices a send reached, to answer retry requests only from
    those.
    Upstream PR: [whisperfish/libsignal-service-rs#485](https://github.com/whisperfish/libsignal-service-rs/pull/485).
  - `patches/libsignal-service-contacts-decode-errors.patch`. A contact
    that failed to decode ended the contact list silently, so a corrupt
    list looked complete; it ends with `ParseContactError::Protobuf` now.
    Upstream PR: [whisperfish/libsignal-service-rs#486](https://github.com/whisperfish/libsignal-service-rs/pull/486).
  - `patches/libsignal-service-deferred-acks.patch`. Envelopes were
    acknowledged as soon as read off the websocket, so quitting or crashing
    before storing one lost it; `MessagePipe::stream_unacknowledged` yields
    each with an `Acknowledgement` to send once handled. Not upstream yet.
- **Used via:** `[patch."https://github.com/whisperfish/libsignal-service-rs"]`
  in the root `Cargo.toml`.

To re-vendor when presage pins a new revision without the fixes: take that
revision, apply the patches with `git apply`, and copy it here without the
files listed above.

To drop it once upstream has the fixes and presage pins a revision with
them: remove the patch entry and this directory, then add the
libsignal-service git hash back to `nix/package.nix`.

## presage

- **Base:** the `presage` crate of [whisperfish/presage](https://github.com/whisperfish/presage)
  at `6f630791b2c7bf3d5b00c75f5a6c94131bf79c8c`, the revision `Cargo.toml`
  pins: its `Cargo.toml`, `src/` and the repository's `LICENSE.md`.
  `presage-store-sqlite` still comes from git, and builds against this
  copy.
- **Patches**, applied in this order:
  - `patches/presage-retry-requests.patch`, on top of libsignal-service's.
    `Received::DecryptionError` carries the failure, for
    `Manager::send_retry_request`; retry requests arrive as
    `Received::RetryRequest`, archiving the session they show broken; sends
    return what went out and the devices it reached (`SentContent`), to
    resend with
    `Manager::resend_content`; `Manager::send_null_message` starts a new
    session. Not upstream yet.
  - `patches/presage-refresh-group.patch`. `Manager::refresh_group`
    fetches and stores a group's state whatever revision is stored, to
    check a sender missing from it before dropping their message.
    Upstream PR: [whisperfish/presage#468](https://github.com/whisperfish/presage/pull/468).
  - `patches/presage-storage-service.patch`. `Manager::storage_service`
    authenticates against Storage Service with the account's storage key,
    to read which contacts and groups the user accepted or blocked, and
    which contacts they verified. Not upstream yet.
  - `patches/presage-linked-sync-requests.patch`. Only the primary device
    answers sync requests; a linked one answered a blocked list request
    with an empty list, which could clear the list on the device asking.
    Not upstream yet.
  - `patches/presage-contacts-sync-result.patch`, on top of
    libsignal-service's. `Received::Contacts` carries the sync's
    `complete` flag and the contacts saved, or `None` if the list couldn't
    be read, parsed or saved in full, to drop contacts the phone no longer
    has.
    Upstream PR: [whisperfish/presage#470](https://github.com/whisperfish/presage/pull/470).
  - `patches/presage-deferred-acks.patch`, on top of libsignal-service's.
    `Manager::receive_messages_unacknowledged` yields each message with its
    `Acknowledgement`, to send once the client stored it; messages handled
    in presage are acknowledged there. `DuplicatedMessage` (decrypted
    before, not acknowledged) is skipped quietly. Not upstream yet.
- **Used via:** `[patch."https://github.com/whisperfish/presage"]` in the
  root `Cargo.toml`.

To re-vendor at a new revision: take that revision's `presage/` crate,
apply the patches with `git apply`, copy it here with `LICENSE.md`, and
update the pins of `presage` and `presage-store-sqlite` in `Cargo.toml`.

To drop it once upstream has the changes: remove the patch entry and this
directory, then key the presage git hash in `nix/package.nix` by
`presage-0.8.0-dev` again rather than `presage-store-sqlite-0.8.0-dev`.
