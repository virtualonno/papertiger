# Changelog

All notable user-visible changes are documented here. Papertiger follows
[Semantic Versioning](https://semver.org/) and this file follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- `setup-project --json` reports the installed native binary's exact path,
  byte count, and SHA-256 in `papertiger.project_setup.v4`. The same identity is
  stored atomically in the ignored per-installed-binary
  `papertiger[.exe].runtime-install.json`; ordinary receipt discovery refuses a
  missing or mismatched host receipt and directs the operator to repair from a
  trusted external release. The tracked `project-install` receipt remains
  clone-portable and contains no platform-binary hash.
- `evidence verify` emits `papertiger.evidence_verification.v2`: full-scope
  verified, failed, and unsupported counts precede a bounded detail projection.
  `--outcome all|incomplete|verified|failed|unsupported`, `--task-state
  all|open|terminal`, `--limit`, and scope-bound continuation cursors make
  whole-authority review actionable without weakening the fail-closed exit
  status. Regular files are streamed in bounded memory, and unsupported schemes
  remain ineligible until an authority-backed resolver exists.
- Global `--project-root <DIR>` selects the authority from the exact project's
  version-checked installation receipt, allowing commands issued from another
  repository to retain one canonical planning authority. Missing receipts and
  ambiguous use with `--db` or `PAPERTIGER_DB` refuse before opening a database;
  `evidence verify` retains the prior database-plus-evidence-root combination.

### Fixed

- Installed guidance distinguishes the repository being edited from the
  initiative that owns planning. Cross-repository outcomes stay in one
  authority, separately committed changes can be separate tasks in that
  authority, and external commit associations use stable `--repo` labels
  instead of duplicating tasks across unsynchronized project databases.

## [0.9.0] - 2026-08-24

This is the first public release after 0.7.1. Versions 0.8.0 and 0.8.1 were
unpublished development versions and have no public tags or release artifacts.

### Added

- `add --start` atomically creates and starts ready work with one rationale;
  any start refusal rolls back the task and its events.
- Intent, result, and note text can record a `user`, `agent`, or `external`
  meaning source independently of the actor that performed the mutation. The
  provenance is visible in task context and history and survives export/import.
- `evidence verify` checks retained `file:` bindings from one stable read of a
  project-contained regular file. It does not mutate the authority, refuses
  missing, unhashed, escaped, symlinked, or changed evidence, and reports exact
  corrective argument vectors.
- `blocker reopen` provides an evented repair path when a resolved blocker must
  be restored before rebinding evidence or completing work again.
- Status v2 distinguishes complete in-progress parent and leaf projections and
  gives every bounded ready-work and recent-note projection exact scope,
  ordering, eligible, returned, and omitted counts plus a continuation command.
- Task edits retain canonical before/after definition revisions in their event
  payloads. Public history labels older edit events without inventing snapshots,
  rejects no-op edits, and audits malformed new revisions.
- The native planner discovers the nearest tracked project-install receipt by
  walking upward, verifies its pinned Papertiger version, and binds the
  receipt-selected authority without a shell launcher or process bridge.
- `setup-project --skill-target auto|agents|claude|both|none` installs only the
  selected thin skill envelopes. Auto recognizes Codex, Pi, OMP, and OpenCode
  markers as bootstrap hints for the shared `.agents/skills` residence, never
  creates harness-native copies, and selects none for an unmarked repository;
  omitted target selection preserves an existing receipt choice.
- `uninstall-project` previews and removes only exact receipt-owned integration
  files with a matching external release binary. It retains planner and Mise
  authorities, evidence objects, repository guidance, unrelated skills, and
  `.gitignore` policy.

### Changed

- Planner and Mise authorities use schema v8 with distinct typed identities.
  Each binary refuses the other's database before migration or mutation. A v7
  authority requires its matching binary's explicit `init` command; transfer
  remains compatible with `papertiger.dump.v7` because modeled dump data did
  not change.
- Installed agent guidance treats independently reviewable outcomes, separate
  commits, and exact resume work as durable planning cases even when one
  session can finish them. It also names bounded edits, read-only work,
  intermediate steps, and domain-owned lifecycle as explicit skip cases.
  Directly requested outcomes record `intent_source=user`, and commit-backed
  outcomes receive an inward full-OID association before completion.
- The installed contract defines a repository guidance discovery trigger with
  those same boundaries. Project integration never edits a consuming
  repository's `AGENTS.md`, `CLAUDE.md`, or equivalent guidance.
- Task titles are limited to 160 characters. Tags are trimmed, limited to 64
  characters, and blank values are refused. Import applies the same canonical
  validation, and replacing sourced intent requires explicitly replacing or
  clearing its meaning source.
- Every top-level Mise command namespace now describes its operational boundary
  in `--help` instead of rendering as an unexplained noun.
- Read commands open the planning authority read-only by construction. The
  installed skill provides a self-contained cold-read path, requires the full
  authority contract only before mutation or advanced use, and never requires
  crossing shells merely to reach the planner.
- Project skills and operating guidance invoke the native planner directly.
  Contextmink's process bridge remains reserved for child-process and argument
  relay; it is not part of planner execution.
- Release publication unfolds source hard-wrapping before creating the GitHub
  description, preserving semantic headings, paragraphs, and list items.
- Release verification compiles every workspace target with the declared Rust
  1.95 minimum in addition to the pinned current toolchain gates.
- Setup receipt v2 records the selected harness targets. Exact legacy receipts
  remain readable, receipt-owned deselection retires only hash-matching skill
  files, and modified retired files refuse before writes.

### Fixed

- Import validates the complete dump before committing any rows, refuses event
  references outside the dump, and requires a reused plan to have the same
  definition as the destination plan.
- `log --after-cursor` reads from one SQLite snapshot, and `focus --json`
  reports scope, ordering, eligible, returned, omitted, completeness, and an
  exact continuation command for bounded results.
- Search refuses a result set too broad to return completely instead of
  silently truncating it, and plan-scoped exports retain global events as
  global rather than rebinding them to the selected plan.
- Missing, empty, corrupt, legacy, and foreign SQLite files produce
  authority-specific refusals with a corrective command or exact missing input.
  Corrupt event payloads are reported or refused rather than silently omitted.

### Removed

- Project-managed `scripts/papertiger` and `scripts/papertiger.cmd` launchers.
  Project identity and database selection no longer depend on shell scripts.

## [0.7.1] - 2026-08-13

### Fixed

- Canonicalized every setup-managed text artifact to LF before rendering or
  hashing, so a release binary built from a CRLF source checkout no longer
  rewrites tracked launchers during an otherwise unchanged project upgrade.
  Upgrading from 0.7.0 recognizes an equivalent managed launcher without
  rewriting it and never changes the local planning authority.

## [0.7.0] - 2026-08-12

This is the first public release after 0.5.0. Version 0.6.0 was an unpublished
development version and has no public tag or release artifact.

### Added

- Versioned JSON for authority status and task lists, plus full event-log
  records with bounded backward pages and incremental history cursors.
- Deterministic task search across titles, tags, intent, results, and event
  rationale, with field weighting, bounded excerpts, plan/status filters, and
  terminal history included by default.
- Atomic `export --output` recovery files with SHA-256, byte, schema, and row
  count receipts; existing destinations require explicit replacement.
- Event-derived task lifecycle timestamps and activity ordering for reliable
  cold-resume orientation without time-spent or productivity reporting.
- Optional, caller-resolved commit associations with local reverse lookup,
  strict full-object-ID validation, audit coverage, and export/import support.
- Byte-identical Agent Skills discovery envelopes for harness-portable,
  proactive use of the project-local planning contract.
- Uniform file/stdin input for durable intent, rationale, result, and note text,
  including an explicit refusal when two fields compete for stdin.
- Same-plan retirement replacements through `retire --into`, with cycle-safe
  storage, visible non-redirecting context, audit coverage, and v6 transfer.
- A tracked `project-install` receipt, explicit authority-path preservation,
  Bash and Windows launchers, safe missing-file repair, and ownership-proven
  full-cutover removal of retired managed artifacts.

### Changed

- Replaced timestamp-only lifecycle projections with event-backed activity that
  carries author provenance. Actor labels are explicitly not task assignment,
  leases, session identity, or liveness; unfinished work remains directly
  resumable when sessions change.
- Removed the redundant `next` command and made `status` and `focus` the
  canonical orientation surfaces.
- Removed internal database row identifiers from public task and plan JSON and
  advanced the affected focus and task-context schemas to v4.
- Reworded the installed skill and agent reference around ordinary proactive
  planning rather than a named discipline, and aligned public binary
  descriptions with their concrete task-planning and candidate-evaluation
  roles.
- Made task sequences explicitly authority-local and prohibited Papertiger task
  references in shared Git and release prose.
- Made `task.seq` the sole authority-local task identity across schema, CLI,
  API, and transfer. Schema v6 accepts only the `papertiger.dump.v6` contract;
  restoring an older dump requires its matching
  release, authority migration, and re-export.
- Standardized mutation subcommands on the single canonical `remove` spelling.
- Gave every SQLite connection a fixed 500 ms lock grace while keeping command
  execution single-shot and longer contention fail-closed.
- Restored exact-source release verification and expanded extracted-consumer
  smoke to prove receipts, both skill envelopes, and root/nested launchers.
- Made setup dry-run guidance replay the reviewed authority and managed-file
  replacement choices exactly instead of suggesting a newly blocked variant.
- Made task selectors canonical and nested CLI help self-describing for agents,
  including fail-closed rejection of inapplicable setup globals.
- Replaced raw source-size objectives in current Mise examples with frozen
  structural boundary and persisted-state hazard detectors.
- Recognized hash-bound pre-receipt vendor manifests as predecessor ownership
  receipts, enabling one-command cutover of exact old bundles while refusing
  changed bundles and full source trees even with a replacement flag.

### Security

- Event cursors bind the exact preceding history and refuse missing, malformed,
  or divergent timelines instead of continuing against an unrelated authority.
- Recovery exports and setup-managed files share staged, synchronized,
  verified replacement rather than direct truncating writes.
- Current-format import now refuses unstable event ownership, malformed status
  transitions, and missing or invalid terminal timestamps instead of inventing
  chronology at import time.
- Setup now refuses authority rebinding, symlink/path/device-name collisions,
  downgrade of newer receipts, modified retired files, and unowned
  replacements; managed writes are staged and atomically replaced with the
  verified receipt written last.

## [0.5.0] - 2026-08-08

### Added

- Release-first `setup-project` installation with dry-run output, explicit
  managed-file replacement, additive ignore policy, and preservation of
  repository-owned agent guidance and existing authorities.
- A project-root-bound shell launcher for the
  durable planning surface.
- `papertiger-mise --project-root` for episodic external campaign operation and
  a bounded, read-only `status` orientation command.
- Version-aligned Windows, Linux, Intel macOS, and Apple Silicon release
  archives containing both binaries, documentation, manifests, licenses, and
  adjacent SHA-256 checksums.
- Public setup, contribution, security, changelog, and release-maintainer
  documentation.

### Changed

- Aligned every workspace crate and binary on release version `0.5.0`.
- Clarified the product boundary: the planner is persistently integrated into
  consuming projects; experimental Mise remains a first-class peer binary used
  only for projects that deliberately request bounded RSI.
- Reduced generic per-commit GitHub automation to a manual release-artifact
  workflow backed by the same local verification commands.

## 0.4.0 - 2026-08-04

### Added

- Lean SQLite planning authority with plans, tasks, dependencies, blockers,
  gates, probe/decision results, event history, deterministic export/import,
  bounded focus projections, and advisory audit.
- Separate Papertiger Mise authority for deterministic and paired campaign
  execution, finite budgets, retained CAS evidence, explicit nomination,
  successor lineage, and planner-safe evidence projection.

### Security

- Fail-closed schema migration, writer admission, evidence validation, frozen
  evaluator identity, and process-lifecycle refusal paths.

[Unreleased]: https://github.com/virtualonno/papertiger/compare/v0.9.0...HEAD
[0.9.0]: https://github.com/virtualonno/papertiger/compare/v0.7.1...v0.9.0
[0.7.1]: https://github.com/virtualonno/papertiger/releases/tag/v0.7.1
[0.7.0]: https://github.com/virtualonno/papertiger/releases/tag/v0.7.0
[0.5.0]: https://github.com/virtualonno/papertiger/releases/tag/v0.5.0
