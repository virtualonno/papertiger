use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisedTrialSpec {
    pub trial_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub reservation_id: String,
    pub tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeterministicEvaluatorRequest {
    pub schema: String,
    pub trial_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub candidate_result_tree: String,
    pub baseline_result_tree: String,
    pub baseline_working_directory: String,
    pub tier: String,
    pub fixture_locator: String,
    pub fixture_sha256: String,
    pub evaluator_protocol: String,
    pub objectives: Vec<crate::manifest::ObjectiveSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeterministicEvaluatorOutput {
    pub schema: String,
    pub observations: Vec<DeterministicObservation>,
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_build: Option<EvaluatorJudgeBuild>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorJudgeBuild {
    pub argv: Vec<String>,
    pub executable_locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeBuildReceipt {
    pub schema: String,
    pub argv: Vec<String>,
    pub toolchain_name: String,
    pub toolchain_version: String,
    pub toolchain_executable_sha256: String,
    pub environment_sha256: String,
    pub source_tree: String,
    pub executable_locator: String,
    pub executable: PreservedObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisedTrialOutcome {
    pub trial_id: String,
    pub receipt: PreservedObject,
    pub classification: Classification,
}

pub(super) fn expected_trial_environment(
    manifest: &CampaignManifest,
    campaign_id: &str,
    trial_id: &str,
) -> Result<BTreeMap<String, String>> {
    let mut environment = manifest.evaluator.environment.clone();
    let rust = manifest.evaluator.rust_build_environment.as_ref();
    let judge_build = manifest.evaluator.judge_build.as_ref();
    if rust.is_none() && judge_build.is_none() {
        return Ok(environment);
    }
    let runtime_root_locator = manifest
        .execution_limits
        .runtime_root_locator
        .as_deref()
        .unwrap_or(&manifest.execution_limits.workspace_root_locator);
    let runtime_root = std::fs::canonicalize(runtime_root_locator)
        .context("canonicalize frozen runtime root for trial environment")?;
    let path_identity = trial_path_identity(campaign_id, trial_id);
    let mut runtime_values = Vec::new();
    if let Some(rust) = rust {
        let rust_root = runtime_root.join("rust").join(&path_identity);
        let temporary_directory = portable_absolute(&rust_root.join("temporary"))?;
        runtime_values.extend([
            ("CARGO".to_owned(), rust.cargo_executable_locator.clone()),
            (
                "PAPERTIGER_MISE_CARGO_EXECUTABLE".to_owned(),
                rust.cargo_executable_locator.clone(),
            ),
            (
                "CARGO_HOME".to_owned(),
                portable_absolute(&rust_root.join("cargo-home"))?,
            ),
            ("CARGO_INCREMENTAL".to_owned(), "0".to_owned()),
            ("CARGO_NET_OFFLINE".to_owned(), "true".to_owned()),
            (
                "CARGO_TARGET_DIR".to_owned(),
                portable_absolute(&rust_root.join("target"))?,
            ),
            ("CARGO_TERM_COLOR".to_owned(), "never".to_owned()),
            ("RUSTC".to_owned(), rust.rustc_executable_locator.clone()),
            ("RUSTUP_TOOLCHAIN".to_owned(), rust.toolchain.clone()),
            ("TEMP".to_owned(), temporary_directory.clone()),
            ("TMP".to_owned(), temporary_directory.clone()),
            ("TMPDIR".to_owned(), temporary_directory),
        ]);
        if let Some(linker) = &rust.linker {
            prepend_bound_tool_directory(&mut environment, &linker.executable_locator)?;
            let key = rust
                .linker_environment_key()?
                .context("Rust linker binding omitted its target environment key")?;
            runtime_values.push((key, linker.executable_locator.clone()));
        }
    }
    if judge_build.is_some() {
        let judge_root = runtime_root.join("judge").join(path_identity);
        runtime_values.push((
            "PAPERTIGER_MISE_JUDGE_BUILD_ROOT".to_owned(),
            portable_absolute(&judge_root)?,
        ));
    }
    for (key, value) in runtime_values {
        if environment.insert(key.to_owned(), value).is_some() {
            bail!("manifest attempted to override runtime-owned trial environment '{key}'");
        }
    }
    Ok(environment)
}

fn prepend_bound_tool_directory(
    environment: &mut BTreeMap<String, String>,
    executable_locator: &str,
) -> Result<()> {
    let directory = Path::new(executable_locator)
        .parent()
        .context("bound tool executable has no parent directory")?;
    let mut paths = vec![directory.to_path_buf()];
    if let Some(existing) = environment.get("PATH") {
        paths.extend(std::env::split_paths(existing).filter(|path| path != directory));
    }
    let joined = std::env::join_paths(paths).context("join frozen evaluator PATH entries")?;
    let joined = joined
        .into_string()
        .map_err(|_| anyhow::anyhow!("frozen evaluator PATH is not UTF-8"))?;
    environment.insert("PATH".to_owned(), joined);
    Ok(())
}

pub(super) fn prepare_trial_environment(
    manifest: &CampaignManifest,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    if manifest.evaluator.rust_build_environment.is_none()
        && manifest.evaluator.judge_build.is_none()
    {
        return Ok(());
    }
    let mut keys = Vec::new();
    if manifest.evaluator.rust_build_environment.is_some() {
        keys.extend(["CARGO_HOME", "CARGO_TARGET_DIR", "TMPDIR"]);
    }
    if manifest.evaluator.judge_build.is_some() {
        keys.push("PAPERTIGER_MISE_JUDGE_BUILD_ROOT");
    }
    let paths = keys
        .iter()
        .map(|key| {
            environment
                .get(*key)
                .map(PathBuf::from)
                .with_context(|| format!("runtime-owned trial environment omitted {key}"))
        })
        .collect::<Result<Vec<_>>>()?;
    for path in &paths {
        if path.exists() {
            bail!(
                "fresh trial environment path already exists: {}",
                path.display()
            );
        }
    }
    for path in &paths {
        std::fs::create_dir_all(path)
            .with_context(|| format!("create fresh trial environment path {}", path.display()))?;
    }
    Ok(())
}

fn preserve_judge_build(
    manifest: &CampaignManifest,
    environment: &BTreeMap<String, String>,
    result_tree: &str,
    reported: Option<&EvaluatorJudgeBuild>,
    object_root: &Path,
) -> Result<Option<JudgeBuildReceipt>> {
    let Some(binding) = &manifest.evaluator.judge_build else {
        if reported.is_some() {
            bail!("evaluator reported a judge build that is absent from the frozen manifest");
        }
        return Ok(None);
    };
    let reported = reported.context(
        "evaluator omitted the frozen judge build; emit judge_build with the exact argv and executable_locator",
    )?;
    if reported.argv != binding.argv || reported.executable_locator != binding.executable_locator {
        bail!("evaluator judge-build attestation differs from the frozen manifest");
    }
    let root = Path::new(
        environment
            .get("PAPERTIGER_MISE_JUDGE_BUILD_ROOT")
            .context("runtime-owned judge-build root is absent")?,
    );
    let canonical_root = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalize fresh judge-build root {}", root.display()))?;
    let requested = root.join(&binding.executable_locator);
    let metadata = std::fs::symlink_metadata(&requested).with_context(|| {
        format!(
            "frozen evaluator did not produce judge executable '{}'",
            binding.executable_locator
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("judge build output must be one plain regular file");
    }
    if metadata.len() > binding.maximum_executable_bytes {
        bail!(
            "judge executable is {} bytes, exceeding the frozen {}-byte maximum",
            metadata.len(),
            binding.maximum_executable_bytes
        );
    }
    let canonical_executable = std::fs::canonicalize(&requested)
        .with_context(|| format!("canonicalize judge executable {}", requested.display()))?;
    if !canonical_executable.starts_with(&canonical_root) {
        bail!("judge build output resolves outside the fresh runtime-owned root");
    }
    let bytes = std::fs::read(&canonical_executable)
        .with_context(|| format!("read judge executable {}", canonical_executable.display()))?;
    let executable = crate::object::preserve_object(object_root, &bytes)?;
    Ok(Some(JudgeBuildReceipt {
        schema: "papertiger-mise.judge-build-receipt.v1".to_owned(),
        argv: binding.argv.clone(),
        toolchain_name: binding.toolchain_name.clone(),
        toolchain_version: binding.toolchain_version.clone(),
        toolchain_executable_sha256: binding.toolchain_executable_sha256.0.clone(),
        environment_sha256: sha256(&serde_json::to_vec(environment)?),
        source_tree: result_tree.to_owned(),
        executable_locator: binding.executable_locator.clone(),
        executable,
    }))
}

pub(super) fn require_trial_reservation(
    connection: &Connection,
    intent: &TrialIntent,
    manifest: &CampaignManifest,
) -> Result<()> {
    let rows = reservation_rows_for_use(connection, &intent.campaign_id, &intent.reservation_id)?;
    if rows.is_empty()
        || rows
            .values()
            .any(|(_, status)| *status != BudgetReservationStatus::Reserved)
    {
        bail!(
            "trial requires one active durable budget reservation; run `papertiger-mise budget reserve {} {} --amount trials=1 --amount failures=1 ...` first",
            intent.campaign_id,
            intent.reservation_id
        );
    }
    for (resource, exact) in [(BudgetResource::Trials, 1), (BudgetResource::Failures, 1)] {
        if rows.get(&resource).map(|row| row.0) != Some(exact) {
            bail!("trial reservation must contain exactly one {resource} unit");
        }
    }
    let disclosure = rows
        .get(&BudgetResource::HoldoutDisclosures)
        .map(|row| row.0);
    if intent.tier.starts_with("calibration.") {
        if disclosure.is_some() {
            bail!("calibration trial reservations must not consume holdout disclosures");
        }
    } else if disclosure != Some(1) {
        bail!("holdout trial reservations must contain exactly one disclosure unit");
    }
    let wall = rows
        .get(&BudgetResource::WallTimeMilliseconds)
        .map(|row| row.0)
        .unwrap_or(0);
    if wall == 0 || wall > manifest.execution_limits.maximum_trial_wall_time_ms {
        bail!("trial wall-time reservation exceeds its frozen instantaneous limit");
    }
    let output = rows
        .get(&BudgetResource::ArtifactBytes)
        .map(|row| row.0)
        .unwrap_or(0);
    if output == 0 {
        bail!("trial requires a nonzero retained-artifact reservation");
    }
    Ok(())
}

pub fn execute_workspace_trial(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    spec: &SupervisedTrialSpec,
) -> Result<SupervisedTrialOutcome> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("trial_id", &spec.trial_id)?;
    validate_nonblank("campaign_id", &spec.campaign_id)?;
    validate_nonblank("candidate_id", &spec.candidate_id)?;
    validate_nonblank("reservation_id", &spec.reservation_id)?;
    validate_nonblank("tier", &spec.tier)?;
    let manifest = campaign_manifest(connection, &spec.campaign_id)?;
    require_deterministic_runtime(&manifest)?;
    if manifest.containment != crate::manifest::ContainmentGrade::WorkspaceOnly {
        bail!(
            "the in-process supervisor only produces WorkspaceOnly evidence; stronger grades require an external authenticated executor"
        );
    }
    if let Some(existing) = trial(connection, &spec.trial_id)?
        && existing.status == TrialStatus::Succeeded
    {
        let mut differing_fields = Vec::new();
        if existing.campaign_id != spec.campaign_id {
            differing_fields.push("campaign_id");
        }
        if existing.candidate_id != spec.candidate_id {
            differing_fields.push("candidate_id");
        }
        if existing.reservation_id != spec.reservation_id {
            differing_fields.push("reservation_id");
        }
        if existing.tier != spec.tier {
            differing_fields.push("tier");
        }
        if !differing_fields.is_empty() {
            bail!(
                "successful trial '{}' conflicts with the requested replay identity; differing fields: {}",
                spec.trial_id,
                differing_fields.join(", ")
            );
        }
        return supervised_trial_replay(connection, object_root, spec, &manifest, existing);
    }
    require_workspace_trial_reservation(
        connection,
        &manifest,
        &spec.campaign_id,
        &spec.reservation_id,
        &spec.tier,
    )?;
    let candidate_materialization = materialization_in(connection, &spec.candidate_id)?
        .with_context(|| format!("candidate '{}' has no materialization", spec.candidate_id))?;
    let baseline_candidate_id: String = connection
        .query_row(
            "SELECT candidate_id FROM candidates WHERE campaign_id=?1 AND patch_sha256=?2",
            params![
                spec.campaign_id,
                manifest.calibration.no_op_material_sha256().0
            ],
            |row| row.get(0),
        )
        .context("campaign has no exact no-op baseline candidate")?;
    let baseline_materialization = materialization_in(connection, &baseline_candidate_id)?
        .context("campaign no-op baseline has no materialization")?;
    let working_directory = canonical_or_pending_absolute(
        &Path::new(&candidate_materialization.worktree_locator)
            .join(&manifest.evaluator.working_directory),
    )?;
    let owner_uuid = sha256(
        format!(
            "papertiger-mise.workspace-owner.v1\n{}\n{}\n{}\n{}",
            spec.campaign_id,
            spec.trial_id,
            manifest.generation.outer_judge_executable_sha256.0,
            manifest.evaluator.evaluator_sha256.0,
        )
        .as_bytes(),
    );
    let supervisor_identity = format!(
        "papertiger-mise.workspace-supervisor.v1:{}",
        manifest.generation.outer_judge_executable_sha256.0
    );
    let trial_environment =
        expected_trial_environment(&manifest, &spec.campaign_id, &spec.trial_id)?;
    let intent = TrialIntent {
        trial_id: spec.trial_id.clone(),
        campaign_id: spec.campaign_id.clone(),
        candidate_id: spec.candidate_id.clone(),
        materialization_receipt_sha256: candidate_materialization.receipt_sha256.clone(),
        baseline_materialization_receipt_sha256: baseline_materialization.receipt_sha256.clone(),
        result_tree: candidate_materialization.result_tree.clone(),
        working_directory: working_directory.clone(),
        reservation_id: spec.reservation_id.clone(),
        tier: spec.tier.clone(),
        argv: manifest.evaluator.argv.clone(),
        environment: serde_json::to_value(&trial_environment)?,
        owner_uuid: owner_uuid.clone(),
        supervisor_identity: supervisor_identity.clone(),
    };
    if !record_trial_intent(connection, actor, &intent)? {
        let existing = trial(connection, &spec.trial_id)?.context("replayed trial disappeared")?;
        if existing.status != TrialStatus::Succeeded {
            bail!(
                "trial '{}' already exists in non-replayable status '{}'",
                spec.trial_id,
                existing.status
            );
        }
        return supervised_trial_replay(connection, object_root, spec, &manifest, existing);
    }
    if let Err(mismatch) = verify_runtime_judge_inputs(
        &manifest,
        Path::new(&candidate_materialization.worktree_locator),
        &spec.tier,
    ) {
        record_runtime_integrity_failure(
            connection,
            actor,
            object_root,
            &intent,
            &manifest,
            &mismatch,
            None,
        )?;
        return Err(anyhow::anyhow!(mismatch))
            .context("judge inputs drifted before evaluator launch");
    }
    if let Err(error) = prepare_trial_environment(&manifest, &trial_environment) {
        reconcile_supervisor_failure_with_capture(
            connection,
            actor,
            object_root,
            &intent,
            &SupervisorFailureCapture {
                process_birth_identity: None,
                reason: "environment-preparation-failed",
                detail: &error.to_string(),
                stdout: None,
                stderr: None,
            },
        )?;
        return Err(error).context("prepare frozen evaluator environment");
    }
    let baseline_result_tree = baseline_materialization.result_tree.clone();
    let (fixture_locator, fixture_sha256) = expected_fixture_binding(&manifest, &spec.tier)?;
    let request = DeterministicEvaluatorRequest {
        schema: "papertiger-mise.deterministic-evaluator-request.v1".to_owned(),
        trial_id: spec.trial_id.clone(),
        campaign_id: spec.campaign_id.clone(),
        candidate_id: spec.candidate_id.clone(),
        candidate_result_tree: candidate_materialization.result_tree.clone(),
        baseline_result_tree,
        baseline_working_directory: baseline_materialization.worktree_locator.clone(),
        tier: spec.tier.clone(),
        fixture_locator,
        fixture_sha256: fixture_sha256.clone(),
        evaluator_protocol: manifest.evaluator.protocol.clone(),
        objectives: manifest.objectives.clone(),
    };
    let request_bytes = serde_json::to_vec(&request)?;
    let mut launched = |pid, process_birth_identity: &str| {
        mark_trial_launched(
            connection,
            actor,
            &spec.trial_id,
            &TrialOwnership {
                owner_uuid: owner_uuid.clone(),
                pid,
                process_birth_identity: process_birth_identity.to_owned(),
                supervisor_identity: supervisor_identity.clone(),
            },
        )
    };
    let mut heartbeat = || {
        heartbeat_trial(
            connection,
            actor,
            &spec.trial_id,
            &owner_uuid,
            &supervisor_identity,
        )
    };
    let execution = execute_supervised(
        &manifest.evaluator.argv,
        Path::new(&working_directory),
        &trial_environment,
        &request_bytes,
        manifest.execution_limits.maximum_trial_wall_time_ms,
        manifest.execution_limits.maximum_trial_output_bytes,
        Some(SupervisionHooks {
            launched: &mut launched,
            heartbeat: &mut heartbeat,
        }),
    )?;
    let execution = match execution {
        SupervisedExecutionOutcome::Completed(execution) => execution,
        SupervisedExecutionOutcome::Failed(failure) => {
            reconcile_supervisor_failure_with_capture(
                connection,
                actor,
                object_root,
                &intent,
                &SupervisorFailureCapture {
                    process_birth_identity: failure.process_birth_identity.as_deref(),
                    reason: failure.reason,
                    detail: &failure.detail,
                    stdout: Some(&failure.stdout),
                    stderr: Some(&failure.stderr),
                },
            )?;
            bail!("WorkspaceOnly evaluator failed: {}", failure.reason);
        }
    };
    let stdout = execution.stdout;
    let stderr = execution.stderr;
    let elapsed_ms = execution.elapsed_ms;
    let process_birth_identity = execution.process_birth_identity;
    let execution_capabilities = execution.capabilities;
    if let Err(mismatch) = verify_runtime_judge_inputs(
        &manifest,
        Path::new(&candidate_materialization.worktree_locator),
        &spec.tier,
    ) {
        record_runtime_integrity_failure(
            connection,
            actor,
            object_root,
            &intent,
            &manifest,
            &mismatch,
            Some((&stdout, &stderr)),
        )?;
        return Err(anyhow::anyhow!(mismatch))
            .context("judge inputs drifted during evaluator execution");
    }
    if !stderr.is_empty() {
        reconcile_supervisor_failure_with_capture(
            connection,
            actor,
            object_root,
            &intent,
            &SupervisorFailureCapture {
                process_birth_identity: Some(&process_birth_identity),
                reason: "unexpected-evaluator-stderr",
                detail: "a successful deterministic evaluator wrote to stderr",
                stdout: Some(&stdout),
                stderr: Some(&stderr),
            },
        )?;
        bail!(
            "WorkspaceOnly evaluator wrote to stderr; captured output was retained as failure evidence"
        );
    }
    let evaluator_output: DeterministicEvaluatorOutput = match serde_json::from_slice(&stdout) {
        Ok(output) => output,
        Err(error) => {
            reconcile_supervisor_failure_with_capture(
                connection,
                actor,
                object_root,
                &intent,
                &SupervisorFailureCapture {
                    process_birth_identity: Some(&process_birth_identity),
                    reason: "invalid-evaluator-output",
                    detail: &error.to_string(),
                    stdout: Some(&stdout),
                    stderr: Some(&stderr),
                },
            )?;
            return Err(error).context("parse deterministic evaluator output");
        }
    };
    if evaluator_output.schema != "papertiger-mise.deterministic-evaluator-output.v1"
        || serde_json::to_vec(&evaluator_output)? != stdout
    {
        reconcile_supervisor_failure_with_capture(
            connection,
            actor,
            object_root,
            &intent,
            &SupervisorFailureCapture {
                process_birth_identity: Some(&process_birth_identity),
                reason: "noncanonical-evaluator-output",
                detail: "evaluator output is not canonical typed v1 JSON",
                stdout: Some(&stdout),
                stderr: Some(&stderr),
            },
        )?;
        bail!("WorkspaceOnly evaluator output is not canonical typed v1 JSON");
    }
    let judge_build = match preserve_judge_build(
        &manifest,
        &trial_environment,
        &candidate_materialization.result_tree,
        evaluator_output.judge_build.as_ref(),
        object_root,
    ) {
        Ok(build) => build,
        Err(error) => {
            reconcile_supervisor_failure_with_capture(
                connection,
                actor,
                object_root,
                &intent,
                &SupervisorFailureCapture {
                    process_birth_identity: Some(&process_birth_identity),
                    reason: "judge-build-verification-failed",
                    detail: &error.to_string(),
                    stdout: Some(&stdout),
                    stderr: Some(&stderr),
                },
            )?;
            return Err(error).context("verify frozen evaluator judge build");
        }
    };
    let measured_usage = supervised_success_usage(
        connection,
        &spec.campaign_id,
        &spec.reservation_id,
        &spec.tier,
        elapsed_ms,
        u64::try_from(stdout.len())?.saturating_add(u64::try_from(stderr.len())?),
    )?;
    let environment_sha256 = if manifest.evaluator.rust_build_environment.is_some()
        || manifest.evaluator.judge_build.is_some()
    {
        Some(sha256(&serde_json::to_vec(&trial_environment)?))
    } else {
        None
    };
    let receipt = TrialReceipt {
        schema: if manifest.evaluator.judge_build.is_some() {
            "papertiger-mise.trial-receipt.v3"
        } else if manifest.evaluator.rust_build_environment.is_some() {
            "papertiger-mise.trial-receipt.v2"
        } else {
            "papertiger-mise.trial-receipt.v1"
        }
        .to_owned(),
        environment_sha256,
        judge_build,
        trial_id: spec.trial_id.clone(),
        campaign_id: spec.campaign_id.clone(),
        candidate_id: spec.candidate_id.clone(),
        materialization_receipt_sha256: candidate_materialization.receipt_sha256,
        baseline_materialization_receipt_sha256: baseline_materialization.receipt_sha256,
        result_tree: candidate_materialization.result_tree,
        working_directory,
        tier: spec.tier.clone(),
        owner_uuid,
        supervisor_identity,
        process_birth_identity: process_birth_identity.clone(),
        launcher_sha256: manifest.evaluator.launcher_sha256.0,
        evaluator_sha256: manifest.evaluator.evaluator_sha256.0,
        fixture_sha256,
        protocol: manifest.evaluator.protocol,
        observations: evaluator_output.observations,
        reason_code: evaluator_output.reason_code,
        measured_usage,
        execution_capabilities: Some(execution_capabilities),
    };
    let receipt_object =
        crate::object::preserve_object(object_root, &serde_json::to_vec(&receipt)?)?;
    let classification = match complete_deterministic_trial(
        connection,
        actor,
        object_root,
        &spec.trial_id,
        &TrialCompletion {
            receipt: receipt_object.clone(),
        },
        &TrialCompletionCredential {
            owner_uuid: receipt.owner_uuid,
            supervisor_identity: receipt.supervisor_identity,
            process_birth_identity,
        },
    ) {
        Ok(classification) => classification,
        Err(error) => {
            reconcile_supervisor_failure_with_capture(
                connection,
                actor,
                object_root,
                &intent,
                &SupervisorFailureCapture {
                    process_birth_identity: Some(&receipt.process_birth_identity),
                    reason: "completion-refused",
                    detail: &error.to_string(),
                    stdout: Some(&stdout),
                    stderr: Some(&stderr),
                },
            )?;
            return Err(error).context("complete supervised deterministic trial");
        }
    };
    Ok(SupervisedTrialOutcome {
        trial_id: spec.trial_id.clone(),
        receipt: receipt_object,
        classification,
    })
}

fn supervised_trial_replay(
    connection: &Connection,
    object_root: &Path,
    spec: &SupervisedTrialSpec,
    manifest: &CampaignManifest,
    existing: TrialRecord,
) -> Result<SupervisedTrialOutcome> {
    verify_completed_trial_evidence(connection, object_root, &spec.trial_id, manifest)?;
    let outcome = existing
        .outcome
        .context("successful replay has no outcome")?;
    Ok(SupervisedTrialOutcome {
        trial_id: spec.trial_id.clone(),
        receipt: serde_json::from_value(
            outcome
                .get("receipt")
                .cloned()
                .context("successful replay has no receipt")?,
        )?,
        classification: serde_json::from_value(
            outcome
                .get("classification")
                .cloned()
                .context("successful replay has no classification")?,
        )?,
    })
}

pub(super) fn verify_runtime_judge_inputs(
    manifest: &CampaignManifest,
    candidate_root: &Path,
    tier: &str,
) -> std::result::Result<(), FrozenJudgeInputMismatch> {
    let bindings =
        frozen_judge_input_bindings(manifest, tier).map_err(|error| FrozenJudgeInputMismatch {
            role: "manifest-runtime-binding".to_owned(),
            locator: tier.to_owned(),
            expected_identity: "valid-admitted-runtime-binding".to_owned(),
            observed_identity: format!("invalid:{error:#}"),
        })?;
    for binding in bindings {
        let observed_identity = observe_frozen_judge_input(&binding, candidate_root);
        if observed_identity != binding.expected_identity {
            return Err(FrozenJudgeInputMismatch {
                role: binding.role.to_owned(),
                locator: binding.locator,
                expected_identity: binding.expected_identity,
                observed_identity,
            });
        }
    }
    Ok(())
}

impl std::fmt::Display for FrozenJudgeInputMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "frozen input '{}' at '{}' drifted: expected '{}', observed '{}'",
            self.role, self.locator, self.expected_identity, self.observed_identity
        )
    }
}

impl std::error::Error for FrozenJudgeInputMismatch {}

fn observe_frozen_judge_input(binding: &FrozenJudgeInputBinding, candidate_root: &Path) -> String {
    match binding.kind {
        FrozenJudgeInputKind::CurrentExecutable => match std::env::current_exe() {
            Ok(path) => observe_absolute_runtime_file(&path),
            Err(error) => format!("unavailable-current-executable:{error}"),
        },
        FrozenJudgeInputKind::AbsoluteFile => {
            observe_absolute_runtime_file(Path::new(&binding.locator))
        }
        FrozenJudgeInputKind::CandidateFile => {
            observe_candidate_runtime_file(candidate_root, &binding.locator)
        }
        FrozenJudgeInputKind::CandidateGitTree => {
            let current_tree = match git_text(candidate_root, &["write-tree"]) {
                Ok(tree) => tree,
                Err(error) => return format!("unavailable-worktree:{error:#}"),
            };
            let tree_spec = format!("{}:{}", current_tree.trim(), binding.locator);
            match git_text(candidate_root, &["rev-parse", "--verify", &tree_spec]) {
                Ok(tree) => format!("git-tree:{}", tree.trim()),
                Err(error) => format!("unavailable-git-tree:{error:#}"),
            }
        }
        FrozenJudgeInputKind::UnsupportedWorkspaceFixture => {
            format!("opaque-workspace-fixture:{}", binding.locator)
        }
    }
}

fn observe_absolute_runtime_file(path: &Path) -> String {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return format!("unavailable-metadata:{error}"),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return format!("non-plain-file:{:?}", metadata.file_type());
    }
    let canonical = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => return format!("unavailable-canonical-path:{error}"),
    };
    let locator = match canonical_or_pending_absolute(&canonical) {
        Ok(locator) => locator,
        Err(error) => return format!("invalid-canonical-path:{error:#}"),
    };
    match std::fs::read(&canonical) {
        Ok(bytes) => absolute_file_identity(&locator, &sha256(&bytes)),
        Err(error) => format!("unavailable-bytes:{error}"),
    }
}

fn observe_candidate_runtime_file(root: &Path, relative: &str) -> String {
    let root = match std::fs::canonicalize(root) {
        Ok(root) => root,
        Err(error) => return format!("unavailable-candidate-root:{error}"),
    };
    let lexical = root.join(relative);
    let metadata = match std::fs::symlink_metadata(&lexical) {
        Ok(metadata) => metadata,
        Err(error) => return format!("unavailable-metadata:{error}"),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return format!("non-plain-file:{:?}", metadata.file_type());
    }
    let path = match std::fs::canonicalize(&lexical) {
        Ok(path) => path,
        Err(error) => return format!("unavailable-canonical-path:{error}"),
    };
    if !path.starts_with(&root) {
        return format!("outside-candidate-root:{}", path.display());
    }
    match std::fs::read(path) {
        Ok(bytes) => candidate_file_identity(relative, &sha256(&bytes)),
        Err(error) => format!("unavailable-bytes:{error}"),
    }
}

pub(super) fn record_runtime_integrity_failure(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    intent: &TrialIntent,
    manifest: &CampaignManifest,
    mismatch: &FrozenJudgeInputMismatch,
    capture: Option<(&[u8], &[u8])>,
) -> Result<()> {
    let materialization =
        materialization_by_receipt(connection, &intent.materialization_receipt_sha256)?
            .context("integrity-failed trial materialization disappeared")?;
    let candidate_root = Path::new(&materialization.worktree_locator);
    let (fixture_locator, expected_fixture_sha256) =
        expected_fixture_binding(manifest, &intent.tier)?;
    let observed_evaluator_sha256 =
        observed_runtime_digest(candidate_root, &manifest.evaluator.evaluator_locator);
    let observed_outer_judge_sha256 = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .map(|bytes| sha256(&bytes))
        .unwrap_or_else(|| "0".repeat(64));
    let observed_launcher_sha256 = std::fs::read(&manifest.evaluator.launcher_locator)
        .ok()
        .map(|bytes| sha256(&bytes))
        .unwrap_or_else(|| "0".repeat(64));
    let observed_fixture_sha256 = if fixture_locator.contains("://") {
        // WorkspaceOnly never authenticates opaque fixtures; reaching this
        // branch is itself a protocol mismatch.
        "0".repeat(64)
    } else {
        observed_runtime_digest(candidate_root, &fixture_locator)
    };
    let stdout = capture
        .map(|(stdout, _)| stdout)
        .map(|bytes| crate::object::preserve_object(object_root, bytes))
        .transpose()?;
    let stderr = capture
        .map(|(_, stderr)| stderr)
        .map(|bytes| crate::object::preserve_object(object_root, bytes))
        .transpose()?;
    let evidence = crate::object::preserve_object(
        object_root,
        &serde_json::to_vec(&json!({
            "schema": "papertiger-mise.runtime-input-integrity.v2",
            "trial_id": intent.trial_id,
            "mismatch": mismatch,
            "observed_outer_judge_sha256": observed_outer_judge_sha256,
            "observed_launcher_sha256": observed_launcher_sha256,
            "observed_evaluator_sha256": observed_evaluator_sha256,
            "observed_fixture_sha256": observed_fixture_sha256,
            "stdout": stdout,
            "stderr": stderr,
        }))?,
    )?;
    record_integrity_failure(
        connection,
        actor,
        object_root,
        &intent.trial_id,
        &IntegrityFailure {
            reason_code: "runtime-judge-input-drift".to_owned(),
            expected_outer_judge_sha256: manifest
                .generation
                .outer_judge_executable_sha256
                .0
                .clone(),
            observed_outer_judge_sha256,
            expected_launcher_sha256: manifest.evaluator.launcher_sha256.0.clone(),
            observed_launcher_sha256,
            expected_evaluator_sha256: manifest.evaluator.evaluator_sha256.0.clone(),
            observed_evaluator_sha256,
            expected_fixture_sha256,
            observed_fixture_sha256,
            frozen_input_mismatches: vec![mismatch.clone()],
            evidence,
        },
    )
}

fn observed_runtime_digest(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    std::fs::symlink_metadata(&path)
        .ok()
        .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .and_then(|_| std::fs::read(path).ok())
        .map(|bytes| sha256(&bytes))
        .unwrap_or_else(|| "0".repeat(64))
}

fn supervised_success_usage(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
    tier: &str,
    elapsed_ms: u64,
    _temporary_output_bytes: u64,
) -> Result<Vec<BudgetSettlement>> {
    let rows = reservation_rows_for_use(connection, campaign_id, reservation_id)?;
    let mut usage = Vec::new();
    for resource in rows.keys() {
        let actual_amount = match resource {
            BudgetResource::Trials => 1,
            BudgetResource::Failures => 0,
            BudgetResource::WallTimeMilliseconds => elapsed_ms,
            // WorkspaceOnly cannot observe arbitrary filesystem writes by the
            // evaluator. Charge its full declared disk reservation instead of
            // presenting captured stdout/stderr as measured host-disk use.
            BudgetResource::DiskBytesWritten => rows[resource].0,
            BudgetResource::HoldoutDisclosures if tier.starts_with("calibration.") => {
                bail!("calibration trial must not reserve a holdout disclosure")
            }
            BudgetResource::HoldoutDisclosures => 1,
            BudgetResource::ArtifactBytes => continue,
            other => bail!(
                "WorkspaceOnly supervisor cannot truthfully measure reserved resource '{other}'"
            ),
        };
        usage.push(BudgetSettlement {
            resource: *resource,
            actual_amount,
        });
    }
    Ok(usage)
}

fn require_workspace_trial_reservation(
    connection: &Connection,
    manifest: &CampaignManifest,
    campaign_id: &str,
    reservation_id: &str,
    tier: &str,
) -> Result<()> {
    let rows = reservation_rows_for_use(connection, campaign_id, reservation_id)?;
    if rows.is_empty()
        || rows
            .values()
            .any(|(_, status)| *status != BudgetReservationStatus::Reserved)
    {
        bail!(
            "WorkspaceOnly trial requires one active durable reservation; run `papertiger-mise budget reserve {campaign_id} {reservation_id} --amount trials=1 --amount failures=1 ...` first"
        );
    }
    let expected = if tier.starts_with("calibration.") {
        BTreeSet::from([
            BudgetResource::Trials,
            BudgetResource::Failures,
            BudgetResource::WallTimeMilliseconds,
            BudgetResource::DiskBytesWritten,
            BudgetResource::ArtifactBytes,
        ])
    } else {
        BTreeSet::from([
            BudgetResource::Trials,
            BudgetResource::Failures,
            BudgetResource::HoldoutDisclosures,
            BudgetResource::WallTimeMilliseconds,
            BudgetResource::DiskBytesWritten,
            BudgetResource::ArtifactBytes,
        ])
    };
    if rows.keys().copied().collect::<BTreeSet<_>>() != expected {
        bail!(
            "WorkspaceOnly trial reservation has an unsupported resource shape; recreate it with `papertiger-mise budget reserve {campaign_id} {reservation_id}` and exactly the resources required by tier '{tier}'"
        );
    }
    for resource in &expected {
        if rows.get(resource).map(|row| row.0).unwrap_or(0) == 0 {
            bail!(
                "WorkspaceOnly trial reservation requires nonzero {resource}; recreate it with `papertiger-mise budget reserve {campaign_id} {reservation_id} --amount {resource}=<positive-amount>`"
            );
        }
    }
    let maximum_output = manifest.execution_limits.maximum_trial_output_bytes;
    if rows[&BudgetResource::WallTimeMilliseconds].0
        < manifest.execution_limits.maximum_trial_wall_time_ms
    {
        bail!("WorkspaceOnly wall-time reservation must cover the frozen trial deadline");
    }
    if rows[&BudgetResource::DiskBytesWritten].0 < maximum_output {
        bail!(
            "WorkspaceOnly disk reservation must be at least {maximum_output} bytes to cover the frozen maximum evaluator output"
        );
    }
    let required_artifact_bytes = maximum_output.saturating_add(8_192).saturating_add(
        manifest
            .evaluator
            .judge_build
            .as_ref()
            .map(|build| build.maximum_executable_bytes)
            .unwrap_or(0),
    );
    if rows[&BudgetResource::ArtifactBytes].0 < required_artifact_bytes {
        bail!(
            "WorkspaceOnly artifact reservation must be at least {required_artifact_bytes} bytes to cover evaluator output, receipt overhead, and the frozen judge-executable maximum"
        );
    }
    Ok(())
}

struct SupervisorFailureCapture<'a> {
    process_birth_identity: Option<&'a str>,
    reason: &'a str,
    detail: &'a str,
    stdout: Option<&'a [u8]>,
    stderr: Option<&'a [u8]>,
}

