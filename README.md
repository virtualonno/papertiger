# Papertiger

Papertiger is a local planning tool for engineering work that continues across
agent sessions. It stores plans, tasks, dependencies, proof gates,
decisions, and event history in SQLite. A new session can read the current
state instead of reconstructing it from handoff notes.

Papertiger has no server or account. It ships as two Rust binaries:

- `papertiger` is the planning tool. It is the ordinary planning entrypoint.
- `papertiger-mise` runs optional, experimental candidate-evaluation
  campaigns. It is bundled alongside the planner but has no authority to
  close planning tasks, integrate changes, or deploy software.

The planning skill supplies the ordinary task workflow without
requiring the full reference. It resumes existing outcomes,
records validated follow-up, and leaves transient checklist steps alone.
For a fixed comparison of plausible candidates, start with `papertiger-mise
guide`; `campaign inspect <id>` discovers retained work in bounded pages.
The guide is not a ready-made domain evaluator, and inspection does not reverify
CAS evidence or establish that a campaign can still execute.

## Install

Papertiger ships as one agent skill directory. Download the archive for the
machine where the agent runs from
[GitHub Releases](https://github.com/virtualonno/papertiger/releases), verify
it against its adjacent `.sha256` file, and extract it into your agent skills
directory. The archive holds one `papertiger/` directory:

```text
papertiger/SKILL.md
papertiger/references/agent_integration.md
papertiger/bin/papertiger[.exe]
papertiger/bin/papertiger-mise[.exe]
papertiger/README.md, CHANGELOG.md, MISE.md and the licenses
```

Extract it into `~/.agents/skills/` for Codex, Pi, Cursor, OMP, OpenCode and
other harnesses that read the shared Agent Skills directory, and into
`~/.claude/skills/` (a copy or a directory link) for Claude Code. Start a fresh
agent session. The skill runs `bin/papertiger` from beside itself, so no PATH
change, shell profile, AGENTS.md edit or per-project installation is needed.
Skill descriptions support automatic selection; they do not guarantee a model
will choose the tool on every request. A synced skill does not bring a native
executable to a remote or cloud environment: install there separately.

The checksum verified at download is the executables' integrity check;
Papertiger does not re-hash them when it runs.

To upgrade, replace the `papertiger/` directory with the new release's (no
Papertiger command may be running from it on Windows). When a release raises
the authority schema, each project's first read refuses with the exact `init`
command that migrates it; take a `backup` first. To remove Papertiger, delete
the directory; planning databases stay in their projects.

Each project keeps its authority at `state/papertiger.sqlite`. Start a new
project's authority with `papertiger --project-root /path/to/project init`;
in a Git working tree, `init` also adds the database, its `-journal`, `-wal`
and `-shm` sidecars and the Mise authority to `.gitignore`. Ignore rules do not
untrack an existing path: if an authority is already tracked, remove only its
index entry with `git rm --cached -- <path>`, preserving the local file.

## What the planner enforces

- `focus --json` provides concise task summaries with pickup identity, readiness,
  real blockers, and ranking information. Read it directly to choose work, then
  use `show <N> --json` for full context.
- `status --json`, `list --json`, and `show <N> --json` provide
  versioned orientation from the live database without exposing internal row
  identifiers. Status distinguishes in-progress parent and leaf work, and each
  bounded projection reports its scope, ordering, eligible, returned, and
  omitted counts plus a continuation command when incomplete. Read commands
  open the SQLite authority read-only by construction.
- `show` exposes event-derived activity. `started_event` exists only while a
  task is currently in progress, and `completed_event` only while it is
  currently done. Their actor fields record transition authorship, not task
  ownership. `list --sort activity` orders work by the latest meaningful event
  without inventing time tracking.
- `show --json` is the task work record. Task context v8 includes full details
  for the selected task and compact identity/status summaries for related
  tasks; read a related task explicitly for its full context. `schema` prints
  the bundled JSON Schema without opening a database; its top-level `oneOf`
  lists the covered documents. Not every JSON output is covered
  (`evidence verify` and `audit` are not). All fields are local
  planning data and require editorial review before external publication.
