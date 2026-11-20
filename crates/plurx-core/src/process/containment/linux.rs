use super::NativeDomainLimits;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

const MAX_CGROUP_PATH_BYTES: usize = 4096;
const CGROUP2_SUPER_MAGIC: libc::c_long = 0x6367_7270;
const CONTROL_READ_LIMIT: usize = 4096;
const MEMBERSHIP_READ_LIMIT: usize = 4096;
const MOUNTINFO_READ_LIMIT: usize = 131072;
const PROCS_READ_LIMIT: usize = 65536;
const CONTROL_FILES: usize = 11;

fn refuse(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObjectIdentity {
    device: u64,
    inode: u64,
    mount: u64,
}

fn object(file: &File) -> io::Result<ObjectIdentity> {
    let mut stat = MaybeUninit::<libc::statx>::zeroed();
    // All pointer arguments are valid for the duration of this syscall.
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_INO | libc::STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    if stat.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(refuse("kernel mount identity unavailable"));
    }
    Ok(ObjectIdentity {
        device: (u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor),
        inode: stat.stx_ino,
        mount: stat.stx_mnt_id,
    })
}

fn cgroupfs(file: &File) -> io::Result<()> {
    let mut stat = MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { stat.assume_init() }.f_type != CGROUP2_SUPER_MAGIC {
        return Err(refuse("native domain requires actual cgroup v2 filesystem"));
    }
    Ok(())
}

fn require_effective_access(file: &File, mode: libc::c_int) -> io::Result<()> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_faccessat2,
            file.as_raw_fd(),
            c"".as_ptr(),
            mode,
            libc::AT_EACCESS | libc::AT_EMPTY_PATH,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn open_at(parent: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn verify_control(directory: &File, file: &File) -> io::Result<()> {
    cgroupfs(file)?;
    let expected = object(directory)?;
    let actual = object(file)?;
    if actual.mount != expected.mount || actual.device != expected.device {
        return Err(refuse(
            "native control is not in the original directory mount",
        ));
    }
    Ok(())
}

fn control(directory: &File, name: &CStr, flags: libc::c_int) -> io::Result<File> {
    let file = open_at(directory.as_raw_fd(), name, flags)?;
    if let Err(error) = verify_control(directory, &file) {
        close_original(file)?;
        return Err(error);
    }
    Ok(file)
}

fn read_original(file: File, limit: usize) -> io::Result<Vec<u8>> {
    let result = read_bounded(&file, limit);
    close_original(file)?;
    result
}

fn current_namespace() -> io::Result<ObjectIdentity> {
    let file = File::open("/proc/self/ns/cgroup")?;
    let identity = object(&file);
    close_original(file)?;
    identity
}

// Reads are capped, restart from the same open control object, and never use a
// path lookup to adopt a replacement cgroup/control file.
fn read_bounded(file: &File, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; limit + 1];
    let mut length = 0;
    while length <= limit {
        let result = unsafe {
            libc::pread(
                file.as_raw_fd(),
                bytes[length..].as_mut_ptr().cast(),
                bytes.len() - length,
                length as libc::off_t,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            break;
        }
        length += result as usize;
    }
    if length > limit {
        return Err(refuse("native control read exceeds fixed limit"));
    }
    bytes.truncate(length);
    Ok(bytes)
}

fn text(bytes: &[u8]) -> io::Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| refuse("invalid native control encoding"))
}

fn scalar(bytes: &[u8]) -> io::Result<u64> {
    let value = text(bytes)?.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refuse("native control is not finite unsigned integer"));
    }
    value
        .parse()
        .map_err(|_| refuse("native control integer overflow"))
}

fn read_scalar(file: &File) -> io::Result<u64> {
    scalar(&read_bounded(file, CONTROL_READ_LIMIT)?)
}

