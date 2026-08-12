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
