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

body="$({
  awk -v heading="## [${version}]" '
    index($0, heading) == 1 { found = 1; next }
    /^## / { if (found) exit }
    found { print }
    END {
      if (!found) {
        print "CHANGELOG has no release section for " heading > "/dev/stderr"
        exit 1
      }
    }
  ' "$changelog" |
  awk '
    function spaces(count, output) {
      output = ""
      while (length(output) < count) output = output " "
      return output
    }
    function emit_sentences(text, prefix, continuation, matched, sentence) {
      prefix = ""
      continuation = ""
      if (text ~ /^[-*+][[:space:]]/) {
        prefix = substr(text, 1, 2)
        continuation = "  "
        text = substr(text, 3)
      } else if (match(text, /^[0-9]+[.)][[:space:]]/)) {
        prefix = substr(text, 1, RLENGTH)
        continuation = spaces(RLENGTH)
        text = substr(text, RLENGTH + 1)
      }
      while (match(text, /[.!?][[:space:]]+[[:upper:][:digit:]`]/)) {
        sentence = substr(text, 1, RSTART)
        print prefix sentence
        prefix = continuation
        matched = substr(text, RSTART, RLENGTH)
        text = substr(matched, length(matched), 1) substr(text, RSTART + RLENGTH)
      }
      if (text != "") print prefix text
    }
    function start_output() {
      if (printed && pending_blank) print ""
      pending_blank = 0
    }
    function emit_block() {
      if (block == "") return
      start_output()
      emit_sentences(block)
      printed = 1
      block = ""
    }
    {
      sub(/\r$/, "")
      if (in_fence) {
        print
        if ($0 ~ /^```[[:space:]]*$/ || $0 ~ /^~~~[[:space:]]*$/) in_fence = 0
        next
      }
      if ($0 ~ /^```/ || $0 ~ /^~~~/) {
        emit_block()
        start_output()
        print
        printed = 1
        in_fence = 1
        next
      }
      if ($0 ~ /^[[:space:]]*$/) {
        emit_block()
        pending_blank = 1
        next
      }
      if ($0 ~ /^#{1,6}[[:space:]]/) {
        emit_block()
        start_output()
        print
        printed = 1
        next
      }
      if ($0 ~ /^[-*+][[:space:]]/ || $0 ~ /^[0-9]+[.)][[:space:]]/) {
        emit_block()
        block = $0
        next
      }
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
      if (block == "") block = line
      else block = block " " line
    }
    END {
      if (in_fence) {
        print "release notes contain an unterminated fenced code block" > "/dev/stderr"
        exit 1
      }
      emit_block()
    }
  '
})"

if [[ -z "$body" ]]; then
  echo "CHANGELOG.md has no release notes for ${version}" >&2
  exit 1
fi

printf '%s\n\n' "$body"
cat <<'EOF'
Prebuilt archives are attached for Windows x64, Linux x64, Intel macOS, and Apple Silicon macOS.
Each contains the version-aligned `papertiger` and `papertiger-mise` binaries, setup and operating documentation, licenses, and a release manifest.
Verify the adjacent SHA-256 asset before extraction.
EOF
