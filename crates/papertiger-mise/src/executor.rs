use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::process_identity::{ProcessObservation, observe_process};

#[derive(Clone, Debug)]
pub(crate) struct ExecutionOutput {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) elapsed_ms: u64,
    pub(crate) process_birth_identity: String,
    pub(crate) capabilities: ExecutionCapabilities,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutionFailure {
    pub(crate) reason: &'static str,
    pub(crate) detail: String,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) elapsed_ms: u64,
    pub(crate) process_birth_identity: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum SupervisedExecutionOutcome {
    Completed(ExecutionOutput),
    Failed(ExecutionFailure),
}

pub(crate) struct SupervisionHooks<'a> {
    pub(crate) launched: &'a mut dyn FnMut(u32, &str) -> Result<()>,
    pub(crate) heartbeat: &'a mut dyn FnMut() -> Result<()>,
}

/// Historical trial-receipt shape. These fields are backend diagnostics only:
/// they never satisfy campaign admission or strengthen decision authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_contract: Option<String>,
    pub platform: String,
    pub process_family: String,
    pub aggregate_process_limit: bool,
    pub aggregate_memory_limit: bool,
}

pub const PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1: &str =
    "papertiger-mise.portable-local-supervision.v1";
pub const HOST_EXECUTION_STATUS_SCHEMA_V1: &str = "papertiger-mise.host-execution-status.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostExecutionStatus {
    pub schema: String,
    pub portable_contract: String,
    pub portable_local_supervision: bool,
    pub adversarial_isolation: bool,
    pub cold_recovery_birth_identity: bool,
    pub platform_diagnostic: String,
    pub native_cleanup_backend_diagnostic: String,
}

