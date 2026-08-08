# Papertiger Mise

Papertiger Mise is Papertiger's project-generic framework for recursively
verified agent improvement. Its practical mission is to make sustained
autonomous engineering more effective without confusing repeated
experimentation with trustworthy improvement. It treats source changes,
proposal policies, adapters, evaluators, and future Mise runtimes as versioned
candidates in finite, evidence-producing campaigns. Candidate proposal is
currently external: an agent or adapter supplies each immutable proposal, Mise
judges it, and a separate operator path owns promotion. Mise is not yet an
autonomous proposal policy or a self-authorizing deployment loop.

The name refers to *mise en abyme*: an improvement process can itself become an
object of improvement. This recursion is deliberately generational rather than
in-place. A running campaign never rewrites the runtime, policy, evaluator,
budget, stop rules, or promotion authority judging that same campaign.

## Frozen outer judge

Every campaign binds an exact repository identity, base commit and tree,
mutation scope, evaluator and fixture hashes, objective contract, cumulative
budget envelope, containment grade, holdout policy, stop rules, proposal policy,
runtime generation, and parent lineage. These bindings are immutable after
admission. The outer judge binding is the canonical host executable path and
SHA-256 that runs the lifecycle API, not merely a digest of source that some
different binary claims to implement. Admission may inspect that external host
artifact, but evaluation must run inside that exact binary.

A runtime or policy proposed as generation N+1 is evaluated by an already
frozen generation N outer judge. It can become eligible only for a successor
campaign. Descendant campaigns debit their parent's cumulative budgets and may
not exceed the declared recursion depth. Root campaigns are generation 1 at
depth 0, and the declared maximum depth cannot exceed the implementation's
finite bound of 32. There is always a finite operator-owned root of trust; Mise
does not disguise that root as limitless self-reference.
Successor admission is an explicit two-authority cutover. Mise first reopens the
parent nomination and every relied-upon CAS object, binds the promoted candidate
materialization tree to the successor's exact clean source tree, and binds the
complete successor manifest including its N+1 judge executable. It preserves a
`development_successor_admission_only` parent-promotion proof for independent
review. Admission then requires a separately closed Papertiger gate naming that
exact proof and, in one Mise transaction, conservatively charges the child's
entire budget envelope to the parent before inserting the child and immutable
lineage receipt. A failed proof, stale gate, wrong tree, depth drift, or
insufficient parent balance leaves both child and ledger unchanged. This
lineage promotion is not deployment authority and cannot substitute for sealed
production promotion.

New development successor proofs require parent candidate trials with a frozen
`evaluator.judge_build` contract. Each such trial receives a previously absent,
runtime-owned output root. The frozen evaluator reports the exact build argv
and executable locator; the supervisor requires those values to match the
manifest, confines the output to that fresh root, accepts only a plain regular
file within its byte ceiling, and preserves the executable in CAS. The v3 trial
receipt binds that object to the promoted source tree, exact evaluator
environment, toolchain executable and version, and build recipe. A v2 parent
proof names every relied-upon candidate build-trial receipt and requires the
successor judge SHA-256 to equal their one parent-produced executable identity.

This is a build receipt, not sealed compiler attestation or a universal
reproducible-build proof. It relies on the already-frozen evaluator to attest
that it executed the declared argv, and on the operator-trusted host, compiler,
filesystem, and CAS boundary. The supervisor proves that the exact retained
bytes appeared in an empty trial-owned output root; it does not trace every
compiler syscall. Historical admitted v1 parent proofs remain reopenable, but
they do not authorize another new descendant without a build-bound parent
campaign.

## Campaign identity and naming

Papertiger and Mise name different authorities. A Papertiger plan slug and
`#N` identify mutable planning position; a Mise `campaign_id` identifies one
immutable admitted evidence scope. A task may require several campaign
attempts, and one campaign may inform several planning tasks, so neither ID is
embedded in the other. Papertiger links the boundary by recording the exact
nomination or promotion-proof receipt on its independent gate.

New campaign IDs use `<subject>-<objective>-aNN`: a stable consuming project or
domain, a concise research objective, and a zero-padded admission-attempt
ordinal. For example, `contextmink-clippy-idioms-a05` and
`papertiger-mise-lifecycle-split-a01` say what is being judged without claiming
an outcome. Increment `aNN` only when an already-admitted immutable manifest
must be replaced; candidate retries and trial repetitions retain their own
lineage inside the same campaign. This is operator naming policy, not campaign
schema grammar: admission continues to accept any portable safe identifier so
foreign domains do not have to adopt Papertiger's vocabulary.

Run `campaign preflight` before the first admission attempt. It checks the
manifest contract, exact source and control repositories, tracked artifacts,
fixtures, and frozen build environment in one DB-independent pass and reports
all defects whose prerequisites remain independently checkable as JSON. A
failed prerequisite may skip its dependent check family rather than infer
additional defects from missing evidence. It neither opens nor creates a Mise
database or object store. Repairs to a manifest that has never been
admitted retain the same `aNN` identity and use the consuming project's one
Mise authority. Admission repeats the same preflight immediately before the
transaction, so a prior successful report is never a reusable capability.

