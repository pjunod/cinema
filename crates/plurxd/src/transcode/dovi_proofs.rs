//! The per-source Dolby Vision Profile 5 pixel proofs this node has made.
//!
//! A proof is two sequential ffmpeg probes of up to 30 seconds each
//! ([`crate::ffmpeg::dovi_reshape_changes_pixels`]). Only a verdict —
//! [`DoviPixelProof::Proved`] or [`DoviPixelProof::Refuted`] — is remembered,
//! keyed by the source's `path:size:mtime`. An inconclusive proof (a timeout,
//! a spawn failure, a non-zero exit) is handed to every caller waiting on it
//! and then forgotten, so the next start measures again instead of inheriting
//! "not Dolby Vision" for the life of the process.
//!
//! Concurrent misses for one key join a single flight: the first caller spawns
//! the proof, every other caller waits on the same result. The proof runs in
//! its own task, so a start that gives up waiting neither cancels the probe
//! for the callers still waiting nor leaves a half-finished flight behind.
//!
//! A flight runs its probes at the class of the caller that opened it, so a
//! caller joins only a flight at least as urgent as itself. A session start
//! ([`ChildClass::Realtime`]) never waits on a probe that is yielding CPU to
//! playback ([`ChildClass::Background`]) — the probe most likely to time out
//! on a loaded node — and opens its own realtime flight instead. A verdict
//! from either class is shared.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::FutureExt;
use tokio::sync::watch;

use crate::ffmpeg::DoviPixelProof;
use crate::process_control::ChildClass;

type Flight = watch::Receiver<Option<DoviPixelProof>>;

#[derive(Default)]
struct Ledger {
    verdicts: HashMap<String, bool>,
    /// In-flight proofs by source key and the class their probes run at.
    flights: HashMap<(String, ChildClass), Flight>,
}

#[derive(Default)]
pub(crate) struct DoviProofs {
    ledger: Arc<Mutex<Ledger>>,
}

impl DoviProofs {
    /// The remembered verdict for `key`, if a proof has reached one.
    pub(crate) fn verdict(&self, key: &str) -> Option<bool> {
        self.ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .verdicts
            .get(key)
            .copied()
    }

    /// Record a verdict directly. Tests use it to stand in for a proof that
    /// already ran.
    #[cfg(test)]
    pub(crate) fn insert(&self, key: String, verdict: bool) {
        self.ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .verdicts
            .insert(key, verdict);
    }

