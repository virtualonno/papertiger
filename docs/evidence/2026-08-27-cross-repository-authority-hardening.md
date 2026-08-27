# Cross-repository authority hardening evidence

Date: 2026-08-27

Base source: `d6dd157`

## Reproduced defect

A RemiNET-owned outcome that required editing a shared skill in Magan acquired
two independent in-progress task identities after the edited repository was
mistaken for the mandatory planning authority. Papertiger authorities cannot
merge, synchronize, or express cross-authority replacement and dependency
edges. Existing external repository labels applied only to commit associations,
while ordinary commands could intentionally select a receipt-bound authority
from another checkout only by changing the working directory or supplying a raw
database path.

## Corrected contract

- Initiative and outcome ownership select one canonical planning authority;
  the repository containing the next edited file does not.
- Independently reviewable or separately committed repository changes can be
  separate tasks inside that authority.
- Editing another repository or loading its Papertiger skill does not create a
  second task identity. External commits use stable `--repo` labels.
- Global `--project-root <DIR>` selects the authority from the receipt at that
  exact root. It never walks upward or falls back to a new default database.
- Missing and version-drifted receipts refuse. Ordinary commands also refuse
  ambiguous coexistence with `--db` or `PAPERTIGER_DB` before opening a
  database. `evidence verify` retains its prior explicit database plus evidence
  root combination.

## Discriminating regression

`explicit_project_root_preserves_one_authority_across_installed_projects`
creates two isolated receipt-managed consumers with initialized authorities,
then invokes the canonical consumer's installed binary from the foreign
consumer directory. The task appears only in the explicitly selected authority,
while the foreign authority's complete event-head identity remains unchanged.

`explicit_project_root_refuses_missing_receipt_and_ambiguous_database_selection`
proves that a missing exact receipt, a nested directory beneath an installed
project, and receipt selection combined with either explicit or environment
database overrides all refuse without creating the override database.

## Verification

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: passed; 274 tests passed and two internal
  subprocess fixtures were intentionally ignored.
- `cargo test -p papertiger-mise --test deterministic_dogfood -- --test-threads=1`:
  passed; two tests passed in 82.92 seconds.
- `papertiger --project-root F:\AI\papertiger evidence verify --json`: complete;
  85 verified, zero failed, zero unsupported.
- `papertiger --project-root F:\AI\papertiger audit`: `no findings`.
- `git diff --check`: passed.
- Canonical and installed agent contracts are byte-identical with SHA-256
  `20e6ebf1ab4388153d058f51a9c73b275e7eee62a6715bb951854de932b73e96`.
- Template, Agent Skills, and Claude skill copies are byte-identical with
  SHA-256
  `acb70f030b2202950ddc6435ef20a308486516fd2157f7fe706bd49cadbc8bce`.
- The final built and receipt-installed planner binaries are byte-identical with
  SHA-256
  `e85040e0a2646a769b590c6fd7058eaf273b4b439eeaaea257fd54008018d382`.

No Papertiger authority schema, task identity, or cross-authority synchronization
mechanism was added. The hardening preserves receipt discovery as the ordinary
default and provides an explicit safe selector only when the canonical project
is elsewhere.
