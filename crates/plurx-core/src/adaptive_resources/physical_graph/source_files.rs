//! Read-only source handles owned by the graph, not by a cancelled request.
//!
//! Blocking operations retain their actual file in a private cell. Neither a
//! pathname nor an operation result can mint a physical-close receipt. These
//! adapters close only their own handle; external readers, remote leases and
//! shared-source ownership still require their own concrete adapters.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex as FileMutex};

use super::*;

pub(super) const MAX_SOURCE_FILES: usize = 64;
const MAX_SOURCE_READ: usize = 1024 * 1024;

/// Graph-bound selection of an owned source, not a transferable File or a close
/// capability. The actual handle and every in-flight operation remain in core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphSourceFile {
    generation: Uuid,
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperationKind {
    Open,
    Read { offset: u64, length: usize },
    Close,
}

struct PendingOperation {
    kind: OperationKind,
    task: JoinHandle<io::Result<Vec<u8>>>,
}

pub(super) struct SourceFile {
    file: Arc<FileMutex<Option<File>>>,
    pending: Option<PendingOperation>,
    ready: bool,
    closed: bool,
    failed: bool,
}

impl SourceFile {
    pub(super) fn retained_read_bytes(&self) -> u64 {
        match self.pending.as_ref().map(|pending| pending.kind) {
            // A finished JoinHandle can still retain its unconsumed Vec. Charge
            // the full requested allocation until complete consumes the result.
            Some(OperationKind::Read { length, .. }) => length as u64,
            _ => 0,
        }
    }

    /// Await by reference: losing the waiter never detaches an unretained task
    /// or drops a result containing the sole actual File.
    async fn complete(&mut self) -> io::Result<Vec<u8>> {
        let Some(pending) = self.pending.as_mut() else {
            return Err(io::Error::other("source operation unavailable"));
        };
        let kind = pending.kind;
        let result = (&mut pending.task)
            .await
            .unwrap_or_else(|_| Err(io::Error::other("source operation abandoned")));
        // No await between consuming the join and replacing its state. A
        // cancelled later caller cannot repoll a completed JoinHandle.
        self.pending = None;
        match (&result, kind) {
            (Ok(_), OperationKind::Open) => self.ready = true,
            (Ok(_), OperationKind::Close) => {
                self.ready = false;
                self.closed = true;
            }
            (Err(_), _) => self.failed = true,
            _ => {}
        }
        result
    }

    pub(super) async fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        if let Some(pending) = self.pending.as_ref() {
            let already_closing = pending.kind == OperationKind::Close;
            // Even a failed open/read must be joined before closing the cell.
            let result = self.complete().await;
            if already_closing {
                return result.map(|_| ());
            }
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let file = Arc::clone(&self.file);
        let task = runtime.spawn_blocking(move || {
            let mut file = file
                .lock()
                .map_err(|_| io::Error::other("source handle owner unavailable"))?;
            // Rust owns this read-only handle. There is no duplicated handle,
            // writable stream or background operation outside this cell/task.
            // This is deliberately NOT OS extinction/remote lease evidence.
            drop(file.take());
            Ok(Vec::new())
        });
        self.pending = Some(PendingOperation {
            kind: OperationKind::Close,
            task,
        });
        self.complete().await.map(|_| ())
    }

    pub(super) fn closed(&self) -> bool {
        self.closed
    }
}

impl OwnedPhysicalGraph {
    /// Open under the graph's exclusive acquisition owner. Physical uncertainty
    /// is recorded BEFORE the blocking call can create the descriptor. The task
    /// installs its actual File into retained custody before it reports success.
    pub async fn open_source_file(&mut self, path: &Path) -> io::Result<GraphSourceFile> {
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let path = path.to_path_buf();
        let custody = self.custody();
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire)
            || members.source_files.len() >= custody.source_limit
        {
            return Err(io::Error::other(
                "physical graph sealed or source bound reached",
            ));
        }
        budgets::check(
            &self.ledger,
            custody,
            &members,
            budgets::BudgetDelta {
                sources: 1,
                ..Default::default()
            },
        )?;
        begin_acquisition(&self.ledger, custody, &mut members)?;
        let file = Arc::new(FileMutex::new(None));
        let owned_file = Arc::clone(&file);
        let task = runtime.spawn_blocking(move || {
            let mut owner = owned_file
                .lock()
                .map_err(|_| io::Error::other("source handle owner unavailable"))?;
            *owner = Some(File::open(path)?);
            Ok(Vec::new())
        });
        let source = GraphSourceFile {
            generation: custody.identity.generation,
            index: members.source_files.len(),
        };
        // No await separates task creation and retention. Cancellation while
        // opening loses only the caller's selection, never core's actual owner.
        members.source_files.push(SourceFile {
            file,
            pending: Some(PendingOperation {
                kind: OperationKind::Open,
                task,
            }),
            ready: false,
            closed: false,
            failed: false,
        });
        members.source_files[source.index].complete().await?;
        Ok(source)
    }

    /// Bounded copied bytes only: never export the counted handle. Retrying a
    /// cancelled read must use its EXACT offset/length; a different request does
    /// not discard the earlier I/O or start a second task behind its owner.
    pub async fn read_source_file_at(
        &mut self,
        source: GraphSourceFile,
        offset: u64,
        length: usize,
    ) -> io::Result<Vec<u8>> {
        if length == 0 || length > MAX_SOURCE_READ || offset.checked_add(length as u64).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source read bound",
            ));
        }
        let custody = self.custody();
        if length > custody.source_read_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source buffer budget",
            ));
        }
        let mut members = custody.members.lock().await;
        if custody.sealed.load(Ordering::Acquire) {
            return Err(io::Error::other("physical graph sealed"));
        }
        if source.generation != custody.identity.generation {
            return Err(io::Error::other("source belongs to another physical graph"));
        }
        let member = members
            .source_files
            .get_mut(source.index)
            .ok_or_else(|| io::Error::other("physical source unavailable"))?;
        if !member.ready || member.closed || member.failed {
            return Err(io::Error::other("physical source not readable"));
        }
        let kind = OperationKind::Read { offset, length };
        if let Some(pending) = member.pending.as_ref() {
            if pending.kind != kind {
                return Err(io::Error::other("different source operation still owned"));
            }
            return member.complete().await;
        }
        let owned_file = Arc::clone(&member.file);
        budgets::check(
            &self.ledger,
            custody,
            &members,
            budgets::BudgetDelta {
                read_bytes: length as u64,
                ..Default::default()
            },
        )?;
        let runtime = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
        let task = runtime.spawn_blocking(move || {
            let mut owner = owned_file
                .lock()
                .map_err(|_| io::Error::other("source handle owner unavailable"))?;
            let file = owner
                .as_mut()
                .ok_or_else(|| io::Error::other("source handle closed"))?;
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; length];
            let read = file.read(&mut bytes)?;
            bytes.truncate(read);
            Ok(bytes)
        });
        let member = &mut members.source_files[source.index];
        member.pending = Some(PendingOperation { kind, task });
        member.complete().await
    }
}

#[cfg(test)]
mod tests;
