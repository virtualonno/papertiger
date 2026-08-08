# Mise improvement brief example

This directory contains a synthetic planning-input example. It exercises the
template and campaign-compilation contract without carrying project history,
local paths, live receipts, or campaign authority.

A project brief may describe project intent, native observables, candidate
workflows, countermetrics, negative controls, and measurement limitations. It
cannot:

- admit or mutate a Mise campaign;
- write Papertiger or project-owned state;
- qualify, integrate, promote, or deploy a candidate;
- treat a convenient proxy as proof of a product improvement; or
- move project-specific commands, thresholds, or doctrine into a generic
  template.

The generic registry uses schema
`papertiger.improvement-paradigm-registry.v1` at
`docs/mise_templates/v2/registry.json`. The content-addressed v1 registry
remains at `docs/mise_templates/v1/registry.json` so old briefs remain
verifiable, while new briefs bind the digest reported by
`papertiger-mise improvement paradigms`. `papertiger-mise improvement verify <file>`
refuses missing canonical paradigms or project command, path,
numeric-threshold, and verdict leakage. Project facts remain in a separately
versioned project brief; the registry supplies question and objective shapes,
never campaign authority.

A brief uses `papertiger.project-improvement-brief.v1` and remains
`planning_input_only`. Derive it read-first from the consuming project's live
source, tests, docs, task authority, runtime evidence, known failures,
invariants, candidate surfaces, fixtures, environment, and resource costs.
Every evidence item is explicitly `live`, `sampled`, `stale`, `unavailable`, or
`inferred`; every non-live item carries a limitation. Fixture disclosure is
typed as `disclosed_workspace`, `sealed`, or `unavailable`. Environment
requirements bind behavior as well as optional locator and digest, because a
tool executable alone is not proof that its native child environment works.
Every opportunity also declares its inference scope and explicit evidence
against capability deletion, workload narrowing, cost displacement, test
weakening, and self-certification. Its objective portfolio requires a
quantitative primary, distinct correctness and compatibility hard constraints,
and protected countermetrics; results remain per-objective rather than a
weighted scalar.

`example-project.runtime-readiness.brief.json` is deliberately unresolved: its
checkout is marked dirty and its sealed fixture and native environment are
unavailable. This lets tests prove that compilation refuses incomplete input.
Validate it with `papertiger-mise improvement
brief-verify <file>`; validation reads no planning or Mise authority.

Compilation requires a separate
`papertiger.project-improvement-brief-approval.v1` whose SHA-256 names the exact
brief bytes. The public example intentionally has no approval file.
`papertiger-mise improvement compile --brief <file> --approval <file> --output
<new-file>` refuses an existing output, dirty or abbreviated source identity,
unavailable fixtures, and environment requirements that are not live and
located. Success writes only a
`papertiger.compiled-improvement-draft.v1` with `authority=non_admitted_draft`,
a proposed task graph, exact objective portfolio, fixtures, environment,
mutation scope, budgets, and stop rules. It never opens a database or admits a
campaign; admission remains a separate explicit Mise command.