#[cfg(windows)]
pub fn host_execution_status() -> Result<HostExecutionStatus> {
    Ok(HostExecutionStatus {
        schema: HOST_EXECUTION_STATUS_SCHEMA_V1.to_owned(),
        portable_contract: PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1.to_owned(),
        portable_local_supervision: true,
        adversarial_isolation: false,
        cold_recovery_birth_identity: true,
        platform_diagnostic: "windows".to_owned(),
        native_cleanup_backend_diagnostic: "windows_job_post_spawn.v1".to_owned(),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn host_execution_status() -> Result<HostExecutionStatus> {
    Ok(HostExecutionStatus {
        schema: HOST_EXECUTION_STATUS_SCHEMA_V1.to_owned(),
        portable_contract: PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1.to_owned(),
        portable_local_supervision: true,
        adversarial_isolation: false,
        cold_recovery_birth_identity: true,
        platform_diagnostic: std::env::consts::OS.to_owned(),
        native_cleanup_backend_diagnostic: "posix_process_group_at_spawn.v1".to_owned(),
    })
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn host_execution_status() -> Result<HostExecutionStatus> {
    bail!(
        "portable local supervision is unsupported on {}; use Windows, Linux, or macOS",
        std::env::consts::OS
    )
}

const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn execute_bounded(
    argv: &[String],
    working_directory: &Path,
    environment: &BTreeMap<String, String>,
    input: &[u8],
    maximum_wall_time_ms: u64,
    maximum_output_bytes: u64,
) -> Result<ExecutionOutput> {
    match execute_supervised(
        argv,
        working_directory,
        environment,
        input,
        maximum_wall_time_ms,
        maximum_output_bytes,
        None,
    )? {
        SupervisedExecutionOutcome::Completed(output) => Ok(output),
        SupervisedExecutionOutcome::Failed(failure) => bail!(
            "adapter supervision failed after {} ms: {}; {}",
            failure.elapsed_ms,
            failure.reason,
            failure.detail
        ),
    }
}

pub(crate) fn execute_supervised(
    argv: &[String],
    working_directory: &Path,
    environment: &BTreeMap<String, String>,
    input: &[u8],
    maximum_wall_time_ms: u64,
    maximum_output_bytes: u64,
    mut hooks: Option<SupervisionHooks<'_>>,
) -> Result<SupervisedExecutionOutcome> {
    let executable = argv.first().context("executor argv has no executable")?;
    let mut process_family = ProcessFamilySupervisor::new()?;
    let capabilities = process_family.capabilities().clone();
    let mut command = Command::new(executable);
    command
        .args(&argv[1..])
        .current_dir(working_directory)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process_family.prepare_command(&mut command)?;
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Ok(SupervisedExecutionOutcome::Failed(ExecutionFailure {
                reason: "spawn-failed",
                detail: format!("{error:#}"),
                stdout: Vec::new(),
                stderr: Vec::new(),
                elapsed_ms: elapsed_ms(started),
                process_birth_identity: None,
            }));
        }
    };
    // `std::process::Command` does not expose a suspended child thread, so the
    // Job assignment necessarily follows process creation. Once assigned, all
    // subsequently created descendants inherit the Job. This bounded executor
    // does not claim that a hostile child cannot escape during the small
    // pre-assignment interval; live supervision evidence stays unavailable
    // until a suspended-create/assign/resume boundary is implemented.
    if let Err(error) = process_family.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(SupervisedExecutionOutcome::Failed(ExecutionFailure {
            reason: "process-family-assignment-failed",
            detail: format!("{error:#}"),
            stdout: Vec::new(),
            stderr: Vec::new(),
            elapsed_ms: elapsed_ms(started),
            process_birth_identity: None,
        }));
    }
    // Adapter stdin is the executor-owned admission gate. A conforming adapter
    // cannot evaluate or exit successfully until its request arrives, so bind
    // the OS birth identity and durable launch hook before releasing any input.
    // This removes the fast-process observation race without weakening cold
    // recovery identity or relying on evaluator-specific sleeps.
    let process_birth_identity = match observe_process(child.id()) {
        Ok(ProcessObservation::Active {
            process_birth_identity,
        })
        | Ok(ProcessObservation::Exited {
            process_birth_identity,
        }) => process_birth_identity,
        Ok(ProcessObservation::Absent) => {
            terminate(&mut child, &process_family)?;
            return Ok(SupervisedExecutionOutcome::Failed(ExecutionFailure {
                reason: "process-identity-unavailable",
                detail:
                    "spawned process disappeared before its OS birth identity could be observed"
                        .to_owned(),
                stdout: Vec::new(),
                stderr: Vec::new(),
                elapsed_ms: elapsed_ms(started),
                process_birth_identity: None,
            }));
        }
        Err(error) => {
            terminate(&mut child, &process_family)?;
            return Ok(SupervisedExecutionOutcome::Failed(ExecutionFailure {
                reason: "process-identity-unavailable",
                detail: format!("{error:#}"),
                stdout: Vec::new(),
                stderr: Vec::new(),
                elapsed_ms: elapsed_ms(started),
                process_birth_identity: None,
            }));
        }
    };
    if let Some(hooks) = hooks.as_mut()
        && let Err(error) = (hooks.launched)(child.id(), &process_birth_identity)
    {
        terminate(&mut child, &process_family)?;
        return Ok(SupervisedExecutionOutcome::Failed(ExecutionFailure {
            reason: "launch-binding-failed",
            detail: format!("{error:#}"),
            stdout: Vec::new(),
            stderr: Vec::new(),
            elapsed_ms: elapsed_ms(started),
            process_birth_identity: Some(process_birth_identity),
        }));
    }
    let bytes_seen = Arc::new(AtomicU64::new(0));
    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout = capture(
        child.stdout.take().context("adapter stdout pipe missing")?,
        maximum_output_bytes,
        Arc::clone(&bytes_seen),
        Arc::clone(&output_exceeded),
    );
    let stderr = capture(
        child.stderr.take().context("adapter stderr pipe missing")?,
        maximum_output_bytes,
        Arc::clone(&bytes_seen),
        Arc::clone(&output_exceeded),
    );
    let input_result = write_input(
        child.stdin.take().context("adapter stdin pipe missing")?,
        input.to_vec(),
    );
    let deadline = Duration::from_millis(maximum_wall_time_ms);
    let mut last_heartbeat = Instant::now();
    let (status, mut failure) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), None);
        }
        if output_exceeded.load(Ordering::SeqCst) {
            terminate(&mut child, &process_family)?;
            break (
                None,
                Some((
                    "output-limit-exceeded",
                    "combined output exceeded its frozen bound".to_owned(),
                )),
            );
        }
        if started.elapsed() >= deadline {
            terminate(&mut child, &process_family)?;
            break (
                None,
                Some((
                    "wall-time-limit-exceeded",
                    "execution exceeded its frozen wall-time bound".to_owned(),
                )),
            );
        }
        if last_heartbeat.elapsed() >= Duration::from_millis(500) {
            if let Some(hooks) = hooks.as_mut()
                && let Err(error) = (hooks.heartbeat)()
            {
                terminate(&mut child, &process_family)?;
                break (None, Some(("heartbeat-failed", format!("{error:#}"))));
            }
            last_heartbeat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // A successful root process is not allowed to leave background work or
    // inherited pipe handles behind. Terminating the Job after root exit is a
    // no-op for an empty tree and closes every assigned descendant otherwise.
    if let Err(error) = process_family.terminate() {
        failure.get_or_insert(("process-family-quiescence-failed", format!("{error:#}")));
    }
    if let Err(error) = receive_input(input_result, OUTPUT_DRAIN_TIMEOUT) {
        failure.get_or_insert(("input-worker-failed", format!("{error:#}")));
    }
    let stdout = match receive_capture(stdout, "stdout", OUTPUT_DRAIN_TIMEOUT) {
        Ok(bytes) => bytes,
        Err(error) => {
            failure.get_or_insert(("inherited-output-handle", format!("{error:#}")));
            Vec::new()
        }
    };
    let stderr = match receive_capture(stderr, "stderr", OUTPUT_DRAIN_TIMEOUT) {
        Ok(bytes) => bytes,
        Err(error) => {
            failure.get_or_insert(("inherited-output-handle", format!("{error:#}")));
            Vec::new()
        }
    };
    if output_exceeded.load(Ordering::SeqCst) {
        failure.get_or_insert((
            "output-limit-exceeded",
            "combined output exceeded its frozen bound".to_owned(),
        ));
    }
    if let Some(status) = status.as_ref().filter(|status| !status.success()) {
        failure.get_or_insert((
            "process-exit-failed",
            format!(
                "process exited with {status}; stderr={}",
                String::from_utf8_lossy(&stderr)
            ),
        ));
    }
    let elapsed_ms = elapsed_ms(started);
    if let Some((reason, detail)) = failure {
        return Ok(SupervisedExecutionOutcome::Failed(ExecutionFailure {
            reason,
            detail,
            stdout,
            stderr,
            elapsed_ms,
            process_birth_identity: Some(process_birth_identity),
        }));
    }
    Ok(SupervisedExecutionOutcome::Completed(ExecutionOutput {
        stdout,
        stderr,
        elapsed_ms,
        process_birth_identity,
        capabilities,
    }))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

fn capture<R: Read + Send + 'static>(
    mut reader: R,
    maximum: u64,
    bytes_seen: Arc<AtomicU64>,
    exceeded: Arc<AtomicBool>,
) -> Receiver<Result<Vec<u8>>> {
    let (sender, receiver) = sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| {
            let mut retained = Vec::new();
            let mut buffer = [0_u8; 8192];
            let retention_limit = maximum.saturating_add(1);
            loop {
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                let read_u64 = u64::try_from(read)?;
                let retained_read = loop {
                    let current = bytes_seen.load(Ordering::SeqCst);
                    if current >= retention_limit {
                        break 0;
                    }
                    let allowed = read_u64.min(retention_limit - current);
                    if bytes_seen
                        .compare_exchange(
                            current,
                            current + allowed,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        break allowed;
                    }
                };
                retained.extend_from_slice(&buffer[..usize::try_from(retained_read)?]);
                if retained_read < read_u64 || bytes_seen.load(Ordering::SeqCst) > maximum {
                    exceeded.store(true, Ordering::SeqCst);
                    break;
                }
            }
            Ok(retained)
        })();
        let _ = sender.send(result);
    });
    receiver
}

