//! Cancellation-independent bounded owner queue. This is an I/O executor, not a
//! timer/recovery daemon. The existing supervisor decides when to retry holds.
use super::*;
use std::collections::BTreeMap;
use std::sync::{mpsc, Mutex};
use tokio::sync::oneshot;

#[cfg(test)]
mod tests;

const MAX_OWNED_OPERATIONS: usize = 2048;
struct OwnedOperation {
    operation: QueuedOperation,
    result: Option<Result<OperationResult, CustodyError>>,
    queued: bool,
}
struct Book {
    operations: BTreeMap<Uuid, Arc<Mutex<OwnedOperation>>>,
}
struct Work {
    operation: Arc<Mutex<OwnedOperation>>,
    reply: oneshot::Sender<Result<OperationResult, CustodyError>>,
}
/// The owner handle is intentionally not Clone. Arc it at the composed runtime,
/// never expose it through request DTOs. Dropping all handles drains the worker.
pub struct CustodyJournal {
    journal_id: Uuid,
    owner_id: Uuid,
    sender: mpsc::SyncSender<Work>,
    book: Arc<Mutex<Book>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
/// Waiter only. Drop does not cancel queued work, proof retention, or accounting.
#[must_use]
pub struct PendingOperation {
    id: Uuid,
    receiver: oneshot::Receiver<Result<OperationResult, CustodyError>>,
}
impl PendingOperation {
    pub fn operation_id(&self) -> Uuid {
        self.id
    }
    pub(crate) async fn wait(mut self) -> Result<OperationResult, CustodyError> {
        self.wait_retained().await
    }
    /// Cancellation of a borrowed runtime wait preserves the actual receiver.
    pub(crate) async fn wait_retained(&mut self) -> Result<OperationResult, CustodyError> {
        (&mut self.receiver).await.map_err(|_| {
            let mut error = CustodyError::new(
                ErrorKind::WorkerStopped,
                "custody result waiter disconnected",
            )
            .bind(self.id, Vec::new());
            // The owner may have failed AFTER COMMIT, before sending its result.
            error.certainty = CommitCertainty::Unknown;
            error
        })?
    }
}
fn obligations(command: &Command) -> Vec<Uuid> {
    match command {
        Command::Prepare { handles, .. } => handles.iter().map(|h| h.obligation_id).collect(),
        Command::Change { key, .. } => vec![key.handle.obligation_id],
    }
}
fn unavailable() -> CustodyError {
    CustodyError::new(ErrorKind::WorkerStopped, "custody owner state unavailable")
}

impl CustodyJournal {
    /// Blocking startup only. Keep the returned reconstruction authoritative and
    /// cleanup-only until main installs every charge/latch/protected path.
    pub fn open(options: OpenOptions) -> Result<(Self, RecoverySnapshot), CustodyError> {
        if options.queue_capacity == 0
            || options.queue_capacity > 1024
            || options.excluded_roots.len() > 64
        {
            return Err(CustodyError::new(
                ErrorKind::InvalidCommand,
                "invalid custody queue or storage bound",
            ));
        }
        transaction::validate_owner(&options.owner)?;
        let mut storage = storage::Storage::open(&options)?;
        let snapshot = transaction::snapshot(&storage.connection, &storage.witness)?;
        transaction::register_owner(&mut storage.connection, &options.owner)?;
        storage.verify()?;
        let journal_id = storage.witness.journal_id;
        let owner_id = options.owner.owner_id;
        let (sender, receiver) = mpsc::sync_channel::<Work>(options.queue_capacity);
        let book = Arc::new(Mutex::new(Book {
            operations: BTreeMap::new(),
        }));
        let worker_book = book.clone();
        let daemon_lock = options.daemon_lock.0;
        let worker = std::thread::Builder::new()
            .name("resource-custody".into())
            .spawn(move || {
                // Lock lifetime includes every admitted queue operation and proof.
                let _daemon_lock = daemon_lock;
                while let Ok(work) = receiver.recv() {
                    let result = run(&mut storage, owner_id, &worker_book, &work.operation);
                    let _ = work.reply.send(result);
                }
            })?;
        Ok((
            Self {
                journal_id,
                owner_id,
                sender,
                book,
                worker: Some(worker),
            },
            snapshot,
        ))
    }
    pub(crate) fn new_family(&self) -> FamilyHandle {
        FamilyHandle {
            journal_id: self.journal_id,
            family_id: Uuid::new_v4(),
        }
    }
    /// Create once and retain operation_id in the runtime before submission.
    /// Exact retry uses retry(), not this constructor (which mints obligations).
    pub(crate) fn prepare(
        &self,
        operation_id: Uuid,
        batch: PrepareBatch,
        deadline: Instant,
    ) -> Result<PendingOperation, CustodyError> {
        codec::nonnil(operation_id)?;
        let handles = batch
            .obligations
            .iter()
            .map(|n| ObligationHandle {
                journal_id: self.journal_id,
                owner_id: self.owner_id,
                obligation_id: Uuid::new_v4(),
                graph_id: n.manifest.graph_id,
                runtime_binding: n.runtime_binding,
            })
            .collect();
        let trial = batch.trial.then(Uuid::new_v4);
        self.enqueue(QueuedOperation {
            id: operation_id,
            deadline,
            command: Command::Prepare {
                batch,
                handles,
                trial,
            },
        })
        .map_err(|(error, _unaccepted_prepare)| error)
    }
    pub(crate) fn change(
        &self,
        key: TransitionKey,
        mutation: Mutation,
        deadline: Instant,
    ) -> Result<PendingOperation, RejectedTransition> {
        if key.operation_id.is_nil() {
            return Err(RejectedTransition {
                error: codec::invalid(),
                key,
                mutation,
            });
        }
        if key.handle.journal_id != self.journal_id || key.handle.owner_id != self.owner_id {
            return Err(RejectedTransition {
                error: CustodyError::new(ErrorKind::Conflict, "foreign custody handle"),
                key,
                mutation,
            });
        }
        match self.enqueue(QueuedOperation {
            id: key.operation_id,
            deadline,
            command: Command::Change { key, mutation },
        }) {
            Ok(pending) => Ok(pending),
            Err((error, operation)) => match operation.command {
                Command::Change { key, mutation } => Err(RejectedTransition {
                    error,
                    key,
                    mutation,
                }),
                Command::Prepare { .. } => unreachable!("change constructs only a transition"),
            },
        }
    }
    fn enqueue(
        &self,
        operation: QueuedOperation,
    ) -> Result<PendingOperation, (CustodyError, QueuedOperation)> {
        let id = operation.id;
        let affected = obligations(&operation.command);
        if operation.deadline <= Instant::now() {
            return Err((
                CustodyError::new(
                    ErrorKind::Deadline,
                    "custody deadline elapsed before enqueue",
                )
                .bind(id, affected),
                operation,
            ));
        }
        let mut book = match self.book.lock() {
            Ok(book) => book,
            Err(_) => return Err((unavailable().bind(id, affected), operation)),
        };
        if book.operations.contains_key(&id) {
            return Err((
                CustodyError::new(
                    ErrorKind::Conflict,
                    "custody operation already owned; retry exact operation",
                )
                .bind(id, affected),
                operation,
            ));
        }
        if book.operations.len() >= MAX_OWNED_OPERATIONS {
            return Err((
                CustodyError::new(
                    ErrorKind::QueueFull,
                    "custody retained operation bound reached",
                )
                .bind(id, affected),
                operation,
            ));
        }
        let owned = Arc::new(Mutex::new(OwnedOperation {
            operation,
            result: None,
            queued: true,
        }));
        let (reply, receiver) = oneshot::channel();
        match self.sender.try_send(Work {
            operation: owned.clone(),
            reply,
        }) {
            Ok(()) => {
                book.operations.insert(id, owned);
                Ok(PendingOperation { id, receiver })
            }
            Err(error) => {
                let (error, work) = match error {
                    mpsc::TrySendError::Full(work) => (
                        CustodyError::new(
                            ErrorKind::QueueFull,
                            "custody queue full before acquisition",
                        ),
                        work,
                    ),
                    mpsc::TrySendError::Disconnected(work) => (unavailable(), work),
                };
                drop(work);
                // Not in book and never delivered: only this local Arc exists.
                // Return the exact command, including its nonclone receipt.
                let mutex = match Arc::try_unwrap(owned) {
                    Ok(mutex) => mutex,
                    Err(_) => unreachable!("unaccepted work has no external owner"),
                };
                let owned = mutex
                    .into_inner()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                Err((error.bind(id, affected), owned.operation))
            }
        }
    }
    /// Re-submit SAME retained command/proof and UUID; a fresh operation cannot
    /// overtake an uncertain one on that obligation. No behavioral polling.
    pub(crate) fn retry(
        &self,
        id: Uuid,
        deadline: Instant,
    ) -> Result<PendingOperation, CustodyError> {
        // Registration and queued-state change are atomic with forgetting/taking
        // an operation; no detached retry may execute after proof was returned.
        let book = self.book.lock().map_err(|_| unavailable())?;
        let owned = book.operations.get(&id).cloned().ok_or_else(unavailable)?;
        {
            let mut operation = owned.lock().map_err(|_| unavailable())?;
            if operation.result.as_ref().is_some_and(Result::is_ok) {
                let result = operation.result.clone().ok_or_else(unavailable)?;
                let (reply, receiver) = oneshot::channel();
                let _ = reply.send(result);
                return Ok(PendingOperation { id, receiver });
            }
            if operation.queued {
                return Err(CustodyError::new(
                    ErrorKind::Conflict,
                    "custody operation still executing",
                ));
            }
            operation.operation.deadline = deadline;
            operation.queued = true;
        }
        drop(book);
        let (reply, receiver) = oneshot::channel();
        if let Err(error) = self.sender.try_send(Work {
            operation: owned.clone(),
            reply,
        }) {
            owned.lock().map_err(|_| unavailable())?.queued = false;
            return Err(match error {
                mpsc::TrySendError::Full(_) => {
                    CustodyError::new(ErrorKind::QueueFull, "custody retry queue full")
                }
                mpsc::TrySendError::Disconnected(_) => unavailable(),
            });
        }
        Ok(PendingOperation { id, receiver })
    }
    /// Main may forget a completed non-settlement command, including a definitive
    /// failure with no possible commit. Unknown results must retain exact retry.
    /// Durable operation history remains on disk; late UUID reuse still refuses.
    pub(crate) fn acknowledge(&self, id: Uuid) -> Result<(), CustodyError> {
        let mut book = self.book.lock().map_err(|_| unavailable())?;
        let owned = book.operations.get(&id).ok_or_else(unavailable)?;
        let operation = owned.lock().map_err(|_| unavailable())?;
        if operation.queued
            || operation
                .result
                .as_ref()
                .is_none_or(|r| matches!(r, Err(e) if e.certainty == CommitCertainty::Unknown))
            || matches!(
                operation.operation.command,
                Command::Change {
                    mutation: Mutation::Settle(_),
                    ..
                }
            )
        {
            return Err(CustodyError::new(
                ErrorKind::Conflict,
                "custody operation still owns unresolved proof",
            ));
        }
        drop(operation);
        book.operations.remove(&id);
        Ok(())
    }
    /// Recover the exact mutation (including actual proof) after a definitive
    /// failed transition so the cleanup owner can correct invalid facts/sequence.
    /// Unknown COMMIT can only use retry; it cannot abandon operation custody.
    pub(crate) fn take_failed_transition(
        &self,
        id: Uuid,
    ) -> Result<RejectedTransition, CustodyError> {
        let mut book = self.book.lock().map_err(|_| unavailable())?;
        let owned = book.operations.get(&id).ok_or_else(unavailable)?;
        let mut operation = owned.lock().map_err(|_| unavailable())?;
        let error = match &operation.result {
            Some(Err(error))
                if !operation.queued && error.certainty != CommitCertainty::Unknown =>
            {
                error.clone()
            }
            _ => {
                return Err(CustodyError::new(
                    ErrorKind::Conflict,
                    "custody result remains owned",
                ))
            }
        };
        if !matches!(operation.operation.command, Command::Change { .. }) {
            return Err(CustodyError::new(
                ErrorKind::Conflict,
                "custody operation is not a transition",
            ));
        }
        let command = std::mem::replace(&mut operation.operation.command, empty_command());
        drop(operation);
        book.operations.remove(&id);
        match command {
            Command::Change { key, mutation } => Ok(RejectedTransition {
                error,
                key,
                mutation,
            }),
            _ => unreachable!("validated transition"),
        }
    }
    /// Private post-COMMIT bridge. Only the exact runtime binding may take the
    /// original receipt for the matching in-memory refund. No receipt is reminted.
    pub(crate) fn take_settled_receipt(
        &self,
        id: Uuid,
        handle: &ObligationHandle,
    ) -> Result<PhysicalCloseReceipt, CustodyError> {
        let mut book = self.book.lock().map_err(|_| unavailable())?;
        let owned = book.operations.get(&id).ok_or_else(unavailable)?;
        let operation = owned.lock().map_err(|_| unavailable())?;
        if !matches!(&operation.result,Some(Ok(OperationResult::Changed(ack))) if ack.phase==Phase::Settled && ack.operation_id==id && ack.obligation_id==handle.obligation_id && ack.graph_id==handle.graph_id)
        {
            return Err(CustodyError::new(
                ErrorKind::Conflict,
                "custody settlement is not durably acknowledged",
            ));
        }
        if !matches!(&operation.operation.command,Command::Change { key,mutation:Mutation::Settle(proof) } if key.handle.journal_id==handle.journal_id && key.handle.owner_id==handle.owner_id && key.handle.runtime_binding==handle.runtime_binding && proof.receipt.custody_binding()==handle.runtime_binding)
        {
            return Err(CustodyError::new(
                ErrorKind::Conflict,
                "custody settlement receipt does not match runtime",
            ));
        }
        drop(operation);
        let owned = book.operations.remove(&id).ok_or_else(unavailable)?;
        // Worker releases its Arc before sending the reply; however it may still
        // be returning. Replace command under mutex, not Arc::try_unwrap racing.
        let mut operation = owned.lock().map_err(|_| unavailable())?;
        match std::mem::replace(&mut operation.operation.command, empty_command()) {
            Command::Change {
                mutation: Mutation::Settle(proof),
                ..
            } => Ok(proof.receipt),
            _ => Err(unavailable()),
        }
    }
    /// Blocking shutdown only. Closing the sender drains committed/queued work;
    /// request cancellation must never call this. No thread is left at handoff.
    pub fn drain(mut self) -> Result<(), CustodyError> {
        drop(self.sender);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| unavailable())?;
        }
        Ok(())
    }
}

