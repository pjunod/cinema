//! The claim records the viewer's ask in its own transaction (D3c), on both
//! backends: only a claim that is `Acquired` upserts, under a predicate that
//! matches this attempt's claim row, and the answer carries the recorded row.
use super::*;
use plurx_core::domain::{DesiredSelectionWrite, MediaSessionRequestClaim};
use plurx_core::playback_principal::PlaybackPrincipal;

const FP: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

fn incarnation(n: u128) -> String {
    uuid::Uuid::from_u128(0xd3c0_0000 + n).to_string()
}

fn ask<'a>(digest: &'a str, form: &'a str) -> DesiredSelectionWrite<'a> {
    ask_at(digest, form, 1_000)
}

fn ask_at<'a>(digest: &'a str, form: &'a str, arrived_at_ms: i64) -> DesiredSelectionWrite<'a> {
    DesiredSelectionWrite {
        digest,
        canonical_form: form,
        arrived_at_ms,
    }
}

async fn desired(
    store: &dyn Store,
    user_id: i64,
    playback: &str,
) -> Option<plurx_core::domain::DesiredOwnership> {
    store
        .desired_selection(&PlaybackPrincipal::LocalUser { user_id }, playback)
        .await
        .expect("read the desired selection")
}

#[allow(clippy::too_many_arguments)]
async fn claim(
    store: &dyn Store,
    user_id: i64,
    request: &str,
    fingerprint: &str,
    playback: &str,
    incarnation_id: &str,
    now: i64,
    expires: i64,
    write: Option<&DesiredSelectionWrite<'_>>,
    backend: &str,
) -> (
    MediaSessionRequestClaim,
    Option<plurx_core::domain::DesiredOwnership>,
) {
    store
        .claim_media_session_request_recording(
            &PlaybackPrincipal::LocalUser { user_id },
            request,
            fingerprint,
            playback,
            incarnation_id,
            now,
            expires,
            write,
        )
        .await
        .unwrap_or_else(|error| panic!("{backend}: claim {request}: {error}"))
}

