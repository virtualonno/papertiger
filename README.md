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
  identifiers.
- `show` exposes event-derived activity. `started_event` exists only while a
  task is currently in progress, and `completed_event` only while it is
  currently done. Their actor fields record transition authorship, not task
  ownership. `list --sort activity` orders work by the latest meaningful event
  without inventing time tracking.
- `log --json` provides full event records and history-bound cursors for older
  pages or incremental reads. A cursor from divergent history refuses.
- `search` ranks literal terms across task titles, tags, intent, results, and
  event rationale. It includes done, retired, and rejected history by default
  and needs no external or separately synchronized index.
- Probe and decision tasks require `--result` or `--result-file` before they can
  close.
- Open dependencies, blockers, gates, and child tasks prevent completion.
- Every mutation records an actor and an event. Actor labels are provenance,
  never assignees, leases, session handles, or liveness signals; unfinished
  work remains `in_progress` across agent replacement without reassignment.
- Optional caller-resolved commit associations support local task/commit lookup
  without invoking Git or coupling commits to completion.
- `retire <old> --into <canonical> --why ...` records measured task
  consolidation as a same-plan replacement without turning tasks into a
  generic relation graph or redirecting `show`. A canonical target with inbound
  replacements cannot be rejected or retired without its own live replacement;
  explicit `retire --into` chains preserve the history without silent rewrites.
- Every connection gets a fixed 500 ms SQLite lock grace so brief read and
  mutation overlap among agents sharing the canonical authority clears
  naturally. Papertiger never replays a command; a longer lock is refused with
  an explicit retry instruction. Independent authorities are never merged or
  synchronized.
- Export and import preserve task identity, graph structure, evidence pointers,
  and history without creating a second live authority. `export --output`
  atomically writes a canonical recovery file and returns its SHA-256 receipt.

Papertiger is intended for plans that outlive one session or carry meaningful
dependencies and proof obligations. A short same-session checklist does not
need it. Domain evidence and issue trackers remain authoritative for their own
facts.

Task sequences are local selectors, not shared issue identifiers. Never place a
Papertiger task number in a shared commit, pull request, changelog, or public
artifact. Shared prose must stand alone; record stable external URLs and useful
Git snapshot associations inward in the private Papertiger authority.

## Install it in a project

Download the archive for your platform from
[GitHub Releases](https://github.com/virtualonno/papertiger/releases), verify
the adjacent SHA-256 checksum, and extract it outside the consuming project.
For an upgrade, run `setup-project` from that newly verified release binary;
the existing project launcher cannot update itself.

Preview the installation:

```bash
papertiger setup-project /path/to/project --dry-run --json
```

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

Setup installs the planner, Bash and Windows launchers, the agent contract, and
byte-identical open Agent Skills discovery envelopes for `.agents/skills` and
`.claude/skills`. The tracked receipt binds the release and authority path, and
hashes the managed text: both launchers, the canonical contract, and both skill
envelopes. The receipt also owns the host-local binary, but deliberately omits
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
This source repository retains the single skill template; the two discovery
copies exist only in projects managed by `setup-project`.

## Start planning

From the repository root, use the launcher for the active shell:

```bash
scripts/papertiger status
scripts/papertiger focus --json
scripts/papertiger search "<terms>" --json
scripts/papertiger log --json
```

```powershell
.\scripts\papertiger.cmd status
.\scripts\papertiger.cmd focus --json
.\scripts\papertiger.cmd search "<terms>" --json
.\scripts\papertiger.cmd log --json
```

From a nested directory, invoke that same launcher through a path that actually
resolves to the repository's `scripts` directory, such as
`../../scripts/papertiger` or `..\..\scripts\papertiger.cmd`. Once invoked, both
launchers derive the project root from their own location, so database identity
does not depend on the caller's working directory.

Set `PAPERTIGER_ACTOR` to a concise author label before mutations. It describes
who wrote an event, not who owns the task now. Each project-local launcher
defaults to the receipt-selected database at the project root. `PAPERTIGER_DB`
or an explicit global `--db` deliberately overrides that default for
operational use. Run `init` only when no prior authority should exist; on an
upgrade, follow a schema refusal's exact migration command deliberately.

The current authority schema is v6. Before migrating an older authority, use
its matching Papertiger release to archive its current export. Current import
accepts only `papertiger.dump.v6`; restore an older dump with the release that
produced it, migrate that temporary authority, and re-export it.

The installer copies [agent_integration.md](agent_integration.md) into the
project. Incorporate its short operating rules into the repository's existing
agent guidance after reviewing them.

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
