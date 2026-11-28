//! Materialize a complete progressive asset through the ordinary subtitle list.

use super::downloaded_subtitles::{MAX_DOWNLOADED_SUBTITLES, MAX_DOWNLOADED_SUBTITLE_BYTES};

// The accepted segments are already ordered by core and the drafts sort cues
// within each core. Escape markup and flatten line breaks to keep cue text
// literal in WebVTT. The enclosing query supplies the asset alias a.
pub(super) const RENDER_VTT: &str = r#"('WEBVTT'||char(10,10)||COALESCE((
 SELECT group_concat(block,'') FROM (
  SELECT printf('%02d:%02d:%02d.%03d --> %02d:%02d:%02d.%03d',
      start_ms/3600000,(start_ms/60000)%60,(start_ms/1000)%60,start_ms%1000,
      end_ms/3600000,(end_ms/60000)%60,(end_ms/1000)%60,end_ms%1000)
    ||char(10)||replace(replace(replace(replace(text,char(10),' '),'&','&amp;'),'<','&lt;'),'>','&gt;')
    ||char(10,10) AS block
  FROM (
    SELECT s.core_index,CAST(c.key AS INTEGER) AS ordinal,
      json_extract(c.value,'$.start_ms') AS start_ms,json_extract(c.value,'$.end_ms') AS end_ms,
      json_extract(c.value,'$.text') AS text
    FROM subtitle_translation_segments s, json_each(s.body,'$.cues') c WHERE s.asset_id=a.id
  ) ORDER BY core_index,ordinal
 )
),''))"#;

const COMPLETED: &str = r#"WITH completed AS MATERIALIZED (
 SELECT a.id,a.file_id,a.audio_index,a.profile_id,a.profile_json,
   CASE WHEN json_extract(a.profile_json,'$.task')='transcribe' THEN a.spoken_language ELSE 'en' END AS language,
   f.size,f.mtime
 FROM subtitle_translation_assets a
 JOIN subtitle_translation_operations op ON op.id=a.operation_id AND op.asset_id=a.id
 JOIN subtitle_translation_finish_demands finish ON finish.operation_id=op.id
 JOIN files f ON f.id=a.file_id AND f.size=json_extract(a.source_json,'$.source_size')
    AND f.mtime=json_extract(a.source_json,'$.source_mtime') AND f.probe_json IS NOT NULL
 WHERE a.id=json_extract($1,'$.recipe.asset_id') AND op.state='complete' AND op.stopped=0
 AND op.operation_epoch=json_extract($1,'$.recipe.operation_epoch') AND a.retired_at_ms IS NULL
 AND EXISTS(SELECT 1 FROM subtitle_translation_segments s WHERE s.asset_id=a.id
   AND s.core_index=json_extract($1,'$.recipe.core_index') AND s.publication_id=json_extract($1,'$.publication_id'))
)"#;

const TRACK_PRESENT: &str = r#"EXISTS(SELECT 1 FROM json_each(file.downloaded_subtitles) existing
 WHERE json_extract(existing.value,'$.transcription.artifact_key')=a.id
 AND json_extract(existing.value,'$.source_size')=a.size AND json_extract(existing.value,'$.source_mtime')=a.mtime)"#;
const CURRENT_LIST: &str = r#"CASE WHEN json_extract(file.downloaded_subtitles,'$[0].source_size')=file.size
 AND json_extract(file.downloaded_subtitles,'$[0].source_mtime')=file.mtime
 THEN file.downloaded_subtitles ELSE '[]' END"#;

fn caption_json(vtt: &str, progressive_asset: &str) -> String {
    format!(
        r#"json_object('source_size',a.size,'source_mtime',a.mtime,'provider_file_id',0,
 'language',a.language,'title','AI subtitles','hearing_impaired',json('false'),'forced',json('false'),'vtt',{vtt},
 'transcription',json_object('audio_index',a.audio_index,'artifact_key',a.id,
   'model_sha256',json_extract(a.profile_json,'$.model_sha256'),'pipeline_digest',a.profile_id,
   'adapter',json_extract(a.profile_json,'$.adapter_contract'),'generated_at_ms',json_extract($1,'$.now_ms'),
   'progressive_asset_id',{progressive_asset},
   'execution',json_object('engine_sha256',json_extract(json_extract($1,'$.segment_body'),'$.provenance.engine_sha256'),
      'platform',json_extract(json_extract($1,'$.segment_body'),'$.provenance.platform'))))"#
    )
}

pub(super) fn store_inline() -> String {
    let caption = caption_json("a.vtt", "NULL");
    format!(
        r#"{COMPLETED}, rendered AS MATERIALIZED (SELECT a.*,{RENDER_VTT} AS vtt FROM completed a)
 UPDATE files AS file SET downloaded_subtitles=json_insert(({CURRENT_LIST}),'$[#]',{caption})
 FROM rendered a
 WHERE file.id=a.file_id AND file.size=a.size AND file.mtime=a.mtime AND NOT {TRACK_PRESENT}
 AND json_array_length(({CURRENT_LIST}))<{MAX_DOWNLOADED_SUBTITLES}
 AND length(CAST(a.vtt AS BLOB))<={MAX_DOWNLOADED_SUBTITLE_BYTES}"#
    )
}

pub(super) fn store_asset_reference() -> String {
    let caption = caption_json("''", "a.id");
    format!(
        r#"{COMPLETED}
 INSERT INTO subtitle_translation_stored_tracks(asset_id,file_id,source_size,source_mtime,caption_json)
 SELECT a.id,a.file_id,a.size,a.mtime,{caption}
 FROM completed a JOIN files file ON file.id=a.file_id
 WHERE NOT {TRACK_PRESENT} ON CONFLICT(asset_id) DO NOTHING"#
    )
}
