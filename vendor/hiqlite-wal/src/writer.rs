use crate::error::Error;
use crate::lockfile::LockFile;
use crate::log_store_impl::{deserialize, serialize};
use crate::metadata::Metadata;
use crate::reader::LogReadMemo;
use crate::wal::WalFileSet;
use crate::{WalRuntimeState, WalStatusHandle};
use openraft::{LeaderId, LogId};
use std::fmt::{Debug, Formatter};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use thread_priority::ThreadPriority;
use tokio::sync::oneshot;
use tokio::time::Interval;
use tokio::{task, time};
use tracing::{debug, error, warn};

pub enum Action {
    Append {
        rx: flume::Receiver<Option<(u64, Vec<u8>)>>,
        callback: Box<dyn FnOnce() + Send>,
        ack: oneshot::Sender<Result<(), Error>>,
    },
    Remove {
        from: u64,
        until: u64,
        last_log: Option<Vec<u8>>,
        ack: oneshot::Sender<Result<(), Error>>,
    },
    Vote {
        value: Vec<u8>,
        ack: oneshot::Sender<Result<(), Error>>,
    },
    Sync,
    Shutdown(oneshot::Sender<()>),
}

impl Debug for Action {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Append { .. } => write!(f, "Action::Append"),
            Action::Remove { .. } => write!(f, "Action::Remove"),
            Action::Vote { .. } => write!(f, "Action::Vote"),
            Action::Sync => write!(f, "Action::Sync"),
            Action::Shutdown(_) => write!(f, "Action::Shutdown"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LogSync {
    Immediate,
    ImmediateAsync,
    IntervalMillis(u64),
}

impl TryFrom<&str> for LogSync {
    type Error = Error;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "immediate" => Ok(Self::Immediate),
            "immediate_async" => Ok(Self::ImmediateAsync),
            v => {
                if let Some(ms) = v.strip_prefix("interval_") {
                    let Ok(ms) = ms.parse::<u64>() else {
                        return Err(Error::Generic(
                            format!(
                                "Invalid value for log_sync interval, cannot parse as u64: {v}"
                            )
                            .into(),
                        ));
                    };
                    Ok(Self::IntervalMillis(ms))
                } else {
                    Err(Error::Generic(
                        format!("Cannot parse LogSync - invalid value: {v}").into(),
                    ))
                }
            }
        }
    }
}

#[allow(clippy::type_complexity)]
pub fn spawn(
    base_path: String,
    lockfile: LockFile,
    sync: LogSync,
    wal_size: u32,
    wal_deep_integrity_check: bool,
    meta: Arc<RwLock<Metadata>>,
    staged: bool,
) -> Result<
    (
        flume::Sender<Action>,
        Arc<RwLock<WalFileSet>>,
        WalStatusHandle,
    ),
    Error,
