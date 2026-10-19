//! Offline exact-cohort package operations. Never kills an installing package manager.
pub use crate::package_script::run as package_script;
use crate::{
    installation::{Architecture, InstallMethod, InstallationV1},
    protocol::ErrorCode,
    supervisor,
    transaction::{JournalV1, Phase, TransactionStore},
    update::{self, ArtifactV1, VerifiedManifest},
    Error, Result,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct PackagePlan {
    pub program: &'static str,
    pub args: Vec<String>,
    pub packages: Vec<(String, String)>,
    pub paths: Vec<PathBuf>,
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}
pub fn rpm_versions_match(actual: &str, expected: &str) -> bool {
    let split = |s: &str| {
        let (epoch, version) = s.split_once(':').unwrap_or(("0", s));
        (epoch.parse::<u64>().ok(), version.to_string())
    };
    let (a, av) = split(actual);
    let (e, ev) = split(expected);
    a.is_some() && a == e && av == ev
}
fn package_arch(i: &InstallationV1) -> &'static str {
    match (&i.method, &i.architecture) {
        (InstallMethod::LinuxDeb, Architecture::Amd64) => "amd64",
        (InstallMethod::LinuxDeb, Architecture::Arm64) => "arm64",
        (_, Architecture::Amd64) => "x86_64",
        (_, Architecture::Arm64) => "aarch64",
        _ => "unsupported",
    }
}
pub fn plan(i: &InstallationV1, candidate: &VerifiedManifest, id: &str) -> Result<PackagePlan> {
    uuid::Uuid::parse_str(id)
        .map_err(|_| Error::new(ErrorCode::InvalidRequest, "invalid transaction ID"))?;
    let assets = update::select_cohort(candidate, i)?;
    let paths: Vec<_> = assets
        .iter()
        .map(|a| i.paths.state.join("transactions").join(id).join(&a.name))
        .collect();
    let (program, mut args) = match i.method {
        InstallMethod::LinuxDeb => (
            "/usr/bin/apt-get",
            strings(&[
                "-o",
                "DPkg::Lock::Timeout=0",
                "-o",
                "APT::Get::AllowUnauthenticated=false",
                "--no-download",
                "--no-remove",
                "--no-install-recommends",
                "--assume-yes",
                "install",
            ]),
        ),
        InstallMethod::LinuxRpm => (
            "/usr/bin/dnf",
            strings(&[
                "--cacheonly",
                "--disablerepo=*",
                "--setopt=exit_on_lock=True",
                "--setopt=install_weak_deps=False",
                "--assumeyes",
                "install",
            ]),
        ),
        _ => {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "not a system package installation",
            ))
        }
    };
    for p in &paths {
        args.push(
            p.to_str()
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::InvalidInstallation,
                        "invalid package path encoding",
                    )
                })?
                .into(),
        );
    }
    Ok(PackagePlan {
        program,
        args,
        packages: assets
            .iter()
            .map(|a| (a.package_name.clone(), a.package_version.clone()))
            .collect(),
        paths,
    })
}
pub fn inventory(i: &InstallationV1, packages: &[(String, String)]) -> Result<String> {
    let (program, mut args) = if i.method == InstallMethod::LinuxDeb {
        (
            "/usr/bin/dpkg-query",
            strings(&[
                "-W",
                "-f=${binary:Package}\t${Version}\t${Architecture}\t${db:Status-Status}\n",
            ]),
        )
    } else {
        (
            "/usr/bin/rpm",
            strings(&[
                "-q",
                "--qf",
                "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}\t%{ARCH}\n",
            ]),
        )
    };
    args.extend(packages.iter().map(|(n, _)| n.clone()));
    let (code, text) = supervisor::output(program, &args, Duration::from_secs(5))?;
    if code != 0 {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "selected installed package inventory unavailable; explicit repair required",
        ));
    }
    Ok(text)
}
pub fn validate_installed(i: &InstallationV1, plan: &PackagePlan, text: &str) -> Result<()> {
    for (name, version) in &plan.packages {
        let matches: Vec<_> = text
            .lines()
            .filter(|l| {
                l.split('\t')
                    .next()
                    .is_some_and(|n| n == name || n == format!("{name}:{}", package_arch(i)))
            })
            .collect();
        if matches.len() != 1 {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "candidate installed package identity missing/ambiguous",
            ));
        }
        let fields: Vec<_> = matches[0].split('\t').collect();
        if fields.len() < 3
            || (if i.method == InstallMethod::LinuxRpm {
                !rpm_versions_match(fields[1], version)
            } else {
                fields[1] != version
            })
            || fields[2] != package_arch(i)
            || (i.method == InstallMethod::LinuxDeb && fields.get(3) != Some(&"installed"))
        {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "installed package version/architecture/configuration differs from signed plan",
            ));
        }
    }
    Ok(())
}
fn verify_package_identity(
    i: &InstallationV1,
    asset: &ArtifactV1,
    path: &std::path::Path,
) -> Result<()> {
    let file = path
        .to_str()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "package path encoding"))?;
    let (program, args) = if i.method == InstallMethod::LinuxDeb {
        (
            "/usr/bin/dpkg-deb",
            strings(&[
                "--show",
                "--showformat=${Package}\t${Version}\t${Architecture}\n",
                file,
            ]),
        )
    } else {
        (
            "/usr/bin/rpm",
            strings(&[
                "-qp",
                "--qf",
                "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}\t%{ARCH}\n",
                file,
            ]),
        )
    };
    let (code, text) = supervisor::output(program, &args, Duration::from_secs(10))?;
    let fields: Vec<_> = text.trim_end().split('\t').collect();
    if code != 0
        || fields.len() != 3
        || fields[0] != asset.package_name
        || fields[2] != package_arch(i)
        || (if i.method == InstallMethod::LinuxRpm {
            !rpm_versions_match(fields[1], &asset.package_version)
        } else {
            fields[1] != asset.package_version
        })
    {
        return Err(Error::new(
            ErrorCode::UntrustedUpdate,
            "package metadata differs from signed package name/version/architecture",
        ));
    }
    Ok(())
}
/// apt's simulation may not add/remove ANY package beyond the selected signed plurx cohort.
pub fn validate_apt_simulation(text: &str, expected: &[(String, String)]) -> Result<()> {
    let names: BTreeMap<_, _> = expected
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines() {
        if line.starts_with("Remv ")
            || line.contains("held broken packages")
            || line.contains("kept back")
        {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "package solver removes packages or conflicts with held policy",
            ));
        }
        if let Some(rest) = line.strip_prefix("Inst ") {
            let mut words = rest.split_whitespace();
            let name = words.next().unwrap_or("").split(':').next().unwrap_or("");
            let wanted = names.get(name).ok_or_else(|| {
                Error::new(
                    ErrorCode::PolicyConflict,
                    "distro prerequisites are missing; install them before stopping plurx",
                )
            })?;
            let (_, version) = rest.split_once('(').ok_or_else(|| {
                Error::new(ErrorCode::PolicyConflict, "unrecognized apt solver output")
            })?;
            if version.split_whitespace().next() != Some(*wanted) {
                return Err(Error::new(
                    ErrorCode::PolicyConflict,
                    "apt candidate differs from exact signed package version",
                ));
            }
            seen.insert(name);
        }
        if let Some(rest) = line.strip_prefix("Conf ") {
            if !names.contains_key(rest.split_whitespace().next().unwrap_or("")) {
                return Err(Error::new(
                    ErrorCode::PolicyConflict,
                    "solver configures unselected packages",
                ));
            }
        }
    }
    if seen.len() != expected.len() {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "solver does not upgrade the complete selected cohort",
        ));
    }
    Ok(())
}
pub fn preflight(
    i: &InstallationV1,
    candidate: &VerifiedManifest,
    store: &TransactionStore,
    j: &JournalV1,
) -> Result<PackagePlan> {
    crate::package_locks::check(i)?;
    let plan = plan(i, candidate, &j.transaction_id)?;
    let dir = store.dir(&j.transaction_id)?;
    for (a, path) in update::select_cohort(candidate, i)?
        .into_iter()
        .zip(&plan.paths)
    {
        update::verify_staged(a, &dir)?;
        verify_package_identity(i, a, path)?;
    }
    if i.method == InstallMethod::LinuxDeb {
        let (audit_code, audit) = supervisor::output(
            "/usr/bin/dpkg",
            &strings(&["--audit"]),
            Duration::from_secs(10),
        )?;
        let (holds_code, holds) = supervisor::output(
            "/usr/bin/apt-mark",
            &strings(&["showhold"]),
            Duration::from_secs(5),
        )?;
        if audit_code != 0
            || !audit.trim().is_empty()
            || holds_code != 0
            || holds.lines().any(|l| {
                plan.packages
                    .iter()
                    .any(|(n, _)| n == l || l.starts_with(&format!("{n}:")))
            })
        {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "incomplete dpkg configuration or selected held package; repair before shutdown",
            ));
        }
        let mut args = plan.args.clone();
        args.insert(0, "--simulate".into());
        let (code, text) = supervisor::output(plan.program, &args, Duration::from_secs(30))?;
        if code != 0 {
            return Err(Error::new(ErrorCode::PolicyConflict, "offline apt simulation failed; prerequisites/locks/pins must be resolved before shutdown"));
        }
        validate_apt_simulation(&text, &plan.packages)?;
    } else {
        // rpm test verifies the exact local cohort against the installed dependency database, without resolving/fetching new dependencies.
        let mut args = strings(&["--test", "-U"]);
        args.extend(plan.paths.iter().map(|p| p.to_string_lossy().into_owned()));
        let (code, _) = supervisor::output("/usr/bin/rpm", &args, Duration::from_secs(30))?;
        if code != 0 {
            return Err(Error::new(ErrorCode::PolicyConflict, "RPM offline prerequisite test failed; install prerequisites/resolve locks before shutdown"));
        }
        let (code, locks) = supervisor::output(
            "/usr/bin/dnf",
            &strings(&["--cacheonly", "versionlock", "list"]),
            Duration::from_secs(10),
        )?;
        // Absent versionlock plugin cannot be treated as permission to override pins.
        if code != 0
            || plan
                .packages
                .iter()
                .any(|(name, _)| locks.lines().any(|line| line.contains(&format!("{name}-"))))
        {
            return Err(Error::new(ErrorCode::PolicyConflict, "cannot establish dnf versionlock policy; use documented manual package maintenance"));
        }
    }
    let _ = inventory(i, &plan.packages)?;
    Ok(plan)
}
#[allow(
    clippy::disallowed_methods,
    reason = "Independent root package worker must not load daemon storage or kill a configuring package manager"
)]
pub fn install(plan: &PackagePlan) -> Result<i32> {
    // No app timeout/kill: if apt/dnf has begun configuring, its authoritative status must settle.
    let status = Command::new(plan.program)
        .args(&plan.args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()?;
    Ok(status.code().unwrap_or(-1))
}
#[cfg(target_os = "linux")]
fn descendant_of(pid: u32, generation: &str) -> Result<bool> {
    if crate::process::inspect(pid)?.generation != generation {
        return Ok(false);
    }
    let mut current = std::process::id();
    for _ in 0..64 {
        if current == pid {
            return Ok(true);
        }
        if current <= 1 {
            return Ok(false);
        }
        let stat = std::fs::read_to_string(format!("/proc/{current}/stat"))?;
        let (_, fields) = stat
            .rsplit_once(')')
            .ok_or_else(|| Error::new(ErrorCode::IdentityMismatch, "ancestor stat malformed"))?;
        current = fields
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::new(ErrorCode::IdentityMismatch, "ancestor PID unavailable"))?;
    }
    Ok(false)
}
/// Hooks do NOT take the app lock. Root journal + live process ancestry is authority, never an environment variable.
pub fn hook_check(i: &InstallationV1, package: &str, version: &str, phase: &str) -> Result<bool> {
    #[cfg(not(target_os = "linux"))]
    let _ = version;
    if crate::installation::native_uid() != 0
        || !matches!(package, "plurxd" | "plurx-desktop")
        || !matches!(
            phase,
            "preinstall" | "postinstall" | "preremove" | "postremove"
        )
    {
        return Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "invalid package hook caller/request",
        ));
    }
    let root = i.state_dir()?;
    let transactions = root.child("transactions", false)?;
    let Some(j) = crate::transaction::read_active(&root, &transactions)? else {
        return Ok(false);
    };
    if j.terminal() {
        return Ok(false);
    }
    if j.phase != Phase::Installing || j.installation_id != i.installation_id {
        return Err(Error::new(
            ErrorCode::Busy,
            "outside package transaction conflicts with active controller operation",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let pid = j.worker_pid.ok_or_else(|| {
            Error::new(
                ErrorCode::AuthorizationDenied,
                "missing root worker lineage",
            )
        })?;
        let generation = j.worker_generation.as_deref().ok_or_else(|| {
            Error::new(ErrorCode::AuthorizationDenied, "missing worker generation")
        })?;
        if !descendant_of(pid, generation)? || crate::process::inspect(pid)?.uid != 0 {
            return Err(Error::new(
                ErrorCode::AuthorizationDenied,
                "package hook is not a descendant of exact active root worker",
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    return Err(Error::new(
        ErrorCode::Unsupported,
        "package hooks are Linux-only",
    ));
    #[cfg(target_os = "linux")]
    {
        let dir = transactions.child(&j.transaction_id, false)?;
        let ledger: update::AcceptedReleasesV1 = root.json("accepted-releases.json")?;
        let verified = update::verify_manifest(
            &dir.read(update::MANIFEST_NAME, update::MAX_MANIFEST as u64)?,
            &dir.read(update::SIGNATURE_NAME, 64)?,
            &update::production_keys()?,
            i,
            &ledger,
            crate::unix_time()?,
        )?;
        let selected = update::select_cohort(&verified, i)?;
        if !selected.iter().any(|a| {
            a.package_name == package && (version.is_empty() || a.package_version == version)
        }) {
            return Err(Error::new(
                ErrorCode::AuthorizationDenied,
                "package hook is outside exact signed transaction plan",
            ));
        }
        dir.write_json(&format!("hook-{package}-{phase}.json"), &serde_json::json!({"schema_version":1,"package":package,"version":version,"phase":phase,"transaction_id":j.transaction_id}))?;
        Ok(true)
    }
}
