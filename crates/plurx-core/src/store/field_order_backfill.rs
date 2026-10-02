//! One bounded page of the stored-probe field-order backfill.
//!
//! The parsed probe owns the "probed, no field order" spelling
//! ([`crate::domain::FIELD_ORDER_UNKNOWN`]); this walk gives every probed row
//! whose column is still `NULL` the token the scanner would write for the
//! same retained document, through the exactly fenced update, and stamps the
//! pass complete once a page comes back empty. The daemon owns the lease and the node-local cursor;
//! everything that decides what a row receives lives here, so the store
//! contract exercises the production page on both backends.

use super::{keys, MissingFieldOrder, Store};
use crate::error::StoreError;

/// The three store operations one page uses. Every [`Store`] is one; the
/// seam exists so the page's own accounting (the fenced counter, and the
/// cursor stopping short of a row whose write failed) is provable without
/// arranging a concurrent rescan or a storage fault on a real backend.
#[async_trait::async_trait]
pub trait FieldOrderBackfillPort: Send + Sync {
    async fn pending_field_order(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingFieldOrder>, StoreError>;
    async fn write_field_order(
        &self,
        candidate: &MissingFieldOrder,
        token: &str,
    ) -> Result<bool, StoreError>;
    async fn stamp_field_order_backfill_done(&self) -> Result<(), StoreError>;
}

#[async_trait::async_trait]
impl<S: Store + ?Sized> FieldOrderBackfillPort for S {
    async fn pending_field_order(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingFieldOrder>, StoreError> {
        self.files_missing_field_order(after_id, limit).await
    }

    async fn write_field_order(
        &self,
        candidate: &MissingFieldOrder,
        token: &str,
    ) -> Result<bool, StoreError> {
        self.set_file_field_order(candidate, token).await
    }

    async fn stamp_field_order_backfill_done(&self) -> Result<(), StoreError> {
        self.put_setting(keys::JOB_FIELD_ORDER_BACKFILL_DONE, "1")
            .await
    }
}

/// What one page did. `cursor` is the highest id this page may be resumed
/// strictly after: on a write failure it stops just before the failed row so
/// the next tick retries it.
#[derive(Debug, Default)]
pub struct FieldOrderBackfillPage {
    pub updated: usize,
    pub fenced: usize,
    pub cursor: i64,
    pub complete: bool,
    pub write_error: Option<(i64, StoreError)>,
}

/// Walk at most `limit` probed `NULL` rows strictly after `cursor`.
///
/// An empty page stamps [`keys::JOB_FIELD_ORDER_BACKFILL_DONE`].
pub async fn field_order_backfill_page<S: FieldOrderBackfillPort + ?Sized>(
    store: &S,
    cursor: i64,
    limit: i64,
) -> Result<FieldOrderBackfillPage, StoreError> {
    let pending = store.pending_field_order(cursor, limit).await?;
    if pending.is_empty() {
        store.stamp_field_order_backfill_done().await?;
        return Ok(FieldOrderBackfillPage {
            cursor,
            complete: true,
            ..FieldOrderBackfillPage::default()
        });
    }
    let mut page = FieldOrderBackfillPage {
        cursor,
        ..FieldOrderBackfillPage::default()
    };
    for candidate in pending {
        page.cursor = page.cursor.max(candidate.id);
        let token = crate::scan::probe::field_order_from_stored_probe(&candidate.probe_json);
        match store.write_field_order(&candidate, &token).await {
            Ok(true) => page.updated += 1,
            Ok(false) => page.fenced += 1,
            Err(error) => {
                page.cursor = page.cursor.min(candidate.id.saturating_sub(1));
                page.write_error = Some((candidate.id, error));
                break;
            }
        }
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Scripted port: the rows a page lists, and what each write answers.
    struct Scripted {
        rows: Vec<MissingFieldOrder>,
        answers: Mutex<Vec<Result<bool, StoreError>>>,
        writes: Mutex<Vec<(i64, String)>>,
        stamped: Mutex<bool>,
    }

    impl Scripted {
        fn new(rows: &[(i64, &str)], answers: Vec<Result<bool, StoreError>>) -> Self {
            Self {
                rows: rows
                    .iter()
                    .map(|(id, probe)| MissingFieldOrder {
                        id: *id,
                        path: format!("/media/{id}.mkv"),
                        size: 1,
                        mtime: 1,
                        probe_json: (*probe).to_owned(),
                    })
                    .collect(),
                answers: Mutex::new(answers.into_iter().rev().collect()),
                writes: Mutex::new(Vec::new()),
                stamped: Mutex::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl FieldOrderBackfillPort for Scripted {
        async fn pending_field_order(
            &self,
            after_id: i64,
            limit: i64,
        ) -> Result<Vec<MissingFieldOrder>, StoreError> {
            Ok(self
                .rows
                .iter()
                .filter(|row| row.id > after_id)
                .take(usize::try_from(limit).unwrap_or(0))
                .cloned()
                .collect())
        }

        async fn write_field_order(
            &self,
            candidate: &MissingFieldOrder,
            token: &str,
        ) -> Result<bool, StoreError> {
            self.writes
                .lock()
                .expect("scripted port lock")
                .push((candidate.id, token.to_owned()));
            self.answers
                .lock()
                .expect("scripted port lock")
                .pop()
                .expect("scripted answer")
        }

        async fn stamp_field_order_backfill_done(&self) -> Result<(), StoreError> {
            *self.stamped.lock().expect("scripted port lock") = true;
            Ok(())
        }
    }

    const HEVC: &str = r#"{"streams":[{"codec_type":"video","codec_name":"hevc"}]}"#;
    const TFF: &str = r#"{"streams":[{"codec_type":"video","field_order":"tt"}]}"#;

    #[tokio::test]
    async fn a_fenced_write_is_counted_and_the_walk_continues_past_it() {
        let port = Scripted::new(
            &[(3, HEVC), (5, TFF), (9, HEVC)],
            vec![Ok(true), Ok(false), Ok(true)],
        );
        let page = field_order_backfill_page(&port, 0, 256)
            .await
            .expect("page");
        assert_eq!((page.updated, page.fenced), (2, 1));
        assert_eq!(page.cursor, 9);
        assert!(!page.complete);
        assert!(page.write_error.is_none());
        assert_eq!(
            *port.writes.lock().expect("scripted port lock"),
            vec![
                (3, "unknown".to_owned()),
                (5, "tt".to_owned()),
                (9, "unknown".to_owned())
            ]
        );
        assert!(!*port.stamped.lock().expect("scripted port lock"));
    }

    #[tokio::test]
    async fn a_failed_write_stops_the_cursor_just_before_that_row() {
        let port = Scripted::new(
            &[(3, HEVC), (5, TFF), (9, HEVC)],
            vec![Ok(true), Err(StoreError::Database("disk".to_owned()))],
        );
        let page = field_order_backfill_page(&port, 0, 256)
            .await
            .expect("page");
        assert_eq!((page.updated, page.fenced), (1, 0));
        // Resumed strictly after 4, the next tick retries row 5 first.
        assert_eq!(page.cursor, 4);
        assert_eq!(page.write_error.as_ref().map(|(id, _)| *id), Some(5));
        assert!(!page.complete);
        assert_eq!(
            port.writes.lock().expect("scripted port lock").len(),
            2,
            "row 9 is not attempted after the failure"
        );
        let resumed = port.rows.iter().filter(|row| row.id > page.cursor).count();
        assert_eq!(resumed, 2, "rows 5 and 9 remain reachable");
    }

    #[tokio::test]
    async fn an_empty_page_stamps_the_pass_and_keeps_the_cursor() {
        let port = Scripted::new(&[(3, HEVC)], vec![]);
        let page = field_order_backfill_page(&port, 3, 256)
            .await
            .expect("page");
        assert!(page.complete);
        assert_eq!(page.cursor, 3);
        assert!(*port.stamped.lock().expect("scripted port lock"));
        assert!(port.writes.lock().expect("scripted port lock").is_empty());
    }
}
