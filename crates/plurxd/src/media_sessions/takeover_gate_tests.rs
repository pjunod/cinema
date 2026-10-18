use super::*;
use std::sync::atomic::AtomicBool;

/// `on` is the authoritative pair a consistent read returns; `replica` is
/// the same pair as this node's own replica holds it.
#[derive(Default)]
struct Switches {
    on: AtomicBool,
    replica: AtomicBool,
    failing: AtomicBool,
    reads: AtomicU64,
    hints: AtomicU64,
}

impl Switches {
    /// A save this replica has applied.
    fn set(&self, on: bool) {
        self.on.store(on, Ordering::SeqCst);
        self.replica.store(on, Ordering::SeqCst);
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

#[plurx_core::cluster::coordination::cluster_job_async_trait]
impl TakeoverSwitches for Switches {
    async fn takeover_switches(&self) -> Result<bool, StoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.failing.load(Ordering::SeqCst) {
            return Err(StoreError::Database("quorum unavailable".into()));
        }
        Ok(self.on.load(Ordering::SeqCst))
    }

    async fn takeover_switches_hint(&self) -> Result<bool, StoreError> {
        self.hints.fetch_add(1, Ordering::SeqCst);
        Ok(self.replica.load(Ordering::SeqCst))
    }
}

fn takeover_interval() -> tokio::time::Interval {
    let mut interval = tokio::time::interval(TAKEOVER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

/// A gate with its own write signal, so parallel tests do not share the
/// process-wide one, watching an applied index the test moves by hand.
fn watched_gate() -> (
    TakeoverGate,
    Arc<TakeoverSettingsSignal>,
    tokio::sync::watch::Sender<u64>,
) {
    let signal = Arc::new(TakeoverSettingsSignal::default());
    let (applied, changes) = tokio::sync::watch::channel(1u64);
    (
        TakeoverGate::with_signal(Arc::clone(&signal), SettingsChangeSource::Test(changes)),
        signal,
        applied,
    )
}

/// An entry this replica applied: some unrelated write, or a save whose
/// value `Switches::set` already put in the replica.
fn apply_entry(applied: &tokio::sync::watch::Sender<u64>) {
    applied.send_modify(|index| *index += 1);
}

/// The Raft watch wakes on any metrics change. One that leaves the applied
/// index where it was (a leader lease, a term) cannot have changed a
/// switch, so it must not cut the 10 s idle sleep short: on a busy
/// cluster it would shrink every idle sleep to the 2 s tick.
#[tokio::test(start_paused = true)]
async fn an_off_gate_sleeps_through_metrics_changes_that_do_not_apply_an_entry() {
    let switches = Switches::default();
    let (mut gate, _signal, applied) = watched_gate();
    let mut interval = takeover_interval();
    assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    let started = tokio::time::Instant::now();
    let nudges = async {
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            applied.send_modify(|_| {});
        }
    };
    let (acted, ()) = tokio::join!(gate.wait_for_tick(&mut interval, &switches), nudges);
    assert!(!acted);
    assert!(
        started.elapsed() >= TAKEOVER_IDLE_TICK.saturating_sub(TAKEOVER_INTERVAL),
        "woken after {:?} by changes that applied nothing",
        started.elapsed()
    );

    // An applied entry still ends the sleep at once.
    let started = tokio::time::Instant::now();
    let entry = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        apply_entry(&applied);
    };
    let (acted, ()) = tokio::join!(gate.wait_for_tick(&mut interval, &switches), entry);
    assert!(!acted);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "an applied entry ends the sleep, slept {:?}",
        started.elapsed()
    );
    assert_eq!(
        switches.reads(),
        1,
        "and the replica agreeing costs no read"
    );
}

