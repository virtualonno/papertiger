# Historical evidence binding audit

Date: 2026-08-23

Purpose: preserve the complete pre-migration state of every Papertiger gate
binding that the local evidence verifier could not verify, independently check
the original evidence as far as retained state permits, and provide one
immutable, project-relative receipt for authority migration. This receipt does
not replace or rewrite the original events. It records them and their limits.

## Frozen authority snapshot

The pre-migration authority export is
`F:/AI/papertiger-authority-backups/papertiger-0.9.0-pre-historical-binding-migration-20260823.json`:

- dump schema: `papertiger.dump.v7`
- authority event head: `1112`
- tasks: `138`
- events: `1112`
- bytes: `1263050`
- SHA-256: `501dd46320644556d9a0becf9e4badabcbb4771e8d9a2bc57438e574d628d2bd`

At that head, `papertiger evidence verify --json` reported 80 bindings:
7 verified, 38 failed, and 35 unverifiable. The 73 nonverified bindings are
enumerated below. Repeated gates with the same original locator and digest are
grouped, but every gate is named and the binding count remains explicit.

## Verification methods and limits

- Git commits were checked as exact full object IDs in this repository, in the
  named consumer repository, or through GitHub's public commit API. Git file
  snapshots were searched through all path-associated commits reachable from
  refs and reflogs; raw blob bytes and an explicit CRLF rendering were hashed.
- Mise records were reopened through `papertiger-mise` public commands. Legacy
  SQLite authorities were copied under ignored `target/`, and only the copies
  were explicitly migrated before read-only inspection. Original authorities
  and CAS stores were not mutated.
- External files were read and hashed in place. The two historical Papertiger
  executable bindings were also searched across 260 retained `papertiger.exe`
  files under `F:/AI`; no matching copy survived.
- GitHub workflow runs and the public commit were checked on 2026-08-23 through
  GitHub's public API. The Beads setup document was checked at its original URL.
- A stored SHA-256 on a `commit:` locator cannot be independently interpreted:
  Papertiger defines no byte projection for that scheme. The commit object can
  be verified, but an additional digest has no defined input bytes.
- “Not retained” below means the old bytes or a byte identity were not present
  in the scoped sources above. It does not mean that current bytes were used as
  a substitute. The task result, gate note, event history, and related commit
  remain preserved in the authority and its frozen export.

## Commit locators — 15 bindings

- Two bindings: *Implement the truthful deterministic Mise campaign runtime*,
  gates `critical-review` and `runtime-tests`. Original
  `commit:478f0e3f3c036c33e044a118a6af4bffdd17d925`, no stored digest. The exact
  local commit exists with subject “feat(mise): add truthful recursive
  improvement kernel”. Disposition: commit identity independently verified.
- Two bindings: *Dogfood Mise on a deterministic optimization fixture*, gates
  `dogfood-e2e` and `interruption-recovery`. Original
  `commit:d7ccbc29dcefa0e53433f1fd24be1ccaaf2d48d0`, no stored digest. The exact
  local commit exists with subject “test(mise): dogfood deterministic runtime”.
  Disposition: commit identity independently verified.
- Two bindings: *Add statistically honest noisy paired evaluation*, gates
  `paired-statistics-tests` and `statistical-review`. Original
  `commit:acce4d93d7119f15578d39040e12c30f32b6e994`, no stored digest. The exact
  local commit exists with subject “feat(mise): add honest paired analysis
  foundations”. Disposition: commit identity independently verified.
- One binding: *Prove clean-room installation and release readiness*, gate
  `source-gates`. Original
  `commit:968a41b9d1b60472e8c0d869ba9cc06dde895ebd`, no stored digest. The exact
  local commit exists with subject “chore: prepare papertiger 0.5.0 public
  release”. Disposition: commit identity independently verified.
- Two bindings: *Create and verify the public GitHub repository*, gates
  `public-clone` and `public-tree`. Original
  `commit:b8a2907ac0c6ee2c1b38bfd2062d921845088353`, no stored digest. GitHub's
  public API returned that exact commit with subject “Release Papertiger
  0.5.0”. Disposition: public commit identity independently verified.
