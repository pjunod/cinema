//! Current authorized file witnesses. No Source user or path crosses the wire.
use super::sharing::Backend;
use crate::{
    error::StoreError,
    secrets::SealedSecret,
    sharing::{invalid, is_hash, SourceId},
    sharing_catalogue_details::SourceFileWitness,
};
use async_trait::async_trait;
use serde::Deserialize;
use uuid::Uuid;

/// Exact persisted revision vocabulary. Admission must regenerate this
/// expression in its atomic transaction using these fixed f/i/s aliases.
/// Caption bodies intentionally have their own future resource revisions.
pub const FILE_REVISION_PROJECTION_SQL:&str="json_array(1,s.server_id,s.catalogue_epoch,CAST(i.library_id AS TEXT),CAST(i.id AS TEXT),CAST(f.id AS TEXT),f.path,f.size,f.mtime,f.duration_ms,f.container,f.video_codec,f.video_profile,f.video_codec_tag,f.field_order,f.width,f.height,f.bit_depth,f.hdr,f.hdr_format,f.bitrate,f.audio_streams,f.subtitle_streams,f.probe_json,f.scanned_at,f.audio_offset_ms,f.dv_profile,f.dv_level,f.dv_bl_compat_id,f.dv_el_present,f.dv_rpu_present,f.max_cll,f.max_fall,f.mastering_max_luminance,f.luminance_source,json((SELECT json_group_array(json_array(json_extract(c.value,'$.source_size'),json_extract(c.value,'$.source_mtime'),json_extract(c.value,'$.provider_file_id'),json_extract(c.value,'$.language'),json_extract(c.value,'$.title'),json_extract(c.value,'$.hearing_impaired'),json_extract(c.value,'$.forced'))) FROM json_each(coalesce(f.downloaded_subtitles,'[]')) c)))";
pub const FILE_REVISION_CAPACITY_SQL:&str="length(f.path)<=32768 AND coalesce(length(f.probe_json),0)<=1048576 AND coalesce(length(f.audio_streams),0)<=65536 AND coalesce(length(f.subtitle_streams),0)<=262144 AND coalesce(length(f.downloaded_subtitles),0)<=4194304";

/// CASE, rather than optimizer-dependent AND evaluation, fences construction
/// of the private projection behind raw-field and serialized-byte limits.
pub fn file_revision_guarded_projection_sql() -> String {
    let text = [
        "path",
        "container",
        "video_codec",
        "video_profile",
        "video_codec_tag",
        "field_order",
        "hdr",
        "hdr_format",
        "luminance_source",
        "audio_streams",
        "subtitle_streams",
        "probe_json",
    ];
    let facts = [
        "size",
        "mtime",
        "duration_ms",
        "width",
        "height",
        "bit_depth",
        "bitrate",
        "scanned_at",
        "audio_offset_ms",
        "dv_profile",
        "dv_level",
        "dv_bl_compat_id",
        "dv_el_present",
        "dv_rpu_present",
        "max_cll",
        "max_fall",
        "mastering_max_luminance",
    ];
    let mut raw = vec![
        "length(CAST(f.path AS BLOB))<=32768".to_owned(),
        "coalesce(length(CAST(f.probe_json AS BLOB)),0)<=1048576".into(),
        "coalesce(length(CAST(f.audio_streams AS BLOB)),0)<=65536".into(),
        "coalesce(length(CAST(f.subtitle_streams AS BLOB)),0)<=262144".into(),
        "coalesce(length(CAST(f.downloaded_subtitles AS BLOB)),0)<=4194304".into(),
    ];
    raw.extend(
        text[1..9]
            .iter()
            .map(|field| format!("coalesce(length(CAST(f.{field} AS BLOB)),0)<=512")),
    );
    raw.extend(
        facts
            .iter()
            .map(|field| format!("typeof(f.{field}) IN ('integer','null')")),
    );
    let estimate = text
        .iter()
        .map(|field| format!("length(CAST(json_quote(f.{field}) AS BLOB))"))
        .collect::<Vec<_>>()
        .join("+");
    // Caption metadata is bounded by its raw source field before this nested
    // expression runs. Caption bodies never enter the canonical witness.
    let caption="json((SELECT json_group_array(json_array(json_extract(c.value,'$.source_size'),json_extract(c.value,'$.source_mtime'),json_extract(c.value,'$.provider_file_id'),json_extract(c.value,'$.language'),json_extract(c.value,'$.title'),json_extract(c.value,'$.hearing_impaired'),json_extract(c.value,'$.forced'))) FROM json_each(coalesce(f.downloaded_subtitles,'[]')) c))";
    format!("CASE WHEN {} THEN CASE WHEN 1024+{estimate}+length(CAST({caption} AS BLOB))<=2097152 THEN {FILE_REVISION_PROJECTION_SQL} ELSE NULL END ELSE NULL END",raw.join(" AND "))
}