Do not encode a Papertiger task number, containment backend or grade,
qualification result, runtime generation, or recursion depth in the campaign
ID. Those facts already have typed authorities and can change independently of
the human label. In particular, `vN` remains schema/protocol/release vocabulary,
not an experiment-attempt counter, and `gN` must not substitute for the
manifest's generation binding. Historical IDs remain immutable and receive no
aliases.

`trial_id` is globally unique within one Mise SQLite authority. When an
authority contains several campaigns, use `<campaign-id>-<role>-NN` rather
than relying on a campaign-local `no-op-1`. Candidate and nomination IDs remain
content-derived hashes. Keep a series that may form parent/child lineage in one
consuming-project Mise database and CAS; campaign-named workspace directories
are disposable ergonomics, not separate evidence authorities.

Mise requires a no-op calibration to characterize evaluator noise and a
known-bad calibration to prove that the evaluator rejects a controlled
regression.
No candidate is accepted merely because one scalar observation improved.
Objectives retain their typed roles, directions, practical thresholds, and
regression tolerances. Hard constraints and protected objectives remain visible
alongside primary optimization objectives. A primary objective must be
quantitative and declare a nonzero `minimum_practical_change`; boolean outcomes
belong only in hard constraints. This prevents a development campaign from
qualifying on feature presence, command success, or another self-certifying
yes/no proxy.

A targeted structural campaign may use an exact target-module measurement as
its primary objective, but it must retain the repository-wide largest-module
measurement as a no-regression hard constraint. Using only the repository-wide
maximum can hide a real target improvement behind an unrelated module-size
floor; using only the target can reward moving complexity elsewhere.

## Noisy paired evidence

Mise's noisy v1 method is `fixed_sample_exact_paired_binomial`. One statistical
block is a complete adjacent baseline/candidate replay pair, never an
individual frame. Each block binds its fixture, workload seed, environment
profile, stratum, candidate identity, and AB/BA order. Every stratum has an even
number of at least two blocks, and its order is exactly balanced. Research
blocks must match the manifest's sealed confirmation fixture binding. No-op and
known-bad observations use separate fixture bindings that must exactly match
their manifest calibration entries.

Research slots, no-op calibration, and known-bad calibration have separate
secret-seed commitments. The manifest also binds the trusted scheduler's seed
generation, retention, and reveal protocol by locator and SHA-256. A reveal
must contain at least 32 bytes and occurs only after the corresponding candidate
identity is frozen. The reveal makes the schedule reproducible without giving
the proposer a reusable future-slot schedule.

Measurements and thresholds are scaled signed integers. Decision arithmetic,
binomial tails, Holm comparisons, and median order statistics use no floating
point. Admission refuses an exact policy whose scaled values do not represent
the campaign's declared objective thresholds and bounds those integers to a
domain where distinct values cannot collapse to the same legacy `f64`
declaration. For a direction-normalized
paired improvement, equality at a practical or non-regression threshold is a
success; equality at a strict regression threshold is not a regression. Every
frozen block remains in the sample. Missing metrics, arithmetic overflow,
duplicate blocks, receipt labels, or schedule drift refuse classification.

Population inference is available only when admission names a population and
binds a sampling protocol under which the paired threshold indicators are
independent Bernoulli observations. Under that contract, an exact one-sided
binomial test asks whether more than half of the population clears each frozen
boundary. Holm controls the hypothesis family inside a candidate analysis, and
a finite Bonferroni allocation controls the campaign family across all admitted
research slots. At least one primary objective must establish a practical win;
every primary and protected objective must establish non-regression; hard
constraints must pass every block. There is no weighted scalar score.

Balanced order does not establish independent sampling or eliminate carryover.
A fixed schedule therefore emits no p-values and can never qualify a candidate;
it can only report schedule-specific measurements or reject an observed hard
failure or regression. Mise does not infer a mean FPS gain, universal
superiority, or safety outside the named objectives, population, hardware,
fixtures, and environment profile.

The current crate exposes the exact classifier and atomically reserves each
research slot against its realized schedule. Historical paired-analysis v1
plans remain readable but are not executable. Paired-analysis v2 freezes an
exact trial adapter executable, argv, working directory, environment, protocol,
and bounds. Before launch, the durable runner derives the complete AB/BA
schedule, writes every measurement-free request to CAS, reserves all trials,
wall time, one cohort-failure unit, and every research disclosure, and commits
the exact run order in SQLite. It then executes only the next prepared run
through the shared portable supervisor. Each success retains the raw adapter
result, exact domain receipt, Mise execution receipt, PID birth identity,
elapsed time, and backend diagnostics before another run can start. Domain
receipt identities are globally unique across live and historical evidence.

