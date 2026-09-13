//! Which airings get a tuner, and which are told they cannot have one.
//!
//! Kept as pure functions over plain rows, with no manager, no clock and no
//! registry, because this is the part of the DVR whose mistakes are silent: a
//! scheduler that quietly drops one recording a week looks exactly like a
//! working scheduler until someone goes to watch the episode that is missing.
//!
//! Two rules do all the work:
//!
//! 1. **Back-to-back airings on one channel cost one tuner.** The engine
//!    records them through a single tuner GET writing two files, so the
//!    planner has to count *channels in use*, not recordings.
//! 2. **Priority decides who loses, not arrival order.** Slots are granted in
//!    rank order — a person's one-off outranks every rule, and rules outrank
//!    each other by their own priority — so a low-priority series cannot take
//!    the tuner a high-priority one needs merely by starting first.

use plurx_core::dvr::{DvrOrigin, DvrRecording, DvrState};

/// What the planner decided about one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Allocation {
    pub(crate) id: String,
    pub(crate) state: DvrState,
    pub(crate) reason: Option<String>,
}

/// One row's claim on a channel for a span of time.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Claim {
    channel_id: String,
    start: i64,
    end: i64,
}

/// Rank a row for the contended case. Lower wins.
///
/// A person who pressed Record outranks every rule, because they asked for one
/// specific programme and a rule is a standing instruction. Beyond that it is
/// the rule's own priority, and finally the airing's start so the answer is
/// stable across ticks rather than depending on map iteration order.
fn rank(row: &DvrRecording, rule_priority: &dyn Fn(&str) -> i64) -> (i64, i64, i64, String) {
    let priority = match (row.origin, row.rule_id.as_deref()) {
        (DvrOrigin::Manual, _) => i64::MIN,
        (DvrOrigin::Rule, Some(rule_id)) => rule_priority(rule_id),
        // A rule row whose rule is gone is about to be withdrawn by
        // reconciliation. Rank it last rather than treating a missing rule as
        // the most important thing on the server.
        (DvrOrigin::Rule, None) => i64::MAX,
    };
    (
        priority,
        row.airing_start,
        row.capture_start,
        row.id.clone(),
    )
}

/// Plan the tuner for every pending row.
///
/// `slots` is `max_sessions − reserve`: the number of tuner sessions
/// recordings may hold at once, never the whole set. `disk_floor_met` being
/// false conflicts everything with one honest reason rather than letting
/// captures start and fail on ENOSPC one by one.
pub(crate) fn allocate(
    rows: &[DvrRecording],
    slots: u8,
    disk_floor_met: bool,
    rule_priority: &dyn Fn(&str) -> i64,
) -> Vec<Allocation> {
    let mut pending = rows
        .iter()
        .filter(|row| matches!(row.state, DvrState::Scheduled | DvrState::Conflict))
        .collect::<Vec<_>>();
    pending.sort_by_key(|row| rank(row, rule_priority));

    // A capture already running holds its channel for the rest of its window,
    // and no plan may take it away: pre-empting a recording that is writing
    // bytes is the one thing the whole design refuses.
    let mut granted = rows
        .iter()
        .filter(|row| row.state == DvrState::Recording)
        .map(|row| Claim {
            channel_id: row.channel_id.clone(),
            start: row.capture_start,
            end: row.capture_end,
        })
        .collect::<Vec<_>>();

    let mut out = Vec::with_capacity(pending.len());
    for row in pending {
        if !disk_floor_met {
            out.push(Allocation {
                id: row.id.clone(),
                state: DvrState::Conflict,
                reason: Some("disk below the free-space floor".to_owned()),
            });
            continue;
        }
        if slots == 0 {
            out.push(Allocation {
                id: row.id.clone(),
                state: DvrState::Conflict,
                reason: Some("every tuner is reserved for viewing".to_owned()),
            });
            continue;
        }
        let claim = Claim {
            channel_id: row.channel_id.clone(),
            start: row.capture_start,
            end: row.capture_end,
        };
        if fits(&granted, &claim, slots) {
            granted.push(claim);
            out.push(Allocation {
                id: row.id.clone(),
                state: DvrState::Scheduled,
                reason: None,
            });
        } else {
            out.push(Allocation {
                id: row.id.clone(),
                state: DvrState::Conflict,
                reason: Some(format!(
                    "no tuner free at that time ({slots} available to recordings)"
                )),
            });
        }
    }
    out
}

