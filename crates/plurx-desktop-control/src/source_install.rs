//! Explicit root-authorized local-source packages. Never a signed release route.
use crate::{
    controller,
    installation::*,
    launch_lock,
    native::ReleaseIdentityV1,
    package, process,
    protocol::*,
    secure_fs, supervisor,
    transaction::{self, JournalV1, Phase, TransactionStore},
    update, Error, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const INTENT: &str = "local-source-install.json";
const PENDING: &str = "local-source-installation.json";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyV1 {
    pub existing: bool,
    pub enabled: bool,
    pub running: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageV1 {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyV1 {
    pub path: PathBuf,
    pub sha256: String,
    pub backup: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateV1 {
    pub schema_version: u32,
    pub transaction_id: String,
    pub source_commit: String,
    pub version: String,
    pub packager: String,
    pub packages: Vec<PackageV1>,
    pub original_policy: PolicyV1,
    pub legacy_files: Vec<LegacyV1>,
}
fn fail(message: &str) -> Error {
    Error::new(ErrorCode::RecoveryRequired, message)
}
fn hex(value: &str, count: usize) -> bool {
    value.len() == count && value.bytes().all(|b| b.is_ascii_hexdigit())
}
impl CandidateV1 {
    pub fn validate(&self) -> Result<()> {
        let names: BTreeSet<_> = self.packages.iter().map(|p| p.name.as_str()).collect();
        if self.schema_version != 1
            || uuid::Uuid::parse_str(&self.transaction_id).is_err()
            || !hex(&self.source_commit, 40)
            || semver::Version::parse(&self.version).is_err()
            || !matches!(self.packager.as_str(), "deb" | "rpm")
            || self.packages.len() != 2
            || names != BTreeSet::from(["plurxd", "plurx-desktop"])
            || self.packages.iter().any(|p| {
                !p.path.is_absolute()
                    || !hex(&p.sha256, 64)
                    || p.size == 0
                    || p.size > update::MAX_ARTIFACT
            })
            || self.legacy_files.iter().any(|p| {
                !hex(&p.sha256, 64)
                    || !p.backup.is_absolute()
                    || !matches!(
                        p.path.to_str(),
                        Some(
                            "/etc/systemd/system/plurxd.service"
                                | "/etc/systemd/system/plurxd.service.d/10-prefix.conf"
                        )
                    )
            })
        {
            return Err(fail("invalid explicit local-source package intent"));
        }
        Ok(())
    }
    fn method(&self) -> InstallMethod {
        if self.packager == "deb" {
            InstallMethod::LinuxDeb
        } else {
            InstallMethod::LinuxRpm
        }
    }
    pub fn package_matches(&self, package: &str, version: &str) -> bool {
        self.packages.iter().any(|p| p.name == package)
            && (version.is_empty() || version == format!("{}-1", self.version))
    }
}
fn file_hash(path: &Path, owner: u32) -> Result<(u64, String)> {
    let parent = secure_fs::open_dir(
        path.parent().ok_or_else(|| fail("missing file parent"))?,
        owner,
    )?;
    let file = parent.open(
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| fail("file name"))?,
    )?;
    let length = file.metadata()?.len();
    if length > update::MAX_ARTIFACT {
        return Err(fail("local source file exceeds bound"));
    }
    let mut hash = Sha256::new();
    std::io::copy(
        &mut file.take(update::MAX_ARTIFACT + 1),
        &mut HashWriter(&mut hash),
    )?;
    Ok((length, hex::encode(hash.finalize())))
}
struct HashWriter<'a>(&'a mut Sha256);
impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn prototype(c: &CandidateV1) -> Result<InstallationV1> {
    let mut account: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65536];
    let code = unsafe {
        libc::getpwnam_r(
            c"plurx".as_ptr(),
            &mut account,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if code != 0 || found.is_null() || account.pw_uid == 0 {
        return Err(fail(
            "package preflight must create the dedicated plurx service account",
        ));
    }
    let i = InstallationV1 {
        schema_version: 1,
        installation_id: c.transaction_id.clone(),
        method: c.method(),
        owner_uid: 0,
        service_uid: account.pw_uid,
        architecture: native_architecture().ok_or_else(|| fail("unsupported architecture"))?,
        desktop_installed: true,
        repository: REPOSITORY.into(),
        current_version: c.version.clone(),
        current_tag: format!("v{}", c.version),
        current_commit: c.source_commit.clone(),
        controller_protocol: 1,
        trust_key_ids: update::production_keys()?
            .into_iter()
            .map(|k| k.id)
            .collect(),
        last_transaction_id: None,
        paths: ManagedPathsV1 {
            state: "/var/lib/plurx-control".into(),
            socket: "/run/plurx/desktop-control.sock".into(),
            runtime: "/usr/lib/plurx".into(),
            config: "/etc/plurx/plurx.toml".into(),
            data: "/var/lib/plurx".into(),
            log: "/var/log/plurxd.log".into(),
            canonical_plist: None,
            login_plist: None,
        },
        service: ServiceV1 {
            kind: SupervisorKind::Systemd,
            name: SYSTEM_UNIT.into(),
            domain: "system".into(),
        },
        browser_origin: "http://127.0.0.1:32400".into(),
    };
    i.validate()?;
    Ok(i)
}
/// Read-only fixed systemd observation also works before the vendor unit exists.
fn positively_stopped() -> Result<()> {
    let (code, text) = supervisor::output(
        supervisor::SYSTEMCTL,
        &[
            "show".into(),
            SYSTEM_UNIT.into(),
            "--property=ActiveState,MainPID".into(),
        ],
        Duration::from_secs(3),
    )?;
    let props: std::collections::BTreeMap<_, _> =
        text.lines().filter_map(|l| l.split_once('=')).collect();
    if code != 0
        || props.get("MainPID") != Some(&"0")
        || !matches!(props.get("ActiveState"), Some(&"inactive" | &"failed"))
    {
        return Err(fail(
            "source replacement requires a positively stopped service with no MainPID",
        ));
    }
    Ok(())
}
fn plan(
    i: &InstallationV1,
    c: &CandidateV1,
    dir: &secure_fs::SecureDir,
) -> Result<package::PackagePlan> {
    let mut paths = Vec::new();
    for p in &c.packages {
        let (size, hash) = file_hash(&p.path, 0)?;
        if size != p.size || hash != p.sha256 {
            return Err(fail(
                "local source package bytes differ from durable intent",
            ));
        }
        let name = format!("{}-source.{}", p.name, c.packager);
        // Copy into the root journal before downtime; manager arguments never use mutable user paths.
        let parent =
            secure_fs::open_dir(p.path.parent().ok_or_else(|| fail("package parent"))?, 0)?;
        dir.write(
            &name,
            &parent.read(
                p.path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| fail("package file"))?,
                update::MAX_ARTIFACT,
            )?,
            0o400,
        )?;
        paths.push(
            i.paths
                .state
                .join("transactions")
                .join(&c.transaction_id)
                .join(name),
        );
    }
    let mut args: Vec<String> = if c.packager == "deb" {
        vec![
            "-o",
            "DPkg::Lock::Timeout=0",
            "--no-download",
            "--no-remove",
            "--reinstall",
            "--assume-yes",
            "install",
        ]
    } else {
        vec![
            "--cacheonly",
            "--setopt=exit_on_lock=True",
            "--assumeyes",
            "install",
        ]
    }
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
    let plan = package::PackagePlan {
        program: if c.packager == "deb" {
            "/usr/bin/apt-get"
        } else {
            "/usr/bin/dnf"
        },
        args,
        packages: c
            .packages
            .iter()
            .map(|p| (p.name.clone(), format!("{}-1", c.version)))
            .collect(),
        paths,
    };
    for (p, path) in c.packages.iter().zip(&plan.paths) {
        let asset = update::ArtifactV1 {
            name: p.name.clone(),
            platform: update::Platform::Linux,
            architecture: i.architecture.clone(),
            format: if c.packager == "deb" {
                update::Format::Deb
            } else {
                update::Format::Rpm
            },
            component: if p.name == "plurxd" {
                update::Component::Server
            } else {
                update::Component::Desktop
            },
            package_name: p.name.clone(),
            package_version: format!("{}-1", c.version),
            size: p.size,
            sha256: p.sha256.clone(),
        };
        package::verify_package_identity(i, &asset, path)?;
    }
    // Source bytes are explicitly root-approved; no release key/signature is inferred.
    Ok(plan)
}
fn preflight_plan(plan: &package::PackagePlan) -> Result<()> {
    let mut args = plan.args.clone();
    if plan.program == "/usr/bin/apt-get" {
        args.insert(0, "--simulate".into());
        let (code, text) = supervisor::output(plan.program, &args, Duration::from_secs(60))?;
        if code != 0 || text.lines().any(|line| line.starts_with("Remv ")) {
            return Err(fail(
                "offline local-source package preflight failed before shutdown",
            ));
        }
    } else {
        let mut test: Vec<String> = vec![
            "--cacheonly".into(),
            "--setopt=tsflags=test".into(),
            "--assumeyes".into(),
            "install".into(),
        ];
        test.extend(plan.paths.iter().map(|p| p.to_string_lossy().into_owned()));
        let (code, _) = supervisor::output("/usr/bin/dnf", &test, Duration::from_secs(60))?;
        if code != 0 {
            return Err(fail("offline RPM prerequisite test failed before shutdown"));
        }
    }
    Ok(())
}
fn install_packages(plan: &package::PackagePlan, c: &CandidateV1) -> Result<i32> {
    if c.packager == "deb" {
        return package::install(plan);
    }
    // Fresh/missing packages need install; same-version source packages need
    // explicit reinstall. Keep server before desktop under the same journal.
    for name in ["plurxd", "plurx-desktop"] {
        let index = plan
            .packages
            .iter()
            .position(|(n, _)| n == name)
            .ok_or_else(|| fail("missing source cohort member"))?;
        let (code, version) = supervisor::output(
            "/usr/bin/rpm",
            &[
                "-q".into(),
                "--qf".into(),
                "%{VERSION}-%{RELEASE}".into(),
                name.into(),
            ],
            Duration::from_secs(10),
        )?;
        let action = if code == 0 && version.trim() == format!("{}-1", c.version) {
            "reinstall"
        } else {
            "install"
        };
        let mut one = plan.clone();
        one.args = vec![
            "--cacheonly".into(),
            "--setopt=exit_on_lock=True".into(),
            "--assumeyes".into(),
            action.into(),
            plan.paths[index].to_string_lossy().into_owned(),
        ];
        let exit = package::install(&one)?;
        if exit != 0 {
            return Ok(exit);
        }
    }
    Ok(0)
}

fn initial_journal(
    i: &InstallationV1,
    c: &CandidateV1,
    store: &TransactionStore,
    managed: bool,
) -> Result<JournalV1> {
    let mut j = if managed {
        store.create(i, Operation::Upgrade, None)?
    } else {
        positively_stopped()?;
        JournalV1 {
            schema_version: 1,
            transaction_id: c.transaction_id.clone(),
            installation_id: i.installation_id.clone(),
            operation: Operation::Upgrade,
            old_version: i.current_version.clone(),
            target_version: None,
            manifest_digest: None,
            phase: Phase::Prepared,
            initial_running: c.original_policy.running,
            initial_startup: StartupStateV1::new(if c.original_policy.enabled {
                StartupState::Enabled
            } else {
                StartupState::Disabled
            }),
            policy_revision: String::new(),
            config_digest: transaction::config_digest(i)?,
            process: None,
            shutdown_generation: None,
            clean_receipt: None,
            rollback: update::Rollback::ManualRecovery,
            worker_pid: None,
            worker_generation: None,
            capsule_sha256: None,
            package_inventory_before: None,
            package_inventory_after: None,
            detail: None,
            expected_node_id: None,
            start_permit_expires: None,
        }
    };
    if managed && j.transaction_id != c.transaction_id {
        // Store::create generates its own UUID; retain that generated preparation as cancelled
        // and use the durable source intent's UUID for every retry/hook/start interlock.
        j.transition(Phase::Cancelled)?;
        store.save(&j)?;
        j.transaction_id = c.transaction_id.clone();
        j.phase = Phase::Prepared;
    }
    if managed
        && (!matches!(
            j.initial_startup.state,
            StartupState::Enabled | StartupState::Disabled
        ) || j.initial_running != c.original_policy.running
            || (j.initial_startup.state == StartupState::Enabled) != c.original_policy.enabled)
    {
        return Err(fail(
            "managed startup/running policy differs from durable source intent",
        ));
    }
    j.target_version = Some(c.version.clone());
    j.rollback = update::Rollback::ManualRecovery;
    let me = process::inspect(std::process::id())?;
    j.worker_pid = Some(me.pid);
    j.worker_generation = Some(me.generation);
    let dir = store.transactions.child(&c.transaction_id, true)?;
    dir.write_json("installation-before.json", i)?;
    dir.write_json(INTENT, c)?;
    dir.write_json("journal.json", &j)?;
    store.root.write_json(
        "active.json",
        &serde_json::json!({"schema_version":1,"transaction_id":c.transaction_id}),
    )?;
    store.save(&j)?;
    Ok(j)
}
/// Package hooks may join only this exact root process generation and explicit source cohort.
pub fn hook_matches(
    i: &InstallationV1,
    j: &JournalV1,
    package: &str,
    version: &str,
) -> Result<bool> {
    let dir = i
        .state_dir()?
        .child("transactions", false)?
        .child(&j.transaction_id, false)?;
    let candidate: CandidateV1 = dir.json(INTENT)?;
    candidate.validate()?;
    if candidate.transaction_id != j.transaction_id
        || j.operation != Operation::Upgrade
        || j.manifest_digest.is_some()
        || j.target_version.as_deref() != Some(candidate.version.as_str())
        || candidate.method() != i.method
        || !candidate.package_matches(package, version)
    {
        return Err(fail("hook is outside exact local-source transaction"));
    }
    Ok(true)
}
/// Fresh package hooks use a temporary root intent, never a fabricated installed record.
pub fn pending_installation() -> Result<Option<InstallationV1>> {
    let root = match secure_fs::open_dir(Path::new("/var/lib/plurx-control"), 0) {
        Ok(r) => r,
        Err(e) if e.os_error == Some(libc::ENOENT) => return Ok(None),
        Err(e) => return Err(e),
    };
    match root.json::<InstallationV1>(PENDING) {
        Ok(i) => {
            i.validate()?;
            Ok(Some(i))
        }
        Err(e) if e.os_error == Some(libc::ENOENT) => Ok(None),
        Err(e) => Err(e),
    }
}
fn old_packages(i: &InstallationV1) -> Vec<(String, String)> {
    let mut names = vec![("plurxd".into(), format!("{}-1", i.current_version))];
    if i.desktop_installed {
        names.push(("plurx-desktop".into(), format!("{}-1", i.current_version)));
    }
    names
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeBeforeV1 {
    files: Vec<(PathBuf, Option<(u64, String)>)>,
    inventory: String,
}
fn runtime_snapshot(i: &InstallationV1) -> Result<RuntimeBeforeV1> {
    let mut paths = vec![
        crate::native::daemon_path(i)?,
        PathBuf::from("/usr/lib/plurx-control/release.json"),
        "/usr/lib/systemd/system/plurxd.service".into(),
        "/usr/lib/systemd/system/plurx-recovery.service".into(),
        "/usr/share/polkit-1/actions/tv.plurx.install.policy".into(),
        "/usr/bin/plurx-tray".into(),
        "/etc/xdg/autostart/tv.plurx.Server-autostart.desktop".into(),
        "/usr/share/applications/tv.plurx.Server.desktop".into(),
    ];
    paths.extend(
        crate::native::CONTROL_BINARIES
            .iter()
            .map(|name| Path::new("/usr/lib/plurx-control").join(name)),
    );
    let files = paths
        .into_iter()
        .map(|path| {
            let fact = match std::fs::symlink_metadata(&path) {
                Ok(_) => Some(file_hash(&path, 0)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            Ok((path, fact))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RuntimeBeforeV1 {
        files,
        inventory: package::inventory(i, &old_packages(i))?,
    })
}

fn unchanged(i: &InstallationV1, store: &TransactionStore, j: &JournalV1) -> Result<()> {
    let dir = store.dir(&j.transaction_id)?;
    let before: RuntimeBeforeV1 = dir.json("old-runtime.json")?;
    if runtime_snapshot(i)? != before {
        return Err(fail("old runtime or inventory changed; restoration held"));
    }
    transaction::revalidate_policy(i, j)?;
    Ok(())
}
fn restore_unchanged(
    i: &InstallationV1,
    store: &TransactionStore,
    j: &mut JournalV1,
) -> Result<()> {
    let before = j.clone();
    restore_unchanged_with(
        store,
        j,
        || unchanged(i, store, &before),
        |journal| {
            controller::authorize_start(store, journal)?;
            supervisor::start(i)?;
            controller::wait_ready(i, journal, Some(&i.current_commit))
        },
    )
}
fn restore_unchanged_with<V, R>(
    store: &TransactionStore,
    j: &mut JournalV1,
    verify: V,
    restore: R,
) -> Result<()>
where
    V: FnOnce() -> Result<()>,
    R: FnOnce(&mut JournalV1) -> Result<()>,
{
    verify()?;
    j.transition(Phase::Restoring)?;
    store.save(j)?;
    if j.initial_running {
        restore(j)?;
    }
    j.start_permit_expires = None;
    j.transition(Phase::Cancelled)?;
    store.save(j)
}
/// Intentionally public fixture seam: never treats a lifecycle request alone as readiness.
pub fn verify_installed_identity(
    c: &CandidateV1,
    i: &InstallationV1,
    release: &ReleaseIdentityV1,
) -> Result<()> {
    if release.source_commit != c.source_commit
        || release.version != c.version
        || release.architecture != i.architecture
        || release.repository != REPOSITORY
    {
        return Err(fail(
            "installed source identity differs from exact durable candidate",
        ));
    }
    Ok(())
}
pub fn run(candidate_path: &Path) -> Result<ControlResultV1> {
    if !cfg!(target_os = "linux") || native_uid() != 0 {
        return Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "local source installation requires an explicit native root CLI invocation",
        ));
    }
    let candidate: CandidateV1 = secure_fs::read_json(candidate_path, 0)?;
    candidate.validate()?;
    let record = default_record()?;
    let old = match InstallationV1::load(&record) {
        Ok(i) => Some(i),
        Err(e) if e.os_error == Some(libc::ENOENT) => None,
        Err(e) => return Err(e),
    };
    let managed = old.is_some();
    let i = match old {
        Some(i) => i,
        None => prototype(&candidate)?,
    };
    if i.method != candidate.method() {
        return Err(fail(
            "source package method differs from native installation",
        ));
    }
    let store = TransactionStore::locked(&i)?;
    if let Some(active) = store.active()? {
        if active.transaction_id == candidate.transaction_id {
            let saved: CandidateV1 = store.dir(&active.transaction_id)?.json(INTENT)?;
            if saved != candidate {
                return Err(fail("retry candidate differs from durable original intent"));
            }
            if active.phase == Phase::Committed {
                let plan = plan(&i, &candidate, &store.dir(&active.transaction_id)?)?;
                let inventory = package::inventory(&i, &plan.packages)?;
                package::validate_installed(&i, &plan, &inventory)?;
                let release: ReleaseIdentityV1 =
                    secure_fs::read_json(Path::new("/usr/lib/plurx-control/release.json"), 0)?;
                release.validate()?;
                verify_installed_identity(&candidate, &i, &release)?;
                let observed = supervisor::observe(&i)?;
                if (observed.startup.state == StartupState::Enabled)
                    != candidate.original_policy.enabled
                {
                    return Err(fail(
                        "startup policy changed after committed source installation",
                    ));
                }
                if candidate.original_policy.running {
                    controller::wait_ready(&i, &active, Some(&candidate.source_commit))?;
                } else {
                    positively_stopped()?;
                }
                return controller::status(&i);
            }
            if active.phase == Phase::Cancelled {
                store
                    .dir(&active.transaction_id)?
                    .write_json("previous-attempt.json", &active)?;
            }
        }
        if !active.terminal() {
            return Err(fail("unfinished source/native transaction retains original intent; explicit recovery required before retry"));
        }
    }
    launch_lock::initialize(&i)?;
    let mut j = initial_journal(&i, &candidate, &store, managed)?;
    let result: Result<ControlResultV1> = (|| {
        let dir = store.dir(&j.transaction_id)?;
        let plan = plan(&i, &candidate, &dir)?;
        if managed {
            dir.write_json("old-runtime.json", &runtime_snapshot(&i)?)?;
        }
        // Both manager preflight and source package bytes have settled before downtime.
        crate::package_locks::check(&i)?;
        preflight_plan(&plan)?;
        if managed {
            transaction::stop_clean(&i, &store, &mut j)?;
        } else {
            positively_stopped()?;
            j.transition(Phase::CleanStop)?;
            store.save(&j)?;
        }
        let install_lock = launch_lock::acquire(&i, true)?;
        positively_stopped()?;
        if let Err(error) = crate::package_locks::check(&i) {
            drop(install_lock);
            if managed {
                restore_unchanged(&i, &store, &mut j)?;
            }
            return Err(error);
        }
        // Hooks for fresh registrations join this intent without creating installation.json early.
        store.root.write_json(PENDING, &i)?;
        j.transition(Phase::Installing)?;
        store.save(&j)?;
        let exit = install_packages(&plan, &candidate)?;
        if exit != 0 {
            if managed && unchanged(&i, &store, &j).is_ok() {
                // The process failed before any selected runtime/inventory change.
                // Preserve proof before moving back across the mutation boundary.
                dir.write_json(
                    "unchanged-after-manager-failure.json",
                    &serde_json::json!({"exit":exit,"source_commit":i.current_commit}),
                )?;
                j.phase = Phase::CleanStop;
                store.save(&j)?;
                drop(install_lock);
                restore_unchanged(&i, &store, &mut j)?;
                return Err(fail("package manager failed without runtime mutation; verified original running/startup state restored"));
            }
            return Err(fail("package manager failed after mutation boundary; retained source intent requires package repair"));
        }
        let inventory = package::inventory(&i, &plan.packages)?;
        j.package_inventory_after = Some(inventory.clone());
        store.save(&j)?;
        package::validate_installed(&i, &plan, &inventory)?;
        let release: ReleaseIdentityV1 =
            secure_fs::read_json(Path::new("/usr/lib/plurx-control/release.json"), 0)?;
        release.validate()?;
        verify_installed_identity(&candidate, &i, &release)?;
        supervisor::reload_after_package_install(&i)?;
        let mut installed = i.clone();
        installed.current_version = candidate.version.clone();
        installed.current_tag = format!("v{}", candidate.version);
        installed.current_commit = candidate.source_commit.clone();
        installed.desktop_installed = true;
        installed.last_transaction_id = Some(j.transaction_id.clone());
        controller::save_installation(&installed)?;
        j.transition(Phase::CandidateInstalled)?;
        store.save(&j)?;
        supervisor::set_startup(&installed, candidate.original_policy.enabled)?;
        let observed = supervisor::observe(&installed)?;
        j.initial_startup = observed.startup;
        j.policy_revision = observed.policy_revision;
        j.config_digest = transaction::config_digest(&installed)?;
        j.transition(Phase::Restoring)?;
        store.save(&j)?;
        drop(install_lock);
        if candidate.original_policy.running {
            controller::authorize_start(&store, &mut j)?;
            supervisor::start(&installed)?;
            controller::wait_ready(&installed, &j, Some(&candidate.source_commit))?;
        } else {
            positively_stopped()?;
        }
        j.start_permit_expires = None;
        j.transition(Phase::Committed)?;
        store.save(&j)?;
        controller::status(&installed)
    })();
    if let Err(error) = &result {
        j.detail = Some(error.message.clone());
        if j.phase == Phase::Prepared {
            j.transition(Phase::Cancelled)?;
        }
        store.save(&j)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/source-install-policy-tests")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&path).expect("fixture");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("private fixture");
            Self(path.canonicalize().expect("canonical fixture"))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn candidate() -> CandidateV1 {
        CandidateV1 {
            schema_version: 1,
            transaction_id: uuid::Uuid::new_v4().to_string(),
            source_commit: "b".repeat(40),
            version: "0.7.1".into(),
            packager: "deb".into(),
            packages: ["plurxd", "plurx-desktop"]
                .into_iter()
                .map(|name| PackageV1 {
                    name: name.into(),
                    path: Path::new("/var/lib/plurx-source-installer/fixture")
                        .join(format!("{name}.deb")),
                    sha256: "c".repeat(64),
                    size: 7,
                })
                .collect(),
            original_policy: PolicyV1 {
                existing: true,
                enabled: true,
                running: true,
            },
            legacy_files: vec![],
        }
    }
    fn installation(root: &Path) -> InstallationV1 {
        InstallationV1 {
            schema_version: 1,
            installation_id: uuid::Uuid::new_v4().to_string(),
            method: if cfg!(target_os = "macos") {
                InstallMethod::MacosNative
            } else {
                InstallMethod::LinuxDeb
            },
            owner_uid: native_uid(),
            service_uid: native_uid(),
            architecture: native_architecture().expect("native arch"),
            desktop_installed: true,
            repository: REPOSITORY.into(),
            current_version: "0.7.0".into(),
            current_tag: "v0.7.0".into(),
            current_commit: "a".repeat(40),
            controller_protocol: 1,
            trust_key_ids: vec![],
            last_transaction_id: None,
            paths: ManagedPathsV1 {
                state: root.to_path_buf(),
                socket: root.join("control.sock"),
                runtime: root.join("runtime"),
                config: root.join("config.toml"),
                data: root.join("data"),
                log: root.join("daemon.log"),
                canonical_plist: None,
                login_plist: None,
            },
            service: ServiceV1 {
                kind: if cfg!(target_os = "macos") {
                    SupervisorKind::Launchd
                } else {
                    SupervisorKind::Systemd
                },
                name: SYSTEM_UNIT.into(),
                domain: "fixture".into(),
            },
            browser_origin: "http://127.0.0.1:32400".into(),
        }
    }
    fn journal(i: &InstallationV1, c: &CandidateV1) -> JournalV1 {
        JournalV1 {
            schema_version: 1,
            transaction_id: c.transaction_id.clone(),
            installation_id: i.installation_id.clone(),
            operation: Operation::Upgrade,
            old_version: i.current_version.clone(),
            target_version: Some(c.version.clone()),
            manifest_digest: None,
            phase: Phase::CleanStop,
            initial_running: true,
            initial_startup: StartupStateV1::new(StartupState::Enabled),
            policy_revision: "original".into(),
            config_digest: "d".repeat(64),
            process: None,
            shutdown_generation: None,
            clean_receipt: None,
            rollback: update::Rollback::ManualRecovery,
            worker_pid: None,
            worker_generation: None,
            capsule_sha256: None,
            package_inventory_before: None,
            package_inventory_after: None,
            detail: None,
            expected_node_id: Some("original-node".into()),
            start_permit_expires: None,
        }
    }
    fn persist(store: &TransactionStore, c: &CandidateV1, j: &JournalV1) {
        let dir = store
            .transactions
            .child(&j.transaction_id, true)
            .expect("transaction");
        dir.write_json(INTENT, c).expect("source intent");
        dir.write_json("journal.json", j).expect("journal");
        store
            .root
            .write_json(
                "active.json",
                &serde_json::json!({"schema_version":1,"transaction_id":j.transaction_id}),
            )
            .expect("active");
    }
    #[test]
    fn source_intent_is_finite_explicit_and_cannot_authorize_release_or_other_packages() {
        let c = candidate();
        c.validate().expect("explicit source cohort");
        assert!(c.package_matches("plurxd", "0.7.1-1"));
        assert!(!c.package_matches("plurxd", "0.7.0-1"));
        assert!(!c.package_matches("systemd", ""));
        for case in 0..5 {
            let mut bad = c.clone();
            match case {
                0 => bad.packager = "shell".into(),
                1 => bad.packages[0].name = "systemd".into(),
                2 => bad.source_commit = "short".into(),
                3 => bad.packages[0].path = "relative.deb".into(),
                _ => bad.legacy_files.push(LegacyV1 {
                    path: "/etc/passwd".into(),
                    sha256: "d".repeat(64),
                    backup: "/safe/backup".into(),
                }),
            };
            assert!(bad.validate().is_err(), "case {case}");
        }
        let mut value = serde_json::to_value(c).expect("serialize");
        value["command"] = serde_json::json!("sh");
        assert!(serde_json::from_value::<CandidateV1>(value).is_err());
    }
    #[test]
    fn source_transaction_holds_start_and_package_replacement_cannot_overlap_daemon_lock() {
        let scratch = Scratch::new();
        let i = installation(&scratch.0);
        let c = candidate();
        let j = journal(&i, &c);
        launch_lock::initialize(&i).expect("fixture launch lock");
        let shared = launch_lock::acquire(&i, false).expect("old daemon shared lock");
        assert_eq!(
            launch_lock::acquire(&i, true)
                .expect_err("replacement refused while daemon owns lock")
                .code,
            ErrorCode::Busy
        );
        drop(shared);
        let store = TransactionStore::locked(&i).expect("coordination");
        persist(&store, &c, &j);
        assert_eq!(
            controller::lifecycle(&i, Operation::Start)
                .expect_err("tray start interlocked before unpack")
                .code,
            ErrorCode::Busy
        );
        let exclusive = launch_lock::acquire(&i, true).expect("stopped replacement lock");
        assert_eq!(
            launch_lock::acquire(&i, false)
                .expect_err("wrapper cannot enter package mutation")
                .code,
            ErrorCode::Busy
        );
        drop(store);
        assert_eq!(
            controller::lifecycle(&i, Operation::Start)
                .expect_err("nonterminal durable intent holds later start")
                .code,
            ErrorCode::RecoveryRequired
        );
        drop(exclusive);
    }
    #[test]
    fn source_manager_failure_restores_only_verified_unchanged_runtime_and_retains_original_intent()
    {
        let scratch = Scratch::new();
        let i = installation(&scratch.0);
        let c = candidate();
        let mut j = journal(&i, &c);
        let store = TransactionStore::locked(&i).expect("coordination");
        persist(&store, &c, &j);
        let restored = std::cell::Cell::new(false);
        restore_unchanged_with(
            &store,
            &mut j,
            || Ok(()),
            |old| {
                assert!(old.initial_running);
                assert_eq!(old.initial_startup.state, StartupState::Enabled);
                assert_eq!(old.expected_node_id.as_deref(), Some("original-node"));
                restored.set(true);
                Ok(())
            },
        )
        .expect("verified old runtime restored");
        assert!(restored.get());
        assert_eq!(j.phase, Phase::Cancelled);
        let retained: CandidateV1 = store
            .dir(&j.transaction_id)
            .expect("transaction")
            .json(INTENT)
            .expect("original source policy retained");
        assert_eq!(retained.original_policy, c.original_policy);
        j.phase = Phase::CleanStop;
        store.save(&j).expect("second fixture");
        assert!(restore_unchanged_with(
            &store,
            &mut j,
            || Err(fail("partial package mutation")),
            |_| panic!("must not restore changed runtime")
        )
        .is_err());
        assert_eq!(j.phase, Phase::CleanStop);
    }
    #[test]
    fn source_partial_mutation_retry_keeps_original_policy_and_requires_recovery() {
        let scratch = Scratch::new();
        let i = installation(&scratch.0);
        let c = candidate();
        let mut j = journal(&i, &c);
        j.phase = Phase::Installing;
        {
            let store = TransactionStore::locked(&i).expect("coordination");
            persist(&store, &c, &j);
        }
        let store = TransactionStore::locked(&i).expect("retry coordination");
        let durable = store
            .active()
            .expect("durable active")
            .expect("source operation");
        assert_eq!(
            durable.recovery_action(),
            transaction::RecoveryAction::HoldPackageRepair
        );
        assert!(durable.initial_running);
        assert_eq!(durable.initial_startup.state, StartupState::Enabled);
        let retained: CandidateV1 = store
            .dir(&c.transaction_id)
            .expect("transaction")
            .json(INTENT)
            .expect("source intent");
        assert_eq!(retained, c);
        drop(store);
        assert_eq!(
            controller::lifecycle(&i, Operation::Start)
                .expect_err("partial mutation cannot start")
                .code,
            ErrorCode::RecoveryRequired
        );
    }
    #[test]
    fn source_success_requires_exact_ready_version_commit_and_original_node() {
        let scratch = Scratch::new();
        let i = installation(&scratch.0);
        let c = candidate();
        let j = journal(&i, &c);
        let mut id = DaemonIdentityV1 {
            schema_version: 1,
            installation_id: i.installation_id.clone(),
            process: ProcessIdentityV1 {
                pid: 1,
                uid: i.service_uid,
                start_generation: "fixture".into(),
                nonce: "fixture".into(),
            },
            node_id: "original-node".into(),
            instance_id: "fixture".into(),
            version: i.current_version.clone(),
            build: i.current_commit.clone(),
            browser_origin: i.browser_origin.clone(),
            supervisor: SupervisorIdentityV1 {
                kind: i.service.kind.clone(),
                name: i.service.name.clone(),
                invocation_id: "fixture".into(),
            },
            ready: true,
        };
        assert!(controller::readiness_matches(
            &i,
            &j,
            &id,
            Some(&i.current_commit)
        ));
        id.ready = false;
        assert!(!controller::readiness_matches(
            &i,
            &j,
            &id,
            Some(&i.current_commit)
        ));
        id.ready = true;
        id.build = "wrong-source".into();
        assert!(!controller::readiness_matches(
            &i,
            &j,
            &id,
            Some(&i.current_commit)
        ));
        id.build = i.current_commit.clone();
        id.node_id = "other-node".into();
        assert!(!controller::readiness_matches(
            &i,
            &j,
            &id,
            Some(&i.current_commit)
        ));
        id.node_id = "original-node".into();
        id.version = "wrong-version".into();
        assert!(!controller::readiness_matches(
            &i,
            &j,
            &id,
            Some(&i.current_commit)
        ));
        let mut release = ReleaseIdentityV1 {
            schema_version: 1,
            repository: REPOSITORY.into(),
            version: c.version.clone(),
            source_commit: c.source_commit.clone(),
            architecture: i.architecture.clone(),
        };
        verify_installed_identity(&c, &i, &release).expect("exact stopped inventory identity");
        release.source_commit = "e".repeat(40);
        assert!(verify_installed_identity(&c, &i, &release).is_err());
    }
    #[test]
    fn fresh_source_intent_can_digest_a_configuration_before_its_parent_exists() {
        let scratch = Scratch::new();
        let mut i = installation(&scratch.0);
        i.paths.config = scratch.0.join("not-created/config/plurx.toml");
        assert!(!i.paths.config.parent().expect("parent").exists());
        let absent =
            transaction::config_digest(&i).expect("missing parent represents absent configuration");
        assert_eq!(
            absent,
            transaction::config_digest(&i).expect("stable missing configuration")
        );
        fs::create_dir_all(i.paths.config.parent().expect("parent"))
            .expect("package creates parent");
        assert_eq!(
            absent,
            transaction::config_digest(&i).expect("absent file has same identity")
        );
        fs::write(&i.paths.config, "[server]\n").expect("package configuration");
        assert_ne!(
            absent,
            transaction::config_digest(&i).expect("present configuration bound")
        );
    }
}