> {
    let mut set = WalFileSet::read(base_path, wal_size)?;
    // TODO emit a warning log in that case and tell the user how to resolve or "force start" in
    // that case, or should be maybe `auto-heal` as much as possible?
    let mut buf = Vec::with_capacity(32);
    set.check_integrity(&mut buf, wal_deep_integrity_check)?;
    #[cfg(unix)]
    set.cleanup_reclaim_stage(wal_size)?;
    if set.files.is_empty() {
        buf.clear();
        set.add_file(wal_size, &mut buf)?;
    }
    let wal_locked = Arc::new(RwLock::new(set.clone_no_map()));
    let status = WalStatusHandle::new(
        wal_locked.clone(),
        meta.clone(),
        match &sync {
            LogSync::Immediate => "immediate".to_owned(),
            LogSync::ImmediateAsync => "immediate_async".to_owned(),
            LogSync::IntervalMillis(millis) => format!("interval_{millis}"),
        },
        u64::from(wal_size),
        wal_deep_integrity_check,
    );

    // Restore a missing purge boundary from the first retained entry. Snapshot
    // installation can begin a still-numbered-one WAL above index zero, so the
    // retained index is the evidence of a gap; the WAL file number is not.
    if meta.read()?.last_purged_log_id.is_none()
        && let Some(front) = set.files.front_mut()
        && front.id_from > 2
    {
        warn!(
            "Restoring missing `last_purged_log_id` before retained WAL index {}",
            front.id_from
        );
        let mut buf = Vec::with_capacity(16);
        let mut memo: Option<LogReadMemo> = None;
        front.mmap()?;
        front.read_logs(front.id_from, front.id_until, &mut memo, &mut buf)?;
        let (_, bytes) = buf.first().unwrap();
        let log: openraft::log_id::LogId<u64> = deserialize(bytes)?;
        let log_id: openraft::log_id::LogId<u64> = LogId {
            leader_id: LeaderId {
                term: log.leader_id.term,
                node_id: log.leader_id.node_id,
            },
            index: log.index - 1,
        };
        front.mmap_drop();

        meta.write()?.last_purged_log_id = Some(serialize(&log_id)?);
        Metadata::write(meta.clone(), &set.base_path)?;
    }

    let (tx, rx) = flume::bounded::<Action>(1);
    let wal = wal_locked.clone();
    let snc = sync.clone();
    let writer_status = status.clone();
    thread::spawn(move || {
        let result = run(
            lockfile,
            meta,
            wal,
            set,
            rx,
            snc,
            wal_size,
            writer_status.clone(),
        );
        if let Err(error) = &result {
            writer_status.record_error(error);
        }
        result
    });

    let mut partial = PartialStartupWriter::new(staged, &tx);
    if let LogSync::IntervalMillis(millis) = &sync {
        let interval = time::interval(Duration::from_millis(*millis));
        spawn_syncer(tx.clone(), interval);
    }

    partial.handoff();
    Ok((tx, wal_locked, status))
}

/// Used only inside the owned blocking constructor. Failure cannot abandon
/// the writer while its interval syncer retains a sender.
pub(crate) struct PartialStartupWriter(Option<flume::Sender<Action>>);

impl PartialStartupWriter {
    pub(crate) fn new(staged: bool, writer: &flume::Sender<Action>) -> Self {
        Self(staged.then(|| writer.clone()))
    }

    pub(crate) fn handoff(&mut self) {
        self.0.take();
    }
}

impl Drop for PartialStartupWriter {
    fn drop(&mut self) {
        if let Some(writer) = self.0.take() {
            let (tx, ack) = oneshot::channel();
            if writer.send(Action::Shutdown(tx)).is_err() || ack.blocking_recv().is_err() {
                tracing::error!(
                    "staged partial WAL writer shutdown failed; no clean termination claim"
                );
            }
        }
    }
}

fn spawn_syncer(tx_writer: flume::Sender<Action>, mut interval: Interval) {
    task::spawn(async move {
        loop {
            interval.tick().await;
            if tx_writer.send_async(Action::Sync).await.is_err() {
                debug!("Error sending ActionWrite::Sync to LogStoreWriter - exiting");
                break;
            }
        }
    });
}

