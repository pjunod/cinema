//! Current authorized file witnesses. No Source user or path crosses the wire.
use super::sharing::Backend;
use crate::{
    error::StoreError,
    secrets::SealedSecret,
    sharing::{invalid, is_hash, SourceId},
    sharing_catalogue::SourceCatalogueRecord,
    sharing_catalogue_details::SourceFileWitness,
};
use async_trait::async_trait;
use serde::Deserialize;
use uuid::Uuid;

/// Exact persisted revision vocabulary. Admission must regenerate this
/// expression in its atomic transaction using these fixed f/i/s aliases.
/// Caption bodies intentionally have their own future resource revisions.
pub const FILE_REVISION_PROJECTION_SQL:&str="json_array(1,s.server_id,s.catalogue_epoch,CAST(i.library_id AS TEXT),CAST(i.id AS TEXT),CAST(f.id AS TEXT),f.path,f.size,f.mtime,f.duration_ms,f.container,f.video_codec,f.video_profile,f.video_codec_tag,f.field_order,f.width,f.height,f.bit_depth,f.hdr,f.hdr_format,f.bitrate,f.audio_streams,f.subtitle_streams,f.probe_json,f.scanned_at,f.audio_offset_ms,f.dv_profile,f.dv_level,f.dv_bl_compat_id,f.dv_el_present,f.dv_rpu_present,f.max_cll,f.max_fall,f.mastering_max_luminance,f.luminance_source,json((SELECT json_group_array(json_array(json_extract(c.value,'$.source_size'),json_extract(c.value,'$.source_mtime'),json_extract(c.value,'$.provider_file_id'),json_extract(c.value,'$.language'),json_extract(c.value,'$.title'),json_extract(c.value,'$.hearing_impaired'),json_extract(c.value,'$.forced'))) FROM json_each(coalesce(f.downloaded_subtitles,'[]')) c)))";

/// CASE, rather than optimizer-dependent AND evaluation, fences construction
/// of the private projection behind raw-field and serialized-byte limits.
fn revision_bounds() -> (String, String) {
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
    (
        raw.join(" AND "),
        format!("1024+{estimate}+length(CAST({caption} AS BLOB))"),
    )
}
pub fn file_revision_guarded_projection_sql() -> String {
    let (raw, estimate) = revision_bounds();
    format!("CASE WHEN {raw} THEN CASE WHEN {estimate}<=2097152 THEN {FILE_REVISION_PROJECTION_SQL} ELSE NULL END ELSE NULL END")
}

/// Private snapshots never implement Serialize or Debug.
pub struct SourceItemSnapshot {
    pub record: SourceCatalogueRecord,
    pub server: Uuid,
    pub epoch: Uuid,
    pub files: Vec<SourceFileWitness>,
}
pub enum SourceDetailsRead<T> {
    Authorized(T),
    Unavailable,
    Capacity,
}

