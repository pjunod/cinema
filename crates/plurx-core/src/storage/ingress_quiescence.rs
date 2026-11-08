//! Conservative backend-transition refusal for logical Source owners and
//! durable accepted-driver custody. These checks do not manufacture physical
//! closure receipts, settle logical owners or authorize cleanup.
use crate::{error::StoreError, sharing_ingress_custody::IngressCustodyState};

pub fn require_quiescent_custody_json(json: &str) -> Result<(), StoreError> {
    let state = IngressCustodyState::decode(json)?;
    if state.open().next().is_some() {
        return Err(StoreError::Migration("backend transition requires settled Source and receiver ingress custody; original accepted-driver owners must close outstanding registrations".into()));
    }
    Ok(())
}

pub fn require_quiescent_custody(connection: &rusqlite::Connection) -> Result<(), StoreError> {
    let present:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sharing_ingress_custody')",[],|row|row.get(0))?;
    if present {
        let mut statement =
            connection.prepare("SELECT custody_json FROM sharing_ingress_custody")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            require_quiescent_custody_json(&row?)?;
        }
    }
    Ok(())
}

/// Logical Source ownership must be retired by its real owners before a
/// backend crossing, independently of physical ingress closure. Ordinary
/// local playback history and local sessions do not acquire this refusal.
pub fn require_quiescent_source(connection: &rusqlite::Connection) -> Result<(), StoreError> {
    for (table, predicate, principal_column) in [
        (
            "sharing_source_session_bindings",
            "reservation_state='held'",
            false,
        ),
        ("sharing_source_dispatch_guard", "1", false),
        (
            "media_session_requests",
            "principal_kind='sharing' AND state='starting'",
            true,
        ),
        (
            "media_sessions",
            "principal_kind='sharing' AND state!='ended'",
            true,
        ),
        (
            "media_session_preparations",
            "principal_kind='sharing'",
            true,
        ),
    ] {
        let present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if !present {
            continue;
        }
        if principal_column {
            let principal: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name='principal_kind')",
                [table],
                |row| row.get(0),
            )?;
            if !principal {
                continue;
            }
        }
        let active: bool = connection.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {predicate})"),
            [],
            |row| row.get(0),
        )?;
        if active {
            return Err(StoreError::Migration(format!("backend transition requires drained logical Source authority: {table}; original owners must retire outstanding work")));
        }
    }
    Ok(())
}

pub fn require_quiescent_backend_transition(
    connection: &rusqlite::Connection,
) -> Result<(), StoreError> {
    require_quiescent_source(connection)?;
    require_quiescent_custody(connection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharing_ingress_custody::{CustodyMutation, IngressRegistration};
    #[test]
    fn logical_source_owners_refuse_without_custody_and_ended_history_is_retained() {
        let connection = rusqlite::Connection::open_in_memory().expect("source drain fixture");
        connection.execute_batch("CREATE TABLE sharing_source_session_bindings(reservation_state TEXT); INSERT INTO sharing_source_session_bindings VALUES('released');
            CREATE TABLE sharing_source_dispatch_guard(passed INTEGER);
            CREATE TABLE media_session_requests(principal_kind TEXT,state TEXT); INSERT INTO media_session_requests VALUES('local','starting'),('sharing','resolved');
            CREATE TABLE media_sessions(principal_kind TEXT,state TEXT); INSERT INTO media_sessions VALUES('local','active'),('sharing','ended');
            CREATE TABLE media_session_preparations(principal_kind TEXT); INSERT INTO media_session_preparations VALUES('local');").expect("source history");
        require_quiescent_backend_transition(&connection)
            .expect("ended Source and ordinary local work remain usable");
        for (begin, settle) in [
            (
                "UPDATE sharing_source_session_bindings SET reservation_state='held'",
                "UPDATE sharing_source_session_bindings SET reservation_state='released'",
            ),
            (
                "UPDATE media_session_requests SET state='starting' WHERE principal_kind='sharing'",
                "UPDATE media_session_requests SET state='resolved' WHERE principal_kind='sharing'",
            ),
            (
                "UPDATE media_sessions SET state='active' WHERE principal_kind='sharing'",
                "UPDATE media_sessions SET state='ended' WHERE principal_kind='sharing'",
            ),
            (
                "INSERT INTO media_session_preparations VALUES('sharing')",
                "DELETE FROM media_session_preparations WHERE principal_kind='sharing'",
            ),
            (
                "INSERT INTO sharing_source_dispatch_guard VALUES(1)",
                "DELETE FROM sharing_source_dispatch_guard",
            ),
        ] {
            connection
                .execute_batch(begin)
                .expect("outstanding logical Source owner");
            assert!(
                require_quiescent_backend_transition(&connection).is_err(),
                "no custody table does not settle logical Source ownership"
            );
            connection
                .execute_batch(settle)
                .expect("actual logical owner retirement fixture");
            require_quiescent_backend_transition(&connection).expect("retired logical owner");
        }
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM media_sessions", [], |row| row
                    .get::<_, i64>(0))
                .expect("retained history"),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM media_sessions WHERE principal_kind='sharing'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("ended Source history"),
            "ended"
        );
    }

    #[test]
    fn ended_logical_sessions_cannot_retire_open_source_or_receiver_custody() {
        for principal in ["source", "receiver"] {
            let connection = rusqlite::Connection::open_in_memory().expect("custody fixture");
            connection.execute_batch("CREATE TABLE media_sessions(state TEXT); INSERT INTO media_sessions VALUES('ended'); CREATE TABLE sharing_source_session_bindings(reservation_state TEXT); INSERT INTO sharing_source_session_bindings VALUES('released'); CREATE TABLE sharing_ingress_custody(principal_kind TEXT,custody_json TEXT);").expect("custody fixture");
            let mut custody = IngressCustodyState::default();
            let registration = IngressRegistration {
                node_id: "owned-node".into(),
                boot_id: uuid::Uuid::new_v4(),
                connection_id: uuid::Uuid::new_v4(),
                driver_sequence: 1,
                registration_sequence: 1,
                closed_confirmation: None,
            };
            assert_eq!(
                custody.register(registration.clone()),
                CustodyMutation::Applied
            );
            connection
                .execute(
                    "INSERT INTO sharing_ingress_custody VALUES(?1,?2)",
                    rusqlite::params![principal, custody.encode().expect("custody fixture")],
                )
                .expect("custody fixture");
            assert!(
                require_quiescent_custody(&connection).is_err(),
                "{principal} actual accepted-driver obligations outlive logical End"
            );
            assert_eq!(
                custody.acknowledge(&registration, &"a".repeat(64)),
                CustodyMutation::Applied
            );
            connection
                .execute(
                    "UPDATE sharing_ingress_custody SET custody_json=?1",
                    [custody.encode().expect("custody fixture")],
                )
                .expect("custody fixture");
            require_quiescent_custody(&connection)
                .expect("existing exact closure metadata is retained");
        }
        assert!(require_quiescent_custody_json("{\"version\":99}").is_err());
    }
}
