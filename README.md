# Papertiger

Papertiger is a local planning tool for engineering work that continues across
agent sessions. It stores plans, tasks, dependencies, blockers, proof gates,
decisions, and event history in SQLite. A new session can read the current
state instead of reconstructing it from handoff notes.

Papertiger has no server or account. It ships as two Rust binaries:

- `papertiger` is the planning tool. It is the ordinary planning entrypoint.
- `papertiger-mise` runs optional, experimental candidate-evaluation
  campaigns. It is bundled alongside the planner but has no authority to
  close planning tasks, integrate changes, or deploy software.

The vendored planning skill supplies the ordinary task workflow without
requiring the full reference. It resumes existing outcomes,
records validated follow-up, and leaves transient checklist steps alone.
For a fixed comparison of plausible candidates, start with `papertiger-mise
guide`; `campaign inspect <id>` discovers retained work in bounded pages.
The guide is not a ready-made domain evaluator, and inspection does not reverify
CAS evidence or establish that a campaign can still execute.

## Add to a project (default)

Download the archive for the machine where the agent runs, verify it against
its adjacent `.sha256` file, and merge its contents into the project root,
including the dot-directories:

```text
.agents/skills/papertiger/SKILL.md
.claude/skills/papertiger/SKILL.md
tools/papertiger/bin/papertiger[.exe]
tools/papertiger/bin/papertiger-mise[.exe]
tools/papertiger/manifest.json
tools/papertiger/agent_integration.md
tools/papertiger/README.md, CHANGELOG.md, MISE.md and the licenses
```

The skills and native executable are already in place. Start a fresh agent
session; no install command, PATH change, AGENTS.md edit, or integration-guide
reading is needed for ordinary work. Claude receives the same complete short
skill body generated from the canonical template. Codex, Pi, Cursor, OMP and
OpenCode can use the shared Agent Skills directory; model selection still
remains discretionary.

Only namespaced skills and `tools/papertiger` are shipped. Existing project guidance,
configuration, receipts and databases are not included or overwritten. Preserve
any customizations inside those tool-owned directories before replacing them.
The checksum verified at download is the binaries' integrity check; Papertiger
does not re-hash its executables when it runs. `manifest.json` names the
release and its source commit, and a binary refuses a project whose manifest
names another release. Papertiger creates a database only through `init`, for
a project with no planning history; established projects reuse theirs. Add
`state/papertiger.sqlite` and its `-journal`, `-wal` and `-shm` sidecars to
`.gitignore` before `init` (`setup-project` writes these rules).

## Optional personal installation

From a verified extracted release, run:

```sh
# macOS/Linux, inside the extracted release
./tools/papertiger/bin/papertiger setup-user --dry-run
./tools/papertiger/bin/papertiger setup-user
```

```powershell
# Windows PowerShell, inside the extracted release
.\tools\papertiger\bin\papertiger.exe setup-user --dry-run
.\tools\papertiger\bin\papertiger.exe setup-user
```

This installs one canonical skill in `~/.agents/skills/papertiger`, an identical generated skill
in `~/.claude/skills/papertiger`, and a native runtime plus detailed reference under
`~/.local/share/papertiger` (the same home-relative layout on Windows). The skill
binds the exact executable; no PATH, shell profile, AGENTS.md, CLAUDE.md, hooks,
or consuming-project files are changed. `--home <existing-directory>` selects
an explicit user home, including disposable test homes. Start a fresh agent
session and verify the skill appears. Skill descriptions support automatic
selection; they do not guarantee a model will choose the tool on every request.
The installer writes both complete skill files itself and executes the copied
runtime before reporting success. No agent-side copying or routing setup remains.

Both skill paths share one semantic body. Codex, Pi and Cursor can discover the
shared Agent Skills location; Claude reads its generated copy. Other harnesses may need
an explicit skill-directory setting. A synced skill does not install a native
runtime in a remote/cloud environment: install there separately.

A host-local `user-install.json` lists the installed paths and records the tool
version and home; it records no hashes. The skills, reference and runtime
belong to the release: setup writes them as shipped, so keep personal guidance
elsewhere. Before every command, the installed runtime refuses a missing
receipt or one that names another release or home, naming `setup-user`.
Run repair or upgrade from an external release, not the installed executable.
Installation preflights all managed paths, but does not promise a crash-atomic
multi-file transaction; an interrupted install must be repaired before the
runtime can run.

