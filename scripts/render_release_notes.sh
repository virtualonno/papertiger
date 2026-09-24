#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -lt 1 || "$#" -gt 2 ]]; then
  echo "usage: render_release_notes.sh <version> [changelog]" >&2
  exit 2
fi

version="$1"
changelog="${2:-CHANGELOG.md}"

if [[ ! -f "$changelog" ]]; then
  echo "release notes changelog does not exist: ${changelog}" >&2
  exit 1
fi

# Preserve the authored Markdown; release rendering must not reinterpret lists,
# tables, code blocks or sentence boundaries. Changelog sections use ## [version].
body="$(awk -v heading="## [${version}]" '
  {
    sub(/\r$/, "")
    # A heading inside a fenced sample belongs to the notes, not the changelog.
    if (match($0, /^ {0,3}(`{3,}|~{3,})/)) {
      marker = substr($0, RSTART, RLENGTH)
      sub(/^ */, "", marker)
      if (fence == "") {
        fence = substr(marker, 1, 1)
        fence_length = length(marker)
      } else if (substr(marker, 1, 1) == fence && length(marker) >= fence_length &&
                 substr($0, RSTART + RLENGTH) ~ /^[[:space:]]*$/) {
        fence = ""
      }
      if (found) { print; printed = 1 }
      next
    }
    if (fence != "") { if (found) print; next }
  }
  $0 == heading || index($0, heading " - ") == 1 { found = 1; next }
  /^## \[/ { if (found) exit }
  found {
    if (!printed && $0 ~ /^[[:space:]]*$/) next
    print
    printed = 1
  }
  END {
    if (!found) {
      print "CHANGELOG has no release section for " heading > "/dev/stderr"
      exit 1
    }
    if (fence != "") {
      print "release notes contain an unterminated fenced code block; close it in the changelog" > "/dev/stderr"
      exit 1
    }
  }
' "$changelog")"

if [[ ! "$body" =~ [^[:space:]] ]]; then
  echo "CHANGELOG.md has no release notes for ${version}" >&2
  exit 1
fi

printf '%s\n\n' "$body"
cat <<'EOF'
Prebuilt archives are attached for Windows x64, Linux x64, Intel macOS, and Apple Silicon macOS.
Merge `.agents`, `.claude`, and `tools` into the project root. Skills are ready to discover; executables, documentation, licenses and the source manifest live under `tools/papertiger`.
Verify the adjacent SHA-256 asset before extraction.
EOF
