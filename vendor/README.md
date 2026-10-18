# Vendored dependencies

Temporary copies of dependencies carrying patches not yet upstream. Remove
each one once its patch is merged and the pin is updated.

## presage-store-sqlite

- **Base:** [whisperfish/presage](https://github.com/whisperfish/presage) at
  `94b3978f5456469cfd4d9f72351a5eff93715f81`, the same revision `presage`
  is pinned to in the root `Cargo.toml`.
- **Patch:** `patches/presage-store-sqlite-close.patch` adds
  `SqliteStore::close()`, so Copiale can close presage's connection pool
  before exit. Without it, SQLCipher's exit handler races connections still
  closing and can crash (see `docs/spike-sqlcipher-results.md`).
- **Other change:** `Cargo.toml` depends on upstream `presage` by git
  revision instead of `../presage`.
- **Upstream PR:** [whisperfish/presage#457](https://github.com/whisperfish/presage/pull/457).
- **Used via:** `[patch."https://github.com/whisperfish/presage"]` in the
  root `Cargo.toml`.

To drop it once the PR is merged:

1. Update the `presage` and `presage-store-sqlite` pins in the root
   `Cargo.toml` to a revision that includes the change.
2. Remove the `[patch."https://github.com/whisperfish/presage"]` entry and
   this directory.
3. Update the git hashes in `nix/package.nix`.

To re-vendor at a new revision: take `presage-store-sqlite/` from that
revision, apply the patch with `git apply`, and repeat the `Cargo.toml`
change above.

## libsignal-service-rs

- **Base:** [whisperfish/libsignal-service-rs](https://github.com/whisperfish/libsignal-service-rs)
  at `fb92b13c82fde40ba4d10696cf79aa4f36ce25f1`, the revision presage pins.
  Copied without `.github/` and `examples/`.
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

To drop it once upstream has the fix and presage pins a revision with it:
remove the patch entry and this directory, then add the libsignal-service
git hash back to `nix/package.nix`.
