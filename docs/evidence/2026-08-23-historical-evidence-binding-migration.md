# Historical evidence binding migration

Date: 2026-08-23

## Outcome

The complete live Papertiger authority now has a locally verifiable evidence
surface. All 73 bindings that were failed or unverifiable at the frozen
pre-migration head were rebound to the immutable historical audit receipt:

- receipt: `docs/evidence/2026-08-23-historical-evidence-binding-audit.md`
- receipt commit: `7569c107736f6f0cf0ba924b61f05a91f841b367`
- receipt SHA-256:
  `fcf6b3904bc5e423bfc45a5ab319dd87f72bb5f65b5224a51030807eb1eae0f2`
- pre-migration authority export SHA-256:
  `501dd46320644556d9a0becf9e4badabcbb4771e8d9a2bc57438e574d628d2bd`

No original close event, locator, digest, note, result, or commit association was
deleted. Each new gate note records its original locator, original digest or
absence, and initial verifier status. The audit receipt records the independent
verification and any retained limitation.

## State restoration proof

Reopening an old dependency safely required reopening its completed dependents;
reopening a child safely required temporarily reopening its retired parent; and
completed historical plans had to be explicitly reactivated. The migration
therefore touched a 45-task dependency and hierarchy closure, not only the 35
tasks that owned the 73 stale bindings.

After all gate changes, a canonical post-migration export was compared with the
frozen pre-migration export. For all 45 tasks, these fields were byte-for-byte
equivalent after normalization by the dump schema:

- status
- result and result provenance
- intent and intent provenance
- kind and priority
- parent and replacement identity
- commit associations

All six temporarily reactivated historical plans returned to their original
`done` status. The two historical umbrella tasks that were `retired` returned
to `retired`; all other tasks in the closure returned to `done`. The comparison
reported zero differences outside the intended gate state and appended event
history.

## Verification

At migration completion, authority event head 1364 was
`event-v1:1364:edc28d9f8049049e542f0fd04a7bc7b911a82525ecf15c852fd405c7e4594c70`.

Fresh checks from the receipt-bound 0.9.0 planner reported:

- `papertiger evidence verify --json`: complete; 81 bindings, 81 verified,
  zero failed, zero unverifiable
- `papertiger audit`: `no findings`
- state restoration comparison: 45 tasks checked, zero field differences;
  every plan status restored

This closes the live verifier debt without claiming that missing historical
bytes were recovered. The immutable audit receipt remains the authoritative
disclosure for the 18 file bindings whose exact old byte identity was not
retained and the two relocated Contextmink nominations whose frozen absolute
source paths prevent full fresh rederivation.