`uninstall-user --dry-run` previews removal. `uninstall-user` removes every
receipt-listed path (skills, reference and runtime) without comparing content,
so an edited file is removed like an intact one. It refuses a link or
non-file at an owned path, then retains the lifecycle receipt;
it never removes project installations or unrelated skills. Do not copy personal
receipts between machines or move their home: install for the new home instead.

Personal setup initializes a private fallback database and `personal` plan
through the planner commands. Existing stores are checked, never silently
migrated or replaced. Uninstall preserves that database and its sidecars. Missing
expected history refuses setup until restored. Ordinary work selects an existing
project/domain authority first. The personal executable discovers a project
receipt before falling back to its private store, without a `--db` argument.
The skill records the consuming-project root in each personal task intent.
No database or planning configuration is created in consuming projects.

Use `setup-project` below only for explicit shared repository adoption, pinned
project runtimes, or repository-owned policy. Existing project receipt choices
remain intact. Skill descriptions route agents; no project guidance edit is needed.


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
- Mutations accept `--json` to emit `papertiger.mutation.v1`: the exact committed
  events and their task/plan snapshots, captured inside the transaction. Clients
  obtain new task selectors from `events[].task.seq` without scraping prose.
  Refusals emit no success receipt; idempotent operations can report
  `changed=false`. Initialization has its own human-readable migration result.
- `decompose <N> --outline-file <path|->` creates a parent's child tasks and
  their dependencies from one `papertiger.task_outline.v1` outline in a single
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
- `--model <model-id>` or `PAPERTIGER_MODEL` optionally records caller-reported
  event authorship independently of actor and meaning source. Creation,
  completion, and status history expose it; historical unknowns remain null.
  `--reasoning-effort` or `PAPERTIGER_REASONING_EFFORT` records known configured
  effort separately alongside the model, such as `gpt-6-astra` and `high`.
  This is attribution, not authenticated identity or a model-quality score.
- `move-plan <N>... --plan <slug> --why <reason>` moves an explicitly selected
  related set atomically. Dependencies, hierarchy, replacement links, gates,
  references, status, and task identity remain intact. A partial set refuses
  with the missing task selectors. Historical events keep their original plans.
- `reference add <N> <locator> --kind <kind>` records inward issue, pull-request,
  review, ADR, input, or other references. Optional `--sha256` and `--note`
  retain byte identity and context; `reference list`, `find`, and `remove`
  support exact archaeology. References never fetch data or satisfy gates.
  A stored reference hash is an assertion; `evidence verify` continues to
  verify gate and blocker bindings, not these planning inputs.
- `log --json` provides full event records and history-bound cursors for older
  pages or incremental reads. A cursor from divergent history refuses. New
  task-edit events carry canonical before/after definition revisions; old edit
  events are labeled `legacy_without_snapshots` rather than assigned invented
  history.
- `search` ranks literal terms across task titles, tags, intent, results, and
  event rationale. It includes done, retired, and rejected history by default
  and needs no external or separately synchronized index.
- Probe and decision tasks require `--result` or `--result-file` before they can
  complete.
- `note --text <text>` or `note --text-file <path|->` records a free-standing
  evented note, optionally on one task with `--task <N>`; the two text forms
  are mutually exclusive and one is required.
- Open dependencies, blockers, gates, and child tasks prevent completion.
- Every change records an actor and an event; idempotent identified pickup emits none. Actor labels are provenance,
  never assignees, leases, session handles, or liveness signals; unfinished
  work remains `in_progress` across agent replacement without reassignment.
- `--session <id>` or `PAPERTIGER_SESSION` records advisory task pickup separately
  from actor provenance. `focus` prefers your session's work and available
  alternatives over work last picked up elsewhere. Another session's pickup is
  never a blocker: `start` resumes unfinished work directly, with no release,
  expiry, heartbeat, or harness integration. It does not detect liveness or
  guarantee exclusive execution; concurrent explicit starts can both succeed.
- `add --start` creates and starts a task atomically, rolling back task and
  events on readiness failure. Intent, result, and note text may separately
  record a `user`, `agent`, or `external` meaning source without confusing it
  with the actor that performed the mutation.
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
- Export and import preserve task identity, graph structure, gates, blockers,
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