fn reconcile_supervisor_failure_with_capture(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    intent: &TrialIntent,
    failure: &SupervisorFailureCapture<'_>,
) -> Result<()> {
    let stdout = failure
        .stdout
        .map(|bytes| crate::object::preserve_object(object_root, bytes))
        .transpose()?;
    let stderr = failure
        .stderr
        .map(|bytes| crate::object::preserve_object(object_root, bytes))
        .transpose()?;
    let evidence_bytes = serde_json::to_vec(&json!({
        "schema": "papertiger-mise.workspace-supervisor-failure.v1",
        "trial_id": intent.trial_id,
        "reason": failure.reason,
        "detail": failure.detail.chars().take(1024).collect::<String>(),
        "process_birth_identity": failure.process_birth_identity,
        "stdout": stdout,
        "stderr": stderr,
    }))?;
    let evidence = crate::object::preserve_object(object_root, &evidence_bytes)?;
    reconcile_lost_trial(
        connection,
        actor,
        object_root,
        &intent.trial_id,
        &AbsenceProof {
            verifier: "papertiger-mise.workspace-supervisor.v1".to_owned(),
            observed_at: now(),
            supervisor_identity: intent.supervisor_identity.clone(),
            process_birth_identity: failure.process_birth_identity.map(str::to_owned),
            evidence,
        },
    )
}