fn write_control(file: &File, bytes: &[u8]) -> io::Result<()> {
    // Each control transaction is one kernel write; short writes are refusals.
    let result = unsafe { libc::write(file.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else if result as usize != bytes.len() {
        Err(refuse("short native control write"))
    } else {
        Ok(())
    }
}

struct PinnedDirectory {
    directory: File,
    parent: File,
    name: CString,
    identity: ObjectIdentity,
}

impl PinnedDirectory {
    fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() || path.as_os_str().len() > MAX_CGROUP_PATH_BYTES {
            return Err(refuse("explicit absolute cgroup root required"));
        }
        let mut directory = open_at(libc::AT_FDCWD, c"/", libc::O_RDONLY | libc::O_DIRECTORY)?;
        let mut components = path.components().peekable();
        while let Some(component) = components.next() {
            let name = match component {
                Component::RootDir => continue,
                Component::Normal(name) => {
                    CString::new(name.as_bytes()).map_err(|_| refuse("NUL in cgroup root"))?
                }
                _ => return Err(refuse("noncanonical cgroup root")),
            };
            let child = open_at(
                directory.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?;
            if components.peek().is_none() {
                let identity = object(&child)?;
                return Ok(Self {
                    directory: child,
                    parent: directory,
                    name,
                    identity,
                });
            }
            close_original(directory)?;
            directory = child;
        }
        Err(refuse("filesystem root cannot be a delegated cgroup root"))
    }

    fn duplicate(&self) -> io::Result<Self> {
        Ok(Self {
            directory: self.directory.try_clone()?,
            parent: self.parent.try_clone()?,
            name: self.name.clone(),
            identity: self.identity,
        })
    }

    fn verify(&self) -> io::Result<()> {
        cgroupfs(&self.directory)?;
        if object(&self.directory)? != self.identity {
            return Err(refuse("pinned native directory identity changed"));
        }
        let linked = open_at(
            self.parent.as_raw_fd(),
            &self.name,
            libc::O_RDONLY | libc::O_DIRECTORY,
        )?;
        let identity = object(&linked);
        close_original(linked)?;
        if identity? != self.identity {
            return Err(refuse("native directory link replaced"));
        }
        Ok(())
    }

    fn close(self) -> io::Result<()> {
        close_original(self.directory).and(close_original(self.parent))
    }

    fn heap_bytes(&self) -> usize {
        self.name.as_bytes_with_nul().len()
    }
}

/// Identity of the pinned kernel object, including its cgroup namespace and
/// mount. It is diagnostic identity, not a serializable authority to reopen it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeDomainIdentity {
    pub device: u64,
    pub inode: u64,
    pub mount: u64,
    pub cgroup_namespace_inode: u64,
}

impl NativeDomainIdentity {
    fn observed(directory: ObjectIdentity, namespace: ObjectIdentity) -> Self {
        Self {
            device: directory.device,
            inode: directory.inode,
            mount: directory.mount,
            cgroup_namespace_inode: namespace.inode,
        }
    }
}

/// Explicit capability for an operator-provided delegated directory. This
/// factory never infers a production root from an environment variable.
/// The caller authorizes this entire subtree and its controller access; other
/// principals with that same authority can change limits or move tasks. A
/// separate launcher sandbox must deny those powers to untrusted descendants.
pub struct NativeDomainRoot {
    root: PinnedDirectory,
    namespace: File,
    namespace_identity: ObjectIdentity,
}

impl NativeDomainRoot {
    /// Requested concrete factory storage and simultaneous descriptors from
    /// open/namespace validation: three retained, two self-directory handles,
    /// and one cgroup.procs reader. Library/kernel overhead is not a peak proof.
    pub(crate) fn factory_projection() -> (usize, u64, u16) {
        // The original root's inline storage is part of its support payload.
        let requested = std::mem::size_of::<[FactorySlot; MAX_NATIVE_DOMAINS]>()
            + MEMBERSHIP_READ_LIMIT + 1
            // The pinned root's basename and bounded namespace paths.
            + 4 * (MAX_CGROUP_PATH_BYTES + 1)
            + MOUNTINFO_READ_LIMIT
            + 1
            + PROCS_READ_LIMIT
            + 1;
        (requested, 3 + 2 + 1, MAX_NATIVE_DOMAINS as u16)
    }

    pub(crate) fn domain_projection() -> usize {
        4097 + 64 + PROCS_READ_LIMIT + 1 + 2 * (CONTROL_READ_LIMIT + 1)
    }

    /// Require a namespace-rooted cgroup2 mount, actual self membership in that
    /// same mount, and memory/pids already enabled for its children. Parent
    /// hierarchy configuration and daemon migration belong to the operator.
    pub fn open(explicit_root: &Path) -> io::Result<Self> {
        Self::open_inner(explicit_root, None)
    }

    /// Read-only, short-lived startup observations. No factory, descendant
    /// domain, writable file handle or entry capability is retained.
    pub(crate) fn probe_finite_startup(
        path: &Path,
    ) -> io::Result<(NativeDomainIdentity, NativeDomainLimits)> {
        let root = PinnedDirectory::open(path)?;
        let observed = (|| {
            root.verify()?;
            validate_finite_delegation(path, &root.directory)?;
            validate_controllers(&root.directory)?;
            Ok((
                NativeDomainIdentity::observed(root.identity, current_namespace()?),
                read_finite_limits(&root.directory)?,
            ))
        })();
        let closed = root.close();
        closed.and(observed)
    }

    pub(crate) fn open_finite(
        explicit_root: &Path,
        expected_identity: NativeDomainIdentity,
        expected_limits: NativeDomainLimits,
    ) -> io::Result<Self> {
        Self::open_inner(explicit_root, Some((expected_identity, expected_limits)))
    }

    fn open_inner(
        explicit_root: &Path,
        finite: Option<(NativeDomainIdentity, NativeDomainLimits)>,
    ) -> io::Result<Self> {
        let root = PinnedDirectory::open(explicit_root)?;
        root.verify()?;
        let namespace = File::open("/proc/self/ns/cgroup")?;
        let namespace_identity = object(&namespace)?;
        if let Some((expected_identity, expected_limits)) = finite {
            if NativeDomainIdentity::observed(root.identity, namespace_identity)
                != expected_identity
            {
                return Err(refuse("startup delegated native root identity changed"));
            }
            validate_finite_delegation(explicit_root, &root.directory)?;
            if read_finite_limits(&root.directory)? != expected_limits {
                return Err(refuse("startup delegated native limits changed"));
            }
        } else {
            validate_namespace_hierarchy(explicit_root, &root.directory, false)?;
        }
        validate_controllers(&root.directory)?;
        Ok(Self {
            root,
            namespace,
            namespace_identity,
        })
    }

    fn verify(&self) -> io::Result<()> {
        self.root.verify()?;
        if object(&self.namespace)? != self.namespace_identity
            || current_namespace()? != self.namespace_identity
        {
            return Err(refuse("native cgroup namespace changed"));
        }
        validate_controllers(&self.root.directory)
    }

    /// Revalidate the existing delegated ceiling on the original held
    /// directory. A changed/unlimited value never permits an unrestricted
    /// replacement after native selection.
    pub(crate) fn verify_finite_limits(
        &self,
        original_path: &Path,
        expected: NativeDomainLimits,
    ) -> io::Result<()> {
        self.verify()?;
        // The full startup path must still name the pinned object before it
        // can identify the ancestor of this specific destination.
        let linked = PinnedDirectory::open(original_path)?;
        let linked_identity = linked.identity;
        linked.close()?;
        if linked_identity != self.root.identity {
            return Err(refuse(
                "startup delegated native path no longer names the original directory",
            ));
        }
        validate_finite_delegation(original_path, &self.root.directory)?;
        if read_finite_limits(&self.root.directory)? != expected {
            return Err(refuse("original delegated native limits changed"));
        }
        Ok(())
    }

    /// The delegated parent and each generated child name require exclusive
    /// controller mutation by this owner. External rename/replacement or task
    /// migration by another equally privileged principal is outside this
    /// contract; pinned descriptors alone cannot make pathname unlink atomic.
    #[allow(
        clippy::result_large_err,
        reason = "The original failed factory custody stays inline; failure must not allocate a new owner box after kernel creation."
    )]
    pub fn create_domain(
        &self,
        limits: NativeDomainLimits,
    ) -> Result<OwnedNativeDomain, NativeDomainCreateFailure> {
        self.verify()?;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 || !limits.memory_bytes().is_multiple_of(page_size as u64) {
            return Err(
                refuse("native memory limit must be an exact positive page multiple").into(),
            );
        }
        let name =
            CString::new(format!("plurx-{}", uuid::Uuid::new_v4())).expect("UUID has no NUL");
        // Reserve all duplications before creating a kernel object.
        let root = self.root.duplicate()?;
        let namespace = self.namespace.try_clone()?;
        let parent = self.root.directory.try_clone()?;
        let mut lease = FactoryLease::reserve(CreatedDirectory {
            root,
            namespace,
            namespace_identity: self.namespace_identity,
            parent,
            name,
            directory: None,
            identity: None,
            controls: std::array::from_fn(|_| None),
            created: false,
        })?;
        let SetupCustody::Created(created) =
            lease.custody.as_mut().expect("reserved factory owner")
        else {
            unreachable!("new lease owns its original creation");
        };
        if unsafe { libc::mkdirat(created.parent.as_raw_fd(), created.name.as_ptr(), 0o700) } != 0 {
            return Err(setup_failure(io::Error::last_os_error(), lease));
        }
        created.created = true;
        // Custody and its fixed retention slot already exist. Every subsequent
        // refusal returns that same owner, including failures before statx.
        if let Err(error) = created.pin() {
            return Err(setup_failure(error, lease));
        }
        let files = match Controls::open(
            created.directory.as_ref().expect("pinned original"),
            &mut created.controls,
        ) {
            Ok(files) => files,
            Err(error) => return Err(setup_failure(error, lease)),
        };
        let SetupCustody::Created(created) =
            lease.custody.take().expect("original factory custody")
        else {
            unreachable!("factory has not issued a domain");
        };
        lease.custody = Some(SetupCustody::Configured(created.into_domain(files, limits)));
        let SetupCustody::Configured(domain) = lease.custody.as_mut().expect("configured custody")
        else {
            unreachable!("configured original domain");
        };
        let configured = (|| {
            validate_domain_type(&domain.directory.directory)?;
            domain.verify_empty()?;
            domain.files.configure(limits)?;
            domain.verify_empty()
        })();
        if let Err(error) = configured {
            return Err(setup_failure(error, lease));
        }
        // Construction owns the original object before the first possible seed.
        let SetupCustody::Configured(domain) =
            lease.custody.take().expect("successful original domain")
        else {
            unreachable!("configured original domain");
        };
        lease.owns_slot = false;
        Ok(OwnedNativeDomain {
            core: Some(domain),
            slot: Some(lease.index),
            close_uncertain: false,
        })
    }

    /// Retry retained failed or abandoned owners belonging to this original root. A retry
    /// never creates another child or remints a directory generation.
    pub fn retry_failed_factory_cleanup(&self) -> io::Result<NativeFactoryCleanupSummary> {
        self.verify()?;
        let mut summary = NativeFactoryCleanupSummary::default();
        for index in 0..MAX_NATIVE_DOMAINS {
            let retained = {
                let mut slots = factory_slots();
                if matches!(&slots[index], FactorySlot::CloseUncertain(identity) if *identity == self.root.identity)
                {
                    summary.retained += 1;
                    continue;
                }
                let matches = matches!(&slots[index], FactorySlot::Held(held)
                    if held.custody.root_identity() == self.root.identity);
                if !matches {
                    continue;
                }
                match std::mem::replace(&mut slots[index], FactorySlot::Reserved) {
                    FactorySlot::Held(held) => held,
                    _ => unreachable!("same locked retained slot"),
                }
            };
            let mut retained = retained;
            if let Err(error) = retained.custody.cleanup() {
                retained.close_uncertain |= lost_close(&error);
                factory_slots()[index] = FactorySlot::Held(retained);
                summary.retained += 1;
                continue;
            }
            let identity = retained.custody.root_identity();
            if retained.custody.close_files().is_ok() && !retained.close_uncertain {
                factory_slots()[index] = FactorySlot::Vacant;
                summary.closed += 1;
            } else {
                factory_slots()[index] = FactorySlot::CloseUncertain(identity);
                summary.retained += 1;
            }
        }
        Ok(summary)
    }

    pub fn owned_memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.root.heap_bytes()
    }
}