- One binding: *Version and prove the exact Papertiger 0.6.0 release
  candidate*, gate `clean-source`. Original
  `commit:cf972ce2ba4603034a7b648a7123203d88867a87`, stored SHA-256
  `79e55d7e4015d75fbbc6edda413b2cf88e8941ebe1ced3b15adf51c241850dc8`.
  The exact local commit exists with subject “Preserve setup preview choices”.
  The additional digest has no defined commit-byte projection. Disposition:
  commit identity verified; opaque digest limitation preserved.
- One binding: *Upgrade wow_modernclient to the exact reviewed Papertiger 0.6.0
  build*, gate `managed-cutover`. Original
  `commit:2bad5cfc64ab644e479bd1d140faaa8018c06edf`, stored SHA-256
  `fbf963a5cd46826c0cbf03bdb240b8558b5520227577ffd4b70ce2e54ff9adfb`.
  The exact commit exists in `F:/AI/wow_modernclient` with subject “Make
  Papertiger authority project-local”. The additional digest has no defined
  commit-byte projection. Disposition: commit identity verified; opaque digest
  limitation preserved.
- One binding: *Verify release and upgrade a live consumer*, gate
  `consumer-upgrade`. Original
  `commit:71a57c60652c0723147e8030c8eb987caf859d4a`, no stored digest. The exact
  commit exists in `F:/AI/wow_modernclient` with subject “Upgrade local planner
  integration to 0.7.0”. Disposition: commit identity independently verified.
- Two bindings: *Verify release and upgrade a live consumer*, gates
  `naming-proof` and `workspace-proof`. Original
  `commit:96b59ac479e6bcedbe194502edef198bf88d6e8b`, no stored digest. The exact
  local commit exists with subject “Add structured history and task retrieval”.
  Disposition: commit identity independently verified.
- One binding: *Publish deterministic managed text in 0.7.1*, gate
  `deterministic-text`. Original
  `commit:e8a0fe8d8625993c199cae902ebed991adb67757`, no stored digest. The exact
  local commit exists with subject “Stabilize managed text across checkouts”.
  Disposition: commit identity independently verified.

## File locators — 38 bindings

- Two bindings: *Restore exact-source release verification and consumer
  artifact smoke*, gates `platform-publication-guard` and
  `workflow-cross-check`. Original
  `file:.github/workflows/release-artifacts.yml`, expected
  `a807e47ce779953696d6c2fd910d8729688b4f06210a822f21180e48b5c5eb6b`.
  That byte sequence was not found in path-associated Git refs or reflogs.
  Disposition: original locator and digest preserved; exact bytes not retained.
- Three bindings: *Gracefully serialize short same-authority agent mutations*,
  gate `concurrency-doctrine`; *Make the harness discovery envelope explicitly
  multi-agent and artifact-proven*, gate `harness-honesty`; and *Make CLI
  boundaries canonical and self-describing*, gate `public-wording`. Original
  `file:agent_integration.md`, expected
  `a5e334f3f6be2760af96f423cf85fa3f2b71dea7b3ae5c287a224ac8b5825ae3`.
  That byte sequence was not found in path-associated Git refs or reflogs.
  Disposition: original locator and digest preserved; exact bytes not retained.
- One binding: *Ratify the project-generic authority and upgrade architecture*,
  gate `wow-dogfood-evidence`. Original external file
  `C:/Users/Onno/.codex/memories/rollout_summaries/2026-08-08T12-45-57-1O3y-deepseek_pi_reconstruction_campaign_safe_shutdown.md`, expected
  `790ce66c0ab1cb04c15d873576b5df74038f0feffcd93007f610f6863a684917`.
  The retained 5,021-byte file hashes exactly. Disposition: exact bytes
  independently verified; the path was outside the verifier's project root.
- One binding: *Version and prove the exact Papertiger 0.6.0 release
  candidate*, gate `version-cutover`. Original `file:Cargo.toml`, expected
  `f803b9d159de413e9323aa69b18f982dbb88eedf4360a3226bb0fb77e50b6530`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- One binding: *Define tasklog adoption and authority semantics*, gate
  `authority-separation`. Original
  `file:docs/evidence/2026-08-21-fresh-agent-tasklog-baseline.md`, expected
  `b8b2d8af6ef5caccb43d1d9f0234d44341bb63ed0b79a521dfc76fd1d3b653ef`.
  Exact raw blob bytes were recovered at local commit
  `e75905fcf5abf524d58bd9a72826629181a890a6`. Disposition: exact bytes
  independently verified in Git history.
