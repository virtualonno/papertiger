# Papertiger

Papertiger is a local planning tool for engineering work that continues across
agent sessions. It stores plans, tasks, dependencies, blockers, proof gates,
decisions, and event history in SQLite. A new session can read the current
state instead of reconstructing it from handoff notes.

Papertiger has no server or account. It ships as two Rust binaries:

- `papertiger` is the planning tool. It is the only binary installed into a
  consuming project.
- `papertiger-mise` runs optional, experimental candidate-evaluation
  campaigns. It stays outside the consuming project and has no authority to
  close planning tasks, integrate changes, or deploy software.

The vendored planning skill supplies the ordinary task workflow without
requiring the full installation reference. It resumes existing outcomes,
records validated follow-up, and leaves transient checklist steps alone.
For a fixed comparison of plausible candidates, start with `papertiger-mise
guide`; `campaign inspect <id>` discovers retained work in bounded pages.
The guide is not a ready-made domain evaluator, and inspection does not reverify
CAS evidence or establish that a campaign can still execute.

## Personal installation (default)

From a verified extracted release, run:

```text
papertiger setup-user --dry-run
papertiger setup-user
```

This installs one canonical skill in `~/.agents/skills/papertiger`, a Claude router
in `~/.claude/skills/papertiger`, and a native runtime plus detailed reference under
`~/.local/share/papertiger` (the same home-relative layout on Windows). The skill
binds the exact executable; no PATH, shell profile, AGENTS.md, CLAUDE.md, hooks,
or consuming-project files are changed. `--home <existing-directory>` selects
an explicit user home, including disposable test homes. Start a fresh agent
session and verify the skill appears. Skill descriptions support automatic
selection; they do not guarantee a model will choose the tool on every request.

Both skill paths share one semantic body. Codex, Pi and Cursor can discover the
shared Agent Skills location; Claude uses its router. Other harnesses may need
an explicit skill-directory setting. A synced skill does not install a native
runtime in a remote/cloud environment: install there separately.

A host-local `user-install.json` binds installed files to raw byte hashes and
the tool version. Owned upgrades need no replacement flag. Conflicting unowned or modified
files refuse; review them before using `--replace-managed`. The installed
runtime refuses a missing or divergent receipt/file set. Run repair or upgrade
from an external release, not the installed executable. Installation preflights
all managed paths, but does not promise a crash-atomic multi-file transaction;
an interrupted install must be repaired before the runtime can run.

`uninstall-user --dry-run` previews removal. `uninstall-user` removes only
receipt-owned matching runtime/skill files and retains the lifecycle receipt;
it never removes project installations or unrelated skills. Do not copy personal
receipts between machines or move their home: install for the new home instead.

Personal setup initializes a private fallback database and `personal` plan
through the planner commands. Existing stores are checked, never silently
migrated or replaced. Uninstall preserves that database and its sidecars. Missing
expected history refuses setup until restored. Ordinary work selects an existing
project/domain authority first; otherwise the installed skill binds the private
store explicitly and records the consuming-project root in each task intent.
No database or planning configuration is created in consuming projects.

Use `setup-project` below only for explicit shared repository adoption, pinned
project runtimes, or repository-owned policy. Existing project receipt choices
remain intact. Project guidance triggers are optional for skills-capable agents.


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
- `show --json` is the task work record. Task context v7 includes full details
  for the selected task and compact identity/status summaries for related
  tasks; read a related task explicitly for its full context. `schema` prints
  the bundled JSON Schema for context, status, task lists, history, recovery,
  and mutation receipts without opening a database. All fields are local
  planning data and require editorial review before external publication.
