"use strict";
// File identity is separate from a player's ordinary B HLS session identity.
// Shared contexts enter only through B's authenticated detail adapter. A source
// file number is metadata; it is never a fallback local route or cache key.
const PLAYBACK_FILE_CONTEXTS=new WeakSet();
const PLAYBACK_FILE_UUID=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
function playbackFileReject(){ throw new TypeError("Invalid playback file context"); }
function playbackFileDecimal(value){
  if(typeof value!=="string"||!/^(0|[1-9][0-9]{0,18})$/.test(value)
    ||(value.length===19&&value>"9223372036854775807")) playbackFileReject();
  return value;
}
function localPlaybackFileContext(id){
  if(typeof id==="number"){
    if(!Number.isSafeInteger(id)||id<0) playbackFileReject();
    id=String(id);
  }
  id=playbackFileDecimal(id);
  return registerPlaybackFileContext({source_ref:Object.freeze({kind:"local",file_id:id}),
    file_base:`/api/v1/files/${id}`,session_id:null});
}
function registerPlaybackFileContext(value){
  const context=Object.freeze(value); PLAYBACK_FILE_CONTEXTS.add(context); return context;
}
function sharedPlaybackFileContextFromDetail(reference,file,lifecycle=file?.reference?.lifecycle_generation){
  if(!reference||!file||typeof reference!=="object") playbackFileReject();
  const keys=["import_id","server_id","catalogue_epoch","library_id","item_id"];
  if(Object.keys(reference).length!==keys.length||keys.some(k=>!Object.hasOwn(reference,k))) playbackFileReject();
  for(const k of keys.slice(0,3)) if(typeof reference[k]!=="string"||!PLAYBACK_FILE_UUID.test(reference[k])||reference[k]==="00000000-0000-0000-0000-000000000000") playbackFileReject();
  for(const k of keys.slice(3)) playbackFileDecimal(reference[k]);
  // No decoding or URL normalization: encoded separators, queries, fragments,
  // dot segments and authorities must never acquire another interpretation.
  const prefix=`/api/v1/shared/imports/${reference.import_id}/files/`;
  const base=file.file_base;
  if(typeof base!=="string"||!base.startsWith(prefix)
    ||! /^[A-Za-z0-9_-]{236}$/.test(base.slice(prefix.length))) playbackFileReject();
  const source_file_id=playbackFileDecimal(file.file_id),r=file.reference;
  if(!r||keys.some(k=>r.item?.[k]!==reference[k])||r.file_id!==source_file_id) playbackFileReject();
  const revision=r.revision;
  if(typeof revision!=="string"||! /^[a-f0-9]{64}$/.test(revision)||file.revision!==revision) playbackFileReject();
  if(typeof lifecycle!=="bigint"||lifecycle<=0n||lifecycle>9223372036854775807n||r.lifecycle_generation!==lifecycle) playbackFileReject();
  return registerPlaybackFileContext({source_ref:Object.freeze({...reference}),
    source_file_id,file_revision:revision,lifecycle_generation:lifecycle,
    auth_generation:AUTH_GENERATION,auth_origin:typeof API==="string"?API:null,file_base:base,session_id:null});
}
function playbackFileContext(value){
  if(value&&typeof value==="object"){
    if(PLAYBACK_FILE_CONTEXTS.has(value)){
      if(value.source_ref.kind!=="local"&&(value.auth_generation!==AUTH_GENERATION||value.auth_origin!==(typeof API==="string"?API:null))) playbackFileReject();
      return value;
    }
    if(value.source_ref||value.file_base) playbackFileReject();
  }
  return localPlaybackFileContext(value);
}
function withPlaybackFileSession(value,id){
  const context=playbackFileContext(value);
  if(typeof id!=="string"||id.length!==36||! /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)) playbackFileReject();
  return registerPlaybackFileContext({...context,session_id:id});
}
function playbackFileKey(value){
  const c=playbackFileContext(value), r=c.source_ref;
  return r.kind==="local"?r.file_id:JSON.stringify([c.auth_origin,c.auth_generation,r.import_id,r.server_id,
    r.catalogue_epoch,r.library_id,r.item_id,c.source_file_id,c.file_revision,String(c.lifecycle_generation),c.file_base]);
}
function playbackFileSuffix(suffix){
  if(typeof suffix!=="string") playbackFileReject();
  if(["decision","hls/sessions","direct","stream.mp4"].includes(suffix)) return suffix;
  const index="(?:0|[1-9][0-9]{0,5})";
  if(new RegExp(`^subs/${index}(?:\\.vtt)?$`).test(suffix)
    ||new RegExp(`^subs/${index}/overlay\\.json$`).test(suffix)
    ||new RegExp(`^subs/${index}/overlay/[a-f0-9]{64}/objects/[a-f0-9]{64}\\.png$`).test(suffix)
    ||new RegExp(`^chapters/${index}/thumb$`).test(suffix)) return suffix;
  playbackFileReject();
}
function playbackFileQuery(suffix,query){
  if(!query||typeof query!=="object"||Array.isArray(query)) playbackFileReject();
  // Actual Caps and StreamQuery fields, including named-profile/native
  // compatibility claims. The browser currently emits a subset in capsQuery.
  const capabilities=["client","device","profile","vcodec","vmaxheight","acodec", "container",
    "maxheight","hdr","dv","dvprofile","dvhls","hdr10t"];
  const allowed=suffix==="decision"?capabilities.concat(["force","audio","subtitle","audio_offset_ms",
    "achannels","capver","hdrtypes","dvdecoders","dvraw","dvstatus"])
    :suffix==="stream.mp4"?capabilities.concat(["force","audio","audio_offset_ms","start","stream"])
      :suffix.startsWith("chapters/")?["v"]:[];
  return Object.entries(query).map(([key,value])=>{
    if(!allowed.includes(key)) playbackFileReject();
    const text=String(value);
    if(["hdr","dv","dvhls","hdr10t"].includes(key)){
      if(!/^[01]$/.test(text)) playbackFileReject();
    }else if(["vcodec","acodec","container"].includes(key)){
      // Profile data and native decoders are not limited to browser codecs.
      // Short tokens remain data; URL delimiters and an unbounded CSV do not.
      if(text.length>256||text!==""&&text.split(",").some(v=>!/^[a-zA-Z0-9_-]{1,32}$/.test(v))) playbackFileReject();
    }else if(key==="dvprofile"){
      if(text.length>64||text!==""&&text.split(",").some(v=>!/^(0|[1-9][0-9]{0,2})$/.test(v)||Number(v)>255)) playbackFileReject();
    }else if(key==="vmaxheight"){
      if(text.length>256||text!==""&&text.split(",").some(v=>{
        const parts=v.split(":");return parts.length!==2||!/^[a-zA-Z0-9_-]{1,32}$/.test(parts[0])
          ||!/^[1-9][0-9]{0,4}$/.test(parts[1])||Number(parts[1])>65535;
      })) playbackFileReject();
    }else if(["client","profile"].includes(key)){
      if(!/^[a-zA-Z0-9_.-]{1,64}$/.test(text)) playbackFileReject();
    }else if(["device","capver","hdrtypes","dvdecoders","dvraw","dvstatus"].includes(key)){
      if(text.length>256||/[\u0000-\u001f\u007f]/.test(text)) playbackFileReject();
    }else if(key==="achannels"){
      if(!/^(?:[1-9]|1[0-6])$/.test(text)) playbackFileReject();
    }else if(key==="force"){
      if(!/^(auto|original|transcode)$/.test(text)) playbackFileReject();
    }else if(key==="stream"){
      if(!/^[a-zA-Z0-9_-]{1,200}$/.test(text)) playbackFileReject();
    }else if(key==="start"){
      if(!/^(0|[1-9][0-9]{0,12})(\.[0-9]{1,3})?$/.test(text)||!Number.isFinite(Number(text))) playbackFileReject();
    }else if(key==="v"){
      if(!/^-?(0|[1-9][0-9]{0,19})$/.test(text)) playbackFileReject();
    }else{
      const n=Number(value);
      const min=key==="subtitle"?-1:key==="audio_offset_ms"?-15000:0;
      const max=key==="audio_offset_ms"?15000:key==="maxheight"?65535:999999;
      if(!Number.isInteger(n)||n<min||n>max||String(n)!==text) playbackFileReject();
    }
    return `${key}=${encodeURIComponent(text)}`;
  }).join("&");
}
function playbackFileUrl(value,suffix,query={}){
  const context=playbackFileContext(value); playbackFileSuffix(suffix);
  let q=playbackFileQuery(suffix,query);
  if(context.session_id&&!["decision","hls/sessions"].includes(suffix))
    q+=(q?"&":"")+`session=${context.session_id}`;
  return context.file_base+"/"+suffix+(q?"?"+q:"");
}
function playbackFileApiPath(value,suffix,query={}){
  return playbackFileUrl(value,suffix,query).slice("/api/v1".length);
}

