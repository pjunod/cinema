//! Windows Service Control Manager integration for the native server.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use plurx_core::config::Config;
use tokio_util::sync::CancellationToken;
use windows_service::define_windows_service;
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{
    self, ServiceControlHandlerResult, ServiceStatusHandle,
};
use windows_service::service_dispatcher;
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const SERVICE_NAME: &str = "plurxd";
const DISPLAY_NAME: &str = "plurx media server";
const DESCRIPTION: &str = "Native plurx media server and discovery service";

static STOP: OnceLock<CancellationToken> = OnceLock::new();
static CONFIG: OnceLock<Config> = OnceLock::new();
static STATUS: OnceLock<ServiceStatusHandle> = OnceLock::new();
static READY_REPORTED: AtomicBool = AtomicBool::new(false);

define_windows_service!(service_main_ffi, service_main);

pub(crate) fn stop_token() -> Option<CancellationToken> {
    STOP.get().cloned()
}

pub(crate) fn install(config_path: Option<&std::path::Path>) -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    let executable_path = std::env::current_exe().map_err(windows_service::Error::Winapi)?;
    let mut launch_arguments = Vec::new();
    if let Some(path) = config_path {
        let absolute = std::path::absolute(path).map_err(windows_service::Error::Winapi)?;
        launch_arguments.push(OsString::from("--config"));
        launch_arguments.push(absolute.into_os_string());
    }
    launch_arguments.extend([OsString::from("service"), OsString::from("run")]);
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path,
        launch_arguments,
        dependencies: Vec::new(),
        account_name: None,
        account_password: None,
    };
    let service = manager.create_service(
        &info,
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS,
    )?;
    service.set_description(DESCRIPTION)?;
    service.start::<&str>(&[])?;
    let started = std::time::Instant::now();
    loop {
        let status = service.query_status()?;
        match status.current_state {
            ServiceState::Running => break,
            ServiceState::Stopped => {
                return Err(windows_service::Error::Winapi(std::io::Error::other(
                    "plurx service stopped before its listener became ready",
                )))
            }
            _ if started.elapsed() < Duration::from_secs(120) => {
                std::thread::sleep(Duration::from_millis(250));
            }
            _ => {
                return Err(windows_service::Error::Winapi(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "plurx service did not report ready within 120 seconds",
                )))
            }
        }
    }
    println!("installed and started {DISPLAY_NAME}");
    Ok(())
}

pub(crate) fn uninstall() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    )?;
    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop()?;
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(30)
            && service.query_status()?.current_state != ServiceState::Stopped
        {
            std::thread::sleep(Duration::from_millis(250));
        }
        if service.query_status()?.current_state != ServiceState::Stopped {
            return Err(windows_service::Error::Winapi(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "plurx service did not stop within 30 seconds; registration was not removed",
            )));
        }
    }
    service.delete()?;
    println!("stopped and removed {DISPLAY_NAME}");
    Ok(())
}

pub(crate) fn dispatch_service(mut config: Config) -> windows_service::Result<()> {
    apply_service_data_default(&mut config).map_err(windows_service::Error::Winapi)?;
    CONFIG.set(config).map_err(|_| {
        windows_service::Error::Winapi(std::io::Error::other("service config already initialized"))
    })?;
    service_dispatcher::start(SERVICE_NAME, service_main_ffi)
}

fn apply_service_data_default(config: &mut Config) -> std::io::Result<()> {
    if config.storage.data_dir == PathBuf::from("./data") {
        let program_data = std::env::var_os("ProgramData").ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "ProgramData is unavailable; set storage.data_dir explicitly",
            )
        })?;
        config.storage.data_dir = PathBuf::from(program_data).join("plurx").join("data");
    }
    Ok(())
}

fn service_status(
    status: &ServiceStatusHandle,
    state: ServiceState,
    accepted: ServiceControlAccept,
    exit_code: u32,
    checkpoint: u32,
    wait_hint: Duration,
) -> windows_service::Result<()> {
    status.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accepted,
        exit_code: ServiceExitCode::Win32(exit_code),
        checkpoint,
        wait_hint,
        process_id: None,
    })
}

pub(crate) fn report_listener_ready() -> anyhow::Result<()> {
    let Some(status) = STATUS.get() else {
        return Ok(());
    };
    service_status(
        status,
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
        0,
        Duration::default(),
    )
    .map_err(|error| anyhow::anyhow!("reporting Windows service readiness: {error}"))?;
    READY_REPORTED.store(true, Ordering::Release);
    Ok(())
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        eprintln!("plurx service failed: {error}");
    }
}

fn run_service() -> windows_service::Result<()> {
    let stop = CancellationToken::new();
    STOP.set(stop.clone()).map_err(|_| {
        windows_service::Error::Winapi(std::io::Error::other(
            "service stop token already initialized",
        ))
    })?;
    let handler = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Some(status) = STATUS.get() {
                let _ = service_status(
                    status,
                    ServiceState::StopPending,
                    ServiceControlAccept::empty(),
                    0,
                    1,
                    Duration::from_secs(30),
                );
            }
            stop.cancel();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    STATUS.set(handler).map_err(|_| {
        windows_service::Error::Winapi(std::io::Error::other("service status already initialized"))
    })?;
    service_status(
        &handler,
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        0,
        1,
        Duration::from_secs(120),
    )?;
    std::thread::spawn(|| {
        let mut checkpoint = 2_u32;
        while !READY_REPORTED.load(Ordering::Acquire)
            && STOP.get().is_some_and(|stop| !stop.is_cancelled())
        {
            std::thread::sleep(Duration::from_secs(2));
            if READY_REPORTED.load(Ordering::Acquire)
                || STOP.get().is_some_and(CancellationToken::is_cancelled)
            {
                break;
            }
            if let Some(status) = STATUS.get() {
                let _ = service_status(
                    status,
                    ServiceState::StartPending,
                    ServiceControlAccept::empty(),
                    0,
                    checkpoint,
                    Duration::from_secs(120),
                );
            }
            checkpoint = checkpoint.saturating_add(1);
        }
    });

    let mut config = CONFIG.get().cloned().ok_or_else(|| {
        windows_service::Error::Winapi(std::io::Error::other("service config unavailable"))
    })?;
    let result = (|| -> anyhow::Result<()> {
        super::canonicalize_storage_roots(&mut config)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(super::run(config))
    })();

    if let Some(stop) = STOP.get() {
        stop.cancel();
    }
    let exit_code = u32::from(result.is_err());
    service_status(
        &handler,
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit_code,
        0,
        Duration::default(),
    )?;
    result.map_err(|error| windows_service::Error::Winapi(std::io::Error::other(error.to_string())))
}
