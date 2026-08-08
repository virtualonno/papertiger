# Papertiger operating contract

Papertiger is optional. Use it for work that crosses sessions or where
dependencies, external blockers, decisions, probes, or proof obligations make
live focus and cold-resume context more useful than a short checklist.

## Authority

The default authority is `state/papertiger.sqlite`. Project-local launchers
bind that path to the canonical repository root even when called from a nested
directory. Set `PAPERTIGER_DB` or pass `--db` only when the repository has
deliberately selected another canonical path.

- Mutate one canonical SQLite file from one planning worktree. Git cannot merge
  independently changed database copies.
- Mutate the database only through Papertiger commands and public APIs.
- Ensure the database plus `-journal`, `-wal`, and `-shm` sidecars are ignored
  before `init`. Never replace a missing authority with a fresh one when prior
  work clearly existed.
- `init` is the only creation and migration command. Read commands never
  migrate; follow their exact corrective command deliberately.
- `export` is transfer and recovery, not a second live authority.

Papertiger owns modeled plans, tasks, dependencies, blockers, gates, and event
history. Domain evidence and issue systems remain authoritative for their own
facts. Markdown carries doctrine and rationale, never duplicated live status.

## Start from live truth

```bash
scripts/papertiger status
scripts/papertiger focus --json
scripts/papertiger show <task.seq> --json
scripts/papertiger audit
```

If more than one plan is active, pass `--plan <slug>` to plan-scoped reads.
`task.seq`, written as `N` or `#N`, is the only task selector. Aliases are
traceability metadata.

## Mutations

Set `PAPERTIGER_ACTOR` to the acting agent name before mutating. Write `--why`
for anything a future session could question, using language that stands alone
without chat context.

```bash
scripts/papertiger start <task.seq> --why "Why execution starts now"
scripts/papertiger gate close <task.seq> <name> \
  --evidence file:path/to/receipt.json --sha256 <digest>
scripts/papertiger done <task.seq>
```

Probe and decision tasks require `--result`. `done` refuses open dependencies,
blockers, gates, or children; close or waive them with evidence and reasons
rather than routing around the refusal. Check `list --status rejected` before
reviving an old approach.

Do not create Papertiger tasks for same-session checklist steps or leave tasks
`in_progress` when no agent owns them.

## Project-local installation

`setup-project` owns only these managed files:

- `tools/papertiger/bin/papertiger[.exe]` (host-local and ignored)
- `tools/papertiger/agent_integration.md`
- `scripts/papertiger`
- additive Papertiger entries in `.gitignore`

Commit `scripts/papertiger` with executable mode. On a host filesystem that
does not preserve that bit, run `git update-index --chmod=+x
scripts/papertiger` before committing it.

It never edits `AGENTS.md` or `CLAUDE.md` and never initializes authority.
Preview upgrades with `setup-project <root> --dry-run --json`; use
`--replace-managed` only after reviewing a reported replacement.

## Mise is an episodic external driver

`papertiger-mise` is included in every Papertiger release but is not vendored
by `setup-project`. When a bounded RSI campaign is warranted, invoke the stable
peer binary from the release against the consumer:

```bash
papertiger-mise --project-root <repository> status --json
papertiger-mise --project-root <repository> init
```

The consumer owns `state/papertiger-mise.sqlite`,
`state/papertiger-mise-objects/`, and campaign workspaces. The release binary
is part of the frozen outer judge and must not change during the campaign.
Read `MISE.md` from the same release before campaign admission.

Mise nominations are evidence, never planning completion, integration,
promotion, or deployment authority. Historical and domain-shadow evidence is
permanently decision-ineligible. Projection back into planning is two-key:
derive with `papertiger-mise projection inspect`, then attach with
`papertiger mise project`; the projection cannot close a task or gate.
