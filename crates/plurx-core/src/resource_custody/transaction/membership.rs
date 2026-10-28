//! Indexed lookup only. Original operation results remain replay authority.
use super::*;

struct EventKey {
    sequence: i64,
    kind: &'static str,
    asset_id: Uuid,
    obligation_id: Uuid,
    graph_id: Uuid,
    member_id: Option<Uuid>,
}

fn event_key(result: &DiskResult) -> Option<EventKey> {
    match result {
        DiskResult::AssetChangedV2(record) => Some(EventKey {
            sequence: record.ack.sequence,
            kind: "asset_v2",
            asset_id: record.change.asset_id.uuid(),
            obligation_id: record.ack.obligation_id,
            graph_id: record.ack.graph_id,
            member_id: Some(record.change.scratch_member_id.uuid()),
        }),
        DiskResult::AssetChangedV3(record) => Some(EventKey {
            sequence: record.ack.sequence,
            kind: "asset_v3",
            asset_id: record.change.asset_id.uuid(),
            obligation_id: record.ack.obligation_id,
            graph_id: record.ack.graph_id,
            member_id: Some(record.change.scratch_member_id.uuid()),
        }),
        DiskResult::RetiredWithAssets(ack) => Some(EventKey {
            sequence: ack.sequence,
            kind: "retirement",
            asset_id: ack.asset_id.uuid(),
            obligation_id: ack.source_obligation_id.uuid(),
            graph_id: ack.source_graph_id.uuid(),
            member_id: None,
        }),
        _ => None,
    }
}

fn result_sequence(result: &DiskResult) -> i64 {
    match result {
        DiskResult::Prepared { sequence, .. } => *sequence,
        DiskResult::Changed(ack) => ack.sequence,
        DiskResult::AssetChangedV2(record) => record.ack.sequence,
        DiskResult::AssetChangedV3(record) => record.ack.sequence,
        DiskResult::RetiredWithAssets(ack) => ack.sequence,
        DiskResult::RecoveredSAdoption(ack) | DiskResult::RecoveredRAdoption(ack) => ack.sequence,
    }
}

pub(super) fn backfill(tx: &Transaction<'_>) -> Result<(), CustodyError> {
    let mut statement =
        tx.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    let mut sequence = 0_i64;
    while let Some(row) = rows.next()? {
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        sequence = sequence.checked_add(1).ok_or_else(codec::invalid)?;
        if result_sequence(&result) != sequence {
            return Err(codec::invalid());
        }
        insert(tx, operation_id, &result)?;
    }
    let next: i64 = tx.query_row(
        "SELECT next_sequence FROM journal_meta WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if sequence != next {
        return Err(codec::invalid());
    }
    Ok(())
}

/// Complete projection/result equality is established before any indexed
/// scratch fold is reconstructed. This pass retains no historical event map.
pub(super) fn validate_all(connection: &Connection, next: i64) -> Result<(), CustodyError> {
    let mut statement =
        connection.prepare("SELECT operation_id,result_json FROM operations ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    let mut sequence = 0_i64;
    let mut projected = 0_i64;
    while let Some(row) = rows.next()? {
        let operation_id = codec::uuid_from_bytes(&row.get::<_, Vec<u8>>(0)?)?;
        let result: DiskResult = codec::decode(&row.get::<_, String>(1)?)?;
        sequence = sequence.checked_add(1).ok_or_else(codec::invalid)?;
        if result_sequence(&result) != sequence {
            return Err(codec::invalid());
        }
        if validate(connection, operation_id, &result)? {
            projected = projected.checked_add(1).ok_or_else(codec::invalid)?;
        }
    }
    if sequence != next {
        return Err(codec::invalid());
    }
    validate_count(connection, projected)
}

pub(super) fn insert(
    tx: &Transaction<'_>,
    operation_id: Uuid,
    result: &DiskResult,
) -> Result<(), CustodyError> {
    let Some(key) = event_key(result) else {
        return Ok(());
    };
    tx.execute(
        "INSERT INTO asset_operation_membership(operation_id,sequence,kind,asset_id,obligation_id,graph_id,member_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            operation_id.as_bytes(),
            key.sequence,
            key.kind,
            key.asset_id.as_bytes(),
            key.obligation_id.as_bytes(),
            key.graph_id.as_bytes(),
            key.member_id.map(|id| id.as_bytes().to_vec()),
        ],
    )?;
    Ok(())
}