function playbackFileContextForPlayer(player){
  if(!player) playbackFileReject();
  if(player.fileContext!=null){
    if(!PLAYBACK_FILE_CONTEXTS.has(player.fileContext)) playbackFileReject();
    return playbackFileContext(player.fileContext);
  }
  if(player.source_ref||player.file_base) playbackFileReject();
  return localPlaybackFileContext(player.fileId);
}
function playbackFileContextForFile(file){
  if(!file) playbackFileReject();
  if(file.fileContext!=null){
    if(!PLAYBACK_FILE_CONTEXTS.has(file.fileContext)) playbackFileReject();
    const c=playbackFileContext(file.fileContext);
    if(file.file_base&&file.file_base!==c.file_base) playbackFileReject();
    if((file.source_ref||file.file_base)&&c.source_ref.kind==="local") playbackFileReject();
    if(file.source_ref&&(Object.keys(file.source_ref).length!==Object.keys(c.source_ref).length
      ||Object.keys(c.source_ref).some(k=>file.source_ref[k]!==c.source_ref[k]))) playbackFileReject();
    return c;
  }
  if(file.source_ref||file.file_base) playbackFileReject();
  return localPlaybackFileContext(file.id_text!=null?file.id_text:file.id);
}
function samePlaybackFile(player,fileId,meta){
  return !!player&&playbackFileKey(playbackFileContextForPlayer(player))
    ===playbackFileKey(meta&&meta.fileContext||fileId);
}
// Legacy capability strings originate in buildPlayCaps. Decode them into the
// same closed typed vocabulary before they can join any file resource URL.
function playbackFileQueryFromLegacy(text){
  if(typeof text!=="string"||text.length>2048) playbackFileReject();
  const query={};
  if(!text) return query;
  for(const entry of text.split("&")){
    const at=entry.indexOf("="); if(at<1) playbackFileReject();
    const key=entry.slice(0,at),value=decodeURIComponent(entry.slice(at+1));
    if(Object.hasOwn(query,key)) playbackFileReject();
    query[key]=value;
  }
  return query;
}
function playbackFileDecisionMediaUrl(context,url){
  const c=playbackFileContext(context);
  if(c.source_ref.kind==="local") return url;
  if(typeof url!=="string") playbackFileReject();
  for(const suffix of ["direct","stream.mp4"]){
    const base=c.file_base+"/"+suffix;
    if(url!==base&&!url.startsWith(base+"?")) continue;
    const query=playbackFileQueryFromLegacy(url===base?"":url.slice(base.length+1));
    if(Object.hasOwn(query,"session")){
      if(!c.session_id||query.session!==c.session_id) playbackFileReject();
      delete query.session;
    }
    return playbackFileUrl(c,suffix,query);
  }
  playbackFileReject();
}

