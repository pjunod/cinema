//! Physical original-source custody in the replicated resource ledger.
//!
//! The move-only guard belongs to the actual read job or child reaper. HTTP
//! and session registries may retain an observer, but cannot return capacity.
use plurx_core::{
    domain::MediaFile,
    store::{
        background_jobs_resources::{
            ForegroundSourceOwner, ForegroundSourcePhase, ForegroundSourcePolicy,
            ForegroundSourceProjection, ForegroundSourceToken, SourceServiceClass,
            SourceServiceGrant,
        },
        Store,
    },
};
use std::sync::{Arc, OnceLock};
use tokio::sync::{mpsc, oneshot};

const LEASE_MS: i64 = 30_000;
pub(crate) const SERVICE_EPOCH_MS: i64 = 5_000;
pub(crate) const BACKGROUND_QUANTUM: u64 = 1_048_576;

pub(crate) fn source_pressure_changed() -> &'static tokio::sync::Notify {
    static CHANGED: OnceLock<tokio::sync::Notify> = OnceLock::new();
    CHANGED.get_or_init(tokio::sync::Notify::new)
}

#[derive(Clone)]
pub(crate) struct SourceIdentity {
    pub(crate) node_id: String,
    pub(crate) boot_id: uuid::Uuid,
}
#[derive(Clone)]
pub(crate) struct SourceRequest {
    store: Arc<dyn Store>,
    owner: ForegroundSourceOwner,
    file: MediaFile,
}
/// Permission for a strict child that must keep its original descriptor in a
/// network-isolated sandbox. The existing child owner retains this value until
/// confirmed reap and broker join. No read is performed by this budget and no
/// quiescence is inferred from a syscall permission or elapsed lease.
#[cfg(target_os = "linux")]
pub(crate) struct SourceChildReadBudget {
    _source: crate::fragment_index_cluster::SourceFence,
    custody: SourceCustody,
    budget: tokio::sync::Mutex<(Option<SourceServiceGrant>, u64)>,
    pause: Option<plurx_core::process::bounded::SourcePauseController>,
}
#[cfg(target_os = "linux")]
impl SourceChildReadBudget {
    pub(crate) async fn reserve_bytes(
        &self,
        count: usize,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        if count == 0 {
            return Ok(());
        }
        if count > 1024 * 1024 {
            return Err("strict child source read exceeds syscall bound".into());
        }
        let observer = self.custody.observer();
        let mut budget = tokio::select! {biased;
            ()=cancel.cancelled()=>return Err("strict child source read cancelled".into()),
            budget=self.budget.lock()=>budget,
        };
        loop {
            if !observer.has_authority() {
                return Err("strict child source authority lost".into());
            }
            tokio::select! {biased;
                ()=cancel.cancelled()=>return Err("strict child source read cancelled".into()),
                result=observer.reads_allowed()=>result?,
            }
            if observer.rolling_policy() {
                return Ok(());
            }
            let now = crate::fragment_index_cluster::unix_ms();
            if budget
                .0
                .as_ref()
                .is_none_or(|grant| now >= grant.expires_at_ms)
            {
                budget.0 = Some(observer.transfer_epoch().await?);
                budget.1 = 0;
            }
            let grant = budget.0.as_ref().expect("strict source epoch");
            if grant.foreground_bytes.saturating_sub(budget.1) >= count as u64 {
                // Reserve the whole issued scalar syscall before CONTINUE.
                // Failed, partial and abandoned reads never refund this credit.
                budget.1 = budget.1.saturating_add(count as u64);
                return Ok(());
            }
            let now = crate::fragment_index_cluster::unix_ms();
            if now >= grant.expires_at_ms {
                continue;
            }
            let wait = grant.expires_at_ms.saturating_sub(now) as u64;
            let _quota = self
                .pause
                .as_ref()
                .map(|control| control.finite_quota_wait());
            tokio::select! {biased;
                ()=cancel.cancelled()=>return Err("strict child source read cancelled".into()),
                ()=observer.authority_lost()=>return Err("strict child source authority lost".into()),
                ()=tokio::time::sleep(std::time::Duration::from_millis(wait))=>{},
            }
        }
    }
}
impl SourceIdentity {
    pub(crate) fn request(
        &self,
        store: Arc<dyn Store>,
        file: &MediaFile,
        lifetime: &str,
        incarnation: uuid::Uuid,
        policy: ForegroundSourcePolicy,
    ) -> SourceRequest {
        use sha2::{Digest, Sha256};
        let mut identity = Sha256::new();
        identity.update(file.id.to_le_bytes());
        identity.update(file.size.to_le_bytes());
        identity.update(file.mtime.to_le_bytes());
        identity.update(file.path.as_os_str().to_string_lossy().as_bytes());
        SourceRequest {
            file: file.clone(),
            store,
            owner: ForegroundSourceOwner {
                owner_id: uuid::Uuid::new_v4().to_string(),
                lifetime_id: lifetime.to_owned(),
                session_incarnation: incarnation.to_string(),
                producer_attempt: "pending".to_owned(),
                node_id: self.node_id.clone(),
                boot_id: self.boot_id.to_string(),
                file_id: file.id,
                source_size: file.size,
                source_mtime: file.mtime,
                source_generation: hex::encode(identity.finalize()),
                policy,
            },
        }
    }
}

