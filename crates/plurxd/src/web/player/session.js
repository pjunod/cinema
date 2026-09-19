"use strict";
// ---- session lifecycle -----------------------------------------------------
// One identity per player instance, for the life of this page. Supersession is
// keyed by it server-side, which is what stops two devices signed in to one
// account from killing each other's streams — and lets this player's own
// seeks, quality changes and audio switches replace its previous stream, which
// is exactly what they should do.
const PLAYBACK_ID=(function(){
  try{ if(crypto&&crypto.randomUUID) return crypto.randomUUID(); }catch(e){}
  return "pb-"+Math.random().toString(36).slice(2)+Date.now().toString(36);
})();
// The control endpoint deliberately requires a UUID so an untrusted client
// cannot create an unbounded identity vocabulary. Keep this separate from the
// older playback_id fallback, whose server contract predates that rule.
const CONTROL_CLIENT_ID=(function(){
  try{ if(crypto&&crypto.randomUUID) return crypto.randomUUID(); }catch(e){}
  let seed=Date.now();
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g,c=>{
    const r=(seed+Math.random()*16)%16|0; seed=Math.floor(seed/16);
    return (c==='x'?r:(r&3)|8).toString(16);
  });
})();
function newRequestId(){
  try{ if(crypto&&crypto.randomUUID) return crypto.randomUUID(); }catch(e){}
  return "rq-"+Math.random().toString(36).slice(2)+Date.now().toString(36);
}
// A progressive remux's telemetry handle. Fresh per stream rather than reusing
// PLAYBACK_ID, because a seek starts a *new* ffmpeg while the old response is
// still draining: one id for both would have the two of them writing to the
// same counters, and the overlay would read a delivery rate that is the sum of
// a stream and its own corpse.
let STREAM_SEQ=0;
function newStreamId(){ return PLAYBACK_ID+"-s"+(++STREAM_SEQ); }
// Build a `/stream.mp4` URL and claim a telemetry handle for it. Every
// progressive remux is built here — first play, seek, audio switch — because
// each of those is a *new* ffmpeg, and a handle that outlived its process
// would have the overlay reporting a dead stream's numbers as live.
function remuxUrl(base, audioIndex, startSec){
  PLAYER.streamId=newStreamId(); PLAYER.sessionId=null;
  let u=tok(base)+"&"+CAPS_Q;
  if(audioIndex!=null) u+="&audio="+audioIndex;
  if(PLAYER.aoffset) u+="&audio_offset_ms="+PLAYER.aoffset;
  if(startSec>0) u+="&start="+startSec.toFixed(1);
  return u+"&stream="+encodeURIComponent(PLAYER.streamId);
}
// Open a stream. POST, not GET: this spawns a process and supersedes the
// previous one, and a GET that does that can be replayed by anything that
// believes GETs are safe.
// The options every transcode session open shares.
//
// A helper because there are four of them — first play, the error fallback, a
// seek, an audio switch — and any one that forgot a field would drop it in
// silence. A seek that omits the burned subtitle simply loses the subtitles,
// which reads to a viewer as a bug in the subtitles rather than in the seek,
// and that is exactly the shape of thing that survives a review.
function transcodeOpts(startSec, audioIndex, autoHeightOverride){
  let height=transcodeHeight();
  if(height==null && qualityForce()==='auto'){
    height=autoHeightOverride>0?autoHeightOverride:(PLAYER&&PLAYER.autoHeight);
  }
  const o={height, start:startSec};
  // No explicit rung: the session may still owe the viewer the source's
  // resolution (see sessionHeight). Every transcode open shares this helper
  // precisely so the burn, the fallback, a seek and an audio switch cannot
  // disagree about it mid-session.
  if(o.height==null) o.height=sessionHeight();
  if(audioIndex!=null) o.audio=audioIndex;
  // `/decision` already proved the browser can decode Main10 PQ and selected
  // the HDR10 grade. Preserve that immutable request across every session
  // open; `deliveredRange` is display truth and mutates when a session lands,
  // so using it here would turn one SDR fallback into permanent SDR on seek.
  if(PLAYER && PLAYER.requestHdr10) o.hdr10=true;
  if(PLAYER && PLAYER.burnedSub!=null) o.subtitle_burn=PLAYER.burnedSub;
  if(PLAYER && PLAYER.aoffset) o.audio_offset_ms=PLAYER.aoffset;
  return o;
}
// The track whose checkmark `/decision` supplied. Every cold-start transport
// carries it explicitly; otherwise progressive remuxes fall back to stream 0
// while the menu continues to claim the preferred language is playing.
function selectedAudioIndex(player){
  const p=player||PLAYER;
  const track=p&&p.audio&&p.audio[p.curAudio];
  return track?track.index:0;
}
function playbackControlCapabilities(){
  const codecs=["h264"];
  const advertised=String(PLAY_CAPS&&PLAY_CAPS.vcodec||"").split(",");
  if(advertised.includes("hevc")) codecs.push("hevc");
  if(advertised.includes("av1")) codecs.push("av1");
  const ranges=["sdr"];
  if(PLAY_CAPS&&PLAY_CAPS.hdr10t) ranges.push("hdr10");
  if(PLAY_CAPS&&PLAY_CAPS.dv) ranges.push("dolby_vision");
  const displayPx=Math.max((typeof screen!=="undefined"&&screen.height)||0, window.innerHeight||0)
    * Math.max(1, window.devicePixelRatio||1);
  const decoderPx=Number(PLAY_CAPS&&PLAY_CAPS.maxheight)||displayPx;
  const px=Math.min(displayPx||decoderPx,decoderPx||displayPx);
  return {platform:"web",max_height:Math.max(144,Math.min(2160,Math.round(px)||1080)),
    codecs,dynamic_ranges:ranges,dual_player_preparation:preparedHandoffOffered(PLAYER)};
}
function playbackControlSelection(p){
  const requested=playQuality();
  const manualHeight=/^\d+$/.test(String(requested||""))?Number(requested):null;
  const quality=["original","nomse"].includes(requested)
    ? {mode:"original"}
    : manualHeight!=null
      ? {mode:"manual",height:Math.max(144,Math.min(2160,manualHeight))}
      // D3-a: Auto carries the rung the client's own controller wants.
      //
      // `p.autoHeight` is what is being DELIVERED; this is what was asked for,
      // and they are deliberately different fields. Without the ask on the
      // wire an Auto controller moving 720 -> 1080 sends the same selection it
      // sent at 720, the server's digest is unchanged, and the exchange is not
      // a selection change at all - so no successor is ever staged for it.
      // Absent means plain Auto, which digests exactly as it always has.
      : (p&&p.autoRequestedHeight>0)
        ? {mode:"auto",height:Math.max(144,Math.min(2160,Math.round(p.autoRequestedHeight)))}
        : {mode:"auto"};
  const selected=(p&&p.curSub>=0)?p.curSub:null;
  const subtitle=selected==null ? {mode:"off",track:null}
    : {mode:p.burnedSub!=null?"burn":"native",track:selected};
  const audioTrack=Math.max(0,Math.min(1024,Math.round(selectedAudioIndex(p))));
  const audioOffset=Math.max(-15000,Math.min(15000,Math.round(p&&p.aoffset||0)));
  return {quality,audio_track:audioTrack,subtitle,
    audio_offset_ms:audioOffset,codec:"auto",dynamic_range:"auto"};
}
function playbackControlBufferedRange(v,p,positionMs){
  try{
    const local=(v.currentTime||0), origin=p.offset||0;
    for(let i=0;i<v.buffered.length;i++){
      if(v.buffered.start(i)<=local+0.25 && v.buffered.end(i)>=local){
        const from=Math.max(0,Math.round((origin+v.buffered.start(i))*1000));
        const through=Math.max(positionMs,Math.round((origin+v.buffered.end(i))*1000));
        return {from,through};
      }
    }
  }catch(e){}
  return {from:null,through:positionMs};
}
// Recovery handlers know facts the media element cannot expose: whether a
// persistent wait ran out of bytes or stalled with bytes already buffered,
// and whether hls.js stopped on a manifest, network, media, or decoder error.
// Admit only protocol enums and a tightly-bounded single-line detail so a
// library error object can never leak arbitrary fields into the wire request.
function playbackControlObservationOverride(value){
  if(!value||typeof value!=="object") return null;
  const observation={};
  if(["unknown","ready","starved","failed"].includes(value.decoder_state))
    observation.decoder_state=value.decoder_state;
  if(["network","manifest","media","decoder","drm","unknown"].includes(value.error_code))
    observation.error_code=value.error_code;
  if(observation.error_code && value.error_detail!=null){
    const detail=String(value.error_detail).replace(/[\r\n\0]/g," ").slice(0,120);
    if(detail) observation.error_detail=detail;
  }
  return Object.keys(observation).length?observation:null;
}
function playbackControlHlsFatal(data,started){
  const detail=String(data&&data.details||"");
  const type=String(data&&data.type||"");
  const errorTypes=typeof Hls!=="undefined"&&Hls.ErrorTypes;
  const mediaFailure=!!((errorTypes && data&&data.type===errorTypes.MEDIA_ERROR)
    || /codec|decode|parsing/i.test(detail));
  const manifestFailure=/manifest/i.test(detail);
  const networkFailure=!mediaFailure && !!((errorTypes&&data&&data.type===errorTypes.NETWORK_ERROR)
    || /network|loaderror|loadtimeout/i.test(type+" "+detail));
  const observation=mediaFailure
    ? {decoder_state:"failed",error_code:/codec|decode/i.test(detail)?"decoder":"media",
        error_detail:detail||type||"hls_media_fatal"}
    : {decoder_state:started?"ready":"unknown",
        error_code:manifestFailure?"manifest":networkFailure?"network":"unknown",
        error_detail:detail||type||"hls_fatal"};
  return {media_failure:mediaFailure,observation};
}
function playbackControlSnapshot(v,p){
  if(!v||!p) return null;
  const pendingSeek=p.controlSeek||null;
  const seeking=!!pendingSeek || !!v.seeking || p._seekPreview!=null;
  const hinted=!p.started&&p.controlPositionHintSec!=null?p.controlPositionHintSec:null;
  const preview=!pendingSeek&&seeking&&p._seekPreview!=null
    ? Math.max(0,p._seekPreview-((p.bookOffset||0)/1000)) : null;
  let positionMs=Math.max(0,Math.round((hinted!=null?hinted:
    preview!=null?preview:(p.offset||0)+(v.currentTime||0))*1000));
  const duration=Math.max(0,Number(p.durMs||p.knownDur||0));
  if(duration>0) positionMs=Math.min(positionMs,duration);
  const range=hinted!=null?{from:null,through:positionMs}
    :playbackControlBufferedRange(v,p,positionMs);
  // `ended` means the end of the bytes the browser received, not necessarily
  // the title. The legacy early-end handler reopens a truncated stream, so it
  // is active failed demand until it is actually within the completion slack.
  const terminalEnded=!!v.ended && (!(duration>0)
    || positionMs>=Math.max(0,duration-ENDED_SLACK_SEC*1000));
  const wantsPlayback=p.wantsPlayback??!v.paused;
  const demand=(terminalEnded?"end":v.ended?"active":wantsPlayback?"active":"hold");
  let render="rendering";
  if(v.error) render="failed";
  else if(v.ended) render=terminalEnded?"ended":"failed";
  else if(p.controlRenderOverride) render=p.controlRenderOverride;
  else if(seeking) render="seeking";
  else if(!p.started) render="starting";
  else if(p.waitAt && performance.now()-p.waitAt>=PERSISTENT_STALL_MS) render="stalled";
  else if(p.waitAt || (!v.paused&&v.readyState<3)) render="waiting";
  const rate=Number(v.playbackRate||0);
  const playbackRate=demand==="active"
    ? Math.max(0.25,Math.min(4,rate||1)) : Math.max(0,Math.min(4,rate));
  let dropped=null;
  try{
    const q=v.getVideoPlaybackQuality&&v.getVideoPlaybackQuality();
    if(q&&Number.isFinite(q.droppedVideoFrames)) dropped=Math.max(0,Math.round(q.droppedVideoFrames));
  }catch(e){}
  const observation={decoder_state:v.error?"failed":render==="stalled"?"starved":p.started?"ready":"unknown"};
  if(dropped!=null) observation.dropped_frames=dropped;
  if(v.error){ observation.error_code="media"; observation.error_detail=`media_code_${v.error.code||0}`; }
  // The element-level error is a generic fallback. A recovery callback may
  // know the actual HLS class, so its bounded evidence takes precedence.
  const triggerObservation=playbackControlObservationOverride(p.controlObservationOverride);
  if(triggerObservation) Object.assign(observation,triggerObservation);
  const bps=p.hls&&Number(p.hls.bandwidthEstimate);
  const snapshot={demand,position_ms:positionMs,buffered_from_ms:range.from,
    buffered_through_ms:range.through,playback_rate:playbackRate,render_state:render,
    seek_target_ms:render==="seeking"
      ? (pendingSeek?pendingSeek.targetMs:positionMs) : null,
    observed_download_bps:Number.isFinite(bps)&&bps>0?Math.round(bps):null,
    selection:playbackControlSelection(p),capabilities:playbackControlCapabilities(),
    observation,acknowledgement:pendingPlaybackControlAcknowledgement(p,demand)};
  // A commit and terminal demand are both true, but the protocol deliberately
  // refuses them in one exchange: publishing the successor has to win before
  // ending the newly-published session. Carry the commit on one synthetic
  // active snapshot; once the accepted acknowledgement leaves the queue, the
  // next capture reports the element's real `end` demand.
  if(snapshot.demand==="end"&&snapshot.acknowledgement?.state==="committed"){
    snapshot.demand="active";
    snapshot.playback_rate=Math.max(0.25,snapshot.playback_rate||0);
  }
  return snapshot;
}