/// There are a lot of `unwrap()`s in this task. The reason is simply, if most of these fail, it can
/// only be because of a non-recoverable error anyway and the application should crash, so that
/// the next health check can restart it.
///
/// Everything related to locking and memory mapping is being `unwrap()`ped. If anything fails in
/// this regard, it's either a physical storage or OS issue and this code an do nothing about it.
fn run(
    lockfile: LockFile,
    meta: Arc<RwLock<Metadata>>,
    wal_locked: Arc<RwLock<WalFileSet>>,
    mut wal: WalFileSet,
    rx: flume::Receiver<Action>,
    sync: LogSync,
    wal_size: u32,
    status: WalStatusHandle,
) -> Result<(), Error> {
    let _ = ThreadPriority::Max.set_for_current();

    let mut is_dirty = false;
    let mut shutdown_ack: Option<oneshot::Sender<()>> = None;
    let data_len_limit = wal_size as usize - wal.active().offset_logs() - 2;

    // openraft will read chunks of 64 logs for bigger tasks
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut buf_logs: Vec<(u64, Vec<u8>)> = Vec::with_capacity(1);

    wal.active().mmap_mut()?;

    while let Ok(action) = rx.recv() {
        match action {
            Action::Append { rx, callback, ack } => {
                debug!("WAL Writer - Action::Append");

                let mut res = Ok(());
                {
                    let mut active = wal.active();
                    // A failed removal may still have changed the physical append head.
                    // Reopen it rather than using a missing/stale writable mapping.
                    active.mmap_mut()?;
                    while let Ok(Some((id, bytes))) = rx.recv() {
                        if bytes.len() > data_len_limit {
                            panic!(
                                "`data` length must not exceed `wal_size` -> data length is {} \
                            vs wal_size (without header) is {data_len_limit}",
                                bytes.len(),
                            );
                        }

                        if !active.has_space(bytes.len() as u32) {
                            buf.clear();
                            let rolled = {
                                let mut lock = wal_locked.write().unwrap();
                                let result = wal.roll_over(wal_size, &mut buf);
                                lock.refresh_from_no_mmap(&wal);
                                result
                            };
                            rolled?;
                            active = wal.active();
                        }

                        buf.clear();
                        if let Err(err) = active.append_log(id, &bytes, &mut buf) {
                            res = Err(err);
                            break;
                        }
                        debug_assert_eq!(
                            active.id_until, id,
                            "active.id_until and id don't match: {} != {id}",
                            active.id_until
                        );
                    }
                }

                {
                    let mut lock = wal_locked.write().unwrap();
                    debug_assert_eq!(lock.active, wal.active);
                    lock.active().clone_from_no_mmap(wal.active());
                }

                if let Err(err) = ack.send(res) {
                    // this should usually not happen, but it may during an incorrect shutdown
                    error!("error sending back ack after logs append: {err:?}");
                }

                if sync == LogSync::Immediate {
                    status.set_state(WalRuntimeState::Syncing);
                    wal.active().flush()?;
                    status.record_sync(Some(wal.active().id_until));
                } else if sync == LogSync::ImmediateAsync {
                    status.set_state(WalRuntimeState::Syncing);
                    wal.active().flush_async()?;
                    status.record_sync(Some(wal.active().id_until));
                } else {
                    is_dirty = true;
                }
                // TODO with the next big openraft release, we can do async callbacks
                callback();

                // Roll WAL pre-emptively if only very few space is left at this point, because
                // if we just wrote some chunks, me probably have a very short break now until the
                // next request comes in.
                //
                // TODO fixed 4kB -> make configurable?
                if wal.active().space_left() < 4 * 1024 {
                    buf.clear();
                    let rolled = {
                        let mut lock = wal_locked.write().unwrap();
                        let result = wal.roll_over(wal_size, &mut buf);
                        lock.refresh_from_no_mmap(&wal);
                        result
                    };
                    rolled?;
                }
            }
            Action::Remove {
                from,
                until,
                last_log,
                ack,
            } => {
                status.set_state(WalRuntimeState::Compacting);
                debug!(
                    "WAL Writer - Action::Remove from {from} until {until} / \
                    last_log: {last_log:?}\n{wal:?}"
                );

                // Before removing any logs, make sure that all in-memory buffers are flushed. If
                // at least headers and metadata are not up to date, and a crash happens in the
                // middle of removing logs, we could end up with a hole between Snapshot and latest
                // existing Raft Log, which must never happen.
                let active = wal.active();
                if is_dirty {
                    buf.clear();
                    active.update_header(&mut buf)?;
                    // async is fine, as long as we trigger it before starting log removal.
                    // If the flush fails, so would the log removal, and we would not have a hole.
                    active.flush_async()?;
                    is_dirty = false;
                    status.record_sync(Some(active.id_until));
                }

                buf.clear();
                buf_logs.clear();
                let result = {
                    // Take the exclusive layout guard before deleting,
                    // recreating, or truncating any WAL path. Readers retain
                    // the shared guard from refresh through mmap/read, so an
                    // old incarnation can never open a replacement pathname.
                    let mut layout = wal_locked.write().unwrap();
                    let shifted =
                        wal.shift_delete_logs(from, until, wal_size, &mut buf, &mut buf_logs);
                    // Deletion or atomic replacement may have succeeded before a later
                    // durability error. Publish the actual layout on every result, and
                    // before metadata publication can fail.
                    wal.active = wal.files.len().checked_sub(1);
                    layout.refresh_from_no_mmap(&wal);
                    shifted.and_then(|()| {
                        // the last_log may be none if logs are truncated
                        if last_log.is_some() {
                            meta.write()?.last_purged_log_id = last_log;
                            Metadata::write(meta.clone(), &wal.base_path)?;
                        }
                        Ok(())
                    })
                };
                match &result {
                    Ok(()) => status.record_compaction(Some(wal.active().id_until)),
                    Err(error) => status.record_error(error),
                }
                if ack.send(result).is_err() {
                    debug!("WAL remove response receiver closed before completion");
                }
                if !wal.writable_head_ready(wal_size) {
                    return Err(Error::Integrity(
                        "WAL removal left no complete writable append target".into(),
                    ));
                }
            }
            Action::Vote { value, ack } => {
                debug!("WAL Writer - Action::Vote");

                buf.clear();
                status.set_state(WalRuntimeState::Syncing);
                wal.active().flush_async()?;
                is_dirty = false;
                status.record_sync(Some(wal.active().id_until));

                meta.write()?.vote = Some(value);
                let res = Metadata::write(meta.clone(), &wal.base_path);

                ack.send(res).unwrap();
            }
            Action::Sync => {
                if is_dirty {
                    let active = wal.active();
                    buf.clear();
                    status.set_state(WalRuntimeState::Syncing);
                    active.update_header(&mut buf)?;
                    active.flush_async()?;
                    is_dirty = false;
                    status.record_sync(Some(active.id_until));
                }
            }
            Action::Shutdown(ack) => {
                status.set_state(WalRuntimeState::Stopping);
                debug!("Raft logs store writer is being shut down");
                shutdown_ack = Some(ack);
                break;
            }
        }
    }

    debug!("Logs Writer exiting");

    let durable_index = {
        let active = wal.active();
        buf.clear();
        active.update_header(&mut buf)?;
        active.flush()?;
        active.id_until
    };
    Metadata::write(meta, &wal.base_path)?;

    // drop the lockfile before trying to remove it to unlock it
    drop(lockfile);
    LockFile::remove(&wal.base_path).expect("LockFile removal failed");
    status.record_stopped(Some(durable_index));

    if let Some(ack) = shutdown_ack {
        ack.send(())
            .expect("Shutdown handler to always wait for ack from logs");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const WAL_SIZE: u32 = 2 * 1024 * 1024;
    type TestWriter = (
        flume::Sender<Action>,
        Arc<RwLock<Metadata>>,
        Arc<RwLock<WalFileSet>>,
    );

    fn test_path(name: &str) -> String {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("test_data/{name}-{}-{nonce}", std::process::id())
    }

    fn start_writer(base_path: &str) -> Result<TestWriter, Error> {
        fs::create_dir_all(base_path)?;
        let lockfile = LockFile::create(base_path)?;
        lockfile.lock()?;
        let meta = Arc::new(RwLock::new(Metadata::read_or_create(base_path)?));
        let (writer, wal, _) = spawn(
            base_path.to_owned(),
            lockfile,
            LogSync::Immediate,
            WAL_SIZE,
            false,
            meta.clone(),
            false,
        )?;
        Ok((writer, meta, wal))
    }

    fn stop_writer(writer: flume::Sender<Action>) {
        let (ack, rx) = oneshot::channel();
        writer.send(Action::Shutdown(ack)).unwrap();
        rx.blocking_recv().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn both_append_rollover_triggers_reclaim_a_previously_purged_active_front() -> Result<(), Error>
    {
        for (name, payload_len, first_last, next_id) in [
            ("space-forced", WAL_SIZE as usize / 4, 3, 4),
            ("preemptive", WAL_SIZE as usize / 3 - 1000, 2, 3),
        ] {
            let path = test_path(name);
            let (writer, _meta, layout) = start_writer(&path)?;
            let append = |from, until| -> Result<(), Error> {
                let (entries, receive) = flume::bounded(2);
                let (ack, acknowledged) = oneshot::channel();
                writer
                    .send(Action::Append {
                        rx: receive,
                        callback: Box::new(|| {}),
                        ack,
                    })
                    .unwrap();
                for id in from..=until {
                    entries
                        .send(Some((id, vec![id as u8; payload_len])))
                        .unwrap();
                }
                entries.send(None).unwrap();
                acknowledged.blocking_recv().unwrap()
            };
            append(1, first_last)?;
            let mut reader = layout.read().unwrap().clone_no_map();
            assert_eq!(reader.files.len(), 1);
            reader.active().mmap()?;
            let mut memo = None;
            reader
                .active()
                .read_logs(1, 1, &mut memo, &mut Vec::new())?;
            let (ack, acknowledged) = oneshot::channel();
            writer
                .send(Action::Remove {
                    from: 0,
                    until: 2,
                    last_log: Some(serialize(&LogId {
                        leader_id: LeaderId {
                            term: 1,
                            node_id: 1_u64,
                        },
                        index: 2,
                    })?),
                    ack,
                })
                .unwrap();
            acknowledged.blocking_recv().unwrap()?;
            {
                let current = layout.read().unwrap();
                assert_eq!(current.files.len(), 1);
                assert_eq!(
                    fs::metadata(&current.files[0].path)?.len(),
                    u64::from(WAL_SIZE)
                );
            }
            append(next_id, next_id)?;
            // Append acknowledgement precedes the preemptive rollover. This
            // non-purge action is a queue barrier, not another reclamation trigger.
            let (ack, acknowledged) = oneshot::channel();
            writer
                .send(Action::Vote {
                    value: vec![1],
                    ack,
                })
                .unwrap();
            acknowledged.blocking_recv().unwrap()?;
            {
                let current = layout.read().unwrap();
                assert_eq!(current.files.len(), 2);
                assert!(fs::metadata(&current.files[0].path)?.len() < u64::from(WAL_SIZE));
                assert_eq!(
                    fs::metadata(&current.files[1].path)?.len(),
                    u64::from(WAL_SIZE)
                );
                assert_eq!(current.files[0].data_start, Some(32));
                reader.refresh_from_no_mmap(&current);
            }
            // Observe invalidation through the public read operation instead
            // of reaching into WalFile's private mapping field.
            assert!(matches!(
                reader.files[0].read_logs(2, 2, &mut None, &mut Vec::new()),
                Err(Error::Generic(message)) if message.as_ref() == "No mmap exists"
            ));
            reader.files[0].mmap()?;
            let last_front = if name == "space-forced" {
                first_last
            } else {
                next_id
            };
            let mut records = Vec::new();
            reader.files[0].read_logs(2, last_front, &mut memo, &mut records)?;
            let expected: Vec<_> = (2..=last_front)
                .map(|id| (id, vec![id as u8; payload_len]))
                .collect();
            assert_eq!(records, expected);
            stop_writer(writer);
            drop(reader);
            drop(layout);
            let mut reopened = WalFileSet::read(path.clone(), WAL_SIZE)?;
            reopened.files[0].mmap()?;
            records.clear();
            reopened.files[0].read_logs(2, last_front, &mut None, &mut records)?;
            assert_eq!(records, expected);
            drop(reopened);
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn remove_metadata_error_still_refreshes_reclaimed_reader_layout() -> Result<(), Error> {
        let path = test_path("reclaim-metadata-error");
        let (writer, _meta, layout) = start_writer(&path)?;
        let (entries, receive) = flume::bounded(2);
        let (ack, acknowledged) = oneshot::channel();
        writer
            .send(Action::Append {
                rx: receive,
                callback: Box::new(|| {}),
                ack,
            })
            .unwrap();
        for id in 1..=4 {
            entries
                .send(Some((id, vec![id as u8; WAL_SIZE as usize / 3])))
                .unwrap();
        }
        entries.send(None).unwrap();
        acknowledged.blocking_recv().unwrap()?;
        let mut reader = layout.read().unwrap().clone_no_map();
        reader.files[0].mmap()?;
        let mut memo = None;
        reader.files[0].read_logs(1, 1, &mut memo, &mut Vec::new())?;
        // A deterministic owned fixture blocks only metadata staging, after the
        // real remove/reclaim has published its new canonical file.
        fs::create_dir(format!("{path}/meta.hql.tmp"))?;
        let (ack, acknowledged) = oneshot::channel();
        writer
            .send(Action::Remove {
                from: 0,
                until: 2,
                last_log: Some(serialize(&LogId {
                    leader_id: LeaderId {
                        term: 1,
                        node_id: 1_u64,
                    },
                    index: 2,
                })?),
                ack,
            })
            .unwrap();
        assert!(acknowledged.blocking_recv().unwrap().is_err());
        {
            let current = layout.read().unwrap();
            assert_eq!(current.files[0].id_from, 2);
            assert!(fs::metadata(&current.files[0].path)?.len() < u64::from(WAL_SIZE));
            reader.refresh_from_no_mmap(&current);
        }
        reader.files[0].mmap()?;
        let mut retained = Vec::new();
        reader.files[0].read_logs(2, 2, &mut memo, &mut retained)?;
        assert_eq!(retained, vec![(2, vec![2; WAL_SIZE as usize / 3])]);
        fs::remove_dir(format!("{path}/meta.hql.tmp"))?;
        stop_writer(writer);
        drop(reader);
        drop(layout);
        let mut reopened = WalFileSet::read(path.clone(), WAL_SIZE)?;
        reopened.files[0].mmap()?;
        retained.clear();
        reopened.files[0].read_logs(2, 2, &mut None, &mut retained)?;
        assert_eq!(retained, vec![(2, vec![2; WAL_SIZE as usize / 3])]);
        drop(reopened);
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn single_file_snapshot_tail_restores_its_missing_purge_boundary() -> Result<(), Error> {
        let base_path = test_path("single-file-snapshot-tail");
        let _ = fs::remove_dir_all(&base_path);

        let (writer, meta, _wal) = start_writer(&base_path)?;
        let first_retained = LogId {
            leader_id: LeaderId {
                term: 1,
                node_id: 1_u64,
            },
            index: 10_000,
        };
        let (entry_tx, entry_rx) = flume::bounded(2);
        let (append_ack, append_rx) = oneshot::channel();
        writer
            .send(Action::Append {
                rx: entry_rx,
                callback: Box::new(|| {}),
                ack: append_ack,
            })
            .unwrap();
        entry_tx
            .send(Some((first_retained.index, serialize(&first_retained)?)))
            .unwrap();
        entry_tx.send(None).unwrap();
        append_rx.blocking_recv().unwrap()?;
        assert!(meta.read().unwrap().last_purged_log_id.is_none());
        stop_writer(writer);

        let (writer, meta, _wal) = start_writer(&base_path)?;
        let bytes = meta
            .read()
            .unwrap()
            .last_purged_log_id
            .clone()
            .expect("the retained index must restore the purge boundary");
        let restored: LogId<u64> = deserialize(&bytes)?;
        assert_eq!(restored.leader_id, first_retained.leader_id);
        assert_eq!(restored.index, 9_999);
        stop_writer(writer);

        fs::remove_dir_all(base_path)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn full_purge_waits_for_an_unmapped_reader_before_reusing_the_wal_path() -> Result<(), Error> {
        let base_path = test_path("full-purge-reader-layout-guard");
        let _ = fs::remove_dir_all(&base_path);

        let (writer, meta, wal_locked) = start_writer(&base_path)?;
        let first = LogId {
            leader_id: LeaderId {
                term: 1,
                node_id: 1_u64,
            },
            index: 1,
        };
        let (entry_tx, entry_rx) = flume::bounded(2);
        let (append_ack, append_rx) = oneshot::channel();
        writer
            .send(Action::Append {
                rx: entry_rx,
                callback: Box::new(|| {}),
                ack: append_ack,
            })
            .unwrap();
        entry_tx
            .send(Some((first.index, serialize(&first)?)))
            .unwrap();
        entry_tx.send(None).unwrap();
        append_rx.blocking_recv().unwrap()?;

        let layout = wal_locked.read().unwrap();
        let mut reader = layout.clone_no_map();
        let (remove_ack, mut remove_rx) = oneshot::channel();
        writer
            .send(Action::Remove {
                from: 0,
                until: first.index,
                last_log: Some(serialize(&first)?),
                ack: remove_ack,
            })
            .unwrap();

        // Fill the writer queue behind Remove. Success proves the writer has
        // received Remove and is waiting on the layout guard held above.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let mut queued = Action::Sync;
        loop {
            match writer.try_send(queued) {
                Ok(()) => break,
                Err(flume::TrySendError::Full(action)) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "writer did not begin the remove action"
                    );
                    queued = action;
                    thread::yield_now();
                }
                Err(flume::TrySendError::Disconnected(_)) => {
                    panic!("writer disconnected during remove")
                }
            }
        }
        assert!(matches!(
            remove_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        // The old identity has no mmap yet. If Remove unlinked/recreated the
        // pathname before taking its exclusive guard, this mmap would attach
        // the replacement inode to the old metadata and the read would fail.
        reader.active().mmap()?;
        let mut memo = None;
        let mut records = Vec::with_capacity(1);
        reader.active().read_logs(1, 1, &mut memo, &mut records)?;
        assert_eq!(records.len(), 1);

        drop(layout);
        remove_rx.blocking_recv().unwrap()?;

        let replacement = LogId {
            leader_id: first.leader_id,
            index: 10_001,
        };
        let (entry_tx, entry_rx) = flume::bounded(2);
        let (append_ack, append_rx) = oneshot::channel();
        writer
            .send(Action::Append {
                rx: entry_rx,
                callback: Box::new(|| {}),
                ack: append_ack,
            })
            .unwrap();
        entry_tx
            .send(Some((replacement.index, serialize(&replacement)?)))
            .unwrap();
        entry_tx.send(None).unwrap();
        append_rx.blocking_recv().unwrap()?;

        {
            let layout = wal_locked.read().unwrap();
            reader.refresh_from_no_mmap(&layout);
            reader.active().mmap()?;
            records.clear();
            reader.active().read_logs(
                replacement.index,
                replacement.index,
                &mut memo,
                &mut records,
            )?;
        }
        assert_eq!(records.len(), 1);

        stop_writer(writer);
        drop(reader);
        drop(wal_locked);
        drop(meta);
        fs::remove_dir_all(base_path)?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn status_distinguishes_append_from_durable_sync_and_clean_stop() -> Result<(), Error> {
        let base_path = test_path("wal-status-durable-boundary");
        let _ = fs::remove_dir_all(&base_path);
        fs::create_dir_all(&base_path)?;
        let lockfile = LockFile::create(&base_path)?;
        lockfile.lock()?;
        let meta = Arc::new(RwLock::new(Metadata::read_or_create(&base_path)?));
        let (writer, _wal, status) = spawn(
            base_path.clone(),
            lockfile,
            LogSync::IntervalMillis(60_000),
            WAL_SIZE,
            false,
            meta,
            false,
        )?;
        // The interval's first tick is immediate. Let that empty sync pass so
        // the assertion below measures this append rather than scheduler order.
        tokio::time::sleep(Duration::from_millis(10)).await;

        let retained = LogId {
            leader_id: LeaderId {
                term: 9,
                node_id: 2_u64,
            },
            index: 42,
        };
        let (entry_tx, entry_rx) = flume::bounded(2);
        let (append_ack, append_rx) = oneshot::channel();
        writer
            .send(Action::Append {
                rx: entry_rx,
                callback: Box::new(|| {}),
                ack: append_ack,
            })
            .unwrap();
        entry_tx
            .send(Some((retained.index, serialize(&retained)?)))
            .unwrap();
        entry_tx.send(None).unwrap();
        append_rx.await.unwrap()?;

        let appended = status.snapshot();
        assert_eq!(appended.last_log_index, Some(42));
        assert_eq!(appended.last_durable_index, None);
        assert_eq!(appended.state, WalRuntimeState::Open);
        assert!(appended.lock_owned);

        writer.send(Action::Sync).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if status.snapshot().last_durable_index == Some(42) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "explicit sync did not publish its durable boundary"
            );
            thread::yield_now();
        }

        let (shutdown_ack, shutdown_rx) = oneshot::channel();
        writer.send(Action::Shutdown(shutdown_ack)).unwrap();
        shutdown_rx.await.unwrap();
        let stopped = status.snapshot();
        assert_eq!(stopped.state, WalRuntimeState::Stopped);
        assert!(!stopped.lock_owned);
        assert_eq!(stopped.last_durable_index, Some(42));
        fs::remove_dir_all(base_path)?;
        Ok(())
    }

    #[test]
    fn status_records_unclean_start_and_bounds_writer_errors() -> Result<(), Error> {
        let base_path = test_path("wal-status-unclean-error");
        let _ = fs::remove_dir_all(&base_path);
        fs::create_dir_all(&base_path)?;
        let lockfile = LockFile::create(&base_path)?;
        lockfile.lock()?;
        let meta = Arc::new(RwLock::new(Metadata::read_or_create(&base_path)?));
        let (writer, _wal, status) = spawn(
            base_path.clone(),
            lockfile,
            LogSync::Immediate,
            WAL_SIZE,
            true,
            meta,
            false,
        )?;

        let startup = status.snapshot();
        assert!(startup.unclean_start_observed);
        assert_eq!(
            startup
                .last_recovery
                .as_ref()
                .map(|value| value.operation.as_str()),
            Some("startup_integrity_check")
        );

        status.set_state(WalRuntimeState::Compacting);
        assert_eq!(status.snapshot().state, WalRuntimeState::Compacting);
        status.record_compaction(startup.last_durable_index);
        let compacted = status.snapshot();
        assert_eq!(compacted.state, WalRuntimeState::Open);
        assert!(compacted.last_compaction_unix_ms.is_some());

        // The writer's outer error boundary calls the same recorder for a
        // failed flush/sync. Pin the operator-visible transition separately
        // from the bounded-message assertion below.
        status.set_state(WalRuntimeState::Syncing);
        status.record_error(&"injected sync failure");
        let sync_failed = status.snapshot();
        assert_eq!(sync_failed.state, WalRuntimeState::Error);
        assert_eq!(
            sync_failed
                .last_error
                .as_ref()
                .map(|value| value.message.as_str()),
            Some("injected sync failure")
        );

        status.record_error(&"x".repeat(2_048));
        let failed = status.snapshot();
        assert_eq!(failed.state, WalRuntimeState::Error);
        assert_eq!(
            failed.last_error.as_ref().unwrap().message.chars().count(),
            512
        );

        stop_writer(writer);
        fs::remove_dir_all(base_path)?;
        Ok(())
    }
}
