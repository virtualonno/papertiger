# Papertiger project reference

The skill carries the ordinary workflow. Read this reference for first use,
authority selection, migration, recovery and transfer, and for the details of
kinds, review, pickup, gates, commits, references and Mise.

## First use

The release archive is laid out for a project root: unpacked there, its skills
and `tools/papertiger/bin/papertiger[.exe]` work as they are. Installation,
upgrade and removal are covered by `--help` and the README.

Start from the authority the project already has. Only a genuinely new project
with no planning history gets one: add the database and its `-journal`, `-wal`
and `-shm` sidecars to Git's ignore rules, run `init`, then
`plan add <slug> "Title" --intent "Purpose"`. When history is unexpectedly
missing, stop and restore it; never run `init` to replace it.

Use Papertiger when work has independently reviewable outcomes, separate
commits, dependencies, external conditions, decisions, probes or proof
obligations that merit durable identity and cold resume, even when all of them
are requested in one session. Record validated deferred defects, decisions and
proof debt as they appear; intermediate steps inside one independently
reviewable outcome do not need tasks.

## Authority

Choose one canonical authority by the outcome that owns the work, not by the
repository holding the next edited file. One outcome spanning repositories
stays in that authority, with separately committed parts as separate tasks
whose commits carry stable `--repo` labels. Use a second authority only for a
genuinely independent outcome; authorities never share dependencies or
replacements and are never merged.

Run the project's own launcher, `<project-root>/tools/papertiger/bin/papertiger[.exe]`,
not a copy on `PATH`. It finds the project receipt or release manifest above
the working directory and opens `state/papertiger.sqlite` there. From another
repository pass the global `--project-root <canonical-project-root>` to that
root's launcher, since projects can pin different releases during a rollout.
`--db` and `PAPERTIGER_DB` are deliberate overrides for recovery, never for
ordinary selection.

- Mutate only through Papertiger commands. A raw SQLite writer fails with
  `papertiger_write_requires_public_api`. Never remove guards or add triggers;
  after stopping any direct access, `repair-guards --dry-run` previews and
  `repair-guards --why <reason>` restores them.
- `init` is the only command that creates or migrates an authority. Read
  commands never migrate; follow their exact corrective command deliberately.
  The current schema is v14 and the dump format `papertiger.dump.v11`; before
  migrating, take `backup --output <new-path>` with the new release.
- `export --output <path>` writes a recovery file; `import` reads v11 and
  converts `papertiger.dump.v10`. An export is transfer and recovery, never a
  second live authority.
- Legacy history recovery is explicit: `backup`, review `history inspect
  <event-id>`, then `history quarantine <event-id> --expect-sha256 <digest>
  --why <reason>` for its reported defects only.

## Start from live truth

```bash
papertiger status
papertiger focus --json
papertiger search "<terms>" --compact
papertiger show <task.seq> --json
papertiger brief <task.seq>
papertiger list --all-plans --status unfinished --json
papertiger audit
```

Pass `--plan <slug>` when more than one plan is active. Bounded reads report
what they omitted; follow their `continuation_command` instead of treating the
visible entries as complete. Reads never change the authority.

`brief <task.seq>` reads a task and its whole subtree: what it is for,
progress, the next ready leaves, open work with pickup and owner state, open
gates, results of done descendants and the latest notes. Use it instead of
handoff documents; anything worth keeping belongs in a result, gate or note.

## Tasks, kinds and handles

Choose each task's kind when creating it: `goal` for a container of child
outcomes, `feature`, `change` (refactor, migration, hardening, removal), `bug`,
`probe`, `decision`, or `release` (publishing, rollout and other chores). The
legacy `work` kind cannot be assigned; retype an open `work` task with
`edit <task.seq> --kind <kind> --why <reason>` (`audit` suggests one). Probe,
decision and bug tasks require `--result` or `--result-file`; a bug's result
names its cause.

Output shows each task by its handle, kind letter and number (`F-924`; JSON
`handle`). Commands accept the handle, `N` or `#N`; prefer the handle or bare
`N`, since `#` starts a shell comment. A handle whose letter no longer matches
the task's kind is refused with the current one. `show`, `focus` and `brief`
place a task under its ancestors (`path: G-1 › F-2`). Keep task numbers and
handles out of shared commits, pull requests and release notes; record shared
artifacts inward with `reference add`.

Priority is a level, `--priority now|next|later` (default `next`); `focus`
ranks it after readiness. Express order between tasks with `--dep`, not finer
priorities. `list`, `focus`, `search` and `tree` filter with `--kind`.

Split a multi-part outcome into children with one
`decompose <parent> --outline-file <path|->` call (`papertiger schema` defines
`papertiger.task_outline.v1`) instead of inventing section or phase labels. An
intent stands alone: fold a source document's substance in with
`--intent-file` rather than citing a scratch file.