function playbackFileContextForPlay(fileId,meta){
  if(meta&&meta.fileContext!=null&&!PLAYBACK_FILE_CONTEXTS.has(meta.fileContext)) playbackFileReject();
  const c=playbackFileContext(meta&&meta.fileContext||fileId);
  if(c.source_ref.kind==="local") return localPlaybackFileContext(fileId);
  if(typeof fileId!=="string"||playbackFileDecimal(fileId)!==c.source_file_id) playbackFileReject();
  return c;
}

function sharedPlaybackSessionPlaylist(url,id){
  if(typeof url!=="string"||url.length>512||url.includes("%"))return false;
  const parts=url.split("?");
  if(parts.length>2||!["master.m3u8","index.m3u8","video.m3u8"].some(name=>parts[0]===`/api/v1/hls/${id}/${name}`))return false;
  if(parts.length===1)return true;
  if(!parts[1]||parts[1].length>256)return false;
  const seen=new Set();
  for(const field of parts[1].split("&")){
    const pair=field.split("=");if(pair.length!==2||seen.has(pair[0]))return false;seen.add(pair[0]);
    const [key,value]=pair;
    if(key==="native"){if(!["0","1"].includes(value))return false;}
    else if(key==="subtitle"){if(value!=="-1"&&(!/^(0|[1-9][0-9]{0,3})$/.test(value)||String(Number(value))!==value||Number(value)>4095))return false;}
    else if(key==="diagnostic"){if(!["video-only","video-only-codecs","video-only-range","video-only-hdr"].includes(value))return false;}
    else return false;
  }
  return true;
}

