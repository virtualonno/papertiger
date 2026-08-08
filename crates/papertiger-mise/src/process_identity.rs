#[cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::Context;
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum ProcessObservation {
    Active { process_birth_identity: String },
    Exited { process_birth_identity: String },
    Absent,
}

#[cfg(windows)]
pub(crate) fn observe_process(pid: u32) -> Result<ProcessObservation> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, GetLastError,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const STILL_ACTIVE_EXIT_CODE: u32 = 259;

    // SAFETY: the handle is checked before use, all output pointers refer to
    // live stack values, and every successful OpenProcess handle is closed.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            let error = GetLastError();
            if error == ERROR_INVALID_PARAMETER {
                return Ok(ProcessObservation::Absent);
            }
            if error == ERROR_ACCESS_DENIED {
                anyhow::bail!(
                    "access denied while inspecting process {pid}; absence is not proven"
                );
            }
            anyhow::bail!("OpenProcess({pid}) failed with Windows error {error}");
        }
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel_time: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let times_ok = GetProcessTimes(
            handle,
            &mut creation,
            &mut exit,
            &mut kernel_time,
            &mut user,
        );
        let mut exit_code = 0_u32;
        let exit_ok = GetExitCodeProcess(handle, &mut exit_code);
        let query_error = if times_ok == 0 || exit_ok == 0 {
            Some(GetLastError())
        } else {
            None
        };
        let _ = CloseHandle(handle);
        if let Some(error) = query_error {
            anyhow::bail!("process identity query for PID {pid} failed with Windows error {error}");
        }
        let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let process_birth_identity = format!("windows-filetime:{ticks}");
        if exit_code == STILL_ACTIVE_EXIT_CODE {
            Ok(ProcessObservation::Active {
                process_birth_identity,
            })
        } else {
            Ok(ProcessObservation::Exited {
                process_birth_identity,
            })
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn observe_process(pid: u32) -> Result<ProcessObservation> {
    let stat_path = std::path::PathBuf::from(format!("/proc/{pid}/stat"));
    let stat = match std::fs::read_to_string(&stat_path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProcessObservation::Absent);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", stat_path.display()));
        }
    };
    let after_name = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields.trim())
        .context("Linux process stat has no command terminator")?;
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    let state = *fields.first().context("Linux process stat has no state")?;
    let start_ticks = fields
        .get(19)
        .context("Linux process stat has no start-time field")?;
    let process_birth_identity = format!("linux-proc-start:{start_ticks}");
    if state == "Z" || state == "X" {
        Ok(ProcessObservation::Exited {
            process_birth_identity,
        })
    } else {
        Ok(ProcessObservation::Active {
            process_birth_identity,
        })
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn observe_process(pid: u32) -> Result<ProcessObservation> {
    use std::ffi::c_void;
    use std::mem::size_of;

    const PROC_PIDTBSDINFO: i32 = 3;
    const SZOMB: u32 = 5;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [libc::c_char; 16],
        pbi_name: [libc::c_char; 32],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }

    let pid = i32::try_from(pid).context("PID does not fit macOS pid_t")?;
    let mut info: ProcBsdInfo = unsafe { std::mem::zeroed() };
    let expected = i32::try_from(size_of::<ProcBsdInfo>())?;
    // SAFETY: `info` is a correctly sized writable PROC_PIDTBSDINFO buffer.
    let returned =
        unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), expected) };
    if returned <= 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(ProcessObservation::Absent);
        }
        return Err(error).context(format!("proc_pidinfo({pid}) failed"));
    }
    if returned != expected || info.pbi_pid != u32::try_from(pid)? {
        anyhow::bail!("macOS process identity query returned an invalid PROC_PIDTBSDINFO record");
    }
    let process_birth_identity = format!(
        "macos-bsd-start:{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    );
    if info.pbi_status == SZOMB {
        Ok(ProcessObservation::Exited {
            process_birth_identity,
        })
    } else {
        Ok(ProcessObservation::Active {
            process_birth_identity,
        })
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub(crate) fn observe_process(pid: u32) -> Result<ProcessObservation> {
    anyhow::bail!(
        "OS-backed process birth observation is unsupported on {} for PID {pid}",
        std::env::consts::OS
    )
}

#[cfg(all(test, any(windows, target_os = "linux", target_os = "macos")))]
mod tests {
    use super::{ProcessObservation, observe_process};

    #[test]
    fn current_process_has_a_stable_platform_birth_identity() {
        let observation = observe_process(std::process::id()).expect("observe current process");
        let ProcessObservation::Active {
            process_birth_identity,
        } = observation
        else {
            panic!("current test process must be active: {observation:?}");
        };
        assert!(!process_birth_identity.trim().is_empty());
    }
}