fn empty_command() -> Command {
    Command::Prepare {
        batch: PrepareBatch {
            obligations: Vec::new(),
            families: Vec::new(),
            trial: false,
        },
        handles: Vec::new(),
        trial: None,
    }
}

fn run(
    storage: &mut storage::Storage,
    owner: Uuid,
    book: &Arc<Mutex<Book>>,
    owned: &Arc<Mutex<OwnedOperation>>,
) -> Result<OperationResult, CustodyError> {
    // Take a short list snapshot; NEVER hold the book mutex across SQLite I/O.
    let others: Vec<_> = book
        .lock()
        .map_err(|_| unavailable())?
        .operations
        .values()
        .filter(|op| !Arc::ptr_eq(op, owned))
        .cloned()
        .collect();
    let mut operation = owned.lock().map_err(|_| unavailable())?;
    if let Some(Ok(result)) = &operation.result {
        return Ok(result.clone());
    }
    let prior_unknown =
        matches!(&operation.result, Some(Err(e)) if e.certainty == CommitCertainty::Unknown);
    let affected = obligations(&operation.operation.command);
    for other in others {
        let other = other.lock().map_err(|_| unavailable())?;
        if matches!(&other.result,Some(Err(e)) if e.certainty==CommitCertainty::Unknown)
            && (matches!(
                operation.operation.command,
                Command::Prepare { .. }
                    | Command::Change {
                        mutation: Mutation::Arm { .. },
                        ..
                    }
            ) || obligations(&other.operation.command)
                .iter()
                .any(|id| affected.contains(id)))
        {
            let mut error = CustodyError::new(
                ErrorKind::Conflict,
                "resolve exact uncertain operation before advancing custody",
            )
            .bind(operation.operation.id, affected);
            if prior_unknown {
                error.certainty = CommitCertainty::Unknown;
            }
            operation.result = Some(Err(error.clone()));
            operation.queued = false;
            return Err(error);
        }
    }
    let result = transaction::execute(storage, &operation.operation, owner).map_err(|mut e| {
        if prior_unknown || e.kind == ErrorKind::Storage {
            e.certainty = CommitCertainty::Unknown;
        } else if e.certainty == CommitCertainty::NotStarted {
            e.certainty = CommitCertainty::RolledBack;
        }
        e.bind(operation.operation.id, affected)
    });
    operation.result = Some(result.clone());
    operation.queued = false;
    result
}