/// An acquired claim records the ask and answers with the row; a joined,
/// conflicting or overloaded claim records nothing and answers `None`; a
/// plain claim records nothing; a repeated digest does not move the revision
/// and a different one advances it by one, including through a re-acquisition.
#[tokio::test]
async fn a_claim_records_the_ask_only_when_it_acquires() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("claim-ask", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create user: {error}"));
        let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        let store = store.as_ref();

        // A plain claim records no ask.
        let plain = incarnation(1);
        let (outcome, recorded) = claim(
            store, user.id, "plain", FP, "pb-plain", &plain, 1_000, 61_000, None, backend,
        )
        .await;
        assert!(matches!(outcome, MediaSessionRequestClaim::Acquired { .. }), "{backend}");
        assert!(recorded.is_none(), "{backend}: no ask, nothing recorded");
        assert!(desired(store, user.id, "pb-plain").await.is_none(), "{backend}");

        // Acquired with an ask: the row is written and comes back.
        let first = incarnation(2);
        let (outcome, recorded) = claim(
            store,
            user.id,
            "req",
            FP,
            "pb",
            &first,
            1_000,
            61_000,
            Some(&ask(&a, "v1;quality=auto")),
            backend,
        )
        .await;
        assert!(
            matches!(&outcome, MediaSessionRequestClaim::Acquired { incarnation_id } if *incarnation_id == first),
            "{backend}: {outcome:?}"
        );
        let recorded = recorded.unwrap_or_else(|| panic!("{backend}: an acquired claim answers the ask"));
        assert_eq!((recorded.revision, recorded.digest.as_str()), (1, a.as_str()), "{backend}");
        assert_eq!(desired(store, user.id, "pb").await, Some(recorded.clone()), "{backend}");

        // A joined claim (same request, another incarnation, the first still
        // starting) upserts nothing, whatever its ask says.
        let joiner = incarnation(3);
        let (outcome, recorded_by_joiner) = claim(
            store,
            user.id,
            "req",
            FP,
            "pb",
            &joiner,
            1_001,
            61_001,
            Some(&ask(&b, "v1;quality=original")),
            backend,
        )
        .await;
        assert!(
            matches!(&outcome, MediaSessionRequestClaim::InFlight { incarnation_id, .. } if *incarnation_id == first),
            "{backend}: {outcome:?}"
        );
        assert!(recorded_by_joiner.is_none(), "{backend}: a joined claim records nothing");
        assert_eq!(desired(store, user.id, "pb").await, Some(recorded.clone()), "{backend}: the joiner's ask did not land");

        // A conflicting fingerprint upserts nothing either.
        let (outcome, none) = claim(
            store,
            user.id,
            "req",
            &"b".repeat(64),
            "pb",
            &incarnation(4),
            1_002,
            61_002,
            Some(&ask(&c, "v1;quality=low")),
            backend,
        )
        .await;
        assert_eq!(outcome, MediaSessionRequestClaim::Conflict, "{backend}");
        assert!(none.is_none(), "{backend}");
        assert_eq!(desired(store, user.id, "pb").await, Some(recorded.clone()), "{backend}");

        // A re-acquisition after the owner let go records the ask in its own
        // transaction: the same digest does not move the revision ...
        assert!(store
            .fail_media_session_request(
                &PlaybackPrincipal::LocalUser { user_id: user.id },
                "req",
                &first,
                1_003,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: fail the first attempt: {error}")));
        let second = incarnation(5);
        let (outcome, again) = claim(
            store,
            user.id,
            "req",
            FP,
            "pb",
            &second,
            1_004,
            61_004,
            Some(&ask(&a, "v1;quality=auto")),
            backend,
        )
        .await;
        assert!(
            matches!(&outcome, MediaSessionRequestClaim::Acquired { incarnation_id } if *incarnation_id == second),
            "{backend}: {outcome:?}"
        );
        let again = again.unwrap_or_else(|| panic!("{backend}: a re-acquired claim answers the ask"));
        assert_eq!(again.revision, 1, "{backend}: a repeated digest keeps its revision");
        assert_eq!(again.updated_at_ms, recorded.updated_at_ms, "{backend}");

        // ... and a different digest advances it by exactly one.
        assert!(store
            .fail_media_session_request(
                &PlaybackPrincipal::LocalUser { user_id: user.id },
                "req",
                &second,
                1_005,
            )
            .await
            .unwrap_or_else(|error| panic!("{backend}: fail the second attempt: {error}")));
        let third = incarnation(6);
        let (outcome, changed) = claim(
            store,
            user.id,
            "req",
            FP,
            "pb",
            &third,
            1_006,
            61_006,
            Some(&ask(&b, "v1;quality=original")),
            backend,
        )
        .await;
        assert!(matches!(outcome, MediaSessionRequestClaim::Acquired { .. }), "{backend}");
        let changed = changed.unwrap_or_else(|| panic!("{backend}: answers the ask"));
        assert_eq!((changed.revision, changed.digest.as_str()), (2, b.as_str()), "{backend}");
        assert_eq!(desired(store, user.id, "pb").await, Some(changed), "{backend}");

        // An expired claim is re-acquired the same way.
        let (outcome, _) = claim(
            store,
            user.id,
            "short",
            FP,
            "pb-short",
            &incarnation(7),
            2_000,
            2_001,
            Some(&ask(&a, "v1;quality=auto")),
            backend,
        )
        .await;
        assert!(matches!(outcome, MediaSessionRequestClaim::Acquired { .. }), "{backend}");
        let (outcome, expired) = claim(
            store,
            user.id,
            "short",
            FP,
            "pb-short",
            &incarnation(8),
            2_010,
            62_010,
            Some(&ask(&b, "v1;quality=original")),
            backend,
        )
        .await;
        assert!(matches!(outcome, MediaSessionRequestClaim::Acquired { .. }), "{backend}: {outcome:?}");
        assert_eq!(
            expired.map(|row| (row.revision, row.digest)),
            Some((2, b.clone())),
            "{backend}: the re-acquisition recorded the later ask"
        );
    })
    .await;
}

