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
function sharedPlaybackFileContextFromDetail(reference,file){
  if(!reference||!file||typeof reference!=="object") playbackFileReject();
  const keys=["import_id","server_id","catalogue_epoch","library_id","item_id"];
  if(Object.keys(reference).length!==keys.length||keys.some(k=>!Object.hasOwn(reference,k))) playbackFileReject();
  for(const k of keys.slice(0,3)) if(typeof reference[k]!=="string"||!PLAYBACK_FILE_UUID.test(reference[k])) playbackFileReject();
  for(const k of keys.slice(3)) playbackFileDecimal(reference[k]);
  // No decoding or URL normalization: encoded separators, queries, fragments,
  // dot segments and authorities must never acquire another interpretation.
  const prefix=`/api/v1/shared/imports/${reference.import_id}/files/`;
  const base=file.file_base;
  if(typeof base!=="string"||!base.startsWith(prefix)
    ||! /^[A-Za-z0-9_-]{1,2048}$/.test(base.slice(prefix.length))) playbackFileReject();
  const source_file_id=playbackFileDecimal(file.id);
  return registerPlaybackFileContext({source_ref:Object.freeze({...reference}),
    source_file_id,auth_generation:AUTH_GENERATION,file_base:base,session_id:null});
}
function playbackFileContext(value){
  if(value&&typeof value==="object"){
    if(PLAYBACK_FILE_CONTEXTS.has(value)){
      if(value.source_ref.kind!=="local"&&value.auth_generation!==AUTH_GENERATION) playbackFileReject();
      return value;
    }
    if(value.source_ref||value.file_base) playbackFileReject();
  }
  return localPlaybackFileContext(value);
}
function withPlaybackFileSession(value,id){
  const context=playbackFileContext(value);
  if(typeof id!=="string"||! /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)) playbackFileReject();
  return registerPlaybackFileContext({...context,session_id:id});
}
function playbackFileKey(value){
  const c=playbackFileContext(value), r=c.source_ref;
  return r.kind==="local"?r.file_id:JSON.stringify([c.auth_generation,r.import_id,r.server_id,
    r.catalogue_epoch,r.library_id,r.item_id,c.file_base]);
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
  const capabilities=["vcodec","acodec","container","dvprofile","hdr","dv","hdr10t","maxheight"];
  const allowed=suffix==="decision"?capabilities.concat(["force","audio","subtitle","audio_offset_ms"])
    :suffix==="stream.mp4"?capabilities.concat(["audio","audio_offset_ms","start","stream"])
      :suffix.startsWith("chapters/")?["v"]:[];
  return Object.entries(query).map(([key,value])=>{
    if(!allowed.includes(key)) playbackFileReject();
    const text=String(value);
    if(["hdr","dv","hdr10t"].includes(key)){
      if(!/^[01]$/.test(text)) playbackFileReject();
    }else if(["vcodec","acodec","container","dvprofile"].includes(key)){
      const values={vcodec:["h264","hevc","hevc10","av1","vp9","vp8"],
        acodec:["aac","mp3","opus","flac","ac3","eac3"],
        container:["mp4","webm","mov","m4a","m4b","mp3","aac","flac","ogg","opus","wav"],
        dvprofile:["5","8"]};
      if(text.length>256||text!==""&&text.split(",").some(v=>!values[key].includes(v))) playbackFileReject();
    }else if(key==="force"){
      if(!/^(auto|original|direct|remux|transcode|[1-9][0-9]{0,4})$/.test(text)) playbackFileReject();
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
