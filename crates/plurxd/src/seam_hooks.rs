//! Shared pieces of the per-owner lifecycle hooks
//! (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8).
//!
//! An owner that a race test pauses holds one small hook trait object in every
//! build, so its layout and its await points are the same in the test and
//! release binaries. Production installs the owner's no-op hooks, whose
//! asynchronous points return [`HookReady`]; tests install a pausing
//! implementation. A paused hook's timing is still a test artefact: what this
//! makes identical is the struct and the set of await points, not scheduling.

/// The future a lifecycle hook returns. Boxed so each owner's hook set is one
/// trait object in every build.
pub(crate) type HookFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

/// A zero-sized, already-ready future, so boxing it allocates nothing and
/// awaiting it costs one poll. Every owner's no-op hooks return it.
pub(crate) struct HookReady;

impl std::future::Future for HookReady {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        std::task::Poll::Ready(())
    }
}

/// How long either side of an [`AsyncPause`] waits for the other before
/// giving up. Every race test meets its pause within milliseconds; the bound
/// only decides how long a broken one takes to report.
#[cfg(test)]
pub(crate) const ASYNC_PAUSE_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// A one-shot rendezvous between a race test and an owner held at one of its
/// asynchronous hook points.
///
/// It replaces the `tokio::sync::Barrier` pair each old seam waited on twice,
/// whose test side waited without bound: a test whose owner never reached the
/// point hung instead of failing, and a test that awaited its owner while
/// holding the barrier deadlocked. Both sides are bounded here by wall-clock
/// time, measured on a helper thread rather than by a Tokio timer, so the
/// bound holds under `tokio::time::pause()` too and does not move a paused
/// clock. The test side ([`AsyncPause::reached`]) panics naming the point when
/// the owner does not arrive within the bound, and returns an
/// [`AsyncPauseHeld`] guard whose release or drop, including while the test
/// unwinds, lets the owner go; the owner side ([`AsyncPause::hold`]) lets
/// itself go after the bound in case the test never arrives. Once released, a
/// pause passes every later arrival straight through, so releasing a pause
/// the owner never reached disarms it.
#[cfg(test)]
pub(crate) struct AsyncPause {
    point: &'static str,
    bound: std::time::Duration,
    arrival: std::sync::Arc<tokio::sync::watch::Sender<Arrival>>,
    released: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arrival {
    Waiting,
    Reached,
    Overdue,
}

/// The test's hold on an owner that reached its [`AsyncPause`]. The owner
/// stays paused until this is released or dropped.
#[cfg(test)]
#[must_use = "dropping the hold releases the owner at once"]
pub(crate) struct AsyncPauseHeld<'a>(&'a AsyncPause);

#[cfg(test)]
impl AsyncPause {
    pub(crate) fn new(point: &'static str) -> std::sync::Arc<Self> {
        Self::with_bound(point, ASYNC_PAUSE_BOUND)
    }

    pub(crate) fn with_bound(
        point: &'static str,
        bound: std::time::Duration,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            point,
            bound,
            arrival: std::sync::Arc::new(tokio::sync::watch::Sender::new(Arrival::Waiting)),
            released: std::sync::Arc::new(tokio::sync::watch::Sender::new(false)),
        })
    }

    /// The owner's side: announce the point, then wait to be released. Returns
    /// whether the test released it, as opposed to the bound running out.
    pub(crate) async fn hold(&self) -> bool {
        self.arrival.send_replace(Arrival::Reached);
        let mut released = self.released.subscribe();
        if *released.borrow_and_update() {
            return true;
        }
        let expired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let released = std::sync::Arc::clone(&self.released);
            let expired = std::sync::Arc::clone(&expired);
            let (point, bound) = (self.point, self.bound);
            std::thread::spawn(move || {
                std::thread::sleep(bound);
                released.send_if_modified(|released| {
                    if *released {
                        return false;
                    }
                    expired.store(true, std::sync::atomic::Ordering::Release);
                    eprintln!(
                        "owner left the {point} pause after {bound:?}: the test never released it"
                    );
                    *released = true;
                    true
                });
            });
        }
        let _ = released.wait_for(|released| *released).await;
        !expired.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether the owner has reached the point.
    pub(crate) fn was_reached(&self) -> bool {
        *self.arrival.borrow() == Arrival::Reached
    }

    /// Let the owner go, or, if it has not arrived, let it pass when it does.
    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }

    /// The test's side: wait until the owner holds this pause, and hold it
    /// there until the returned guard is released or dropped. Panics, so the
    /// test fails with the point's name, when the owner does not arrive
    /// within the bound.
    pub(crate) async fn reached(&self) -> AsyncPauseHeld<'_> {
        let mut arrival = self.arrival.subscribe();
        if *arrival.borrow_and_update() != Arrival::Reached {
            let timer = std::sync::Arc::clone(&self.arrival);
            let bound = self.bound;
            std::thread::spawn(move || {
                std::thread::sleep(bound);
                timer.send_if_modified(|arrival| {
                    if *arrival != Arrival::Waiting {
                        return false;
                    }
                    *arrival = Arrival::Overdue;
                    true
                });
            });
            let _ = arrival
                .wait_for(|arrival| *arrival != Arrival::Waiting)
                .await;
        }
        if *self.arrival.borrow() != Arrival::Reached {
            self.release();
            panic!(
                "the owner never reached the {} pause within {:?}",
                self.point, self.bound
            );
        }
        AsyncPauseHeld(self)
    }
}

