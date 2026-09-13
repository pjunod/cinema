//! Telling something outside plurx that a reminder fired or a recording
//! started, finished or failed.
//!
//! Best effort, and structured so that it cannot be anything else. The loops
//! that produce these events are the same loops that start and stop captures,
//! and a dead endpoint must never delay either: three five-second attempts
//! inside a fifteen-second tick would stall recording control for as long as
//! the endpoint stayed dead. So the loops only ever *enqueue*, a single worker
//! drains the queue, and a full queue drops its oldest event with a warning
//! rather than growing without bound or blocking its producer.
//!
//! A missed webhook is a log line. It is not a queue on disk, not a retry
//! schedule, and not a reason for a recording to be late.

use std::sync::Arc;

use plurx_core::dvr::DVR_WEBHOOK_QUEUE;
use tokio_util::sync::CancellationToken;

use super::{approved_webhook_url, DvrConfig, LiveTvManager};

const WEBHOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const WEBHOOK_ATTEMPTS: usize = 3;

/// What happened, in the words the receiving automation needs.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum DvrEvent {
    Reminder {
        reminder_id: String,
        user_id: i64,
        channel_id: String,
        guide_number: String,
        title: String,
        airing_start: i64,
        lead_s: i64,
    },
    RecordingStarted {
        recording_id: String,
        channel_id: String,
        guide_number: String,
        title: String,
        airing_start: i64,
        capture_end: i64,
    },
    RecordingFinished {
        recording_id: String,
        title: String,
        state: String,
        bytes: i64,
        gap_s: i64,
        path: Option<String>,
    },
    RecordingFailed {
        recording_id: String,
        title: String,
        reason: String,
    },
}

impl DvrEvent {
    fn name(&self) -> &'static str {
        match self {
            Self::Reminder { .. } => "reminder",
            Self::RecordingStarted { .. } => "recording_started",
            Self::RecordingFinished { .. } => "recording_finished",
            Self::RecordingFailed { .. } => "recording_failed",
        }
    }
}

/// The producer half. Cloned into the loops; never awaits the network.
///
/// Backed by a queue that drops its **oldest** entry when full. A recording
/// that just started is the event someone is waiting on; the reminder from an
/// hour ago that nobody could deliver is not. `tokio`'s bounded channel drops
/// the incoming message instead, which is the wrong end, so the queue is a
/// small deque behind a mutex with a notifier — sixty-four events is not a
/// structure worth optimising.
#[derive(Clone)]
pub(crate) struct DvrEventSink {
    queue: Arc<DvrEventQueue>,
}

pub(crate) struct DvrEventQueue {
    events: std::sync::Mutex<std::collections::VecDeque<DvrEvent>>,
    arrived: tokio::sync::Notify,
}

impl DvrEventQueue {
    pub(crate) fn take(&self) -> Option<DvrEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }
}

impl DvrEventSink {
    /// Enqueue, or drop the event and say so.
    ///
    /// `try_send` rather than `send`, deliberately: an `await` here would make
    /// a slow endpoint into a slow recording tick, which is the exact coupling
    /// this whole module exists to prevent.
    pub(crate) fn enqueue(&self, event: DvrEvent) {
        {
            let mut events = self
                .queue
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while events.len() >= DVR_WEBHOOK_QUEUE {
                if let Some(dropped) = events.pop_front() {
                    tracing::warn!(
                        event = dropped.name(),
                        "the DVR webhook queue is full; the oldest event was dropped"
                    );
                }
            }
            events.push_back(event);
        }
        self.queue.arrived.notify_one();
    }
}

/// Build the queue. The sink goes to the loops, the worker runs on its own
/// task and owns every network call the DVR makes.
pub(crate) fn channel() -> (DvrEventSink, Arc<DvrEventQueue>) {
    let queue = Arc::new(DvrEventQueue {
        events: std::sync::Mutex::new(std::collections::VecDeque::new()),
        arrived: tokio::sync::Notify::new(),
    });
    (
        DvrEventSink {
            queue: Arc::clone(&queue),
        },
        queue,
    )
}

pub(crate) async fn webhook_worker(
    manager: Arc<LiveTvManager>,
    events: Arc<DvrEventQueue>,
    shutdown: CancellationToken,
) {
    // A client of its own, with redirects refused: the URL an operator
    // approved is the host this talks to, and a redirect is how an approved
    // host sends a request somewhere that was never approved.
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(WEBHOOK_TIMEOUT)
        .user_agent(concat!("plurx/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "the DVR webhook client could not be built; webhooks are off");
            return;
        }
    };
    loop {
        let event = match events.take() {
            Some(event) => event,
            None => {
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = events.arrived.notified() => continue,
                }
            }
        };
        let Ok((_, dvr)) = manager.dvr_configs().await else {
            continue;
        };
        deliver(&client, &dvr, &event).await;
    }
}

async fn deliver(client: &reqwest::Client, dvr: &DvrConfig, event: &DvrEvent) {
    if dvr.webhook_url.is_empty() {
        return;
    }
    // Re-checked at send time, not only when the setting was saved: the
    // policy is what decides whether this request may go out at all, and a
    // setting written by an older build must not bypass it.
    let url = match approved_webhook_url(&dvr.webhook_url) {
        Ok(url) => url,
        Err(reason) => {
            tracing::warn!(
                reason,
                "the configured DVR webhook URL is not one this server calls"
            );
            return;
        }
    };
    for attempt in 1..=WEBHOOK_ATTEMPTS {
        match client.post(url.clone()).json(event).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) => {
                if attempt == WEBHOOK_ATTEMPTS {
                    tracing::warn!(
                        event = event.name(),
                        status = response.status().as_u16(),
                        "the DVR webhook endpoint refused this event"
                    );
                }
            }
            Err(_) => {
                if attempt == WEBHOOK_ATTEMPTS {
                    tracing::warn!(
                        event = event.name(),
                        "the DVR webhook endpoint could not be reached"
                    );
                }
            }
        }
    }
}
