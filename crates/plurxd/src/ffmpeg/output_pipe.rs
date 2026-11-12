//! Descriptor inheritance shared by index and playback measurement pipes.

/// Fixed child descriptor targets for a held source and measurement outputs.
/// Every source is duplicated above all targets before any target is installed,
/// so a target can never overwrite another output's still-needed source.
/// Install only from Command::pre_exec; inheritance allocates nothing.
#[derive(Clone, Copy)]
pub(crate) struct InheritedFds<const N: usize> {
    from: [std::os::fd::RawFd; N],
    to: [std::os::fd::RawFd; N],
    len: usize,
}

impl<const N: usize> Default for InheritedFds<N> {
    fn default() -> Self {
        Self {
            from: [0; N],
            to: [0; N],
            len: 0,
        }
    }
}

impl<const N: usize> InheritedFds<N> {
    pub(crate) fn push(&mut self, from: std::os::fd::RawFd, to: std::os::fd::RawFd) {
        self.from[self.len] = from;
        self.to[self.len] = to;
        self.len += 1;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Runs in the child between `fork` and `exec`. Every source is first
    /// duplicated above the target range, so installing one target can never
    /// close another source still needed by a later output.
    pub(crate) fn inherit(&self) -> std::io::Result<()> {
        let mut high = [0; N];
        let floor = self.to[..self.len]
            .iter()
            .copied()
            .max()
            .unwrap_or(2)
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("child descriptor target overflow"))?;
        for (slot, from) in high.iter_mut().zip(&self.from).take(self.len) {
            // SAFETY: plain descriptor syscalls on descriptors this process
            // holds; async-signal-safe.
            let duplicate = unsafe { libc::fcntl(*from, libc::F_DUPFD_CLOEXEC, floor) };
            if duplicate == -1 {
                return Err(std::io::Error::last_os_error());
            }
            *slot = duplicate;
        }
        for (duplicate, to) in high.iter().zip(&self.to).take(self.len) {
            // SAFETY: as above.
            unsafe {
                if libc::dup2(*duplicate, *to) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(*duplicate);
                let flags = libc::fcntl(*to, libc::F_GETFD);
                if flags == -1 || libc::fcntl(*to, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::InheritedFds;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    /// Installing one output target must not overwrite a descriptor still
    /// needed as another source. Swap the real child stdout/stderr and prove
    /// both original pipes survive, rather than inspecting a mapping array.
    #[test]
    fn colliding_output_sources_are_duplicated_before_any_target_changes() {
        let mut descriptors = InheritedFds::<2>::default();
        descriptors.push(1, 2);
        descriptors.push(2, 1);
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "printf out; printf err >&2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: inheritance performs only descriptor syscalls, with no
        // allocation or locks after fork.
        unsafe {
            command.pre_exec(move || descriptors.inherit());
        }
        let output = command
            .output()
            .expect("spawn descriptor collision fixture");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"err");
        assert_eq!(output.stderr, b"out");
    }
}
