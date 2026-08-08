# Papertiger

Papertiger is a local planning tool for engineering work that continues across
agent sessions. It stores plans, tasks, dependencies, blockers, proof gates,
decisions, and event history in SQLite. A new session can read the current
state instead of reconstructing it from handoff notes.

Papertiger has no server or account. It ships as two Rust binaries:

- `papertiger` is the planning tool. It is the only binary installed into a
  consuming project.
- `papertiger-mise` runs optional, experimental recursive-improvement
  campaigns. It stays outside the consuming project and has no authority to
  close planning tasks, integrate changes, or deploy software.

## What the planner enforces

- `status`, `focus --json`, and `show <N> --json` provide bounded orientation
  from the live database.
- Probe and decision tasks require a recorded result before they can close.
- Open dependencies, blockers, gates, and child tasks prevent completion.
- Every mutation records an actor and an event.
- Concurrent writers are refused immediately rather than hidden behind
  retries.
- Export and import preserve task identity, graph structure, evidence
  pointers, and history without creating a second live authority.

Papertiger is intended for plans that outlive one session or carry meaningful
dependencies and proof obligations. A short same-session checklist does not
need it. Domain evidence and issue trackers remain authoritative for their own
facts.

## Install it in a project

Download the archive for your platform from
[GitHub Releases](https://github.com/virtualonno/papertiger/releases), verify
the adjacent SHA-256 checksum, and extract it outside the consuming project.

Preview the installation:

```bash
papertiger setup-project /path/to/project --dry-run --json
```

Apply it:

```bash
papertiger setup-project /path/to/project --json
```

Setup installs the planner, project-local launchers, and the agent contract. It
appends the required `.gitignore` entries but does not initialize a database or
edit the project's agent guidance. Existing planning state is left untouched.

## Start planning

From the consuming project:

```bash
papertiger init
papertiger plan add delivery "Delivery"
papertiger add "Prove the release path"
papertiger status
papertiger focus --json
```

Set `PAPERTIGER_ACTOR` to the acting agent's name before mutations. Each
project-local launcher binds the default database to the project root,
including when called from a nested directory.

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
