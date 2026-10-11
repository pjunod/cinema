//! Current Source artwork selection. Internal names are never wire or diagnostic
//! data. Candidate catalogue layout/floor installation remains coordinated.
use super::{sharing::Backend, sharing_catalogue_source::ready};
use crate::{
    error::StoreError,
    sharing::{invalid, is_hash},
    sharing_artwork::{ArtKind, SourceArtReference},
};
use async_trait::async_trait;
use serde::Deserialize;
use uuid::Uuid;

pub struct SourceArtSnapshot {
    reference: SourceArtReference,
    filename: String,
    identity: crate::sharing::SharingIdentity,
}
impl SourceArtSnapshot {
    pub fn reference(&self) -> &SourceArtReference {
        &self.reference
    }
    pub fn identity(&self) -> &crate::sharing::SharingIdentity {
        &self.identity
    }
    /// Private server resolution only; never place this value in a response,
    /// cache key, error, URL, metrics label or diagnostic formatter.
    pub fn filename(&self) -> &str {
        &self.filename
    }
    pub fn same_selection(&self, other: &Self) -> bool {
        self.reference == other.reference && self.filename == other.filename
    }
}
pub enum SourceArtRead {
    Authorized(SourceArtSnapshot),
    Unavailable,
    Capacity,
}
#[async_trait]
pub trait SharingSourceArtworkStore: Send + Sync {
    async fn source_art_snapshot(
        &self,
        hash: &str,
        grant: Uuid,
        reference: &SourceArtReference,
    ) -> Result<SourceArtRead, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingSourceArtworkStore for T {
    async fn source_art_snapshot(
        &self,
        hash: &str,
        grant: Uuid,
        reference: &SourceArtReference,
    ) -> Result<SourceArtRead, StoreError> {
        if !is_hash(hash)
            || grant.is_nil()
            || reference.grant_id != grant
            || reference.server_id.is_nil()
            || reference.catalogue_epoch.is_nil()
        {
            return Err(invalid());
        }
        ready(self).await?;
        // Only a closed enum chooses a persisted column. No caller expression,
        // filename or path is interpolated. Size refusal precedes construction.
        let column = match reference.kind {
            ArtKind::Poster => "i.poster_path",
            ArtKind::Backdrop => "i.backdrop_path",
        };
        let sql = format!("SELECT CASE WHEN length(CAST({column} AS BLOB))<=256 THEN json_object('status','authorized','filename',{column},'created_at_ms',s.created_at_ms) ELSE json_object('status','capacity') END AS payload FROM sharing_exports e JOIN sharing_export_libraries scope ON scope.grant_id=e.id JOIN libraries l ON l.id=scope.library_id JOIN items i ON i.library_id=l.id JOIN sharing_identity s ON s.singleton=1 WHERE e.id=$1 AND e.token_hash=$2 AND e.state='active' AND s.server_id=$3 AND s.catalogue_epoch=$4 AND i.library_id=$5 AND i.id=$6 AND l.kind IN ('movies','shows') AND i.kind IN ('movie','show','season','episode') AND {column} IS NOT NULL AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0)");
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    grant.into(),
                    hash.to_owned().into(),
                    reference.server_id.into(),
                    reference.catalogue_epoch.into(),
                    reference
                        .library_id
                        .as_str()
                        .parse::<i64>()
                        .map_err(|_| invalid())?
                        .into(),
                    reference
                        .item_id
                        .as_str()
                        .parse::<i64>()
                        .map_err(|_| invalid())?
                        .into(),
                ],
            )
            .await?;
        if rows.len() > 1 {
            return Err(invalid());
        }
        let Some(row) = rows.first() else {
            return Ok(SourceArtRead::Unavailable);
        };
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Row {
            status: String,
            filename: Option<String>,
            created_at_ms: Option<i64>,
        }
        let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
        match (row.status.as_str(), row.filename, row.created_at_ms) {
            ("capacity", None, None) => Ok(SourceArtRead::Capacity),
            ("authorized", Some(filename), Some(created_at_ms)) => {
                if created_at_ms <= 0
                    || filename.is_empty()
                    || filename.starts_with('.')
                    || !filename
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                {
                    return Err(invalid());
                }
                Ok(SourceArtRead::Authorized(SourceArtSnapshot {
                    reference: reference.clone(),
                    filename,
                    identity: crate::sharing::SharingIdentity {
                        server_id: reference.server_id,
                        catalogue_epoch: reference.catalogue_epoch,
                        created_at_ms,
                    },
                }))
            }
            _ => Err(invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        sharing::*,
        sharing_artwork::{artwork_expiry, ArtVariant},
        store::{LibraryStore, MediaStore, SharingStore, SqliteStore},
    };
    #[tokio::test]
    async fn sharing_art_snapshot_refuses_wrong_scope_moves_and_private_names() {
        let directory = tempfile::tempdir().expect("art fixtures");
        for store in [
            SqliteStore::open_in_memory().expect("memory"),
            SqliteStore::open(&directory.path().join("art.sqlite")).expect("pooled"),
        ] {
            let identity = store.sharing_identity(1000).await.expect("Source identity");
            let mut libraries = Vec::new();
            for name in ["Shared", "Private"] {
                libraries.push(
                    store
                        .create_library(&NewLibrary {
                            name: name.into(),
                            kind: LibraryKind::Movies,
                            paths: vec!["/synthetic".into()],
                            anime: false,
                        })
                        .await
                        .expect("library")
                        .id,
                );
            }
            let grant = Uuid::new_v4();
            let invitation = Uuid::new_v4();
            store
                .create_share_invitation(InvitationRecord {
                    id: invitation,
                    token_hash: "a".repeat(64),
                    library_ids: vec![libraries[0]],
                    created_at_ms: 1000,
                    expires_at_ms: 2000,
                })
                .await
                .expect("invitation");
            store
                .claim_share(ShareClaim {
                    invitation_id: invitation,
                    invitation_hash: "a".repeat(64),
                    claim_id: Uuid::new_v4(),
                    grant_id: grant,
                    recipient_server_id: Uuid::new_v4(),
                    recipient_name: "Fixture B".into(),
                    credential_hash: "b".repeat(64),
                    now_ms: 1001,
                })
                .await
                .expect("claim");
            store.approve_share(grant, 1, 1002).await.expect("approval");
            let statements =
                super::super::sharing_catalogue_source::candidate_item_identity_statements()
                    .into_iter()
                    .chain(super::super::sharing_catalogue_source::candidate_statements())
                    .map(|sql| (sql, vec![]))
                    .collect();
            store
                .sharing_txn(statements)
                .await
                .expect("candidate-only layout fixture");
            let item = store
                .insert_item(&NewItem {
                    library_id: libraries[0],
                    kind: ItemKind::Movie,
                    parent_id: None,
                    title: "Scoped art".into(),
                    year: None,
                    season_number: None,
                    episode_number: None,
                })
                .await
                .expect("item");
            store.sharing_txn(vec![("UPDATE items SET poster_path='scoped-poster.jpg',backdrop_path='scoped-backdrop.webp' WHERE id=$1".into(),vec![item.into()])]).await.expect("art selection");
            let reference = SourceArtReference {
                server_id: identity.server_id,
                catalogue_epoch: identity.catalogue_epoch,
                grant_id: grant,
                library_id: SourceId::parse(&libraries[0].to_string()).expect("library"),
                item_id: SourceId::parse(&item.to_string()).expect("item"),
                kind: ArtKind::Poster,
                variant: ArtVariant::Original,
                expires_at_ms: artwork_expiry(1_700_000_000_001).expect("expiry"),
            };
            let SourceArtRead::Authorized(first) = store
                .source_art_snapshot(&"b".repeat(64), grant, &reference)
                .await
                .expect("selected snapshot")
            else {
                panic!("authorized art");
            };
            assert_eq!(first.filename(), "scoped-poster.jpg");
            assert!(matches!(
                store
                    .source_art_snapshot(&"c".repeat(64), grant, &reference)
                    .await
                    .expect("foreign credential"),
                SourceArtRead::Unavailable
            ));
            let mut wrong = reference.clone();
            wrong.catalogue_epoch = Uuid::new_v4();
            assert!(matches!(
                store
                    .source_art_snapshot(&"b".repeat(64), grant, &wrong)
                    .await
                    .expect("foreign epoch"),
                SourceArtRead::Unavailable
            ));
            wrong = reference.clone();
            wrong.kind = ArtKind::Backdrop;
            let SourceArtRead::Authorized(backdrop) = store
                .source_art_snapshot(&"b".repeat(64), grant, &wrong)
                .await
                .expect("backdrop")
            else {
                panic!("backdrop snapshot");
            };
            assert_eq!(backdrop.filename(), "scoped-backdrop.webp");
            assert!(!first.same_selection(&backdrop));
            for invalid_name in [
                "../private.jpg".to_owned(),
                "https://private.invalid/a.jpg".to_owned(),
                "subdir/a.jpg".to_owned(),
                "a\\b.jpg".to_owned(),
                "x".repeat(257),
            ] {
                store
                    .sharing_txn(vec![(
                        "UPDATE items SET poster_path=$2 WHERE id=$1".into(),
                        vec![item.into(), invalid_name.clone().into()],
                    )])
                    .await
                    .expect("invalid private selection fixture");
                let result = store
                    .source_art_snapshot(&"b".repeat(64), grant, &reference)
                    .await;
                if invalid_name.len() > 256 {
                    assert!(matches!(
                        result.expect("bounded refusal"),
                        SourceArtRead::Capacity
                    ));
                } else {
                    assert!(result.is_err());
                }
            }
            store
                .sharing_txn(vec![(
                    "UPDATE items SET poster_path='current.jpg',library_id=$2 WHERE id=$1".into(),
                    vec![item.into(), libraries[1].into()],
                )])
                .await
                .expect("item move");
            assert!(matches!(
                store
                    .source_art_snapshot(&"b".repeat(64), grant, &reference)
                    .await
                    .expect("moved item"),
                SourceArtRead::Unavailable
            ));
            store
                .sharing_txn(vec![
                    (
                        "UPDATE items SET library_id=$2 WHERE id=$1".into(),
                        vec![item.into(), libraries[0].into()],
                    ),
                    (
                        "UPDATE item_identity_watermark SET importing=1 WHERE singleton=1".into(),
                        vec![],
                    ),
                ])
                .await
                .expect("import fence");
            assert!(store
                .source_art_snapshot(&"b".repeat(64), grant, &reference)
                .await
                .is_err());
            store
                .sharing_txn(vec![
                    (
                        "UPDATE item_identity_watermark SET importing=0 WHERE singleton=1".into(),
                        vec![],
                    ),
                    (
                        "DELETE FROM sharing_export_libraries WHERE grant_id=$1".into(),
                        vec![grant.into()],
                    ),
                ])
                .await
                .expect("scope loss");
            assert!(matches!(
                store
                    .source_art_snapshot(&"b".repeat(64), grant, &reference)
                    .await
                    .expect("scope loss"),
                SourceArtRead::Unavailable
            ));
        }
    }
}