- One binding: *Migrate applicable project-local Papertiger installations*,
  gate `consumer-migrations`. Original external file
  `F:/AI/papertiger-backups/fleet-0.7-20260812-2051/migration-receipt.json`,
  expected `0adce337846fb9915a29dfa7cce0a4e4c6d51854673a9109e6bc9eaf7fd9006e`.
  The retained 5,906-byte file hashes exactly. Disposition: exact bytes
  independently verified; the path was outside the verifier's project root.
- One binding: *Harden the public release contract from adversarial evidence*,
  gate `smog-evidence`. Original external file
  `F:/AI/papertiger-backups/smog-papertiger-0.7-20260812/20260812T200839853281Z-e85cf903a722/receipt.json`,
  expected `b83a9371e875678cfa7ae9fb1497017aee5054eb4eea0c49ee39d30e8b237f7f`.
  The retained 514,980-byte file hashes exactly. Disposition: exact bytes
  independently verified; the path was outside the verifier's project root.
- One binding: *Ratify the project-generic authority and upgrade architecture*,
  gate `contextmink-patterns`. Original external file
  `F:/AI/wow_modernclient/tools/contextmink/src/project_setup.rs`, expected
  `694e611b1a002c9034e07744bb9a521306b017a75950668b07c60fdac7a1cd97`.
  The live file has evolved, but exact raw blob bytes were recovered in the
  relocated canonical `F:/AI/contextmink` repository at commit
  `dbfccce96170d4faca18eb7566c246934ebb5e1e`. Disposition: exact bytes
  independently verified in Git history.
- One binding: *Upgrade wow_modernclient to the exact reviewed Papertiger 0.6.0
  build*, gate `exact-local-binary`. Original external file
  `F:/AI/wow_modernclient/tools/papertiger/bin/papertiger.exe`, expected
  `838cabe969b20ac90266369b4b59a6a55defc4094d67e7be123ff52df813fd1b`.
  The consumer now contains a newer binary. No exact match survived among 260
  retained Papertiger executables searched under `F:/AI`. Disposition: original
  locator and digest preserved; exact executable not retained.
- One binding: *Replace raw line-count objectives with structural
  maintainability signals*, gate `doctrine-cutover`. Original `file:MISE.md`,
  expected `6f81d44e4c90716480a5657907cfcfcbdc6aebea5e408f6b973d1830f3f87087`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- One binding: *Record optional local commit associations without Git
  authority*, gate `docs`. Original `file:README.md`, with no stored digest.
  A later integration commit contains the documented behavior, but the gate
  never established byte identity. Disposition: unhashed historical defect
  preserved; current bytes are not substituted.
- One binding: *Close local-history correctness and documentation gaps*, gate
  `history-doc-honesty`. Original `file:README.md`, expected
  `c75b4e09e127bcaf181e1d205b15fe3498867e10765e0c4feac7057028501d3a`.
  That byte sequence was not found in path-associated Git refs or reflogs.
  Disposition: original locator and digest preserved; exact bytes not retained.
- Two bindings: *Make the harness discovery envelope explicitly multi-agent
  and artifact-proven*, gate `packaged-consumer-smoke`, and *Restore
  exact-source release verification and consumer artifact smoke*, gate
  `exact-source-suite`. Original `file:scripts/cross_check.sh`, expected
  `dba0655a48815a334d6370a340cf3c5c018f35b1265d78e19ce098bccc71d397`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- One binding: *Close local-history correctness and documentation gaps*, gate
  `history-audit`. Original `file:src/lib.rs`, expected
  `ad7106c07b34c0ba8557cc27dd3a80f0dd8a8937e8cda24c1e4d828f93370de3`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- One binding: *Make CLI boundaries canonical and self-describing*, gate
  `help-contract`. Original `file:src/main.rs`, expected
  `92d8608e7041eb206bb9fb08c44e41f2069f99b131025f7dd1a8316f2e572c0f`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- Two bindings: *Package one harness-neutral Papertiger integration*, gates
  `portability` and `tests`. Original `file:src/project_setup.rs`, with no stored
  digest. The monolithic path exists in historical commits from
  `968a41b9d1b60472e8c0d869ba9cc06dde895ebd` through
  `281a7cac6563cd4c6ce98f1a38eb608f37ec525d`, but the gate never established
  byte identity. Disposition: unhashed and later-moved historical defect
  preserved; current module bytes are not substituted.
