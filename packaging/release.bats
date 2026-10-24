# Tests for release.sh's checks, changelog handling and step detection.

setup() {
  # shellcheck source=packaging/release.sh
  source "$BATS_TEST_DIRNAME/release.sh"
  cd "$BATS_TEST_TMPDIR"
  git init --quiet --initial-branch=main
  git config user.name Test
  git config user.email test@example.com
  printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.0"\nedition = "2024"\n' >Cargo.toml
  touch Cargo.lock
  git add Cargo.toml Cargo.lock
  commit "First"
}

commit() { git commit --quiet --allow-empty -m "$1"; }

@test "versions are X.Y.Z" {
  check_version 0.1.0
  check_version 10.20.30
  for bad in 1.2 v1.2.3 1.2.3-rc.1 01.2.3 "1.2.3 "; do
    run check_version "$bad"
    [ "$status" -eq 1 ]
  done
}

@test "newer compares versions numerically" {
  newer 0.10.0 0.9.0
  newer 1.0.0 0.99.99
  ! newer 0.9.0 0.10.0
  ! newer 1.0.0 1.0.0
}

@test "the workspace version comes from [workspace.package]" {
  [ "$(workspace_version)" = 0.1.0 ]
}

@test "the workflows' Rust version must match the flake's" {
  mkdir -p .github/workflows
  printf 'env:\n  RUST_VERSION: "1.98.1"\n' >.github/workflows/macos.yml
  printf 'name: other\n' >.github/workflows/other.yml
  check_rust_version 1.98.1
  run check_rust_version 1.99.0
  [ "$status" -eq 1 ]
  [[ $output == *"RUST_VERSION 1.98.1, the flake's rustc is 1.99.0"* ]]
}

@test "a first draft lists every commit in a new CHANGELOG.md" {
  commit 'Add a \n backslash'
  draft_changelog 0.1.0 ""
  [ "$(head -n 1 CHANGELOG.md)" = "# Changelog" ]
  has_section 0.1.0
  changelog_section 0.1.0 | grep -qxF "$DRAFT_MARKER"
  changelog_section 0.1.0 | grep -qxF -- '- Add a \n backslash'
  changelog_section 0.1.0 | grep -qxF -- '- First'
}

@test "a draft lists the commits since the last tag, above older sections" {
  printf '# Changelog\n\n## 0.1.0 - 2026-01-01\n\n- Old\n' >CHANGELOG.md
  git tag v0.1.0
  commit "New"
  draft_changelog 0.2.0 v0.1.0
  [ "$(grep '^## ' CHANGELOG.md)" = "$(printf '## 0.2.0\n## 0.1.0 - 2026-01-01')" ]
  [ "$(changelog_section 0.2.0)" = "$(printf '%s\n\n- New' "$DRAFT_MARKER")" ]
  [ "$(changelog_section 0.1.0)" = "- Old" ]
}

@test "sections match whole versions" {
  printf '# Changelog\n\n## 0.1.10\n\n- Ten\n' >CHANGELOG.md
  has_section 0.1.10
  ! has_section 0.1.1
  ! has_section 0x1x10
  [ -z "$(changelog_section 0.1.1)" ]
}

@test "notes need approval without --approved" {
  printf '# Changelog\n\n## 0.2.0\n\n- New\n' >CHANGELOG.md
  have_tty() { return 1; }
  approve_notes 0.2.0 yes
  run approve_notes 0.2.0 no
  [ "$status" -eq 1 ]
  [[ $output == *"pass --approved"* ]]
}

@test "bump sets the version and dates the heading" {
  cargo() { :; }
  printf '# Changelog\n\n## 0.2.0\n\n- New\n\n## 0.1.0 - 2026-01-01\n' >CHANGELOG.md
  bump 0.2.0
  [ "$(workspace_version)" = 0.2.0 ]
  grep -qx "## 0.2.0 - $(date -u +%F)" CHANGELOG.md
  grep -qx "## 0.1.0 - 2026-01-01" CHANGELOG.md
}

@test "no release commit before it's made" {
  [ -z "$(release_commit 0.2.0)" ]
}

@test "the release commit is HEAD once made" {
  commit "Release v0.2.0"
  [ "$(release_commit 0.2.0)" = "$(git rev-parse HEAD)" ]
  [ -z "$(release_commit 0.3.0)" ]
}

@test "the release commit is the tag's once tagged" {
  commit "Release v0.2.0"
  git tag -a v0.2.0 -m "Copiale 0.2.0"
  tagged=$(git rev-parse HEAD)
  commit "Later"
  [ "$(release_commit 0.2.0)" = "$tagged" ]
}

@test "prepare drafts, then refuses the draft, then commits" {
  cargo() { :; }
  have_tty() { return 1; }
  commit "Feature"
  run prepare 0.2.0 yes
  [ "$status" -eq 1 ]
  [[ $output == *"drafted the 0.2.0 section"* ]]
  run prepare 0.2.0 yes
  [ "$status" -eq 1 ]
  [[ $output == *"still a draft"* ]]
  sed -i "/^<!--/d" CHANGELOG.md
  prepare 0.2.0 yes
  [ "$(git log -1 --format=%s)" = "Release v0.2.0" ]
  [ "$(workspace_version)" = 0.2.0 ]
  [ -z "$(git status --porcelain)" ]
}

@test "prepare refuses versions not newer than the last tag" {
  git tag v0.2.0
  run prepare 0.2.0 yes
  [ "$status" -eq 1 ]
  [[ $output == *"isn't newer than v0.2.0"* ]]
  run prepare 0.1.5 yes
  [ "$status" -eq 1 ]
}

@test "prepare refuses versions older than the workspace's" {
  sed -i 's/^version = .*/version = "0.3.0"/' Cargo.toml
  git commit --quiet -am "Bump"
  run prepare 0.2.0 yes
  [ "$status" -eq 1 ]
  [[ $output == *"older than the workspace's 0.3.0"* ]]
}
