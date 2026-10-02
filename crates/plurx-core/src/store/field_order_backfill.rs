//! One bounded page of the stored-probe field-order backfill.
//!
//! The parsed probe owns the "probed, no field order" spelling
//! ([`crate::domain::FIELD_ORDER_UNKNOWN`]); this walk gives every probed row
//! whose column is still `NULL` the token the scanner would write for the
//! same retained document, through the exactly fenced update, and stamps the
//! pass complete once a page comes back empty. The daemon owns the lease and the node-local cursor;
//! everything that decides what a row receives lives here, so the store
//! contract exercises the production page on both backends.

use super::{keys, Store};
use crate::error::StoreError;

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
pub async fn field_order_backfill_page(
    store: &dyn Store,
    cursor: i64,
    limit: i64,
) -> Result<FieldOrderBackfillPage, StoreError> {
    let pending = store.files_missing_field_order(cursor, limit).await?;
    if pending.is_empty() {
        store
            .put_setting(keys::JOB_FIELD_ORDER_BACKFILL_DONE, "1")
            .await?;
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
        match store.set_file_field_order(&candidate, &token).await {
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
