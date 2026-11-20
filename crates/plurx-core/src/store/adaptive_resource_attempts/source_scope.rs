//! Source-domain authority projected from the original adaptive attempt.
use super::{invalid, schema::quote, AdaptiveSourceScope};
use crate::error::StoreError;

pub(crate) const CATALOG_SQL: &str = "SELECT name,sql FROM sqlite_master WHERE name GLOB 'adaptive_source_scope_*' AND sql IS NOT NULL ORDER BY name";

pub(crate) fn objects() -> Vec<(String, String)> {
    let mut objects = include_str!("source_scope.sql")
        .split("-- next statement")
        .map(|sql| sql.trim().trim_end_matches(';'))
        .map(|sql| {
            let name = sql.split_whitespace().nth(2).expect("static SQL object");
            (name.to_owned(), sql.to_owned())
        })
        .collect::<Vec<_>>();
    objects.sort_by(|left, right| left.0.cmp(&right.0));
    objects
}

pub(crate) fn catalog_is_exact(rows: &[(String, String)]) -> bool {
    rows == objects()
}

pub(crate) fn ready_guard() -> String {
    let objects = objects();
    let count = objects.len();
    let exact = objects
        .into_iter()
        .map(|(name, sql)| {
            format!(
                "EXISTS(SELECT 1 FROM sqlite_master WHERE name={} AND sql={})",
                quote(&name),
                quote(&sql)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    format!("(SELECT count(*) FROM ({CATALOG_SQL}))={count} AND {exact}")
}

pub(crate) fn install_sqlite(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    let rows = {
        let mut stmt = tx.prepare(CATALOG_SQL)?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if rows.is_empty() {
        for (_, sql) in objects() {
            tx.execute_batch(&sql)?;
        }
    } else if !catalog_is_exact(&rows) {
        return Err(StoreError::Migration(
            "adaptive source authority catalogue is partial or incompatible".into(),
        ));
    }
    tx.commit()?;
    Ok(())
}

/// Existing legacy source arbitration projects every root of the file's
/// library, including the bare wildcard for any unmapped root.
fn projection(file_id: i64, size: i64, mtime: i64, generation: i64) -> String {
    format!(
        r#"SELECT json_object(
 'file_id',file.id,'source_size',file.size,'source_mtime',file.mtime,'source_path',file.path,
 'library_id',library.id,'mapping_revision',COALESCE((
   SELECT revision FROM background_storage_domain_pending WHERE singleton=1),0),
 'resource_keys',json((SELECT json_group_array(resource_key) FROM (
   SELECT DISTINCT COALESCE('source_io:'||mapping.domain_id,'source_io') AS resource_key
   FROM (SELECT library.paths AS paths) roots
   LEFT JOIN json_each(CASE WHEN json_valid(roots.paths) THEN roots.paths ELSE '[]' END) root
   LEFT JOIN background_storage_domains mapping
     ON mapping.library_id=library.id AND mapping.root_path=root.value
   ORDER BY resource_key
 )))) AS payload
FROM files file JOIN items item ON item.id=file.item_id
JOIN libraries library ON library.id=item.library_id
WHERE file.id={file_id} AND file.size={size} AND file.mtime={mtime}
  AND EXISTS(SELECT 1 FROM playback_input_generation WHERE singleton=1 AND generation={generation})"#
    )
}

pub(crate) async fn read<B: super::transactions::Backend>(
    store: &B,
    file_id: i64,
    size: i64,
    mtime: i64,
    generation: i64,
) -> Result<Option<AdaptiveSourceScope>, StoreError> {
    if file_id <= 0 || size < 0 || mtime < 0 || generation < 0 {
        return Err(invalid());
    }
    let rows = store
        .attempt_read(format!(
            "{} AND ({})",
            projection(file_id, size, mtime, generation),
            ready_guard()
        ))
        .await?;
    let [row] = rows.as_slice() else {
        return Ok(None);
    };
    if row.len() > 8192 {
        return Ok(None);
    }
    let scope: AdaptiveSourceScope = serde_json::from_str(row).map_err(|_| invalid())?;
    scope.validate()?;
    Ok(Some(scope))
}

pub(super) fn claim_guard(
    scope: &AdaptiveSourceScope,
    generation: i64,
) -> Result<String, StoreError> {
    let projection = projection(
        scope.file_id,
        scope.source_size,
        scope.source_mtime,
        generation,
    );
    let scope_json = quote(&serde_json::to_string(scope).map_err(|_| invalid())?);
    Ok(format!(
        "({}) AND EXISTS(SELECT 1 FROM ({projection}) current WHERE current.payload={scope_json})",
        ready_guard()
    ))
}

pub(super) fn prepared_guard(
    scope: &AdaptiveSourceScope,
    generation: i64,
) -> Result<String, StoreError> {
    let projection = projection(
        scope.file_id,
        scope.source_size,
        scope.source_mtime,
        generation,
    );
    let scope = quote(&serde_json::to_string(scope).map_err(|_| invalid())?);
    // An unrelated save can advance the pending revision while this original
    // attempt holds its mapping. Relevant transitions cannot commit.
    Ok(format!(
        "({}) AND EXISTS(SELECT 1 FROM ({projection}) current WHERE json_remove(current.payload,'$.mapping_revision')=json_remove({scope},'$.mapping_revision'))",
        ready_guard()
    ))
}

pub(super) fn held_source_matches(
    scope: Option<&AdaptiveSourceScope>,
    version: Option<&str>,
) -> bool {
    let (Some(scope), Some(version)) = (scope, version) else {
        return scope.is_none() && version.is_none();
    };
    if version.len() > super::MAX_HELD_SOURCE_VERSION_BYTES {
        return false;
    }
    let mut fields = version.split(':');
    if fields.next() != Some("s1") {
        return false;
    }
    let [Some(dev), Some(ino), Some(size), Some(mtime), Some(mtime_ns), Some(ctime), Some(ctime_ns)] =
        std::array::from_fn(|_| fields.next())
    else {
        return false;
    };
    if fields.next().is_some() {
        return false;
    }
    let (Ok(dev), Ok(ino), Ok(size), Ok(mtime), Ok(mtime_ns), Ok(ctime), Ok(ctime_ns)) = (
        dev.parse::<u64>(),
        ino.parse::<u64>(),
        size.parse::<u64>(),
        mtime.parse::<i64>(),
        mtime_ns.parse::<i64>(),
        ctime.parse::<i64>(),
        ctime_ns.parse::<i64>(),
    ) else {
        return false;
    };
    size == scope.source_size as u64
        && mtime.max(0) == scope.source_mtime
        && (0..1_000_000_000).contains(&mtime_ns)
        && (0..1_000_000_000).contains(&ctime_ns)
        && crate::source_identity::SourceIdentity::from_fields(
            dev, ino, size, mtime, mtime_ns, ctime, ctime_ns,
        )
        .render_s1()
            == version
}
