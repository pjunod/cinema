//! Daemon source coordination for the shared strict decode probe.
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) use plurx_core::process::fixed_probe::*;

#[cfg(target_os = "linux")]
mod source_reads;
#[cfg(target_os = "linux")]
mod source_session;

struct DaemonSourceService {
    foreground: Option<crate::source_custody::SourceRequest>,
    background: Option<Arc<crate::background_jobs::SourceReadBudget>>,
}

pub(crate) fn new_source(
    handle: Arc<std::fs::File>,
    offset_gate: Arc<tokio::sync::Semaphore>,
    work: crate::process_control::ChildWork,
) -> DecodeFactSource {
    DecodeFactSource::new(handle, offset_gate, work).with_source_scope(
        crate::source_custody::current_request(),
        crate::background_jobs::source_budget(),
    )
}

pub(crate) trait WithSourceScope {
    fn with_source_scope(
        self,
        foreground: Option<crate::source_custody::SourceRequest>,
        background: Option<Arc<crate::background_jobs::SourceReadBudget>>,
    ) -> Self;
}

impl WithSourceScope for DecodeFactSource {
    fn with_source_scope(
        self,
        foreground: Option<crate::source_custody::SourceRequest>,
        background: Option<Arc<crate::background_jobs::SourceReadBudget>>,
    ) -> Self {
        let service = (foreground.is_some() || background.is_some()).then(|| {
            Arc::new(DaemonSourceService {
                foreground,
                background,
            }) as Arc<_>
        });
        self.with_source_service(service)
    }
}

#[async_trait::async_trait]
impl DecodeFactSourceService for DaemonSourceService {
    fn register_source(&self, source: Arc<std::fs::File>) -> Result<(), DecodeFactError> {
        self.background.as_ref().map_or(Ok(()), |owner| {
            owner
                .register_source_file(source)
                .map_err(DecodeFactError::SourceService)
        })
    }

    fn must_join(&self) -> bool {
        self.background.is_some()
    }

    async fn collect(
        &self,
        source: Arc<std::fs::File>,
        #[cfg(target_os = "linux")] pause: Option<(
            plurx_core::process::bounded::SourcePauseController,
            bool,
        )>,
        cancelled: CancellationToken,
        work: ProbeCollection<'_>,
    ) -> Result<(), DecodeFactError> {
        #[cfg(not(target_os = "linux"))]
        let _ = cancelled;
        let collecting = crate::source_custody::with_request(self.foreground.clone(), async {
            #[cfg(target_os = "linux")]
            {
                let collecting = async {
                    let permission = source_reads::ReadPermission::for_source(&source)
                        .await?
                        .map(|permit| permit as Arc<dyn ProbeReadPermission>);
                    with_probe_read_permission(permission, work).await
                };
                match pause {
                    Some((control, inherited)) => {
                        source_session::run(
                            control,
                            inherited,
                            self.background.clone(),
                            cancelled,
                            collecting,
                        )
                        .await
                    }
                    None => collecting.await,
                }
            }
            #[cfg(windows)]
            {
                let input = crate::source_input::SourceInput::open_current_held(
                    &source,
                    "strict-windows-source-probe",
                )
                .await
                .map_err(DecodeFactError::SourceService)?;
                let result =
                    with_probe_input_url(input.as_ref().map(|input| input.url().to_owned()), work)
                        .await;
                if let Some(input) = input {
                    input
                        .close_and_wait()
                        .await
                        .map_err(|error| DecodeFactError::Read(error.to_string()))?;
                }
                result
            }
            #[cfg(target_os = "macos")]
            {
                let input = crate::source_input::SourceInput::open_current_held(
                    &source,
                    "strict-wasi-source-probe",
                )
                .await
                .map_err(DecodeFactError::SourceService)?
                .map(crate::source_input::SourceInput::without_prefetch);
                let reader = input.as_ref().map(|input| {
                    let reader = input.range_reader();
                    let read: Arc<ProbeRangeReader> = Arc::new(
                        move |buffer: &mut [u8], offset: u64, cancel: &CancellationToken| {
                            reader.read_at(buffer, offset, cancel)
                        },
                    );
                    read
                });
                let result = with_probe_range_reader(reader, work).await;
                if let Some(input) = input {
                    input
                        .close_and_wait()
                        .await
                        .map_err(|error| DecodeFactError::Read(error.to_string()))?;
                }
                result
            }
        });
        match &self.background {
            Some(owner) => crate::background_jobs::budgeted(Arc::clone(owner), collecting).await,
            None => collecting.await,
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
