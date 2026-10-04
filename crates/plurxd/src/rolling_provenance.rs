//! Optional process-private provenance for the actual rolling producer input.
//! Failure removes measured-output eligibility, never ordinary playback.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use plurx_core::domain::MediaFile;
use sha2::{Digest, Sha256};

pub(crate) struct RollingProduction {
    source: crate::fragment_index_cluster::SourceFence,
    executable: crate::ffmpeg::EncodedExecutable,
    engine: crate::ffmpeg::EncodedEngine,
    binding: [u8; 32],
    attempt: AtomicU64,
    refused: AtomicBool,
}

impl RollingProduction {
    pub(crate) fn executable_path(&self) -> &std::path::Path {
        &self.executable.path
    }
    /// `logical` is freshly constructed by the real route's resolved argv
    /// builder, with a fixed output destination. It is not received metadata.
    pub(crate) async fn capture(
        file: &MediaFile,
        logical: &[u8],
        multiple_source_inputs: bool,
        program: &str,
    ) -> Option<Arc<Self>> {
        if logical.is_empty() || logical.len() > 64 * 1024 {
            return None;
        }
        // Linux procfs opens independent descriptions. /dev/fd on macOS can
        // share seek offsets, so copied audio's second demuxer is not attested
        // by the single inherited descriptor. Leave its old path untouched.
        if !cfg!(unix) || (!cfg!(target_os = "linux") && multiple_source_inputs) {
            return None;
        }
        let source = crate::fragment_index_cluster::open_source_fence(file, None)
            .await
            .ok()?;
        let executable = crate::ffmpeg::EncodedExecutable::capture_program(program)
            .await
            .ok()?;
        let engine = crate::ffmpeg::EncodedEngine::capture(None).await.ok()?;
        if !source.unchanged() || !engine.is_current_with_executable(&executable).await {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(b"plurx/rolling-full-output/logical-v1\0");
        for bytes in [
            logical,
            source.object_version().as_bytes(),
            executable.digest.as_bytes(),
            engine.digest.as_bytes(),
        ] {
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
        Some(Arc::new(Self {
            source,
            executable,
            engine,
            binding: digest.finalize().into(),
            attempt: AtomicU64::new(u64::MAX),
            refused: AtomicBool::new(false),
        }))
    }

    /// Execution-only input path. Canonical source/recipe facts stay original.
    pub(crate) fn input_path(&self) -> std::path::PathBuf {
        if cfg!(target_os = "linux") {
            "/proc/self/fd/3".into()
        } else {
            "/dev/fd/3".into()
        }
    }

    #[cfg(unix)]
    pub(crate) fn source_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.source.handle.as_raw_fd()
    }

    /// Called only under the existing child-replacement gate, after the
    /// predecessor is reaped. On /dev/fd the inherited open description may
    /// retain its old offset; resetting that same held object avoids turning
    /// an otherwise ordinary fallback into an empty-input producer.
    pub(crate) fn rewind_reaped_input(&self) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};
        if !self.source.unchanged() {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut handle = &self.source.handle;
        handle.seek(SeekFrom::Start(0))?;
        Ok(())
    }

    pub(crate) fn bind_initial_attempt(&self, attempt: u64) {
        if self
            .attempt
            .compare_exchange(u64::MAX, attempt, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.refuse();
        }
    }

    /// Retry output must never inherit the predecessor's resolved production
    /// identity. Its held source still keeps playback descriptor-safe.
    pub(crate) fn refuse(&self) {
        self.refused.store(true, Ordering::Release);
    }

    pub(crate) async fn current(&self, attempt: u64) -> bool {
        !self.refused.load(Ordering::Acquire)
            && self.attempt.load(Ordering::Acquire) == attempt
            && self.source.unchanged()
            && self
                .engine
                .is_current_with_executable(&self.executable)
                .await
            && self.source.unchanged()
            && !self.refused.load(Ordering::Acquire)
            && self.attempt.load(Ordering::Acquire) == attempt
    }

    pub(crate) async fn input_current(&self) -> bool {
        self.source.unchanged()
            && self
                .engine
                .is_current_with_executable(&self.executable)
                .await
            && self.source.unchanged()
    }

    pub(crate) fn binding(&self) -> [u8; 32] {
        self.binding
    }

    /// Captured but never bound to a producer attempt or refused, so the same
    /// start may hand it to its producer instead of capturing again.
    pub(crate) fn unbound(&self) -> bool {
        !self.refused.load(Ordering::Acquire) && self.attempt.load(Ordering::Acquire) == u64::MAX
    }
}