- Mutations accept `--json` to emit `papertiger.mutation.v1`: the exact committed
  events and their task/plan snapshots, captured inside the transaction. Clients
  obtain new task selectors from `events[].task.seq` without scraping prose.
  Refusals emit no success receipt; idempotent operations can report
  `changed=false`. Initialization has its own human-readable migration result.
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
  close.
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
- Export and import preserve task identity, graph structure, evidence pointers,
  and history without creating a second live authority. `export --output`
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
and only the selected thin skill envelope: `.agents/skills` for the open Agent
Skills convention, `.claude/skills` for Claude, both, or neither. The tracked
receipt binds the release, authority path, resolved skill targets, and hashes
the managed text: the canonical contract and selected skill envelopes. It
deliberately omits platform-specific binary bytes so clones stay portable.
Instead, the ignored sibling
`tools/papertiger/bin/papertiger[.exe].runtime-install.json` records the exact
installed path, byte count, and SHA-256. `setup-project --dry-run --json`
exposes that identity in `runtime_install` without an ad hoc hash command. Its
`papertiger.project_setup.v5` result also includes `project_guidance`, a bounded
read-only observation of repository-root `AGENTS.md` and `CLAUDE.md`; setup
still never edits or owns either file. The host receipt is written atomically
as the final installation commit marker.
Ordinary receipt discovery refuses a missing, malformed, or mismatched host
receipt with the repair command. This identity is an observable local fact,
not a claim that separate platform builds reproduce identical bytes. The
tracked receipt itself and additive `.gitignore` policy also sit outside the
text hash list. A normal upgrade automatically replaces only receipt-matching
prior managed text and repairs missing files; modified managed text refuses
with a corrective action, and receipt-retired paths are removed only when prior
ownership is hash-proven.
For a host receipt that must change, a current tracked receipt plus a valid
prior runtime receipt that exactly matches the installed native binary proves
ownership even when the release version or integration contract changes. A
non-identical existing host receipt whose ownership is malformed, mismatched,
legacy, or otherwise unproved is reported in the dry-run and requires reviewed
`--replace-managed`; a missing host receipt is repaired without claiming an
existing file.
An older release also refuses to downgrade a newer receipt, even with
`--replace-managed`; rerun setup with the recorded release or a newer one.
A pre-receipt vendor manifest at `tools/papertiger/README.md` is accepted as a
predecessor receipt only when its recorded binary, agent-contract, and Mise
contract SHA-256 values match the files on disk. Setup can then replace the
owned contract and retire that manifest plus the old direct binary and Mise
copy without a replacement flag. A changed bundle, an unrecognized README, or
a full source tree refuses; `--replace-managed` cannot authorize guessed
retirement. The flag remains available for explicit recovery of a modified
current managed path.

Setup appends required `.gitignore` entries but does not initialize a database
or edit the project's agent guidance. It installs no hooks, MCP server, global
configuration, or harness update. Existing planning state is left untouched.
Setup never invokes Git, and ignore rules do not untrack an existing path. If
the host binary or selected authority is already tracked, review it and remove
only its index entry with `git rm --cached -- <path>`, preserving the local
file.
This source repository retains one skill template, installed canonically under
`.agents/skills`. Claude gets a thin router with inherited discovery metadata
and a link to that file; Claude selection also installs its canonical dependency.
Harnesses that do not load either skill location can use the canonical contract through concise
repository-owned guidance without installing a generic resident skill.

After installation, inspect that repository-owned discovery surface from the
project root or any nested directory:

```bash
tools/papertiger/bin/papertiger inspect-project-guidance --json
papertiger --project-root /path/to/project inspect-project-guidance --json
```

The command validates the project receipt and host runtime but never opens the
planning authority. It reads only regular, non-symlink repository-root
`AGENTS.md` and `CLAUDE.md`, with a fixed 64 KiB cap per file. Deterministic JSON
distinguishes an exact selected-skill trigger, a generic Papertiger-skill
trigger, `CLAUDE.md` indirection to `AGENTS.md`, an integration pointer, a bare
mention, stale positive shell-launcher wording, absence, and bounded refusal.
It also reports exact byte identity when both files were read. These are lexical
observations, not proof that a harness discovers or follows the guidance;
nested guidance and imported semantics remain outside the result.

To remove the integration, use the same verified release from outside the
consuming project:

```bash
papertiger uninstall-project /path/to/project --dry-run --json
papertiger uninstall-project /path/to/project --json
```

Uninstall requires a matching-version receipt and removes only receipt-owned
contract and skill files, the native binary when its bytes equal the external
release binary, its exact host receipt, and finally the tracked receipt. It
refuses modified files, a differing binary or host receipt, and project-local
self-deletion before writing. It deliberately retains the planner authority
and SQLite sidecars, Mise authority and evidence store, repository guidance,
unrelated skills, and the complete `.gitignore` policy. Removing or archiving
retained authority is a separate data-lifecycle decision.