fn validate_finite_delegation(path: &Path, directory: &File) -> io::Result<()> {
    // An effective write check also refuses a read-only mount. Directory
    // creation and cgroup migration remain separate kernel operations.
    require_effective_access(directory, libc::W_OK | libc::X_OK)?;
    validate_namespace_hierarchy(path, directory, true)
}

fn read_finite_limits(directory: &File) -> io::Result<NativeDomainLimits> {
    let read_limit = |name| -> io::Result<u64> {
        let file = control(directory, name, libc::O_RDONLY)?;
        let observed = read_scalar(&file);
        close_original(file)?;
        observed
    };
    let memory = read_limit(c"memory.max")?;
    let tasks = u32::try_from(read_limit(c"pids.max")?)
        .map_err(|_| refuse("native delegated task limit exceeds the supported range"))?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 || !memory.is_multiple_of(page_size as u64) {
        return Err(refuse(
            "native memory limit must be an exact positive page multiple",
        ));
    }
    NativeDomainLimits::new(memory, tasks)
}

fn validate_controllers(directory: &File) -> io::Result<()> {
    validate_domain_type(directory)?;
    for name in [c"cgroup.controllers", c"cgroup.subtree_control"] {
        let file = control(directory, name, libc::O_RDONLY)?;
        let bytes = read_original(file, CONTROL_READ_LIMIT)?;
        let value = text(&bytes)?;
        if !["memory", "pids"]
            .iter()
            .all(|needed| value.split_whitespace().any(|word| word == *needed))
        {
            return Err(refuse(
                "memory and pids controllers must be delegated and enabled",
            ));
        }
    }
    Ok(())
}

fn validate_domain_type(directory: &File) -> io::Result<()> {
    let file = control(directory, c"cgroup.type", libc::O_RDONLY)?;
    if text(&read_original(file, CONTROL_READ_LIMIT)?)?.trim() != "domain" {
        return Err(refuse("native cgroup must be an ordinary domain"));
    }
    Ok(())
}

// Capacity is reserved before mkdir. Active domains, errors and retained owners
// share these slots; full capacity is an ordinary pre-creation refusal.
const MAX_NATIVE_DOMAINS: usize = 16;
static FACTORY_FAILURES: std::sync::Mutex<[FactorySlot; MAX_NATIVE_DOMAINS]> =
    std::sync::Mutex::new([const { FactorySlot::Vacant }; MAX_NATIVE_DOMAINS]);

#[allow(
    clippy::large_enum_variant,
    reason = "The fixed inline retention table reserves its complete owner storage before kernel creation."
)]
enum FactorySlot {
    Vacant,
    Reserved,
    Held(RetainedFactoryFailure),
    CloseUncertain(ObjectIdentity),
}

fn factory_slots() -> std::sync::MutexGuard<'static, [FactorySlot; MAX_NATIVE_DOMAINS]> {
    // No user callback runs under this lock. Preserve all original slots even
    // if another operation unwound; poison never authorizes dropping custody.
    FACTORY_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct RetainedFactoryFailure {
    custody: SetupCustody,
    _source: io::Error,
    close_uncertain: bool,
}

struct CreatedDirectory {
    root: PinnedDirectory,
    namespace: File,
    namespace_identity: ObjectIdentity,
    parent: File,
    name: CString,
    directory: Option<File>,
    identity: Option<ObjectIdentity>,
    controls: [Option<File>; CONTROL_FILES],
    created: bool,
}

impl CreatedDirectory {
    fn pin(&mut self) -> io::Result<()> {
        if self.directory.is_none() {
            // Retry selects the same preowned name under exclusive controller
            // mutation. It never retries mkdir or adopts a different root.
            self.directory = Some(open_at(
                self.parent.as_raw_fd(),
                &self.name,
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?);
        }
        let directory = self.directory.as_ref().expect("held created directory");
        cgroupfs(directory)?;
        let identity = object(directory)?;
        if identity.mount != self.root.identity.mount
            || identity.device != self.root.identity.device
        {
            return Err(refuse("new native directory changed kernel hierarchy"));
        }
        if self.identity.is_some_and(|original| original != identity) {
            return Err(refuse("created native directory identity changed"));
        }
        self.identity = Some(identity);
        Ok(())
    }

    fn into_domain(mut self, files: Controls, limits: NativeDomainLimits) -> NativeDomainCore {
        NativeDomainCore {
            root: self.root,
            directory: PinnedDirectory {
                directory: self
                    .directory
                    .take()
                    .expect("factory pinned before controls"),
                parent: self.parent,
                name: self.name,
                identity: self.identity.expect("factory observed before controls"),
            },
            files,
            namespace: self.namespace,
            namespace_identity: self.namespace_identity,
            limits,
            state: EntryState::Ready,
        }
    }
}

enum SetupCustody {
    Created(CreatedDirectory),
    Configured(NativeDomainCore),
}

impl SetupCustody {
    fn root_identity(&self) -> ObjectIdentity {
        match self {
            Self::Created(created) => created.root.identity,
            Self::Configured(domain) => domain.root.identity,
        }
    }

    fn close_files(self) -> io::Result<()> {
        match self {
            Self::Configured(domain) => domain.close_removed(),
            Self::Created(created) => [
                created.root.directory,
                created.root.parent,
                created.namespace,
                created.parent,
            ]
            .into_iter()
            .chain(created.directory)
            .chain(created.controls.into_iter().flatten())
            .fold(Ok(()), |result, file| result.and(close_original(file))),
        }
    }