/// A disable saved on another node that this replica has not applied:
/// the gate still holds its "on" (its replica agrees with it), so the
/// authority's inventory is what refuses (`TakeoverInventory::Disabled`,
/// store contract
/// `the_takeover_inventory_is_refused_while_either_switch_is_off`). The
/// refusal drops the held answer: the next tick costs one consistent
/// read, which proves the "off", and the gate then holds it.
#[tokio::test(start_paused = true)]
async fn an_authority_refusal_drops_a_stale_on_and_reproves() {
    let switches = Switches::default();
    switches.set(true);
    let (mut gate, _signal, applied) = watched_gate();
    let mut interval = takeover_interval();
    assert!(gate.wait_for_tick(&mut interval, &switches).await);
    assert_eq!(switches.reads(), 1);

    switches.on.store(false, Ordering::SeqCst);
    apply_entry(&applied);
    assert!(
        gate.wait_for_tick(&mut interval, &switches).await,
        "a replica that has not applied the disable still agrees with the held on"
    );
    assert_eq!(switches.reads(), 1);

    gate.refused_by_authority();
    assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    assert_eq!(switches.reads(), 2, "the refusal costs one consistent read");
    for _ in 0..30 {
        assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    }
    assert_eq!(switches.reads(), 2, "and the proved off is then held");
}

/// K-03 regression: an enabled gate on a node with nothing happening to
/// the switches. The old loop took one consistent read every 2 s tick
/// (lab4: 3,435 in 6,872 s); ten minutes now cost the first proof and
/// nothing else, while every tick still runs the scan and unrelated
/// writes keep moving the applied index (each costs one replica look).
#[tokio::test(start_paused = true)]
async fn an_idle_takeover_gate_takes_no_consistent_reads_after_its_first_proof() {
    for on in [true, false] {
        let switches = Switches::default();
        switches.set(on);
        let (mut gate, _signal, applied) = watched_gate();
        let mut interval = takeover_interval();
        let started = tokio::time::Instant::now();
        let (mut ticks, mut acted) = (0, 0);
        while started.elapsed() < Duration::from_secs(600) {
            if ticks % 3 == 1 {
                apply_entry(&applied);
            }
            if gate.wait_for_tick(&mut interval, &switches).await {
                acted += 1;
            }
            ticks += 1;
        }
        assert_eq!(
            switches.reads(),
            1,
            "on={on}: ten idle minutes must cost only the first consistent read"
        );
        assert!(
            switches.hints.load(Ordering::SeqCst) > 0,
            "on={on}: unrelated applied entries are checked against the replica"
        );
        if on {
            assert!(
                (299..=301).contains(&acted),
                "enabled takeover keeps its 2 s cadence, acted {acted} times in 600 s"
            );
        } else {
            assert_eq!(acted, 0, "an off switch must never reach the inventory");
        }
    }
}

/// A save on this node: exactly one consistent read, on the next tick,
/// and the new value applies there; then the gate holds it again.
#[tokio::test(start_paused = true)]
async fn a_local_takeover_save_costs_one_consistent_read_and_applies() {
    let switches = Switches::default();
    switches.set(true);
    let (mut gate, signal, applied) = watched_gate();
    let mut interval = takeover_interval();
    assert!(gate.wait_for_tick(&mut interval, &switches).await);
    assert_eq!(switches.reads(), 1);

    // The settings route writes, its own replica applies the entry, and
    // the route signals.
    switches.set(false);
    apply_entry(&applied);
    signal.changed();
    assert!(
        !gate.wait_for_tick(&mut interval, &switches).await,
        "a local disable stops the scan and CAS on the next tick"
    );
    assert_eq!(switches.reads(), 2, "the save costs exactly one read");
    let since = tokio::time::Instant::now();
    while since.elapsed() < Duration::from_secs(300) {
        apply_entry(&applied);
        assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    }
    assert_eq!(switches.reads(), 2, "and the off is then held");

    switches.set(true);
    apply_entry(&applied);
    signal.changed();
    assert!(gate.wait_for_tick(&mut interval, &switches).await);
    assert_eq!(switches.reads(), 3, "a local enable costs exactly one read");
}

