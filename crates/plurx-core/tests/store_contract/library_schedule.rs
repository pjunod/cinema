//! A new library and its automatic scan interval are one write on every
//! backend (pipeline contract v2 §5.2): the API creates libraries at the
//! six-hour default without a second, separately failing schedule write,
//! while a library created the old way keeps the column default of `0`.
use super::*;
use plurx_core::domain::DEFAULT_SCAN_INTERVAL_MINS;

#[tokio::test]
async fn a_library_and_its_scan_interval_are_one_write_on_every_backend() {
    for_each_backend(|store, backend| async move {
        let new = |name: &str| NewLibrary {
            name: format!("{name} {backend}"),
            kind: LibraryKind::Movies,
            paths: vec![format!("/schedule/{name}-{backend}").into()],
            anime: false,
        };
        let scheduled = store
            .create_library_with_scan_interval(&new("Scheduled"), DEFAULT_SCAN_INTERVAL_MINS)
            .await
            .expect("scheduled library");
        assert_eq!(scheduled.scan_interval_mins, 360, "{backend}");
        assert_eq!(scheduled.refresh_interval_mins, 0, "{backend}");
        let never = store
            .create_library_with_scan_interval(&new("Never"), 0)
            .await
            .expect("explicit never");
        assert_eq!(never.scan_interval_mins, 0, "{backend}");
        let existing = store
            .create_library(&new("Existing"))
            .await
            .expect("old path");
        assert_eq!(
            existing.scan_interval_mins, 0,
            "the column default is untouched: {backend}"
        );
        let edited = store
            .update_library(existing.id, &new("Existing renamed"))
            .await
            .expect("edit")
            .expect("library");
        assert_eq!(
            edited.scan_interval_mins, 0,
            "an edit never opts in: {backend}"
        );
        let listed = store.list_libraries().await.expect("list");
        let interval = |id: i64| {
            listed
                .iter()
                .find(|library| library.id == id)
                .expect("listed")
                .scan_interval_mins
        };
        assert_eq!(
            (
                interval(scheduled.id),
                interval(never.id),
                interval(existing.id)
            ),
            (360, 0, 0),
            "{backend}"
        );
        // A duplicate name still fails as one write: nothing half-made.
        assert!(store
            .create_library_with_scan_interval(&new("Scheduled"), DEFAULT_SCAN_INTERVAL_MINS)
            .await
            .is_err());
        assert_eq!(
            store.list_libraries().await.expect("list").len(),
            listed.len()
        );
    })
    .await;
}
