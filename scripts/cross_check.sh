#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --workspace --bins

exe=""
case "${OS:-}:$(uname -s 2>/dev/null || true)" in
  Windows_NT:* | *:MINGW* | *:MSYS* | *:CYGWIN*) exe=".exe" ;;
esac
# Run the binaries cargo just built, wherever CARGO_TARGET_DIR put them.
target_dir="${CARGO_TARGET_DIR:-$root/target}"
planner="$target_dir/debug/papertiger$exe"
mise="$target_dir/debug/papertiger-mise$exe"
planner_version="$($planner --version)"
mise_version="$($mise --version)"
planner_semver="${planner_version#papertiger }"
test "$planner_semver" = "${mise_version#papertiger-mise }"

release_workflow="$root/.github/workflows/release-artifacts.yml"
grep -Fq '"schema": "papertiger.project_uninstall.v3"' "$release_workflow"

# Only a released version has a dated CHANGELOG section, so dispatch must
# refuse a pre-release workspace; the dispatch checks then run against a
# release fixture instead of the workspace.
dispatch_root="$root"
dispatch_version="$planner_semver"
dispatch_fixture=""
dispatch_cleanup() {
  case "$dispatch_fixture" in
    "") ;;
    "${TMPDIR:-/tmp}"/papertiger-release-dispatch.*) rm -rf -- "$dispatch_fixture" ;;
    *) echo "refusing to remove unexpected release-dispatch fixture: $dispatch_fixture" >&2 ;;
  esac
}
if [[ "$planner_semver" == *-* ]]; then
  echo "notice: ${planner_semver} is a pre-release without a dated CHANGELOG section; checking that release dispatch refuses it, then checking dispatch validation against a release fixture"
  if release_error="$(bash scripts/validate_release_dispatch.sh \
      "$planner_semver" false refs/heads/codex/local-verification 2>&1)"; then
    echo "release dispatch validation accepted pre-release ${planner_semver} without a dated CHANGELOG section" >&2
    exit 1
  fi
  case "$release_error" in
    *"CHANGELOG.md has no dated ${planner_semver} section"*) ;;
    *)
      echo "pre-release dispatch refusal did not name the missing dated section: ${release_error}" >&2
      exit 1
      ;;
  esac
  dispatch_fixture="$(mktemp -d "${TMPDIR:-/tmp}/papertiger-release-dispatch.XXXXXX")"
  trap dispatch_cleanup EXIT
  dispatch_root="$dispatch_fixture"
  dispatch_version="1.2.3"
  printf '[workspace.package]\nversion = "%s"\n' "$dispatch_version" \
    > "$dispatch_root/Cargo.toml"
  printf '# Changelog\n\n## [%s] - 2026-08-13\n' "$dispatch_version" \
    > "$dispatch_root/CHANGELOG.md"
fi
validate_dispatch() {
  (cd "$dispatch_root" && bash "$root/scripts/validate_release_dispatch.sh" "$@")
}

validate_dispatch "$dispatch_version" false refs/heads/codex/local-verification
if release_error="$(validate_dispatch 999.0.0 false refs/heads/master 2>&1)"; then
  echo "release dispatch validation accepted a mismatched artifact version" >&2
  exit 1
fi
case "$release_error" in
  *"dispatch the workflow with artifact_version=${dispatch_version}"*) ;;
  *)
    echo "release dispatch mismatch did not name the corrective artifact_version" >&2
    exit 1
    ;;
esac
if release_error="$(validate_dispatch \
    "$dispatch_version" true refs/heads/codex/local-verification 2>&1)"; then
  echo "release dispatch validation accepted publication from a non-master ref" >&2
  exit 1
fi
case "$release_error" in
  *"dispatch from master or set create_release=false"*) ;;
  *)
    echo "release ref refusal did not name both corrective choices" >&2
    exit 1
    ;;
esac
dispatch_cleanup
trap - EXIT

notes_fixture="$(mktemp -d "${TMPDIR:-/tmp}/papertiger-release-notes.XXXXXX")"
notes_cleanup() {
  case "$notes_fixture" in
    "${TMPDIR:-/tmp}"/papertiger-release-notes.*) rm -rf -- "$notes_fixture" ;;
    *) echo "refusing to remove unexpected release-notes fixture: $notes_fixture" >&2 ;;
  esac
}
trap notes_cleanup EXIT
cat > "$notes_fixture/CHANGELOG.md" <<'EOF'
# Changelog

## [1.2.3] - 2026-08-13

This sentence was wrapped in
the middle. This sentence was not.

### Fixed

- One release-note item was wrapped
  at an arbitrary source column. Its second sentence is semantic.
- Another item stayed on one line.

