#!/usr/bin/env bash
# Releases Copiale (#84): changelog, version bump, macOS CI, tag, signed .dmg
# and a draft Codeberg release. Each step is skipped once done, so a failed
# run resumes by running the same command again. See docs/release-macos.md.
#
#   release <version> [--approved]   --approved: release notes already approved
#   release publish <version>

CODEBERG_REPO=korba/copiale
CODEBERG_API=https://codeberg.org/api/v1/repos/$CODEBERG_REPO
GITHUB_REPO=rzuasti/copiale
TOKEN_ENTRY=codeberg-release-token
DRAFT_MARKER='<!-- Draft from git log: summarise, remove this line, run release again. -->'

say() { printf '\033[1m==> %s\033[0m\n' "$*" >&2; }
die() {
  printf 'release: %s\n' "$*" >&2
  exit 1
}
have_tty() { (: </dev/tty) 2>/dev/null; }

usage() {
  die "usage: release <X.Y.Z> [--approved] | release publish <X.Y.Z>"
}

check_version() {
  [[ $1 =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || die "version must be X.Y.Z, not '$1'"
}

# True if version $1 is newer than $2.
newer() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)" = "$1" ]
}

latest_tag() { git tag --list 'v*' --sort=-version:refname | head -n 1; }

workspace_version() {
  sed -n '/^\[workspace.package\]/,/^\[/ s/^version = "\(.*\)"$/\1/p' Cargo.toml
}