// B's Shared status grammar for this context's started session. Only the
// nested Source metrics come back, and only when the outer subject, session,
// item and file revision are this context's; anything else is no sample at
// all, never something to read as Local status.
function sharedPlaybackStatusMetrics(value,reply){
  const c=playbackFileContext(value);
  if(c.source_ref.kind==="local"||!c.session_id||!reply||typeof reply!=="object"||Array.isArray(reply)) return null;
  const r=reply.reference,status=reply.status;
  if(reply.subject!=="shared"||reply.session_id!==c.session_id||!r||typeof r!=="object"
    ||r.item?.import_id!==c.source_ref.import_id||r.item?.item_id!==c.source_ref.item_id
    ||r.file_id!==c.source_file_id||r.revision!==c.file_revision
    ||!status||typeof status!=="object"||Array.isArray(status)) return null;
  return status;
}
// Ordinary complete B Start reply, bound to the authenticated signed-file
// request. This validates routing metadata; it is not Source producer evidence.
function sharedPlaybackStartContext(value,response){
  const c=playbackFileContext(value);
  if(c.source_ref.kind==="local"||c.session_id||!response||typeof response!=="object") playbackFileReject();
  const id=response.session_id,control=response.control;
  if(typeof id!=="string"||id.length!==36||! /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)
    ||!sharedPlaybackSessionPlaylist(response.playlist_url,id)
    ||response.vod!==true||!Number.isFinite(response.start_seconds)||response.start_seconds<0
    ||response.start_seconds>9007199254740
    ||response.duration_ms!=null&&(!Number.isSafeInteger(response.duration_ms)||response.duration_ms<0)
    ||response.media_origin_ms!=null&&!Number.isSafeInteger(response.media_origin_ms)
    ||!control||control.protocol!=="plurx-playback-control-v1"
    ||control.url!==`/api/v1/hls/${id}/control`
    ||typeof control.generation!=="string"||control.generation.length!==36||! /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(control.generation)
    ||!Number.isSafeInteger(control.control_epoch)||control.control_epoch<=0
    ||control.next_exchange_ms!==5000||control.lease_timeout_ms!==300000) playbackFileReject();
  return withPlaybackFileSession(c,id);
}
// The media types Local direct play can name, which is all B relays for a
// Shared direct session (sharing_direct_wire::DIRECT_MIMES).
const SHARED_DIRECT_MIMES=Object.freeze(["video/mp4","video/webm","video/x-matroska","video/mp2t",
  "video/x-msvideo","audio/mp4","audio/aac","audio/mpeg","audio/flac","audio/ogg","audio/wav",
  "audio/x-ms-wma","application/octet-stream"]);
// B's complete direct Start reply. Exactly five fields: no control route, no
// playlist, and a byte URL that is this context's own file alias bound to the
// returned B session — never a Source, Local or foreign URL.
function sharedPlaybackDirectStartContext(value,response){
  const c=playbackFileContext(value);
  if(c.source_ref.kind==="local"||c.session_id||!response||typeof response!=="object"||Array.isArray(response)) playbackFileReject();
  const keys=["presentation","session_id","url","length","mime"],present=Object.keys(response);
  if(present.length!==keys.length||keys.some(k=>!present.includes(k))) playbackFileReject();
  if(response.presentation!=="direct"||!Number.isSafeInteger(response.length)||response.length<0
    ||!SHARED_DIRECT_MIMES.includes(response.mime)) playbackFileReject();
  const bound=withPlaybackFileSession(c,response.session_id);
  if(response.url!==playbackFileUrl(bound,"direct")) playbackFileReject();
  return bound;
}