    fn cleanup(&mut self) -> io::Result<()> {
        match self {
            Self::Created(created) => {
                if !created.created {
                    return Ok(());
                }
                created.root.verify()?;
                if object(&created.namespace)? != created.namespace_identity
                    || current_namespace()? != created.namespace_identity
                {
                    return Err(refuse("native factory cgroup namespace changed"));
                }
                created.pin()?;
                let directory = created.directory.as_ref().expect("same pinned creation");
                let identity = created.identity.expect("observed original creation");
                remove_unentered(directory, &created.parent, &created.name, identity)
            }
            Self::Configured(domain) => {
                if domain.state == EntryState::Ready {
                    // Factory setup never issued entry authority; incomplete
                    // limit readback does not prevent known empty cleanup.
                    domain.state = EntryState::Sealed;
                }
                if domain.state == EntryState::Removed {
                    return Ok(());
                }
                if domain.state != EntryState::Sealed {
                    return Err(refuse("original native entry capability remains issued"));
                }
                if removed_original(&domain.directory.directory).is_ok() {
                    domain.state = EntryState::Removed;
                    return Ok(());
                }
                domain.verify_empty()?;
                remove_unentered(
                    &domain.directory.directory,
                    &domain.directory.parent,
                    &domain.directory.name,
                    domain.directory.identity,
                )?;
                domain.state = EntryState::Removed;
                Ok(())
            }
        }
    }
}

fn removed_original(directory: &File) -> io::Result<()> {
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { stat.assume_init() }.st_nlink != 0 {
        return Err(refuse(
            "native unlink did not remove the original held directory",
        ));
    }
    Ok(())
}

fn remove_unentered(
    directory: &File,
    parent: &File,
    name: &CStr,
    identity: ObjectIdentity,
) -> io::Result<()> {
    // A preceding unlink may have succeeded even if its result/readback was
    // lost. Only the same held object's zero link count closes that retry.
    if removed_original(directory).is_ok() {
        return Ok(());
    }
    cgroupfs(directory)?;
    if object(directory)? != identity {
        return Err(refuse("original native directory changed"));
    }
    let linked = open_at(parent.as_raw_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY)?;
    if object(&linked)? != identity {
        return Err(refuse("native directory link replaced"));
    }
    let events = control(directory, c"cgroup.events", libc::O_RDONLY)?;
    let procs = control(directory, c"cgroup.procs", libc::O_RDONLY)?;
    let tasks = control(directory, c"pids.current", libc::O_RDONLY)?;
    if populated(&read_bounded(&events, CONTROL_READ_LIMIT)?)?
        || !text(&read_bounded(&procs, CONTROL_READ_LIMIT)?)?
            .trim()
            .is_empty()
        || read_scalar(&tasks)? != 0
    {
        return Err(refuse("unentered native setup directory is populated"));
    }
    // Exclusive parent/name mutation is required: identity checks and unlink
    // are separate syscalls. A post-unlink replacement race stays Unknown.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        return Err(io::Error::last_os_error());
    }
    removed_original(directory)
}

struct FactoryLease {
    index: usize,
    custody: Option<SetupCustody>,
    source: Option<io::Error>,
    owns_slot: bool,
    close_uncertain: bool,
}

impl FactoryLease {
    fn reserve(created: CreatedDirectory) -> io::Result<Self> {
        let mut slots = factory_slots();
        let index = slots
            .iter()
            .position(|slot| matches!(slot, FactorySlot::Vacant))
            .ok_or_else(|| io::Error::other("native domain custody slots are full"))?;
        slots[index] = FactorySlot::Reserved;
        Ok(Self {
            index,
            custody: Some(SetupCustody::Created(created)),
            source: None,
            owns_slot: true,
            close_uncertain: false,
        })
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if let Some(custody) = self.custody.as_mut() {
            if let Err(error) = custody.cleanup() {
                self.close_uncertain |= lost_close(&error);
                return Err(error);
            }
        }
        if let Some(custody) = self.custody.take() {
            let identity = custody.root_identity();
            let closed = custody.close_files();
            self.close_uncertain |= closed.is_err();
            if self.close_uncertain {
                factory_slots()[self.index] = FactorySlot::CloseUncertain(identity);
                self.owns_slot = false;
                return closed.and(Err(refuse(
                    "original failed factory descriptor close is uncertain",
                )));
            }
        }
        if self.close_uncertain {
            return Err(refuse(
                "original failed factory descriptor close is uncertain",
            ));
        }
        if self.owns_slot {
            factory_slots()[self.index] = FactorySlot::Vacant;
            self.owns_slot = false;
        }
        Ok(())
    }
}

impl Drop for FactoryLease {
    fn drop(&mut self) {
        if !self.owns_slot {
            return;
        }
        if matches!(self.custody.as_ref(), Some(SetupCustody::Created(created)) if !created.created)
        {
            // No kernel object was created. Exact pre-creation descriptors
            // still close through the same retained lease.
            let _ = self.cleanup();
            return;
        }
        let mut slots = factory_slots();
        slots[self.index] = match self.custody.take() {
            Some(custody) => FactorySlot::Held(RetainedFactoryFailure {
                custody,
                close_uncertain: self.close_uncertain,
                _source: self
                    .source
                    .take()
                    .unwrap_or_else(|| io::Error::other("original native factory owner abandoned")),
            }),
            None => FactorySlot::Vacant,
        };
    }
}

/// Recoverable original factory failure. Ignoring this value transfers all
/// unresolved custody and the actual cause into its pre-reserved fixed slot.
/// There is no conversion to an ordinary io::Error that could erase that owner.
pub struct NativeDomainCreateFailure {
    source: Option<io::Error>,
    lease: Option<FactoryLease>,
}

impl NativeDomainCreateFailure {
    pub(crate) fn close_checked_preclone_setup(&mut self) -> io::Result<bool> {
        let Some(lease) = self.lease.as_mut() else {
            // Early refusal does not retain an exact factory lease and
            // cannot prove every partial duplication was closed.
            return Ok(false);
        };
        lease.cleanup()?;
        Ok(true)
    }

    pub fn cleanup_pending(&self) -> bool {
        self.lease
            .as_ref()
            .is_some_and(|lease| lease.custody.is_some() || lease.close_uncertain)
    }

    pub fn retry_cleanup(&mut self) -> io::Result<bool> {
        if let Some(lease) = self.lease.as_mut() {
            lease.cleanup()?;
        }
        Ok(!self.cleanup_pending())
    }
}

impl From<io::Error> for NativeDomainCreateFailure {
    fn from(source: io::Error) -> Self {
        Self {
            source: Some(source),
            lease: None,
        }
    }
}

impl std::fmt::Debug for NativeDomainCreateFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for NativeDomainCreateFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "native factory refused (cleanup pending: {}): {}",
            self.cleanup_pending(),
            self.source.as_ref().expect("original cause retained")
        )
    }
}

