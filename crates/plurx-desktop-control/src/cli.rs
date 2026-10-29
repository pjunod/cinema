//! Finite JSON CLI; all native invocations use argument arrays.
use crate::{
    controller,
    installation::{self, InstallMethod, InstallationV1},
    native, package,
    protocol::*,
    supervisor, transaction, Error, Result,
};
use clap::{Parser, Subcommand, ValueEnum};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};
#[derive(Debug, Parser)]
#[command(name = "plurx-control", version)]
pub struct Cli {
    #[arg(long)]
    pub installation: Option<PathBuf>,
    #[command(subcommand)]
    pub command: ControlCommand,
}
#[derive(Debug, Parser)]
#[command(name = "plurx-install-helper", version)]
struct HelperCli {
    #[command(subcommand)]
    command: ControlCommand,
}
#[derive(Debug, Subcommand)]
pub enum ControlCommand {
    /// Inspect compiled-in public update roots without loading an installation or making network requests.
    TrustInfo,
    Status,
    Start,
    Stop {
        #[arg(long)]
        confirm_interruption: bool,
    },
    Restart {
        #[arg(long)]
        confirm_interruption: bool,
    },
    Startup {
        #[command(subcommand)]
        command: StartupCommand,
    },
    Updates {
        #[command(subcommand)]
        command: UpdateCommand,
    },
    Logs {
        #[arg(long, default_value_t=100, value_parser=clap::value_parser!(u16).range(1..=1000))]
        lines: u16,
    },
    ValidateInstallation,
    /// Explicit root-authorized local source packages, independently of signed releases.
    SourceInstall {
        #[arg(long)]
        candidate: PathBuf,
    },
    Bootstrap {
        #[command(subcommand)]
        command: BootstrapCommand,
    },
    #[command(hide = true)]
    PackageScript {
        #[arg(value_enum)]
        phase: HookPhase,
        #[arg(value_enum)]
        component: PackageComponent,
        #[arg(last = true)]
        original_args: Vec<String>,
    },
    #[command(hide = true)]
    HookCheck {
        #[arg(long, value_enum)]
        package: PackageArg,
        #[arg(long)]
        version: String,
        #[arg(long, value_enum)]
        phase: HookPhase,
    },
}
#[derive(Debug, Subcommand)]
pub enum StartupCommand {
    Get,
    Set {
        #[arg(value_enum)]
        target: StartupTarget,
        #[arg(value_enum)]
        value: OnOff,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StartupTarget {
    Server,
    Tray,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}
#[derive(Debug, Subcommand)]
pub enum UpdateCommand {
    Check,
    Prepare {
        #[arg(long)]
        version: String,
    },
    Apply {
        #[arg(long)]
        transaction: String,
        #[arg(long)]
        confirm_interruption: bool,
    },
    Status {
        #[arg(long)]
        transaction: Option<String>,
    },
    Cancel {
        #[arg(long)]
        transaction: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum BootstrapCommand {
    /// Install an explicitly assembled native source runtime, never a release archive.
    MacosSource {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        no_start: bool,
        #[arg(long)]
        preflight: bool,
    },
    Macos {
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        no_start: bool,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PackageComponent {
    Server,
    Desktop,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PackageArg {
    Plurxd,
    PlurxDesktop,
}
impl PackageArg {
    fn name(self) -> &'static str {
        match self {
            Self::Plurxd => "plurxd",
            Self::PlurxDesktop => "plurx-desktop",
        }
    }
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HookPhase {
    Preinstall,
    Postinstall,
    Preremove,
    Postremove,
}
impl HookPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Preinstall => "preinstall",
            Self::Postinstall => "postinstall",
            Self::Preremove => "preremove",
            Self::Postremove => "postremove",
        }
    }
}
pub fn control_entry() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            if e.use_stderr() {
                return emit(Err(Error::new(ErrorCode::InvalidRequest, e.to_string())));
            }
            let _ = e.print();
            return 0;
        }
    };
    if matches!(cli.command, ControlCommand::TrustInfo) {
        return match crate::update::parse_trust_roots(
            crate::update::PRODUCTION_ROOTS_JSON.as_bytes(),
        ) {
            Ok(roots) => match serde_json::to_string(&roots) {
                Ok(json) => {
                    println!("{json}");
                    0
                }
                Err(e) => emit(Err(e.into())),
            },
            Err(e) => emit(Err(e)),
        };
    }
    if let ControlCommand::SourceInstall { candidate } = &cli.command {
        if cli.installation.is_some() {
            return emit(Err(Error::new(
                ErrorCode::InvalidRequest,
                "source installation uses canonical native paths",
            )));
        }
        return emit_with_recovery(crate::source_install::run(candidate));
    }
    if let ControlCommand::Bootstrap { command } = &cli.command {
        let (package, no_start, source, preflight) = match command {
            BootstrapCommand::Macos { package, no_start } => (package, no_start, false, false),
            BootstrapCommand::MacosSource {
                package,
                no_start,
                preflight,
            } => (package, no_start, true, *preflight),
        };
        if cli.installation.is_some() {
            return emit(Err(Error::new(
                ErrorCode::InvalidRequest,
                "bootstrap uses only canonical native installation paths",
            )));
        }
        if preflight {
            return emit_with_recovery(native::preflight_source_package(package).map(|()| {
                ControlResultV1::Update {
                    state: UpdateState::Idle,
                    version: None,
                    transaction_id: None,
                    detail: Some("Read-only native source migration preflight passed".into()),
                }
            }));
        }
        let installed = if source {
            native::bootstrap_source_package(package, *no_start)
        } else {
            native::bootstrap_package(package, *no_start)
        };
        return emit_with_recovery(installed.and_then(|i| controller::status(&i)));
    }
    if matches!(
        cli.command,
        ControlCommand::HookCheck { .. } | ControlCommand::PackageScript { .. }
    ) {
        return emit(Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "package hooks use the installed narrow helper",
        )));
    }
    let path = match cli
        .installation
        .clone()
        .map(Ok)
        .unwrap_or_else(installation::default_record)
    {
        Ok(p) => p,
        Err(e) => return emit(Err(e)),
    };
    let i = match InstallationV1::load(&path) {
        Ok(i) => i,
        Err(e) => return emit(Err(e)),
    };
    if needs_authority(&cli.command) && installation::native_uid() != i.owner_uid {
        if cli.installation.is_some() {
            return emit(Err(Error::new(
                ErrorCode::AuthorizationDenied,
                "custom record overrides cannot be forwarded to privileged helpers",
            )));
        }
        return invoke_helper();
    }
    if cli.installation.is_some() {
        emit(execute(&i, cli.command))
    } else {
        emit_with_recovery(execute(&i, cli.command))
    }
}
pub fn needs_authority(command: &ControlCommand) -> bool {
    !matches!(
        command,
        ControlCommand::Status
            | ControlCommand::Updates {
                command: UpdateCommand::Status { .. }
            }
            | ControlCommand::ValidateInstallation
            | ControlCommand::Startup {
                command: StartupCommand::Get
                    | StartupCommand::Set {
                        target: StartupTarget::Tray,
                        ..
                    }
            }
    )
}
#[allow(
    clippy::disallowed_methods,
    reason = "Polkit frontend is independent of daemon process/activity store"
)]
fn invoke_helper() -> i32 {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // Fixed action/program; original finite CLI args were parsed above. No shell or record override.
    match Command::new("/usr/bin/pkexec")
        .arg(installation::INSTALL_HELPER)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        Ok(output)
            if !output.status.success() && matches!(output.status.code(), Some(126 | 127)) =>
        {
            emit(Err(Error::new(
                ErrorCode::AuthorizationDenied,
                "authorization was cancelled/denied or polkit agent unavailable; no state change",
            )))
        }
        Ok(output) => {
            if output.stdout.len() > 1024 * 1024 {
                return emit(Err(Error::new(
                    ErrorCode::InvalidRequest,
                    "helper response exceeded bound",
                )));
            }
            let response: ControlResponseV1 = match serde_json::from_slice(&output.stdout) {
                Ok(r) => r,
                Err(_) => {
                    return emit(Err(Error::new(
                        ErrorCode::Unavailable,
                        "installed helper returned no valid controller response",
                    )))
                }
            };
            if response.schema_version != 1 {
                return emit(Err(Error::new(
                    ErrorCode::Unsupported,
                    "helper protocol bridge required",
                )));
            }
            let _ = std::io::stdout().write_all(&output.stdout);
            if response.ok && output.status.success() {
                0
            } else {
                1
            }
        }
        Err(_) => emit(Err(Error::new(
            ErrorCode::AuthorizationRequired,
            "install/enable the system polkit authorization agent or use authorized native CLI",
        ))),
    }
}
pub fn helper_entry() -> i32 {
    let cli = match HelperCli::try_parse() {
        Ok(c) => c,
        Err(e) => return emit(Err(Error::new(ErrorCode::InvalidRequest, e.to_string()))),
    };
    if installation::native_uid() != 0 || cfg!(target_os = "macos") {
        return emit(Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "installed package helper requires native Linux root authorization",
        )));
    }
    if matches!(
        cli.command,
        ControlCommand::Bootstrap { .. }
            | ControlCommand::ValidateInstallation
            | ControlCommand::SourceInstall { .. }
    ) {
        return emit(Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "bootstrap/record validation is not a polkit helper operation",
        )));
    }
    if let ControlCommand::PackageScript {
        phase,
        component,
        original_args,
    } = &cli.command
    {
        let name = match component {
            PackageComponent::Server => "plurxd",
            PackageComponent::Desktop => "plurx-desktop",
        };
        return match package::package_script(name, phase.name(), original_args) {
            Ok(true) => 10,
            Ok(false) => 0,
            Err(e) => {
                eprintln!("{}", e.message);
                1
            }
        };
    }
    let i = match installation::default_record().and_then(|p| InstallationV1::load(&p)) {
        Ok(i) => i,
        Err(e) => return emit(Err(e)),
    };
    if let ControlCommand::HookCheck {
        package,
        version,
        phase,
    } = cli.command
    {
        return match package::hook_check(&i, package.name(), &version, phase.name()) {
            Ok(true) => 20,
            Ok(false) => 0,
            Err(e) => {
                eprintln!("{}", e.message);
                1
            }
        };
    }
    if matches!(
        cli.command,
        ControlCommand::Startup {
            command: StartupCommand::Set {
                target: StartupTarget::Tray,
                ..
            }
        }
    ) {
        return emit(Err(Error::new(
            ErrorCode::AuthorizationDenied,
            "root helper cannot change a desktop user's tray preference",
        )));
    }
    emit(execute(&i, cli.command))
}
pub fn execute(i: &InstallationV1, command: ControlCommand) -> Result<ControlResultV1> {
    match command {
        ControlCommand::TrustInfo => Err(Error::new(
            ErrorCode::InvalidRequest,
            "trust-info is a local inspection command",
        )),
        ControlCommand::Status => controller::status(i),
        ControlCommand::Start => controller::lifecycle(i, Operation::Start),
        ControlCommand::Stop {
            confirm_interruption,
        }
        | ControlCommand::Restart {
            confirm_interruption,
        } if !confirm_interruption => Err(Error::new(
            ErrorCode::InvalidRequest,
            "explicit --confirm-interruption required",
        )),
        ControlCommand::Stop { .. } => controller::lifecycle(i, Operation::Stop),
        ControlCommand::Restart { .. } => controller::lifecycle(i, Operation::Restart),
        ControlCommand::Startup {
            command: StartupCommand::Get,
        } => controller::startup(i),
        ControlCommand::Startup {
            command:
                StartupCommand::Set {
                    target: StartupTarget::Server,
                    value,
                },
        } => {
            controller::require_owner(i)?;
            let _store = transaction::TransactionStore::locked(i)?;
            supervisor::set_startup(i, matches!(value, OnOff::On))?;
            controller::startup(i)
        }
        ControlCommand::Startup {
            command:
                StartupCommand::Set {
                    target: StartupTarget::Tray,
                    value,
                },
        } => controller::set_tray(i, matches!(value, OnOff::On)),
        ControlCommand::Updates { command } => match command {
            UpdateCommand::Check => controller::check_updates(i),
            UpdateCommand::Prepare { version } => controller::prepare_update(i, &version),
            UpdateCommand::Apply {
                transaction,
                confirm_interruption,
            } => {
                if !confirm_interruption {
                    return Err(Error::new(
                        ErrorCode::InvalidRequest,
                        "explicit --confirm-interruption required",
                    ));
                }
                controller::apply_update(i, &transaction)
            }
            UpdateCommand::Status { transaction } => {
                controller::update_status(i, transaction.as_deref())
            }
            UpdateCommand::Cancel { transaction } => controller::cancel_update(i, &transaction),
        },
        ControlCommand::Logs { lines } => {
            let (lines, truncated) = supervisor::logs(i, lines as usize)?;
            Ok(ControlResultV1::Logs { lines, truncated })
        }
        ControlCommand::ValidateInstallation => {
            i.validate()?;
            Ok(ControlResultV1::Update {
                state: UpdateState::Idle,
                version: Some(i.current_version.clone()),
                transaction_id: None,
                detail: Some(
                    "Installation routing/schema validated; no startup policy changed".into(),
                ),
            })
        }
        _ => Err(Error::new(
            ErrorCode::InvalidRequest,
            "command is not available through this controller entrypoint",
        )),
    }
}
fn recovery_command(path: &std::path::Path, transaction: &str) -> String {
    format!(
        "'{}' --transaction {}",
        path.display().to_string().replace('\'', "'\\''"),
        transaction
    )
}
fn recovery_hint() -> Option<String> {
    let installation = InstallationV1::load(&installation::default_record().ok()?).ok()?;
    if installation.method != InstallMethod::MacosNative {
        return None;
    }
    let root = installation.state_dir().ok()?;
    let transactions = root.child("transactions", false).ok()?;
    let journal = transaction::read_active(&root, &transactions).ok()??;
    if journal.terminal() || journal.installation_id != installation.installation_id {
        return None;
    }
    Some(recovery_command(
        &installation.paths.state.join("plurx-recovery"),
        &journal.transaction_id,
    ))
}
pub fn emit(result: Result<ControlResultV1>) -> i32 {
    emit_response(result, false)
}
fn emit_with_recovery(result: Result<ControlResultV1>) -> i32 {
    emit_response(result, true)
}
fn emit_response(result: Result<ControlResultV1>, canonical_recovery: bool) -> i32 {
    let ok = result.is_ok();
    let recovery = if !ok && canonical_recovery {
        recovery_hint()
    } else {
        None
    };
    let response = match result {
        Ok(value) => ControlResponseV1::success(value),
        Err(e) => ControlResponseV1::failure(e.wire()),
    };
    match serde_json::to_vec(&response) {
        Ok(mut bytes) if bytes.len() < 1024 * 1024 => {
            bytes.push(b'\n');
            if std::io::stdout().write_all(&bytes).is_err() {
                return 1;
            }
        }
        _ => {
            eprintln!("failed to serialize bounded controller response");
            return 1;
        }
    }
    if let Some(command) = recovery {
        eprintln!("Recovery command:\n{command}");
    }
    if ok {
        0
    } else {
        1
    }
}
#[derive(Parser)]
struct WorkerCli {
    #[arg(long)]
    transaction: String,
}
pub fn worker_entry() -> i32 {
    let args = match WorkerCli::try_parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let result = installation::default_record()
        .and_then(|p| InstallationV1::load(&p))
        .and_then(|i| controller::run_worker(&i, &args.transaction));
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", e.message);
            1
        }
    }
}
#[derive(Parser)]
#[command(group(clap::ArgGroup::new("mode").args(["guard", "transaction"])))]
struct RecoveryCli {
    #[arg(long)]
    guard: bool,
    #[arg(long)]
    transaction: Option<String>,
}
pub fn recovery_entry() -> i32 {
    let args = match RecoveryCli::try_parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let result = installation::default_record()
        .and_then(|p| InstallationV1::load(&p))
        .and_then(|i| {
            if args.guard {
                transaction::guard(&i)
            } else if args.transaction.is_none() {
                controller::boot_reconcile(&i)
            } else {
                controller::recover(&i, args.transaction.as_deref().unwrap_or(""))
            }
        });
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", e.message);
            1
        }
    }
}
#[derive(Parser)]
struct LaunchCli {
    #[arg(long, hide = true)]
    run: bool,
}
pub fn service_launch_entry() -> i32 {
    if let Err(e) = LaunchCli::try_parse() {
        eprintln!("{e}");
        return 1;
    }
    let result = installation::default_record()
        .and_then(|p| InstallationV1::load(&p))
        .and_then(|i| {
            if i.service_uid != installation::native_uid() {
                return Err(Error::new(
                    ErrorCode::AuthorizationDenied,
                    "service launch must run as exact installed service UID, not privileged guard",
                ));
            }
            if i.method == InstallMethod::MacosNative {
                controller::boot_reconcile(&i)?;
            }
            let _launch = crate::launch_lock::acquire(&i, false)?;
            let path = native::daemon_path(&i)?;
            let parent = path
                .parent()
                .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "daemon parent"))?;
            crate::secure_fs::open_dir(parent, i.owner_uid)?.open("plurxd")?;
            use std::os::unix::process::CommandExt;
            let mut command = Command::new(&path);
            command
                .arg("run")
                .env("PLURX_DESKTOP_INSTALLATION_ID", &i.installation_id)
                .env("PLURX_DESKTOP_CONTROL_SOCKET", &i.paths.socket)
                .env(
                    "PLURX_DESKTOP_RECEIPT_DIR",
                    i.paths.state.join("shutdown-receipts"),
                );
            if i.method == InstallMethod::MacosNative {
                for (key, tool) in [("PLURX_FFMPEG", "ffmpeg"), ("PLURX_FFPROBE", "ffprobe")] {
                    if let Some(path) = native::packaged_media_override(
                        &i.paths.runtime,
                        tool,
                        std::env::var_os(key).as_deref(),
                    ) {
                        command.env(key, path);
                    }
                }
            }
            let error = command.exec();
            Err(error.into())
        });
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", e.message);
            1
        }
    }
}

#[cfg(test)]
mod recovery_command_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn recovery_command_is_directly_pasteable_with_spaces_quotes_and_shell_characters() {
        let root =
            std::env::temp_dir().join(format!("plurx-recovery-command-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).expect("private test directory");
        let binary = root.join("recovery 'quoted' $HOME;echo");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").expect("argument recorder");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let transaction = "05734dec-45c5-4659-82cf-1f30f31c782d";
        let (status, output) = supervisor::output(
            "/bin/sh",
            &["-c".into(), recovery_command(&binary, transaction)],
            std::time::Duration::from_secs(5),
        )
        .expect("paste command into shell");
        assert_eq!(status, 0);
        assert_eq!(output, format!("--transaction\n{transaction}\n"));
        std::fs::remove_dir_all(root).expect("remove test directory");
    }
}
