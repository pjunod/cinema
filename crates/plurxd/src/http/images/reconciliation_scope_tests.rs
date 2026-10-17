use super::*;

fn view(
    active_voter: bool,
    committed_voter: bool,
    committed_member: bool,
) -> ArtworkMembershipView {
    ArtworkMembershipView {
        active_voter,
        committed_voter,
        committed_member,
    }
}

/// A learner reconciles its own artwork forever, and is never mistaken for
/// a removed node.
///
/// This is the regression: the tick used to read "not a voter" as "removed
/// voter" and `break` out of the loop on the learner's first pass, roughly
/// two seconds after boot. The learner then never materialized an image
/// again — not after a promotion either, because the loop was gone — and
/// the log said it had been removed, which was false.
#[test]
fn a_member_without_a_vote_reconciles_locally_instead_of_stopping() {
    assert_eq!(
        artwork_tick(view(false, false, true)),
        ArtworkTick::Reconcile(ArtworkReconciliation::LocalOnly),
        "a learner is a member; it converges its own artwork directory"
    );
}

/// Promotion needs no restart: the same view a moment later is a full pass.
#[test]
fn a_promoted_learner_starts_provider_work_on_its_next_tick() {
    assert_eq!(
        artwork_tick(view(true, true, true)),
        ArtworkTick::Reconcile(ArtworkReconciliation::Full)
    );
}

/// Only genuine absence from committed membership ends the loop.
#[test]
fn stopping_is_reserved_for_a_node_that_is_no_longer_a_member() {
    assert_eq!(artwork_tick(view(false, false, false)), ArtworkTick::Stop);
}

/// A committed voter whose durable row is fenced by an in-flight removal
/// waits, rather than doing provider work in the transition window or
/// deciding it has been removed.
#[test]
fn a_fenced_voter_waits_for_the_membership_change_to_settle() {
    assert_eq!(artwork_tick(view(false, true, true)), ArtworkTick::Wait);
}