## [1.2.2] - 2026-08-12
EOF
cat > "$notes_fixture/expected.md" <<'EOF'
This sentence was wrapped in the middle.
This sentence was not.

### Fixed

- One release-note item was wrapped at an arbitrary source column.
  Its second sentence is semantic.
- Another item stayed on one line.

Prebuilt archives are attached for Windows x64, Linux x64, Intel macOS, and Apple Silicon macOS.
Merge `.agents`, `.claude`, and `tools` into the project root. Skills are ready to discover; executables, documentation, licenses and the source manifest live under `tools/papertiger`.
Verify the adjacent SHA-256 asset before extraction.
EOF
bash scripts/render_release_notes.sh 1.2.3 \
  "$notes_fixture/CHANGELOG.md" > "$notes_fixture/actual.md"
diff -u "$notes_fixture/expected.md" "$notes_fixture/actual.md"
if notes_error="$(bash scripts/render_release_notes.sh \
    9.9.9 "$notes_fixture/CHANGELOG.md" 2>&1)"; then
  echo "release-note rendering accepted a missing changelog section" >&2
  exit 1
fi
case "$notes_error" in
  *"CHANGELOG has no release section for ## [9.9.9]"*) ;;
  *)
    echo "release-note refusal did not identify the missing version section" >&2
    exit 1
    ;;
esac
notes_cleanup
trap - EXIT

fixture="$(mktemp -d "${TMPDIR:-/tmp}/papertiger-cross-check.XXXXXX")"
cleanup() {
  case "$fixture" in
    "${TMPDIR:-/tmp}"/papertiger-cross-check.*) rm -rf -- "$fixture" ;;
    *) echo "refusing to remove unexpected fixture path: $fixture" >&2 ;;
  esac
}
trap cleanup EXIT

project="$fixture/consumer"
mkdir -p "$project/nested/work"
printf 'repository contract\n' > "$project/AGENTS.md"
printf 'target/\n' > "$project/.gitignore"
mkdir -p "$project/.agents/skills/unrelated"
printf 'unrelated skill\n' > "$project/.agents/skills/unrelated/SKILL.md"

"$planner" setup-project "$project" --dry-run --json > "$fixture/setup-dry-run.json"
grep -q '"schema": "papertiger.project_install_result.v7"' "$fixture/setup-dry-run.json"
grep -q '"agents"' "$fixture/setup-dry-run.json"

"$planner" setup-project "$project" --json > "$fixture/setup.json"
test -f "$project/tools/papertiger/bin/papertiger$exe"
test ! -e "$project/tools/papertiger/bin/papertiger-mise$exe"
test -f "$project/tools/papertiger/project-install.json"
grep -Fq '"schema": "papertiger.project_install.v3"' \
    "$project/tools/papertiger/project-install.json"
grep -Fq "\"papertiger_version\": \"$planner_semver\"" \
    "$project/tools/papertiger/project-install.json"
grep -Fq '"authority_path": "state/papertiger.sqlite"' \
    "$project/tools/papertiger/project-install.json"
cmp "$project/tools/papertiger/agent_integration.md" \
    "$root/templates/agent_integration.md"
test -f "$project/.agents/skills/papertiger/SKILL.md"
test ! -e "$project/.claude/skills/papertiger/SKILL.md"
cmp "$project/.agents/skills/papertiger/SKILL.md" \
    "$root/templates/skills/papertiger/SKILL.md"
test "$(cat "$project/AGENTS.md")" = "repository contract"
installed_planner="$project/tools/papertiger/bin/papertiger$exe"
test "$(cd "$project" && "$installed_planner" --version)" = "$planner_version"

(cd "$project/nested/work" && "$installed_planner" init)
(cd "$project/nested/work" && PAPERTIGER_ACTOR=cross-check \
  "$installed_planner" plan add orientation "Orientation smoke")
(cd "$project/nested/work" && PAPERTIGER_ACTOR=cross-check \
  "$installed_planner" add "Read-only orientation" --plan orientation)
authority_fingerprint="$(cksum "$project/state/papertiger.sqlite")"
(cd "$project/nested/work" && "$installed_planner" status --json) \
  > "$fixture/status.json"
grep -q '"schema": "papertiger.status.v3"' "$fixture/status.json"
(cd "$project/nested/work" && "$installed_planner" \
  focus --plan orientation --json) > "$fixture/focus.json"
(cd "$project/nested/work" && "$installed_planner" show 1 --json) \
  > "$fixture/show.json"
(cd "$project/nested/work" && "$installed_planner" \
  search "read-only orientation" --json) > "$fixture/search.json"