Adjudication regenerates the schedule and reopens every request, result, domain
receipt, and Mise receipt from CAS. It never classifies an in-memory adapter
return. A missing, noncanonical, substituted, or mismatched object terminally
marks the complete cohort `integrity_failed` and charges its reservation;
calling adjudication before every run succeeds is an ordinary refusal and does
not destroy the cohort. A launched run can resume only by proving its exact PID
and OS birth identity absent, and that recovery charges the failed cohort
without replaying the process.

The same bounded `executor` runs historical observers. It clears ambient
environment state, accepts one canonical request on stdin, and requires one
canonical typed result with empty stderr. Mise preserves the request, result,
binding, and its own historical receipt in CAS and indexes every domain trial
receipt globally.

Historical evidence enters only through `evidence historical-shadow`. Its
schema structurally requires `decision_eligible=false`, no campaign or candidate
binding, unavailable schedule authority, and prohibited adjudication. Start
times may describe observed order, but can never manufacture a precommitted
schedule. This is replay and cutover evidence, not qualification evidence.

Read-only domain observations use the separate `evidence domain-shadow`
contract. A domain-shadow adapter binds an exact executable, argv, working
directory, environment, result schema, and resource bounds. Its typed result
contains domain-owned authority, context, observation, and exact state objects
before and after execution. Mise recomputes both state-object SHA-256 identities
and accepts the result only when they are equal. The request, raw result, and
Mise receipt are retained in CAS; SQLite independently requires unchanged state,
`decision_eligible=false`, `adjudication=prohibited`, and immutable rows. A
domain shadow has no paired participants, measurements, schedule, candidate,
classification, or nomination semantics.

The deterministic runtime still refuses to execute or adjudicate a manifest
containing a paired-analysis plan; `paired prepare`, `run-next`, `recover`, and
`adjudicate` are the sole live path. WorkspaceOnly paired plans bind their
research blocks to the campaign's disclosed exploration tier. They can produce
a durable local development qualification after reason-coded no-op and
known-bad calibration authorities exist. Qualification never nominates
automatically. `paired derive-nomination` is the sole explicit transition: it
requires the exact no-op, known-bad, and qualified research cohort identities,
reopens every cohort and its candidate/materialization evidence from CAS,
reclassifies them under the admitted plan, and can mint one immutable
`workspace_only_development` nomination. Generic promotion inspection repeats
that derivation. Paired promotion-proof derivation still refuses until sealed,
verdict-only cohort attestations are implemented.

Sealed paired plans bind the confirmation tier, but the local runner explicitly
refuses them. A future sealed path must supply the platform-neutral attested
worker and genuinely verdict-only receipts; local process supervision may not
impersonate that authority. The trial adapter's wall/output bounds also may not
exceed the campaign's portable limits.

The no-op and known-bad assessors require their independently committed cohorts,
but their pure results are not calibration authority by themselves. The paired
adapter must bind those cohorts to the manifest's exact calibration candidate
identities and require the known-bad campaign reason code. Calibration and every
research block consume two trial executions; admission reserves trial capacity
for every cohort and confirmation-disclosure capacity for every research slot.

Noisy v1 refuses partial or extended cohorts and has no optional-stopping
method. A different stopping method requires a new audited method, stop rule,
and evidence schema. Sealed confirmation and genuinely verdict-only disclosure
remain separate promotion requirements.

