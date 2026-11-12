//! Daemon-private, descriptor-bound AVIO input for producer byte service.
//! A URL names one random loopback capability, never an arbitrary path. All
//! ranges and demuxer inputs share one physical read budget and held object.
use crate::source_custody::{SourceCustody, SourceObserver, SourceRequest};
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use plurx_core::store::background_jobs_resources::SourceServiceGrant;
use plurx_core::store::background_jobs_resources::{BackgroundSourceOwner, ForegroundSourcePolicy};
use plurx_core::store::Store;
use std::{net::SocketAddr, sync::Arc};
const SOURCE_WINDOW_MAX: usize = 4 * 1024 * 1024;
const SOURCE_WINDOW_CONTROL: usize = 256 * 1024;
const SOURCE_HTTP_CHUNK: usize = 256 * 1024;

pub(crate) trait SourceInputByteBudget: Send + Sync {
    fn allowance(
        &self,
        max: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>;
    fn revalidate_read(
        &self,
        size: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>
    {
        Box::pin(async move { Ok(size) })
    }
}

/// Reads the current accepted rendition purpose. A new parent can attach to
/// the same immutable rendition while one producer generation is alive.
pub(crate) type SourceRequired = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync,
>;

struct CompanionReadBudget {
    budget: Arc<crate::background_jobs::SourceReadBudget>,
    observer: SourceObserver,
    required: SourceRequired,
    required_published: std::sync::atomic::AtomicBool,
    last_allowance_required: std::sync::atomic::AtomicBool,
}
impl SourceInputByteBudget for CompanionReadBudget {
    fn allowance(
        &self,
        max: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>
    {
        Box::pin(async move {
            loop {
                if (self.required)().await {
                    if !self
                        .required_published
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        self.observer
                            .declare_rolling_policy()
                            .await
                            .map_err(std::io::Error::other)?;
                        self.observer
                            .refill()
                            .await
                            .map_err(std::io::Error::other)?;
                        self.required_published
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                    self.last_allowance_required
                        .store(true, std::sync::atomic::Ordering::Release);
                    return Ok(max);
                }
                self.required_published
                    .store(false, std::sync::atomic::Ordering::Release);
                // This existing finite foreground class publishes the same
                // domain grant used by background source-service allocation.
                // The optional reader cannot consume its foreground half.
                self.observer
                    .companion_service()
                    .await
                    .map_err(std::io::Error::other)?;
                tokio::select! {
                    result = self.budget.allowance(max.min(SOURCE_WINDOW_CONTROL)) => {
                        let allowance = result.map_err(std::io::Error::other)?;
                        self.last_allowance_required.store(false, std::sync::atomic::Ordering::Release);
                        return Ok(allowance);
                    },
                    () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
                }
            }
        })
    }
    fn revalidate_read(
        &self,
        size: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>
    {
        Box::pin(async move {
            if (self.required)().await {
                if !self
                    .required_published
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    self.observer
                        .declare_rolling_policy()
                        .await
                        .map_err(std::io::Error::other)?;
                    self.observer
                        .refill()
                        .await
                        .map_err(std::io::Error::other)?;
                    self.required_published
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                return Ok(size);
            }
            self.required_published
                .store(false, std::sync::atomic::Ordering::Release);
            self.observer
                .companion_service()
                .await
                .map_err(std::io::Error::other)?;
            if self
                .last_allowance_required
                .load(std::sync::atomic::Ordering::Acquire)
            {
                self.budget
                    .allowance(size.min(SOURCE_WINDOW_CONTROL))
                    .await
                    .map_err(std::io::Error::other)
            } else {
                Ok(size.min(SOURCE_WINDOW_CONTROL))
            }
        })
    }
}

impl SourceInputByteBudget for crate::background_jobs::SourceReadBudget {
    fn allowance(
        &self,
        max: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>
    {
        Box::pin(async move {
            crate::background_jobs::SourceReadBudget::allowance(self, max)
                .await
                .map_err(std::io::Error::other)
        })
    }
}

pub(crate) struct SourceInput {
    url: String,
    state: Arc<InputState>,
    close: tokio_util::sync::CancellationToken,
    server: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
/// Blocking host adapters retain only this revocable transport capability.
/// The existing operation owns SourceInput and explicitly joins it after all
/// readers; this value neither owns custody nor starts a cleanup task.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone)]
pub(crate) struct SourceInputRangeReader {
    url: String,
    len: u64,
    runtime: tokio::runtime::Handle,
    client: reqwest::Client,
    close: tokio_util::sync::CancellationToken,
}
#[cfg(any(target_os = "macos", test))]
impl SourceInputRangeReader {
    pub(crate) fn read_at(
        &self,
        buffer: &mut [u8],
        offset: u64,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> std::io::Result<usize> {
        use std::io::{Error, ErrorKind};
        if buffer.is_empty() || offset >= self.len {
            return Ok(0);
        }
        let wanted = buffer
            .len()
            .min(256 * 1024)
            .min(usize::try_from(self.len - offset).unwrap_or(usize::MAX));
        let end = offset + wanted as u64 - 1;
        let expected = format!("bytes {offset}-{end}/{}", self.len);
        let bytes=self.runtime.block_on(async {
            let read=async {
                let mut response=self.client.get(&self.url)
                    .header(reqwest::header::RANGE,format!("bytes={offset}-{end}"))
                    .send().await.map_err(|error|Error::other(crate::scratch_put::redact(&error.to_string())))?;
                if response.status()!=reqwest::StatusCode::PARTIAL_CONTENT
                    || response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|value|value.to_str().ok())!=Some(expected.as_str())
                    || response.content_length()!=Some(wanted as u64) {
                    return Err(Error::new(ErrorKind::InvalidData,"source capability returned an invalid bounded range"));
                }
                let mut bytes=Vec::with_capacity(wanted);
                while let Some(chunk)=response.chunk().await.map_err(|error|Error::other(crate::scratch_put::redact(&error.to_string())))? {
                    if chunk.len()>wanted.saturating_sub(bytes.len()) {return Err(Error::new(ErrorKind::InvalidData,"source capability exceeded range bound"));}
                    bytes.extend_from_slice(&chunk);
                }
                if bytes.len()!=wanted {return Err(Error::new(ErrorKind::UnexpectedEof,"source capability ended before range settled"));}
                Ok(bytes)
            };
            tokio::select! {biased;
                ()=cancelled.cancelled()=>Err(Error::new(ErrorKind::Interrupted,"source range reader cancelled")),
                ()=self.close.cancelled()=>Err(Error::new(ErrorKind::Interrupted,"source input closed")),
                result=read=>result,
            }
        })?;
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}
struct InputState {
    // Drop the original capability before its exact ledger guard.
    source: std::sync::Mutex<Option<crate::fragment_index_cluster::SourceFence>>,
    custody: std::sync::Mutex<Option<SourceCustody>>,
    _background_holder: Option<crate::background_jobs::SourceHolder>,
    observer: Option<SourceObserver>,
    purpose_budget: Option<Arc<dyn SourceInputByteBudget>>,
    background_budget: Option<Arc<dyn SourceInputByteBudget>>,
    pause: Option<plurx_core::process::bounded::SourcePauseController>,
    budget: tokio::sync::Mutex<ByteBudget>,
    close: tokio_util::sync::CancellationToken,
    read_slot: Arc<tokio::sync::Semaphore>,
    len: u64,
    mime: &'static str,
    window_bytes: std::sync::atomic::AtomicUsize,
    source_bytes: std::sync::atomic::AtomicU64,
    cache_hits: std::sync::atomic::AtomicU64,
    cached_bytes: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    read_gate: Option<Arc<tests::ReadGate>>,
}
#[derive(Default)]
struct ByteBudget {
    grant: Option<SourceServiceGrant>,
    used: u64,
    window: ReadWindow,
}
#[derive(Default)]
struct ReadWindow {
    next: u64,
    bytes: Vec<u8>,
    cursor: usize,
}
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct SourceInputObservation {
    pub(crate) window_bytes: usize,
    pub(crate) source_bytes: u64,
    pub(crate) cache_hits: u64,
    pub(crate) cached_bytes: usize,
}
impl SourceInput {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn range_reader(&self) -> SourceInputRangeReader {
        SourceInputRangeReader {
            url: self.url.clone(),
            len: self.state.len,
            runtime: tokio::runtime::Handle::current(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("private source HTTP client"),
            close: self.close.clone(),
        }
    }

    pub(crate) async fn open(
        request: SourceRequest,
        attempt: impl ToString,
    ) -> Result<Self, String> {
        Self::open_purposed(request, attempt, None).await
    }
    pub(crate) async fn open_companion(
        request: SourceRequest,
        attempt: impl ToString,
        store: Arc<dyn Store>,
        required: SourceRequired,
    ) -> Result<Self, String> {
        Self::open_purposed(request, attempt, Some((store, required))).await
    }
    async fn open_purposed(
        request: SourceRequest,
        attempt: impl ToString,
        companion: Option<(Arc<dyn Store>, SourceRequired)>,
    ) -> Result<Self, String> {
        let source =
            crate::fragment_index_cluster::open_source_playback_fence(request.file(), None).await?;
        let len = source
            .handle
            .metadata()
            .map_err(|error| error.to_string())?
            .len();
        let mime = crate::http::stream::file_content_type(&request.file().path);
        let custody = request
            .clone()
            .object_version(source.object_version())
            .register(attempt)
            .await?;
        let observer = custody.observer();
        observer.mediate_reads();
        let close = tokio_util::sync::CancellationToken::new();
        let mut initial_required = false;
        let purpose_budget: Option<Arc<dyn SourceInputByteBudget>> =
            if let Some((store, required)) = companion {
                let token = custody.token();
                if token.owner.policy != ForegroundSourcePolicy::Companion {
                    return Err("companion source owner has the wrong purpose".into());
                }
                initial_required = (required)().await;
                let budget = crate::background_jobs::SourceReadBudget::new(
                    store,
                    BackgroundSourceOwner::Companion {
                        token: Box::new(token),
                    },
                    close.clone(),
                );
                Some(Arc::new(CompanionReadBudget {
                    budget,
                    observer: observer.clone(),
                    required,
                    required_published: std::sync::atomic::AtomicBool::new(initial_required),
                    last_allowance_required: std::sync::atomic::AtomicBool::new(initial_required),
                }))
            } else {
                None
            };
        if purpose_budget.is_some() {
            if initial_required {
                observer.declare_rolling_policy().await?;
                observer.refill().await?;
            } else {
                // Publish the finite optional service class before the bridge
                // is visible. Its own startup must not block a peer.
                observer.companion_service().await?;
            }
        }
        let state = Arc::new(InputState {
            source: std::sync::Mutex::new(Some(source)),
            _background_holder: None,
            custody: std::sync::Mutex::new(Some(custody)),
            observer: Some(observer),
            purpose_budget,
            background_budget: None,
            pause: None,
            budget: tokio::sync::Mutex::new(ByteBudget::default()),
            read_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            close: close.clone(),
            len,
            mime,
            window_bytes: std::sync::atomic::AtomicUsize::new(SOURCE_WINDOW_MAX),
            source_bytes: std::sync::atomic::AtomicU64::new(0),
            cache_hits: std::sync::atomic::AtomicU64::new(0),
            cached_bytes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            read_gate: None,
        });
        Self::launch(state).await
    }
    pub(crate) async fn open_current_held(
        handle: &std::fs::File,
        attempt: &str,
    ) -> Result<Option<Self>, String> {
        let foreground = crate::source_custody::current_request();
        let background = crate::background_jobs::source_budget();
        if foreground.is_none() && background.is_none() {
            return Ok(None);
        }
        let source = crate::fragment_index_cluster::SourceFence::clone_current_held(handle)?;
        if let Some(budget) = background {
            return Self::open_background_held(source, "application/octet-stream", budget)
                .await
                .map(Some);
        }
        let request = foreground.expect("source request observed above");
        Self::open_held_with_request(source, "application/octet-stream", request, attempt)
            .await
            .map(Some)
    }
    pub(crate) async fn open_foreground_held(
        request: SourceRequest,
        source: crate::fragment_index_cluster::SourceFence,
        attempt: impl ToString,
    ) -> Result<Self, String> {
        let mime = crate::http::stream::file_content_type(&request.file().path);
        Self::open_held_with_request(source, mime, request, attempt).await
    }
    async fn open_held_with_request(
        source: crate::fragment_index_cluster::SourceFence,
        mime: &'static str,
        request: SourceRequest,
        attempt: impl ToString,
    ) -> Result<Self, String> {
        let len = source
            .handle
            .metadata()
            .map_err(|error| error.to_string())?
            .len();
        let custody = request
            .object_version(source.object_version())
            .register(attempt)
            .await?;
        let observer = custody.observer();
        observer.mediate_reads();
        let state = Arc::new(InputState {
            source: std::sync::Mutex::new(Some(source)),
            _background_holder: None,
            custody: std::sync::Mutex::new(Some(custody)),
            observer: Some(observer),
            purpose_budget: None,
            background_budget: None,
            pause: None,
            budget: tokio::sync::Mutex::new(ByteBudget::default()),
            close: tokio_util::sync::CancellationToken::new(),
            read_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            len,
            mime,
            window_bytes: std::sync::atomic::AtomicUsize::new(SOURCE_WINDOW_MAX),
            source_bytes: std::sync::atomic::AtomicU64::new(0),
            cache_hits: std::sync::atomic::AtomicU64::new(0),
            cached_bytes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            read_gate: None,
        });
        Self::launch(state).await
    }
    pub(crate) async fn open_background(
        file: &plurx_core::domain::MediaFile,
        budget: Arc<crate::background_jobs::SourceReadBudget>,
    ) -> Result<Self, String> {
        let source = crate::fragment_index_cluster::open_source_fence(file, None).await?;
        Self::open_background_held(
            source,
            crate::http::stream::file_content_type(&file.path),
            budget,
        )
        .await
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) async fn qualification_fixture(bytes: &[u8]) -> (Self, std::fs::File) {
        let (input, file, _) = tests::input(bytes, SOURCE_HTTP_CHUNK, None).await;
        (input, file)
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) async fn completion_read_fixture(
        file: &Arc<std::fs::File>,
        owner: &Arc<crate::background_jobs::SourceReadBudget>,
    ) -> (
        Self,
        tokio::sync::oneshot::Receiver<()>,
        Arc<tests::ReadGate>,
    ) {
        let source = crate::fragment_index_cluster::SourceFence::clone_current_held(file)
            .expect("fixture source");
        let holder = owner.source_holder().expect("registered physical source");
        let (reached, parked) = tokio::sync::oneshot::channel();
        let gate = Arc::new(tests::ReadGate::new(reached));
        let len = file.metadata().expect("source metadata").len();
        let state = Arc::new(InputState {
            source: std::sync::Mutex::new(Some(source)),
            custody: std::sync::Mutex::new(None),
            _background_holder: holder,
            observer: None,
            purpose_budget: None,
            background_budget: Some(Arc::new(tests::UnlimitedBudget::default())),
            pause: None,
            budget: tokio::sync::Mutex::new(ByteBudget::default()),
            close: tokio_util::sync::CancellationToken::new(),
            read_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            len,
            mime: "application/octet-stream",
            window_bytes: std::sync::atomic::AtomicUsize::new(SOURCE_WINDOW_MAX),
            source_bytes: std::sync::atomic::AtomicU64::new(0),
            cache_hits: std::sync::atomic::AtomicU64::new(0),
            cached_bytes: std::sync::atomic::AtomicUsize::new(0),
            read_gate: Some(Arc::clone(&gate)),
        });
        (
            Self::launch(state).await.expect("real input bridge"),
            parked,
            gate,
        )
    }
    pub(crate) async fn open_background_held(
        source: crate::fragment_index_cluster::SourceFence,
        mime: &'static str,
        budget: Arc<crate::background_jobs::SourceReadBudget>,
    ) -> Result<Self, String> {
        let holder = budget.source_holder()?;
        if !source.unchanged() {
            return Err("held source changed before input bridge".into());
        }
        let len = source
            .handle
            .metadata()
            .map_err(|error| error.to_string())?
            .len();
        let state = Arc::new(InputState {
            source: std::sync::Mutex::new(Some(source)),
            _background_holder: holder,
            custody: std::sync::Mutex::new(None),
            observer: None,
            purpose_budget: None,
            background_budget: Some(budget),
            pause: plurx_core::process::bounded::source_pause_controller(),
            budget: tokio::sync::Mutex::new(ByteBudget::default()),
            close: tokio_util::sync::CancellationToken::new(),
            read_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            len,
            mime,
            window_bytes: std::sync::atomic::AtomicUsize::new(SOURCE_WINDOW_MAX),
            source_bytes: std::sync::atomic::AtomicU64::new(0),
            cache_hits: std::sync::atomic::AtomicU64::new(0),
            cached_bytes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            read_gate: None,
        });
        Self::launch(state).await
    }
    async fn launch(state: Arc<InputState>) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let route = format!("/{}", uuid::Uuid::new_v4().simple());
        let router = Router::new()
            .route(&route, any(serve_input))
            .with_state(Arc::clone(&state));
        let close = state.close.clone();
        if let Some(observer) = state.observer.clone() {
            let source_close = close.clone();
            tokio::spawn(async move {
                tokio::select! {
                    ()=source_close.cancelled()=>{},
                    ()=observer.authority_lost()=>source_close.cancel(),
                }
            });
        }
        let shutdown = close.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await;
        });
        Ok(Self {
            url: format!("http://{address}{route}"),
            state,
            close,
            server: std::sync::Mutex::new(Some(server)),
        })
    }
    /// Finite renderer ranges must not acquire a sequential window beyond
    /// one transport chunk; their exact HTTP Range length remains the bound.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn without_prefetch(self) -> Self {
        self.state
            .window_bytes
            .store(SOURCE_HTTP_CHUNK, std::sync::atomic::Ordering::SeqCst);
        self
    }
    /// Internal qualification control; no host tuning or Developer switch.
    /// Set before passing the URL to a child. Both candidates remain bounded.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_read_window(self, bytes: usize) -> Result<Self, String> {
        if !(SOURCE_WINDOW_CONTROL..=SOURCE_WINDOW_MAX).contains(&bytes) {
            return Err("source read window outside qualification bounds".into());
        }
        self.state
            .window_bytes
            .store(bytes, std::sync::atomic::Ordering::SeqCst);
        Ok(self)
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn observation(&self) -> SourceInputObservation {
        SourceInputObservation {
            window_bytes: self
                .state
                .window_bytes
                .load(std::sync::atomic::Ordering::SeqCst),
            source_bytes: self
                .state
                .source_bytes
                .load(std::sync::atomic::Ordering::SeqCst),
            cache_hits: self
                .state
                .cache_hits
                .load(std::sync::atomic::Ordering::SeqCst),
            cached_bytes: self
                .state
                .cached_bytes
                .load(std::sync::atomic::Ordering::SeqCst),
        }
    }
    pub(crate) fn url(&self) -> &str {
        &self.url
    }
    pub(crate) fn foreground_observer(&self) -> Option<SourceObserver> {
        self.state.observer.clone()
    }
    pub(crate) fn observer(&self) -> SourceObserver {
        self.state
            .observer
            .as_ref()
            .expect("foreground source observer")
            .clone()
    }
    /// Join actual source reads, then close the held capability before a
    /// background caller returns its job/resource claim.
    pub(crate) async fn close_and_wait(self) -> std::io::Result<()> {
        self.close.cancel();
        if let Some(observer) = &self.state.observer {
            observer.mark_cleanup_pending();
        }
        let server = { self.server.lock().expect("source input server").take() };
        if let Some(server) = server {
            if let Err(error) = server.await {
                tracing::warn!(%error,"source input listener owner ended; joining descriptor reads");
            }
        }
        let permit = Arc::clone(&self.state.read_slot)
            .acquire_owned()
            .await
            .map_err(std::io::Error::other)?;
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            drop(state.source.lock().expect("source input").take());
            drop(state.custody.lock().expect("source custody").take());
        })
        .await
        .map_err(std::io::Error::other)?;
        Ok(())
    }
    pub(crate) fn cleanup_pending(self) {
        self.close.cancel();
        if let Some(custody) = self.state.custody.lock().expect("source custody").take() {
            custody.cleanup_pending();
        }
    }
}
impl Drop for SourceInput {
    fn drop(&mut self) {
        self.close.cancel();
    }
}

/// Runtime argv rewriting only. The immutable recipe retains its original
/// path and identity. Only original media inputs are replaced; subtitle and
/// output capabilities remain separate descriptors.
pub(crate) fn source_args(
    args: &[String],
    source: &std::path::Path,
    original_descriptors: &[i32],
    url: Option<&str>,
) -> Vec<String> {
    let Some(url) = url else { return args.to_vec() };
    let original = source.to_string_lossy();
    let mut result = args.to_vec();
    for index in 1..result.len() {
        if result[index - 1] == "-i"
            && (result[index] == original
                || original_descriptors.iter().any(|descriptor| {
                    result[index] == format!("/proc/self/fd/{descriptor}")
                        || result[index] == format!("/dev/fd/{descriptor}")
                }))
        {
            result[index] = url.to_owned();
        }
    }
    result
}
async fn serve_input(
    State(state): State<Arc<InputState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if !peer.ip().is_loopback() || state.close.is_cancelled() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    use crate::http::stream::FileRangePlan;
    let plan = crate::http::stream::plan_file_range(&headers, &method, state.len);
    let (status, fields) = crate::http::stream::file_range_head(plan, state.len, state.mime);
    let body = match plan {
        FileRangePlan::Unsatisfiable => Body::empty(),
        _ if method == Method::HEAD => Body::empty(),
        FileRangePlan::Partial { start, end } => {
            input_body(Arc::clone(&state), start, end - start + 1)
        }
        FileRangePlan::Full => input_body(Arc::clone(&state), 0, state.len),
    };
    let mut response = (status, body).into_response();
    for (name, value) in fields {
        if let Ok(value) = axum::http::HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}
fn input_body(state: Arc<InputState>, offset: u64, remaining: u64) -> Body {
    let stream = futures_util::stream::try_unfold(
        (state, offset, remaining),
        |(state, offset, remaining)| async move {
            if remaining == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            let bytes = read_source(&state, offset, remaining).await?;
            let count = bytes.len() as u64;
            Ok(Some((
                bytes::Bytes::from(bytes),
                (state, offset + count, remaining - count),
            )))
        },
    );
    Body::from_stream(stream)
}
async fn read_source(
    state: &Arc<InputState>,
    offset: u64,
    remaining: u64,
) -> std::io::Result<Vec<u8>> {
    let mut budget = tokio::select! {biased;
        ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
        budget=state.budget.lock()=>budget,
    };
    if state
        .observer
        .as_ref()
        .is_some_and(|observer| !observer.has_authority())
    {
        return Err(std::io::Error::other("source authority lost"));
    }
    if budget.window.next == offset && budget.window.cursor < budget.window.bytes.len() {
        let source = Arc::clone(state);
        let permit = Arc::clone(&state.read_slot)
            .acquire_owned()
            .await
            .map_err(std::io::Error::other)?;
        let valid = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            source
                .source
                .lock()
                .expect("source input")
                .as_ref()
                .is_some_and(|source| source.unchanged())
        })
        .await
        .map_err(std::io::Error::other)?;
        if !valid {
            return Err(std::io::Error::other("source input changed"));
        }
        let count = (remaining.min(SOURCE_HTTP_CHUNK as u64) as usize)
            .min(budget.window.bytes.len() - budget.window.cursor);
        let bytes =
            budget.window.bytes[budget.window.cursor..budget.window.cursor + count].to_vec();
        budget.window.cursor += count;
        budget.window.next += count as u64;
        state
            .cache_hits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return Ok(bytes);
    }
    // A seek invalidates the old window. Reuse its allocation for the next
    // actual read, so two four-MiB source buffers never coexist.
    let mut window = std::mem::take(&mut budget.window.bytes);
    window.clear();
    state
        .cached_bytes
        .store(0, std::sync::atomic::Ordering::SeqCst);
    let wanted = remaining
        .min(state.len.saturating_sub(offset))
        .min(state.window_bytes.load(std::sync::atomic::Ordering::SeqCst) as u64);
    let observer = state.observer.as_ref();
    if observer.is_some_and(|observer| !observer.has_authority()) {
        return Err(std::io::Error::other("source authority lost"));
    }
    let allowance = if let Some(allocator) = state.purpose_budget.as_ref() {
        tokio::select! {biased;
            ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
            allowance=allocator.allowance(wanted as usize)=>allowance? as u64,
        }
    } else if let Some(observer) = observer {
        if !observer.rolling_policy() && budget.grant.is_none() && observer.has_completed_read() {
            budget.grant = Some(
                observer
                    .transfer_epoch()
                    .await
                    .map_err(std::io::Error::other)?,
            );
        }
        if !observer.rolling_policy() {
            if let Some(grant) = &budget.grant {
                let now = crate::fragment_index_cluster::unix_ms();
                if budget.used >= grant.foreground_bytes && now < grant.expires_at_ms {
                    observer.idle().await.map_err(std::io::Error::other)?;
                    tokio::select! {biased;
                        ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
                        ()=observer.policy_changed()=>{},
                        ()=tokio::time::sleep(std::time::Duration::from_millis((grant.expires_at_ms-now) as u64))=>{},
                    }
                }
                if !observer.rolling_policy()
                    && (budget.used >= grant.foreground_bytes
                        || crate::fragment_index_cluster::unix_ms() >= grant.expires_at_ms)
                {
                    observer.refill().await.map_err(std::io::Error::other)?;
                    budget.grant = Some(
                        observer
                            .transfer_epoch()
                            .await
                            .map_err(std::io::Error::other)?,
                    );
                    budget.used = 0;
                }
            }
        }
        if observer.rolling_policy() {
            wanted
        } else {
            budget
                .grant
                .as_ref()
                .map_or(SOURCE_WINDOW_CONTROL as u64, |grant| {
                    grant.foreground_bytes.saturating_sub(budget.used)
                })
        }
    } else {
        let allocator = state
            .background_budget
            .as_ref()
            .ok_or_else(|| std::io::Error::other("source budget missing"))?;
        tokio::select! {biased;
            ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
            allowance=allocator.allowance(wanted as usize)=>allowance? as u64,
        }
    };
    let size = wanted.min(allowance) as usize;
    if size == 0 {
        return Err(std::io::Error::other(
            "source service returned an empty allowance",
        ));
    }
    // Reserve the entire issued window before handing it to a blocking
    // worker. Cancelled/short reads receive no refund and cannot reset an
    // epoch by abandoning their HTTP waiter.
    budget.used = budget.used.saturating_add(size as u64);
    if let Some(observer) = observer {
        tokio::select! {biased;
            ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
            allowed=observer.reads_allowed()=>allowed.map_err(std::io::Error::other)?,
        }
    }
    let read_permit = tokio::select! {biased;
        ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
        permit=Arc::clone(&state.read_slot).acquire_owned()=>permit.map_err(std::io::Error::other)?,
    };
    // Recheck closure after acquiring the actual read slot. No new work may
    // start after close_and_wait has joined and closed the held descriptor.
    if state.close.is_cancelled() {
        return Err(std::io::Error::other("source input closed"));
    }
    if observer.is_some_and(|observer| !observer.has_authority()) {
        return Err(std::io::Error::other("source authority lost"));
    }
    if let Some(observer) = observer {
        tokio::select! {biased;
            ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
            allowed=observer.reads_allowed()=>allowed.map_err(std::io::Error::other)?,
        }
    }
    let pause_guard = match &state.pause {
        Some(pause) => Some(pause.enlist(&state.close).await?),
        None => None,
    };
    let read_state = Arc::clone(state);
    let activity = if let Some(observer) = observer {
        loop {
            if let Some(activity) = observer.try_active_read() {
                break Some(activity);
            }
            tokio::select! {biased;
                ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
                allowed=observer.reads_allowed()=>allowed.map_err(std::io::Error::other)?,
            }
        }
    } else {
        None
    };
    let job = tokio::task::spawn_blocking(move || {
        let activity = activity;
        let _pause_guard = pause_guard;
        #[cfg(test)]
        if let Some(gate) = &read_state.read_gate {
            gate.park()?;
        }
        // The accepted quality can change while this read waits for its
        // source slot or worker. Revalidate immediately before read_at.
        let size = if let Some(allocator) = read_state.purpose_budget.as_ref() {
            tokio::runtime::Handle::current().block_on(allocator.revalidate_read(size))?
        } else {
            size
        };
        if size == 0 || read_state.close.is_cancelled() {
            return Err(std::io::Error::other("source purpose or input closed"));
        }
        let mut source = read_state.source.lock().expect("source input");
        let source = source
            .as_mut()
            .ok_or_else(|| std::io::Error::other("source input closed"))?;
        if !source.unchanged() {
            return Err(std::io::Error::other("source input changed"));
        }
        window.resize(size, 0);
        let mut count = 0;
        while count < size {
            #[cfg(unix)]
            let read = {
                use std::os::unix::fs::FileExt;
                source
                    .handle
                    .read_at(&mut window[count..], offset + count as u64)?
            };
            #[cfg(windows)]
            let read = {
                use std::os::windows::fs::FileExt;
                source
                    .handle
                    .seek_read(&mut window[count..], offset + count as u64)?
            };
            if read == 0 {
                break;
            }
            count += read;
        }
        read_state
            .source_bytes
            .fetch_add(count as u64, std::sync::atomic::Ordering::SeqCst);
        if !source.unchanged() {
            return Err(std::io::Error::other("source input changed"));
        }
        if count > 0 {
            if let Some(activity) = &activity {
                activity.completed();
            }
        }
        window.truncate(count);
        Ok::<_, std::io::Error>((window, read_permit))
    });
    let (bytes, _read_permit) = tokio::select! {biased;
        ()=state.close.cancelled()=>return Err(std::io::Error::other("source input closed")),
        result=job=>result.map_err(std::io::Error::other)??,
    };
    if bytes.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "source input truncated",
        ));
    }
    if let Some(observer) = observer {
        if !observer.rolling_policy() && budget.grant.is_none() {
            budget.grant = Some(
                observer
                    .transfer_epoch()
                    .await
                    .map_err(std::io::Error::other)?,
            );
        }
    }
    state
        .cached_bytes
        .store(bytes.len(), std::sync::atomic::Ordering::SeqCst);
    let count = (remaining.min(SOURCE_HTTP_CHUNK as u64) as usize).min(bytes.len());
    let output = bytes[..count].to_vec();
    budget.window = ReadWindow {
        next: offset + count as u64,
        bytes,
        cursor: count,
    };
    Ok(output)
}

#[cfg(test)]
#[path = "source_input/tests.rs"]
mod tests;
