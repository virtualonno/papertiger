# Papertiger — Agent Instructions

Local task planning (`papertiger`) plus the opt-in candidate-evaluation
campaign runtime (`papertiger-mise`). Rust + SQLite, no server. Two binaries,
two separate SQLite authorities, one workspace.

## How To Use This File

This file is the always-loaded contract. Load operational detail only when
needed:

- **Planner usage**: `agent_integration.md` (the one-page vendored
  contract).
- **Mise campaigns**: `MISE.md` (generational model, evidence contract,
  promotion boundary). Ordinary planning work never needs it.
- **Live truth**: `papertiger status` / `focus` / `show <N> --json`, never
  markdown. This repository's plan is `state/papertiger.sqlite`.

**Do not grow this file with operational detail.** New commands, flags, and
contracts go to the document that owns them; this file gets at most a one-line
trip-wire plus pointer. Keep `AGENTS.md` and `CLAUDE.md` byte-identical and
each under 32 KiB.

## Hard Rules

- **Never run `git clean` in this repository.** `state/` is Git-ignored:
  `-x`/`-X` irrecoverably delete the planning database, the Mise database, and
  the content-addressed evidence store. Delete explicit paths with `rm -f`;
  never sweep.
- `state/papertiger.sqlite`, `state/papertiger-mise.sqlite`, and
  `state/papertiger-mise-objects/` are mutated **only** through their
  binaries' commands and public APIs. Direct SQLite or CAS writes are outside
  the trust boundary — they can mint evidence the runtime would refuse. If a
  database is missing or schema-empty where work clearly existed, stop and ask
  the operator; never initialize a fresh authority in its place.
- `papertiger init` is the only migration path. Read commands refuse older
  schemas with the exact command to run; run it deliberately, never as a
  reflex inside a script.
- Set `PAPERTIGER_ACTOR` to a concise author label before mutating;
  unattributed events make cold history unreadable. It is event provenance,
  never task ownership or session liveness.
- One canonical planning worktree owns the DB. Never initialize or mutate a
  forked copy from another worktree; `export` is transfer/recovery, not a
  second authority.

## Planning Use in This Repository

- Enter from live truth: `status`, `focus`, `show <N> --json`. Roadmap prose,
  handoff notes, and memories are orientation only — if they disagree with the
  DB, the DB wins and the prose gets corrected.
- `--why` on anything a future session might question, written for a reader
  with zero context. Probe/decision tasks require `--result` before `done`.
- `done` refuses open deps, blockers, gates, children. Close or waive with
  reasons; never route around a refusal — the refusal is the product working.
- Do not duplicate task status into markdown, and do not create tasks for
  same-session checklist steps. Markdown carries doctrine, zero status.
- Check `list --status rejected` before proposing a revival; read its `--why`.
- Treat `task.seq` as authority-local: never put Papertiger task numbers in
  shared Git/PR/changelog prose. Record optional commit associations inward.
- When validated deferred work, proof debt, or tooling friction should survive
  this session, use the Papertiger contract proactively; do not create
  same-session checklist tasks or interrupt authorized work unnecessarily.

## Coding Contract

- Toolchain is pinned by `rust-toolchain.toml`. Gates before claiming done:
  `cargo fmt --check`, `cargo clippy --workspace --all-targets` (warnings are
  defects), `cargo test --workspace`, and for Mise changes
  `cargo test -p papertiger-mise --test deterministic_dogfood -- --test-threads=1`.
- **Fail-closed is the house style.** No error swallowing, no optimistic
  defaults, no converting an integrity failure into a score or a skipped
  check. A refusal path is a feature with tests, not dead weight.
- **Doc–code honesty rule.** `MISE.md` and `README.md` state capabilities in
  the present tense only when the code enforces them and a test proves it.
  Aspirational behavior is written as an explicit gap ("until X lands").
  When review finds an overclaim, fixing the sentence is part of the fix.
- Naming: live vocabulary is runtime, scheduler, executor, supervisor,
  adapter, classifier, authority. No metaphor placeholders; one concept has
  one canonical name across CLI, schema, code, and docs; renames are full
  cutovers with no compatibility aliases.
- Prefer cohesive owned subsystems over monolith growth. Keep Git
  materialization in `git_materialization.rs`, path identity in
  `path_identity.rs`, and lifecycle fixtures in `lifecycle_tests.rs`; do not
  recombine them. Split new behavior where it has a distinct authority or
  invariant boundary, not merely to move lines. Never add a second copy of an
  existing helper (`sha256`, `validate_sha256`, bounded capture) — hoist to a
  shared module instead.
- Every public refusal message names the corrective command or the exact
  missing input. That standard already exists in the codebase; match it.

## Mise Boundary (trip-wires)

- A nomination is evidence, never promotion. Nothing in this repo may close a
  Papertiger gate, advance a branch, or "deploy" from a Mise result; promotion
  requires the separate operator-owned proof path, which is deliberately
  fail-closed today.
- Shadow evidence (historical or domain) is permanently decision-ineligible.
  Never present it as qualification, and never weaken a
  `decision_eligible=false` invariant to make a demo work.
- A consuming project's campaign state lives in that project's tree, not in
  this repository's `state/`.
- Campaign IDs follow MISE.md's subject-objective-attempt convention; never
  encode Papertiger task numbers, containment, disposition, or inferred generation.
- The campaign manifest freezes its outer judge (the driver binary's own path
  and hash). Rebuilding a driver invalidates its campaign by design; plan
  campaign runs after the binary is final, not before.

## Verification

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets
    cargo test --workspace
    cargo test -p papertiger-mise --test deterministic_dogfood -- --test-threads=1
    papertiger audit