/// An overloaded claim records nothing, and an invalid ask is refused before
/// any write.
#[tokio::test]
async fn an_overloaded_or_invalid_claim_records_no_ask() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("claim-ask-overload", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create user: {error}"));
        let a = "a".repeat(64);
        let store = store.as_ref();
        // The per-user in-flight allowance is 32 unexpired `starting` claims.
        for n in 0..32u128 {
            let (outcome, _) = claim(
                store,
                user.id,
                &format!("fill-{n}"),
                FP,
                &format!("fill-pb-{n}"),
                &incarnation(100 + n),
                5_000,
                65_000,
                None,
                backend,
            )
            .await;
            assert!(
                matches!(outcome, MediaSessionRequestClaim::Acquired { .. }),
                "{backend}: fill {n}"
            );
        }
        let (outcome, none) = claim(
            store,
            user.id,
            "over",
            FP,
            "pb-over",
            &incarnation(200),
            5_001,
            65_001,
            Some(&ask(&a, "v1;quality=auto")),
            backend,
        )
        .await;
        assert_eq!(outcome, MediaSessionRequestClaim::Overloaded, "{backend}");
        assert!(none.is_none(), "{backend}");
        assert!(
            desired(store, user.id, "pb-over").await.is_none(),
            "{backend}: an overloaded claim recorded an ask"
        );

        // A malformed ask is refused before the claim writes anything.
        let refused = store
            .claim_media_session_request_recording(
                &PlaybackPrincipal::LocalUser { user_id: user.id },
                "bad",
                FP,
                "pb-bad",
                &incarnation(300),
                5_002,
                65_002,
                Some(&ask("not-a-digest", "v1;quality=auto")),
            )
            .await;
        assert!(refused.is_err(), "{backend}: a malformed ask is refused");
        assert!(
            desired(store, user.id, "pb-bad").await.is_none(),
            "{backend}"
        );
        assert!(
            store
                .media_session_request_progress(
                    &PlaybackPrincipal::LocalUser { user_id: user.id },
                    "bad"
                )
                .await
                .unwrap_or_else(|error| panic!("{backend}: progress: {error}"))
                .is_none(),
            "{backend}: the refused claim left no row"
        );
    })
    .await;
}

/// Concurrent claims for one request upsert at most once: one is `Acquired`
/// and its ask is the one recorded; the rest answer `InFlight` and record
/// nothing, so the revision is 1 and the winner's digest stands.
#[tokio::test]
async fn concurrent_claims_for_one_request_record_one_ask() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("claim-ask-race", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create user: {error}"));
        let digests: Vec<String> = (0..4u8).map(|n| format!("{n:x}").repeat(64)).collect();
        let forms: Vec<String> = (0..4).map(|n| format!("v1;quality={n}")).collect();
        let incarnations: Vec<String> = (0..4u128).map(|n| incarnation(400 + n)).collect();
        let user_id = user.id;
        let results = futures_util::future::join_all((0..4).map(|n| {
            let store = Arc::clone(&store);
            let (digest, form, inc) = (
                digests[n].clone(),
                forms[n].clone(),
                incarnations[n].clone(),
            );
            async move {
                claim(
                    store.as_ref(),
                    user_id,
                    "race",
                    FP,
                    "pb-race",
                    &inc,
                    9_000,
                    69_000,
                    Some(&ask(&digest, &form)),
                    backend,
                )
                .await
            }
        }))
        .await;
        let winners: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, (outcome, _))| {
                matches!(outcome, MediaSessionRequestClaim::Acquired { .. })
            })
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "{backend}: exactly one claim acquires: {results:?}"
        );
        for (n, (_, recorded)) in results.iter().enumerate() {
            assert_eq!(
                recorded.is_some(),
                n == winners[0],
                "{backend}: only the winner's claim answers an ask"
            );
        }
        let row = desired(store.as_ref(), user.id, "pb-race")
            .await
            .unwrap_or_else(|| panic!("{backend}: the winner's ask is recorded"));
        assert_eq!(row.revision, 1, "{backend}: the ask was upserted once");
        assert_eq!(
            row.digest, digests[winners[0]],
            "{backend}: it is the winner's ask"
        );
    })
    .await;
}

