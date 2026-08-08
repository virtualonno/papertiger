use std::collections::BTreeMap;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::digest::{sha256, validate_sha256};
use crate::executor::execute_bounded;
use crate::manifest::Sha256Digest;
use crate::object::{
    PreservedObject, indexed_object as artifact, preserve_object, read_verified_json,
    record_indexed_object as record_artifact,
};
use crate::path_identity::portable_absolute;
use crate::store::{begin_mutation, now};
use crate::validation::{validate_actor, validate_bounded_token as validate_token};

pub const DOMAIN_SHADOW_ADAPTER_BINDING_SCHEMA_V1: &str =
    "papertiger-mise.domain-shadow-adapter-binding.v1";
pub const DOMAIN_SHADOW_RECEIPT_SCHEMA_V1: &str = "papertiger-mise.domain-shadow-receipt.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainShadowAdapterBinding {
    pub schema: String,
    pub executable_locator: String,
    pub executable_sha256: Sha256Digest,
    pub argv: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
    pub result_schema: String,
    pub maximum_wall_time_ms: u64,
    pub maximum_output_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainShadowState {
    pub identity_sha256: Sha256Digest,
    /// Domain-owned state identity material. Mise requires an object, hashes its
    /// canonical JSON, and otherwise assigns no meaning to its fields.
    pub state: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainShadowResult {
    pub schema: String,
    pub observation_id: String,
    pub request_sha256: Sha256Digest,
    pub adapter_executable_sha256: Sha256Digest,
    /// Exact domain authority used to admit the read-only observation.
    pub domain_authority: Value,
    /// Exact domain context, including domain-specific non-mutation proof.
    pub domain_context: Value,
    pub state_before: DomainShadowState,
    pub state_after: DomainShadowState,
    /// Domain-owned observation. It is retained exactly and is never treated as
    /// a Mise measurement, classification, nomination, or scheduling input.
    pub observation: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainShadowReceipt {
    pub schema: String,
    pub scope: String,
    pub decision_eligible: bool,
    pub adjudication: String,
    pub binding_sha256: String,
    pub request: PreservedObject,
    pub result: PreservedObject,
    pub observation_id: String,
    pub state_identity_sha256: Sha256Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainShadowRecord {
    pub evidence_id: String,
    pub binding: DomainShadowAdapterBinding,
    pub receipt: DomainShadowReceipt,
    pub result: DomainShadowResult,
    pub recorded_by: String,
    pub recorded_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainShadowOutcome {
    Recorded,
    Existing,
}

#[derive(Debug)]
struct ExecutedDomainShadow {
    request_bytes: Vec<u8>,
    output_bytes: Vec<u8>,
    result: DomainShadowResult,
}

impl DomainShadowAdapterBinding {
    pub fn validate(&self) -> Result<()> {
        self.validate_contract()?;
        validate_existing(
            "domain-shadow adapter executable",
            &self.executable_locator,
            true,
        )?;
        validate_existing(
            "domain-shadow adapter working directory",
            &self.working_directory,
            false,
        )?;
        Ok(())
    }

    fn validate_contract(&self) -> Result<()> {
        if self.schema != DOMAIN_SHADOW_ADAPTER_BINDING_SCHEMA_V1 {
            bail!(
                "domain-shadow adapter binding schema must be '{DOMAIN_SHADOW_ADAPTER_BINDING_SCHEMA_V1}'"
            );
        }
        validate_absolute_syntax("domain-shadow adapter executable", &self.executable_locator)?;
        validate_sha256(
            &self.executable_sha256.0,
            "domain-shadow adapter executable",
        )?;
        validate_absolute_syntax(
            "domain-shadow adapter working directory",
            &self.working_directory,
        )?;
        if self.argv.first() != Some(&self.executable_locator)
            || self
                .argv
                .iter()
                .any(|argument| argument.is_empty() || argument.contains('\0'))
        {
            bail!(
                "domain-shadow adapter argv must begin with its exact executable and contain no empty or NUL arguments"
            );
        }
        validate_token("domain-shadow result schema", &self.result_schema)?;
        if !(10..=86_400_000).contains(&self.maximum_wall_time_ms) {
            bail!("domain-shadow wall-time bound must be between 10 ms and 24 hours");
        }
        if !(1..=64 * 1024 * 1024).contains(&self.maximum_output_bytes) {
            bail!("domain-shadow output bound must be between 1 byte and 64 MiB");
        }
        for (key, value) in &self.environment {
            if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
                bail!("domain-shadow environment must contain exact portable entries");
            }
        }
        Ok(())
    }
}

pub fn record_domain_shadow(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    binding: &DomainShadowAdapterBinding,
    domain_request: &Value,
) -> Result<(DomainShadowOutcome, DomainShadowRecord)> {
    validate_actor("domain-shadow actor", actor)?;
    let executed = execute_domain_shadow(binding, domain_request)?;
    let binding_bytes = serde_json::to_vec(binding)?;
    let binding_sha256 = sha256(&binding_bytes);
    let request = preserve_object(object_root, &executed.request_bytes)?;
    let result = preserve_object(object_root, &executed.output_bytes)?;
    let receipt = DomainShadowReceipt {
        schema: DOMAIN_SHADOW_RECEIPT_SCHEMA_V1.to_owned(),
        scope: "domain_shadow".to_owned(),
        decision_eligible: false,
        adjudication: "prohibited".to_owned(),
        binding_sha256: binding_sha256.clone(),
        request: request.clone(),
        result: result.clone(),
        observation_id: executed.result.observation_id.clone(),
        state_identity_sha256: executed.result.state_before.identity_sha256.clone(),
    };
    let receipt_object = preserve_object(object_root, &serde_json::to_vec(&receipt)?)?;
    let evidence_id = receipt_object.sha256.clone();

    let transaction = begin_mutation(connection)?;
    if let Some(existing) = transaction
        .query_row(
            "SELECT receipt_sha256 FROM domain_shadows WHERE evidence_id=?1",
            params![evidence_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        if existing != receipt_object.sha256 {
            bail!("domain-shadow evidence identity conflicts with its durable receipt");
        }
        transaction.commit()?;
        let record = domain_shadow(connection, object_root, &evidence_id)?
            .context("domain shadow disappeared after idempotent replay")?;
        return Ok((DomainShadowOutcome::Existing, record));
    }
    record_artifact(&transaction, &request, "application/json")?;
    record_artifact(&transaction, &result, "application/json")?;
    record_artifact(&transaction, &receipt_object, "application/json")?;
    transaction.execute(
        "INSERT INTO domain_shadows
         (evidence_id, binding_sha256, binding_json, request_sha256,
          result_sha256, receipt_sha256, state_before_sha256, state_after_sha256,
          decision_eligible, adjudication, recorded_by, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 'prohibited', ?9, ?10)",
        params![
            evidence_id,
            binding_sha256,
            String::from_utf8(binding_bytes)?,
            request.sha256,
            result.sha256,
            receipt_object.sha256,
            executed.result.state_before.identity_sha256.0,
            executed.result.state_after.identity_sha256.0,
            actor,
            now(),
        ],
    )?;
    transaction.commit()?;
    let record = domain_shadow(connection, object_root, &evidence_id)?
        .context("domain shadow disappeared after recording")?;
    Ok((DomainShadowOutcome::Recorded, record))
}

pub fn domain_shadow(
    connection: &Connection,
    object_root: &Path,
    evidence_id: &str,
) -> Result<Option<DomainShadowRecord>> {
    let row = connection
        .query_row(
            "SELECT binding_sha256, binding_json, request_sha256, result_sha256,
                    receipt_sha256, state_before_sha256, state_after_sha256,
                    decision_eligible, adjudication, recorded_by, recorded_at
             FROM domain_shadows WHERE evidence_id=?1",
            params![evidence_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        binding_sha256,
        binding_json,
        request_sha256,
        result_sha256,
        receipt_sha256,
        state_before_sha256,
        state_after_sha256,
        decision_eligible,
        adjudication,
        recorded_by,
        recorded_at,
    )) = row
    else {
        return Ok(None);
    };
    if evidence_id != receipt_sha256
        || decision_eligible != 0
        || adjudication != "prohibited"
        || state_before_sha256 != state_after_sha256
        || sha256(binding_json.as_bytes()) != binding_sha256
    {
        bail!("domain-shadow durable identity failed closed");
    }
    let binding: DomainShadowAdapterBinding = serde_json::from_str(&binding_json)?;
    if serde_json::to_vec(&binding)? != binding_json.as_bytes() {
        bail!("domain-shadow durable binding is not canonical JSON");
    }
    binding.validate_contract()?;
    let request = artifact(connection, &request_sha256)?;
    let result_object = artifact(connection, &result_sha256)?;
    let receipt_object = artifact(connection, &receipt_sha256)?;
    let (request_value, request_bytes): (Value, Vec<u8>) =
        read_verified_json(object_root, &request, "domain-shadow request")?;
    reject_decision_input(&request_value)?;
    let (result, _): (DomainShadowResult, Vec<u8>) =
        read_verified_json(object_root, &result_object, "domain-shadow result")?;
    let (receipt, receipt_bytes): (DomainShadowReceipt, Vec<u8>) =
        read_verified_json(object_root, &receipt_object, "domain-shadow receipt")?;
    validate_domain_shadow_result(&binding, &result, &sha256(&request_bytes))?;
    if receipt.request != request
        || receipt.result != result_object
        || receipt.binding_sha256 != binding_sha256
        || receipt.observation_id != result.observation_id
        || receipt.state_identity_sha256 != result.state_before.identity_sha256
        || receipt.decision_eligible
        || receipt.scope != "domain_shadow"
        || receipt.adjudication != "prohibited"
        || sha256(&receipt_bytes) != evidence_id
        || state_before_sha256 != result.state_before.identity_sha256.0
    {
        bail!("domain-shadow CAS receipt failed exact replay verification");
    }
    Ok(Some(DomainShadowRecord {
        evidence_id: evidence_id.to_owned(),
        binding,
        receipt,
        result,
        recorded_by,
        recorded_at,
    }))
}

fn execute_domain_shadow(
    binding: &DomainShadowAdapterBinding,
    request: &Value,
) -> Result<ExecutedDomainShadow> {
    binding.validate()?;
    reject_decision_input(request)?;
    let request_bytes = serde_json::to_vec(request)?;
    let executable = Path::new(&binding.executable_locator);
    let before = hash_plain_file(executable, "domain-shadow adapter executable")?;
    if before != binding.executable_sha256.0 {
        bail!("domain-shadow adapter executable differs from its frozen binding before launch");
    }
    let working_directory = std::fs::canonicalize(&binding.working_directory)
        .context("canonicalize domain-shadow adapter working directory")?;
    if portable_absolute(&working_directory)? != binding.working_directory {
        bail!("domain-shadow adapter working directory differs from its canonical binding");
    }
    let execution = execute_bounded(
        &binding.argv,
        &working_directory,
        &binding.environment,
        &request_bytes,
        binding.maximum_wall_time_ms,
        binding.maximum_output_bytes,
    )?;
    if hash_plain_file(executable, "domain-shadow adapter executable")? != before {
        bail!("domain-shadow adapter executable changed during invocation");
    }
    if !execution.stderr.is_empty() {
        bail!("successful domain-shadow adapter wrote to stderr");
    }
    let output_bytes = strip_one_line_ending(&execution.stdout)?;
    let value: Value =
        serde_json::from_slice(output_bytes).context("parse domain-shadow result")?;
    if serde_json::to_vec(&value)? != output_bytes {
        bail!("domain-shadow adapter result is not canonical compact JSON");
    }
    let result: DomainShadowResult =
        serde_json::from_value(value).context("type domain-shadow adapter result")?;
    validate_domain_shadow_result(binding, &result, &sha256(&request_bytes))?;
    Ok(ExecutedDomainShadow {
        request_bytes,
        output_bytes: output_bytes.to_vec(),
        result,
    })
}

fn validate_domain_shadow_result(
    binding: &DomainShadowAdapterBinding,
    result: &DomainShadowResult,
    request_sha256: &str,
) -> Result<()> {
    if result.schema != binding.result_schema
        || result.request_sha256.0 != request_sha256
        || result.adapter_executable_sha256 != binding.executable_sha256
    {
        bail!("domain-shadow result differs from its frozen adapter, protocol, or exact request");
    }
    validate_token("domain-shadow observation id", &result.observation_id)?;
    if !result.domain_authority.is_object()
        || !result.domain_context.is_object()
        || !result.state_before.state.is_object()
        || !result.state_after.state.is_object()
        || !result.observation.is_object()
    {
        bail!("domain-shadow authority, context, states, and observation must be objects");
    }
    for (label, state) in [
        ("before", &result.state_before),
        ("after", &result.state_after),
    ] {
        validate_sha256(
            &state.identity_sha256.0,
            &format!("domain-shadow {label} state identity"),
        )?;
        if sha256(&serde_json::to_vec(&state.state)?) != state.identity_sha256.0 {
            bail!("domain-shadow {label} state identity does not hash its exact state object");
        }
    }
    if result.state_before.identity_sha256 != result.state_after.identity_sha256 {
        bail!("domain-shadow observation changed the underlying state identity");
    }
    Ok(())
}

fn reject_decision_input(value: &Value) -> Result<()> {
    const PROHIBITED: &[&str] = &[
        "adjudication",
        "classification",
        "decision_eligible",
        "disposition",
        "measurements",
        "nomination",
        "schedule_authority",
    ];
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                if PROHIBITED.contains(&key.as_str()) {
                    bail!("domain-shadow request may not contain caller-supplied '{key}'");
                }
                reject_decision_input(nested)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_decision_input(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_absolute_syntax(field: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    if !path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
    {
        bail!("{field} must be a normalized absolute local path");
    }
    Ok(())
}

fn validate_existing(field: &str, value: &str, file: bool) -> Result<()> {
    validate_absolute_syntax(field, value)?;
    let path = Path::new(value);
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("inspect {field} {value}"))?;
    if (file && (!metadata.is_file() || metadata.file_type().is_symlink()))
        || (!file && !metadata.is_dir())
    {
        bail!("{field} has the wrong filesystem type");
    }
    Ok(())
}

fn hash_plain_file(path: &Path, role: &str) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect {role} {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("{role} must be a plain file");
    }
    Ok(sha256(&std::fs::read(path)?))
}

fn strip_one_line_ending(stdout: &[u8]) -> Result<&[u8]> {
    let stripped = stdout
        .strip_suffix(b"\r\n")
        .or_else(|| stdout.strip_suffix(b"\n"))
        .unwrap_or(stdout);
    if stripped.is_empty() || stripped.contains(&b'\n') || stripped.contains(&b'\r') {
        bail!("domain-shadow adapter must emit exactly one compact JSON document");
    }
    Ok(stripped)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::store::init;

    fn digest(value: &Value) -> Sha256Digest {
        Sha256Digest(sha256(&serde_json::to_vec(value).expect("state JSON")))
    }

    #[test]
    fn result_requires_exactly_unchanged_hashed_state() {
        let state = json!({"database": {"bytes": 7}, "program": {"epoch": 3}});
        let binding = DomainShadowAdapterBinding {
            schema: DOMAIN_SHADOW_ADAPTER_BINDING_SCHEMA_V1.to_owned(),
            executable_locator: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .replace('\\', "/"),
            executable_sha256: Sha256Digest("a".repeat(64)),
            argv: vec![],
            working_directory: std::env::current_dir()
                .expect("cwd")
                .to_string_lossy()
                .replace('\\', "/"),
            environment: BTreeMap::new(),
            result_schema: "fixture.result.v1".to_owned(),
            maximum_wall_time_ms: 100,
            maximum_output_bytes: 1024,
        };
        let mut result = DomainShadowResult {
            schema: binding.result_schema.clone(),
            observation_id: "fixture-observation".to_owned(),
            request_sha256: Sha256Digest("b".repeat(64)),
            adapter_executable_sha256: binding.executable_sha256.clone(),
            domain_authority: json!({}),
            domain_context: json!({}),
            state_before: DomainShadowState {
                identity_sha256: digest(&state),
                state: state.clone(),
            },
            state_after: DomainShadowState {
                identity_sha256: digest(&state),
                state,
            },
            observation: json!({}),
        };
        validate_domain_shadow_result(&binding, &result, &"b".repeat(64)).expect("unchanged state");
        result.state_after.state["program"]["epoch"] = json!(4);
        assert!(
            validate_domain_shadow_result(&binding, &result, &"b".repeat(64))
                .expect_err("state drift")
                .to_string()
                .contains("does not hash")
        );
    }

    #[test]
    fn caller_cannot_supply_decision_material() {
        assert!(reject_decision_input(&json!({"decision_eligible": true})).is_err());
        assert!(reject_decision_input(&json!({"query": {"classification": "win"}})).is_err());
        reject_decision_input(&json!({"query": "read-only"})).expect("observation request");
    }

    #[test]
    fn frozen_adapter_result_and_unchanged_state_reopen_from_cas() {
        let temporary = tempdir().expect("domain shadow fixture root");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/domain_shadow_adapter.rs");
        let executable = temporary.path().join(format!(
            "domain-shadow-fixture{}",
            std::env::consts::EXE_SUFFIX
        ));
        let compilation = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .expect("launch rustc for domain shadow fixture");
        assert!(
            compilation.status.success(),
            "fixture compilation failed: {}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        let executable = portable_absolute(
            &std::fs::canonicalize(&executable).expect("canonical fixture executable"),
        )
        .expect("portable fixture executable");
        let working_directory = portable_absolute(
            &std::fs::canonicalize(temporary.path()).expect("canonical fixture root"),
        )
        .expect("portable fixture root");
        let executable_sha256 = Sha256Digest(
            hash_plain_file(Path::new(&executable), "fixture executable").expect("fixture hash"),
        );
        let request = json!({
            "schema": "papertiger-mise.domain-shadow-fixture-request.v1",
            "target": "fixture"
        });
        let request_sha256 = Sha256Digest(sha256(
            &serde_json::to_vec(&request).expect("fixture request JSON"),
        ));
        let state = json!({
            "authority": "fixture",
            "database_identity": "unchanged",
            "program_epoch": 7
        });
        let state_identity = digest(&state);
        let result = DomainShadowResult {
            schema: "papertiger-mise.domain-shadow-fixture-result.v1".to_owned(),
            observation_id: "deterministic-domain-shadow".to_owned(),
            request_sha256,
            adapter_executable_sha256: executable_sha256.clone(),
            domain_authority: json!({"fixture": "deterministic"}),
            domain_context: json!({"operation": "read_only_probe"}),
            state_before: DomainShadowState {
                identity_sha256: state_identity.clone(),
                state: state.clone(),
            },
            state_after: DomainShadowState {
                identity_sha256: state_identity,
                state,
            },
            observation: json!({"result": "retained_only"}),
        };
        let mut environment = BTreeMap::new();
        environment.insert(
            "PAPERTIGER_MISE_DOMAIN_SHADOW_FIXTURE_RESULT".to_owned(),
            String::from_utf8(
                serde_json::to_vec(&serde_json::to_value(&result).expect("fixture result value"))
                    .expect("fixture result JSON"),
            )
            .expect("fixture result UTF-8"),
        );
        let binding = DomainShadowAdapterBinding {
            schema: DOMAIN_SHADOW_ADAPTER_BINDING_SCHEMA_V1.to_owned(),
            executable_locator: executable.clone(),
            executable_sha256,
            argv: vec![executable],
            working_directory,
            environment,
            result_schema: result.schema.clone(),
            maximum_wall_time_ms: 5_000,
            maximum_output_bytes: 64 * 1024,
        };
        let connection = Connection::open_in_memory().expect("Mise authority");
        init(&connection).expect("initialize Mise authority");
        let objects = temporary.path().join("objects");
        let (outcome, record) =
            record_domain_shadow(&connection, "fixture", &objects, &binding, &request)
                .expect("record domain shadow");
        assert_eq!(outcome, DomainShadowOutcome::Recorded);
        assert!(!record.receipt.decision_eligible);
        assert_eq!(record.receipt.adjudication, "prohibited");
        assert_eq!(
            record.result.state_before.identity_sha256,
            record.result.state_after.identity_sha256
        );
        let (replay, replayed) =
            record_domain_shadow(&connection, "fixture", &objects, &binding, &request)
                .expect("replay domain shadow");
        assert_eq!(replay, DomainShadowOutcome::Existing);
        assert_eq!(replayed, record);
        std::fs::remove_file(&binding.executable_locator).expect("remove fixture executable");
        let reopened = domain_shadow(&connection, &objects, &record.evidence_id)
            .expect("reopen domain shadow after adapter removal")
            .expect("domain shadow record");
        assert_eq!(reopened, record);
    }
}