pub(crate) struct SourceCustody {
    observer: SourceObserver,
    settled: bool,
}
#[derive(Clone)]
pub(crate) struct SourceObserver {
    commands: mpsc::UnboundedSender<Command>,
    reads: Arc<ReadActivity>,
    foreground_bytes: u64,
}
#[derive(Default)]
struct ReadActivity {
    known: std::sync::atomic::AtomicBool,
    active: std::sync::atomic::AtomicUsize,
    rolling: std::sync::atomic::AtomicBool,
    policy_changed: tokio::sync::Notify,
    authority_lost: tokio_util::sync::CancellationToken,
    reads_held: std::sync::atomic::AtomicBool,
    sharing: std::sync::atomic::AtomicBool,
    gate: std::sync::Mutex<()>,
    reads_resumed: tokio::sync::Notify,
    completed_read: std::sync::atomic::AtomicBool,
    read_settled: tokio::sync::Notify,
    foreground_bytes: std::sync::atomic::AtomicU64,
}
pub(crate) struct SourceReadActivity(Arc<ReadActivity>);
impl SourceReadActivity {
    pub(crate) fn completed(&self) {
        self.0
            .completed_read
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.0.read_settled.notify_one();
    }
}
impl Drop for SourceReadActivity {
    fn drop(&mut self) {
        self.0
            .active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
enum Command {
    Project(
        ForegroundSourceProjection,
        oneshot::Sender<Result<(), String>>,
    ),
    Settled,
    CleanupPending,
}
impl SourceRequest {
    #[cfg(target_os = "linux")]
    pub(crate) async fn register_child_reads(
        self,
        held: &std::fs::File,
        attempt: &str,
    ) -> Result<Arc<SourceChildReadBudget>, String> {
        let source = crate::fragment_index_cluster::SourceFence::clone_current_held(held)?;
        let custody = self
            .object_version(source.object_version())
            .register(attempt)
            .await?;
        // Deliberately do not call mediate_reads: the sandbox's actual syscall
        // completion cannot be inferred by its pre-read permission broker.
        Ok(Arc::new(SourceChildReadBudget {
            _source: source,
            custody,
            budget: tokio::sync::Mutex::new((None, 0)),
            pause: plurx_core::process::bounded::source_pause_controller(),
        }))
    }
    fn foreground_epoch_bytes(&self) -> u64 {
        // The quantum is a reserved minimum, not a low throughput ceiling.
        // Size a bounded five-second class for three times container average;
        // unknown-rate media gets a bounded 64 MiB class, explicitly unqualified.
        let bps = self
            .file
            .bitrate
            .filter(|rate| *rate > 0)
            .map(|rate| rate as u128)
            .or_else(|| {
                self.file
                    .duration_ms
                    .filter(|duration| *duration > 0)
                    .map(|duration| {
                        (self.file.size.max(0) as u128).saturating_mul(8_000) / (duration as u128)
                    })
            });
        let foreground =
            bps.map_or(64_u128 * 1024 * 1024, |bps| {
                bps.saturating_mul(5_000).saturating_mul(3) / 8_000
            })
            .clamp(u128::from(BACKGROUND_QUANTUM * 2), 512_u128 * 1024 * 1024) as u64;
        foreground.div_ceil(2) * 2
    }
    pub(crate) fn file(&self) -> &MediaFile {
        &self.file
    }
    pub(crate) fn object_version(mut self, version: &str) -> Self {
        use sha2::{Digest, Sha256};
        self.owner.source_generation = hex::encode(Sha256::digest(version.as_bytes()));
        self
    }
    /// Registration is the only start barrier. It never waits for another
    /// reader's physical pause/reap, whose old charge remains in the ledger.
    pub(crate) async fn register(&self, attempt: impl ToString) -> Result<SourceCustody, String> {
        let now = crate::fragment_index_cluster::unix_ms();
        let mut owner = self.owner.clone();
        owner.owner_id = uuid::Uuid::new_v4().to_string();
        owner.producer_attempt = attempt.to_string();
        let candidate = ForegroundSourceToken {
            owner: owner.clone(),
            fence: 1,
            lease_expires_at_ms: now + LEASE_MS,
        };
        let token = match self
            .store
            .register_foreground_source(owner, now, now + LEASE_MS)
            .await
        {
            Ok(Some(token)) => token,
            outcome => match self
                .store
                .resolve_foreground_source(candidate.clone())
                .await
            {
                Ok(Some(token)) => token,
                Ok(None) => {
                    return Err(match outcome {
                        Err(error) => error.to_string(),
                        _ => "original-source registration lost authority".into(),
                    })
                }
                Err(error) => {
                    // No input was opened. A lost registration acknowledgement
                    // still needs an exact cleanup owner until absence is proved.
                    let (commands, receiver) = mpsc::unbounded_channel();
                    let _ = commands.send(Command::Settled);
                    tokio::spawn(run_owner(
                        Arc::clone(&self.store),
                        candidate,
                        receiver,
                        Arc::new(ReadActivity::default()),
                    ));
                    return Err(error.to_string());
                }
            },
        };
        source_pressure_changed().notify_waiters();
        let (commands, receiver) = mpsc::unbounded_channel();
        let reads = Arc::new(ReadActivity::default());
        reads.foreground_bytes.store(
            self.foreground_epoch_bytes(),
            std::sync::atomic::Ordering::SeqCst,
        );
        reads.rolling.store(
            self.owner.policy == ForegroundSourcePolicy::Rolling,
            std::sync::atomic::Ordering::SeqCst,
        );
        tokio::spawn(run_owner(
            Arc::clone(&self.store),
            token,
            receiver,
            Arc::clone(&reads),
        ));
        Ok(SourceCustody {
            observer: SourceObserver {
                commands,
                reads,
                foreground_bytes: self.foreground_epoch_bytes(),
            },
            settled: false,
        })
    }
}
impl SourceCustody {
    pub(crate) fn observer(&self) -> SourceObserver {
        self.observer.clone()
    }
    /// A failed wait is not cleanup proof. The independent ledger owner keeps
    /// renewing the visible cleanup charge until the physical owner repairs it.
    pub(crate) fn cleanup_pending(mut self) {
        self.settled = true;
        let _ = self.observer.commands.send(Command::CleanupPending);
    }
}
impl Drop for SourceCustody {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.observer.commands.send(Command::Settled);
        }
    }
}
impl SourceObserver {
    /// This RAII counter belongs inside the actual blocking read closure.
    /// Knowing the waiter disappeared never implies source quiescence.
    pub(crate) async fn begin_read(&self) -> Result<SourceReadActivity, String> {
        loop {
            if let Some(activity) = self.try_active_read() {
                return Ok(activity);
            }
            self.reads_allowed().await?;
        }
    }
    pub(crate) fn try_active_read(&self) -> Option<SourceReadActivity> {
        let _gate = self.reads.gate.lock().expect("source read gate");
        if self
            .reads
            .reads_held
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return None;
        }
        self.reads
            .active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.reads
            .known
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Some(SourceReadActivity(Arc::clone(&self.reads)))
    }
    async fn project(&self, projection: ForegroundSourceProjection) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Project(projection, reply))
            .map_err(|_| "original-source owner ended".to_owned())?;
        let result = response
            .await
            .map_err(|_| "original-source projection owner ended".to_owned())?;
        if result.is_err() {
            self.reads.authority_lost.cancel();
        }
        result
    }
    pub(crate) async fn refill(&self) -> Result<(), String> {
        let mut projection =
            ForegroundSourceProjection::startup(crate::fragment_index_cluster::unix_ms());
        projection.phase = ForegroundSourcePhase::Refill;
        self.project(projection).await?;
        self.reads
            .reads_held
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.reads.reads_resumed.notify_waiters();
        Ok(())
    }
    pub(crate) async fn rolling(
        &self,
        lead_ms: Option<u64>,
        low_ms: u64,
        high_ms: u64,
        held: bool,
        fresh: bool,
    ) -> Result<(), String> {
        if !self.rolling_policy() {
            return Ok(());
        }
        let held = {
            let _gate = self.reads.gate.lock().expect("source read gate");
            if held {
                self.reads
                    .reads_held
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            held && self.reads.known.load(std::sync::atomic::Ordering::SeqCst)
                && self.reads.active.load(std::sync::atomic::Ordering::SeqCst) == 0
        };
        let sharing = held
            && fresh
            && lead_ms.is_some_and(|lead| {
                lead > low_ms
                    && (lead >= high_ms
                        || self.reads.sharing.load(std::sync::atomic::Ordering::SeqCst))
            });
        self.reads
            .sharing
            .store(sharing, std::sync::atomic::Ordering::SeqCst);
        let now = crate::fragment_index_cluster::unix_ms();
        let mut projection = ForegroundSourceProjection::startup(now);
        projection.phase = if sharing {
            ForegroundSourcePhase::Sharing
        } else {
            ForegroundSourcePhase::Refill
        };
        projection.source_quiescent = held;
        projection.contiguous_lead_ms = lead_ms;
        projection.low_ms = Some(low_ms);
        projection.high_ms = Some(high_ms);
        projection.observation_expires_at_ms = now + SERVICE_EPOCH_MS;
        self.project(projection).await
    }
    pub(crate) fn has_completed_read(&self) -> bool {
        self.reads
            .completed_read
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    pub(crate) fn mediate_reads(&self) {
        self.reads
            .known
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub(crate) async fn reads_allowed(&self) -> Result<(), String> {
        loop {
            let resumed = self.reads.reads_resumed.notified();
            tokio::pin!(resumed);
            resumed.as_mut().enable();
            if !self.has_authority() {
                return Err("source authority lost".into());
            }
            if !self
                .reads
                .reads_held
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(());
            }
            if !self.rolling_policy() {
                self.refill().await?;
                continue;
            }
            tokio::select! {
                ()=&mut resumed=>{},
                ()=self.authority_lost()=>return Err("source authority lost".into()),
            }
        }
    }
    pub(crate) fn has_authority(&self) -> bool {
        !self.reads.authority_lost.is_cancelled()
    }
    pub(crate) fn mark_cleanup_pending(&self) {
        let _ = self.commands.send(Command::CleanupPending);
    }
    pub(crate) async fn authority_lost(&self) {
        self.reads.authority_lost.cancelled().await;
    }
    pub(crate) async fn policy_changed(&self) {
        self.reads.policy_changed.notified().await;
    }
    pub(crate) async fn declare_rolling_policy(&self) -> Result<(), String> {
        if self.rolling_policy() {
            return Ok(());
        }
        // Close the old transfer opportunity before making source reads
        // unconstrained by that class's finite grant.
        self.refill().await?;
        self.reads
            .rolling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.reads.policy_changed.notify_one();
        Ok(())
    }
    pub(crate) fn rolling_policy(&self) -> bool {
        self.reads.rolling.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub(crate) async fn transfer_epoch(&self) -> Result<SourceServiceGrant, String> {
        let now = crate::fragment_index_cluster::unix_ms();
        let grant = SourceServiceGrant {
            epoch_id: u64::try_from(now / SERVICE_EPOCH_MS).unwrap_or(0),
            starts_at_ms: (now / SERVICE_EPOCH_MS) * SERVICE_EPOCH_MS,
            expires_at_ms: (now / SERVICE_EPOCH_MS + 1) * SERVICE_EPOCH_MS,
            foreground_bytes: self.foreground_bytes,
            background_bytes: self.foreground_bytes / 2,
            foreground_floor_bps: None,
            supply_floor_bps: None,
            service_class: SourceServiceClass::WeightedUnqualified,
        };
        let mut projection = ForegroundSourceProjection::startup(now);
        projection.phase = ForegroundSourcePhase::TransferService;
        projection.observation_expires_at_ms = grant.expires_at_ms;
        projection.transfer_grant = Some(grant.clone());
        self.project(projection).await?;
        Ok(grant)
    }
    /// Call only after the read join and all prefetch have settled. Holding
    /// buffered materialized output alone does not spend source occupancy.
    pub(crate) async fn idle(&self) -> Result<(), String> {
        let now = crate::fragment_index_cluster::unix_ms();
        let mut projection = ForegroundSourceProjection::startup(now);
        projection.phase = ForegroundSourcePhase::SourceIdle;
        projection.source_quiescent = true;
        projection.observation_expires_at_ms = now + SERVICE_EPOCH_MS;
        self.project(projection).await
    }
}
async fn release_settled(
    store: &dyn Store,
    token: &mut ForegroundSourceToken,
    now: i64,
) -> Result<(), String> {
    if let Ok(true) = store.release_foreground_source(token.clone(), now).await {
        return Ok(());
    }
    match store
        .resolve_foreground_source(token.clone())
        .await
        .map_err(|error| error.to_string())?
    {
        None => Ok(()),
        Some(current) => {
            *token = current;
            if store
                .release_foreground_source(token.clone(), now)
                .await
                .map_err(|error| error.to_string())?
            {
                Ok(())
            } else {
                Err("source release fence changed; exact owner retained".into())
            }
        }
    }
}
async fn run_owner(
    store: Arc<dyn Store>,
    mut token: ForegroundSourceToken,
    mut commands: mpsc::UnboundedReceiver<Command>,
    reads: Arc<ReadActivity>,
) {
    let mut projection =
        ForegroundSourceProjection::startup(crate::fragment_index_cluster::unix_ms());
    let mut published_projection = projection.clone();
    let mut version = 0_u64;
    let mut cleanup_pending = false;
    let mut release_pending = false;
    let mut first_read_promoted = false;
    let mut last_publication_ms = crate::fragment_index_cluster::unix_ms();
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(5));
    heartbeat.tick().await;
    loop {
        let reply = tokio::select! {
            command=commands.recv()=>match command {
                Some(Command::Settled)=>{release_pending=true;cleanup_pending=true;None},
                Some(Command::CleanupPending)=>{cleanup_pending=true;None},
                Some(Command::Project(next,reply)) if !cleanup_pending=>{
                    let now=crate::fragment_index_cluster::unix_ms();
                    if next.phase==published_projection.phase && next.source_quiescent==published_projection.source_quiescent
                        && next.transfer_grant.as_ref().map(|grant|grant.epoch_id)==published_projection.transfer_grant.as_ref().map(|grant|grant.epoch_id)
                        && now.saturating_sub(last_publication_ms)<SERVICE_EPOCH_MS {
                        projection=next;let _=reply.send(Ok(()));continue;
                    }
                    projection=next;Some(reply)
                },
                Some(Command::Project(_,reply))=>{let _=reply.send(Err("source cleanup is pending".into()));continue;},
                None=>{
                    if !cleanup_pending {release_pending=true;cleanup_pending=true;}
                    heartbeat.tick().await;None
                }
            },
            _=reads.read_settled.notified()=>{
                if first_read_promoted||cleanup_pending||reads.rolling.load(std::sync::atomic::Ordering::SeqCst) {continue;}
                first_read_promoted=true;
                if projection.phase!=ForegroundSourcePhase::Startup {continue;}
                let now=crate::fragment_index_cluster::unix_ms();
                projection=ForegroundSourceProjection::startup(now);
                projection.phase=ForegroundSourcePhase::TransferService;
                let foreground=reads.foreground_bytes.load(std::sync::atomic::Ordering::SeqCst).max(BACKGROUND_QUANTUM*2);
                projection.transfer_grant=Some(SourceServiceGrant {epoch_id:(now/SERVICE_EPOCH_MS) as u64,
                    starts_at_ms:(now/SERVICE_EPOCH_MS)*SERVICE_EPOCH_MS,expires_at_ms:(now/SERVICE_EPOCH_MS+1)*SERVICE_EPOCH_MS,
                    foreground_bytes:foreground,background_bytes:foreground/2,foreground_floor_bps:None,supply_floor_bps:None,
                    service_class:SourceServiceClass::WeightedUnqualified});
                projection.observation_expires_at_ms=(now/SERVICE_EPOCH_MS+1)*SERVICE_EPOCH_MS;None
            },
            _=heartbeat.tick()=>None,
        };
        let now = crate::fragment_index_cluster::unix_ms();
        if release_pending {
            match release_settled(store.as_ref(), &mut token, now).await {
                Ok(()) => {
                    source_pressure_changed().notify_waiters();
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error,owner=%token.owner.owner_id,"settled source release retained for exact retry")
                }
            }
        }
        if cleanup_pending {
            projection.phase = ForegroundSourcePhase::CleanupPending;
            projection.source_quiescent = false;
            projection.transfer_grant = None;
        } else if projection.observation_expires_at_ms <= now
            && matches!(
                projection.phase,
                ForegroundSourcePhase::Sharing | ForegroundSourcePhase::TransferService
            )
        {
            if projection.phase == ForegroundSourcePhase::Sharing {
                reads
                    .sharing
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
            let idle = {
                let _gate = reads.gate.lock().expect("source read gate");
                let idle = projection.phase == ForegroundSourcePhase::TransferService
                    && reads.known.load(std::sync::atomic::Ordering::SeqCst)
                    && reads.active.load(std::sync::atomic::Ordering::SeqCst) == 0;
                if idle {
                    reads
                        .reads_held
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                idle
            };
            projection.phase = if idle {
                ForegroundSourcePhase::SourceIdle
            } else {
                ForegroundSourcePhase::Refill
            };
            projection.source_quiescent = idle;
            projection.transfer_grant = None;
        }
        version = version
            .max(u64::try_from(token.fence).unwrap_or(0))
            .saturating_add(1);
        projection.observation_version = version;
        let result = store
            .project_foreground_source(token.clone(), projection.clone(), now, now + LEASE_MS)
            .await;
        match result {
            Ok(Some(next)) => {
                token = next;
                published_projection = projection.clone();
                last_publication_ms = now;
                source_pressure_changed().notify_waiters();
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            outcome => {
                // A committed write may have lost its acknowledgement. Resolve
                // the exact immutable owner before retrying a newer CAS; never
                // abandon the receiver while its physical custody survives.
                reads.authority_lost.cancel();
                cleanup_pending = true;
                match store.resolve_foreground_source(token.clone()).await {
                    Ok(Some(current)) => token = current,
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error,owner=%token.owner.owner_id,"source fence resolution will retry")
                    }
                }
                let error = match outcome {
                    Err(error) => error.to_string(),
                    _ => "source projection fence changed".into(),
                };
                if let Some(reply) = reply {
                    let _ = reply.send(Err(error));
                }
            }
        }
    }
}

/// One direct-range descriptor. A blocking read owns an Arc independently of
/// its waiter, so disconnect cannot return source credit before the read join.
struct DirectReader {
    source: std::sync::Mutex<crate::fragment_index_cluster::SourceFence>,
    _custody: SourceCustody,
}
pub(crate) async fn file_body(
    source: crate::fragment_index_cluster::SourceFence,
    start: u64,
    len: u64,
    request: SourceRequest,
) -> Result<axum::body::Body, String> {
    let custody = request
        .object_version(source.object_version())
        .register(format!("range-{start}-{len}"))
        .await?;
    let observer = custody.observer();
    let reader = Arc::new(DirectReader {
        source: std::sync::Mutex::new(source),
        _custody: custody,
    });
    let stream = futures_util::stream::try_unfold(
        (
            reader,
            observer,
            start,
            len,
            None::<SourceServiceGrant>,
            0_u64,
        ),
        |(reader, observer, offset, remaining, mut grant, mut used)| async move {
            if remaining == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            if let Some(current) = &grant {
                let now = crate::fragment_index_cluster::unix_ms();
                if used >= current.foreground_bytes && now < current.expires_at_ms {
                    // No source read remains outstanding: yield the finite
                    // background quantum rather than holding startup forever.
                    observer.idle().await.map_err(std::io::Error::other)?;
                    tokio::time::sleep(std::time::Duration::from_millis(
                        (current.expires_at_ms - now) as u64,
                    ))
                    .await;
                }
                if used >= current.foreground_bytes
                    || crate::fragment_index_cluster::unix_ms() >= current.expires_at_ms
                {
                    observer.refill().await.map_err(std::io::Error::other)?;
                    grant = Some(
                        observer
                            .transfer_epoch()
                            .await
                            .map_err(std::io::Error::other)?,
                    );
                    used = 0;
                }
            }
            let allowance = grant.as_ref().map_or(64 * 1024, |grant| {
                grant.foreground_bytes.saturating_sub(used)
            });
            let size = remaining.min(64 * 1024).min(allowance) as usize;
            if !observer.has_authority() {
                return Err(std::io::Error::other("source authority lost"));
            }
            let work_reader = Arc::clone(&reader);
            let read_activity = observer.begin_read().await.map_err(std::io::Error::other)?;
            let bytes = tokio::task::spawn_blocking(move || {
                let read_activity = read_activity;
                use std::io::{Read, Seek};
                let mut source = work_reader.source.lock().expect("direct source read");
                if !source.unchanged() {
                    return Err(std::io::Error::other("source range changed"));
                }
                source.handle.seek(std::io::SeekFrom::Start(offset))?;
                let mut bytes = vec![0; size];
                let n = source.handle.read(&mut bytes)?;
                if !source.unchanged() {
                    return Err(std::io::Error::other("source range changed"));
                }
                bytes.truncate(n);
                if n > 0 {
                    read_activity.completed();
                }
                Ok::<_, std::io::Error>(bytes)
            })
            .await
            .map_err(std::io::Error::other)??;
            if bytes.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "source range truncated",
                ));
            }
            if grant.is_none() {
                grant = Some(
                    observer
                        .transfer_epoch()
                        .await
                        .map_err(std::io::Error::other)?,
                );
            }
            let count = bytes.len() as u64;
            Ok(Some((
                bytes::Bytes::from(bytes),
                (
                    reader,
                    observer,
                    offset + count,
                    remaining - count,
                    grant,
                    used + count,
                ),
            )))
        },
    );
    Ok(axum::body::Body::from_stream(stream))
}

