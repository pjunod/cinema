//! Original unnamed Windows Job owner. Self-contained native implementation so
//! its exact FFI source can also be cross-compiled without unrelated C backends.
use std::io;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

pub(crate) struct WindowsJob {
    handle: HANDLE,
    // An ordinary attach-after-execution Job cannot certify descendant coverage.
    pre_execution_contained: bool,
}

// The handle is uniquely owned, never exported and closed only on final Drop.
// Moving the owner between runtime threads does not change its kernel object.
unsafe impl Send for WindowsJob {}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

impl WindowsJob {
    fn create() -> io::Result<Self> {
        // Null security attributes make this handle non-inheritable; a null
        // name prevents reopening it by a later same-name replacement protocol.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self {
            handle,
            pre_execution_contained: false,
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // In particular, do NOT permit BREAKAWAY_OK or SILENT_BREAKAWAY_OK.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(crate) fn attach_pid(pid: u32) -> io::Result<Self> {
        let job = Self::create()?;
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let assigned = unsafe { AssignProcessToJobObject(job.handle, process) };
        // Capture the assignment error BEFORE CloseHandle can change last-error.
        let result = if assigned == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        };
        unsafe { CloseHandle(process) };
        result?;
        Ok(job)
    }

    /// Only the suspended async launcher invokes this, after assignment and
    /// BEFORE resume. No public caller or disk row can upgrade provenance.
    pub(crate) fn mark_pre_execution(&mut self) {
        self.pre_execution_contained = true;
    }

    fn require_pre_execution(&self) -> io::Result<()> {
        if !self.pre_execution_contained {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "original suspended-entry Job unavailable",
            ));
        }
        Ok(())
    }

    pub(crate) fn terminate_containment(&self) -> io::Result<()> {
        self.require_pre_execution()?;
        // SAFETY: borrowing the live owner cannot race its final Drop. No
        // numerical PID, stored HANDLE or reopened name selects this Job.
        if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn containment_population(&self) -> io::Result<u32> {
        self.require_pre_execution()?;
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let length = std::mem::size_of_val(&accounting) as u32;
        let mut returned = 0;
        // SAFETY: exact-size writable native buffer; original Job stays retained
        // for this entire synchronous query. Query failure is NEVER zero.
        let queried = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                length,
                &raw mut returned,
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error());
        }
        if returned != length {
            return Err(io::Error::other("incomplete original Job population query"));
        }
        Ok(accounting.ActiveProcesses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_job_without_suspended_entry_never_supplies_population_proof() {
        let job = WindowsJob::create().expect("real private empty Job");
        assert_eq!(
            job.containment_population()
                .expect_err("not launch provenance")
                .kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn job_termination_requires_suspended_entry_not_kill_on_close_configuration() {
        let job = WindowsJob::create().expect("real private kill-on-close Job");
        assert_eq!(
            job.terminate_containment()
                .expect_err("not launch provenance")
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
