//! How an idle reader of replicated settings learns that they may have
//! changed on another node, without a consistent read per look.
//!
//! A reader takes one consistent read to prove a value, records the applied
//! Raft index that read covered, and holds the value. While this replica's
//! applied index has not moved, no write anywhere can have changed it. Once
//! the index moves, the reader looks at the setting in its own replica (a
//! hint, no consensus) and owes a consistent read only when the hint
//! disagrees with what it holds. A save on this node reaches its reader
//! through that reader's own local signal.

use std::future::Future;

use plurx_core::error::StoreError;

/// What tells a parked settings observer that the replicated settings may
/// have changed on another node. Each reader holds its own clone: a watch
/// marks what its holder has seen, so readers never consume each other's
/// observations.
#[derive(Clone)]
pub(crate) enum SettingsChangeSource {
    /// Nothing but this process writes the settings (a single-node store), and
    /// its saves reach their readers themselves. No other process writes
    /// settings into a running daemon's SQLite database: `restore` runs only
    /// while the daemon is stopped, `reset-password` refuses, and
    /// `refresh-metadata` writes no settings.
    LocalOnly,
    /// A replicated store whose Raft log is applied in another process: no
    /// change signal reaches this one, so the observer reads every interval.
    Polled,
    /// This node's in-process Raft watch: a write anywhere in the cluster
    /// moves the applied index once this replica applies it.
    Applied(hiqlite::LocalDbRaftMetrics),
    /// An applied index a test moves by hand.
    #[cfg(test)]
    Test(tokio::sync::watch::Receiver<u64>),
}

impl SettingsChangeSource {
    pub(crate) fn applied_index(&self) -> Option<u64> {
        match self {
            Self::LocalOnly | Self::Polled => None,
            Self::Applied(metrics) => metrics.snapshot().last_applied_index,
            #[cfg(test)]
            Self::Test(applied) => Some(*applied.borrow()),
        }
    }

    /// Whether a change made elsewhere would be seen at all. A stopped Raft
    /// node or a closed watch reports no later change, so the applied index
    /// it last showed proves nothing.
    pub(crate) fn live(&self) -> bool {
        match self {
            Self::LocalOnly => true,
            Self::Polled => false,
            Self::Applied(metrics) => metrics.membership_snapshot().running,
            #[cfg(test)]
            Self::Test(applied) => applied.has_changed().is_ok(),
        }
    }

    /// The next observation; `false` once the source can report none.
    pub(crate) async fn changed(&mut self) -> bool {
        match self {
            Self::LocalOnly | Self::Polled => std::future::pending().await,
            Self::Applied(metrics) => metrics.wait_for_change().await,
            #[cfg(test)]
            Self::Test(applied) => applied.changed().await.is_ok(),
        }
    }

    /// Wait until this replica's applied index differs from the one `held`
    /// covers, or until the source can report nothing more. The Raft watch
    /// wakes on any metrics change (a heartbeat's leader lease, a term, a
    /// membership snapshot); one that leaves the applied index where it was
    /// cannot have changed a setting, so it does not end the wait. With
    /// nothing held there is no index to move past, and this never returns.
    pub(crate) async fn applied_past<T>(&mut self, held: Option<&Proved<T>>) {
        let Some(held) = held else {
            return std::future::pending().await;
        };
        while self.applied_index() == held.applied {
            if !self.changed().await {
                return;
            }
        }
    }

    /// Mark a value as proved by a consistent read that starts now. The mark
    /// is taken before the read because the read covers every entry applied
    /// before it; an entry applied during the read moves the index past the
    /// mark and costs one hint look, never a missed change.
    pub(crate) fn mark(&self) -> ProofMark {
        ProofMark(self.applied_index())
    }

    /// Whether `held` still stands without a consistent read.
    ///
    /// An unmoved applied index costs nothing: no entry has reached this
    /// replica since the proof. A moved index costs one read of this node's
    /// own replica through `hint`; agreement carries the proof forward to the
    /// new index. A disagreement, an unreadable replica, or a source that
    /// would not report a change made elsewhere all answer `false`, and the
    /// caller takes its consistent read exactly as it would with nothing held.
    ///
    /// A follower behind the consistent read can disagree with it for a
    /// while; that costs a re-read, never a stale answer. It cannot agree
    /// with a value it has not applied yet without its index moving again
    /// once it does.
    pub(crate) async fn confirms<T, F, Fut>(&self, held: &mut Proved<T>, hint: F) -> bool
    where
        T: PartialEq,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, StoreError>>,
    {
        if !self.live() {
            return false;
        }
        let applied = self.applied_index();
        if applied == held.applied {
            return true;
        }
        match hint().await {
            Ok(value) if value == held.value => {
                held.applied = applied;
                true
            }
            Ok(_) => false,
            Err(error) => {
                tracing::debug!(%error, "replicated setting hint could not be read");
                false
            }
        }
    }
}

/// The applied index a consistent read started at; see
/// [`SettingsChangeSource::mark`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProofMark(Option<u64>);

impl ProofMark {
    /// The value the read returned, held as of this mark.
    pub(crate) fn proved<T>(self, value: T) -> Proved<T> {
        Proved {
            value,
            applied: self.0,
        }
    }
}

/// A value one consistent read proved, and the applied index it covered.
#[derive(Debug)]
pub(crate) struct Proved<T> {
    pub(crate) value: T,
    applied: Option<u64>,
}