(cd "$project/nested/work" && "$installed_planner" audit)
test "$(cksum "$project/state/papertiger.sqlite")" = "$authority_fingerprint"
if [[ "$exe" = ".exe" ]]; then
  planner_windows="$(cygpath -w "$installed_planner")"
  nested_windows="$(cygpath -w "$project/nested/work")"
  powershell.exe -NoProfile -Command \
    "Set-Location -LiteralPath '$nested_windows'; & '$planner_windows' status --json" \
      > "$fixture/windows-status.json"
  grep -q '"schema": "papertiger.status.v3"' "$fixture/windows-status.json"
  test "$(cksum "$project/state/papertiger.sqlite")" = "$authority_fingerprint"
fi
test -f "$project/state/papertiger.sqlite"
test ! -e "$project/nested/work/state"

"$mise" --project-root "$project" status --json > "$fixture/mise-before.json"
grep -q '"initialized": false' "$fixture/mise-before.json"
"$mise" --project-root "$project" init
"$mise" --project-root "$project" status --json > "$fixture/mise-after.json"
grep -q '"initialized": true' "$fixture/mise-after.json"
test -f "$project/state/papertiger-mise.sqlite"
test ! -e "$project/tools/papertiger/bin/papertiger-mise$exe"

"$planner" setup-project "$project" --json > "$fixture/setup-second.json"
if grep -Eq '"action": "(create|replace|update_gitignore)"' "$fixture/setup-second.json"; then
  echo "second setup-project run was not idempotent" >&2
  exit 1
fi

# Binaries are checked at install time only: setup replaces a host binary that
# differs from the release binary and removes an earlier release's runtime
# receipt; ordinary commands never read the installed binary's bytes.
retired_receipt="$installed_planner.runtime-install.json"
printf '{"schema": "papertiger.runtime_install.v1"}\n' > "$retired_receipt"
printf 'modified' >> "$installed_planner"
"$planner" setup-project "$project" --dry-run --json > "$fixture/setup-repair-preview.json"
grep -Fq "\"path\": \"tools/papertiger/bin/papertiger$exe\"" \
  "$fixture/setup-repair-preview.json"
grep -Fq '"action": "replace"' "$fixture/setup-repair-preview.json"
grep -Fq '"action": "remove"' "$fixture/setup-repair-preview.json"
test -f "$retired_receipt"
"$planner" setup-project "$project" --json > "$fixture/setup-repair.json"
test ! -e "$retired_receipt"
cmp "$installed_planner" "$planner"
(cd "$project/nested/work" && "$installed_planner" status --json) \
  > "$fixture/repaired-status.json"
grep -q '"schema": "papertiger.status.v3"' "$fixture/repaired-status.json"

ignore_fingerprint="$(cksum "$project/.gitignore")"
mise_fingerprint="$(cksum "$project/state/papertiger-mise.sqlite")"
# Ownership is by path: uninstall removes an edited host binary without
# comparing content.
printf 'modified' >> "$installed_planner"
"$planner" uninstall-project "$project" --dry-run --json \
  > "$fixture/uninstall-dry-run.json"
grep -q '"schema": "papertiger.project_uninstall.v3"' \
  "$fixture/uninstall-dry-run.json"
grep -q '"operation": "remove"' "$fixture/uninstall-dry-run.json"
test -f "$project/tools/papertiger/project-install.json"
test -f "$installed_planner"

"$planner" uninstall-project "$project" --json > "$fixture/uninstall.json"
test ! -e "$project/tools/papertiger/project-install.json"
test ! -e "$installed_planner"
test ! -e "$project/tools/papertiger/agent_integration.md"
test ! -e "$project/.agents/skills/papertiger/SKILL.md"
test -f "$project/.agents/skills/unrelated/SKILL.md"
test "$(cat "$project/AGENTS.md")" = "repository contract"
test "$(cksum "$project/.gitignore")" = "$ignore_fingerprint"
test "$(cksum "$project/state/papertiger.sqlite")" = "$authority_fingerprint"
test "$(cksum "$project/state/papertiger-mise.sqlite")" = "$mise_fingerprint"

unmarked="$fixture/unmarked"
mkdir -p "$unmarked"
"$planner" setup-project "$unmarked" --json > "$fixture/unmarked-setup.json"
grep -Fq '"skill_targets": []' "$fixture/unmarked-setup.json"
test ! -e "$unmarked/.agents/skills/papertiger/SKILL.md"
test ! -e "$unmarked/.claude/skills/papertiger/SKILL.md"
"$planner" uninstall-project "$unmarked" --json \
  > "$fixture/unmarked-uninstall.json"
test ! -e "$unmarked/tools/papertiger/project-install.json"

echo "Papertiger cross-check passed: $planner_version / $mise_version"
