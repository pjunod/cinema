//! Descriptor inheritance shared by index and playback measurement pipes.

#[cfg(test)]
mod tests {
    use crate::producer_spawn::{with_std_manifest, ChildStdio, ChildStdios, Descriptors};
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::process::Command;

    /// Both parent pipes deliberately occupy another mapping's target number.
    /// The real builder must preserve both exact pipes through child exec.
    #[test]
    fn colliding_output_sources_are_duplicated_before_any_target_changes() {
        let (mut read_a, write_a) = std::io::pipe().expect("first pipe");
        let (mut read_b, write_b) = std::io::pipe().expect("second pipe");
        let target_a = write_b.as_raw_fd();
        let target_b = write_a.as_raw_fd();
        let descriptors = Descriptors::from_mappings(
            &[
                (write_a.as_raw_fd(), target_a),
                (write_b.as_raw_fd(), target_b),
            ],
            None,
        );
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("printf out >/dev/fd/{target_a}; printf err >/dev/fd/{target_b}"),
        ]);
        let manifest = descriptors
            .prepare_std(
                &mut command,
                ChildStdios {
                    stdin: ChildStdio::Null,
                    stdout: ChildStdio::Null,
                    stderr: ChildStdio::Null,
                },
            )
            .expect("collision-safe builder");
        #[allow(clippy::disallowed_methods)] // This fixture owns and waits its exact child.
        let status = with_std_manifest(&manifest, &mut command, |command| command.status())
            .expect("descriptor collision fixture");
        assert!(status.success());
        drop((command, manifest, write_a, write_b));
        let (mut a, mut b) = (Vec::new(), Vec::new());
        read_a.read_to_end(&mut a).expect("first output");
        read_b.read_to_end(&mut b).expect("second output");
        assert_eq!(a, b"out");
        assert_eq!(b, b"err");
    }
}
