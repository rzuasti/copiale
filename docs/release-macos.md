# Releasing

One command, run from a clean `main` on a Linux workstation:

```sh
nix run .#release -- 0.2.0
```

GitHub Actions on the mirror (github.com/rzuasti/copiale) builds, signs and
notarizes the macOS `.dmg`; nothing is signed locally. Each step is skipped
once done, so after any failure, fix the cause and run the same command again.

## Steps

1. **Preflight**: on `main`, no uncommitted changes (other than
   `CHANGELOG.md`), not behind Codeberg; `RUST_VERSION` in the workflows
   matches the flake's `rustc`; `gh` logged in; releases enabled on Codeberg.
2. **Changelog**: without a section for the version, one is drafted in
   `CHANGELOG.md` from the commits since the last tag, and the command stops.
   Rewrite it as a short summary for users, remove the marker line, run again.
   The section is shown for approval (`--approved` skips the prompt when it
   was approved elsewhere, e.g. with Claude); it becomes the release notes.
3. **Release commit**: workspace version and `Cargo.lock` bumped, heading
   dated, committed as `Release vX.Y.Z`.
4. **Push** `main` to Codeberg and GitHub, then follow the `macos.yml` run
   for the commit (~25 min). A failed run stops here: fix it, or rerun it
   (`gh run rerun <id> --failed -R rzuasti/copiale`), then run again.
5. **Tag** `vX.Y.Z` (annotated) and push it to both remotes. GitHub's
   `macos-release.yml` signs, notarizes and staples the app and `.dmg` built
   for that commit; the command follows it. Notarization can take an hour or
   more per submission.
6. **Verify**: the `.dmg` and checksum are downloaded to `target/dist/X.Y.Z/`;
   the checksum and the attestation (`macos-release.yml`, the tag, the
   commit, GitHub-hosted runner) are checked.
7. **Draft release** on Codeberg with the changelog section, `.dmg` and
   checksum.
8. **Publish**: smoke test the `.dmg` on the Mac (#85), then answer the
   prompt. Without a terminal, or to publish later:
   `nix run .#release -- publish 0.2.0`.

Pushes ask for a YubiKey touch; a failed first try is retried once.

## One-time setup

- **Signing** (done, #82): Developer ID Application certificate and App
  Store Connect API key, as secrets in the mirror's `release` environment
  (deployable from `v*` tags only). No copy is kept locally.
- **GitHub**: `gh auth login` on each release workstation; the YubiKey SSH
  key on the GitHub account; the `github` remote:
  `git remote add github git@github.com:rzuasti/copiale.git`.
- **Codeberg token**: Settings → Applications → new token with
  `write:repository`, stored in Bitwarden as `codeberg-release-token`; the
  command reads it with `rbw get`.
- **Codeberg**: releases enabled in the repo's settings (Units).

## Renewal

The Developer ID certificate expires 2027-11-23: #86. Apps already signed
and notarized stay valid after it expires.