impl std::error::Error for NativeDomainCreateFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl Drop for NativeDomainCreateFailure {
    fn drop(&mut self) {
        if let Some(mut lease) = self.lease.take() {
            lease.source = self.source.take();
            drop(lease);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeFactoryCleanupSummary {
    pub closed: usize,
    pub retained: usize,
}

fn setup_failure(source: io::Error, mut lease: FactoryLease) -> NativeDomainCreateFailure {
    lease.close_uncertain |= lost_close(&source);
    let _ = lease.cleanup();
    NativeDomainCreateFailure {
        source: Some(source),
        lease: Some(lease),
    }
}

fn validate_namespace_hierarchy(
    path: &Path,
    directory: &File,
    require_migration_write: bool,
) -> io::Result<()> {
    // clone3 checks the calling thread's source cgroup. A daemon's
    // thread-group leader alone cannot identify that migration ancestor.
    let membership = read_original(
        File::open("/proc/thread-self/cgroup")?,
        MEMBERSHIP_READ_LIMIT,
    )?;
    let membership = text(&membership)?
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| refuse("unified self cgroup membership unavailable"))?;
    let member_path = Path::new(membership);
    if !member_path.is_absolute()
        || member_path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(refuse("invalid self cgroup membership"));
    }
    let mounts = read_original(File::open("/proc/self/mountinfo")?, MOUNTINFO_READ_LIMIT)?;
    let identity = object(directory)?;
    for line in text(&mounts)?.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        if after.split_whitespace().next() != Some("cgroup2") {
            continue;
        }
        let mut fields = before.split_whitespace();
        let mount_id = fields.next().and_then(|v| v.parse::<u64>().ok());
        let _parent = fields.next();
        let _device = fields.next();
        let mount_root = fields.next();
        let Some(mount_point) = fields.next() else {
            continue;
        };
        if mount_id != Some(identity.mount) {
            continue;
        }
        if mount_root != Some("/") || mount_point.contains('\\') || !path.starts_with(mount_point) {
            return Err(refuse(
                "native root must be inside the current namespace-rooted cgroup2 mount",
            ));
        }
        let relative = membership.trim_start_matches('/');
        let self_path_len = mount_point.len() + 1 + relative.len();
        if self_path_len > MAX_CGROUP_PATH_BYTES {
            return Err(refuse(
                "self cgroup membership path exceeds the supported bound",
            ));
        }
        let mut self_path = PathBuf::with_capacity(self_path_len);
        self_path.push(mount_point);
        self_path.push(relative);
        let own = PinnedDirectory::open(&self_path)?;
        let checked = (|| {
            if own.identity.mount != identity.mount || own.identity.device != identity.device {
                return Err(refuse(
                    "self membership and delegated root differ in kernel hierarchy",
                ));
            }
            let procs = control(&own.directory, c"cgroup.procs", libc::O_RDONLY)?;
            let members = read_original(procs, PROCS_READ_LIMIT)?;
            let pid = u64::from(std::process::id());
            if !text(&members)?
                .lines()
                .any(|line| scalar(line.as_bytes()).ok() == Some(pid))
            {
                return Err(refuse(
                    "actual kernel self membership does not match namespace hierarchy",
                ));
            }
            Ok(())
        })();
        let closed = own.close();
        closed.and(checked)?;
        if require_migration_write {
            // The destination will be a newly created direct child of path.
            // Its common ancestor with the daemon is therefore the common
            // ancestor of the existing delegated parent and current membership.
            let ancestor = common_cgroup_ancestor(path, &self_path);
            if !ancestor.starts_with(mount_point) {
                return Err(refuse("common cgroup ancestor escapes the original mount"));
            }
            let held = PinnedDirectory::open(&ancestor)?;
            let checked = (|| {
                if held.identity.mount != identity.mount || held.identity.device != identity.device
                {
                    return Err(refuse("common cgroup ancestor differs in kernel hierarchy"));
                }
                let procs = control(&held.directory, c"cgroup.procs", libc::O_PATH)?;
                let allowed = require_effective_access(&procs, libc::W_OK);
                let closed = close_original(procs);
                closed.and(allowed)
            })();
            let closed = held.close();
            closed.and(checked)?;
        }
        return Ok(());
    }
    Err(refuse("no matching cgroup2 namespace mount"))
}

fn common_cgroup_ancestor(target_parent: &Path, self_path: &Path) -> PathBuf {
    let mut ancestor = PathBuf::with_capacity(
        target_parent
            .as_os_str()
            .len()
            .min(self_path.as_os_str().len()),
    );
    for (target_part, self_part) in target_parent.components().zip(self_path.components()) {
        if target_part != self_part {
            break;
        }
        ancestor.push(target_part.as_os_str());
    }
    ancestor
}

struct Controls {
    procs: File,
    kill: File,
    events: File,
    tasks: File,
    memory: File,
    memory_max: File,
    swap_max: File,
    oom_group: File,
    pids_max: File,
    pids_events: File,
    memory_events: File,
}

impl Controls {
    fn open(dir: &File, originals: &mut [Option<File>; CONTROL_FILES]) -> io::Result<Self> {
        for (slot, (name, flags)) in originals.iter_mut().zip([
            (c"cgroup.procs", libc::O_RDWR),
            (c"cgroup.kill", libc::O_WRONLY),
            (c"cgroup.events", libc::O_RDONLY),
            (c"pids.current", libc::O_RDONLY),
            (c"memory.current", libc::O_RDONLY),
            (c"memory.max", libc::O_RDWR),
            (c"memory.swap.max", libc::O_RDWR),
            (c"memory.oom.group", libc::O_RDWR),
            (c"pids.max", libc::O_RDWR),
            (c"pids.events", libc::O_RDONLY),
            (c"memory.events", libc::O_RDONLY),
        ]) {
            *slot = Some(open_at(dir.as_raw_fd(), name, flags)?);
            verify_control(dir, slot.as_ref().expect("original control recorded"))?;
        }
        let [procs, kill, events, tasks, memory, memory_max, swap_max, oom_group, pids_max, pids_events, memory_events] =
            originals
                .each_mut()
                .map(|slot| slot.take().expect("all original controls recorded"));
        Ok(Self {
            procs,
            kill,
            events,
            tasks,
            memory,
            memory_max,
            swap_max,
            oom_group,
            pids_max,
            pids_events,
            memory_events,
        })
    }

    fn configure(&self, limits: NativeDomainLimits) -> io::Result<()> {
        write_control(
            &self.memory_max,
            limits.memory_bytes().to_string().as_bytes(),
        )?;
        write_control(&self.swap_max, b"0")?;
        write_control(&self.oom_group, b"1")?;
        write_control(&self.pids_max, limits.tasks().to_string().as_bytes())?;
        self.verify_limits(limits)
    }

    fn verify_limits(&self, limits: NativeDomainLimits) -> io::Result<()> {
        if read_scalar(&self.memory_max)? != limits.memory_bytes()
            || read_scalar(&self.swap_max)? != 0
            || read_scalar(&self.oom_group)? != 1
            || read_scalar(&self.pids_max)? != u64::from(limits.tasks())
        {
            return Err(refuse("native limits readback differs from owned contract"));
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq)]
enum EntryState {
    Ready,
    Issued,
    Sealed,
    Removed,
}

/// Movable original native fields, retained by exactly one original slot owner.
struct NativeDomainCore {
    root: PinnedDirectory,
    directory: PinnedDirectory,
    files: Controls,
    namespace: File,
    namespace_identity: ObjectIdentity,
    limits: NativeDomainLimits,
    state: EntryState,
}

/// A sealed domain has no kernel tasks. Residual charged memory can remain;
/// this proves neither closure of pipes nor release of the whole graph budget.
pub struct NativeDomainExtinction {
    identity: NativeDomainIdentity,
    memory_current_bytes: u64,
}

impl NativeDomainExtinction {
    pub fn identity(&self) -> NativeDomainIdentity {
        self.identity
    }
    pub fn memory_current_bytes(&self) -> u64 {
        self.memory_current_bytes
    }
}

impl NativeDomainCore {
    /// Read the actual original leaf's hard-limit events. A memory limit
    /// hit is pressure even when the kernel has not killed a process.
    fn observed_trial_limit_pressure(&self) -> io::Result<bool> {
        self.verify()?;
        let pids = read_bounded(&self.files.pids_events, CONTROL_READ_LIMIT)?;
        let memory = read_bounded(&self.files.memory_events, CONTROL_READ_LIMIT)?;
        let pids_max = event_counter(&pids, "max")?;
        let memory_max = event_counter(&memory, "max")?;
        let oom_kills = event_counter(&memory, "oom_kill")?;
        Ok(pids_max != 0 || memory_max != 0 || oom_kills != 0)
    }

    /// Original-domain controller observation, never a resource upper bound.
    pub fn observed_pids_limit_events(&self) -> io::Result<u64> {
        self.verify()?;
        event_counter(
            &read_bounded(&self.files.pids_events, CONTROL_READ_LIMIT)?,
            "max",
        )
    }

    /// Original-domain controller observation, never proof of whole-graph
    /// closure or an exact memory peak.
    pub fn observed_oom_kill_events(&self) -> io::Result<u64> {
        self.verify()?;
        event_counter(
            &read_bounded(&self.files.memory_events, CONTROL_READ_LIMIT)?,
            "oom_kill",
        )
    }