## Authorship and review

Every write records the model that authored it; without one it is refused.
Pass `--model <your-model-id>` with your own exact model unless
`PAPERTIGER_MODEL` was set where the session was launched; events record
which (`model_source`). Never use `human` (the operator's manual model) or
`runtime` (reserved for the tool). Harness, session and the actor label come
from the environment. Authorship is provenance, never ownership.

A task carries two claims: its filing (title and intent) and, once done, its
result. Each shows its review state with the model that made it, e.g.
`[unreviewed · deepseek-v4.1-flash]`. An unreviewed or disputed claim is
unverified: check it before acting on it or building on it, then record what
you found:

```bash
papertiger review <task.seq> --verdict confirmed --why "What you verified"
papertiger review <task.seq> --result --verdict disputed --why "What does not hold"
```

A session cannot review its own claim. A disputed result holds the tasks that
depend on it until confirmed or revised. `list --unreviewed` and
`focus --unreviewed` select claims needing a check; `models` counts how each
model's filings and results fared.

## Pickup

`start <task.seq>` records pickup and resumes in-progress work directly; an
earlier pickup never needs release, permission or a heartbeat. Outside a
detected harness, set `PAPERTIGER_SESSION` once to a unique identity per agent
session. `focus` prefers your own work, then unattributed in-progress work,
then ready work, then work picked up elsewhere; when choosing freely, prefer
other available work.

Where the harness names its process, a pickup records its owner, and
`owner_state` reports `ended` only when that is provable on the owner's host,
otherwise `running` or `unknown`. Age proves nothing. An ended owner is a
reason to resume the work with `start`, never an automatic transition.

## Mutations and receipts

Write `--why` for anything a future session could question, in language that
stands alone. Long text uses `--intent-file`, `--why-file`, `--result-file` or
`note --text-file`, with `-` for stdin; text must be UTF-8.

For scripted chains pass `--json` and read `papertiger.mutation.v1`: `changed`
and the exact emitted `events`, with new task numbers in `events[].task.seq`.
Check the exit code before parsing. If handling fails after a successful
write, verify with `show` or `log`; never replay the mutation.

```bash
papertiger add "Outcome" --kind feature --start \
  --intent "Standalone purpose" \
  --why "Why this outcome starts now"
papertiger gate add <task.seq> <name> --kind test --requirement "Tests pass"
papertiger gate resolve <task.seq> <name> \
  --evidence file:path/to/receipt.json --sha256 <digest>
papertiger done <task.seq> --result "Outcome and verification"
```

`done` refuses open dependencies, gates or children; resolve or waive them
with evidence and reasons rather than routing around the refusal. Check
`list --status rejected` before reviving an old approach; `reopen` revisits a
rejected task. For a duplicate, `retire <duplicate> --into <canonical> --why
<reason>`; for an approach that should not be pursued, `reject`. Rehome a
connected set with `move-plan <N>... --plan <destination> --why <reason>`.

## Gates and evidence

A gate is a proof obligation: `gate add <task.seq> <name> --kind
<test|benchmark|review|capture|fixture|build|doc|external|other> --requirement
<text>`. An `external` gate names a condition outside the task, such as a
vendor fix, and keeps the task out of ready work while open. Clear gates with
`gate resolve` (evidence) or `gate waive` (reason).

`gate resolve` refuses a new `file:` locator unless it is relative to the
project root, uses `/` separators and names an existing file whose bytes match
any `--sha256` given. Evidence under ignored or temporary paths rots: keep
durable text in the authority with `note --text-file <path> --task <task.seq>`
and resolve with a `note:<summary>` locator. `evidence verify --json`
re-checks stored bindings read-only and names corrective commands.

## Commits and references

When a local commit represents a task outcome, record it inward
before task completion:

```bash
git rev-parse --verify 'HEAD^{commit}'
papertiger commit add <task.seq> <full-oid>
papertiger commit find <full-oid>
```

Pass `--repo <label>` only for a nested or external repository. A commit is
lookup evidence, never completion authority; Papertiger does not run Git or
read commit messages. Record shared issues, pull requests and other artifacts
with `reference add <task.seq> <locator> --kind
<pull_request|issue|review|adr|input|other>`; references never fetch data or
satisfy proof.

## Mise is an episodic external driver

`papertiger-mise` ships in every release but is not installed by
`setup-project`. When a bounded improvement campaign is warranted, invoke the
release's binary against the consumer; read `MISE.md` from the same release
before admission. The consumer owns its Mise database, object store and
campaign workspaces, and the binary must not change during a campaign. Mise
nominations are evidence, never completion, promotion or deployment
authority; attach a derived projection with `papertiger mise record`, which
cannot resolve a task or gate.