- Mutations accept `--json` to emit `papertiger.mutation.v2`: the exact committed
  events and their task/plan snapshots, captured inside the transaction. Clients
  obtain new task selectors from `events[].task.seq` without scraping prose.
  Refusals emit no success receipt; idempotent operations can report
  `changed=false`. Initialization has its own human-readable migration result.
- `decompose <N> --outline-file <path|->` creates a parent's child tasks and
  their dependencies from one `papertiger.task_outline.v2` outline in a single
  transaction. Dependencies name siblings by batch-local key or existing tasks
  by number; keys are never stored. The receipt lists one task create event per
  child in outline order, and plain output prints each key with its new task.
  The whole outline is validated first: an invalid entry, unknown reference,
  repeated tag or dependency, sibling cycle, dependency that waits for the
  parent, or title that repeats another child or a live child of the parent
  refuses every child, and the refusal lists each such entry problem. A JSON key
  repeated in any object, an outline over 1 MiB or 256 children, or a finished
  parent or plan refuses before the entries are checked.
  `--start-ready` also starts the children whose dependencies are all done
  tasks. A replay is refused by its duplicate titles only while the earlier
  children are still proposed or in progress; once they are finished, the same
  outline creates new children, so check `show <parent>` before retrying.
- Every write records its author, and a write without a model is refused.
  The model comes from `--model <model-id>` (the agent's own report) or
  `PAPERTIGER_MODEL` (configured where the session is launched), and each
  event records which as `model_source` (`flag` or `environment`). Model ids
  are stored lowercased. `PAPERTIGER_MODEL=human` records manual operator use
  and is refused inside a detected harness, so an agent that inherits the
  operator's shell is never recorded as the operator; `runtime` is reserved
  for transitions the tool performs itself. The harness (Claude Code, Codex,
  pi, OpenCode, Cursor) and its session come from the environment, as does
  Claude Code's reasoning effort; `--reasoning-effort` or
  `PAPERTIGER_REASONING_EFFORT` overrides it. The actor label defaults to the
  harness and a short session id (`claude-e2affada`), the operator for
  `human`, or else the model; `--actor`/`PAPERTIGER_ACTOR` overrides it. A
  subagent inherits its parent's environment, so a model set at launch can
  misattribute a subagent running another model; pass `--model` there. This is
  attribution, not authenticated identity or a model-quality score.
- `move-plan <N>... --plan <slug> --why <reason>` moves an explicitly selected
  related set atomically. Dependencies, hierarchy, replacement links, gates,
  references, status, and task identity remain intact. A partial set refuses
  with the missing task selectors. Historical events keep their original plans.
- `reference add <N> <locator> --kind <kind>` records inward issue, pull-request,
  review, ADR, input, or other references. Optional `--sha256` and `--note`
  retain byte identity and context; `reference list`, `find`, and `remove`
  support exact archaeology. References never fetch data or satisfy gates.
  A stored reference hash is an assertion; `evidence verify` continues to
  verify gate bindings, not these planning inputs.
- `log --json` provides full event records and history-bound cursors for older
  pages or incremental reads. A cursor from divergent history refuses. New
  task-edit events carry canonical before/after definition revisions; old edit
  events are labeled `legacy_without_snapshots` rather than assigned invented
  history.
- `search` ranks literal terms across task titles, tags, intent, results, and
  event rationale. It includes done, retired, and rejected history by default
  and needs no external or separately synchronized index.
- Every task has a kind chosen when it is created (`add --kind`, or `kind` on
  each outline child): `goal`, `feature`, `change`, `bug`, `probe`, `decision`
  or `release`. `work` remains only on tasks recorded before typed kinds and
  cannot be assigned; `audit` reports open `work` tasks with a suggested kind.
  Probe, decision and bug tasks require `--result` or `--result-file` before
  they can complete.
- `note --text <text>` or `note --text-file <path|->` records a free-standing
  evented note, optionally on one task with `--task <N>`; the two text forms
  are mutually exclusive and one is required.
- Open dependencies, gates, and child tasks prevent completion. An `external`
  gate (`gate add <N> <name> --kind external --requirement <condition>`) names
  a condition outside the task, such as a vendor fix, and also keeps the task
  from starting while open; it replaced blockers, which the v14 migration and
  the `papertiger.dump.v10` conversion turn into external gates.
- Every change records an actor and an event; idempotent identified pickup emits none. Actor labels are provenance,
  never assignees, leases, session handles, or liveness signals; unfinished
  work remains `in_progress` across agent replacement without reassignment.
- The session (harness-derived, or `--session <id>`/`PAPERTIGER_SESSION`)
  records advisory task pickup separately from actor provenance. `focus` prefers your session's work and available
  alternatives over work last picked up elsewhere. Another session's pickup is
  never a blocker: `start` resumes unfinished work directly, with no release,
  expiry, heartbeat, or takeover permission, and it guarantees no exclusive
  execution; concurrent explicit starts can both succeed. Where the harness
  names its process (`CLAUDE_PID`, `OPENCODE_PID`), a pickup records that
  owner by host, PID and birth time, and `focus`, `status` and `show` report
  `owner_state`: `ended` only when provable on the owner's host (the PID is
  gone, exited, or names a newer process), `running`, or `unknown` (another
  host, no recorded owner, or an uninspectable process). Age is never proof,
  and an ended owner changes nothing by itself.
