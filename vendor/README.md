# Vendored dependencies

Temporary copies of dependencies carrying patches not yet upstream. Remove
each one once its patch is merged and the pin is updated.

## libsignal-service-rs

- **Base:** [whisperfish/libsignal-service-rs](https://github.com/whisperfish/libsignal-service-rs)
  at `fae0679db1659bb2decb650fd07437c5dfd72f37`, the revision presage pins.
  Copied without `.github/`, `examples/`, `AGENTS.md`, `.gitignore` and
  `rust-toolchain`.
- **Patch:** `patches/libsignal-service-contacts-bounds.patch`. Parsing a
  contacts sync whose avatar length runs past the end of the data panicked
  (`copy_to_bytes`), killing the receive connection mid-batch: messages
  already decrypted but not yet acknowledged came back and failed to decrypt
  again. The patch ends the contact list with
  `ParseContactError::Truncated` instead, keeps the contacts parsed so far,
  and logs sizes only (no contact data) to help find the cause. It also
  strips the attachment padding to the declared size; each padding byte
  otherwise parsed as an empty contact.
- **Upstream PR:** [whisperfish/libsignal-service-rs#483](https://github.com/whisperfish/libsignal-service-rs/pull/483).
- **Used via:** `[patch."https://github.com/whisperfish/libsignal-service-rs"]`
  in the root `Cargo.toml`.

To re-vendor when presage pins a new revision without the fix: take that
revision, apply the patch with `git apply`, and copy it here without the
files listed above.

To drop it once upstream has the fix and presage pins a revision with it:
remove the patch entry and this directory, then add the libsignal-service
git hash back to `nix/package.nix`.
