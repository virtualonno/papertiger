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
`papertiger-mise.improvement_paradigm_registry.v2` at
`docs/mise_templates/v3/registry.json`. The content-addressed v1 and v2
registries keep their earlier template content under the current schema
identifiers (their digests changed at the 0.18.0 schema-id cutover), while new
briefs bind the digest reported by
`papertiger-mise improvement paradigms`. `papertiger-mise improvement verify-registry <file>`
refuses missing canonical paradigms or project command, path,
numeric-threshold, and verdict leakage. Project facts remain in a separately
versioned project brief; the registry supplies question and objective shapes,
never campaign authority.

A brief uses `papertiger-mise.project_improvement_brief.v3` and remains
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

Every objective and `resource_costs` entry carries a typed measurement contract:
subject, process role and executable name, cost category, phase, metric meaning
and units, workload and cardinality, host and environment classes, sampling and
aggregation, cache state, rationale, tradeoff, and limitations. Resource-cost
entries describe diagnostic costs; objectives separately decide which costs
are protected or constrained. Compiler and test-harness costs cannot be labeled
as product behavior. Hard resource constraints require domain rationale and
exact no-op and known-bad fixture hashes, alongside a behavioral primary.

The compiler preserves these contracts in draft v2. Campaign v2 requires them
at admission and compares them with retained samples during completion and CAS
rederivation. Collectors remain trusted for raw measurement truth and host
compliance. A successful planning draft supplies no observations and therefore
establishes neither resource acceptance nor performance improvement. Historical
brief v1 bytes remain historical input; author a new v2 brief to validate or
compile under the current contract.

Proposal and follow-up decisions belong to the external operator or agent.
The proposed task graph is advisory draft content; there is no built-in
task-selecting controller. Generic templates do not require a proposal provider
or access to a mutable external issue or chat. Such a fixture needs its own
input-only snapshot and independence proof before repeated observations can
support a behavioral claim.

`example-project.runtime-readiness.brief.json` is deliberately unresolved: its
checkout is marked dirty and its sealed fixture and native environment are
unavailable. This lets tests prove that compilation refuses incomplete input.
Validate it with `papertiger-mise improvement
verify-brief <file>`; validation reads no planning or Mise authority.

Compilation requires a separate
`papertiger-mise.project_improvement_brief_approval.v2` whose SHA-256 names the exact
brief bytes. The public example intentionally has no approval file.
`papertiger-mise improvement compile --brief <file> --approval <file> --output
<new-file>` refuses an existing output, dirty or abbreviated source identity,
unavailable fixtures, and environment requirements that are not live and
located. Success writes only a
`papertiger-mise.compiled_improvement_draft.v3` with `authority=non_admitted_draft`,
a proposed task graph, exact objective portfolio, fixtures, environment,
mutation scope, budgets, and stop rules. It never opens a database or admits a
campaign; admission remains a separate explicit Mise command.
