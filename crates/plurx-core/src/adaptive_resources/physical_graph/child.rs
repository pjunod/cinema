//! One graph-owned child facade. The native branch keeps original domain and
//! pidfd authority; converting its output adapters preserves the same FDs.
use super::*;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
use tokio::process::Child;

pub(super) struct GraphChild {
    backend: Backend,
    pub(super) stdin: Option<ChildStdin>,
    pub(super) stdout: Option<ChildStdout>,
    pub(super) stderr: Option<ChildStderr>,
}

enum Backend {
    Legacy(Child),
    #[cfg(target_os = "linux")]
    Native {
        child: crate::process::contained_launch::OwnedContainedChild,
        adapter_uncertain: bool,
    },
}

impl From<Child> for GraphChild {
    fn from(mut child: Child) -> Self {
        Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            backend: Backend::Legacy(child),
        }
    }
}

impl GraphChild {
    #[cfg(target_os = "linux")]
    pub(super) fn from_native(
        child: crate::process::contained_launch::OwnedContainedChild,
    ) -> Self {
        Self {
            backend: Backend::Native {
                child,
                adapter_uncertain: false,
            },
            stdin: None,
            stdout: None,
            stderr: None,
        }
    }

    /// Called only after retaining this child in its original graph. Adapter
    /// setup errors therefore cannot drop the sole native wait/domain owner.
    #[cfg(target_os = "linux")]
    pub(super) fn install_native_pipes(&mut self) -> io::Result<()> {
        let Backend::Native {
            child,
            adapter_uncertain,
            ..
        } = &mut self.backend
        else {
            return Err(io::Error::other("native child missing"));
        };
        if *adapter_uncertain {
            return Err(io::Error::other(
                "original native pipe conversion is uncertain",
            ));
        }
        convert_native_pipe(
            child.take_stdout(),
            &mut self.stdout,
            adapter_uncertain,
            tokio_stdout,
        )?;
        convert_native_pipe(
            child.take_stderr(),
            &mut self.stderr,
            adapter_uncertain,
            tokio_stderr,
        )?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn native_pidfd(&self) -> io::Result<std::os::fd::OwnedFd> {
        match &self.backend {
            Backend::Native { child, .. } => child.original_pidfd_copy(),
            Backend::Legacy(_) => Err(io::Error::other("original native child missing")),
        }
    }

    pub(super) fn is_native(&self) -> bool {
        match &self.backend {
            Backend::Legacy(_) => false,
            #[cfg(target_os = "linux")]
            Backend::Native { .. } => true,
        }
    }

    pub(super) fn id(&self) -> Option<u32> {
        match &self.backend {
            Backend::Legacy(child) => child.id(),
            #[cfg(target_os = "linux")]
            Backend::Native { child, .. } => child.id(),
        }
    }

    pub(super) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        match &mut self.backend {
            Backend::Legacy(child) => child.try_wait(),
            #[cfg(target_os = "linux")]
            Backend::Native { child, .. } => {
                use std::os::unix::process::ExitStatusExt;
                child
                    .try_wait()
                    .map(|status| status.map(std::process::ExitStatus::from_raw))
            }
        }
    }

    pub(super) async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        match &mut self.backend {
            Backend::Legacy(child) => child.wait().await,
            #[cfg(target_os = "linux")]
            Backend::Native { child, .. } => {
                use std::os::unix::process::ExitStatusExt;
                loop {
                    if let Some(status) = child.try_wait()? {
                        return Ok(std::process::ExitStatus::from_raw(status));
                    }
                    // This is the original cancellable waiter, not detached
                    // supervision; cancellation retains the same graph child.
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        }
    }

    pub(super) fn start_kill(&mut self) -> io::Result<()> {
        match &mut self.backend {
            Backend::Legacy(child) => child.start_kill(),
            #[cfg(target_os = "linux")]
            Backend::Native { child, .. } => child.request_kill(),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn reap_refused_native(&mut self) -> io::Result<bool> {
        let Backend::Native { child, .. } = &mut self.backend else {
            return Err(io::Error::other("original refused native child missing"));
        };
        let stdout = self
            .stdout
            .take()
            .map_or(Ok(()), super::finite_pipes::close_unclaimed_stdout);
        let stderr = self
            .stderr
            .take()
            .map_or(Ok(()), super::finite_pipes::close_unclaimed_stderr);
        let controls = child.reap_refused_native();
        stdout.and(stderr).and(controls)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn try_close_native(&mut self) -> io::Result<bool> {
        match &mut self.backend {
            Backend::Legacy(_) => Ok(true),
            #[cfg(target_os = "linux")]
            Backend::Native {
                child,
                adapter_uncertain,
            } => {
                if *adapter_uncertain {
                    return Err(io::Error::other(
                        "original native pipe conversion is uncertain",
                    ));
                }
                if self.stdin.is_some() || self.stdout.is_some() || self.stderr.is_some() {
                    return Ok(false);
                }
                child.try_close_native()
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn tokio_stdout(file: File) -> io::Result<ChildStdout> {
    ChildStdout::from_std(std::process::ChildStdout::from(OwnedFd::from(file)))
}

#[cfg(target_os = "linux")]
fn tokio_stderr(file: File) -> io::Result<ChildStderr> {
    ChildStderr::from_std(std::process::ChildStderr::from(OwnedFd::from(file)))
}

#[cfg(target_os = "linux")]
fn convert_native_pipe<T>(
    original: Option<File>,
    converted: &mut Option<T>,
    uncertain: &mut bool,
    convert: impl FnOnce(File) -> io::Result<T>,
) -> io::Result<()> {
    if *uncertain {
        return Err(io::Error::other(
            "original native pipe conversion already uncertain",
        ));
    }
    if let Some(file) = original {
        // Tokio owns this consuming conversion. On Err or unwind there is no
        // returned descriptor or checked-close receipt for the original.
        *uncertain = true;
        *converted = Some(convert(file)?);
        *uncertain = false;
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn consuming_native_pipe_conversion_failure_preserves_other_original() {
        for fail_stdout in [true, false] {
            // epoll accepts an original pipe but rejects a regular file. The
            // failing attempt reaches the real consuming Tokio conversion.
            let (pipe, _writer) = std::io::pipe().expect("original file descriptors");
            let pipe = File::from(OwnedFd::from(pipe));
            let unpollable = File::open("/dev/null").expect("original unpollable descriptor");
            let (stdout, stderr) = if fail_stdout {
                (unpollable, pipe)
            } else {
                (pipe, unpollable)
            };
            let mut stderr = Some(stderr);
            let mut converted_stdout = None;
            let mut converted_stderr = None;
            let mut uncertain = false;
            let result = convert_native_pipe(
                Some(stdout),
                &mut converted_stdout,
                &mut uncertain,
                tokio_stdout,
            )
            .and_then(|()| {
                convert_native_pipe(
                    stderr.take(),
                    &mut converted_stderr,
                    &mut uncertain,
                    tokio_stderr,
                )
            });
            assert!(result.is_err());
            assert!(uncertain);
            assert_eq!(converted_stdout.is_none(), fail_stdout);
            assert_eq!(stderr.is_some(), fail_stdout);
            assert!(converted_stderr.is_none());
            assert!(
                convert_native_pipe(None, &mut converted_stdout, &mut uncertain, tokio_stdout)
                    .is_err()
            );
        }
    }
}
