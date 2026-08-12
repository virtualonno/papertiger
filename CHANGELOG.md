# Changelog

All notable user-visible changes are documented here. Papertiger follows
[Semantic Versioning](https://semver.org/) and this file follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
  API, and transfer. Current schema v6 accepts only the
  `papertiger.dump.v6` contract; restoring an older dump requires its matching
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

[Unreleased]: https://github.com/virtualonno/papertiger/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/virtualonno/papertiger/releases/tag/v0.7.0
[0.5.0]: https://github.com/virtualonno/papertiger/releases/tag/v0.5.0