- One binding: *Make setup-project upgrades full-cutover, effortless, and
  self-verifying*, gate `crash-safe-ownership`. Original
  `file:src/project_setup/filesystem.rs`, expected
  `09dc7241db2dfbedc945e6e7c182ea36286951ae36fe49872bd348b343cae4f5`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- Three bindings: *Make setup-project upgrades full-cutover, effortless, and
  self-verifying*, gates `cross-platform-paths`, `idempotent-upgrade`, and
  `legacy-cutover`. Original `file:src/project_setup/mod.rs`, expected
  `1a14f28917db456ac36339492f7f922b78b3eeb900384fbfd35561d32c6c06af`.
  That byte sequence was not found in path-associated Git refs or reflogs.
  Disposition: original locator and digest preserved; exact bytes not retained.
- One binding: *Make setup-project upgrades full-cutover, effortless, and
  self-verifying*, gate `authority-identity`. Original
  `file:src/project_setup/receipt.rs`, expected
  `db0284ca4e703fdc189d80c157af210505fd6e78d38f454840565df04328bb05`.
  That byte sequence was not found in path-associated Git refs or reflogs.
  Disposition: original locator and digest preserved; exact bytes not retained.
- One binding: *Version and prove the exact Papertiger 0.6.0 release
  candidate*, gate `workspace-gates`. Original
  `file:target/release/papertiger.exe`, expected
  `838cabe969b20ac90266369b4b59a6a55defc4094d67e7be123ff52df813fd1b`.
  The build path now contains a newer binary. No exact match survived among 260
  retained Papertiger executables searched under `F:/AI`. Disposition: original
  locator and digest preserved; exact executable not retained.
- One binding: *Make the harness discovery envelope explicitly multi-agent and
  artifact-proven*, gate `envelope-byte-identity`. Original
  `file:templates/papertiger/SKILL.md`, expected
  `36ec88742ea08421d2d3f9a9825cbc916fa6dcb5ace5944e83576da16d5b5b15`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- Two bindings: *Add durable task consolidation through retire --into*, gate
  `replacement-roundtrip`, and *Version and prove the exact Papertiger 0.6.0
  release candidate*, gate `free-text-input`. Original `file:tests/cli.rs`,
  expected `c9e6ed783c2ccbd223dcf80498165e12e1fe3697e3a887db63209d1c497fb71b`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.
- Three bindings: *Record optional local commit associations without Git
  authority*, gate `tests`, and *Expose event-derived lifecycle recency*, gates
  `tests` and `transfer`. Original `file:tests/core.rs`, with no stored digest.
  Later integration commits contain the tested behavior, but the gates never
  established byte identity. Disposition: unhashed historical defects
  preserved; current bytes are not substituted.
- Five bindings: *Gracefully serialize short same-authority agent mutations*,
  gates `long-lock-refusal` and `short-lock-overlap`; *Close local-history
  correctness and documentation gaps*, gate `timestamp-import`; *Add durable
  task consolidation through retire --into*, gate `replacement-invariants`;
  and *Make CLI boundaries canonical and self-describing*, gate
  `canonical-cli-boundary`. Original `file:tests/core.rs`, expected
  `698c2bfaa3b2baf4da40cfcbe1e2782ed22e00c10f771c14202da161b6a007e7`.
  Exact raw blob bytes were recovered at local commit
  `49050130a3f48adf33363cf42ab3a83f1258f3ef`. Disposition: exact bytes
  independently verified in Git history.

## Mise and receipt locators — 14 bindings

- One binding: *Authorize exact Mise successor admission*, gate
  `successor-admission-proof`. Original
  `papertiger-mise:parent-promotion-proof/6609479cac82aeb2dab94f4f57be035264e04f522afb13366a820eaf260b0a9e`,
  expected the same digest. `papertiger-mise object read` reopened the exact
  1,872-byte CAS object from `state/mise-lineage-a05/objects`. Disposition:
  content address and bytes independently verified.
- One binding: *Authorize corrected Mise successor admission*, gate
  `successor-admission-proof`. Original
  `papertiger-mise:parent-promotion-proof/bdab15f2be21c7186a9e3cca5c6b3f3d41fb450fbc4987d60d823f1f48b351c4`,
  expected the same digest. `papertiger-mise object read` reopened the exact
  1,872-byte CAS object from `state/mise-lineage-a06/objects`. Disposition:
  content address and bytes independently verified.