pub enum SourceDetailsRead<T> {
    Authorized(T),
    Unavailable,
    Capacity,
}

#[async_trait]
pub trait SharingSourceDetailsStore: Send + Sync {
    /// Read existing purpose material only. Absence is unavailable; this never
    /// creates a key and never changes a Source identity or epoch.
    async fn source_catalogue_revision_key(
        &self,
        server: Uuid,
        epoch: Uuid,
    ) -> Result<Option<SealedSecret>, StoreError>;
    /// A wire revision alone cannot construct this server-only witness.
    async fn source_item_file_witness(
        &self,
        credential_hash: &str,
        grant: Uuid,
        item: SourceId,
        file: SourceId,
    ) -> Result<SourceDetailsRead<SourceFileWitness>, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingSourceDetailsStore for T {
    async fn source_catalogue_revision_key(
        &self,
        server: Uuid,
        epoch: Uuid,
    ) -> Result<Option<SealedSecret>, StoreError> {
        let rows = self.sharing_revision_key_rows().await?;
        let envelopes = super::sharing::revision_key_envelopes(rows.clone())?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_str(row).map_err(|_| invalid())?;
        if value["server_id"].as_str() != Some(server.to_string().as_str())
            || value["catalogue_epoch"].as_str() != Some(epoch.to_string().as_str())
        {
            return Ok(None);
        }
        Ok(envelopes.into_iter().next())
    }
    async fn source_item_file_witness(
        &self,
        hash: &str,
        grant: Uuid,
        item: SourceId,
        file: SourceId,
    ) -> Result<SourceDetailsRead<SourceFileWitness>, StoreError> {
        if !is_hash(hash) {
            return Err(invalid());
        }
        super::sharing_catalogue_source::ready(self).await?;
        let projection = file_revision_guarded_projection_sql();
        let sql=format!("SELECT json_object('server',s.server_id,'epoch',s.catalogue_epoch,'library',CAST(i.library_id AS TEXT),'item',CAST(i.id AS TEXT),'file',CAST(f.id AS TEXT),'projection',({projection})||'') AS payload FROM sharing_exports e JOIN sharing_identity s ON s.singleton=1 JOIN sharing_export_libraries x ON x.grant_id=e.id JOIN libraries l ON l.id=x.library_id JOIN items i ON i.library_id=l.id JOIN files f ON f.item_id=i.id WHERE e.id=$1 AND e.token_hash=$2 AND e.state='active' AND i.id=$3 AND f.id=$4 AND i.kind IN ('movie','episode') AND l.kind IN ('movies','shows') AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0)");
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    grant.into(),
                    hash.to_owned().into(),
                    item.as_str().parse::<i64>().map_err(|_| invalid())?.into(),
                    file.as_str().parse::<i64>().map_err(|_| invalid())?.into(),
                ],
            )
            .await?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Row {
            server: String,
            epoch: String,
            library: SourceId,
            item: SourceId,
            file: SourceId,
            projection: Option<String>,
        }
        if rows.len() > 1 {
            return Err(invalid());
        }
        match rows.first() {
            None => Ok(SourceDetailsRead::Unavailable),
            Some(row) => {
                let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
                let uuid = |value: &str| -> Result<Uuid, StoreError> {
                    let id = Uuid::parse_str(value).map_err(|_| invalid())?;
                    if id.to_string() != value {
                        return Err(invalid());
                    }
                    Ok(id)
                };
                if row.item != item || row.file != file {
                    return Err(invalid());
                }
                let Some(projection) = row.projection else {
                    return Ok(SourceDetailsRead::Capacity);
                };
                Ok(SourceDetailsRead::Authorized(
                    SourceFileWitness::from_current_projection(
                        uuid(&row.server)?,
                        uuid(&row.epoch)?,
                        row.library,
                        row.item,
                        row.file,
                        projection,
                    )?,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{LibraryKind, NewLibrary},
        secrets::CredentialKey,
        sharing::{InvitationRecord, ShareClaim},
        sharing_catalogue_details::CatalogueRevisionKey,
        store::{LibraryStore, SharingStore, SqliteStore},
    };
    use std::path::PathBuf;
    async fn current(store: &SqliteStore, grant: Uuid) -> SourceFileWitness {
        match store
            .source_item_file_witness(
                &"b".repeat(64),
                grant,
                SourceId::parse("9007199254740993").expect("item"),
                SourceId::parse("9223372036854775807").expect("file"),
            )
            .await
            .expect("current snapshot")
        {
            SourceDetailsRead::Authorized(witness) => witness,
            _ => panic!("expected currently authorized witness"),
        }
    }
    #[tokio::test]
    async fn sharing_catalogue_file_witness_queries_current_authority_and_short_circuits_capacity()
    {
        let directory = tempfile::tempdir().expect("details fixtures");
        for store in [
            SqliteStore::open_in_memory().expect("memory"),
            SqliteStore::open(&directory.path().join("details.sqlite")).expect("pooled"),
        ] {
            let identity = store.sharing_identity(1000).await.expect("identity");
            let library = store
                .create_library(&NewLibrary {
                    name: "Exported movies".into(),
                    kind: LibraryKind::Movies,
                    paths: vec![PathBuf::from("/synthetic")],
                    anime: false,
                })
                .await
                .expect("library")
                .id;
            let grant = Uuid::new_v4();
            let invitation = Uuid::new_v4();
            store
                .create_share_invitation(InvitationRecord {
                    id: invitation,
                    token_hash: "a".repeat(64),
                    library_ids: vec![library],
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
                    recipient_name: "synthetic".into(),
                    credential_hash: "b".repeat(64),
                    now_ms: 1001,
                })
                .await
                .expect("claim");
            store.approve_share(grant, 1, 1002).await.expect("approve");
            store.sharing_txn(super::super::sharing_catalogue_source::candidate_statements().into_iter().chain(super::super::sharing_catalogue_source::candidate_item_identity_statements()).map(|sql|(sql,vec![])).collect()).await.expect("candidate qualified layout fixture");
            let captions=serde_json::json!([{"source_size":20,"source_mtime":1000,"provider_file_id":7,"language":"en","title":"English","hearing_impaired":false,"forced":false,"vtt":"PRIVATE CAPTION BODY"}]).to_string();
            store.sharing_txn(vec![("INSERT INTO items(id,library_id,kind,title,sort_title) VALUES(9007199254740993,$1,'movie','Movie','movie')".into(),vec![library.into()]),("INSERT INTO files(id,item_id,path,size,mtime,probe_json,downloaded_subtitles) VALUES(9223372036854775807,9007199254740993,'/private/synthetic.mkv',20,1000,'PRIVATE PROBE',$1)".into(),vec![captions.into()])]).await.expect("private current file fixture");
            let witness = current(&store, grant).await;
            assert!(witness
                .canonical_projection()
                .contains("/private/synthetic.mkv"));
            assert!(!witness
                .canonical_projection()
                .contains("PRIVATE CAPTION BODY"));
            let sealing = CredentialKey::from_bytes([17; 32]);
            let envelope = CatalogueRevisionKey::generate_sealed(&sealing, identity.clone())
                .expect("purpose key");
            let key = CatalogueRevisionKey::open(&sealing, identity.clone(), &envelope)
                .expect("stable key");
            assert!(store
                .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
                .await
                .expect("read never initializes absent key")
                .is_none());
            store
                .sharing_txn(vec![
                    (
                        super::super::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA
                            .into(),
                        vec![],
                    ),
                    (
                        "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)".into(),
                        vec![
                            identity.server_id.into(),
                            identity.catalogue_epoch.into(),
                            envelope.as_stored().to_owned().into(),
                        ],
                    ),
                ])
                .await
                .expect("fixture-only purpose key provisioning");
            let loaded = store
                .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
                .await
                .expect("current binding")
                .expect("existing key");
            assert!(loaded.as_stored() == envelope.as_stored());
            assert!(store
                .source_catalogue_revision_key(identity.server_id, Uuid::new_v4())
                .await
                .expect("wrong epoch")
                .is_none());
            let initial = key.file_revision(&witness).expect("initial revision");
            store.sharing_txn(vec![("UPDATE files SET downloaded_subtitles=json_set(downloaded_subtitles,'$[0].vtt','NEW PRIVATE CAPTION BODY')".into(),vec![])]).await.expect("caption body mutation");
            assert_eq!(
                initial,
                key.file_revision(&current(&store, grant).await)
                    .expect("caption bodies have separate resource revisions")
            );
            store.sharing_txn(vec![("UPDATE files SET downloaded_subtitles=json_set(downloaded_subtitles,'$[0].language','fr')".into(),vec![])]).await.expect("caption resource metadata mutation");
            assert_ne!(
                initial,
                key.file_revision(&current(&store, grant).await)
                    .expect("resource metadata revision")
            );
            store
                .sharing_txn(vec![(
                    "UPDATE files SET probe_json=$1".into(),
                    vec!["x".repeat(1048577).into()],
                )])
                .await
                .expect("oversized private snapshot");
            assert!(matches!(
                store
                    .source_item_file_witness(
                        &"b".repeat(64),
                        grant,
                        SourceId::parse("9007199254740993").expect("item"),
                        SourceId::parse("9223372036854775807").expect("file")
                    )
                    .await
                    .expect("typed capacity refusal"),
                SourceDetailsRead::Capacity
            ));
            // Malformed caption JSON must not be evaluated after the raw probe
            // capacity branch has already refused this projection.
            store
                .sharing_txn(vec![(
                    "UPDATE files SET downloaded_subtitles='malformed private JSON'".into(),
                    vec![],
                )])
                .await
                .expect("short circuit witness");
            assert!(matches!(
                store
                    .source_item_file_witness(
                        &"b".repeat(64),
                        grant,
                        SourceId::parse("9007199254740993").expect("item"),
                        SourceId::parse("9223372036854775807").expect("file")
                    )
                    .await
                    .expect("CASE precedes JSON construction"),
                SourceDetailsRead::Capacity
            ));
            store
                .share_scope(grant, 2, vec![], 1010)
                .await
                .expect("scope revocation");
            assert!(matches!(
                store
                    .source_item_file_witness(
                        &"b".repeat(64),
                        grant,
                        SourceId::parse("9007199254740993").expect("item"),
                        SourceId::parse("9223372036854775807").expect("file")
                    )
                    .await
                    .expect("current grant refusal"),
                SourceDetailsRead::Unavailable
            ));
        }
    }
}
