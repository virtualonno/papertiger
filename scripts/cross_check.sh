#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p papertiger-mise --test deterministic_dogfood -- --test-threads=1
cargo build --locked --release --workspace --bins

exe=""
case "${OS:-}:$(uname -s 2>/dev/null || true)" in
  Windows_NT:* | *:MINGW* | *:MSYS* | *:CYGWIN*) exe=".exe" ;;
esac
planner="$root/target/release/papertiger$exe"
mise="$root/target/release/papertiger-mise$exe"
planner_version="$($planner --version)"
mise_version="$($mise --version)"
planner_semver="${planner_version#papertiger }"
test "$planner_semver" = "${mise_version#papertiger-mise }"

bash scripts/validate_release_dispatch.sh \
  "$planner_semver" false refs/heads/codex/local-verification
if release_error="$(bash scripts/validate_release_dispatch.sh \
    999.0.0 false refs/heads/master 2>&1)"; then
  echo "release dispatch validation accepted a mismatched artifact version" >&2
  exit 1
fi
case "$release_error" in
  *"dispatch the workflow with artifact_version=${planner_semver}"*) ;;
  *)
    echo "release dispatch mismatch did not name the corrective artifact_version" >&2
    exit 1
    ;;
esac
if release_error="$(bash scripts/validate_release_dispatch.sh \
    "$planner_semver" true refs/heads/codex/local-verification 2>&1)"; then
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
Each contains the version-aligned `papertiger` and `papertiger-mise` binaries, setup and operating documentation, licenses, and a release manifest.
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

"$planner" setup-project "$project" --dry-run --json > "$fixture/setup-dry-run.json"
grep -q '"schema": "papertiger.project_setup.v2"' "$fixture/setup-dry-run.json"
test ! -e "$project/scripts/papertiger"

"$planner" setup-project "$project" --json > "$fixture/setup.json"
test -f "$project/tools/papertiger/bin/papertiger$exe"
test ! -e "$project/tools/papertiger/bin/papertiger-mise$exe"
test -f "$project/tools/papertiger/project-install.json"
grep -Fq '"schema": "papertiger.project_install.v1"' \
    "$project/tools/papertiger/project-install.json"
grep -Fq "\"papertiger_version\": \"$planner_semver\"" \
    "$project/tools/papertiger/project-install.json"
grep -Fq '"authority_path": "state/papertiger.sqlite"' \
    "$project/tools/papertiger/project-install.json"
test -f "$project/scripts/papertiger.cmd"
cmp "$project/tools/papertiger/agent_integration.md" \
    "$root/agent_integration.md"
test -f "$project/.agents/skills/papertiger/SKILL.md"
test -f "$project/.claude/skills/papertiger/SKILL.md"
cmp "$project/.agents/skills/papertiger/SKILL.md" \
    "$project/.claude/skills/papertiger/SKILL.md"
cmp "$project/.agents/skills/papertiger/SKILL.md" \
    "$root/templates/papertiger/SKILL.md"
test "$(cat "$project/AGENTS.md")" = "repository contract"
test "$(cd "$project" && ./scripts/papertiger --version)" = "$planner_version"

(cd "$project/nested/work" && ../../scripts/papertiger init)
(cd "$project/nested/work" && ../../scripts/papertiger status)
(cd "$project/nested/work" && ../../scripts/papertiger audit)
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

echo "Papertiger cross-check passed: $planner_version / $mise_version"