- One binding: *Run a build-bound lifecycle successor*, gate
  `lifecycle-successor-admission`. Original
  `papertiger-mise:parent-promotion-proof/91129ac9867fceae073249dc2568408e1e4701a07980b4faa208f3645cdd857e`,
  expected the same digest. `papertiger-mise object read` reopened the exact
  2,223-byte CAS object from the default object store. Disposition: content
  address and bytes independently verified.
- One binding: *Authorize descendant Mise source integration*, gate
  `descendant-source-integration`. Original
  `papertiger-mise:nomination/6c64d96b869c2f7125c3affa6b43b755ccbedabb2ed58d39b8a348bddf1bafbb`,
  expected the same digest. A migrated diagnostic copy of the retained a06
  authority rederived the nomination through `promotion inspect`. Disposition:
  nomination and relied-upon evidence independently rederived.
- One binding: *Authorize parent Mise preflight source integration*, gate
  `parent-preflight-source-integration`. Original
  `papertiger-mise:nomination/8160286551ec541a281a165b3844c000b06b88b55c417643acad01830e783fc6`,
  expected the same digest. A migrated diagnostic copy of the retained a06
  authority rederived the nomination through `promotion inspect`. Disposition:
  nomination and relied-upon evidence independently rederived.
- One binding: *Cut over Minetiger and Chimera experimentation through a domain
  adapter*, gate `minetiger-shadow-equivalence`. Original
  `receipt:ad387d27d35a45e9a11327e01ebbd3820b54915ceabc8e2887906bc61544e7b0`,
  expected the same digest. `papertiger-mise object read` reopened the exact
  2,488-byte CAS object from the default object store. Disposition: content
  address and bytes independently verified; the owning umbrella remains
  retired for unrelated architecture reasons.
- One binding: *Run a portable Papertiger self-evaluation probe*, gate
  `portable-self-campaign`. Original
  `receipt:367794014118a3272b556b70a328e0caad270eeefae2471bb3c88eb763310c9c`,
  expected the same digest. A migrated diagnostic copy of the retained
  self-campaign authority rederived the nomination through `promotion inspect`.
  Disposition: nomination and relied-upon evidence independently rederived.
- One binding: *Split Mise lifecycle trial execution through a frozen
  campaign*, gate `mise-runtime-split-promotion`. Original
  `receipt:733b4df8bd3c80b94410255d953e975ff5b83b632798ffad0e325639d97d7560`,
  expected the same digest. The current default authority rederived the
  nomination and its relied-upon evidence through `promotion inspect`.
  Disposition: nomination independently rederived.
- One binding: *Run a build-bound lifecycle successor*, gate
  `lifecycle-integration`. Original
  `receipt:f8de3ee73faedf161e3a44ce3ba7196642e308ffac26afd5ee60a97ce1f95cbc`,
  expected the same digest. The current default authority rederived the
  nomination and its relied-upon evidence through `promotion inspect`.
  Disposition: nomination independently rederived.
- One binding: *Revalidate Contextmink through the portable Mise executor*,
  gate `portable-campaign-evidence`. Original
  `receipt:b90783848263fcd6d32120333724c626f9b085e9c78722129b50ffc441002e24`,
  expected the same digest. A migrated diagnostic copy of the retained v3
  authority recognized the exact durable nomination, then full rederivation
  refused because its frozen source locator points to the pre-relocation
  `F:/AI/wow_modernclient/tools/contextmink/.../source` path. Disposition:
  durable identity retained and relocation limitation preserved; no substitute
  path or evidence was minted.
- One binding: *Probe: run Contextmink clippy-idiom campaign v5 through the
  frozen lean judge*, gate `v5-campaign-evidence`. Original
  `receipt:e542331920d3b7c95bde9ffb5d51bbafb99118cfc11fc0eff4153b767bcc6ddc`,
  expected the same digest. A migrated diagnostic copy of the retained v5
  authority recognized the exact durable nomination, then full rederivation
  refused because its frozen source locator points to the pre-relocation
  `F:/AI/wow_modernclient/tools/contextmink/.../source` path. Disposition:
  durable identity retained and relocation limitation preserved; no substitute
  path or evidence was minted.
