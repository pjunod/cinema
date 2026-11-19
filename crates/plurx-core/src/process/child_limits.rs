//! Fail-closed child resource policy, separate from descriptor installation.
//!
//! This boundary registers only the audited file-size hook. Descriptor setup
//! remains owned by the prepared descriptor builder, and priority remains a
//! separate best-effort policy.

use std::io;

/// Bound every regular-file write by the child to the caller's byte limit.
/// Both limits are installed before exec; a failed setrlimit fails the spawn.
/// Exceeding the limit retains the kernel's existing SIGXFSZ behavior.
pub(super) fn file_size(command: &mut tokio::process::Command, max_bytes: usize) {
    let limit = max_bytes as libc::rlim_t;
    // SAFETY: the child hook captures only the limit and calls setrlimit with
    // a live stack value. It neither allocates nor changes any descriptors.
    unsafe {
        command.pre_exec(move || {
            let bounds = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &bounds) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
}
