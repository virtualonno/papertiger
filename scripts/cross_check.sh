#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Cargo owns source and installation behavior tests. The release workflow owns
# extracted-artifact smoke tests; do not rebuild or guess a binary path here.
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace

# These production scripts are not exercised by Cargo. Fixed inputs keep their
# tests independent of the workspace version and whether it is a prerelease.
fixture="$(mktemp -d "${TMPDIR:-/tmp}/papertiger-release-check.XXXXXX")"
cleanup() {
  case "$fixture" in
    "${TMPDIR:-/tmp}"/papertiger-release-check.*) rm -rf -- "$fixture" ;;
    *) echo "refusing to remove unexpected fixture path: $fixture" >&2 ;;
  esac
}
trap cleanup EXIT
printf '[workspace.package]\nversion = "1.2.3"\n' > "$fixture/Cargo.toml"
printf '# Changelog\n\n## [1.2.3] - 2026-08-13\n' > "$fixture/CHANGELOG.md"
validate_dispatch() {
  (cd "$fixture" && bash "$root/scripts/validate_release_dispatch.sh" "$@")
}
expect_dispatch_refusal() {
  local expected="$1" error
  shift
  if error="$(validate_dispatch "$@" 2>&1)"; then
    echo "release dispatch unexpectedly accepted: $*" >&2
    exit 1
  fi
  if [[ "$error" != *"$expected"* ]]; then
    echo "release dispatch refusal did not name '$expected': $error" >&2
    exit 1
  fi
}
validate_dispatch 1.2.3 false refs/heads/onno/local-verification
validate_dispatch 1.2.3 true refs/heads/master
expect_dispatch_refusal 'artifact_version=1.2.3' 999.0.0 false refs/heads/master
expect_dispatch_refusal 'dispatch from master or set create_release=false' \
  1.2.3 true refs/heads/onno/local-verification
expect_dispatch_refusal 'create-release must be true or false' \
  1.2.3 invalid refs/heads/master
printf '[workspace.package]\nversion = "1.2.3-dev.1"\n' > "$fixture/Cargo.toml"
expect_dispatch_refusal 'CHANGELOG.md has no dated 1.2.3-dev.1 section' \
  1.2.3-dev.1 false refs/heads/master

cat > "$fixture/body.md" <<'EOF'
This sentence was wrapped in
the middle. This sentence was not.

### Fixed

- One release-note item was wrapped
  at an arbitrary source column. Its second sentence is semantic.
- Another item stayed on one line.

> A quotation
> keeps its structure.

| Item | Value |
| --- | --- |
| Example | 3 |

```text
## This is a code sample
## [9.9.9] - 2026-01-01
  indentation stays
```

Notes continue after the sample.
EOF
{
  printf '# Changelog\n\n## [1.2.3] - 2026-08-13\n\n'
  cat "$fixture/body.md"
  printf '\n## [1.2.2] - 2026-08-12\nOlder release marker\n'
} > "$fixture/CHANGELOG.md"
bash scripts/render_release_notes.sh 1.2.3 \
  "$fixture/CHANGELOG.md" > "$fixture/actual.md"
# The body is an authored fixture, never captured from the renderer output.
head -c "$(wc -c < "$fixture/body.md")" "$fixture/actual.md" > "$fixture/prefix.md"
cmp "$fixture/body.md" "$fixture/prefix.md"
if grep -Fq 'Older release marker' "$fixture/actual.md"; then
  echo "release-note rendering leaked another release section" >&2
  exit 1
fi
if ! grep -Fq 'Verify the adjacent SHA-256 asset before extraction.' "$fixture/actual.md"; then
  echo "release notes omitted the archive checksum instruction" >&2
  exit 1
fi
if notes_error="$(bash scripts/render_release_notes.sh \
    9.9.9 "$fixture/CHANGELOG.md" 2>&1)"; then
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

# Blank sections must not produce a release containing only installation text.
printf '## [1.2.3] - 2026-08-13\n\n  \n' > "$fixture/empty.md"
if bash scripts/render_release_notes.sh 1.2.3 "$fixture/empty.md" > /dev/null 2>&1; then
  echo "release-note rendering accepted an empty section" >&2
  exit 1
fi

echo "Papertiger source and release-script checks passed"