    /// The proof for `key`: the remembered verdict, the result of an
    /// in-flight proof at least as urgent as `class`, or — for the first
    /// caller to miss — the result of `probe`, which runs at most once per
    /// flight and must run its children at `class`.
    pub(crate) async fn prove<F, Fut>(
        &self,
        key: String,
        class: ChildClass,
        probe: F,
    ) -> DoviPixelProof
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = DoviPixelProof> + Send + 'static,
    {
        let mut flight = {
            let mut ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(verdict) = ledger.verdicts.get(&key) {
                return if *verdict {
                    DoviPixelProof::Proved
                } else {
                    DoviPixelProof::Refuted
                };
            }
            // `ChildClass` orders most urgent first.
            let joinable = ChildClass::ALL
                .into_iter()
                .filter(|flight_class| *flight_class <= class)
                .find_map(|flight_class| ledger.flights.get(&(key.clone(), flight_class)).cloned());
            match joinable {
                Some(flight) => flight,
                None => {
                    let (sender, flight) = watch::channel(None);
                    let slot = (key.clone(), class);
                    ledger.flights.insert(slot.clone(), flight.clone());
                    let probe = probe();
                    let owner = Arc::clone(&self.ledger);
                    tokio::spawn(async move {
                        let proof = std::panic::AssertUnwindSafe(probe)
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                DoviPixelProof::Inconclusive("the pixel proof panicked".to_owned())
                            });
                        {
                            // The verdict and the end of the flight are one
                            // step, so a caller that misses the flight finds
                            // the verdict instead of starting a second proof.
                            let mut ledger = owner.lock().unwrap_or_else(PoisonError::into_inner);
                            if let Some(verdict) = proof.verdict() {
                                ledger.verdicts.insert(key, verdict);
                            }
                            ledger.flights.remove(&slot);
                        }
                        let _ = sender.send(Some(proof));
                    });
                    flight
                }
            }
        };
        let settled = match flight.wait_for(Option::is_some).await {
            Ok(proof) => proof.clone(),
            Err(_) => None,
        };
        settled.unwrap_or_else(|| {
            DoviPixelProof::Inconclusive("the pixel proof ended without a result".to_owned())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn an_inconclusive_proof_is_shared_but_never_remembered() {
        let proofs = DoviProofs::default();
        let first = proofs
            .prove("a:1:1".into(), ChildClass::Realtime, || async {
                DoviPixelProof::Inconclusive("Dolby Vision pixel probe timed out".into())
            })
            .await;
        assert!(matches!(first, DoviPixelProof::Inconclusive(_)));
        assert_eq!(proofs.verdict("a:1:1"), None, "a timeout is not a verdict");

        let second = proofs
            .prove("a:1:1".into(), ChildClass::Realtime, || async {
                DoviPixelProof::Proved
            })
            .await;
        assert_eq!(second, DoviPixelProof::Proved);
        assert_eq!(proofs.verdict("a:1:1"), Some(true));

        let refuted = proofs
            .prove("b:1:1".into(), ChildClass::Realtime, || async {
                DoviPixelProof::Refuted
            })
            .await;
        assert_eq!(refuted, DoviPixelProof::Refuted);
        assert_eq!(proofs.verdict("b:1:1"), Some(false));
        // A remembered verdict answers without running the probe again.
        let cached = proofs
            .prove(
                "b:1:1".into(),
                ChildClass::Realtime,
                || -> std::future::Ready<DoviPixelProof> {
                    panic!("a remembered verdict must not re-probe")
                },
            )
            .await;
        assert_eq!(cached, DoviPixelProof::Refuted);
    }

    #[tokio::test]
    async fn concurrent_misses_join_one_proof() {
        let proofs = DoviProofs::default();
        let runs = AtomicUsize::new(0);
        // `join_all` polls all four starts once before the current-thread
        // runtime can run the spawned proof, so every one of them misses the
        // verdict cache while the proof is still in flight.
        let results = futures_util::future::join_all((0..4).map(|_| {
            proofs.prove("p5:1:1".into(), ChildClass::Realtime, || {
                runs.fetch_add(1, Ordering::SeqCst);
                async {
                    tokio::task::yield_now().await;
                    DoviPixelProof::Inconclusive("probe timed out".into())
                }
            })
        }))
        .await;
        for result in results {
            assert_eq!(
                result,
                DoviPixelProof::Inconclusive("probe timed out".into()),
                "every waiter gets the leader's result"
            );
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "one probe for one flight");
        assert!(proofs.ledger.lock().expect("ledger").flights.is_empty());
        assert_eq!(proofs.verdict("p5:1:1"), None);
    }

    /// A session start never waits on a background-class proof: with only a
    /// background flight in progress it opens its own realtime flight, which
    /// a later background caller may join. A verdict from either is shared.
    #[tokio::test]
    async fn a_realtime_start_never_joins_a_background_proof() {
        let proofs = DoviProofs::default();
        let (release_background, background_released) = tokio::sync::oneshot::channel::<()>();
        let background = proofs.prove("p5:1:1".into(), ChildClass::Background, || async move {
            let _ = background_released.await;
            DoviPixelProof::Inconclusive("background probe timed out".into())
        });
        let classes = std::sync::Mutex::new(Vec::new());
        let realtime = async {
            // Let the background caller open its flight first.
            tokio::task::yield_now().await;
            assert!(proofs
                .ledger
                .lock()
                .expect("ledger")
                .flights
                .contains_key(&("p5:1:1".to_owned(), ChildClass::Background)));
            let proof = proofs
                .prove("p5:1:1".into(), ChildClass::Realtime, || {
                    classes.lock().expect("classes").push(ChildClass::Realtime);
                    async { DoviPixelProof::Proved }
                })
                .await;
            let _ = release_background.send(());
            proof
        };
        let (background, realtime) = tokio::join!(background, realtime);
        assert_eq!(
            classes.into_inner().expect("classes"),
            vec![ChildClass::Realtime],
            "the start ran its own realtime proof"
        );
        assert_eq!(realtime, DoviPixelProof::Proved);
        assert!(matches!(background, DoviPixelProof::Inconclusive(_)));
        assert_eq!(
            proofs.verdict("p5:1:1"),
            Some(true),
            "the verdict is shared"
        );
        assert!(proofs.ledger.lock().expect("ledger").flights.is_empty());

        // A background caller joins a realtime flight in progress.
        let runs = AtomicUsize::new(0);
        let results = futures_util::future::join_all(
            [ChildClass::Realtime, ChildClass::Background].map(|class| {
                proofs.prove("q:1:1".into(), class, || {
                    runs.fetch_add(1, Ordering::SeqCst);
                    async {
                        tokio::task::yield_now().await;
                        DoviPixelProof::Refuted
                    }
                })
            }),
        )
        .await;
        assert_eq!(
            results,
            vec![DoviPixelProof::Refuted, DoviPixelProof::Refuted]
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "background joined the realtime flight"
        );
    }
}