fn write_input(mut stdin: std::process::ChildStdin, input: Vec<u8>) -> Receiver<Result<()>> {
    let (sender, receiver) = sync_channel(1);
    std::thread::spawn(move || {
        let result = stdin.write_all(&input).context("write adapter request");
        drop(stdin);
        let _ = sender.send(result);
    });
    receiver
}

fn receive_input(receiver: Receiver<Result<()>>, timeout: Duration) -> Result<()> {
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            bail!("adapter stdin worker retained an inherited pipe handle after tree termination")
        }
        Err(RecvTimeoutError::Disconnected) => bail!("adapter stdin worker disconnected"),
    }
}

fn receive_capture(
    receiver: Receiver<Result<Vec<u8>>>,
    stream: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => bail!(
            "adapter {stream} worker retained an inherited output handle after tree termination"
        ),
        Err(RecvTimeoutError::Disconnected) => {
            bail!("adapter {stream} worker disconnected")
        }
    }
}

fn terminate(
    child: &mut std::process::Child,
    process_family: &ProcessFamilySupervisor,
) -> Result<()> {
    // Reap a root that has already exited before addressing its process group.
    // On macOS an unreaped group leader can make kill(-pgid, SIGKILL) report
    // EPERM even when the group has no live descendants. Reaping first turns
    // that case into ESRCH, while a surviving descendant keeps the dedicated
    // group alive and is still terminated below.
    #[cfg(not(windows))]
    let root_already_exited = child.try_wait()?.is_some();
    process_family
        .terminate()
        .context("terminate adapter native cleanup family")?;
    #[cfg(not(windows))]
    if !root_already_exited && child.try_wait()?.is_none() {
        child
            .kill()
            .context("terminate owned adapter root process")?;
    }
    if child.try_wait()?.is_none() {
        child
            .wait()
            .context("wait for terminated adapter root process")?;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) struct ProcessFamilySupervisor {
    job: windows_sys::Win32::Foundation::HANDLE,
    capabilities: ExecutionCapabilities,
}

#[cfg(windows)]
impl ProcessFamilySupervisor {
    pub(crate) fn new() -> Result<Self> {
        use std::mem::size_of;
        use std::ptr;

        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        // SAFETY: null security attributes and name request one unnamed Job;
        // the returned handle is checked and exclusively owned by `Self`.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error()).context("create adapter Job Object");
        }
        let tree = Self {
            job,
            capabilities: ExecutionCapabilities {
                portable_contract: Some(PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1.to_owned()),
                platform: "windows".to_owned(),
                process_family: "windows_job_post_spawn.v1".to_owned(),
                aggregate_process_limit: false,
                aggregate_memory_limit: false,
            },
        };
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let information_bytes = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .context("adapter Job limit structure size does not fit u32")?;
        // SAFETY: `tree.job` is a live Job handle and `information` is a
        // correctly sized, initialized extended-limit structure.
        if unsafe {
            SetInformationJobObject(
                tree.job,
                JobObjectExtendedLimitInformation,
                (&raw const information).cast(),
                information_bytes,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("configure adapter Job limits");
        }
        Ok(tree)
    }

    pub(crate) fn prepare_command(&mut self, _command: &mut Command) -> Result<()> {
        Ok(())
    }

    pub(crate) fn assign(&mut self, child: &std::process::Child) -> Result<()> {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let process = child.as_raw_handle().cast();
        // SAFETY: the Child owns a live process handle and this object owns a
        // live Job handle for the duration of the call.
        if unsafe { AssignProcessToJobObject(self.job, process) } == 0 {
            return Err(std::io::Error::last_os_error()).context("assign adapter root to Job");
        }
        Ok(())
    }

    pub(crate) fn terminate(&self) -> Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: this object exclusively owns a live Job handle. Exit code 1
        // is reserved here for executor-controlled termination.
        if unsafe { TerminateJobObject(self.job, 1) } == 0 {
            return Err(std::io::Error::last_os_error()).context("terminate adapter Job");
        }
        Ok(())
    }

    pub(crate) fn capabilities(&self) -> &ExecutionCapabilities {
        &self.capabilities
    }
}

