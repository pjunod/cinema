//! Fixed writer-to-owner mailbox for durable finite output events.
use super::SoftwareVodEvent;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::{mpsc::error::TryRecvError, Notify};

struct Slot {
    event: Option<SoftwareVodEvent>,
    sender_live: bool,
    receiver_live: bool,
}

pub(super) struct SoftwareVodMailbox {
    slot: Mutex<Slot>,
    readable: Notify,
    writable: Notify,
}

pub(super) struct EventSender(Arc<SoftwareVodMailbox>);

/// One reader of the original finite writer's publication metadata.
/// Channel state never authorizes native cleanup or a resource refund.
pub struct SoftwareVodEvents(Arc<SoftwareVodMailbox>);

pub(super) fn channel() -> (EventSender, SoftwareVodEvents) {
    let mailbox = Arc::new(SoftwareVodMailbox {
        slot: Mutex::new(Slot {
            event: None,
            sender_live: true,
            receiver_live: true,
        }),
        readable: Notify::new(),
        writable: Notify::new(),
    });
    (
        EventSender(Arc::clone(&mailbox)),
        SoftwareVodEvents(mailbox),
    )
}

impl EventSender {
    pub(super) async fn send(&self, event: SoftwareVodEvent) -> Result<(), ()> {
        loop {
            {
                let mut slot = self.0.slot.lock().map_err(|_| ())?;
                if !slot.receiver_live {
                    return Err(());
                }
                if slot.event.is_none() {
                    slot.event = Some(event);
                    self.0.readable.notify_one();
                    return Ok(());
                }
            }
            // Single writer and reader. notify_one retains the permit
            // if the receiver emptied the slot before this await.
            self.0.writable.notified().await;
        }
    }
}

impl Drop for EventSender {
    fn drop(&mut self) {
        self.0
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sender_live = false;
        self.0.readable.notify_one();
    }
}

impl SoftwareVodEvents {
    /// Cancel-safe wait. Only taking an event under the original slot lock
    /// consumes it, and no yield separates that take from returning it.
    pub async fn recv(&mut self) -> Option<SoftwareVodEvent> {
        loop {
            match self.try_recv() {
                Ok(event) => return Some(event),
                Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => self.0.readable.notified().await,
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<SoftwareVodEvent, TryRecvError> {
        let mut slot = self.0.slot.lock().map_err(|_| TryRecvError::Disconnected)?;
        match slot.event.take() {
            Some(event) => {
                self.0.writable.notify_one();
                Ok(event)
            }
            None if slot.sender_live => Err(TryRecvError::Empty),
            None => Err(TryRecvError::Disconnected),
        }
    }
}

impl Drop for SoftwareVodEvents {
    fn drop(&mut self) {
        let mut slot = self.0.slot.lock().unwrap_or_else(PoisonError::into_inner);
        slot.receiver_live = false;
        let pending = slot.event.take();
        self.0.writable.notify_one();
        drop(slot);
        drop(pending);
    }
}