/// Whether one more claim fits inside `slots` at every instant of its span.
///
/// The count at an instant is *distinct channels*, because one channel is one
/// tuner however many recordings are taking their own file from it. So a claim
/// on a channel that is already held at that instant is free, and only a claim
/// on a new channel has to find room.
fn fits(granted: &[Claim], claim: &Claim, slots: u8) -> bool {
    let slots = usize::from(slots);
    // Every instant where the answer can change: a claim starting or ending.
    let mut boundaries = vec![claim.start];
    for existing in granted {
        for point in [existing.start, existing.end] {
            if point > claim.start && point < claim.end {
                boundaries.push(point);
            }
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    for point in boundaries {
        let mut channels = granted
            .iter()
            .filter(|existing| existing.start <= point && existing.end > point)
            .map(|existing| existing.channel_id.as_str())
            .collect::<Vec<_>>();
        channels.sort_unstable();
        channels.dedup();
        if channels.contains(&claim.channel_id.as_str()) {
            continue;
        }
        if channels.len() + 1 > slots {
            return false;
        }
    }
    true
}

/// Whether a sink may join an open transport on the same channel, rather than
/// needing a tuner of its own.
///
/// The transport is already tuned to this channel and its bytes are the same
/// bytes. It can be shared as long as it is still open when the new capture
/// wants to begin — the tail padding of the 8:30 programme overlapping the
/// head padding of the 9:00 one is exactly the case this exists for.
pub(crate) fn may_share_transport(transport_end: i64, capture_start: i64) -> bool {
    transport_end >= capture_start
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, channel: &str, start: i64, end: i64, state: DvrState) -> DvrRecording {
        DvrRecording {
            id: id.to_owned(),
            origin: DvrOrigin::Rule,
            rule_id: Some("rule-a".to_owned()),
            requested_by_user_id: None,
            channel_id: channel.to_owned(),
            guide_number: channel.to_owned(),
            channel_name: channel.to_owned(),
            airing_start: start,
            airing_end: end,
            capture_start: start,
            capture_end: end,
            title: id.to_owned(),
            episode_title: None,
            episode: None,
            synopsis: None,
            image_url: None,
            original_air_date: None,
            series_id: None,
            programme_id: None,
            state,
            state_reason: None,
            attempt: 0,
            gap_s: 0,
            late_start_s: 0,
            tuner_owner_node_id: None,
            path: None,
            bytes: 0,
            last_progress_ms: None,
            stop_requested_at_ms: None,
            stop_requested_by_user_id: None,
            item_id: None,
            file_id: None,
            started_at_ms: None,
            finished_at_ms: None,
            stopped_by_user_id: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn flat(_: &str) -> i64 {
        10
    }

    fn state_of(plan: &[Allocation], id: &str) -> DvrState {
        plan.iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("{id} is missing from the plan"))
            .state
    }

    #[test]
    fn back_to_back_airings_on_one_channel_cost_one_tuner() {
        let rows = vec![
            row("a", "7.1", 0, 1800, DvrState::Scheduled),
            row("b", "7.1", 1800, 3600, DvrState::Scheduled),
            row("c", "7.1", 3600, 5400, DvrState::Scheduled),
        ];
        let plan = allocate(&rows, 1, true, &flat);
        for id in ["a", "b", "c"] {
            assert_eq!(
                state_of(&plan, id),
                DvrState::Scheduled,
                "one channel is one tuner however many programmes come off it"
            );
        }
    }

    #[test]
    fn overlapping_pads_on_one_channel_still_share_the_tuner() {
        // The 8:30 programme's tail pad runs into the 9:00 programme's head
        // pad. Two files, one tuner, and both must be planned.
        let rows = vec![
            row("tail", "7.1", 0, 1920, DvrState::Scheduled),
            row("head", "7.1", 1740, 3600, DvrState::Scheduled),
        ];
        let plan = allocate(&rows, 1, true, &flat);
        assert_eq!(state_of(&plan, "tail"), DvrState::Scheduled);
        assert_eq!(state_of(&plan, "head"), DvrState::Scheduled);
    }

    #[test]
    fn a_fourth_channel_at_the_same_time_is_a_conflict_with_a_reason() {
        let rows = vec![
            row("a", "7.1", 0, 3600, DvrState::Scheduled),
            row("b", "5.1", 0, 3600, DvrState::Scheduled),
            row("c", "4.1", 0, 3600, DvrState::Scheduled),
            row("d", "2.1", 0, 3600, DvrState::Scheduled),
        ];
        let plan = allocate(&rows, 3, true, &flat);
        let conflicts = plan
            .iter()
            .filter(|entry| entry.state == DvrState::Conflict)
            .collect::<Vec<_>>();
        assert_eq!(conflicts.len(), 1, "three slots hold three channels");
        assert!(conflicts[0]
            .reason
            .as_deref()
            .expect("a conflict always says why")
            .contains("no tuner free"));
    }

    #[test]
    fn priority_decides_who_loses_rather_than_who_asked_first() {
        let mut early = row("early", "7.1", 0, 3600, DvrState::Scheduled);
        early.rule_id = Some("low".to_owned());
        let mut late = row("late", "5.1", 60, 3600, DvrState::Scheduled);
        late.rule_id = Some("high".to_owned());
        let priority = |id: &str| if id == "high" { 1 } else { 9 };

        let plan = allocate(&[early, late], 1, true, &priority);
        assert_eq!(
            state_of(&plan, "late"),
            DvrState::Scheduled,
            "the higher-priority rule takes the tuner"
        );
        assert_eq!(state_of(&plan, "early"), DvrState::Conflict);
    }

    #[test]
    fn a_persons_one_off_outranks_every_rule() {
        let mut manual = row("mine", "5.1", 0, 3600, DvrState::Scheduled);
        manual.origin = DvrOrigin::Manual;
        manual.rule_id = None;
        let mut best_rule = row("rule", "7.1", 0, 3600, DvrState::Scheduled);
        best_rule.rule_id = Some("top".to_owned());

        let plan = allocate(&[best_rule, manual], 1, true, &|_| i64::MIN + 1);
        assert_eq!(state_of(&plan, "mine"), DvrState::Scheduled);
        assert_eq!(state_of(&plan, "rule"), DvrState::Conflict);
    }

    #[test]
    fn a_running_capture_is_never_planned_away() {
        let running = row("running", "7.1", 0, 3600, DvrState::Recording);
        let mut rival = row("rival", "5.1", 0, 3600, DvrState::Scheduled);
        rival.origin = DvrOrigin::Manual;
        rival.rule_id = None;

        let plan = allocate(&[running, rival], 1, true, &flat);
        assert_eq!(
            state_of(&plan, "rival"),
            DvrState::Conflict,
            "pre-empting a capture that is writing bytes is the one thing this refuses"
        );
        assert!(
            plan.iter().all(|entry| entry.id != "running"),
            "a running capture is not re-planned at all"
        );
    }

    #[test]
    fn a_freed_tuner_unconflicts_the_next_airing_without_anyone_asking() {
        let rows = vec![
            row("first", "7.1", 0, 1800, DvrState::Scheduled),
            row("second", "5.1", 0, 1800, DvrState::Scheduled),
            // Starts after the first two end, so it always fits.
            row("later", "4.1", 1800, 3600, DvrState::Scheduled),
        ];
        let plan = allocate(&rows, 1, true, &flat);
        assert_eq!(
            state_of(&plan, "later"),
            DvrState::Scheduled,
            "the plan is recomputed over time, not over arrival"
        );
        assert_eq!(state_of(&plan, "second"), DvrState::Conflict);
    }

    #[test]
    fn a_disk_below_the_floor_conflicts_everything_with_one_honest_reason() {
        let rows = vec![row("a", "7.1", 0, 1800, DvrState::Scheduled)];
        let plan = allocate(&rows, 3, false, &flat);
        assert_eq!(state_of(&plan, "a"), DvrState::Conflict);
        assert_eq!(
            plan[0].reason.as_deref(),
            Some("disk below the free-space floor"),
            "a capture that would fail on ENOSPC is better refused with the real reason"
        );
    }

    #[test]
    fn reserving_every_tuner_stops_recording_rather_than_underflowing() {
        let rows = vec![row("a", "7.1", 0, 1800, DvrState::Scheduled)];
        let plan = allocate(&rows, 0, true, &flat);
        assert_eq!(state_of(&plan, "a"), DvrState::Conflict);
        assert_eq!(
            plan[0].reason.as_deref(),
            Some("every tuner is reserved for viewing")
        );
    }

    #[test]
    fn a_transport_is_shared_only_while_it_is_still_open() {
        assert!(may_share_transport(1920, 1740), "the pads overlap");
        assert!(may_share_transport(1800, 1800), "back to back, exactly");
        assert!(
            !may_share_transport(1800, 1801),
            "a second of gap means the tuner was released"
        );
    }
}