The statistical design follows Holm's strong family-wise error control and
keeps optional stopping outside v1 until a time-uniform method is implemented:
[Holm (1979)](https://doi.org/10.2307/4615733) and
[Howard et al. (2021)](https://doi.org/10.1214/20-AOS1991).

## Authority split

Papertiger owns plans, tasks, dependencies, blockers, completion gates, and the
separate authorization to materialize nominated work. Mise owns campaign
manifests, immutable candidate lineage, trial reservations and measurements,
cumulative budget accounting, evaluator-integrity outcomes, negative evidence,
and nominations.

Feedback crosses this boundary only as an actor-attributed, idempotent
reference to immutable Mise evidence. A planner projection must bind the exact
campaign and manifest identities, candidate identity and material, terminal
cohort or nomination and receipt identities, disposition, evidence scope,
limitations, and consumed budgets. Replaying the same reference may append no
second event or task. Infrastructure failure projects as evidence for a
bounded diagnostic or replacement-campaign decision; it never authorizes the
planner or controller to repair an admitted manifest in place.

The projection may append evidence or propose follow-up work. It may not write
the Mise authority, integrate candidate source, mark implementation complete,
close a promotion gate, overwrite task intent, or translate nomination into
deployment. Domain adapters retain product truth, Papertiger retains mutable
planning authority, and Mise retains immutable experimental authority.

The live boundary is intentionally two-step and operator-controlled. First,
`papertiger-mise projection inspect --nomination <id> --objects <root>` (or
`--candidate <id>` for terminal non-nominations) opens the Mise authority
read-only, reopens the exact candidate material and relied-upon CAS evidence,
rederives budget balances, and emits
`papertiger.mise-planner-projection.v1`. Then
`papertiger mise project <task> <projection.json>` validates the document again
and records it immutably in the independent planning database. Repeating the
same projection on the same task is a no-op; binding the candidate to another
task or changing its projected payload is refused. `papertiger mise list
<task>`, `papertiger mise show <projection-sha256>`, and `papertiger show
<task> --json` revalidate stored payloads on read. Projection records survive
`papertiger.dump.v3` export/import. None of these commands changes task, gate,
or candidate state.

The projection argument may be `-`, so a shell may use a no-scratch-file path:

```text
papertiger-mise projection inspect --nomination <id> --objects <root> |
  papertiger mise project <task> -
```

On shells whose native pipeline does not preserve large JSON arguments, use
`papertiger-mise projection inspect ... --output <new-file>` followed by
`papertiger mise project <task> <file>`. The inspector refuses to overwrite an
existing output.

Generic improvement guidance is separately content-addressed by
`papertiger.improvement-paradigm-registry.v1`. Templates contain discovery
questions, objective-role shapes, countermetrics, controls, fixture guidance,
candidate-scope guidance, invalid-proxy warnings, and symbolic stop defaults.
They contain no project paths, commands, thresholds, or verdicts. A later
approved project brief supplies those facts; neither a template nor a brief is
an admitted campaign. `papertiger-mise improvement paradigms`, `brief-verify`,
and `compile` own this pre-admission surface; the planner binary does not.

### Maintainability and debt-erasure campaigns

Maintainability is a legitimate improvement paradigm when the experiment
measures removal of a specific, independently detectable change hazard. Its
primary is a count produced by a frozen detector outside the candidate mutation
scope, run over the same frozen repository universe for baseline and candidate.
Suitable primaries include duplicate-helper instances, files exceeding a
declared line budget, raw state-vocabulary match sites, and warnings emitted by
a pinned strict lint profile. Total line count, module count, build time, and
artifact size alone are not maintainability outcomes.

The detector implementation, configuration, included and excluded paths, base
revision, toolchain, and output parser are judge inputs. Admission freezes them
and protects them from candidate writes. The detector must count movement as
well as deletion: moving a duplicated decision into an excluded file, generated
artifact, test helper, or new dependency does not reduce the primary. A known
debt instance and a no-op candidate are calibration controls; a behavior-
deleting candidate is the known-bad anti-golf control.

Every debt-erasure portfolio protects all of these countermetric families:

- an external public behavior and refusal differential, executed from protected
  judge scope against both baseline and candidate;
- test and assertion count non-decrease over the frozen repository universe;
- comment mass and public documentation non-decrease;
- refusal-path count non-decrease; and
- suppression count non-increase, including lint-allow attributes and excluded
  detector paths.

Project briefs instantiate concrete measurements for every family; a family
that cannot be measured makes the debt campaign inadmissible rather than
silently omitted. Correctness and compatibility remain hard constraints, full
project-native gates run under the frozen environment, and the evaluator emits
the per-metric baseline and candidate values rather than a weighted score.

Refactors may allowlist unit-test edits when imports, module placement, or
fixtures necessarily move. That exception requires both the protected external
behavior/refusal differential and assertion-count parity; it never allows
editing the detector, weakening expected outcomes, adding suppressions, or
narrowing the tested public surface.

Debt campaigns produce structural evidence under this paradigm. They do not
count as an actionable behavioral end-outcome loop and cannot satisfy a gate
whose evidence contract requires feature or operator behavior improvement.
Known deterministic fix lists remain ordinary engineering work; a campaign is
appropriate when several valid structural decompositions need bounded proposal,
evaluation, and retained negative evidence.

A Mise nomination is evidence, not promotion. It cannot close its own
Papertiger gate, alter the canonical worktree, advance a product branch, or
declare project work complete. Production promotion must validate a separately
closed Papertiger gate whose evidence locator and digest bind a re-derived
promotion proof. That proof includes the exact nomination, manifest, candidate
result, relied-upon trial receipts, sealed-executor attestations, and external
containment-policy digest. Normal repository ownership, review, commit, and
release rules still apply.

Development successor admission uses a narrower parent-promotion proof with an
explicit non-deployment scope. It still requires operator-owned integration of
the exact nominated tree and a separate Papertiger gate, but it exists only to
authorize that tree and exact N+1 executable as the frozen judge of one child
campaign. The resulting descendant can generate further development evidence;
it cannot deploy itself or enter a production promotion proof as sealed
evidence.

The implemented sealed-confirmation boundary deliberately cannot produce a
promotion proof. Deterministic and paired execution receipts retain raw
objective observations, so accepting a signed `verdict_only` confirmation
attestation would leak the hidden measurements and make the attestation false.
The attestation boundary rejects confirmation trials fail-closed until a
genuinely redacted sealed receipt protocol lands.
The proof and independent-gate verifier are present as a constrained boundary,
not as currently exercisable deployment authority.

Detached worktrees isolate checkout state but are not security sandboxes. Mise
defines one OS-independent local-supervision contract. Its Windows and POSIX
implementations promise
exact argv/environment/working-directory binding, bounded retained output, a
wall deadline that includes child I/O, OS-birth-bound root ownership, cleanup
attempts, and fail-closed stream quiescence before evidence succeeds. It does
not promise adversarial process containment, filesystem isolation, denied
network access, aggregate process or memory ceilings, or cleanup after the Mise
supervisor itself is killed. Those nonportable claims do not exist in a local
campaign manifest.

The runtime uses native cleanup mechanisms as private ergonomics: a
kill-on-close Job Object on Windows and a dedicated process group on POSIX.
They reduce leaked descendants but do not alter campaign admission,
classification, nomination eligibility, or promotion authority. A hostile
POSIX child can leave its group, and the current Windows assignment occurs just
after spawn. Receipts retain the backend and platform as diagnostics alongside
the common `papertiger-mise.portable-local-supervision.v1` contract. The
`papertiger-mise execution-status` command reports the same separation: local
supervision is available, adversarial isolation is not.

This checkout has direct runtime evidence on Windows and Linux. macOS shares
the compiled POSIX implementation and is present in the CI matrix, but no
macOS runner receipt is retained in this repository yet; release claims must
not call the three-platform contract verified until that job actually runs.

WorkspaceOnly campaigns therefore require unrestricted network policy and
cannot declare native process-count or memory limits. They revalidate candidate,
baseline, evaluator, and local fixture bytes around a real child process and
charge the full declared disk reservation conservatively. Exact Git checks rely
on an operator-trusted Git executable and host/repository configuration. Local
evidence can support development nominations, but cannot enter a promotion
proof as isolation evidence.

Admission canonicalizes objective keys lexicographically. A deterministic
evaluator must emit exactly one observation for every admitted objective in that
canonical order, and its successful stdout must be the compact exact Rust-typed
serialization of `papertiger-mise.deterministic-evaluator-output.v1`. Field
order is therefore part of the byte contract; arbitrary valid or sorted JSON is
not canonical. The
objective-order refusal prints both the expected and observed sequences; adapter
authors should treat the admitted manifest returned by `campaign show`, not the
authoring file's display order, as the executable contract.

The evaluator request binds both result-tree IDs and the runtime-verified
baseline worktree locator. Successor-safe evaluators measure the candidate and
that exact no-op worktree under one implementation; they do not carry a
campaign-specific baseline literal forward into a descendant. A tracked Rust
evaluator can use the admitted Cargo environment and select the platform's
normal executable name without placing PowerShell, `.exe`, or a host-specific
tool path in the portable evaluation logic.
When deterministic dogfood owns process interruption and recovery, the
evaluator runs it with `--test-threads=1` as a separate Cargo target. Other
workspace, Mise library, CLI, and evaluator tests remain explicit gates; the
serial requirement is not left to the host test runner's default concurrency.

Adversarial isolation is a separate, platform-neutral attested-worker
contract. Attestation v2 binds outcomes rather than host mechanisms: an
isolated workspace, denied network, read-only evaluator inputs, exact nonzero
process and memory ceilings, cleanup after controller loss, hidden-fixture
secrecy, and verdict-only disclosure where required. An OCI container, VM, or
remote worker may implement that contract on any operator OS; no Job Object,
cgroup, launchd, shell, or container brand appears in campaign authority. The
campaign freezes its executor and profile but cannot nominate its own trust
key. Promotion requires an operator-owned Ed25519 policy and valid signed
attestations for every relied-upon trial. The current runtime still refuses
sealed confirmation because its durable receipt is not yet genuinely
verdict-only, so no implemented path presently claims adversarial isolation.

## Adapter boundaries

Mise owns only the project-generic experiment lifecycle. An adapter translates
domain work into immutable candidate materialization, evaluator invocations,
typed measurements, and evidence locators. It does not move domain authority
into Papertiger.

For Minetiger and Chimera, Mise owns candidate lineage, detached workspace
handling, schedules, retries, cumulative budgets, portable local supervision,
resource leases, generic process telemetry, comparison, classification,
negative-evidence retention, and nomination. Only a separately attested worker
may own resource or access isolation. Minetiger retains Minecraft target identity, loader/JDK and
content selection, session construction, readiness and failure-marker meaning,
rendering correctness, in-process GPU and frame telemetry, scaled raw metric
extraction, and dedicated-server proof. Its existing campaign and verdict
receipts remain readable as historical domain evidence, but no new
project-generic campaign or comparison authority belongs there. Minetiger may
validate Minecraft's self-reported display and GPU state as an independent
domain check; the outer executor receipt is the isolation authority.

For Ghidramink, Mise may organize hypotheses, candidate batches, evaluation,
and nominations. Ghidramink remains authoritative for the
selected Ghidra Program, DB-first evidence, Program-bound write authorization,
automatic mutation leases, atomic metadata application, save behavior,
refresh/index reconciliation, and exact closeout. A Mise nomination never
grants a Ghidra mutation capability.

The first Ghidramink fixture is deliberately narrower: a historical domain
shadow may reopen exact DB-native function evidence and call only the genuinely
read-only `plan_operations` surface. It must retain Program and database state
identities before and after and prove them unchanged. It must not call
`preview_mutation`, acquire or expose a write token, open a mutation
transaction, apply, save, refresh, or create live eligibility. Those limits are
domain proof inside the adapter result and do not weaken Mise's generic equal-
state and decision-ineligibility invariants.

These two adapters are intentionally unlike each other. Chimera exercises Git
source and runtime benchmarking; Ghidramink exercises a live, capability-bound
program mutation system. Project-generic behavior belongs in Mise only after it
survives both boundaries without importing either domain's assumptions.

New campaigns use a CAS-bound candidate-material envelope. The manifest freezes
its kind, protocol, and media type; the envelope binds its canonical typed
payload SHA-256 and a scope rederived from that payload. The first writable
format is `git_change_set`: uniquely path-sorted regular-file add, modify, and
delete records with exact old/new content SHA-256 and Git modes. New content is
embedded as lowercase hex and rehashed before admission and materialization.
Mise rejects stale old state, symlinks, submodules, mode changes, copies,
renames, duplicate content, ambiguous paths, scope drift, and any result tree
other than the frozen base plus the retained records. The public constructor
derives this material from two exact full Git trees; the record command accepts
only its canonical bytes.

Previously admitted `git_patch.v1` campaigns and receipts remain readable and
reverifiable. There is deliberately no public patch-recording compatibility
alias: new operator writes use `candidate build-material` and `--material`.
Legacy admission still refuses an allowlist entry absent from its base tree,
while a typed Git change-set campaign may admit a new path. This contract does
not pretend every domain is Git. A future Ghidramink operation packet requires
a separately named kind, protocol, scope validator, and materializer before it
can become live decision evidence; existing Ghidramink runs remain shadow
evidence.

The materialization boundary resolves a caller's pending worktree path to one
absolute portable identity before Git sees it, so relative CLI paths cannot be
reinterpreted under the source repository. If an attempt fails after its
reservation is bound, that reservation is conservatively charged and remains
immutable. A fresh reservation may retry the same candidate only after every
prior attempt is terminally settled and no materialization receipt exists;
both bindings and an explicit retry event remain in the authority. If a caller
dies after binding but before settlement, `candidate abandon-materialization`
records the operator's reason and atomically charges the exact bound
reservation without claiming what filesystem work occurred.

## Evidence and stopping doctrine

Candidate material, evaluator inputs, canonical successful observations,
bounded failure stdout/stderr, measurements, crashes, and rejection reasons are
content-addressed or durably receipt-bound independently of disposable
worktrees. Successful evaluators must keep stderr empty; any stderr converts the
trial into a retained infrastructure failure instead of silently discarding a
warning. A retry is new lineage with an explicit differentiator, not a rewrite
of failed history. Cold recovery observes the durable PID and OS process birth
identity itself and refuses a matching live evaluator. Absence evidence,
conservative settlement, and the terminal trial transition commit in one
transaction. It can also reverify a successful trial's CAS receipt and
atomically settle a legacy
succeeded-but-reserved row without replaying the evaluator. New successful
completion commits terminal evidence and measured settlement in one SQLite
transaction. The ambiguous `owned` window before PID ownership commits is
closed only by `trial abandon`, which records an operator reason and charges
the full bound reservation atomically. It explicitly records that no process-
absence claim was made; launched work instead requires `trial recover` and its
OS-derived process observation.

Every campaign has a finite deadline, cumulative resource caps, failure caps,
and a no-improvement stopping rule. Sequential early stopping is permitted only
when the declared analysis method remains valid under continuous observation.
Otherwise the campaign completes its predeclared paired repetitions. Evaluator
drift or tampering is an integrity failure and stops the campaign; it is never
converted into an unfavorable score.

The intended result is recursive improvement that remains comprehensible:
autonomous enough to explore, strict enough to falsify itself, durable enough to
recover decidable launched work after restart, and incapable of
self-authorizing its own deployment.

## Operational boundary

Mise is an episodic external driver, not permanent consuming-project tooling.
Keep the exact `papertiger-mise` peer binary from a Papertiger release outside
the consumer and invoke it with `--project-root <consumer>`. Every relative
database, object, manifest, output, and workspace path then resolves from that
root regardless of the agent's caller directory. The consumer owns
`state/papertiger-mise.sqlite` and content-addressed objects under
`state/papertiger-mise-objects/sha256`; the tool installation owns neither.
`papertiger-mise --project-root <consumer> status --json` is bounded and
read-only. `init` is the only authority creation or migration step.

This is intentionally asymmetric with the durable planner: `papertiger
setup-project` vendors only the planner binary and its launchers. A project
does not add or later uninstall Mise merely because it wants one campaign. The
release binary remains outside the consumer and is frozen by absolute path and
hash as the outer judge; its project-owned SQLite/CAS evidence remains after
active RSI work ends so later review can rederive claims.

Campaign admission verifies the manifest from
live Git truth, including clean exact source/control repositories; tracked
adapter, evaluator, fixture-bundle, policy, containment-executor, seed-protocol,
and sampling-protocol artifacts; plus the bundle membership of every
calibration, holdout, and paired-block fixture. The exact
evaluator launcher and outer-judge host executable are local absolute
path-plus-hash bindings. The public library accepts only an opaque
verified-admission token; typed JSON alone is not admission authority. Direct
write access to the SQLite authority or CAS remains an operator trust boundary,
not an adversarial-tenant boundary supplied by this library.

Fixture baselines must be measured under the manifest's exact evaluator
environment, not the authoring shell: the trial supervisor clears ambient
state, and observed toolchain output (including rendered clippy warning sets)
differs between an operator's full environment and the frozen one. Two
Contextmink campaigns have now failed closed on exactly this class of
measurement skew — a stale shared `CARGO_TARGET_DIR` in the first pilot and an
ambient-environment baseline in `contextmink-clippy-portable-v4` — which is the
no-op calibration doing its job, not runtime noise.

An evaluator may additionally declare `rust_build_environment`. Admission then
binds exact Cargo and rustc executables, a named toolchain, the tracked
`Cargo.lock` and Cargo configuration, the Git tree containing vendored sources,
and one target triple with its exact linker executable. Each trial gets
previously absent, trial-scoped `CARGO_HOME` and `CARGO_TARGET_DIR` paths; Mise
also exposes one fresh trial-scoped process scratch directory through the
conventional `TMPDIR`, `TMP`, and `TEMP` variables. Mise supplies the
runtime-owned Cargo variables, derives Cargo's standard
target-specific linker variable from the triple, disables incremental
compilation, and requests offline dependency resolution. Evaluators also
receive `PAPERTIGER_MISE_CARGO_EXECUTABLE`, which preserves the exact admitted
Cargo locator when `cargo run` rewrites Cargo's conventional `CARGO` variable
for its child process; manifests cannot override it. Toolchain-backed
campaigns also freeze an absolute `execution_limits.runtime_root_locator`
separately from the candidate-worktree root. All Cargo, compiler scratch, and
judge-build paths are trial-unique children of that operator-owned root; this
lets operators choose a short native scratch prefix where a host tool has a
fixed path budget without weakening worktree confinement. This contract is
platform-neutral: the campaign supplies its normal Windows, Linux, or macOS
linker rather than relying on ambient `PATH` order. Mise sets Cargo's exact
target-specific linker variable and prepends the verified linker's directory to
the frozen path, so nested direct compiler invocations resolve the same tool.
Compiler subprocesses never need an ambient writable temporary directory. Rust
and judge-build roots use one bounded, domain-separated 128-bit path identity
derived from the exact campaign and trial IDs; receipts retain the full IDs,
while caller-chosen label length cannot exhaust a host path limit. Environment-
bound v2 receipts remain readable; a trial that also preserves a judge build
uses v3 and binds both the exact environment and build receipt. Nomination
reopening rederives either admitted form. This removes shared mutable build
caches, temporary paths, and ambient linker lookup from the decision path.
It does not claim OS-enforced network denial: under
`WorkspaceOnly`, candidate code and build scripts remain capable of using the
host network. Only an attested sealed worker may support that stronger claim.

A development campaign that may parent a successor also declares
`evaluator.judge_build`: exact argv, toolchain name and version, absolute
toolchain executable path and hash, output locator, and maximum executable
bytes. The evaluator writes only the relative output below
`PAPERTIGER_MISE_JUDGE_BUILD_ROOT` and includes the exact `judge_build` echo in
its canonical result. Trial artifact reservations must cover the evaluator
output, receipt overhead, and frozen executable ceiling. Nomination reopening
rechecks both the v3 receipt and the executable object before a v2 successor
proof can be derived.

The command-line surface intentionally exposes only operator boundaries. The
examples below assume the prefix `papertiger-mise --project-root <consumer>`:

```text
papertiger-mise --project-root <consumer> status --json
papertiger-mise init
papertiger-mise execution-status
papertiger-mise campaign source-binding <repository>
papertiger-mise campaign fixture-bundle <repository> <output> --entry <key=locator> ...
papertiger-mise campaign preflight <manifest.json>
papertiger-mise campaign admit <manifest.json>
papertiger-mise promotion derive-parent --nomination <id> --successor-manifest <manifest.json> --objects <object-root>
papertiger-mise promotion verify-parent --nomination <id> --successor-manifest <manifest.json> --task <seq> --gate <name> --evidence <locator> --sha256 <digest> --objects <object-root>
papertiger-mise campaign admit-successor <manifest.json> --parent-nomination <id> --gate-binding <binding.json> --papertiger-db <db> --objects <object-root>
papertiger-mise campaign show-successor <campaign>
papertiger-mise budget reserve <campaign> <reservation> --amount <resource>=<n>
papertiger-mise candidate build-material --repository <repo> --base-tree <tree> --result-tree <tree> --output <file>
papertiger-mise candidate record --proposal <proposal.json> --material <material.json> --reservation <id>
papertiger-mise candidate materialize <candidate> --reservation <id> --worktree <path>
papertiger-mise candidate abandon-materialization <candidate> --reservation <id> --reason <reason>
papertiger-mise candidate show <candidate>
papertiger-mise candidate adjudicate <candidate>
papertiger-mise trial run --spec <trial.json>
papertiger-mise trial recover <trial> --objects <object-root>
papertiger-mise trial abandon <trial> --reason <reason>
papertiger-mise trial show <trial>
papertiger-mise paired reserve-slot <campaign> <candidate> <slot> --seed <file>
papertiger-mise paired prepare --spec <cohort.json> --objects <object-root>
papertiger-mise paired run-next <cohort> --objects <object-root>
papertiger-mise paired adjudicate <cohort> --objects <object-root>
papertiger-mise paired derive-nomination <research-cohort> --no-op <cohort> --known-bad <cohort> --objects <object-root>
papertiger-mise paired recover <execution> --objects <object-root>
papertiger-mise paired list-cohorts <campaign>
papertiger-mise paired show-cohort <cohort>
papertiger-mise paired list-runs <cohort>
papertiger-mise paired show-run <execution>
papertiger-mise object read <sha256> <bytes> --objects <object-root>
papertiger-mise evidence historical-shadow --binding <binding.json> --request <request.json>
papertiger-mise evidence show-historical-shadow <evidence-id>
papertiger-mise evidence domain-shadow --binding <binding.json> --request <request.json>
papertiger-mise evidence show-domain-shadow <evidence-id>
papertiger-mise promotion list [--campaign <id>]
papertiger-mise promotion inspect <nomination> --objects <object-root>
papertiger-mise projection inspect --nomination <id> --objects <object-root>
papertiger-mise projection inspect --candidate <id> --objects <object-root>
papertiger-mise promotion derive --nomination <id> --containment-policy <policy.json>
papertiger-mise promotion verify --nomination <id> --containment-policy <policy.json> ...
```

Adapters use the Rust lifecycle API for candidate recording, exact detached
materialization, owned WorkspaceOnly evaluation, adjudication, and signed
containment evidence. Raw trial-intent, PID-ownership, heartbeat, completion,
and proof-taking reconciliation transitions are crate-private. The sole public
cold-recovery path performs its own OS observation, so external callers cannot
mint nomination evidence by repeating self-asserted process strings. Generic
executors must not acquire domain-specific authority through the public API.
Adapter and policy inputs described as canonical JSON mean UTF-8 and the exact
compact `serde_json::to_vec` bytes of the named typed schema, after that type's
declared collection canonicalization, with no trailing newline. Dynamic JSON
object maps are key-ordered by serde_json's map representation. Admission
rejects semantically equivalent but byte-different input.
`SupervisedTrialOutcome.receipt` is a CAS pointer, so callers reopen it with
`read_object` and deserialize the retained `TrialReceipt`. Trial and
materialization reservation shapes are public typed Rust values rather than an
implicit CLI convention.

The first repeatable external paired dogfood targets an isolated clone of
Contextmink and uses a tracked synthetic score solely to exercise the lifecycle:

```text
cargo build -p papertiger-mise --examples
contextmink_paired_dogfood --contextmink <repo> --state <new-contextmink-state-dir>
```

It creates three real candidate materializations and 48 supervised adapter
executions. A successful run requires an exactly flat no-op, a rejected
known-bad calibration, a qualified research cohort reconstructed from CAS, globally
unique domain receipts, and zero residual budget reservations. The output is
not a Contextmink performance claim.

The repeatable deterministic dogfood is executable with:

```text
cargo test -p papertiger-mise --test deterministic_dogfood -- --test-threads=1
```

It uses fresh on-disk SQLite/CAS state and separate clean source/control Git
repositories. Its nested supervisor test is intentionally killed. A
kill-on-close backend may make the evaluator absent immediately; a
process-group backend may first expose the exact live evaluator. Both paths
must retain and reconcile the same OS-birth-bound absence proof.
An operator-approved project brief can be deterministically compiled only into
a `non_admitted_draft`. Approval binds the exact brief bytes. The compiler
refuses dirty or abbreviated source identity, unresolved fixtures, incomplete
environment behavior, boolean primary objectives, missing countermetrics,
missing correctness or compatibility constraints, undeclared inference scope,
incomplete anti-Goodhart evidence, and unbounded budgets or stop rules. The CLI
also refuses an existing output path rather than overwriting it. The draft carries
an explicit-admission requirement and cannot mutate either authority.