/// A save on another node reaches this one as an applied entry whose
/// switches disagree with what the gate holds: one consistent read, on
/// the next tick after this replica applies it.
#[tokio::test(start_paused = true)]
async fn a_remote_takeover_change_costs_one_consistent_read_once_applied_here() {
    let switches = Switches::default();
    switches.set(true);
    let (mut gate, _signal, applied) = watched_gate();
    let mut interval = takeover_interval();
    assert!(gate.wait_for_tick(&mut interval, &switches).await);

    switches.set(false);
    apply_entry(&applied);
    assert!(
        !gate.wait_for_tick(&mut interval, &switches).await,
        "a disable saved elsewhere is refused on the next tick once applied here"
    );
    assert_eq!(switches.reads(), 2, "for one consistent read");

    // An enable saved elsewhere while this gate sleeps in its off period:
    // the applied entry ends the sleep, so it is acted on within one tick
    // of being applied rather than at the end of the idle period.
    let flipped_from = tokio::time::Instant::now();
    let flip = async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        switches.set(true);
        apply_entry(&applied);
    };
    let gate_loop = async {
        loop {
            if gate.wait_for_tick(&mut interval, &switches).await {
                break tokio::time::Instant::now();
            }
        }
    };
    let ((), acted_at) = tokio::join!(flip, gate_loop);
    assert!(
        acted_at.saturating_duration_since(flipped_from)
            <= Duration::from_secs(3) + TAKEOVER_INTERVAL,
        "a remote enable must be acted on within one tick of being applied, took {:?}",
        acted_at.saturating_duration_since(flipped_from)
    );
    assert_eq!(switches.reads(), 3, "for one consistent read");
}

/// A replica behind the consistent read disagrees with it until it
/// applies the save: that costs re-reads, never a stale answer, and the
/// gate holds again once the replica agrees.
#[tokio::test(start_paused = true)]
async fn a_replica_behind_the_proof_costs_rereads_until_it_catches_up() {
    let switches = Switches::default();
    switches.replica.store(true, Ordering::SeqCst);
    let (mut gate, _signal, applied) = watched_gate();
    let mut interval = takeover_interval();
    assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    apply_entry(&applied);
    assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    assert_eq!(
        switches.reads(),
        2,
        "the disagreeing replica costs a re-read"
    );

    switches.replica.store(false, Ordering::SeqCst);
    apply_entry(&applied);
    for _ in 0..30 {
        assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    }
    assert_eq!(switches.reads(), 2, "an agreeing replica holds the proof");
}

/// Fail-closed as before: while the authoritative pair cannot be read
/// every tick is off, the gate keeps reading on the idle cadence (applied
/// entries do not hurry it), and the first success is then held.
#[tokio::test(start_paused = true)]
async fn a_failed_takeover_read_keeps_the_gate_reading_until_one_succeeds() {
    let switches = Switches::default();
    switches.set(true);
    switches.failing.store(true, Ordering::SeqCst);
    let (mut gate, _signal, applied) = watched_gate();
    let mut interval = takeover_interval();
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        apply_entry(&applied);
        assert!(
            !gate.wait_for_tick(&mut interval, &switches).await,
            "an unreadable switch is off"
        );
    }
    let failed = switches.reads();
    assert!(
        (7..=8).contains(&failed),
        "a failing read is retried once an idle period, read {failed} times in 60 s"
    );

    switches.failing.store(false, Ordering::SeqCst);
    let mut acted = 0;
    let healed = tokio::time::Instant::now();
    while healed.elapsed() < Duration::from_secs(60) {
        apply_entry(&applied);
        if gate.wait_for_tick(&mut interval, &switches).await {
            acted += 1;
        }
    }
    assert!(
        acted >= 25,
        "the first success turns takeover on, acted {acted}"
    );
    assert_eq!(
        switches.reads(),
        failed + 1,
        "and is then held: one more consistent read"
    );
}

/// Without a change source that reports saves made elsewhere, the gate
/// keeps the K-03 M3 cadence: an "off" held for a minute, an "on" read
/// every tick.
#[tokio::test(start_paused = true)]
async fn the_takeover_gate_keeps_the_old_cadence_without_a_change_watch() {
    let switches = Switches::default();
    let signal = Arc::new(TakeoverSettingsSignal::default());
    let mut gate = TakeoverGate::with_signal(signal, SettingsChangeSource::Polled);
    let mut interval = takeover_interval();
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(600) {
        assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    }
    let reads = switches.reads();
    assert!(
        (10..=11).contains(&reads),
        "ten minutes off should read the switches about once a minute, read {reads}"
    );

    switches.set(true);
    tokio::time::sleep(TAKEOVER_SETTINGS_REFRESH).await;
    assert!(gate.wait_for_tick(&mut interval, &switches).await);
    let before = switches.reads();
    for _ in 0..30 {
        assert!(gate.wait_for_tick(&mut interval, &switches).await);
    }
    assert_eq!(
        switches.reads() - before,
        30,
        "an unwatched \"on\" is never held"
    );
}

