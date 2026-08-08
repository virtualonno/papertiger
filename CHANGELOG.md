# Changelog

All notable user-visible changes are documented here. Papertiger follows
[Semantic Versioning](https://semver.org/) and this file follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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

[Unreleased]: https://github.com/virtualonno/papertiger/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/virtualonno/papertiger/releases/tag/v0.5.0
