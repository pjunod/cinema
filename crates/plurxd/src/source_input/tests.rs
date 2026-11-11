use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct UnlimitedBudget(AtomicU64);
impl SourceInputByteBudget for UnlimitedBudget {
    fn allowance(
        &self,
        max: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>
    {
        self.0.fetch_add(max as u64, Ordering::SeqCst);
        Box::pin(async move { Ok(max) })
    }
}
pub(super) struct ReadGate {
    reached: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}
impl ReadGate {
    pub(super) fn park(&self) -> std::io::Result<()> {
        if let Some(reached) = self.reached.lock().expect("read gate").take() {
            let _ = reached.send(());
        }
        let released = self.released.lock().expect("read gate");
        let (released, _) = self
            .changed
            .wait_timeout_while(released, std::time::Duration::from_secs(10), |released| {
                !*released
            })
            .expect("read gate wait");
        if !*released {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "test source read gate",
            ));
        }
        Ok(())
    }
    fn release(&self) {
        *self.released.lock().expect("read gate") = true;
        self.changed.notify_all();
    }
}
struct ReleaseOnDrop(Arc<ReadGate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

pub(super) async fn input(
    bytes: &[u8],
    window: usize,
    read_gate: Option<Arc<ReadGate>>,
) -> (SourceInput, std::fs::File, Arc<UnlimitedBudget>) {
    use std::io::Write;
    let mut file = tempfile::tempfile().expect("source file");
    file.write_all(bytes).expect("source bytes");
    file.sync_all().expect("source sync");
    let source =
        crate::fragment_index_cluster::SourceFence::clone_current_held(&file).expect("held source");
    let allocator = Arc::new(UnlimitedBudget(AtomicU64::new(0)));
    let state = Arc::new(InputState {
        source: std::sync::Mutex::new(Some(source)),
        custody: std::sync::Mutex::new(None),
        observer: None,
        purpose_budget: None,
        background_budget: Some(allocator.clone()),
        pause: plurx_core::process::bounded::source_pause_controller(),
        budget: tokio::sync::Mutex::new(ByteBudget::default()),
        close: tokio_util::sync::CancellationToken::new(),
        read_slot: Arc::new(tokio::sync::Semaphore::new(1)),
        len: bytes.len() as u64,
        mime: "application/octet-stream",
        window_bytes: std::sync::atomic::AtomicUsize::new(window),
        read_ceiling: AtomicU64::new(u64::MAX),
        seek_bootstrap: std::sync::OnceLock::new(),
        source_bytes: AtomicU64::new(0),
        cache_hits: AtomicU64::new(0),
        cached_bytes: std::sync::atomic::AtomicUsize::new(0),
        read_gate,
    });
    (
        SourceInput::launch(state).await.expect("input bridge"),
        file,
        allocator,
    )
}

struct SwitchingPurposeBudget {
    required: std::sync::atomic::AtomicBool,
    claimed: AtomicU64,
}
impl SourceInputByteBudget for SwitchingPurposeBudget {
    fn allowance(
        &self,
        max: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>
    {
        let count = if self.required.load(Ordering::SeqCst) {
            max
        } else {
            max.min(SOURCE_WINDOW_CONTROL)
        };
        self.claimed.fetch_add(count as u64, Ordering::SeqCst);
        Box::pin(async move { Ok(count) })
    }
    fn revalidate_read(
        &self,
        size: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<usize>> + Send + '_>>
    {
        let count = if self.required.load(Ordering::SeqCst) {
            size
        } else {
            size.min(SOURCE_WINDOW_CONTROL)
        };
        Box::pin(async move { Ok(count) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn companion_read_window_tracks_selected_purpose_at_physical_read() {
    use std::io::Write;
    let bytes = vec![7; 8 * 1024 * 1024];
    let mut file = tempfile::tempfile().expect("source file");
    file.write_all(&bytes).expect("source bytes");
    let source =
        crate::fragment_index_cluster::SourceFence::clone_current_held(&file).expect("held source");
    let budget = Arc::new(SwitchingPurposeBudget {
        required: std::sync::atomic::AtomicBool::new(false),
        claimed: AtomicU64::new(0),
    });
    let (reached, parked) = tokio::sync::oneshot::channel();
    let gate = Arc::new(ReadGate {
        reached: std::sync::Mutex::new(Some(reached)),
        released: std::sync::Mutex::new(false),
        changed: std::sync::Condvar::new(),
    });
    let state = Arc::new(InputState {
        source: std::sync::Mutex::new(Some(source)),
        custody: std::sync::Mutex::new(None),
        observer: None,
        purpose_budget: Some(budget.clone()),
        background_budget: None,
        pause: None,
        budget: tokio::sync::Mutex::new(ByteBudget::default()),
        close: tokio_util::sync::CancellationToken::new(),
        read_slot: Arc::new(tokio::sync::Semaphore::new(1)),
        len: bytes.len() as u64,
        mime: "application/octet-stream",
        window_bytes: std::sync::atomic::AtomicUsize::new(SOURCE_WINDOW_MAX),
        read_ceiling: AtomicU64::new(u64::MAX),
        seek_bootstrap: std::sync::OnceLock::new(),
        source_bytes: AtomicU64::new(0),
        cache_hits: AtomicU64::new(0),
        cached_bytes: std::sync::atomic::AtomicUsize::new(0),
        read_gate: Some(Arc::clone(&gate)),
    });
    let first_state = Arc::clone(&state);
    let first = tokio::spawn(async move { read_source(&first_state, 0, 8 * 1024 * 1024).await });
    parked.await.expect("worker parked before physical read");
    budget.required.store(true, Ordering::SeqCst);
    gate.release();
    first.await.expect("read task").expect("optional read");
    assert_eq!(
        state.source_bytes.load(Ordering::SeqCst),
        SOURCE_WINDOW_CONTROL as u64
    );
    let selected = read_source(&state, 4 * 1024 * 1024, 4 * 1024 * 1024)
        .await
        .expect("selected read");
    assert_eq!(selected.len(), SOURCE_HTTP_CHUNK);
    assert_eq!(
        state.source_bytes.load(Ordering::SeqCst),
        (SOURCE_WINDOW_CONTROL + SOURCE_WINDOW_MAX) as u64
    );
    assert_eq!(
        budget.claimed.load(Ordering::SeqCst),
        (SOURCE_WINDOW_CONTROL + SOURCE_WINDOW_MAX) as u64
    );
}

#[test]
fn rewrites_only_proven_original_inputs_and_preserves_prepared_fd4() {
    let args = [
        "-i",
        "/media/movie.mkv",
        "-i",
        "/dev/fd/3",
        "-i",
        "/dev/fd/4",
        "-i",
        "/cache/prepared.mks",
        "-i",
        "/dev/fd/5",
        "/cache/output.mp4",
    ]
    .map(str::to_owned);
    let output = source_args(
        &args,
        std::path::Path::new("/media/movie.mkv"),
        &[3],
        Some("http://127.0.0.1:9/capability"),
    );
    assert_eq!(output[1], "http://127.0.0.1:9/capability");
    assert_eq!(output[3], output[1]);
    assert_eq!(output[5], "/dev/fd/4");
    assert_eq!(output[7], "/cache/prepared.mks");
    assert_eq!(output[9], "/dev/fd/5");
    assert_eq!(output[10], "/cache/output.mp4");
    let audio = source_args(
        &args,
        std::path::Path::new("/media/movie.mkv"),
        &[3, 4],
        Some("http://127.0.0.1:9/capability"),
    );
    assert_eq!(audio[5], audio[1]);
}

#[tokio::test]
async fn bounded_sequential_window_charges_once_and_seek_replaces_it() {
    let bytes = (0..8 * 1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let (input, _file, allocator) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    let first = read_source(&input.state, 0, bytes.len() as u64)
        .await
        .expect("first read");
    assert_eq!(first, &bytes[..SOURCE_HTTP_CHUNK]);
    assert_eq!(input.observation().source_bytes, SOURCE_WINDOW_MAX as u64);
    let second = read_source(
        &input.state,
        SOURCE_HTTP_CHUNK as u64,
        (bytes.len() - SOURCE_HTTP_CHUNK) as u64,
    )
    .await
    .expect("cached read");
    assert_eq!(second, &bytes[SOURCE_HTTP_CHUNK..2 * SOURCE_HTTP_CHUNK]);
    assert_eq!(input.observation().source_bytes, SOURCE_WINDOW_MAX as u64);
    assert_eq!(input.observation().cache_hits, 1);
    let offset = 6 * 1024 * 1024;
    let seek = read_source(&input.state, offset as u64, (bytes.len() - offset) as u64)
        .await
        .expect("seek read");
    assert_eq!(seek, &bytes[offset..offset + SOURCE_HTTP_CHUNK]);
    assert_eq!(input.observation().cached_bytes, 2 * 1024 * 1024);
    assert_eq!(allocator.0.load(Ordering::SeqCst), 6 * 1024 * 1024);
    input.close_and_wait().await.expect("close source");
}

#[tokio::test]
async fn qualification_control_caps_window_and_cache_refuses_source_change() {
    let bytes = vec![7; 2 * 1024 * 1024];
    let (input, file, _) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    let input = input
        .with_read_window(SOURCE_WINDOW_CONTROL)
        .expect("control window");
    let _ = read_source(&input.state, 0, bytes.len() as u64)
        .await
        .expect("first read");
    assert_eq!(
        input.observation().source_bytes,
        SOURCE_WINDOW_CONTROL as u64
    );
    assert_eq!(input.observation().window_bytes, SOURCE_WINDOW_CONTROL);
    file.set_len((bytes.len() + 1) as u64)
        .expect("source replacement mutation");
    assert!(read_source(
        &input.state,
        SOURCE_HTTP_CHUNK as u64,
        (bytes.len() - SOURCE_HTTP_CHUNK) as u64
    )
    .await
    .is_err());
    input.close_and_wait().await.expect("close source");
}

#[tokio::test]
async fn loopback_capability_preserves_ranges_head_and_refuses_other_path() {
    let bytes = b"0123456789abcdef";
    let (input, _file, _) = input(bytes, SOURCE_WINDOW_CONTROL, None).await;
    let client = reqwest::Client::new();
    let response = client
        .get(input.url())
        .header("range", "bytes=-4")
        .send()
        .await
        .expect("suffix range");
    assert_eq!(response.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 12-15/16");
    assert_eq!(
        response.bytes().await.expect("range body").as_ref(),
        b"cdef"
    );
    let head = client.head(input.url()).send().await.expect("HEAD");
    assert_eq!(head.status(), reqwest::StatusCode::OK);
    assert_eq!(head.headers()["content-length"], "16");
    assert!(head.bytes().await.expect("HEAD body").is_empty());
    let invalid = client
        .get(input.url())
        .header("range", "bytes=99-")
        .send()
        .await
        .expect("invalid range");
    assert_eq!(invalid.status(), reqwest::StatusCode::RANGE_NOT_SATISFIABLE);
    let wrong = format!("{}/wrong", input.url());
    assert_eq!(
        client
            .get(wrong)
            .send()
            .await
            .expect("wrong capability")
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    input.close_and_wait().await.expect("close source");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_waiter_cannot_release_read_slot_or_close_source_before_worker_settles() {
    let (reached, observed) = tokio::sync::oneshot::channel();
    let gate = Arc::new(ReadGate {
        reached: std::sync::Mutex::new(Some(reached)),
        released: std::sync::Mutex::new(false),
        changed: std::sync::Condvar::new(),
    });
    let _release = ReleaseOnDrop(Arc::clone(&gate));
    let (input, _file, _) = input(
        &vec![9; 1024 * 1024],
        SOURCE_WINDOW_MAX,
        Some(Arc::clone(&gate)),
    )
    .await;
    let state = Arc::clone(&input.state);
    let read = tokio::spawn(async move { read_source(&state, 0, 1024 * 1024).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), observed)
        .await
        .expect("read reached")
        .expect("read observer");
    read.abort();
    let _ = read.await;
    let state = Arc::clone(&input.state);
    let close = tokio::spawn(input.close_and_wait());
    tokio::task::yield_now().await;
    assert!(!close.is_finished());
    assert!(state.source.lock().expect("source state").is_some());
    assert_eq!(state.read_slot.available_permits(), 0);
    gate.release();
    tokio::time::timeout(std::time::Duration::from_secs(5), close)
        .await
        .expect("close joined")
        .expect("close owner")
        .expect("physical close");
    assert!(state.source.lock().expect("source state").is_none());
}

#[tokio::test]
async fn renderer_exact_range_does_not_read_ahead_or_share_the_audio_cap() {
    let bytes = vec![3; 2 * 1024 * 1024];
    let (input, _file, _) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    let input = input.without_prefetch();
    let client = reqwest::Client::new();
    for offset in [0, 1024 * 1024] {
        let response = client
            .get(input.url())
            .header("range", format!("bytes={offset}-{}", offset + 32767))
            .send()
            .await
            .expect("finite renderer range");
        assert_eq!(
            response.bytes().await.expect("finite range bytes").len(),
            32768
        );
    }
    assert_eq!(input.observation().source_bytes, 65536);
    input.close_and_wait().await.expect("finite source close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_host_range_reader_is_positioned_bounded_and_revoked_on_close() {
    let bytes = (0..1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let (input, _file, budget) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    let input = input.without_prefetch();
    let reader = input.range_reader();
    let worker = reader.clone();
    let actual = tokio::task::spawn_blocking(move || {
        let mut bytes = vec![0; 512 * 1024];
        let count = worker
            .read_at(
                &mut bytes,
                12345,
                &tokio_util::sync::CancellationToken::new(),
            )
            .expect("bounded range");
        bytes.truncate(count);
        bytes
    })
    .await
    .expect("range worker");
    assert_eq!(actual, &bytes[12345..12345 + 256 * 1024]);
    assert_eq!(input.observation().source_bytes, 256 * 1024);
    assert_eq!(budget.0.load(Ordering::SeqCst), 256 * 1024);
    input.close_and_wait().await.expect("source close");
    let error = tokio::task::spawn_blocking(move || {
        reader.read_at(&mut [0; 16], 0, &tokio_util::sync::CancellationToken::new())
    })
    .await
    .expect("revoked reader")
    .expect_err("closed capability");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn processed_input_close_waits_for_actual_registered_child_reap() {
    let (input, _file, _budget) = input(b"owned original bytes", SOURCE_WINDOW_MAX, None).await;
    let state = Arc::clone(&input.state);
    let url = input.url().to_owned();
    let lease = crate::dv_segment::SourceInputLease::new(input.without_prefetch());
    let physical_owner = lease.physical_owner();
    let mut command = tokio::process::Command::new("/bin/sleep");
    command
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let (child, job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::background(
            "processed original input ownership regression",
        ),
    )
    .expect("real owned child");
    let slot = Arc::new(crate::prodrun::ProducerSlot::new());
    let (registration, writers) = slot
        .attach_registered_job_owned(child, job, 0, Some(Box::new(physical_owner)))
        .await;
    writers.settled();
    let closing_lease = Arc::clone(&lease);
    let closing = tokio::spawn(async move { closing_lease.close_and_wait().await });
    tokio::task::yield_now().await;
    assert!(
        !closing.is_finished(),
        "returning an error cannot close a still-owned original input"
    );
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .expect("capability remains live until reap");
    assert_eq!(
        response.bytes().await.expect("owned source body").as_ref(),
        b"owned original bytes"
    );
    assert!(state.source.lock().expect("source state").is_some());
    assert!(slot
        .request_registered_retirement(&registration)
        .await
        .expect("exact child retirement"));
    registration.wait_confirmed_reap().await;
    tokio::time::timeout(std::time::Duration::from_secs(2), closing)
        .await
        .expect("physical input join")
        .expect("close owner")
        .expect("joined input close");
    assert!(state.source.lock().expect("source state").is_none());
}

#[tokio::test]
async fn census_input_ceiling_is_immutable_and_cannot_borrow_a_later_inputs_budget() {
    let bytes = vec![7u8; 1024 * 1024];
    let (census, _file, _budget) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    census
        .seal_read_ceiling(SOURCE_HTTP_CHUNK as u64)
        .expect("seal operation");
    assert!(
        census.seal_read_ceiling(u64::MAX).is_err(),
        "no reset/remove API"
    );
    let first = read_source(&census.state, 0, bytes.len() as u64)
        .await
        .expect("bounded source read");
    assert_eq!(first.len(), SOURCE_HTTP_CHUNK);
    assert_eq!(census.observation().source_bytes, SOURCE_HTTP_CHUNK as u64);
    assert!(
        read_source(&census.state, SOURCE_HTTP_CHUNK as u64, bytes.len() as u64)
            .await
            .is_err()
    );
    let (primary, _primary_file, _primary_budget) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    assert_ne!(primary.url(), census.url());
    assert_eq!(
        read_source(&primary.state, 0, bytes.len() as u64)
            .await
            .expect("independent operation")
            .len(),
        SOURCE_HTTP_CHUNK
    );
    assert!(
        read_source(&census.state, 0, bytes.len() as u64)
            .await
            .is_err(),
        "late census URL never sees primary allowance"
    );
    census.close_and_wait().await.expect("join census input");
    primary.close_and_wait().await.expect("join primary input");
}

#[tokio::test]
async fn verified_seek_bootstrap_unlocks_only_its_fresh_primary_input() {
    let bytes = vec![9u8; 1024 * 1024];
    let (primary, _file, _budget) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    let token = primary
        .seal_seek_bootstrap_limit(SOURCE_HTTP_CHUNK as u64)
        .expect("source input fixture");
    assert!(!token.is_verified());
    assert!(primary.seal_seek_bootstrap_limit(u64::MAX).is_err());
    assert_eq!(
        read_source(&primary.state, 0, bytes.len() as u64)
            .await
            .expect("source input fixture")
            .len(),
        SOURCE_HTTP_CHUNK
    );
    assert!(
        read_source(&primary.state, SOURCE_HTTP_CHUNK as u64, bytes.len() as u64)
            .await
            .is_err()
    );
    let (other, _other_file, _other_budget) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    let other_token = other
        .seal_seek_bootstrap_limit(SOURCE_HTTP_CHUNK as u64)
        .expect("source input fixture");
    token.verified_landing();
    assert!(token.is_verified());
    assert!(!other_token.is_verified());
    assert_eq!(
        read_source(&primary.state, SOURCE_HTTP_CHUNK as u64, bytes.len() as u64)
            .await
            .expect("source input fixture")
            .len(),
        SOURCE_HTTP_CHUNK
    );
    assert_eq!(
        read_source(&other.state, 0, bytes.len() as u64)
            .await
            .expect("source input fixture")
            .len(),
        SOURCE_HTTP_CHUNK
    );
    assert!(
        read_source(&other.state, SOURCE_HTTP_CHUNK as u64, bytes.len() as u64)
            .await
            .is_err()
    );
    let (census, _census_file, _census_budget) = input(&bytes, SOURCE_WINDOW_MAX, None).await;
    census
        .seal_read_ceiling(SOURCE_HTTP_CHUNK as u64)
        .expect("source input fixture");
    assert!(census
        .seal_seek_bootstrap_limit(SOURCE_HTTP_CHUNK as u64)
        .is_err());
    primary.close_and_wait().await.expect("reader closes");
    other.close_and_wait().await.expect("reader closes");
    census.close_and_wait().await.expect("reader closes");
}
