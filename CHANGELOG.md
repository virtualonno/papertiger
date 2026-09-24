# Changelog

All notable user-visible changes are documented here. Papertiger follows
[Semantic Versioning](https://semver.org/) and this file follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed

- `add --dep`, `dep add` and `edit --parent` refuse an edge that would leave
  tasks waiting on each other forever: a dependency on the task's own parent,
  on an ancestor of that parent, or on any task that depends on one of them,
  and a new parent whose completion the task already waits for. Previously
  these were accepted, and neither the child nor the parent could ever start
  or complete until the edge was removed by hand.

## [0.19.0] - 2026-09-23

Planner schema v13 and Mise schema v10 are unchanged; no `init` is needed.
Verify the archive against its published `.sha256`, then upgrade a project by
unpacking the archive over the project root or by running `setup-project` from
the new release binary, and a personal installation with `setup-user`. Scripts
that pass `--replace-managed` or check setup result schemas need updating.

### Added

- `--project-root <root>` selects an existing `<root>/state/papertiger.sqlite`
  when the root has neither a project-install receipt nor a release bundle, so
  a personal installation can reach a project's planning history without a
  committed Papertiger integration. It still refuses when that file is absent,
  and discovery without `--project-root` still uses only receipts and bundles.

### Changed

- Installed skills, the project reference and installed binaries are release
  files: `setup-project` and `setup-user` overwrite them with the release
  version, and `uninstall-project`/`uninstall-user` remove them, whether or not
  they were edited. Keep project-specific guidance in `AGENTS.md` or
  `CLAUDE.md`, which setup never touches.
- Binaries are verified once, at download, against the release's `.sha256`
  file; Papertiger no longer hashes installed binaries when it runs. Project
  discovery, `--project-root`, release bundles and the personal runtime check
  only that the receipt or manifest names the running release (and, for the
  personal runtime, its home), and each refusal names the command that fixes
  it. `setup-project` still replaces an installed binary that differs from the
  release binary running it.
- `setup-project` no longer writes the host-local
  `tools/papertiger/bin/papertiger[.exe].runtime-install.json` receipt, and
  removes one left by an earlier release. Its result no longer has
  `runtime_receipt_path` or `runtime_install`, and `uninstall-project` no
  longer lists that receipt.
- `init --json` names `init`'s plain-text report in its refusal instead of the
  generic "no JSON projection" message.
- The project reference installed as `tools/papertiger/agent_integration.md`
  teaches planning use only; installation, upgrade and removal are in the
  README and `--help`.
- Project receipts are `papertiger.project_install.v3` and personal receipts
  `papertiger.user_install.v2`; neither records file or binary hashes. The next
  `setup-project` or `setup-user` rewrites a v2 project or v1 personal receipt.
  An older receipt is refused: move it aside and rerun setup to reinstall.
- `setup-project` reports `papertiger.project_install_result.v7`,
  `uninstall-project` reports `papertiger.project_uninstall.v3` and
  `setup-user` reports `papertiger.user_setup.v2`; update scripts that check
  these schemas. The uninstall result's only refusal action is
  `non_file_refusal`, for a symlink or non-regular file at an owned path; it
  replaces `modified_refusal`.
- Mise refuses an unrecognized schema or protocol id by naming the id it
  expects. `papertiger_mise::schema_ids` is no longer public.
- Mise command messages consistently say "paired execution" (for example
  `paired recover` on an unknown id), and refusals raised by the authority's
  own integrity checks no longer end with a raw SQLite error code.

### Removed

- `--replace-managed` on `setup-project` and `setup-user`.
- Cleanup of files from pre-receipt project installations and release
  bundles with a `papertiger.release_manifest.v1` manifest; reinstall such a
  project with `setup-project` from this release.

### Fixed

- `setup-project` no longer reports a successful upgrade that leaves the project
  unusable. When `tools/papertiger/manifest.json` from an unpacked release
  archive names another release or cannot be read, `setup-project` and
  `--dry-run` now refuse before writing anything. Before this fix, setup exited
  0 and every later command refused the manifest. To upgrade such a project,
  unpack the new release archive over the project root, or move the manifest
  aside and rerun `setup-project`. A custom authority path stays selected
  either way.

## [0.18.0] - 2026-09-23

This release renames planner storage, commands, flags and JSON identifiers
with no compatibility aliases. To upgrade an authority, run
`papertiger --db <authority> backup --output <new-path>` with the new binary,
then `papertiger init` to migrate it from schema v12 to v13. Until then
commands that open the authority, other than `init` and `backup`, refuse it
and name that command. A Mise authority likewise needs
`papertiger-mise --db <database> init` (schema v10). Then update scripts for the renames below and rerun `setup-project` from the
verified release binary so the project skill and reference match.

### Changed

- Release archives carry `papertiger.release_manifest.v3` (was
  `papertiger.release-manifest.v2`); update scripts that check the manifest
  schema.
- Blockers record a `condition`: `blocker add <task> <name> --condition <text>`
  replaces `--reason`, and JSON, dumps and the database column use `condition`.
- Gates use the blockers' vocabulary: `gate resolve` replaces `gate close`, and
  status `resolved` with `resolved_at` replaces `closed` and `closed_at`. The
  migration converts stored gates. Stored history keeps its original event
  kinds, so gate resolutions recorded before the upgrade still read `closed` in
  `log`.
- Exports are `papertiger.dump.v10`. `import` also accepts
  `papertiger.dump.v9` and converts it; older dumps still need their matching
  release to migrate and re-export.
- A raw SQLite writer now fails with `papertiger_write_requires_public_api`
  (was `papertiger_write_requires_executable`). The migration reinstalls every
  guard.
- `list --all-plans` pages with one opaque `--after-cursor` taken from the
  previous page's `next_cursor` or `continuation_command`; `--after-seq` and
  `--snapshot` are removed. A cursor from different `--status`/`--tag` filters
  or from a changed authority is refused with the restart command.
- Renamed flags and commands: `focus --include-blocked` (was `--all`),
  `evidence verify --classification` (was `--outcome`), and
  `papertiger mise record` (was `papertiger mise project`).
- `search --compact` and `show --no-history` print JSON without `--json`;
  both are JSON-only projections and previously refused the command.
- `search --compact` returns 5 results by default (full `search` keeps 20) and
  includes a `continuation_command` when more results match.
- JSON ids are snake_case, with their version raised where the id or shape
  changed: `papertiger.task_context.v8`, `papertiger.task_current.v3`,
  `papertiger.task_inventory.v2` (`next_cursor` and `continuation_command`
  replace `snapshot` and `next_after_seq`), `papertiger.search_compact.v2`,
  `papertiger.evidence_verification.v3` (`projection.classification` replaces
  `projection.outcome`), `papertiger.history_inspection.v2`,
  `papertiger.history_quarantine.v2`, `papertiger.mise_planner_projection.v2`
  and `papertiger.project_install_result.v6` (was `papertiger.project_setup.v5`).
  Quarantine envelopes and Mise projections recorded before this release keep
  their original ids and stay readable, exportable and importable;
  `papertiger mise record` refuses a new projection with the retired id and
  names the command that regenerates it.
- Help text names task arguments "Task number (N or #N)", no longer claims that
  `reject` prevents re-litigation (`reopen` accepts rejected tasks), states
  that `export` carries events and Mise projections, and documents the
  `reference`, `history` and personal setup arguments.
- The project skill and reference are shorter. `commit add` omits `--repo`
  unless the commit belongs to a nested or external repository.

- Mise commands are renamed without aliases; the old spellings are
  refused. `paired run-next`, `show-run` and `list-runs` become
  `execute-next`, `show-execution` and `list-executions`; `improvement verify
  <FILE>` becomes `verify-registry`; `improvement brief-verify` becomes
  `verify-brief`; `promotion inspect` becomes `promotion rederive`;
  `projection inspect` becomes `projection export`. Update scripts and
  agent instructions that call them.
- Mise rationale flags are `--why <TEXT>` or `--why-file <PATH|->` (stdin
  with `-`) instead of `--reason` on `budget release`, `candidate
  abandon-materialization`, `trial cancel`, `trial abandon` and `paired
  cancel`. Cancellation requests report the rationale as `why`, and their
  JSON `target` for a paired execution is `paired_execution`.
- Mise authority schema v10 renames the recorded cancellation rationale to
  `why` and the recorded Papertiger gate time to `papertiger_gate_resolved_at`;
  promotion proofs and successor admissions report it as `resolved_at`.
  Run `papertiger-mise --db <database> init` once after upgrading;
  read commands refuse a v9 authority and name that command.
- Every Mise schema, protocol and domain-separation id is
  `papertiger-mise.<snake_case_name>.v<N>` with its version advanced, and
  the improvement formats move from the `papertiger.` prefix to
  `papertiger-mise.`. Operator-authored inputs must be reissued under the
  new ids: campaign manifests `papertiger-mise.campaign.v4` (measurement
  contracts `measurement_contract.v2`), adapter bindings
  `paired_adapter_binding.v2` and `domain_shadow_adapter_binding.v2`, fixture
  bundles `fixture_bundle.v2` (regenerate with `campaign fixture-bundle`),
  briefs `project_improvement_brief.v3`, approvals
  `project_improvement_brief_approval.v2` and registries
  `improvement_paradigm_registry.v2`. Deterministic evaluators must read
  `deterministic_evaluator_request.v3` and emit
  `deterministic_evaluator_output.v3`; paired adapters receive
  `paired_trial_request.v4`. Rerun `campaign source-binding`, because the
  repository identity digest changed with its domain-separation id. The
  complete retired-to-current table is
  `papertiger_mise::schema_ids::RETIRED_SCHEMA_IDS`.
- Mise containment policies are `ContainmentPolicy` with schema
  `papertiger-mise.containment_policy.v3` and protocol
  `papertiger-mise.ed25519_sealed.v3`, replacing `TrustedContainmentPolicy`
  and `papertiger-mise.trusted-containment-policy.v2`. Reissue existing policy
  files; a retired policy is refused with that instruction. Promotion proofs
  and sealed attestations name the policy digest `containment_policy_sha256`.
- Mise refuses campaigns, receipts and shadow evidence recorded before this
  release instead of reading them. Each refusal names the replacement id; reopen
  frozen evidence with a 0.17.x `papertiger-mise`, or admit a new campaign.
- Mise `--papertiger-db` defaults to `state/papertiger.sqlite`, resolved from
  `--project-root`, for `campaign admit-successor`, `promotion verify-parent`
  and `promotion verify`. `promotion verify` previously required it.
- Mise `promotion derive` and `promotion verify` help states that both always
  refuse until sealed confirmation attestation lands.
- Planner Mise projections require candidate material
  `papertiger-mise.candidate_material.v2`; export them with this release's
  `papertiger-mise projection export`.

### Removed

- `inspect-project-guidance` and the `project_guidance` field of the setup
  result. Papertiger no longer recommends trigger text for a project's
  `AGENTS.md` or `CLAUDE.md`; installed skills route agents by their own
  descriptions. Delete any Papertiger trigger text you added for it.

### Fixed

- A trigger added by direct SQLite access is write-guard drift. Previously it
  passed `audit` and `repair-guards --dry-run`, then ran inside the next
  Papertiger mutation and could change planning data without an event. Writes
  now refuse and name it; `repair-guards --why <reason>` removes it and records
  its SQL. The Mise projection immutability triggers are also checked and
  restored. An authority that already contains such a trigger refuses writes
  after upgrading until `repair-guards` runs; reads, export and backup continue.
- `repair-guards --json` reports each entry's `state` as `missing`, `altered`
  or `foreign`.

## [0.17.1] - 2026-09-22

No schema change; 0.17.0 authorities need no migration.

### Fixed

- Missing or altered write guards no longer lock an authority. Reads, export
  and backup continue, `audit` reports the drift, and writes refuse with the
  repair command. `repair-guards --dry-run` previews the drift and
  `repair-guards --why <reason>` reinstalls the guards and the canonical history
  view, recording what it found in an audited event. It cannot write planning
  data. Previously a single dropped trigger blocked status, export and `init`
  with no supported recovery.
- The README and operating reference state the current schema as v12.

## [0.17.0] - 2026-09-22

Upgrading migrates planner schema v10 or v11 to v12. Read commands refuse the older
schema and name the `init` command to run; take a `backup` first.

### Changed

- Every planner table refuses writes from ordinary SQLite connections; use the
  executable or enter through the public mutation API. Stored events are
  append-only, and normal opens refuse missing or altered guards. This protects
  against raw-SQL misuse, not a filesystem owner deliberately defeating the
  guards. Public API connections are trusted after mutation entry.

### Added

- `history inspect <event-id>` and `history quarantine <event-id>
  --expect-sha256 <digest> --why <reason>` recover structurally invalid legacy
  history after explicit review. Original rows remain unchanged; an audited
  recovery event preserves every raw field through export/import. Unknown
  timestamps and associations remain unknown, and task state is not inferred.

### Fixed

- `audit` identifies every malformed stable event reference and reports
  oversized titles on terminal tasks, which can also block recovery imports.
- Task reads identify priorities stored as text and name the recovery command.
  After preserving a backup, `edit <task> --priority <integer> --why <reason>`
  can repair that value as an isolated edit, preserving the original text in a
  `repair_priority` event. Reads never infer a numeric default, and other
  unreadable task fields roll back the repair.
- Project skill guidance no longer points to an absent personal command binding.
  First-use diagnostics preserve the selected authority instead of instructing
  project users to guess a database path.

## [0.16.0] - 2026-09-17

The release archives were replaced with ready-to-use project overlays. Download them again and verify the new checksums. Merge `.agents`, `.claude`, and `tools` into the project root; executables and documentation have moved under `tools/papertiger`. No setup command or project guidance rewrite is needed. Existing configuration and planning data are not shipped or overwritten.

### Added

- `setup-user` installs a personal skill, native runtime and private fallback planner without changing consuming projects, `AGENTS.md`, shell profiles or harness settings. The installed runtime discovers existing project authorities before using its private fallback, without a `--db` argument. Start a fresh agent session after installation; skill discovery does not guarantee model selection.
- `uninstall-user` removes matching receipt-owned runtime and skills while retaining planner history. Setup and removal support `--dry-run`; unowned or modified files require review and explicit replacement. Missing expected history refuses setup. Upgrade or repair from an external release binary.

### Changed

- Every installed skill location contains the complete short workflow generated from one template. Existing owned Claude routers upgrade in place, removing the extra read needed to reach executable bindings and operational guidance.
- Direct project overlays are the default adoption path; personal installation remains optional. `setup-project` remains available for shared repository adoption and pinned runtimes; project guidance triggers are optional with skill-capable harnesses. Cursor project markers select the shared skill in automatic setup.
- The planning skill carries the ordinary workflow and loads installation, migration and less common operations from its reference only when needed.

## [0.15.0] - 2026-09-17

### Added

- `--session <id>` and `PAPERTIGER_SESSION` record advisory pickup when starting
  or resuming work. `focus` prefers the caller's work and available alternatives
  over tasks last picked up by another session. `show`, `focus`, and `status`
  expose pickup identity and time without claiming that the agent is still alive.

### Changed

- `focus` is concise by default and preserves pickup, readiness, and blockers
  together. JSON v7 replaces full task records with summaries, moves pickup to
  `entries[].pickup`, and reduces `plan` to slug and status. Use `show` for intent,
  results, and history.
- `start` can resume in-progress work without a release or takeover command.
  Repeated pickup by the same identified session emits no event. Pickup never
  blocks another session or guarantees exclusive execution; real dependencies,
  blockers, gates, and completion checks remain enforced.
- Upgrading from 0.14.0 migrates planner schema v9 to v10 for pickup context.
  Preserve an export and standalone
  backup before explicitly running `init`; migration does not infer sessions
  from historical actors. Recovery dumps use `papertiger.dump.v9`. Changed read
  contracts are task context v7, current task v2, status v3, focus v7, and search v2.
  Refresh consuming installations with `setup-project`; keep old Mise drivers
  for campaigns that bind their executable identity.
- Rust callers pass optional session context to `start_task`, `focus`, and
  `TaskCreation`; pass `None` when no session identity is available.
- JSON-schema verification runs in the Rust workspace tests without Python.

### Removed

- Auxiliary Python scripts. The historical native-value comparison document
  points to its archived runner and retains the original evidence and limitations.

## [0.14.0] - 2026-09-16

Planner schema remains v9; upgrading from 0.13.0 needs no database migration. Run `setup-project` from the new release to refresh project binaries and skills. Keep the previous Mise driver for existing campaigns that bind its executable hash.

### Added

- `list --all-plans --status unfinished --json` inventories open tasks across every plan state, including paused plans, with owning plan identity, exact totals and bounded continuation. Reuse the returned snapshot and unchanged filters for subsequent pages; changed authority history refuses continuation and requires restarting the inventory.
- `plan list --json` exposes structured plan orientation; `--plan SLUG` selects one plan.
- `search --compact --json` returns ranked task summaries and excerpts without full bodies. `show --no-history --json` returns full current context with an explicit history command. These opt-in projections use `papertiger.search_compact.v1` and `papertiger.task_current.v1`; existing full JSON reads retain their schemas.

### Changed

- The installed skill uses compact discovery and current-state rechecks where appropriate while retaining full context for resumption. Mutation guidance distinguishes optional task summaries from full task records and uses read-only recovery after a postcommit display failure.

### Fixed

- `inspect-project-guidance` recognizes explicit `@AGENTS.md` imports and directed local Markdown links in `CLAUDE.md` as indirection. This observation does not claim that imported instructions were loaded or followed.

## [0.13.0] - 2026-09-09

Planner schema remains v9; this update needs no database migration. Retain the
previous Mise driver for existing campaigns whose manifests bind its exact
executable hash.

### Added

- Planner mutations accept `--reasoning-effort` or `PAPERTIGER_REASONING_EFFORT` alongside a known model. Event history and task activity preserve the reported effort separately; existing events retain null effort without a database migration.
- Mise `guide` supplies the running driver's concise agent workflow; `guide --reference` emits its full operating reference without opening an authority.
- Mise `campaign inspect` discovers candidates, trials, paired cohorts, and reservations through bounded read-only pages. Continuation arguments preserve project and database selection. Recorded state remains separate from CAS verification and execution readiness.

### Changed

- The vendored Papertiger skill contains the ordinary task workflow and loads installation, recovery, and advanced-operation detail only when needed. Existing durable work remains tracked when its next step is a bounded edit or read-only check.
- Agents are instructed to resolve the exact model variant and known reasoning effort once per execution context, then reuse those values until settings change.
- Claude skill installation uses a thin router to the canonical `.agents/skills/papertiger/SKILL.md`. Selecting Claude also installs that dependency; receipt-owned full copies upgrade automatically.

### Fixed

- Task search results report the owning plan slug in `plan` instead of the task status, including searches across multiple plans.

## [0.12.1] - 2026-09-05

### Fixed

- The top-level `reference` help describes task reference locators instead of
  repeating the `commit` command description.

## [0.12.0] - 2026-09-05

### Added

- `backup --output <new-path> --json` creates a standalone SQLite recovery copy
  with a digest receipt, preserving committed WAL data and planner schemas 1–9
  without migration. It refuses existing destinations and foreign authorities.
  Historical records are preserved without semantic validation, including legacy
  evidence that JSON import refuses.
- Planner mutations accept `--json`, returning committed events and task/plan
  snapshots in `papertiger.mutation.v1` so clients can create dependent tasks
  without parsing prose. Concurrent writers cannot enter another command's receipt.
- Optional `--model` or `PAPERTIGER_MODEL` records caller-reported event authorship
  separately from actor and meaning source. Creation, completion, and disposition
  history expose it; unknown historical models remain unset.
- `move-plan <tasks> --plan <destination> --why <reason>` atomically moves an
  explicit related set while preserving identity, obligations, and original event
  plans. Partial selections refuse with the missing task selectors.
- `reference add|remove|list|find` records exact inward artifact locators with
  optional hashes and notes. References do not fetch external state or satisfy gates.
- `schema` prints the bundled JSON Schema for planner context, status, task lists,
  history, recovery dumps, and mutation receipts without opening an authority.
- Mise `trial cancel` and `paired cancel` accept `--reason` to request cancellation
  of launched work. The live supervisor stops the process and records failure;
  the request alone does not prove cleanup. Cancelled work cannot qualify and its
  bound reservation is charged. A dead supervisor still requires `recover`.
- Mise `budget release <campaign> <reservation> --reason <reason>` releases an
  unused reservation at zero cost. It refuses reservations already bound to
  lifecycle work, and released reservation identifiers cannot be reused.
- Mise measurement contracts bind declared process, workload, phase, units,
  sampling and cost category to each retained observation and its selected inputs.
  Compiler or test-harness resource cost cannot satisfy a product-runtime contract.
  Hard resource thresholds must pass their no-op control and reject their known-bad
  control. The collector remains trusted; matching metadata cannot attest an
  untrusted host or fabricated measurements.

### Changed

- Planner schema v9 requires explicit `init` after archiving an export with the
  previous release and taking a SQLite `backup` with the new release. Current
  portable recovery uses `papertiger.dump.v8`; restore older
  dumps with their matching release, migrate the temporary authority, and re-export.
  Plan-scoped exports retain moved tasks' original history and required former-plan
  definitions, and refuse missing task identities instead of omitting history.
  Older planner binaries refuse the new authority.
- `show --json` emits `papertiger.task_context.v6`, with full selected-task details
  and compact related-task summaries. Read each related task explicitly for its
  intent and result. The record remains local planning data requiring editorial
  review before publication.
- Mise cancellation requires authority schema v9. Run
  `papertiger-mise --db <database> init` explicitly to migrate; older binaries
  refuse the new authority.
- Mise `status --json` emits `papertiger-mise.project-status.v2`. A custom
  `--db` requires `status --objects <object-root>` because the authority does not
  bind its object-store location. The status reports directory presence only;
  it does not verify the stored evidence.
- New Mise campaigns require `papertiger-mise.campaign.v2` with measurement
  contracts on every objective. Update adapters for deterministic request/output
  v2 and trial receipt v4, or paired request v3. Brief and compiled-draft inputs
  use v2. Historical campaign v1 bytes remain readable with their original
  protocols and no inferred provenance; they cannot be newly admitted or upgraded
  in place. This protocol cutover adds no SQLite migration beyond schema v9.

### Fixed

- Project setup preserves the position of its managed `.gitignore` block when
  later rules cannot unignore protected files, avoiding changes to adjacent tool
  integrations. Later negations still move the protection block after them.
- Mise candidate authoring uses portable Git paths when creating worktrees,
  including repositories in Windows paths with spaces.
- Mise Git mutation diagnostics go to stderr so a successful materialization
  returns a complete JSON receipt that clients can parse directly.
- Deterministic Mise repetitions compare validated objective values while
  retaining each trial's distinct process and environment evidence. Matching
  values from separately launched processes no longer cause false disagreement;
  changed values or mismatched measurement bindings still refuse adjudication.
- Mise successor admission preserves canonical ordering when a parent proof
  includes multiple build receipts, allowing exact replay after reopening CAS.

## [0.11.0] - 2026-08-30

This release establishes the planner, receipt-bound project integration, evidence verification,
and Mise lifecycle as the next experimental minor line. It incorporates the source changes
prepared under the unpublished 0.10.1 version; no 0.10.1 tag or release artifacts were published.

### Changed

- Release source verification and native artifact builds now run concurrently while publication
  still requires both. The source job owns the complete workspace suite and strict Clippy once;
  native matrix tests are limited to distinct Windows and macOS process behavior, while every
  architecture retains its exact build and packaged setup, authority, Mise, and uninstall lifecycle.
- Release jobs use exact-action, target-keyed Rust caches and Node 24 artifact actions, with
  digest mismatches rejected during publication. The source gate runs the default workspace suite
  once, and its local lifecycle reuses debug binaries instead of recompiling release binaries
  already owned by the native artifact matrix.
- The 1,555-line deterministic Mise campaign remains an explicit gate for Mise lifecycle changes,
  but unrelated workspace and release checks no longer execute its full on-disk campaign.

## [0.10.0] - 2026-08-28

### Added

- `setup-project --json` reports the installed native binary's exact path,
  byte count, and SHA-256 in `papertiger.project_setup.v5`. The same identity is
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

[Unreleased]: https://github.com/virtualonno/papertiger/compare/v0.19.0...HEAD
[0.19.0]: https://github.com/virtualonno/papertiger/compare/v0.18.0...v0.19.0
[0.18.0]: https://github.com/virtualonno/papertiger/compare/v0.17.1...v0.18.0
[0.17.1]: https://github.com/virtualonno/papertiger/compare/v0.17.0...v0.17.1
[0.17.0]: https://github.com/virtualonno/papertiger/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/virtualonno/papertiger/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/virtualonno/papertiger/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/virtualonno/papertiger/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/virtualonno/papertiger/compare/v0.12.1...v0.13.0
[0.12.1]: https://github.com/virtualonno/papertiger/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/virtualonno/papertiger/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/virtualonno/papertiger/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/virtualonno/papertiger/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/virtualonno/papertiger/compare/v0.7.1...v0.9.0
[0.7.1]: https://github.com/virtualonno/papertiger/releases/tag/v0.7.1
[0.7.0]: https://github.com/virtualonno/papertiger/releases/tag/v0.7.0
[0.5.0]: https://github.com/virtualonno/papertiger/releases/tag/v0.5.0