/// Called for EVERY original operation in global append order. PK and total
/// count also reject an extra projection row pointing at an unrelated result.
pub(super) fn validate(
    connection: &Connection,
    operation_id: Uuid,
    result: &DiskResult,
) -> Result<bool, CustodyError> {
    type OperationMembershipRow = (i64, String, Vec<u8>, Vec<u8>, Vec<u8>, Option<Vec<u8>>);
    let row: Option<OperationMembershipRow> = connection
        .query_row(
            "SELECT sequence,kind,asset_id,obligation_id,graph_id,member_id FROM asset_operation_membership WHERE operation_id=?1",
            [operation_id.as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()?;
    match (event_key(result), row) {
        (None, None) => Ok(false),
        (Some(key), Some((sequence, kind, asset, obligation, graph, member))) => {
            if key.sequence == sequence
                && key.kind == kind
                && key.asset_id == codec::uuid_from_bytes(&asset)?
                && key.obligation_id == codec::uuid_from_bytes(&obligation)?
                && key.graph_id == codec::uuid_from_bytes(&graph)?
                && key.member_id == member.as_deref().map(codec::uuid_from_bytes).transpose()?
            {
                Ok(true)
            } else {
                Err(codec::invalid())
            }
        }
        _ => Err(codec::invalid()),
    }
}

pub(super) fn validate_count(connection: &Connection, counted: i64) -> Result<(), CustodyError> {
    let stored: i64 = connection.query_row(
        "SELECT count(*) FROM asset_operation_membership",
        [],
        |row| row.get(0),
    )?;
    if counted != stored {
        return Err(codec::invalid());
    }
    Ok(())
}

/// v1-v5 migration validation uses the original full stream. Recognized v6/v7
/// startup has already validated every projection row against result_json.
pub(super) fn indexed(connection: &Connection) -> Result<bool, CustodyError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    match version {
        1..=5 => Ok(false),
        6 | 7 => Ok(true),
        _ => Err(codec::invalid()),
    }
}

pub(super) fn history_sql(indexed: bool) -> &'static str {
    if indexed {
        "SELECT operations.operation_id,operations.result_json FROM asset_operation_membership AS membership JOIN operations ON operations.operation_id=membership.operation_id WHERE membership.asset_id=?1 ORDER BY membership.sequence"
    } else {
        "SELECT operation_id,result_json FROM operations ORDER BY rowid"
    }
}

pub(super) fn validate_asset_row(
    connection: &Connection,
    operation_id: Uuid,
    result: &DiskResult,
    asset_id: Uuid,
) -> Result<(), CustodyError> {
    if !validate(connection, operation_id, result)?
        || event_key(result)
            .as_ref()
            .is_none_or(|key| key.asset_id != asset_id)
    {
        return Err(codec::invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_custody::codec_v2::Id;

    #[test]
    fn indexed_membership_validates_more_than_1024_events_and_oldest_identity() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(crate::resource_custody::schema::DDL)
            .unwrap();
        let mut first = None;
        {
            let tx = connection.transaction().unwrap();
            for sequence in 1..=1025_i64 {
                let operation = Uuid::new_v4();
                let asset = Uuid::new_v4();
                let result = DiskResult::RetiredWithAssets(RetirementAck {
                    version: 2,
                    operation_id: Id::new(operation).unwrap(),
                    sequence,
                    source_obligation_id: Id::new(Uuid::new_v4()).unwrap(),
                    source_graph_id: Id::new(Uuid::new_v4()).unwrap(),
                    target_obligation_id: Id::new(Uuid::new_v4()).unwrap(),
                    target_graph_id: Id::new(Uuid::new_v4()).unwrap(),
                    asset_id: Id::new(asset).unwrap(),
                });
                tx.execute(
                    "INSERT INTO operations VALUES(?1,?2,?3)",
                    params![
                        operation.as_bytes(),
                        codec::encode(&"0".repeat(64)).unwrap(),
                        codec::encode(&result).unwrap()
                    ],
                )
                .unwrap();
                insert(&tx, operation, &result).unwrap();
                first.get_or_insert(operation);
            }
            tx.commit().unwrap();
        }
        validate_all(&connection, 1025).unwrap();
        let first = first.unwrap();
        let original_asset: Vec<u8> = connection
            .query_row(
                "SELECT asset_id FROM asset_operation_membership WHERE operation_id=?1",
                [first.as_bytes()],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE asset_operation_membership SET asset_id=?1 WHERE operation_id=?2",
                params![Uuid::new_v4().as_bytes(), first.as_bytes()],
            )
            .unwrap();
        assert!(validate_all(&connection, 1025).is_err());
        connection
            .execute(
                "UPDATE asset_operation_membership SET asset_id=?1 WHERE operation_id=?2",
                params![original_asset, first.as_bytes()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE asset_operation_membership SET kind='asset_v2',member_id=?1 WHERE operation_id=?2",
                params![Uuid::new_v4().as_bytes(), first.as_bytes()],
            )
            .unwrap();
        assert!(validate_all(&connection, 1025).is_err());
        connection
            .execute(
                "UPDATE asset_operation_membership SET kind='retirement',member_id=NULL,sequence=1026 WHERE operation_id=?1",
                [first.as_bytes()],
            )
            .unwrap();
        assert!(validate_all(&connection, 1025).is_err());
        connection
            .execute(
                "UPDATE asset_operation_membership SET sequence=1 WHERE operation_id=?1",
                [first.as_bytes()],
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM asset_operation_membership WHERE operation_id=?1",
                [first.as_bytes()],
            )
            .unwrap();
        assert!(validate_all(&connection, 1025).is_err());
    }

    #[test]
    fn extra_membership_for_non_event_result_refuses() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(crate::resource_custody::schema::DDL)
            .unwrap();
        let operation = Uuid::new_v4();
        let result = DiskResult::Prepared {
            sequence: 1,
            obligations: vec![Uuid::new_v4()],
            trial: None,
        };
        connection
            .execute(
                "INSERT INTO operations VALUES(?1,?2,?3)",
                params![
                    operation.as_bytes(),
                    codec::encode(&"0".repeat(64)).unwrap(),
                    codec::encode(&result).unwrap()
                ],
            )
            .unwrap();
        validate_all(&connection, 1).unwrap();
        connection
            .execute(
                "INSERT INTO asset_operation_membership VALUES(?1,1,'retirement',?2,?3,?4,NULL)",
                params![
                    operation.as_bytes(),
                    Uuid::new_v4().as_bytes(),
                    Uuid::new_v4().as_bytes(),
                    Uuid::new_v4().as_bytes()
                ],
            )
            .unwrap();
        assert!(validate_all(&connection, 1).is_err());
    }
}
