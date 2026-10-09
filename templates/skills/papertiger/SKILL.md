---
name: papertiger
description: Use to continue work in progress, pick the next or ready task, or record work in the project's planner; also before multi-part changes and to preserve decisions and deferred defects across sessions. A session todo list is not the project planner. Skip small edits nobody asked to track and work owned by another tracker.
---

# Papertiger

Use a durable record when it helps the work survive a handoff. One independently
reviewable outcome gets one task; investigation and verification are steps inside
it. Resume existing records even for a small next step. Record validated deferred
work as it appears, without interrupting authorized progress. Do not mirror an
existing team or domain lifecycle.

## Select the authority once

Run `papertiger` from PATH, or else `bin/papertiger[.exe]` inside this skill's
directory; both are the same verified release. Run it in the native shell from
the project you are working in, not from the skill directory: it uses the
nearest `state/papertiger.sqlite` above the working directory, in a linked Git
worktree the main working tree's, and in a repository routed to another
project that project's; `papertiger route show` names the authority and why.
Keep work in the canonical project that owns the outcome, including
cross-repository work; from elsewhere pass `--project-root <canonical-root>`
(a runner may set `PAPERTIGER_PROJECT_ROOT` for one process instead). When
your operator says a repository's planning belongs to another project, route
it once from inside it with `papertiger route set <project-root>`. Do not
guess `--db` or create a second store in a worktree. When no project owns the work, use the
authority your operator named and put the consuming project's absolute root in
each task intent. Write through `papertiger`, never raw SQLite. A refusal does
not authorize removing guards or manufacturing admission; use the corrective
command or preserve the blocker. A genuinely new project with no planning
history gets an authority with `papertiger init` at its Git root, then
`plan add <slug> "Title"`; see [first use](references/agent_integration.md#first-use).

Every write records the model that authored it: pass `--model <your exact
model id>` on each write (shell variables you export may not survive to the
next command; `PAPERTIGER_MODEL` counts only when every command sees it), and
never use `human` or `runtime`. Harness, session and actor label come from the environment;
outside a harness set `PAPERTIGER_SESSION` to one unique session ID.

## Enter, work, finish

Orient to an unfamiliar authority with `status` once. For a known task, read
`show <N>` and resume it; for a goal or parent, `brief <N>` shows its whole
subtree. Otherwise select work with `focus`, and before adding, look for it
with `search "<terms>"`; both cover every plan unless `--plan <slug>` narrows
them. Text output is written for you to read and is the cheapest; add
`--json` only when a script parses the output or you need a field the text
omits.
Another session's pickup is advisory: not a lock, and no reason to ask
permission for requested work. `start` a task before working on it: until
then it looks free to every other session.

```text
papertiger start <N>
papertiger add "Outcome" --kind <kind> --plan <slug> --start --intent "Standalone purpose"
papertiger done <N> "Outcome and verification"
papertiger decompose <N> --outline-file outline.json --start-ready
```

Choose each task's kind: goal, feature, change, bug, probe, decision or
release. Probes, decisions and bugs require results: a probe's states what it
found, a decision's the choice and its reasoning, a bug's the cause and fix.
Split a multi-part outcome into children with one `decompose` call instead of
inventing section or phase labels; the outline is JSON like
`{"schema": "papertiger.task_outline.v2", "why": "...", "children": [{"key":
"fix", "kind": "bug", "title": "...", "intent": "..."}, {"key": "docs", "kind":
"change", "title": "...", "deps": ["fix"]}]}` (`papertiger schema` has every
field). Refer to local work by its handle (`F-924`) inside Papertiger
and by its outcome everywhere else; keep both out of shared Git/PR prose.

Review is optional. A claim marked `[disputed · <model>]` was found wrong;
when you do check a claim, record
`review <N> --verdict confirmed|disputed --why "..."` (add `--result` for a
done task's result). Read rejected history before reviving an approach. An
intent stands alone: fold a source plan's substance in (`--intent-file`)
instead of citing a scratch file. Write `--why` for choices a cold reader
could question, and record decisions with `note <N> "..."`. `focus` ranks by
structure, not notes: record that work waits with `edit <N> --priority later`,
a dependency or a gate, and keep a finished parent open with an open gate;
`--intent-file`, `--why-file`, `--result-file` and `note --text-file` read
UTF-8 text when shell quoting becomes awkward.

Each receipt names the task's handle (`F-12 added to app`). Bound large reads
with `--limit` rather than head/tail, and follow a printed continuation. If
output handling fails after a successful mutation, check with `show`/`log`;
never replay it.

Before completion, record any commit that represents the outcome with
`commit add <N> <full-oid>` (add `--repo <label>` only for a nested or external
repository). `done` refuses open obligations; resolve them with evidence,
never route around a refusal. Run `audit` after planning changes.

Use live `--help` for operations. Read [the reference](references/agent_integration.md)
for authority changes, migration or recovery; its relevant section
covers gates, transfers, external references and optional Mise. Only `init`
initializes or migrates a database; never use it to replace missing history.