Papertiger is intended for independently reviewable outcomes, separate commits,
or work with meaningful dependencies and proof obligations. That boundary can
apply even when several outcomes are requested in one session; intermediate
steps inside one independently reviewable outcome do not need tasks. Domain
evidence and issue trackers remain authoritative for their own facts.

Task sequences are local selectors, not shared issue identifiers. Never place a
Papertiger task number in a shared commit, pull request, changelog, or public
artifact. Shared prose must stand alone; record stable external URLs and useful
Git snapshot associations inward in the private Papertiger authority.

## Install it in a project

Download the archive for your platform from
[GitHub Releases](https://github.com/virtualonno/papertiger/releases), verify
the adjacent SHA-256 checksum, and extract it outside the consuming project.
For an upgrade, run `setup-project` from that newly verified release binary;
a project-local binary cannot overwrite itself while running on Windows.
A project that was also unpacked from a release archive carries
`tools/papertiger/manifest.json`, which discovery trusts ahead of the receipt.
When that manifest names another release or cannot be read, `setup-project`
and its `--dry-run` refuse before writing anything. Either unpack the new
release archive over the project root, which replaces `tools/papertiger` and
the skills while the receipt keeps selecting the authority, or move the
manifest aside and rerun `setup-project` to manage the project through its
receipt alone. Setup never writes or relabels a release manifest.

Preview the installation:

```bash
papertiger setup-project /path/to/project --dry-run --json
```

On a first install, the default `auto` selection follows existing harness
markers. `.agents`, `.codex`, `.cursor`, `.pi`, `.omp`, `.opencode`, `AGENTS.md`, or an
OpenCode `opencode.json` / `opencode.jsonc` file selects the shared `agents`
residence; `.claude` or `CLAUDE.md` selects `claude`; both marker families
select `both`; and an unmarked repository selects `none`. These are bootstrap
hints for `.agents/skills`, not harness-specific installation targets. Select
a target explicitly when needed:

```bash
papertiger setup-project /path/to/project \
  --skill-target agents --dry-run --json
```

Explicit selections are `agents`, `claude`, `both`, and `none`; `auto` reruns
marker detection. An upgrade with no `--skill-target` preserves
the receipt's existing selection, even if a harness directory is temporarily
absent. The dry-run prints an apply command with the resolved target frozen.

For an existing project whose canonical authority is not the default, declare
the project-relative path on the first receipt-backed cutover:

```bash
papertiger setup-project /path/to/project \
  --authority-path plans/papertiger.sqlite --dry-run --json
```

Later upgrades read that choice from `tools/papertiger/project-install.json`;
they never infer, move, initialize, or migrate the database.

Apply the same variant you previewed. The commands below are alternatives; the
dry-run deliberately writes no receipt:

```bash
# Default authority:
papertiger setup-project /path/to/project --json

# First cutover to an existing custom authority:
papertiger setup-project /path/to/project \
  --authority-path plans/papertiger.sqlite --json
```

Setup installs the native planner binary, the harness-neutral agent contract,
and only the selected short skill envelope: `.agents/skills` for the open Agent
Skills convention, `.claude/skills` for Claude, both, or neither. These files
belong to the release: setup writes them as shipped on every install, repair,
or upgrade, and a deselected skill target's file is removed. Keep
project-specific guidance outside them. The tracked receipt records the
release, authority path, and resolved skill targets. It omits
platform-specific binary bytes so clones stay portable. Setup replaces the
installed binary when it differs from the release binary running setup; the
binary is not checked again when it runs, because the release checksum
verified it at download. Setup removes a leftover
`tools/papertiger/bin/papertiger[.exe].runtime-install.json` from an earlier
release. Setup never edits or owns `AGENTS.md` or `CLAUDE.md`. A release
refuses to downgrade a newer receipt; rerun setup with the recorded release or
a newer one. Start a fresh agent session after setup changes a skill.

Setup appends required `.gitignore` entries but does not initialize a database
or edit the project's agent guidance. It installs no hooks, MCP server, global
configuration, or harness update. Existing planning state is left untouched.
Setup never invokes Git, and ignore rules do not untrack an existing path. If
the host binary or selected authority is already tracked, review it and remove
only its index entry with `git rm --cached -- <path>`, preserving the local
file.
This source repository retains one skill template. Setup generates the same
complete short body under `.agents/skills` and, when selected, `.claude/skills`.
Either discovered path is usable immediately; no second skill read is required.

To remove the integration, use the same verified release from outside the
consuming project:

```bash
papertiger uninstall-project /path/to/project --dry-run --json
papertiger uninstall-project /path/to/project --json
```

Uninstall requires a matching-version receipt and removes, by path and without
comparing content, the contract, the receipt's skill files, the native binary,
and finally the tracked receipt, so an edited file is removed like an intact
one. Before writing, uninstall refuses a symlink or
non-regular file at an owned path (`non_file_refusal` in its
`papertiger.project_uninstall.v3` result) and project-local self-deletion. It
deliberately retains the planner authority
and SQLite sidecars, Mise authority and evidence store, repository guidance,
unrelated skills, and the complete `.gitignore` policy. Removing or archiving
retained authority is a separate data-lifecycle decision.

## Start planning

Invoke the project's installed native binary directly:
`tools/papertiger/bin/papertiger` on POSIX or
`tools\papertiger\bin\papertiger.exe` on Windows. The following examples use
`papertiger` for that executable:

```bash
papertiger status
papertiger focus --json
papertiger search "<terms>" --json
papertiger log --json
```

The binary walks upward from the current directory to the nearest tracked
`tools/papertiger/project-install.json` or release `tools/papertiger/manifest.json`,
refuses one that names another Papertiger release with the corrective command
(a manifest naming the running release takes precedence over an older receipt's
recorded version; the receipt still selects the authority path),
then resolves its authority against that project root. This works from nested directories without a
launcher or shell wrapper.
For an intentional command issued from a different repository, select the
canonical installed project explicitly:

```bash
papertiger --project-root /path/to/canonical-project status
```

`--project-root` selects the exact root's receipt-selected authority (checking
its version); without a receipt, the release bundle's or an existing
`state/papertiger.sqlite`. It never walks upward into another project. Without
a receipt or bundle it selects only a database that already exists; `init`
creates only the receipt- or bundle-selected authority. Choose
that root by the initiative or outcome that owns the work, not by every
repository containing edited files. Keep a cross-repository outcome in one
authority and record external commits there with stable `--repo` labels.

