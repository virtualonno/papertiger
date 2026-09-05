# Mise's present value and next investment boundary

Retain Mise as an opt-in, reproducible experiment authority. Its current evidence
does not justify treating it as a general recursive-improvement engine or adding
it to ordinary planning. The next investment must be a bounded useful comparison
with room to improve, rather than more infrastructure to make the value claim true.

The job is to decide whether a candidate is worth adopting, with evidence another
session can independently reopen. Those are two related benefits: choosing well
and retaining trustworthy experiment history. They need separate value evidence.

## Direct comparison with ordinary regression work

`replay_native_value.py` uses the same retained, hash-checked native binaries,
profiles, source bindings and fixtures as two historical Mise campaigns. It calls
the executables directly, outside Mise, twice per variant. Contextmink command
strings are parser input through stdin; they are never executed as shell commands.
Ghidramink's native environment probe receives its fixture through stdin.

| Fixed case suite | Incumbent | Known-bad control | Research candidate | Direct regression decision | Retained Mise decision |
| --- | ---: | ---: | ---: | --- | --- |
| Contextmink guard, 18 cases | 18 exact | 6 exact | 13 exact, 5 unsafe acceptances | Reject | Reject |
| Ghidramink environment, 12 cases | 12 exact | 2 exact | 9 exact, 2 unsafe acceptances | Reject | Reject |

Both repetitions agreed. The Ghidramink research candidate also misreported a
mixed environment without making it fully acceptable, hence three mismatches but
two unsafe acceptances. These fixtures cover native behavior, not downstream
repository task success or independent generation of useful candidates.

Both frozen manifests maximize `exact-outcomes` with a minimum practical change
of one. The incumbents already satisfy every case. Headroom is therefore zero:
no candidate can qualify by improving this primary on these fixed cases. This is
a legitimate rejection/control experiment, but an impossible improvement test.
Changing the metric after seeing outcomes would require a new honest experiment,
not a reinterpretation of the frozen campaign.

The retained direct-run report records all invocations, outputs, source identities,
SHA-256 values and wall times at
`state/agent-integration-20260905/ordinary-native-comparison-final.json`. The final
rerun also verifies the profile against the frozen manifest and each variant's
fixture bytes and Git tree against its bound commit. A compact copy of the
identities and measurements is committed in
[the comparison summary](2026-09-05-mise-value-comparison.json). Reproduce with:

```powershell
python -X utf8 docs/evidence/replay_native_value.py --output <new-report.json>
```

The script expects this machine's retained consumer evidence trees. It is a small
comparison artifact, not a replacement experiment platform or a portable release
test. Source identities and hashes remain in the report even if those local trees
later become unavailable.

## What survives the comparison

The ordinary runner reaches the same candidate rejection with a short direct
fixture loop. This falsifies a claim that Mise was necessary for these particular
decisions. It does not reproduce Mise's content-addressed evidence, frozen outer
judge, calibration bindings, candidate lineage, reservations, budget enforcement,
and failure/recovery transitions. Removing Mise transfers those obligations to
someone or something else when the experiment actually needs them.

Ten historical receipt checks were reopened with their frozen drivers in this
session; the read-only results are retained in
`state/agent-integration-20260905/historical-reopening.json`. Rebuilding a driver
would change the experiment, so current build verification used a separate target
directory. Reopening evidence establishes reproducibility of the retained record;
it does not establish that the experiment's objective was valuable.

The direct-run timings cover already-built native executions. They exclude fixture
authoring, acquisition, build, review and operator time. Historical Mise budgets
and these new warm runs have different cost boundaries, so no speedup or total
cost ratio is inferred. The persistent two-authority/CAS/driver model has a real
ownership cost; this pass did not measure its long-term amortization.

## Implementation alignment

The CLI now supplies a compact `guide` with a conditional full reference and
bounded `campaign inspect` sections. Agents can discover the next operation and
inspect a growing campaign without reading an entire campaign dump. Snapshot
inspection is labeled as state, not a fresh verification receipt. Ordinary
Papertiger work does not load the Mise contract.

The guide and MISE.md now require callers to establish primary-metric headroom,
compare with ordinary engineering, and count setup/operator cost. Headroom is
caller-owned reasoning; the runtime does not infer arbitrary metric ceilings.
No integrity invariant, shadow-evidence exclusion, or operator-owned promotion
boundary was weakened to produce a positive demonstration.

The required format, Clippy, workspace tests, and separately selected ignored
deterministic dogfood passed after the guide change. Their command receipts and
logs are retained under `state/agent-integration-20260905/final-guide-gate-*`.

## Reversal and stop conditions

Continue only a bounded experiment on a real development decision with measurable
headroom. Freeze equivalent inputs for the strongest ordinary workflow, include
at least one held-out workload, and budget candidate acquisition, review and
operator time before running. Measure whether Mise changes a consequential
decision, prevents a credible evidence/recovery failure, or reduces total cost
enough to pay for its ownership. Strong no-change outcomes must remain acceptable.

If that comparison yields only another correct rejection that ordinary tests
already provide, stop expanding the general RSI claim and keep the narrow
experiment-control role. Repeated real downstream gains or demonstrated recovery
benefit would justify widening it. The existence of a sophisticated runtime,
more campaign receipts, or more passing integrity tests would not.