#[cfg(test)]
impl AsyncPauseHeld<'_> {
    /// Let the owner go now.
    pub(crate) fn release(self) {}
}

#[cfg(test)]
impl Drop for AsyncPauseHeld<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn async_pause_holds_the_owner_until_the_test_releases_it() {
        let pause = AsyncPause::new("fixture");
        let owner = tokio::spawn({
            let pause = std::sync::Arc::clone(&pause);
            async move { pause.hold().await }
        });
        let held = pause.reached().await;
        assert!(pause.was_reached());
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!owner.is_finished(), "a held owner must stay at its point");
        held.release();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), owner)
                .await
                .expect("a released owner continues")
                .expect("owner task"),
            "the test released the owner, not the bound"
        );
    }

    #[tokio::test]
    async fn async_pause_releases_its_owner_when_the_test_unwinds() {
        let pause = AsyncPause::new("fixture");
        let owner = tokio::spawn({
            let pause = std::sync::Arc::clone(&pause);
            async move { pause.hold().await }
        });
        let unwound = {
            let pause = std::sync::Arc::clone(&pause);
            tokio::spawn(async move {
                let _held = pause.reached().await;
                panic!("an assertion fails while the owner is held");
            })
        }
        .await;
        assert!(unwound.is_err_and(|error| error.is_panic()));
        tokio::time::timeout(Duration::from_secs(1), owner)
            .await
            .expect("the unwinding test released its owner promptly")
            .expect("owner task");
    }

    #[tokio::test]
    async fn async_pause_names_a_point_the_owner_never_reaches() {
        let pause = AsyncPause::with_bound("fixture point", Duration::from_millis(50));
        let waited = tokio::spawn({
            let pause = std::sync::Arc::clone(&pause);
            async move {
                let _held = pause.reached().await;
            }
        })
        .await
        .expect_err("a point nobody reaches fails the test");
        let message = waited.into_panic();
        let message = message
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default();
        assert!(message.contains("fixture point"), "{message}");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), pause.hold())
                .await
                .expect("the failed test disarmed its pause"),
            "a pause the test released passes a late owner through"
        );
    }

    #[tokio::test]
    async fn async_pause_lets_the_owner_go_when_the_test_never_arrives() {
        let pause = AsyncPause::with_bound("fixture", Duration::from_millis(50));
        let released_by_test = tokio::time::timeout(Duration::from_secs(5), pause.hold())
            .await
            .expect("the owner lets itself go after the bound");
        assert!(!released_by_test);
    }

    #[tokio::test(start_paused = true)]
    async fn async_pause_bound_is_wall_clock_under_a_paused_runtime() {
        let pause = AsyncPause::new("fixture");
        let owner = tokio::spawn({
            let pause = std::sync::Arc::clone(&pause);
            async move { pause.hold().await }
        });
        let held = pause.reached().await;
        // Idle virtual time far beyond the bound: a Tokio-timer bound would
        // have let the owner go here.
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!owner.is_finished(), "virtual time does not end a hold");
        held.release();
        assert!(owner.await.expect("owner task"));
    }
}
