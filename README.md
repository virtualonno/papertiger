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

## What the planner enforces

- `status --json`, `focus --json`, `list --json`, and `show <N> --json` provide
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
- Every mutation records an actor and an event. Actor labels are provenance,
  never assignees, leases, session handles, or liveness signals; unfinished
  work remains `in_progress` across agent replacement without reassignment.
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
- `evidence verify` resolves stored `file:` bindings beneath the project root,
  rejects path escapes and symlinks, hashes one stable read of each regular
  file, and fails closed on missing, unhashed, changed, or mismatched evidence.
  Failed bindings include exact corrective argument vectors. Other locator
  schemes remain explicitly unsupported until they have scheme-specific
  verifiers.

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
markers. `.agents`, `.codex`, `.pi`, `.omp`, `.opencode`, `AGENTS.md`, or an
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
the managed text: the canonical contract and selected skill envelopes. The
receipt also owns the host-local binary, but deliberately omits
its platform-specific bytes from that text hash list; every applied setup
upgrades it to the exact bytes of the running release binary. The receipt
itself and additive `.gitignore` policy also sit outside the hash list. A normal
upgrade automatically replaces only receipt-matching prior managed text and
repairs missing files; modified managed text refuses with a corrective action,
and receipt-retired paths are removed only when prior ownership is hash-proven.
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
This source repository retains the single skill template; selected discovery
copies in consuming projects stay byte-identical to it. Harnesses that do not
load either skill location can use the canonical contract through concise
repository-owned guidance without installing a generic resident skill.

To remove the integration, use the same verified release from outside the
consuming project:

```bash
papertiger uninstall-project /path/to/project --dry-run --json
papertiger uninstall-project /path/to/project --json
```

Uninstall requires a matching-version receipt and removes only receipt-owned
contract and skill files, the native binary when its bytes equal the external
release binary, and finally the receipt. It refuses modified files, a differing
binary, and project-local self-deletion before writing. It deliberately retains
the planner authority and SQLite sidecars, Mise authority and evidence store,
repository guidance, unrelated skills, and the complete `.gitignore` policy.
Removing or archiving retained authority is a separate data-lifecycle decision.

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
`tools/papertiger/project-install.json`, verifies that the receipt version
matches, and resolves its authority against that project root. This works from
nested directories without a launcher, shell transition, or process bridge.
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

The current authority schema is v8. Before migrating an older authority, use
its matching Papertiger release to archive its current export. Current import
accepts only `papertiger.dump.v7`; restore an older dump with the release that
produced it, migrate that temporary authority, and re-export it.

Planner and Mise schema v8 store distinct authority identities. Either binary
refuses the other authority without changing its bytes. A v7 authority is
migrated only by its matching binary's explicit `init` command; the dump shape
remains `papertiger.dump.v7` because authority identity is local metadata.

Verify retained local evidence without mutating the authority:

```bash
papertiger evidence verify --project-root /path/to/project --json
papertiger evidence verify --task <task.seq> --project-root /path/to/project
```

When receipt discovery can identify the project root, `--project-root` is
optional. The global option both selects that receipt's authority and supplies
the evidence root. With an explicit `--db`, it supplies only the evidence root.
A failed binding reports ordered `program` and `arguments` arrays for the
explicit reopen, close or resolve, and re-completion workflow.

The installer copies [agent_integration.md](agent_integration.md) into the
project. After reviewing it, incorporate its concise repository-guidance
discovery trigger into the project's existing agent guidance. A bare link is
not equivalent: the trigger names both the multi-outcome/exact-resume cases and
the bounded/read-only/domain-lifecycle skips. Setup never edits repository-owned
guidance itself.

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

## Further reading

- [MISE.md](MISE.md) defines campaign admission, evidence, budgets, and the
  promotion boundary.
- [CHANGELOG.md](CHANGELOG.md) records user-visible changes.

## License

Distributions include [LICENSE](LICENSE), [LICENSE-SSL](LICENSE-SSL), and
[LICENSE-VPL](LICENSE-VPL).
