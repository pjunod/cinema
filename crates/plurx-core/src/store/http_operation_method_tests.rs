//! The per-request split of Store operations by the Store method that issued
//! them (D3a, `docs/streaming/PLAYBACK-START-D3-BUILD.md` section 7.1-2).

use super::*;
use std::time::Duration;

const LOCAL: usize = 0;
const AUTHORITY: usize = 1;
const WRITE: usize = 2;

#[tokio::test]
async fn operations_are_split_by_method_and_the_split_sums_to_the_totals() {
    let counts = HttpStoreOperationCounts::default();
    scope_http_store_operations(counts.clone(), async {
        for (class, method) in [
            (AUTHORITY, "get_settings"),
            (AUTHORITY, "get_settings"),
            (LOCAL, "get_settings"),
            (WRITE, "claim_media_session_request"),
            (AUTHORITY, "claim_media_session_request"),
        ] {
            record_http_store_operation(class, method, Duration::from_millis(1));
        }
    })
    .await;
    assert_eq!(counts.snapshot(), [1, 3, 1]);
    assert_eq!(
        counts.method_snapshot(),
        vec![
            ("claim_media_session_request", [0, 1, 1]),
            ("get_settings", [1, 2, 0]),
        ]
    );
    let sum = counts
        .method_snapshot()
        .iter()
        .fold([0_u64; 3], |mut sum, (_, by_class)| {
            for (total, count) in sum.iter_mut().zip(by_class) {
                *total += count;
            }
            sum
        });
    assert_eq!(sum, counts.snapshot());
}

#[tokio::test]
async fn a_window_reports_only_what_ran_inside_it() {
    let counts = HttpStoreOperationCounts::default();
    scope_http_store_operations(counts.clone(), async {
        record_http_store_operation(AUTHORITY, "get_user", Duration::ZERO);
        let before = counts.method_snapshot();
        record_http_store_operation(AUTHORITY, "get_user", Duration::ZERO);
        record_http_store_operation(WRITE, "put_settings", Duration::ZERO);
        let window = counts.method_snapshot_since(&before);
        assert_eq!(
            window,
            vec![("get_user", [0, 1, 0]), ("put_settings", [0, 0, 1])]
        );
        assert_eq!(
            format_method_counts(&window),
            "get_user=1/0/0,put_settings=0/0/1"
        );
        // Nothing ran since: nothing is listed.
        assert!(counts
            .method_snapshot_since(&counts.method_snapshot())
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_request_folds_methods_past_the_cap_into_one_overflow_label() {
    let counts = HttpStoreOperationCounts::default();
    let labels: Vec<&'static str> = (0..MAX_METHODS_PER_REQUEST + 40)
        .map(|index| &*Box::leak(format!("method_{index:03}").into_boxed_str()))
        .collect();
    scope_http_store_operations(counts.clone(), async {
        for label in &labels {
            record_http_store_operation(AUTHORITY, label, Duration::ZERO);
        }
        // A method already in the map keeps counting under its own name.
        record_http_store_operation(AUTHORITY, labels[0], Duration::ZERO);
    })
    .await;
    let snapshot = counts.method_snapshot();
    assert_eq!(
        snapshot.len(),
        MAX_METHODS_PER_REQUEST + 1,
        "cap plus overflow"
    );
    assert_eq!(
        snapshot
            .iter()
            .find(|(method, _)| *method == METHOD_OVERFLOW_LABEL)
            .map(|(_, counts)| counts[AUTHORITY]),
        Some(40)
    );
    assert_eq!(
        snapshot
            .iter()
            .find(|(method, _)| *method == labels[0])
            .map(|(_, counts)| counts[AUTHORITY]),
        Some(2)
    );
    // Folding loses no operation.
    let total: u64 = snapshot.iter().map(|(_, counts)| counts[AUTHORITY]).sum();
    assert_eq!(total, counts.snapshot()[AUTHORITY]);
}