## Start planning

Resolve and invoke the installed native binary directly. If `papertiger` is on
`PATH`, use that name. Otherwise select the release-managed binary at
`tools/papertiger/bin/papertiger` on POSIX or
`tools\papertiger\bin\papertiger.exe` on Windows. The following examples use
`papertiger` for that resolved executable:

```bash
papertiger status
papertiger focus --json
papertiger search "<terms>" --json
papertiger log --json
```

The binary walks upward from the current directory to find the nearest tracked
`tools/papertiger/project-install.json`, verifies its version and the matching
host-local runtime receipt and binary identity, then resolves its authority
against that project root. This works from nested directories without a
launcher, shell transition, or process bridge.
For an intentional command issued from a different repository, select the
canonical installed project explicitly:

```bash
papertiger --project-root /path/to/canonical-project status
```

`--project-root` requires the exact root's receipt and version; it never walks
upward into another project or falls back to a new default database. Choose
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

The current planner authority schema is v10. Before migrating an older authority,
archive its export with the matching Papertiger release and use the new release's
`--db <source> backup --output <new-path> --json` to create a standalone SQLite
recovery file. Current import
accepts only `papertiger.dump.v9`; restore an older dump with the release that
produced it, migrate that temporary authority, and re-export it. Migration preserves
old in-progress tasks without inventing session identities. Exported pickup context
remains advisory after recovery; it never becomes a lock.

`backup` preserves planner schemas 1–10 and committed WAL data through SQLite's
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
papertiger evidence verify --outcome failed --task-state open --limit 50 --json
papertiger evidence verify --outcome unsupported --task-state terminal --json
```

When receipt discovery can identify the project root, `--project-root` is
optional. The global option both selects that receipt's authority and supplies
the evidence root. With an explicit `--db`, it supplies only the evidence root.
A failed binding reports ordered `program` and `arguments` arrays for the
explicit reopen, close or resolve, and re-completion workflow. Version 2 JSON
always reports counts for the complete task scope in `summary`; filtering and
`--limit` affect only `projection`. The summary also breaks results down by
exact status and unsupported scheme, so resolver gaps are visible without
paging through every binding. The default projection is `--outcome incomplete`,
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

The installer copies [agent_integration.md](agent_integration.md) into the
project. After reviewing it, incorporate its concise repository-guidance
discovery trigger into the project's existing agent guidance. A bare link is
not equivalent: the trigger names both the multi-outcome/exact-resume cases and
the bounded/read-only/domain-lifecycle skips. Setup never edits repository-owned
guidance itself. Use `inspect-project-guidance --json` to audit the bounded
repository-root surface without transferring ownership to setup.

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

Use `search "terms" --compact --json` for ranked task identities and bounded
matched excerpts. Its `papertiger.search_compact.v1` projection preserves totals,
truncation, ranking and match provenance. Follow with `show N --no-history --json`
for the full current task, relationships and obligations without historical
payloads (`papertiger.task_current.v1`). The returned `history_command` retrieves
the event history; read it before interpreting a rejected proposal or a prior
decision. Ordinary `search --json` and `show --json` retain their full contracts.

`plan list --json` returns every plan, including paused and terminal plans, with
full orientation; `plan list --plan SLUG --json` retrieves one known plan.
`list --all-plans --status unfinished --json` inventories proposed and in-progress
tasks across **every** plan state, independently of readiness. Each row identifies
its plan and plan state. The default page is 100 rows (`--limit 1..500`). Continue
with the same filters, returned `next_after_seq` as `--after-seq`, and `--snapshot`.
`total` counts all matching tasks; `remaining` counts matching tasks after this
page. A changed event history refuses continuation: restart the inventory rather
than combining different snapshots. These reads neither resume plans nor change
mutation authority. Ordinary plan-selected `list` is unchanged.

## License

Distributions include [LICENSE](LICENSE), [LICENSE-SSL](LICENSE-SSL), and
[LICENSE-VPL](LICENSE-VPL).