    pub fn identity(&self) -> NativeDomainIdentity {
        NativeDomainIdentity::observed(self.directory.identity, self.namespace_identity)
    }

    pub fn limits(&self) -> NativeDomainLimits {
        self.limits
    }

    pub fn owned_memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.root.heap_bytes() + self.directory.heap_bytes()
    }

    fn verify(&self) -> io::Result<()> {
        if self.state == EntryState::Removed {
            return Err(refuse("native domain already removed"));
        }
        self.root.verify()?;
        self.directory.verify()?;
        validate_domain_type(&self.directory.directory)?;
        if object(&self.namespace)? != self.namespace_identity
            || current_namespace()? != self.namespace_identity
        {
            return Err(refuse("native cgroup namespace changed"));
        }
        Ok(())
    }

    fn empty(&self) -> io::Result<bool> {
        let events = read_bounded(&self.files.events, CONTROL_READ_LIMIT)?;
        let populated = populated(&events)?;
        let procs = read_bounded(&self.files.procs, CONTROL_READ_LIMIT)?;
        let tasks = read_scalar(&self.files.tasks)?;
        Ok(!populated && text(&procs)?.trim().is_empty() && tasks == 0)
    }

    fn verify_empty(&self) -> io::Result<()> {
        self.verify()?;
        if !self.empty()? {
            return Err(refuse("new native domain is not empty"));
        }
        Ok(())
    }

    pub(in crate::process) fn take_entry_seed(&mut self) -> io::Result<EntrySeed> {
        if self.state != EntryState::Ready {
            return Err(refuse("native entry seed already issued or sealed"));
        }
        self.verify_empty()?;
        self.files.verify_limits(self.limits)?;
        let file = self.directory.directory.try_clone()?;
        self.state = EntryState::Issued;
        Ok(EntrySeed { file })
    }

    /// Called only by the consuming atomic launcher after its command/entry
    /// token has been destroyed. Closes entry even on uncertain launch failure.
    pub(in crate::process) fn seal_entry(&mut self) {
        if self.state != EntryState::Removed {
            self.state = EntryState::Sealed;
        }
    }

    pub fn request_termination(&mut self) -> io::Result<()> {
        // An unused domain has never handed out entry authority. Stop seals
        // that case too, so it can be positively removed without a launcher.
        if self.state == EntryState::Ready {
            self.state = EntryState::Sealed;
        }
        // Even identity refusal cannot justify abandoning the original pinned
        // kill descriptor. It is safe to kill that owned object, never a path.
        let verified = self.verify();
        let killed = write_control(&self.files.kill, b"1");
        verified?;
        killed?;
        // A readable post-request snapshot is not a bound or an extinction
        // proof. Only try_extinction after entry is sealed can return proof.
        self.empty().map(|_| ())
    }

    pub fn try_extinction(&mut self) -> io::Result<Option<NativeDomainExtinction>> {
        if self.state != EntryState::Sealed {
            return Err(refuse("native entry must be sealed before extinction"));
        }
        self.verify()?;
        self.files.verify_limits(self.limits)?;
        if !self.empty()? {
            return Ok(None);
        }
        Ok(Some(NativeDomainExtinction {
            identity: self.identity(),
            memory_current_bytes: read_scalar(&self.files.memory)?,
        }))
    }

    /// Removal requires exclusive controller mutation of this parent/name;
    /// verification and unlink are separate kernel calls. Original-object
    /// zero-link readback is required before reporting Removed. Failure or an
    /// external replacement race retains uncertain original ownership.
    pub fn remove_extinct(&mut self, proof: NativeDomainExtinction) -> io::Result<()> {
        if proof.identity != self.identity() || self.state != EntryState::Sealed {
            return Err(refuse("extinction proof does not own this sealed domain"));
        }
        if removed_original(&self.directory.directory).is_ok() {
            self.state = EntryState::Removed;
            return Ok(());
        }
        self.verify_empty()?;
        if unsafe {
            libc::unlinkat(
                self.directory.parent.as_raw_fd(),
                self.directory.name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        removed_original(&self.directory.directory)?;
        self.state = EntryState::Removed;
        Ok(())
    }
}

impl NativeDomainCore {
    fn close_removed(self) -> io::Result<()> {
        let Controls {
            procs,
            kill,
            events,
            tasks,
            memory,
            memory_max,
            swap_max,
            oom_group,
            pids_max,
            pids_events,
            memory_events,
        } = self.files;
        // Once unlink and zero-link readback have both succeeded, consume
        // every original exactly once. A failed close must never be retried
        // by numeric descriptor or converted into successful closure.
        [
            self.root.directory,
            self.root.parent,
            self.directory.directory,
            self.directory.parent,
            self.namespace,
            procs,
            kill,
            events,
            tasks,
            memory,
            memory_max,
            swap_max,
            oom_group,
            pids_max,
            pids_events,
            memory_events,
        ]
        .into_iter()
        .fold(Ok(()), |result, file| result.and(close_original(file)))
    }
}

#[derive(Debug)]
struct NativeCloseUncertain(io::Error);
impl std::fmt::Display for NativeCloseUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "original native descriptor close is uncertain: {}",
            self.0
        )
    }
}
impl std::error::Error for NativeCloseUncertain {}
fn lost_close(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<NativeCloseUncertain>())
}

fn close_original(file: impl IntoRawFd) -> io::Result<()> {
    if unsafe { libc::close(file.into_raw_fd()) } == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        Err(io::Error::new(error.kind(), NativeCloseUncertain(error)))
    }
}

/// Original domain and its fixed custody slot remain together until positive
/// removal. The inner core is private and movable only for exact-owner cleanup;
/// callers cannot swap the native object independently of its original slot.
pub struct OwnedNativeDomain {
    core: Option<NativeDomainCore>,
    slot: Option<usize>,
    close_uncertain: bool,
}

impl OwnedNativeDomain {
    fn core(&self) -> &NativeDomainCore {
        self.core.as_ref().expect("live original domain")
    }
    fn core_mut(&mut self) -> &mut NativeDomainCore {
        self.core.as_mut().expect("live original domain")
    }

    pub fn identity(&self) -> NativeDomainIdentity {
        self.core().identity()
    }
    pub fn limits(&self) -> NativeDomainLimits {
        self.core().limits()
    }
    pub fn owned_memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.core().owned_memory_bytes()
            - std::mem::size_of::<NativeDomainCore>()
    }
    pub fn observed_pids_limit_events(&self) -> io::Result<u64> {
        self.core().observed_pids_limit_events()
    }
    pub fn observed_oom_kill_events(&self) -> io::Result<u64> {
        self.core().observed_oom_kill_events()
    }
    pub(crate) fn observed_trial_limit_pressure(&self) -> io::Result<bool> {
        self.core().observed_trial_limit_pressure()
    }
    pub(in crate::process) fn take_entry_seed(&mut self) -> io::Result<EntrySeed> {
        let result = self.core_mut().take_entry_seed();
        self.close_uncertain |= result.as_ref().is_err_and(lost_close);
        result
    }
    pub(in crate::process) fn seal_entry(&mut self) {
        self.core_mut().seal_entry();
    }
    pub fn request_termination(&mut self) -> io::Result<()> {
        let result = self
            .core
            .as_mut()
            .map_or(Ok(()), NativeDomainCore::request_termination);
        self.close_uncertain |= result.as_ref().is_err_and(lost_close);
        result
    }
    pub fn try_extinction(&mut self) -> io::Result<Option<NativeDomainExtinction>> {
        self.core
            .as_mut()
            .map_or(Ok(None), NativeDomainCore::try_extinction)
    }
    pub fn remove_extinct(&mut self, proof: NativeDomainExtinction) -> io::Result<()> {
        self.core_mut().remove_extinct(proof)
    }

    pub(in crate::process) fn try_close_original(&mut self) -> io::Result<bool> {
        if self.close_uncertain {
            return Err(refuse("original domain descriptor close is uncertain"));
        }
        let Some(core) = self.core.as_mut() else {
            return Ok(true);
        };
        let removed = (|| {
            if core.state != EntryState::Removed {
                let Some(proof) = core.try_extinction()? else {
                    return Ok(false);
                };
                core.remove_extinct(proof)?;
            }
            Ok(true)
        })();
        self.close_uncertain |= removed.as_ref().is_err_and(lost_close);
        if !removed? {
            return Ok(false);
        }
        let core = self.core.take().expect("original removed domain");
        let identity = core.root.identity;
        if let Err(error) = core.close_removed() {
            self.close_uncertain = true;
            if let Some(slot) = self.slot.take() {
                factory_slots()[slot] = FactorySlot::CloseUncertain(identity);
            }
            return Err(error);
        }
        if let Some(slot) = self.slot.take() {
            factory_slots()[slot] = FactorySlot::Vacant;
        }
        Ok(true)
    }
}