/// Only callers holding an unreaped producer from producer_spawn may signal
/// this group. Its id cannot have been reused while that child is owned.
pub(crate) fn signal_group(
    pid: u32,
    signal: crate::process_control::ProcessSignal,
) -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let group = i32::try_from(pid).map_err(std::io::Error::other)?;
        let signal = match signal {
            crate::process_control::ProcessSignal::Suspend => libc::SIGSTOP,
            crate::process_control::ProcessSignal::Resume => libc::SIGCONT,
            crate::process_control::ProcessSignal::Terminate => libc::SIGKILL,
        };
        // SAFETY: the exact unreaped child reserves this fresh group id.
        if unsafe { libc::kill(-group, signal) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        crate::process_control::signal(pid, signal)
    }
}
pub(crate) async fn stopped_group(pid: Option<u32>) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Some(group) = pid.and_then(|pid| i32::try_from(pid).ok()) else {
            return false;
        };
        if !plurx_core::process::bounded::source_group_quiescent(group, true)
            .await
            .unwrap_or(false)
        {
            return false;
        }
        // Reissue STOP before a second complete group/thread census to close
        // a fork racing the first acknowledgement. D-state stays charged.
        if !signal_group(group as u32, crate::process_control::ProcessSignal::Suspend)
            .unwrap_or(false)
        {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        plurx_core::process::bounded::source_group_quiescent(group, true)
            .await
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

tokio::task_local! {static FOREGROUND_REQUEST:SourceRequest;}
pub(crate) async fn with_request<T>(
    request: Option<SourceRequest>,
    work: impl std::future::Future<Output = T>,
) -> T {
    match request {
        Some(request) => FOREGROUND_REQUEST.scope(request, work).await,
        None => work.await,
    }
}
pub(crate) fn current_request() -> Option<SourceRequest> {
    FOREGROUND_REQUEST.try_with(Clone::clone).ok()
}