Set `PAPERTIGER_ACTOR` to a concise author label before mutations. It describes
who wrote an event, not who owns the task now. The native binary defaults to
the receipt-selected database at the project root. `PAPERTIGER_DB`
or an explicit global `--db` deliberately overrides that default for
operational use. Ordinary commands refuse combining those raw database
overrides with `--project-root`; `evidence verify` alone retains the combination
so an explicitly selected database can resolve `file:` locators beneath the
supplied root. Run `init` only when no prior authority should exist; on an
upgrade, follow a schema refusal's exact migration command deliberately.

The current planner authority schema is v13. Before migrating an older authority,
use the new release's `--db <source> backup --output <new-path> --json` to create
a standalone SQLite recovery file, then run `init`. Import reads
`papertiger.dump.v10` and converts `papertiger.dump.v9`; restore an older dump
with the release that produced it, migrate that temporary authority, and
re-export it. Migration never rewrites stored events. Exported pickup context
remains advisory after recovery; it never becomes a lock. A connection must
enter through the public mutation API before it can write any planner table.
Ordinary SQLite writers fail with `papertiger_write_requires_public_api`:
use the project's installed Papertiger executable. Stored events and recovery
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
its bytes. Run the installed planner's explicit `init` to migrate an existing
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

When receipt discovery can identify the project root, `--project-root` is
optional. The global option both selects that receipt's authority and supplies
the evidence root. With an explicit `--db`, it supplies only the evidence root.
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

Run the complete local verification lane with:

```bash
bash scripts/cross_check.sh
```

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
matched excerpts. Its `papertiger.search_compact.v2` projection returns 5 results
by default and preserves totals, truncation, ranking and match provenance, with a
`continuation_command` when more match. Follow with `show N --no-history`
for the full current task, relationships and obligations without historical
payloads (`papertiger.task_current.v3`). The returned `history_command` retrieves
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

## License

Distributions include [LICENSE](LICENSE), [LICENSE-SSL](LICENSE-SSL), and
[LICENSE-VPL](LICENSE-VPL).
