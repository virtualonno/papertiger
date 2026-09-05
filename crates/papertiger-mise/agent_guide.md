# Use Papertiger Mise

Mise is the experiment authority for finite comparisons of immutable candidates.
The agent supplies hypotheses and candidates; Mise freezes the evaluator,
accounts for the budget, runs trials, and retains evidence for later review.
Use it when comparing several plausible changes justifies a reusable evaluator,
controls, and durable negative evidence. A known fix with a decisive regression
test usually needs ordinary engineering. Do not manufacture a campaign merely
to make use of the toolkit.

## Establish a useful experiment

Name the observable outcome before authoring a manifest. Freeze a baseline,
representative inputs, a quantitative primary with a practical improvement
threshold, correctness and compatibility constraints, and countermetrics that
would catch a misleading win. No-op and known-bad controls must demonstrate that
this evaluator can distinguish improvement from breakage. Instruction length,
test success, a synthetic score, and compiler cost alone do not prove better
agent or product behavior.

Check improvement headroom on the frozen workload. A primary counting exact
outcomes cannot improve when the baseline already matches every case. Such a
matrix can prove regression rejection, but cannot demonstrate an improvement
nomination. Preserve correctness as a constraint and choose a defensible primary
with real headroom, or keep the work as an ordinary regression comparison.
Do not weaken controls, invent a proxy, or expand the framework to rescue a
value claim. Record setup and operator cost separately from runtime ledgers.

For instruction changes, measure actual unprompted agent behavior on positive
requests and negative controls: appropriate invocation, omitted obligations,
duplicates, tool failures, and unnecessary work. A lexical skill check verifies
packaging, not agent adoption. Keep the model, harness, supplied context, and
sampling limits visible. A new prompt evaluated against a mutable conversation
needs an independently checked input boundary; use immutable snapshots otherwise.

If the evaluator cannot support the decision, improve the evaluator or use
ordinary engineering. The framework cannot supply missing domain truth.

## Enter from current evidence

Invoke a stable external `papertiger-mise` binary with
`--project-root <consumer>`. Every relative path resolves there. The consumer
owns the SQLite authority and objects; keep the same binary path and bytes
throughout an admitted campaign. Do not rebuild a frozen driver in place.

```text
papertiger-mise --project-root <consumer> status --json
papertiger-mise --project-root <consumer> campaign inspect <campaign-id>
papertiger-mise --project-root <consumer> campaign inspect <campaign-id> --section trials
```

Inspection is bounded discovery over recorded state, not live execution
readiness or CAS verification. Follow the returned continuation arguments to
find omitted identities; inspect their exact records and evidence. Old
`evaluating` candidates do not prove a process is alive. For launched work,
`trial recover` or `paired recover` performs the OS-bound absence check; never
invent absence from status, a PID alone, or elapsed time.

`init` is the sole creation/migration command. Never initialize a replacement
when prior work existed, directly write SQLite/CAS, or clone an authority for
continued operation. Set `PAPERTIGER_ACTOR` before mutations.

## Prepare, compare, and stop

Read `guide --reference` before first admission. The Rust public API exposes
typed manifest, candidate material, trial, measurement, and result contracts;
use those types rather than reverse-engineering canonical JSON field order.
`improvement paradigms` and `improvement show <key>` provide design questions,
not an evaluator or ready-to-admit manifest. Brief compilation produces only
a non-admitted draft and does not replace campaign admission.

Use `campaign source-binding`, `campaign fixture-bundle`, and
`campaign preflight <manifest.json>` before admission. Repair all independently
reported defects in an unadmitted draft. Admission freezes its identity;
changing its judge, inputs, or budget requires another admission attempt.

Run no-op and known-bad controls before interpreting research candidates.
`candidate build-material` constructs exact material from full base/result
trees. Reserve finite resources, record and materialize the candidate, and use
`trial run` for deterministic evaluation or the separate `paired` lifecycle
for a predeclared paired design. Command help names each required input.
Calibrate and reopen results under the same frozen evaluator environment.

Adjudicate from retained evidence, then use `projection inspect` to reopen a
terminal candidate or nomination. Attach that projection with `papertiger mise
project` only when it informs a planning outcome. Rejections and infrastructure
failures are useful retained results; neither should be disguised as a score.
Stop at the admitted budget/deadline/failure rules or when another candidate no
longer justifies its cost. The external agent owns that decision and proposal
work; Mise does not choose planner tasks or invoke a model provider.

Local execution supports trusted-host development evidence. It does not provide
adversarial isolation, hidden-fixture secrecy, independent collector truth, or
production promotion. A nomination is evidence for separate review and
integration. Successor admission has its own exact build and independent gate
requirements; a campaign never authorizes its own replacement judge.
