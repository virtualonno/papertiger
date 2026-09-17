---
name: papertiger
description: Use before implementing multiple independent changes, resuming recorded work, or preserving decisions and deferred defects. Keep outcomes resumable across sessions; skip small self-contained edits and work owned by another tracker.
---

# Papertiger

Use a durable record when it helps the work survive a handoff. One independently
reviewable outcome gets one task; investigation and verification are steps inside
it. Resume existing records even for a small next step. Record validated deferred
work as it appears, without interrupting authorized progress. Do not mirror an
existing team or domain lifecycle.

## Select the authority once

<!-- installed-command -->

An established project's canonical authority wins, including when working from
another checkout. Use its native `tools/papertiger/bin/papertiger[.exe]` and, when
outside that project, `--project-root <canonical-root>`. Resolve the executable
from the selected root, not a nested cwd. Do not guess from `target/` or create a
second database in a worktree. Otherwise use the personal executable and store
specified above. If neither installation exists, report the missing setup-user
installation; do not rewrite project guidance or initialize replacement history.
In the personal store, select only records belonging to the consuming project
or cross-project outcome; a global focus list does not change ownership.
Below, `papertiger` means this selected executable plus its authority arguments.

Set `PAPERTIGER_ACTOR` to a concise author label and `PAPERTIGER_SESSION` to one
unique session ID before mutations; reuse them. Global `--actor` and `--session`
work when shells do not retain environment. Set `PAPERTIGER_MODEL` and
`PAPERTIGER_REASONING_EFFORT` only when their actual values are known.

## Enter, work, finish

Start with `status`, then `focus --plan <slug> --json` and
`search "<outcome terms>" --plan <slug> --compact --json`. Read the selected
`show <N> --json`; use `--no-history` for a current-state recheck. Follow reported
continuations when evidence is bounded. Keep focus's readiness, pickup and real
blockers together: another session's pickup is advisory, not a lock or a reason
to wait for permission to do requested work.

```text
papertiger start <N> --why "Why this work resumes" --json
papertiger add "Outcome" --plan <slug> --start --intent "Standalone purpose" --intent-source user --why "Why now" --json
papertiger done <N> --result "Outcome and verification" --result-source agent --json
```

Search before adding; read rejected history before reviving an approach. Choose
`user`, `agent`, or `external` for the actual source of intent. Write `--why` for
choices a cold reader could question. `--intent-file`, `--why-file`, and
`--result-file` accept UTF-8 text when shell quoting becomes awkward.

Do not pipe JSON receipts through `head` or `tail`; use native compact or
projection options when needed. JSON mutations acknowledge exact committed events. New selectors are in
`events[].task.seq`; retain the receipt, don't replay a successful mutation if
local display/parsing fails. Read-only `show` or `log` resolves uncertainty.

Before completion, associate any representing commit with
`commit add <N> <full-oid> --repo <stable-label>`. Omit it when no commit represents
the outcome. Keep task numbers out of shared Git/PR prose. Probes and decisions
require results; `done` refuses open obligations. Resolve them with evidence,
never route around a refusal. Run `audit` after planning changes.

Use live `--help` for operations. Read [the reference](../../../tools/papertiger/agent_integration.md)
for installation, authority changes, migration or recovery; its relevant section
covers gates, transfers, external references and optional Mise. Only `init`
initializes or migrates a database; never use it to replace missing history.