# Fails unless the workflows' RUST_VERSION is $1 (the flake's rustc).
check_rust_version() {
  local workflow pinned
  for workflow in .github/workflows/*.yml; do
    pinned=$(sed -n 's/^ *RUST_VERSION: "\(.*\)"$/\1/p' "$workflow")
    [ -z "$pinned" ] || [ "$pinned" = "$1" ] || die "$workflow: RUST_VERSION $pinned, the flake's rustc is $1"
  done
}

has_section() {
  [ -f CHANGELOG.md ] && grep -qE "^## ${1//./\\.}( |$)" CHANGELOG.md
}

# The section for version $1 in CHANGELOG.md, without its heading.
changelog_section() {
  [ -f CHANGELOG.md ] || return 0
  awk -v version="$1" '
    /^## / { if (inside) exit; inside = ($2 == version); next }
    inside' CHANGELOG.md | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}'
}

# Adds a draft section for version $1 listing the commits since tag $2 (all
# commits without one), above the previous sections.
draft_changelog() {
  local range=HEAD section
  [ -z "$2" ] || range="$2..HEAD"
  section=$(printf '## %s\n\n%s\n\n%s' "$1" "$DRAFT_MARKER" "$(git log --no-merges --format='- %s' "$range")")
  [ -f CHANGELOG.md ] || printf '# Changelog\n' >CHANGELOG.md
  SECTION=$section awk '
    !done && /^## / { print ENVIRON["SECTION"]; print ""; done = 1 }
    { print }
    END { if (!done) { print ""; print ENVIRON["SECTION"] } }' CHANGELOG.md >CHANGELOG.md.new
  mv CHANGELOG.md.new CHANGELOG.md
}

approve_notes() {
  local answer
  say "Release notes for $1:"
  changelog_section "$1" >&2
  [ "$2" != yes ] || return 0
  have_tty || die "no terminal to approve the release notes; pass --approved once they are"
  read -r -p "Use these release notes? [y/N] " answer </dev/tty
  [[ $answer == [yY]* ]] || die "release notes not approved"
}

# Sets the workspace version to $1 and dates its changelog heading.
bump() {
  sed -i "/^\[workspace.package\]/,/^\[/ s/^version = \".*\"$/version = \"$1\"/" Cargo.toml
  cargo update --workspace --quiet
  sed -i "s/^## ${1//./\\.}$/## $1 - $(date -u +%F)/" CHANGELOG.md
}

# The commit `Release v$1`: the tag's, else HEAD if it is that commit.
release_commit() {
  git rev-parse --quiet --verify "refs/tags/v$1^{commit}" && return
  [ "$(git log -1 --format=%s HEAD)" != "Release v$1" ] || git rev-parse HEAD
}

# YubiKey SSH keys often fail the first time; fetches retry likewise.
push() { git push "$@" || git push "$@"; }

preflight() {
  [ "$(git branch --show-current)" = main ] || die "not on main"
  [ -z "$(git status --porcelain -- . ':!CHANGELOG.md')" ] || die "uncommitted changes"
  [ -z "${FLAKE_RUSTC:-}" ] || check_rust_version "$FLAKE_RUSTC"
  command -v gh >/dev/null || die "gh not found"
  gh auth status --hostname github.com >/dev/null 2>&1 || die "gh isn't logged in: gh auth login"
  command -v rbw >/dev/null || die "rbw not found (it holds the Codeberg token)"
  [ "$(curl --fail --silent "$CODEBERG_API" | jq .has_releases)" = true ] ||
    die "releases are off in the Codeberg repo's settings"
  say "Fetching from Codeberg"
  git fetch --quiet --tags origin main || git fetch --quiet --tags origin main
  git merge-base --is-ancestor origin/main HEAD || die "main is behind Codeberg; pull first"
}

# Changelog, version bump and the release commit.
prepare() {
  local latest current
  latest=$(latest_tag)
  [ -z "$latest" ] || newer "$1" "${latest#v}" || die "$1 isn't newer than $latest"
  current=$(workspace_version)
  [ "$current" = "$1" ] || newer "$1" "$current" || die "$1 is older than the workspace's $current"
  if ! has_section "$1"; then
    draft_changelog "$1" "$latest"
    changelog_section "$1" >&2
    die "drafted the $1 section of CHANGELOG.md from the git log; summarise it, then run release again"
  fi
  if changelog_section "$1" | grep -qF "$DRAFT_MARKER"; then
    die "the $1 section of CHANGELOG.md is still a draft; summarise it and remove the marker line"
  fi
  approve_notes "$1" "$2"
  bump "$1"
  git add CHANGELOG.md Cargo.toml Cargo.lock
  git commit --quiet -m "Release v$1"
  say "Committed Release v$1"
}

push_main() {
  if ! git merge-base --is-ancestor "$1" origin/main; then
    say "Pushing main to Codeberg"
    push origin "$1:refs/heads/main"
  fi
  # Whether GitHub's main contains the commit.
  case "$(gh api "repos/$GITHUB_REPO/compare/$1...main" --jq .status 2>/dev/null)" in
    identical | ahead) ;;
    *)
      say "Pushing main to GitHub"
      push github "$1:refs/heads/main"
      ;;
  esac
}

# Follows the newest push run of workflow $1 matching the `gh run list`
# filters after it; prints its ID, fails unless it passes.
follow_run() {
  local workflow=$1 run=""
  shift
  for _ in $(seq 1 30); do
    run=$(gh run list -R "$GITHUB_REPO" --workflow "$workflow" --event push "$@" \
      --limit 1 --json databaseId --jq '.[0].databaseId // empty')
    [ -z "$run" ] || break
    sleep 10
  done
  [ -n "$run" ] || die "no $workflow run started"
  say "Following $workflow: https://github.com/$GITHUB_REPO/actions/runs/$run"
  # Watching also stops on network errors; only the run's result counts.
  until gh run watch "$run" -R "$GITHUB_REPO" --exit-status --compact --interval 60 >&2; do
    case "$(gh run view "$run" -R "$GITHUB_REPO" --json status,conclusion --jq '"\(.status) \(.conclusion)"' 2>/dev/null)" in
      "completed success") break ;;
      completed*) die "$workflow run $run failed; fix it or rerun it (gh run rerun $run --failed -R $GITHUB_REPO), then run release again" ;;
    esac
    sleep 30
  done
  echo "$run"
}

tag() {
  local tag=v$1
  if ! git rev-parse --quiet --verify "refs/tags/$tag" >/dev/null; then
    git tag -a "$tag" -m "Copiale $1" "$2"
    say "Tagged $tag"
  fi
  if ! curl --fail --silent --output /dev/null "$CODEBERG_API/tags/$tag"; then
    say "Pushing $tag to Codeberg"
    push origin "refs/tags/$tag"
  fi
  if ! gh api "repos/$GITHUB_REPO/git/ref/tags/$tag" >/dev/null 2>&1; then
    say "Pushing $tag to GitHub"
    push github "refs/tags/$tag"
  fi
}

# Downloads the signed .dmg of run $3 into target/dist/$1 and checks it was
# built and signed from commit $2 by the release workflow.
fetch_dmg() {
  local dir=target/dist/$1 dmg=Copiale-$1.dmg
  if [ ! -f "$dir/$dmg.sha256" ]; then
    rm -rf "$dir"
    gh run download "$3" -R "$GITHUB_REPO" --name Copiale.dmg --dir "$dir"
  fi
  (cd "$dir" && sha256sum --check --quiet "$dmg.sha256") || die "$dir/$dmg: checksum mismatch"
  gh attestation verify "$dir/$dmg" -R "$GITHUB_REPO" \
    --signer-workflow "$GITHUB_REPO/.github/workflows/macos-release.yml" \
    --source-digest "$2" --source-ref "refs/tags/v$1" --deny-self-hosted-runners >/dev/null ||
    die "$dir/$dmg: attestation doesn't match v$1"
  say "Verified $dir/$dmg"
}

load_token() {
  [ -n "${CODEBERG_TOKEN:-}" ] || CODEBERG_TOKEN=$(rbw get "$TOKEN_ENTRY") ||
    die "no Codeberg token: rbw get $TOKEN_ENTRY"
}

# Codeberg API: method, path, extra curl arguments. The token stays out of argv.
api() {
  local method=$1 path=$2
  shift 2
  curl --fail-with-body --silent --show-error -X "$method" \
    -H @<(printf 'Authorization: token %s\n' "$CODEBERG_TOKEN") "$CODEBERG_API$path" "$@"
}

release_id() {
  api GET "/releases?limit=50" | jq -r --arg tag "v$1" 'first(.[] | select(.tag_name == $tag) | .id) // empty'
}

# Creates the draft release for version $1, uploading the .dmg and checksum.
draft_release() {
  local id body name existing
  id=$(release_id "$1")
  if [ -z "$id" ]; then
    body=$(printf '%s\n\n%s\n' "$(changelog_section "$1")" \
      "Verify the download: \`shasum -a 256 -c Copiale-$1.dmg.sha256\`; its provenance (built and signed by GitHub Actions from this tag): \`gh attestation verify Copiale-$1.dmg -R $GITHUB_REPO\`.")
    id=$(jq -n --arg tag "v$1" --arg name "Copiale $1" --arg body "$body" \
      '{tag_name: $tag, name: $name, body: $body, draft: true}' |
      api POST /releases -H 'Content-Type: application/json' --data @- | jq -r .id)
    say "Created the draft Codeberg release"
  fi
  existing=$(api GET "/releases/$id/assets" | jq -r '.[].name')
  for name in "Copiale-$1.dmg" "Copiale-$1.dmg.sha256"; do
    if ! grep -qxF "$name" <<<"$existing"; then
      say "Uploading $name"
      api POST "/releases/$id/assets?name=$name" -F "attachment=@target/dist/$1/$name" >/dev/null
    fi
  done
}

publish() {
  local id release
  load_token
  id=$(release_id "$1")
  [ -n "$id" ] || die "no Codeberg release for v$1"
  release=$(api GET "/releases/$id")
  if [ "$(jq .draft <<<"$release")" != true ]; then
    say "v$1 is already published"
    return
  fi
  [ "$(jq '.assets | length' <<<"$release")" -ge 2 ] || die "the v$1 draft is missing its .dmg or checksum; run release $1 again"
  api PATCH "/releases/$id" -H 'Content-Type: application/json' --data '{"draft": false}' >/dev/null
  say "Published https://codeberg.org/$CODEBERG_REPO/releases/tag/v$1"
}

release() {
  local commit run answer
  preflight
  commit=$(release_commit "$1")
  if [ -z "$commit" ]; then
    prepare "$1" "$2"
    commit=$(git rev-parse HEAD)
  fi
  push_main "$commit"
  follow_run macos.yml --commit "$commit" >/dev/null
  tag "$1" "$commit"
  run=$(follow_run macos-release.yml --branch "v$1" --commit "$commit")
  fetch_dmg "$1" "$commit" "$run"
  load_token
  draft_release "$1"
  if have_tty; then
    read -r -p "Smoke-tested target/dist/$1/Copiale-$1.dmg on the Mac? Publish? [y/N] " answer </dev/tty
    if [[ $answer == [yY]* ]]; then
      publish "$1"
      return
    fi
  fi
  say "Draft release ready: https://codeberg.org/$CODEBERG_REPO/releases"
  say "After the Mac smoke test: nix run .#release -- publish $1"
}

main() {
  set -euo pipefail
  shopt -s inherit_errexit
  cd "$(git rev-parse --show-toplevel)"
  case "${1:-}" in
    publish)
      [ $# -eq 2 ] || usage
      check_version "${2#v}"
      publish "${2#v}"
      ;;
    "" | -*) usage ;;
    *)
      local approved=no
      case "${2:-}" in
        "") ;;
        --approved) approved=yes ;;
        *) usage ;;
      esac
      [ $# -le 2 ] || usage
      check_version "${1#v}"
      release "${1#v}" "$approved"
      ;;
  esac
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