#[async_trait]
pub trait SharingSourceDetailsStore: Send + Sync {
    async fn source_item_details_snapshot(
        &self,
        credential_hash: &str,
        grant: Uuid,
        item: SourceId,
    ) -> Result<SourceDetailsRead<SourceItemSnapshot>, StoreError>;
    /// Current tuple authority for a captured details response. Revision changes
    /// do not revoke metadata; deletion, movement and effective scope do.
    async fn source_content_files_authorized(
        &self,
        grant: Uuid,
        server: Uuid,
        epoch: Uuid,
        files: &[(SourceId, SourceId, SourceId)],
    ) -> Result<bool, StoreError>;
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
    async fn source_content_files_authorized(
        &self,
        grant: Uuid,
        server: Uuid,
        epoch: Uuid,
        files: &[(SourceId, SourceId, SourceId)],
    ) -> Result<bool, StoreError> {
        if files.len() > 64 {
            return Err(invalid());
        }
        super::sharing_catalogue_source::ready(self).await?;
        let tuples = serde_json::to_string(files).map_err(|_| invalid())?;
        let rows=self.sharing_read("SELECT json_quote(EXISTS(SELECT 1 FROM sharing_exports e JOIN sharing_identity s ON s.singleton=1 WHERE e.id=$1 AND e.state='active' AND s.server_id=$2 AND s.catalogue_epoch=$3 AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0) AND NOT EXISTS(SELECT 1 FROM json_each($4) r WHERE NOT EXISTS(SELECT 1 FROM files f JOIN items i ON i.id=f.item_id JOIN libraries l ON l.id=i.library_id JOIN sharing_export_libraries x ON x.library_id=l.id AND x.grant_id=e.id WHERE CAST(l.id AS TEXT)=json_extract(r.value,'$[0]') AND CAST(i.id AS TEXT)=json_extract(r.value,'$[1]') AND CAST(f.id AS TEXT)=json_extract(r.value,'$[2]') AND l.kind IN ('movies','shows') AND i.kind IN ('movie','episode'))))) AS payload",vec![grant.into(),server.into(),epoch.into(),tuples.into()]).await?;
        Ok(rows.len() == 1 && rows[0] == "1")
    }
    async fn source_item_details_snapshot(
        &self,
        hash: &str,
        grant: Uuid,
        item: SourceId,
    ) -> Result<SourceDetailsRead<SourceItemSnapshot>, StoreError> {
        if !is_hash(hash) {
            return Err(invalid());
        }
        super::sharing_catalogue_source::ready(self).await?;
        let (raw, estimate) = revision_bounds();
        let projection = file_revision_guarded_projection_sql();
        // The aggregate estimate runs before canonical projections are built.
        // LIMIT 65 is a refusal sentinel, never a truncated file listing.
        let record = super::sharing_catalogue_source::RECORD;
        let sql=format!("WITH allowed AS (SELECT i.id FROM sharing_exports e JOIN sharing_identity s ON s.singleton=1 AND length(CAST(s.server_id AS BLOB))=36 AND length(CAST(s.catalogue_epoch AS BLOB))=36 JOIN sharing_export_libraries x ON x.grant_id=e.id JOIN libraries l ON l.id=x.library_id JOIN items i ON i.library_id=l.id WHERE e.id=$1 AND e.token_hash=$2 AND e.state='active' AND i.id=$3 AND i.kind IN ('movie','show','season','episode') AND l.kind IN ('movies','shows') AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0)), bounded_files AS (SELECT f.id FROM files f WHERE f.item_id=$3 ORDER BY f.id LIMIT 65), capacity AS (SELECT count(*) AS n,coalesce(sum(CASE WHEN {raw} THEN {estimate} ELSE 8388609 END),0) AS bytes FROM bounded_files b JOIN files f ON f.id=b.id) SELECT json_object('server',s.server_id,'epoch',s.catalogue_epoch,'record',json({record}),'projections',CASE WHEN c.n<=64 AND c.bytes<=8388608 THEN json((SELECT json_group_array(({projection})||'') FROM bounded_files b JOIN files f ON f.id=b.id)) ELSE NULL END) AS payload FROM allowed a JOIN items i ON i.id=a.id JOIN sharing_identity s ON s.singleton=1 CROSS JOIN capacity c");
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    grant.into(),
                    hash.to_owned().into(),
                    item.as_str().parse::<i64>().map_err(|_| invalid())?.into(),
                ],
            )
            .await?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Row {
            server: Uuid,
            epoch: Uuid,
            record: Option<SourceCatalogueRecord>,
            projections: Option<Vec<Option<String>>>,
        }
        if rows.len() > 1 {
            return Err(invalid());
        }
        let Some(row) = rows.first() else {
            return Ok(SourceDetailsRead::Unavailable);
        };
        let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
        let (Some(record), Some(projections)) = (row.record, row.projections) else {
            return Ok(SourceDetailsRead::Capacity);
        };
        record.item.validate().map_err(|_| invalid())?;
        if record.item.item_id != item || projections.len() > 64 {
            return Err(invalid());
        }
        let mut files = Vec::with_capacity(projections.len());
        for projection in projections {
            let Some(projection) = projection else {
                return Ok(SourceDetailsRead::Capacity);
            };
            let value: serde_json::Value =
                serde_json::from_str(&projection).map_err(|_| invalid())?;
            let file = SourceId::parse(value[5].as_str().ok_or_else(invalid)?)?;
            files.push(SourceFileWitness::from_current_projection(
                row.server,
                row.epoch,
                record.item.library_id.clone(),
                item.clone(),
                file,
                projection,
            )?);
        }
        Ok(SourceDetailsRead::Authorized(SourceItemSnapshot {
            record,
            server: row.server,
            epoch: row.epoch,
            files,
        }))
    }
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
        let sql=format!("SELECT json_object('server',s.server_id,'epoch',s.catalogue_epoch,'library',CAST(i.library_id AS TEXT),'item',CAST(i.id AS TEXT),'file',CAST(f.id AS TEXT),'projection',({projection})||'') AS payload FROM sharing_exports e JOIN sharing_identity s ON s.singleton=1 JOIN sharing_export_libraries x ON x.grant_id=e.id JOIN libraries l ON l.id=x.library_id JOIN items i ON i.library_id=l.id JOIN files f ON f.item_id=i.id WHERE length(CAST(s.server_id AS BLOB))=36 AND length(CAST(s.catalogue_epoch AS BLOB))=36 AND e.id=$1 AND e.token_hash=$2 AND e.state='active' AND i.id=$3 AND f.id=$4 AND i.kind IN ('movie','episode') AND l.kind IN ('movies','shows') AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0)");
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
            store.sharing_txn(vec![("UPDATE files SET probe_json=$1".into(),vec![r#"{"chapters":[{"start_time":"0","end_time":"1.5","tags":{"title":"Opening"}}],"private":"PRIVATE PROBE"}"#.to_owned().into()])]).await.expect("bounded valid probe");
            let item = SourceId::parse("9007199254740993").expect("item");
            let file = SourceId::parse("9223372036854775807").expect("file");
            let read = store
                .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
                .await
                .expect("one consistent detail query");
            let SourceDetailsRead::Authorized(snapshot) = read else {
                panic!("expected details")
            };
            assert_eq!(snapshot.files.len(), 1);
            let facts = snapshot.files[0].playable_file(&key).expect("closed facts");
            assert_eq!(facts.chapters[0].end_ms, 1500);
            let wire_value = serde_json::to_value(&facts).expect("closed wire");
            let mut injected = wire_value.clone();
            injected["path"] = serde_json::json!("/private/injected");
            assert!(
                serde_json::from_value::<crate::sharing_catalogue_details::SourcePlayableFile>(
                    injected
                )
                .is_err()
            );
            for size in ["-1", "01", "9223372036854775808"] {
                let mut changed = wire_value.clone();
                changed["size"] = serde_json::json!(size);
                assert!(serde_json::from_value::<
                    crate::sharing_catalogue_details::SourcePlayableFile,
                >(changed)
                .expect("string size")
                .validate()
                .is_err());
            }
            let track = serde_json::json!({"index":0,"codec":"aac","channels":2,"sample_rate":48000,"language":"en","title":null,"default":true});
            let mut duplicates = wire_value.clone();
            duplicates["audio_streams"] = serde_json::json!([track.clone(), track.clone()]);
            assert!(
                serde_json::from_value::<crate::sharing_catalogue_details::SourcePlayableFile>(
                    duplicates
                )
                .expect("bounded duplicate tracks")
                .validate()
                .is_err()
            );
            let mut excess_tracks = wire_value.clone();
            excess_tracks["audio_streams"] = serde_json::json!(vec![track; 65]);
            assert!(
                serde_json::from_value::<crate::sharing_catalogue_details::SourcePlayableFile>(
                    excess_tracks
                )
                .is_err()
            );
            let mut excess = wire_value.clone();
            excess["chapters"] = serde_json::json!(vec![
                serde_json::to_value(&facts.chapters[0])
                    .expect("chapter");
                1025
            ]);
            assert!(
                serde_json::from_value::<crate::sharing_catalogue_details::SourcePlayableFile>(
                    excess
                )
                .is_err()
            );
            let detail = serde_json::json!({"item":snapshot.record.item,"files":vec![wire_value.clone();65]});
            assert!(
                serde_json::from_value::<crate::sharing_catalogue_details::SourceItemDetails>(
                    detail
                )
                .is_err()
            );
            let duplicated = serde_json::json!({"item":snapshot.record.item,"files":[wire_value.clone(),wire_value]});
            assert!(
                serde_json::from_value::<crate::sharing_catalogue_details::SourceItemDetails>(
                    duplicated
                )
                .expect("bounded duplicate")
                .validate()
                .is_err()
            );
            let wire = serde_json::to_string(&facts).expect("wire");
            assert!(!wire.contains("PRIVATE") && !wire.contains("/private/"));
            let tuples = vec![(
                snapshot.record.item.library_id.clone(),
                item.clone(),
                file.clone(),
            )];
            assert!(store
                .source_content_files_authorized(
                    grant,
                    identity.server_id,
                    identity.catalogue_epoch,
                    &tuples
                )
                .await
                .expect("current tuples"));
            store
                .sharing_txn(vec![(
                    "UPDATE files SET path='/private/changed.mkv'".into(),
                    vec![],
                )])
                .await
                .expect("benign revision change");
            assert!(store
                .source_content_files_authorized(
                    grant,
                    identity.server_id,
                    identity.catalogue_epoch,
                    &tuples
                )
                .await
                .expect("revision change preserves metadata authority"));
            store.sharing_txn(vec![("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<64) INSERT INTO files(id,item_id,path,size,mtime) SELECT x,9007199254740993,'/synthetic/'||x,20,1000 FROM n".into(),vec![])]).await.expect("65 files fixture");
            assert!(matches!(
                store
                    .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
                    .await
                    .expect("file count refusal"),
                SourceDetailsRead::Capacity
            ));
            store
                .sharing_txn(vec![("DELETE FROM files WHERE id=64".into(), vec![])])
                .await
                .expect("64 files fixture");
            let SourceDetailsRead::Authorized(snapshot) = store
                .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
                .await
                .expect("64 admitted")
            else {
                panic!("64 files fit")
            };
            assert_eq!(snapshot.files.len(), 64);
            store
                .sharing_txn(vec![(
                    "UPDATE files SET probe_json=$1 WHERE id<=8".into(),
                    vec![serde_json::to_string(&"x".repeat(1048574))
                        .expect("bounded scalar")
                        .into()],
                )])
                .await
                .expect("aggregate fixture");
            assert!(matches!(
                store
                    .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
                    .await
                    .expect("aggregate refusal"),
                SourceDetailsRead::Capacity
            ));
            store
                .sharing_txn(vec![
                    (
                        "DELETE FROM files WHERE id<9223372036854775807".into(),
                        vec![],
                    ),
                    (
                        "UPDATE items SET overview=$1".into(),
                        vec!["x".repeat(8193).into()],
                    ),
                ])
                .await
                .expect("oversized public fixture");
            assert!(matches!(
                store
                    .source_item_details_snapshot(&"b".repeat(64), grant, item.clone())
                    .await
                    .expect("public capacity refusal"),
                SourceDetailsRead::Capacity
            ));
            store
                .sharing_txn(vec![("UPDATE items SET overview=NULL".into(), vec![])])
                .await
                .expect("restore public fields");
            let initial = key
                .file_revision(&current(&store, grant).await)
                .expect("initial revision");
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