impl Drop for OwnedNativeDomain {
    fn drop(&mut self) {
        let Some(mut core) = self.core.take() else {
            return;
        };
        let Some(slot) = self.slot.take() else {
            return;
        };
        let cleanup = (|| {
            if core.state == EntryState::Removed {
                return Ok(());
            }
            // request_termination seals Ready only. An issued entry capability
            // cannot be closed merely because its Rust domain waiter dropped.
            core.request_termination()?;
            let proof = core
                .try_extinction()?
                .ok_or_else(|| refuse("native domain tasks remain"))?;
            core.remove_extinct(proof)
        })();
        match cleanup {
            Ok(()) => {
                let identity = core.root.identity;
                factory_slots()[slot] = if core.close_removed().is_ok() && !self.close_uncertain {
                    FactorySlot::Vacant
                } else {
                    FactorySlot::CloseUncertain(identity)
                };
            }
            Err(source) => {
                factory_slots()[slot] = FactorySlot::Held(RetainedFactoryFailure {
                    custody: SetupCustody::Configured(core),
                    close_uncertain: self.close_uncertain || lost_close(&source),
                    _source: source,
                });
            }
        }
    }
}

/// Private non-clone capability retained by one consuming launcher for
/// clone3(CLONE_INTO_CGROUP). There is no PID migration operation: the child
/// must be born in the configured domain, before any pre-exec callbacks.
pub(in crate::process) struct EntrySeed {
    file: File,
}

impl EntrySeed {
    pub(in crate::process) fn directory_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }

    pub(in crate::process) fn close_original(self) -> io::Result<()> {
        close_original(self.file)
    }
}

fn event_counter(bytes: &[u8], key: &str) -> io::Result<u64> {
    let mut value = None;
    for line in text(bytes)?.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some(key) {
            continue;
        }
        let next = scalar(
            fields
                .next()
                .ok_or_else(|| refuse("native event counter value absent"))?
                .as_bytes(),
        )?;
        if fields.next().is_some() || value.replace(next).is_some() {
            return Err(refuse("ambiguous native event counter"));
        }
    }
    value.ok_or_else(|| refuse("native event counter absent"))
}

