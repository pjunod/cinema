//! Install already-owned descriptors in a Unix child before `exec`.
//!
//! The caller must keep every descriptor's actual owner alive through spawn.
//! This helper only arranges child descriptor numbers; it grants no acquisition,
//! custody, closure, refund, or other physical-operation authority.

/// Copy existing raw descriptors to fixed child targets in a `pre_exec` hook.
///
/// All sources are duplicated to high close-on-exec descriptors before any
/// target is overwritten, so an input that itself occupies fd 3–9 is safe.
/// Mapping count and target range are intentionally bounded. On macOS,
/// `cwd_target` also selects the child's working directory after descriptor
/// installation.
///
/// `descriptors` are borrowed only as raw numbers. Their actual owners must
/// remain alive through the spawn attempt; this function does not acquire or
/// retain them and does not prove their eventual closure.
pub fn inherit_raw_file_descriptors_with_cwd(
    command: &mut tokio::process::Command,
    descriptors: &[(std::os::fd::RawFd, i32)],
    cwd_target: Option<i32>,
) {
    assert!(
        descriptors.len() <= 7
            && descriptors
                .iter()
                .all(|(_, target)| (3..=9).contains(target))
    );
    assert!(cwd_target.is_none_or(|target| descriptors.iter().any(|(_, fd)| *fd == target)));
    let descriptors = descriptors.to_vec();
    unsafe {
        command.pre_exec(move || {
            let mut copies = [None; 7];
            for (index, (fd, target)) in descriptors.iter().enumerate() {
                let duplicate = libc::fcntl(*fd, libc::F_DUPFD_CLOEXEC, 10);
                if duplicate == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                copies[index] = Some((duplicate, *target));
            }
            for (duplicate, target) in copies.into_iter().flatten() {
                if libc::dup2(duplicate, target) == -1 {
                    libc::close(duplicate);
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(duplicate);
            }
            #[cfg(target_os = "macos")]
            if let Some(target) = cwd_target {
                if libc::fchdir(target) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}