#[cfg(windows)]
impl Drop for ProcessFamilySupervisor {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        // SAFETY: this object exclusively owns the Job handle. The configured
        // kill-on-close flag makes failure paths retain tree ownership too.
        unsafe {
            CloseHandle(self.job);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct ProcessFamilySupervisor {
    process_group: Option<i32>,
    capabilities: ExecutionCapabilities,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ProcessFamilySupervisor {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            process_group: None,
            capabilities: ExecutionCapabilities {
                portable_contract: Some(PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1.to_owned()),
                platform: std::env::consts::OS.to_owned(),
                process_family: "posix_process_group_at_spawn.v1".to_owned(),
                aggregate_process_limit: false,
                aggregate_memory_limit: false,
            },
        })
    }

    pub(crate) fn prepare_command(&mut self, command: &mut Command) -> Result<()> {
        use std::os::unix::process::CommandExt as _;

        command.process_group(0);
        Ok(())
    }

    pub(crate) fn assign(&mut self, child: &std::process::Child) -> Result<()> {
        self.process_group = Some(i32::try_from(child.id()).context("child PID does not fit i32")?);
        Ok(())
    }

    pub(crate) fn terminate(&self) -> Result<()> {
        let Some(process_group) = self.process_group else {
            return Ok(());
        };
        // SAFETY: a negative PID targets the dedicated child process group.
        let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("terminate owned Unix process group");
            }
        }
        Ok(())
    }

    pub(crate) fn capabilities(&self) -> &ExecutionCapabilities {
        &self.capabilities
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for ProcessFamilySupervisor {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) struct ProcessFamilySupervisor;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl ProcessFamilySupervisor {
    pub(crate) fn new() -> Result<Self> {
        bail!(
            "portable local supervision is unsupported on {}; use Windows, Linux, or macOS",
            std::env::consts::OS
        )
    }

    pub(crate) fn prepare_command(&mut self, _command: &mut Command) -> Result<()> {
        unreachable!()
    }

    pub(crate) fn assign(&mut self, _child: &std::process::Child) -> Result<()> {
        unreachable!()
    }

    pub(crate) fn terminate(&self) -> Result<()> {
        unreachable!()
    }

    pub(crate) fn capabilities(&self) -> &ExecutionCapabilities {
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Read as _;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use std::time::Instant;

    use super::{
        HOST_EXECUTION_STATUS_SCHEMA_V1, PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1,
        SupervisedExecutionOutcome, SupervisionHooks, execute_bounded, execute_supervised,
        host_execution_status,
    };

    const HELPER_ENABLED: &str = "PAPERTIGER_MISE_EXECUTOR_HELPER";
    const HELPER_MODE: &str = "PAPERTIGER_MISE_EXECUTOR_HELPER_MODE";
    const HELPER_PID_FILE: &str = "PAPERTIGER_MISE_EXECUTOR_HELPER_PID_FILE";

    #[test]
    fn host_status_exposes_one_portable_contract_and_only_diagnostic_native_details() {
        let status = host_execution_status().expect("supported test host");
        assert_eq!(status.schema, HOST_EXECUTION_STATUS_SCHEMA_V1);
        assert_eq!(
            status.portable_contract,
            PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1
        );
        assert!(status.portable_local_supervision);
        assert!(!status.adversarial_isolation);
        assert!(status.cold_recovery_birth_identity);
        assert_eq!(status.platform_diagnostic, std::env::consts::OS);
        assert!(!status.native_cleanup_backend_diagnostic.is_empty());
    }

    #[test]
    #[ignore = "internal subprocess fixture"]
    #[allow(clippy::zombie_processes)] // Deliberately prove executor-owned orphan cleanup.
    fn native_cleanup_family_root_helper() {
        if std::env::var_os(HELPER_ENABLED).is_none() {
            return;
        }
        let mut signal = [0_u8; 1];
        std::io::stdin()
            .read_exact(&mut signal)
            .expect("wait for post-assignment executor signal");
        let executable = std::env::current_exe().expect("helper executable");
        let descendant = Command::new(executable)
            .arg("native_cleanup_family_descendant_helper")
            .arg("--ignored")
            .arg("--nocapture")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn process-family descendant");
        fs::write(
            std::env::var_os(HELPER_PID_FILE).expect("helper PID file"),
            descendant.id().to_string(),
        )
        .expect("write descendant PID");
        if std::env::var(HELPER_MODE).as_deref() == Ok("hold-root") {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    #[ignore = "internal subprocess fixture"]
    fn native_cleanup_family_descendant_helper() {
        if std::env::var_os(HELPER_ENABLED).is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn wall_timeout_terminates_adapter_descendants() {
        let directory = tempfile::tempdir().expect("executor fixture directory");
        let pid_file = directory.path().join("descendant.pid");
        let error = execute_helper(directory.path(), &pid_file, "hold-root", 2_000)
            .expect_err("holding native cleanup family must exceed wall time");
        assert!(error.to_string().contains("wall-time bound"), "{error:#}");
        let descendant = read_pid(&pid_file);
        assert_process_exited(descendant);
    }

    #[test]
    fn root_exit_closes_inherited_descendant_output_handles() {
        let directory = tempfile::tempdir().expect("executor fixture directory");
        let pid_file = directory.path().join("descendant.pid");
        let output = execute_helper(directory.path(), &pid_file, "exit-root", 10_000)
            .expect("executor must terminate the output-holding descendant");
        assert!(!output.stdout.is_empty());
        let descendant = read_pid(&pid_file);
        assert_process_exited(descendant);
    }

    #[test]
    fn launch_binding_precedes_adapter_input_release() {
        let directory = tempfile::tempdir().expect("executor fixture directory");
        let pid_file = directory.path().join("descendant.pid");
        let executable = std::env::current_exe().expect("test executable");
        let argv = vec![
            executable.to_string_lossy().into_owned(),
            "native_cleanup_family_root_helper".to_owned(),
            "--ignored".to_owned(),
            "--nocapture".to_owned(),
        ];
        let environment = BTreeMap::from([
            (HELPER_ENABLED.to_owned(), "1".to_owned()),
            (HELPER_MODE.to_owned(), "exit-root".to_owned()),
            (
                HELPER_PID_FILE.to_owned(),
                pid_file.to_string_lossy().into_owned(),
            ),
        ]);
        let mut launch_bound = false;
        let mut launched = |_: u32, _: &str| {
            assert!(
                !pid_file.exists(),
                "adapter consumed stdin before its launch identity was bound"
            );
            launch_bound = true;
            Ok(())
        };
        let mut heartbeat = || Ok(());
        let outcome = execute_supervised(
            &argv,
            directory.path(),
            &environment,
            &[1],
            10_000,
            4_096,
            Some(SupervisionHooks {
                launched: &mut launched,
                heartbeat: &mut heartbeat,
            }),
        )
        .expect("supervised helper");
        assert!(
            matches!(outcome, SupervisedExecutionOutcome::Completed(_)),
            "helper must complete after admission: {outcome:?}"
        );
        assert!(launch_bound);
        let descendant = read_pid(&pid_file);
        assert_process_exited(descendant);
    }

    fn execute_helper(
        working_directory: &Path,
        pid_file: &Path,
        mode: &str,
        wall_time_ms: u64,
    ) -> anyhow::Result<super::ExecutionOutput> {
        let executable = std::env::current_exe()?;
        let argv = vec![
            executable.to_string_lossy().into_owned(),
            "native_cleanup_family_root_helper".to_owned(),
            "--ignored".to_owned(),
            "--nocapture".to_owned(),
        ];
        let environment = BTreeMap::from([
            (HELPER_ENABLED.to_owned(), "1".to_owned()),
            (HELPER_MODE.to_owned(), mode.to_owned()),
            (
                HELPER_PID_FILE.to_owned(),
                pid_file.to_string_lossy().into_owned(),
            ),
        ]);
        execute_bounded(
            &argv,
            working_directory,
            &environment,
            b"1",
            wall_time_ms,
            1024 * 1024,
        )
    }

    fn read_pid(path: &Path) -> u32 {
        fs::read_to_string(path)
            .expect("descendant PID file")
            .parse()
            .expect("descendant PID")
    }

    #[cfg(windows)]
    fn assert_process_exited(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_is_active(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_active(pid),
            "descendant PID {pid} survived Job termination"
        );
    }

    #[cfg(windows)]
    fn process_is_active(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        const STILL_ACTIVE: u32 = 259;
        // SAFETY: the returned process handle is checked and closed; the exit
        // code points to live writable storage.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return false;
            }
            let mut exit_code = 0_u32;
            let active =
                GetExitCodeProcess(process, &raw mut exit_code) != 0 && exit_code == STILL_ACTIVE;
            CloseHandle(process);
            active
        }
    }

    #[cfg(unix)]
    fn assert_process_exited(pid: u32) {
        let pid = i32::try_from(pid).expect("descendant PID fits i32");
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_is_active(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_active(pid),
            "descendant PID {pid} survived process-group termination"
        );
    }

    #[cfg(unix)]
    fn process_is_active(pid: i32) -> bool {
        // SAFETY: signal zero performs an existence/permission query only.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}