- Review state is derived per claim: a task's filing and, once done, its
  result. Review is optional: text output marks only a disputed claim, with
  the model that made it, e.g. `[disputed · deepseek-v4.1-flash]`; `show`
  and the JSON of `show`, `focus` and `list` carry every claim's `review`. A filing becomes `engaged` when a
  different session starts the task or rewrites its title or intent.
  `review <N> --verdict confirmed|disputed --why ... [--result]` records a
  verdict from a different session (the claim's own session is refused).
  Nothing is refused or reranked, except that a disputed result holds its
  dependents and its parent's closure. `list --unreviewed` and
  `focus --unreviewed` select unreviewed or disputed claims for optional triage; results recorded before
  authorship are not flagged. Same-model reviews count and are reported as
  such (`same_model`).
- Priority is a scheduling level, `--priority now|next|later` (default
  `next`), ranked after readiness; sequence between tasks belongs in
  dependencies. The v14 migration and `papertiger.dump.v10` conversion map
  integer priorities: positive to `now`, zero to `next`, negative to `later`.
- `brief <N> [--json]` resumes a task and its whole subtree in bounded
  sections: intent, ancestry and filing review; progress over every
  descendant; the next ready leaves in focus order; open descendants with
  pickup and owner state; open gates (external conditions included); results
  of done descendants, latest first; and the latest notes. Each section
  reports what it omitted. It replaces hand-written handoff documents.
- `models [--plan <slug>] [--since <RFC3339>] [--json]` reports claim
  outcomes per authoring model, grouping ids that differ only by `.` or `-`
  between digits: filings (done, retired, rejected, open, engaged) and results
  (reopened), each with confirmed and disputed reviews split by whether the
  reviewer ran the same model, and unreviewed counts. Claims recorded before
  authorship group as `unknown`; groups under 10 claims are marked as small
  samples. It counts; it does not rank models.
- `add --start` creates and starts a task atomically, rolling back task and
  events on readiness failure.
- Optional caller-resolved commit associations support local task/commit lookup
  without invoking Git or coupling commits to completion.
- `retire <old> --into <canonical> --why ...` records measured task
  consolidation as a same-plan replacement without turning tasks into a
  generic relation graph or redirecting `show`. A canonical target with inbound
  replacements cannot be rejected or retired without its own live replacement;
  explicit `retire --into` chains preserve the history without silent rewrites.
- Every connection gets a fixed 500 ms SQLite lock grace so brief read and
  mutation overlap among agents sharing the canonical authority can clear.
  Papertiger never replays a command; a longer lock is refused with
  an explicit retry instruction. Independent authorities are never merged or
  synchronized.
- Export and import preserve task identity, graph structure, gates,
  evidence pointers, events and Mise projections without creating a second live
  authority. `export --output`
  atomically writes a canonical recovery file and returns its SHA-256 receipt.
- `evidence verify` reports full-scope integrity counts before a bounded detail
  projection. It resolves stored `file:` bindings beneath the project root,
  rejects path escapes and symlinks, hashes each regular file in bounded memory
  from one stable read, and fails closed on missing, unhashed, changed, or
  mismatched evidence. Failed bindings include exact corrective argument
  vectors. Other locator schemes remain explicitly unsupported until they have
  scheme-specific authority-backed verifiers.
- `gate resolve` refuses a new `file:` locator that is
  absolute, leaves the project root, separates components with `\`, or does
  not name an existing regular file beneath it, using the same path rules as `evidence verify`; with `--sha256`
  it also refuses a digest that does not match the file's current bytes and
  name the actual digest. The root is the
  explicit `--project-root` or the discovered project; an authority selected by
  `--db`, `PAPERTIGER_DB` or the personal store has none, so a `file:` locator
  there needs `--project-root`. Import restores stored locators unchecked, and
  other schemes are accepted as before.

Papertiger is intended for independently reviewable outcomes, separate commits,
or work with meaningful dependencies and proof obligations. That boundary can
apply even when several outcomes are requested in one session; intermediate
steps inside one independently reviewable outcome do not need tasks. Domain
evidence and issue trackers remain authoritative for their own facts.

Task sequences are local selectors, not shared issue identifiers. Output shows
each task by its handle, its kind letter and number (`F-924`); commands accept
the handle, `N` or `#N`, and refuse a handle whose letter no longer matches the
task's kind. Never place a Papertiger task number or handle in a shared commit,
pull request, changelog, or public artifact. Shared prose must stand alone; record stable external URLs and useful
Git snapshot associations inward in the private Papertiger authority.

## Start planning

Run the skill's `bin/papertiger` (`bin\papertiger.exe` on Windows); the
examples use `papertiger` for that executable:

```bash
papertiger status
papertiger focus --json
papertiger search "<terms>" --json
papertiger log --json
```

The binary walks upward from the current directory to the nearest
`state/papertiger.sqlite`, the way Git finds its repository, so it works from
nested directories without a launcher or shell wrapper. With no authority
above the working directory, commands refuse and name `--project-root` and
`init`. For an intentional command issued from a different repository, select
the canonical project explicitly:

```bash
papertiger --project-root /path/to/canonical-project status
```

`--project-root` selects that exact root's `state/papertiger.sqlite` and never
walks upward into another project; only `init` creates a missing one. Choose
that root by the initiative or outcome that owns the work, not by every
repository containing edited files. Keep a cross-repository outcome in one
authority and record external commits there with stable `--repo` labels.

Authorship describes who wrote an event, not who owns the task now.
`PAPERTIGER_DB` or an explicit global `--db` deliberately overrides the
discovered authority for operational use. Ordinary commands refuse combining those raw database
overrides with `--project-root`; `evidence verify` and `gate resolve` alone
retain the combination so an explicitly selected
database can verify or bind `file:` locators beneath the supplied root. Run `init` only when no prior authority should exist; on an
upgrade, follow a schema refusal's exact migration command deliberately.

The current planner authority schema is v14. Before migrating an older authority,
use the new release's `--db <source> backup --output <new-path> --json` to create
a standalone SQLite recovery file, then run `init`. Import reads
`papertiger.dump.v11` and converts `papertiger.dump.v10`; restore an older dump
with the release that produced it, migrate that temporary authority, and
re-export it. Migration never rewrites stored events. Exported pickup context
remains advisory after recovery; it never becomes a lock. A connection must
enter through the public mutation API before it can write any planner table.
Ordinary SQLite writers fail with `papertiger_write_requires_public_api`:
use the Papertiger executable. Stored events and recovery
mappings are append-only. Any trigger the executable did not install counts as
drift, because it would run inside admitted mutations. Missing, altered or
foreign triggers block writes but never reads, export or backup: `audit`
reports them and `repair-guards --why <reason>`, after a `--dry-run` preview,
reinstalls the guards and removes foreign triggers with an audited record of
the observed SQL. An altered history view also blocks reads until repaired. It
restores only the tool's own schema boundary and cannot write planning data. API
callers are trusted after mutation entry. This guards against accidental and
agent-driven raw SQL, not a filesystem owner deliberately replacing the guards
or registering its own admission function.

Earlier malformed rows remain visible to `audit`. After taking a backup,
`history inspect <event-id>` reports every original field and its digest.
`history quarantine <event-id> --expect-sha256 <digest> --why <reason>` explicitly
quarantines a structurally invalid record. It retains the original row unchanged
and appends a recovery event with its verbatim fields, raw payload, and defects.
Ordinary history and export use that envelope instead of the invalid row; export
and import preserve the envelope. Missing timezones and associations remain
unknown, and no task state is inferred. Quarantine never accepts structurally
valid history as a substitute for recording a disagreement.

`backup` preserves planner schemas 1–13 and committed WAL data through SQLite's
[online backup API](https://www.sqlite.org/backup.html). It publishes one recovery
file after SQLite integrity verification and returns its SHA-256, byte count,
original schema, and task/event counts. It refuses foreign authorities, newer
schemas, and existing destination files or sidecars. SQLite sidecar filename
suffixes are reserved on every platform. It records no event and performs no
migration or task/evidence semantic validation: historical records
that JSON import refuses can still be retained exactly. Inspect a recovery copy
with its matching release using `--db`; restoration is a deliberate operator
action, and the copy must never become a second live planning authority.

Planner and Mise store distinct authority identities; their schema versions
are independent. Either binary refuses the other authority without changing
its bytes. Run the planner's explicit `init` to migrate an existing
authority. Schema v9 adds external references and protects relocated history
from older readers. Current plan-scoped exports include only tasks currently
in that plan, their complete histories, and the former-plan definitions needed
to restore those histories. Prior event cursors remain valid after a move in
the same authority; importing a scoped export is a separate recovery history.

Verify retained local evidence without mutating the authority:

```bash
papertiger evidence verify --project-root /path/to/project --json
papertiger evidence verify --task <task.seq> --project-root /path/to/project
papertiger evidence verify --classification failed --task-state open --limit 50 --json
papertiger evidence verify --classification unsupported --task-state terminal --json
```

From inside the project, `--project-root` is optional: the discovered
authority's project is the evidence root. The global option both selects that
root's authority and supplies the evidence root. With an explicit `--db`, it supplies only the evidence root.
A failed binding reports ordered `program` and `arguments` arrays for the
explicit reopen, resolve, and re-completion workflow. Version 3 JSON
always reports counts for the complete task scope in `summary`; filtering and
`--limit` affect only `projection`. The summary also breaks results down by
exact status and unsupported scheme, so resolver gaps are visible without
paging through every binding. The default projection is `--classification incomplete`,
which includes failed and unsupported bindings without repeating verified
detail. Every projection states eligible, returned, omitted, and remaining
counts. When `has_more` is true, execute the exact structured
`continuation_command`; its cursor is bound to the project root, filters, task
scope, stored bindings, and live verification results, so drift refuses rather
than silently skipping entries.

A stored `file:` locator plus SHA-256 is the byte receipt for retained evidence;
it is not a repository snapshot. For a commit-backed outcome, retain and hash
the immutable audit receipt as evidence, then record the full commit object ID
separately with `commit add`. Neither identity substitutes for the other.

The installer copies [templates/agent_integration.md](templates/agent_integration.md) into the
project as the reference the skill links to for authority changes, migration
and recovery. No repository guidance edit is required.

## Add Mise when a campaign is warranted

Mise does not need to be installed into the project. Keep a stable Papertiger
release outside the project and point its peer binary at the consumer:

```bash
papertiger-mise --project-root /path/to/project status --json
papertiger-mise --project-root /path/to/project execution-status
papertiger-mise --project-root /path/to/project init
```

`status` is read-only and prints the initialization command when the project
has no Mise authority. `--project-root` resolves relative database, object,
manifest, and workspace paths from the consuming project rather than the
current directory or the Papertiger checkout.

The consuming project owns `state/papertiger-mise.sqlite`, the
content-addressed object store, and campaign workspaces. The campaign manifest
freezes the external Mise binary by path and hash, so that binary must not be
rebuilt during the campaign. When the campaign ends, stop invoking Mise; no
project-local runtime needs to be removed, and retained evidence stays
available for review.

A nomination is evidence, not promotion. Project integration still requires
the operator-owned proof path. Historical and domain-shadow evidence is never
eligible to decide a campaign.

Mise currently supports bounded, trusted development campaigns with an exact
Git source identity, a frozen evaluator, no-op and known-bad calibrations,
finite budgets, and retained evidence. It is not a security sandbox. Process
cleanup does not prove network, filesystem, display, secret, or adversarial
isolation. Read [MISE.md](MISE.md) before admitting a campaign.

## Build and verify

The Rust toolchain is pinned in `rust-toolchain.toml`.

```bash
cargo build --locked --release --workspace --bins
```

Run formatting, strict Clippy, workspace tests and release-script checks with:

```bash
bash scripts/cross_check.sh
```

Run the deterministic Mise lifecycle separately:

```bash
cargo test -p papertiger-mise --test deterministic_dogfood -- --ignored --test-threads=1
```

The release workflow also tests installation and removal using the extracted
archives on native runners. Source checks alone do not qualify an archive.

Release archives are built for Windows x64, Linux x64, Intel macOS, and Apple
Silicon macOS. Each archive has an adjacent `.sha256` file.

The GitHub Release Artifacts workflow defaults to building without publication.
Set `artifact_version` to the workspace version with a dated changelog section.
Source, MSRV, and native platform jobs run concurrently; publication requires all
of them to pass and an explicit `create_release=true` dispatch from `master`.
Each build retains rendered notes, four native archives, and adjacent SHA-256
files. The archive manifest identifies the source commit used for verification.

## Further reading

- [MISE.md](MISE.md) defines campaign admission, evidence, budgets, and the
  promotion boundary.
- [CHANGELOG.md](CHANGELOG.md) records user-visible changes.

### Progressive structured reads

Use `search "terms" --compact` for ranked task identities and bounded
matched excerpts. Its `papertiger.search_compact.v3` projection returns 5 results
by default and preserves totals, truncation, ranking and match provenance, with a
`continuation_command` when more match. Follow with `show N --no-history`
for the full current task, relationships and obligations without historical
payloads (`papertiger.task_current.v4`). The returned `history_command` retrieves
the event history; read it before interpreting a rejected proposal or a prior
decision. Ordinary `search --json` and `show --json` retain their full contracts.

`plan list --json` returns every plan, including paused and terminal plans, with
full orientation; `plan list --plan SLUG --json` retrieves one known plan.
`list --all-plans --status unfinished --json` inventories proposed and in-progress
tasks across **every** plan state, independently of readiness. Each row identifies
its plan and plan state. The default page is 100 rows (`--limit 1..500`). Continue
with the returned `continuation_command`, whose `--after-cursor` is bound to the
same filters and the authority snapshot. `total` counts all matching tasks;
`remaining` counts matching tasks after this page. A changed event history
refuses the cursor: restart the inventory rather than combining different
snapshots. These reads neither resume plans nor change
mutation authority. Ordinary plan-selected `list` is unchanged.

`list`, `focus`, `search` and `tree` accept `--kind <kind>` (`work` included);
`tree --kind` keeps each match under the ancestors that place it, and a cursor
from `list --all-plans` is bound to its kind filter too. `status` reports each
active plan's open tasks per kind (`open_by_kind`), e.g. `open by kind: F 3, B 1`.

## License

Distributions include [LICENSE](LICENSE), [LICENSE-SSL](LICENSE-SSL), and
[LICENSE-VPL](LICENSE-VPL).
