//! The selected finite recipe's exact read-only loader objects. This is a
//! structural allowance, not a complete allocator/kernel/producer peak.
use super::*;
use crate::adaptive_resources::runtime::finite_vod::SoftwareAvcCommand;
use crate::source_identity::SourceIdentity;
use std::os::fd::{BorrowedFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::FileExt;

const MAX_OBJECTS: usize = 250;
const INTERPRETER_PATH_LIMIT: usize = 4096;
const EXECUTABLE: &str = "/proc/self/fd/9";
// The child has read-only object grants and cannot use daemon cache/temp/home.
// Stderr is an actual pipe, so FFmpeg's usual non-TTY logging needs no
// force-color override. Preserve the fixed C locale and the loader's path.
const ENVIRONMENT: [&str; 1] = ["LC_ALL=C"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FiniteNativeAllowance {
    pub handles: u64,
    pub requested_bytes: u64,
}

/// Original loader Files survive the child and retain uncertain checked closes.
pub(crate) struct FiniteNativeProfile {
    spec: Option<SealedLaunchSpec>,
    executable: Option<File>,
    input: Option<File>,
    interpreter: Option<File>,
    finite_inputs: Vec<File>,
    sandbox: Option<sandbox::PreparedSandbox>,
    objects: Vec<File>,
    close_failed: bool,
}

impl FiniteNativeProfile {
    /// Retain this cell in the original graph before prepare_in_place. All
    /// original partial files are installed immediately after acquisition.
    pub(crate) fn empty() -> Self {
        Self {
            spec: None,
            executable: None,
            input: None,
            interpreter: None,
            finite_inputs: Vec::with_capacity(2),
            sandbox: None,
            objects: Vec::with_capacity(MAX_OBJECTS),
            close_failed: false,
        }
    }
    /// No descriptor or domain is created by this eligibility projection.
    /// Excess or unsupported packages retain the distinct trusted baseline.
    pub(crate) fn allowance(command: &SoftwareAvcCommand) -> Option<FiniteNativeAllowance> {
        if !cfg!(target_arch = "x86_64") || unsafe { libc::geteuid() } == 0 {
            return None;
        }
        let objects = command.native_engine_objects();
        let strings = command
            .args()
            .len()
            .checked_add(command.env().len())?
            .checked_add(1 + ENVIRONMENT.len())?;
        let mut bytes = EXECUTABLE.len() + 1;
        for arg in command.args() {
            bytes = bytes.checked_add(arg.len() + 1)?;
        }
        for entry in ENVIRONMENT {
            bytes = bytes.checked_add(entry.len() + 1)?;
        }
        for (key, value) in command.env() {
            if key != "LD_LIBRARY_PATH" {
                return None;
            }
            bytes = bytes.checked_add(key.len() + value.len() + 2)?;
        }
        if objects.is_empty()
            || objects.len() > MAX_OBJECTS
            || strings > MAX_STRINGS
            || bytes > MAX_STRING_BYTES
        {
            return None;
        }
        if objects.iter().any(|(path, version)| {
            !path.is_absolute() || path.as_os_str().len() > 4096 || version.is_empty()
        }) {
            return None;
        }
        // 16 retained domain interfaces; factory setup/verification, entry
        // seed, original pidfd and registration copy, 3 pipe pairs and their
        // high-fd child copies, executable/input/null copies and ruleset.
        let domain_retained = 16u64;
        let verification_readers = 3u64;
        let input_executable_null_copies = command.source_count() as u64 + 3; // includes interpreter alias
        let pipe_ends = 3 * 2;
        let child_high_copies = command.source_count() as u64 + 5;
        let entry_seed = 1;
        let original_and_registered_pidfds = 2;
        let ruleset = 1;
        let child_priority_control = 1;
        let handles = domain_retained
            + verification_readers
            + input_executable_null_copies
            + pipe_ends
            + child_high_copies
            + entry_seed
            + original_and_registered_pidfds
            + ruleset
            + child_priority_control
            + objects.len() as u64;
        let requested_bytes = std::mem::size_of::<Self>()
            .checked_add((MAX_OBJECTS + 2 * 2) * std::mem::size_of::<File>())?
            .checked_add(INTERPRETER_PATH_LIMIT)?
            .checked_add(std::mem::size_of::<super::LaunchScratch>())?
            .checked_add(MAX_STRING_BYTES)?
            .checked_add(
                MAX_STRINGS * (std::mem::size_of::<CString>() + 2 * std::mem::size_of::<usize>()),
            )?
            .checked_add(crate::process::containment::NativeDomainRoot::domain_projection())?
            .checked_add(std::mem::size_of::<super::linux::PreparedChild<'static>>())?
            .checked_add(
                (objects.len() + command.source_count())
                    .checked_mul(std::mem::size_of::<&File>())?,
            )?
            .checked_add(std::mem::size_of::<OwnedContainedChild>())?
            .checked_add(
                crate::process::priority::native_registration_requested_bytes(
                    command.program().as_os_str(),
                )?,
            )?;
        Some(FiniteNativeAllowance {
            handles,
            requested_bytes: requested_bytes as u64,
        })
    }

    pub(crate) fn prepare_in_place(
        &mut self,
        command: &SoftwareAvcCommand,
        descriptors: &[(RawFd, i32)],
    ) -> io::Result<()> {
        Self::allowance(command)
            .ok_or_else(|| io::Error::other("finite native package exceeds sealed profile"))?;
        if descriptors.len() != command.source_count() + 1
            || !(1..=2).contains(&command.source_count())
        {
            return Err(io::Error::other(
                "finite original descriptor inventory mismatch",
            ));
        }
        let copy = |raw| -> io::Result<File> {
            // Original SourceCells remain borrowed under the bundle lock.
            Ok(unsafe { BorrowedFd::borrow_raw(raw) }
                .try_clone_to_owned()?
                .into())
        };
        if self.executable.is_some() || self.spec.is_some() {
            return Err(io::Error::other("original finite profile already prepared"));
        }
        self.executable = Some(copy(
            descriptors
                .last()
                .ok_or_else(|| io::Error::other("missing original executable"))?
                .0,
        )?);
        let interpreter = interpreter_path(
            self.executable
                .as_ref()
                .ok_or_else(|| io::Error::other("missing original executable"))?,
        )?;
        if let Some(path) = &interpreter {
            self.interpreter = Some(File::open(path)?);
        }
        let interpreter_identity = self
            .interpreter
            .as_ref()
            .map(|file| {
                file.metadata()
                    .map(|metadata| SourceIdentity::from_metadata(&metadata))
            })
            .transpose()?;
        for (index, (raw, target)) in descriptors.iter().take(command.source_count()).enumerate() {
            if *target != 3 + index as i32 {
                return Err(io::Error::other("finite input role changed"));
            }
            self.finite_inputs.push(copy(*raw)?);
        }
        if descriptors.last().map(|(_, target)| *target) != Some(9) {
            return Err(io::Error::other("finite executable role changed"));
        }
        let mut arguments = Vec::with_capacity(command.args().len() + 1);
        arguments.push(CString::new(EXECUTABLE).map_err(io::Error::other)?);
        for arg in command.args() {
            arguments.push(CString::new(arg.as_bytes()).map_err(io::Error::other)?);
        }
        let mut environment = Vec::with_capacity(command.env().len() + ENVIRONMENT.len());
        for entry in ENVIRONMENT {
            environment.push(CString::new(entry).map_err(io::Error::other)?);
        }
        for (key, value) in command.env() {
            let mut entry = key.as_encoded_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_encoded_bytes());
            environment.push(CString::new(entry).map_err(io::Error::other)?);
        }
        self.input = Some(File::open("/dev/null")?);
        for file in self.executable.iter().chain(&self.finite_inputs) {
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY || !file.metadata()?.is_file()
            {
                return Err(io::Error::other(
                    "original finite inputs must remain read-only regular objects",
                ));
            }
        }
        let mut interpreter_captured = interpreter_identity.is_none();
        for (path, expected) in command.native_engine_objects() {
            self.objects.push(File::open(path)?);
            let file = self
                .objects
                .last()
                .ok_or_else(|| io::Error::other("original loader file not retained"))?;
            let metadata = file.metadata()?;
            let actual = SourceIdentity::from_metadata(&metadata).render_s1();
            if !metadata.is_file() || actual.strip_prefix("s1:") != Some(expected.as_str()) {
                return Err(io::Error::other("original captured loader object changed"));
            }
            if interpreter_identity == Some(SourceIdentity::from_metadata(&metadata)) {
                interpreter_captured = true;
            }
        }
        if !interpreter_captured {
            return Err(io::Error::other(
                "original ELF interpreter object is absent from captured package",
            ));
        }
        let interpreters = self.interpreter.as_slice();
        self.spec = Some(SealedLaunchSpec {
            executable: self
                .executable
                .take()
                .ok_or_else(|| io::Error::other("original executable missing"))?,
            input: self
                .input
                .take()
                .ok_or_else(|| io::Error::other("original null input missing"))?,
            finite_inputs: std::mem::replace(&mut self.finite_inputs, Vec::with_capacity(2)),
            arguments,
            environment,
            sandbox: None,
        });
        let spec = self
            .spec
            .as_mut()
            .ok_or_else(|| io::Error::other("original spec missing"))?;
        spec.validate()?;
        let mut reads = Vec::with_capacity(self.objects.len() + spec.finite_inputs.len());
        reads.extend(self.objects.iter());
        reads.extend(spec.finite_inputs.iter());
        // The policy borrows exact opened objects; no library-directory grant.
        sandbox::PreparedSandbox::prepare_in_place(
            &mut spec.sandbox,
            &spec.executable,
            &spec.input,
            interpreters,
            &reads,
        )?;
        Ok(())
    }

    #[allow(
        clippy::result_large_err,
        reason = "The original failed native domain stays with the attempt without a new allocation after entry."
    )]
    pub(crate) fn launch(
        &mut self,
        domain: OwnedNativeDomain,
        work: super::super::priority::ChildWork,
    ) -> Result<OwnedContainedChild, LaunchFailure> {
        let Some(spec) = self.spec.as_ref() else {
            return Err(LaunchFailure {
                domain,
                reason: LaunchRefusal::Refused(io::Error::other("original finite profile missing")),
                setup_close_uncertain: false,
            });
        };
        super::launch_borrowed(domain, spec, Some(work.class))
    }

    pub(crate) fn close_original_objects(&mut self) -> io::Result<()> {
        if self.close_failed {
            return Err(io::Error::other("original loader close is uncertain"));
        }
        if let Some(spec) = self.spec.take() {
            self.executable = Some(spec.executable);
            self.input = Some(spec.input);
            self.finite_inputs = spec.finite_inputs;
            self.sandbox = spec.sandbox;
        }
        if let Some(sandbox) = self.sandbox.take() {
            if let Err(error) = sandbox.close_original() {
                self.close_failed = true;
                return Err(error);
            }
        }
        for cell in [&mut self.executable, &mut self.input, &mut self.interpreter] {
            if let Some(file) = cell.take() {
                if unsafe { libc::close(file.into_raw_fd()) } != 0 {
                    self.close_failed = true;
                    return Err(io::Error::last_os_error());
                }
            }
        }
        while let Some(file) = self.finite_inputs.pop() {
            if unsafe { libc::close(file.into_raw_fd()) } != 0 {
                self.close_failed = true;
                return Err(io::Error::last_os_error());
            }
        }
        while let Some(file) = self.objects.pop() {
            let raw = file.into_raw_fd();
            if unsafe { libc::close(raw) } != 0 {
                self.close_failed = true;
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

fn interpreter_path(file: &File) -> io::Result<Option<std::path::PathBuf>> {
    let invalid = || io::Error::other("finite executable has unsupported ELF interpreter metadata");
    let mut header = [0u8; 64];
    file.read_exact_at(&mut header, 0)?;
    if &header[..6] != b"\x7fELF\x02\x01" {
        return Err(invalid());
    }
    let offset = u64::from_le_bytes(header[32..40].try_into().map_err(|_| invalid())?);
    let size = u16::from_le_bytes(header[54..56].try_into().map_err(|_| invalid())?);
    let count = u16::from_le_bytes(header[56..58].try_into().map_err(|_| invalid())?);
    if size != 56 || count > 128 {
        return Err(invalid());
    }
    let mut found = None;
    for index in 0..u64::from(count) {
        let mut entry = [0u8; 56];
        file.read_exact_at(
            &mut entry,
            offset.checked_add(index * 56).ok_or_else(invalid)?,
        )?;
        if u32::from_le_bytes(entry[..4].try_into().map_err(|_| invalid())?) != 3 {
            continue;
        }
        if found.is_some() {
            return Err(invalid());
        }
        let start = u64::from_le_bytes(entry[8..16].try_into().map_err(|_| invalid())?);
        let length = u64::from_le_bytes(entry[32..40].try_into().map_err(|_| invalid())?);
        if !(2..=INTERPRETER_PATH_LIMIT as u64).contains(&length) {
            return Err(invalid());
        }
        let mut bytes = vec![0; length as usize];
        file.read_exact_at(&mut bytes, start)?;
        if bytes.pop() != Some(0) || bytes.contains(&0) || bytes.first() != Some(&b'/') {
            return Err(invalid());
        }
        found = Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(
            bytes,
        )));
    }
    Ok(found)
}