/// Flipping the switch on this node wakes the loop within one tick, and
/// with both switches on the cadence is the unchanged 2 s.
#[tokio::test(start_paused = true)]
async fn takeover_loop_wakes_on_a_local_flip_and_keeps_its_two_second_cadence() {
    let switches = Arc::new(Switches::default());
    let (mut gate, signal, _applied) = watched_gate();
    let mut interval = takeover_interval();
    // Settle into the off state.
    for _ in 0..3 {
        assert!(!gate.wait_for_tick(&mut interval, switches.as_ref()).await);
    }
    let flip = Arc::clone(&switches);
    let flip_signal = Arc::clone(&signal);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        flip.set(true);
        flip_signal.changed();
    });
    let flipped_from = tokio::time::Instant::now();
    loop {
        if gate.wait_for_tick(&mut interval, switches.as_ref()).await {
            break;
        }
    }
    assert!(
        flipped_from.elapsed() <= Duration::from_secs(3) + TAKEOVER_INTERVAL,
        "a local flip must be acted on within one tick, took {:?}",
        flipped_from.elapsed()
    );

    let reads_before = switches.reads();
    let on_since = tokio::time::Instant::now();
    let mut ticks = 0;
    while on_since.elapsed() < Duration::from_secs(120) {
        assert!(gate.wait_for_tick(&mut interval, switches.as_ref()).await);
        ticks += 1;
    }
    assert!(
        (59..=61).contains(&ticks),
        "enabled takeover keeps its 2 s cadence, ticked {ticks} times in 120 s"
    );
    assert_eq!(
        switches.reads(),
        reads_before,
        "and holds the proved \"on\" while nothing changes"
    );
}

/// PR #405 review P2: an admin turns takeover off on this node while it
/// is on. The very next tick must refuse, and so must every tick after.
#[tokio::test(start_paused = true)]
async fn takeover_loop_stops_acting_within_one_tick_of_a_local_disable() {
    let switches = Switches::default();
    switches.set(true);
    let (mut gate, signal, _applied) = watched_gate();
    let mut interval = takeover_interval();
    assert!(gate.wait_for_tick(&mut interval, &switches).await);

    switches.set(false);
    signal.changed();
    let disabled_at = tokio::time::Instant::now();
    let mut acted = 0;
    while disabled_at.elapsed() < Duration::from_secs(68) {
        if gate.wait_for_tick(&mut interval, &switches).await {
            acted += 1;
        }
    }
    assert_eq!(
        acted, 0,
        "a local disable must stop the scan and CAS from the next tick"
    );
}

/// A switch write made while takeover is on (e.g. re-saving "on") costs
/// its one read, and a wake left over from it costs the later off state
/// nothing: the off read after that write is still current.
#[tokio::test(start_paused = true)]
async fn takeover_loop_ignores_a_wake_left_over_from_a_write_made_while_on() {
    let switches = Switches::default();
    switches.set(true);
    let (mut gate, signal, applied) = watched_gate();
    let mut interval = takeover_interval();
    assert!(gate.wait_for_tick(&mut interval, &switches).await);
    signal.changed();
    assert!(gate.wait_for_tick(&mut interval, &switches).await);
    assert_eq!(switches.reads(), 2);

    // Turned off on another node: the next tick reads "off" and holds it.
    switches.set(false);
    apply_entry(&applied);
    let off_since = tokio::time::Instant::now();
    while off_since.elapsed() < Duration::from_secs(50) {
        assert!(!gate.wait_for_tick(&mut interval, &switches).await);
    }
    assert_eq!(
        switches.reads(),
        3,
        "one read of \"off\", then the proof holds"
    );
}