/// Asks stay ordered by arrival, not by which create reaches its claim first
/// (D3 review S2): the create that arrived earlier but claims later answers
/// with the ask that stands and replaces nothing; a later arrival, and an
/// equal stamp, replace; a re-acquired claim follows the same rule.
#[tokio::test]
async fn an_earlier_but_slower_create_does_not_replace_a_newer_ask() {
    for_each_backend(|store, backend| async move {
        let user = store
            .create_user("claim-order", "hash", false)
            .await
            .unwrap_or_else(|error| panic!("{backend}: create user: {error}"));
        let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        let store = store.as_ref();

        // B arrived second (5_000) and claims first.
        let (outcome, recorded) = claim(
            store,
            user.id,
            "req-b",
            FP,
            "pb-order",
            &incarnation(10),
            10_000,
            70_000,
            Some(&ask_at(&b, "v1;quality=b", 5_000)),
            backend,
        )
        .await;
        assert!(
            matches!(outcome, MediaSessionRequestClaim::Acquired { .. }),
            "{backend}"
        );
        let b_row = recorded.unwrap_or_else(|| panic!("{backend}: B records"));
        assert_eq!(
            (b_row.revision, b_row.digest.as_str()),
            (1, b.as_str()),
            "{backend}"
        );

        // A arrived first (4_000) and claims second: acquired, but its ask
        // replaces nothing and the answer is the row that stands.
        let (outcome, recorded) = claim(
            store,
            user.id,
            "req-a",
            FP,
            "pb-order",
            &incarnation(11),
            10_100,
            70_100,
            Some(&ask_at(&a, "v1;quality=a", 4_000)),
            backend,
        )
        .await;
        assert!(
            matches!(outcome, MediaSessionRequestClaim::Acquired { .. }),
            "{backend}"
        );
        assert_eq!(
            recorded,
            Some(b_row.clone()),
            "{backend}: the newer ask stands"
        );
        assert_eq!(
            desired(store, user.id, "pb-order").await,
            Some(b_row.clone()),
            "{backend}"
        );

        // A later arrival replaces.
        let (_, recorded) = claim(
            store,
            user.id,
            "req-a2",
            FP,
            "pb-order",
            &incarnation(12),
            10_200,
            70_200,
            Some(&ask_at(&a, "v1;quality=a", 6_000)),
            backend,
        )
        .await;
        let a_row = recorded.unwrap_or_else(|| panic!("{backend}: a later arrival records"));
        assert_eq!(
            (a_row.revision, a_row.digest.as_str()),
            (2, a.as_str()),
            "{backend}"
        );

        // An equal stamp replaces.
        let (_, recorded) = claim(
            store,
            user.id,
            "req-c",
            FP,
            "pb-order",
            &incarnation(13),
            10_300,
            70_300,
            Some(&ask_at(&c, "v1;quality=c", 6_000)),
            backend,
        )
        .await;
        let c_row = recorded.unwrap_or_else(|| panic!("{backend}: an equal stamp records"));
        assert_eq!(
            (c_row.revision, c_row.digest.as_str()),
            (3, c.as_str()),
            "{backend}"
        );

        // A re-acquired claim follows the same rule: its older arrival
        // replaces nothing.
        let (outcome, _) = claim(
            store,
            user.id,
            "req-r",
            FP,
            "pb-order",
            &incarnation(14),
            10_400,
            10_500,
            None,
            backend,
        )
        .await;
        assert!(
            matches!(outcome, MediaSessionRequestClaim::Acquired { .. }),
            "{backend}"
        );
        let (outcome, recorded) = claim(
            store,
            user.id,
            "req-r",
            FP,
            "pb-order",
            &incarnation(15),
            20_000,
            80_000,
            Some(&ask_at(&a, "v1;quality=a", 2_000)),
            backend,
        )
        .await;
        assert!(
            matches!(outcome, MediaSessionRequestClaim::Acquired { .. }),
            "{backend}: {outcome:?}"
        );
        assert_eq!(
            recorded,
            Some(c_row.clone()),
            "{backend}: a re-acquired older ask replaces nothing"
        );
        assert_eq!(
            desired(store, user.id, "pb-order").await,
            Some(c_row),
            "{backend}"
        );
    })
    .await;
}
