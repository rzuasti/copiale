# Code signing policy

Windows packages aren't signed yet: Copiale is applying to SignPath
Foundation. Once it's accepted, this applies to them:

Free code signing provided by [SignPath.io](https://about.signpath.io),
certificate by [SignPath Foundation](https://signpath.org).

macOS builds are signed and notarized under the project's own Developer ID.

## How releases are built

Every package is built from this repository's source by GitHub Actions on
GitHub-hosted runners, on the [mirror](https://github.com/rzuasti/copiale), from
the `main` commit a release tags, with a build attestation
(`gh attestation verify <file> -R rzuasti/copiale`). Only software built from
this project's own source is signed.

## Team

- Committers and reviewers: [korba](https://codeberg.org/korba)
- Approvers: [korba](https://codeberg.org/korba)

An approver checks and approves each release by hand before it's signed. All
members use multi-factor authentication for the repository and for SignPath.

## Privacy

Copiale has no telemetry. It connects only to the Signal service, to do what
the user asks of a Signal client, and to nothing else unless the user asks
(opening a link hands it to the browser). What Signal handles is covered by
[Signal's privacy policy](https://signal.org/legal/#privacy-policy). See
[security.md](security.md).