- One binding: *Upgrade wow_modernclient to the exact reviewed Papertiger 0.6.0
  build*, gate `consumer-smoke`. Original external receipt
  `F:/AI/papertiger-backups/wow_modernclient-20260812-015706157-live/papertiger.dump.v6.json`,
  expected `bd2d948c0119f521f884ec2bf0bd8518b70ea40a09a8e91170a8e79c0920dbea`.
  The retained 1,975,112-byte file hashes exactly. Disposition: exact external
  receipt independently verified.
- One binding: *Publish and verify the public 0.7.0 artifacts*, gate
  `published-consumer`. Original external receipt
  `F:/AI/papertiger-backups/public-0.7.0/papertiger-0.7.0-windows-x86_64.zip.sha256`,
  expected `19667fd94b0fd5aa239a982e636eb6031fcd15e1f4251fc45af79d74ac317812`.
  The retained 102-byte file hashes exactly. Disposition: exact external
  receipt independently verified.
- One binding: *Publish deterministic managed text in 0.7.1*, gate
  `fleet-no-churn`. Original external receipt
  `F:/AI/papertiger-backups/fleet-0.7.1-20260813/migration-receipt.json`,
  expected `6770db955793d22ae8878ff257df1d98f8a37b367678bc22ced90528ad618702`.
  The retained 5,366-byte file hashes exactly. Disposition: exact external
  receipt independently verified.

## URL and note locators — 6 bindings

- One binding: *Ratify the project-generic authority and upgrade architecture*,
  gate `beads-upgrade-patterns`. Original
  `url:https://github.com/gastownhall/beads/blob/main/docs/cli-reference/setup.md`,
  no stored digest. The URL still resolves; on 2026-08-23 it described Beads
  setup as writing editor-specific integration files and native hooks.
  Disposition: locator and claimed research subject independently verified;
  mutable web bytes were never content-addressed.
- One binding: *Create and verify the public GitHub repository*, gate
  `github-push`. Original
  `url:https://github.com/virtualonno/papertiger/commit/b8a2907ac0c6ee2c1b38bfd2062d921845088353`,
  no stored digest. GitHub's public API returned that exact commit.
  Disposition: public target independently verified.
- One binding: *Create and verify the public GitHub repository*, gate
  `hosted-artifacts`. Original
  `url:https://github.com/virtualonno/papertiger/actions/runs/31284999458`, no
  stored digest. GitHub reported `completed/success` at head
  `b8a2907ac0c6ee2c1b38bfd2062d921845088353`. Disposition: run identity,
  terminal status, conclusion, and head independently verified.
- One binding: *Publish and verify the public 0.7.0 artifacts*, gate
  `hosted-targets`. Original
  `url:https://github.com/virtualonno/papertiger/actions/runs/31642774851`, no
  stored digest. GitHub reported `completed/success` at head
  `545a8be0e883ed292f326623993efc8ecc2c8e50`. Disposition: run identity,
  terminal status, conclusion, and head independently verified.
- One binding: *Publish deterministic managed text in 0.7.1*, gate
  `hosted-patch`. Original
  `url:https://github.com/virtualonno/papertiger/actions/runs/31645329830`, no
  stored digest. GitHub reported `completed/success` at head
  `5c1691c004162b83b970c8e77cd6e6e8dd893eef`. Disposition: run identity,
  terminal status, conclusion, and head independently verified.
- One binding: *Harden the public release contract from adversarial evidence*,
  gate `local-release-gates`. Original `note:cross-check-2026-08-12`, no stored
  digest. The authority retains the gate note describing the exact cross-check
  lanes and the associated clean source commit
  `5a447f68d5f0931e3689ee02abdb58164bf8b84a`. No separately addressable note
  payload was ever created. Disposition: authority record and source commit
  independently verified; absence of content identity preserved.

## Migration disposition

These 73 bindings mix valid historical claims with invalid long-lived binding
forms. Rebinding them to current source files would falsify history. The safe
migration is to reopen each affected terminal gate and close it against this
exact immutable receipt with this file's SHA-256. The old locator, digest, gate
note, task result, and close event remain in the append-only authority history
and in the frozen pre-migration export above.

After migration, authority-wide verification must report every live binding as
verified. That result means the current gate surface is locally resolvable and
the historical limitations are durably disclosed; it does not upgrade the 18
file bindings whose original exact byte identity was not retained, nor the two
relocated Contextmink nominations whose frozen absolute source paths prevent a
fresh full rederivation.
