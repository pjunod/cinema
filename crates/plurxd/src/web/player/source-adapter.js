"use strict";
// ---- playback source adapter ---------------------------------------------
// File playback keeps its numeric/string wire id. Managed sources carry the
// finite, path-free identity returned by their owner. The player asks these
// helpers for identity and routes; it never manufactures a MediaFile id for a
// disc title.
function playbackInput(value){
  if(value&&typeof value==="object"&&value.source==="optical"){
    return Object.freeze(Object.assign({},value,{angle:Math.max(1,Number(value.angle)||1)}));
  }
  return value;
}
function playbackInputIsOptical(value){ return !!(value&&typeof value==="object"&&value.source==="optical"); }
function playbackInputFileId(value){ return playbackInputIsOptical(value)?null:value; }
function playbackInputKey(value){
  if(!playbackInputIsOptical(value)) return `file:${String(value)}`;
  return ["optical",value.owner_node_id,value.drive_id,value.media_generation,
    value.disc_id,value.title_id,value.angle].map(part=>String(part??"")).join(":");
}
function playbackInputSame(left,right){ return playbackInputKey(left)===playbackInputKey(right); }
function playbackInputForPlayer(player){
  return player&&player.playbackSource ? player.playbackSource : player&&player.fileId;
}
function playbackInputPresent(player){
  const value=playbackInputForPlayer(player);
  return value!==null&&value!==undefined&&value!=="";
}
function opticalRouteDrive(source){
  return source.route_drive_id||`${source.owner_node_id}:${source.drive_id}`;
}
function opticalDecisionPath(source){
  return `/optical/drives/${encodeURIComponent(opticalRouteDrive(source))}`+
    `/titles/${encodeURIComponent(source.title_id)}/decision`;
}
function opticalSessionPath(source){
  return `/optical/drives/${encodeURIComponent(opticalRouteDrive(source))}`+
    `/titles/${encodeURIComponent(source.title_id)}/sessions`;
}
function opticalProgressPath(source){
  return `/optical/discs/${encodeURIComponent(source.disc_id)}`+
    `/titles/${encodeURIComponent(source.title_id)}/progress`;
}