fn populated(bytes: &[u8]) -> io::Result<bool> {
    let mut value = None;
    for line in text(bytes)?.lines() {
        let mut words = line.split_whitespace();
        if words.next() != Some("populated") {
            continue;
        }
        let next = match words.next() {
            Some("0") => false,
            Some("1") => true,
            _ => return Err(refuse("invalid cgroup populated value")),
        };
        if words.next().is_some() || value.replace(next).is_some() {
            return Err(refuse("ambiguous cgroup populated value"));
        }
    }
    value.ok_or_else(|| refuse("cgroup populated value absent"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Kernel fixture assertions fail the test on invalid setup."
)]
mod tests {
    use super::*;

    static FACTORY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Synthetic custody only: these tests exercise fixed retention and original
    // object readback, never claim a regular directory is native containment.
    fn uncreated_factory_fixture() -> CreatedDirectory {
        let root = PinnedDirectory::open(Path::new("/tmp")).unwrap();
        let namespace = File::open("/proc/self/ns/cgroup").unwrap();
        CreatedDirectory {
            parent: root.directory.try_clone().unwrap(),
            root,
            namespace_identity: object(&namespace).unwrap(),
            namespace,
            name: CString::new(format!("uncreated-fixture-{}", uuid::Uuid::new_v4())).unwrap(),
            directory: None,
            identity: None,
            controls: std::array::from_fn(|_| None),
            created: false,
        }
    }

    #[test]
    fn delegated_root_probe_rejects_non_cgroup_and_symlink_paths() {
        let original = tempfile::tempdir().expect("private delegation fixture");
        let linked = original.path().join("link");
        std::os::unix::fs::symlink(original.path(), &linked).unwrap();
        assert!(NativeDomainRoot::probe_finite_startup(original.path()).is_err());
        assert!(NativeDomainRoot::probe_finite_startup(&linked).is_err());
    }

    #[test]
    fn delegated_common_ancestor_compares_complete_cgroup_components() {
        let parent = Path::new("/sys/fs/cgroup/delegated/playback");
        assert_eq!(
            common_cgroup_ancestor(parent, Path::new("/sys/fs/cgroup/delegated/daemon")),
            Path::new("/sys/fs/cgroup/delegated"),
        );
        assert_eq!(
            common_cgroup_ancestor(
                parent,
                Path::new("/sys/fs/cgroup/delegated/playback/daemon")
            ),
            parent,
        );
        assert_eq!(
            common_cgroup_ancestor(parent, Path::new("/sys/fs/cgroup/delegated_other/daemon")),
            Path::new("/sys/fs/cgroup"),
        );
    }

    #[test]
    fn failed_control_validation_keeps_the_partial_original_descriptor() {
        let temporary = tempfile::tempdir().expect("private partial control fixture");
        let control_path = temporary.path().join("cgroup.procs");
        std::fs::write(&control_path, b"original").unwrap();
        let directory = File::open(temporary.path()).unwrap();
        let mut originals = std::array::from_fn(|_| None);
        assert!(Controls::open(&directory, &mut originals).is_err());
        let original = originals[0]
            .take()
            .expect("same acquired file before validation failure");
        assert_eq!(read_bounded(&original, 32).unwrap(), b"original");
        assert!(originals.iter().all(Option::is_none));
        close_original(original).expect("positive checked closure of the acquired file");
    }

    #[test]
    fn factory_slots_refuse_before_creation_and_release_proven_uncreated_leases() {
        let _serial = FACTORY_TEST_LOCK.lock().unwrap();
        let leases: Vec<_> = (0..MAX_NATIVE_DOMAINS)
            .map(|_| FactoryLease::reserve(uncreated_factory_fixture()).unwrap())
            .collect();
        assert!(FactoryLease::reserve(uncreated_factory_fixture()).is_err());
        assert!(leases.iter().all(|lease| matches!(lease.custody.as_ref(), Some(SetupCustody::Created(created)) if !created.created)));
        drop(leases);
        let retry = FactoryLease::reserve(uncreated_factory_fixture()).unwrap();
        drop(retry);
        assert!(factory_slots()
            .iter()
            .all(|slot| matches!(slot, FactorySlot::Vacant)));
    }

    #[test]
    fn ignored_factory_failure_retains_original_cause_and_same_unknown_owner() {
        let _serial = FACTORY_TEST_LOCK.lock().unwrap();
        let mut created = uncreated_factory_fixture();
        let original_root = created.root.identity;
        let original_name = created.name.clone();
        created.created = true;
        let lease = FactoryLease::reserve(created).unwrap();
        let slot = lease.index;
        // Wrong filesystem refuses cleanup and must retain synthetic custody.
        let mut failure = setup_failure(io::Error::from_raw_os_error(libc::ENOSPC), lease);
        assert!(failure.cleanup_pending());
        assert!(failure.retry_cleanup().is_err());
        drop(failure);
        let mut slots = factory_slots();
        let FactorySlot::Held(retained) = &slots[slot] else {
            panic!("ignored error must retain original custody");
        };
        assert_eq!(retained._source.raw_os_error(), Some(libc::ENOSPC));
        let SetupCustody::Created(created) = &retained.custody else {
            panic!("same pre-pin owner");
        };
        assert_eq!(created.root.identity, original_root);
        assert_eq!(created.name, original_name);
        assert!(created.directory.is_none());
        // Fixture never created a kernel object; explicit fixture teardown.
        slots[slot] = FactorySlot::Vacant;
    }

    fn unavailable_domain_fixture(state: EntryState) -> OwnedNativeDomain {
        let mut created = uncreated_factory_fixture();
        created.directory = Some(created.root.directory.try_clone().unwrap());
        created.identity = Some(created.root.identity);
        created.created = true;
        let mut lease = FactoryLease::reserve(created).unwrap();
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap();
        let files = Controls {
            procs: null.try_clone().unwrap(),
            kill: null.try_clone().unwrap(),
            events: null.try_clone().unwrap(),
            tasks: null.try_clone().unwrap(),
            memory: null.try_clone().unwrap(),
            memory_max: null.try_clone().unwrap(),
            swap_max: null.try_clone().unwrap(),
            oom_group: null.try_clone().unwrap(),
            pids_max: null.try_clone().unwrap(),
            pids_events: null.try_clone().unwrap(),
            memory_events: null,
        };
        let SetupCustody::Created(created) = lease.custody.take().unwrap() else {
            panic!("fixture creation");
        };
        let mut core = created.into_domain(files, NativeDomainLimits::new(4096, 1).unwrap());
        core.state = state;
        lease.owns_slot = false;
        OwnedNativeDomain {
            core: Some(core),
            slot: Some(lease.index),
            close_uncertain: false,
        }
    }

    #[test]
    fn domain_drop_retains_same_slot_on_unavailable_cleanup_and_never_seals_issued_entry() {
        let _serial = FACTORY_TEST_LOCK.lock().unwrap();
        for state in [EntryState::Ready, EntryState::Issued] {
            let issued = state == EntryState::Issued;
            let domain = unavailable_domain_fixture(state);
            let identity = domain.identity();
            let slot = domain.slot.unwrap();
            drop(domain);
            let mut slots = factory_slots();
            let FactorySlot::Held(retained) = &mut slots[slot] else {
                panic!("unknown drop must retain original slot");
            };
            let SetupCustody::Configured(core) = &retained.custody else {
                panic!("same native core");
            };
            assert_eq!(core.identity(), identity);
            assert!(
                core.state
                    == if issued {
                        EntryState::Issued
                    } else {
                        EntryState::Sealed
                    }
            );
            assert!(retained.custody.cleanup().is_err());
            // This fixture owns no actual cgroup or process; explicit teardown.
            slots[slot] = FactorySlot::Vacant;
        }
    }

    #[test]
    fn unlink_readback_requires_the_original_object_to_have_zero_links() {
        let base =
            std::env::temp_dir().join(format!("native-unlink-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let name = base.join("original");
        let moved = base.join("moved");
        std::fs::create_dir(&name).unwrap();
        let original = File::open(&name).unwrap();
        assert!(removed_original(&original).is_err());
        std::fs::rename(&name, &moved).unwrap();
        std::fs::create_dir(&name).unwrap();
        std::fs::remove_dir(&name).unwrap();
        assert!(removed_original(&original).is_err());
        std::fs::remove_dir(&moved).unwrap();
        removed_original(&original).unwrap();
        std::fs::remove_dir(&base).unwrap();
    }

    #[test]
    fn native_event_observations_refuse_missing_duplicate_and_nonfinite_values() {
        assert_eq!(event_counter(b"max 2\n", "max").unwrap(), 2);
        for bytes in [
            b"max max\n".as_slice(),
            b"max 1\nmax 2\n",
            b"max 1 extra\n",
            b"oom 1\n",
            b"max -1\n",
        ] {
            assert!(event_counter(bytes, "max").is_err());
        }
    }

    #[test]
    fn native_controls_refuse_missing_duplicate_or_nonfinite_values() {
        assert_eq!(scalar(b"42\n").unwrap(), 42);
        for bytes in [
            b"max".as_slice(),
            b"-1",
            b"+1",
            b"",
            b"18446744073709551616",
        ] {
            assert!(scalar(bytes).is_err());
        }
        assert!(!populated(b"populated 0\nfrozen 0\n").unwrap());
        assert!(populated(b"populated 1\n").unwrap());
        for bytes in [
            b"frozen 0\n".as_slice(),
            b"populated 2\n",
            b"populated 0\npopulated 0\n",
            b"populated 0 extra\n",
        ] {
            assert!(populated(bytes).is_err());
        }
    }

    #[test]
    fn regular_files_and_relative_roots_never_become_native_domains() {
        assert!(NativeDomainRoot::open(Path::new("relative")).is_err());
        assert!(NativeDomainRoot::open(Path::new("/tmp")).is_err());
    }

    #[test]
    #[ignore = "requires explicit writable delegated cgroup2 root; run with PLURX_TEST_CGROUP_ROOT and --ignored"]
    fn strict_native_empty_domain_configures_limits_and_proves_extinction() {
        let _serial = FACTORY_TEST_LOCK.lock().unwrap();
        let root =
            std::env::var_os("PLURX_TEST_CGROUP_ROOT").expect("explicit delegated root required");
        let root = NativeDomainRoot::open(Path::new(&root))
            .expect("actual delegated cgroup2 root required");
        let mut domain = root
            .create_domain(NativeDomainLimits::new(64 * 1024 * 1024, 8).unwrap())
            .unwrap();
        assert!(domain.try_extinction().is_err());
        domain.seal_entry();
        let proof = domain
            .try_extinction()
            .unwrap()
            .expect("new sealed domain is empty");
        assert_eq!(proof.identity(), domain.identity());
        domain.remove_extinct(proof).unwrap();
        assert!(domain.try_extinction().is_err());
    }

    #[test]
    #[ignore = "requires explicit writable delegated cgroup2 root; run with PLURX_TEST_CGROUP_ROOT and --ignored"]
    fn repeated_unused_domain_drop_removes_original_names_and_reuses_fixed_slots() {
        let _serial = FACTORY_TEST_LOCK.lock().unwrap();
        let path =
            std::env::var_os("PLURX_TEST_CGROUP_ROOT").expect("explicit delegated root required");
        let path = Path::new(&path);
        let root = NativeDomainRoot::open(path).expect("actual delegated root required");
        let names = || {
            std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let before = names();
        for _ in 0..(MAX_NATIVE_DOMAINS * 2 + 1) {
            let domain = root
                .create_domain(NativeDomainLimits::new(64 * 1024 * 1024, 8).unwrap())
                .unwrap();
            drop(domain);
            assert_eq!(names(), before);
        }
        assert_eq!(
            root.retry_failed_factory_cleanup().unwrap(),
            NativeFactoryCleanupSummary::default()
        );
    }
}
