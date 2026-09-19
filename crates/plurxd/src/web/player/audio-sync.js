"use strict";
// ---- audio sync (A/V offset) ----------------------------------------------
// Scoped to this player only. Every nudge restarts at the same position with
// the shift on that stream request; closing or starting another title resets.
function offsetLabel(ms){ return ms===0?"0 ms (no correction)":`${ms>0?"+":""}${ms} ms — audio plays ${ms>0?"later":"earlier"}`; }
function syncMenuHtml(){
  return `<div class="amsec">Audio sync</div>
    <div class="syncnow">${esc(offsetLabel(PLAYER.aoffset||0))}
      ${PLAYER.declared?`<div class="hint">container declares ${PLAYER.declared>0?"+":""}${PLAYER.declared} ms (already honored)</div>`:''}</div>
    <button onclick="nudgeSync(-250)">−250 ms · audio earlier</button>
    <button onclick="nudgeSync(-50)">−50 ms · audio earlier</button>
    <button onclick="nudgeSync(50)">+50 ms · audio later</button>
    <button onclick="nudgeSync(250)">+250 ms · audio later</button>
    ${PLAYER.aoffset? `<button onclick="setSync(0)">reset to 0</button>`:''}`;
}
function nudgeSync(d){ setSync((PLAYER.aoffset||0)+d); }
async function setSync(v){
  const me=PLAYER; if(!me||!me.fileId) return;
  PLAYER.aoffset=Math.max(-15000,Math.min(15000,Math.round(v)));
  const m=document.getElementById("pmenu");
  if(m&&m.classList.contains("on")&&m.dataset.kind==="settings") refreshPlayerSettingsMenu();
  toast("Audio sync: "+offsetLabel(PLAYER.aoffset));
  const video=document.getElementById("video");
  const pos=positionForPlaybackIntent(video,PLAYER);
  if(me.pendingMediaChange){
    beginPlaybackControlSeek(me,pos);
    return executePlaybackMediaChange(me,me.pendingMediaChange);
  }
  if(PLAYER.method==='direct_play'){
    // A direct file has nowhere to apply a correction, so this playback alone
    // moves onto the remux path. The next play still begins from its decision.
    PLAYER.method='remux';
    PLAYER.copyHls=useNativeHls(video);
  }
  // Cached VOD seeks stay inside the old recipe, so a seek alone cannot apply
  // a new audio offset. Rebuild the decision while preserving this intent.
  beginPlaybackControlSeek(me,pos);
  PENDING_ATTEMPT_REASON="audio-sync";
  play(me.fileId,me.title||"",Math.round(pos*1000),me.knownDur||0,me.meta);
}
// Subtitles. The VTT the server extracts carries the file's own timestamps. A
// stream that starts at an offset (a transcode, a copy-HLS session, a resumed
// remux) reports currentTime from that offset, so a plain <track> would run
// ahead by exactly the offset. Two paths: no offset → hand the <track> to the
// browser; offset → fetch the VTT once and re-add the cues shifted back, into
// one script-created track we keep reusing. pbTick() re-applies when the offset
// moves, since seeking a transcode restarts the session at a new position.
function subUrl(index){ return tok(`/api/v1/files/${PLAYER.fileId}/subs/${index}`); }
function subLabelFor(s, index){ return s?subLabelMenu(s):("Subtitle "+(index+1)); }
function clearSubs(v){
  [...v.querySelectorAll("track")].forEach(t=>t.remove());
  try{ for(const t of v.textTracks) t.mode="disabled"; }catch(e){}
}
function vttTime(s){
  const m=/^(?:(\d+):)?(\d{1,2}):(\d{2})(?:[.,](\d{1,3}))?$/.exec(String(s).trim());
  return m ? (+(m[1]||0))*3600+(+m[2])*60+(+m[3])+(+((m[4]||"0").padEnd(3,"0")))/1000 : null;
}
// Minimal WebVTT reader — enough for what ffmpeg emits: a WEBVTT header, blank
// -line-separated blocks, an optional cue id line, "start --> end [settings]",
// then the payload. Inline tags stay in the text for the browser to render.
function vttParse(text){
  const out=[];
  for(const b of String(text).replace(/\r\n?/g,"\n").split(/\n{2,}/)){
    const lines=b.split("\n").filter(l=>l.trim()!=="");
    const i=lines.findIndex(l=>l.includes("-->"));
    if(i<0) continue;                                   // WEBVTT header, NOTE, STYLE, REGION
    const parts=lines[i].split("-->");
    const st=vttTime(parts[0]);
    const rest=(parts[1]||"").trim().split(/\s+/);
    const en=vttTime(rest.shift()||"");
    const body=lines.slice(i+1).join("\n");
    if(st==null||en==null||en<=st||!body) continue;
    out.push({start:st,end:en,text:body});
  }
  return out;
}
// Keep this playback's track choice current, so an internal reopen (a quality
// change) reproduces what is on screen instead of falling back to the
// cold-start policy default. Same shape as a pre-play selection, because it is
// the same thing: what this one playback is watching.
function rememberPlaybackSelection(kind, index){
  if(!PLAYER) return;
  PLAYER.preplay=Object.assign({audio:null,subtitle:null}, PLAYER.preplay||{}, {[kind]:index});
}
async function setSub(index){
  closeMenu();
  const me=PLAYER; if(!me) return;
  endWait(false); // a viewer-directed track change supersedes stall recovery
  // A bitmap subtitle (PGS on a Blu-ray remux, VobSub on a DVD one) is a
  // picture, not text. There is nothing to hand a <track>, so the only way to
  // show it is to have the server draw it into the frames — which means a
  // transcode, and a new one, because the burn is baked in. Everything else
  // here is the text path, which stays local and free.
  const want=(me.subs||[]).find(x=>x.index===index);
  const burnAction=PlaybackPolicy.subtitleBurnAction({
    requiresBurn:index>=0&&subNeedsBurn(want), deliveredRange:me.deliveredRange
  });
  if(burnAction==="keep_hdr"){
    raisePlaybackSurface("degraded_notice",
      {title:"That subtitle requires an SDR burn-in. HDR playback was kept unchanged."});
    return false;   // nothing changed, so nothing to remember
  }
  const video=document.getElementById("video");
  const position=positionForPlaybackIntent(video,me);
  me.curSub=index;
  if(burnAction==="burn"||me.burnedSub!=null){
    beginPlaybackControlSeek(me,position);
  }else{
    supersedePlaybackControlIntent(me,{preserveHlsStartup:
      !me.pendingOpenAttempt&&!me.retiringOpenAttempt&&!me.pendingMediaChange});
    notifyPlaybackControl();
  }
  rememberPlaybackSelection("subtitle", index);
  if(burnAction!=="burn"&&me.burnedSub!=null) return burnSub(null);
  if(burnAction==="burn") me.burnedSub=index;
  if(restartPendingPlaybackOpen(me,"subtitle")) return true;
  if(burnAction==="burn"){ await burnSub(index); return true; }
  if(me.burnedSub!=null){
    // Leaving a burned track behind: the picture still has it drawn on, so
    // "off" also has to be a restart.
    return burnSub(index>=0?index:null);
  }
  const v=video; if(!v) return;
  pbSyncSubIcon();
  clearSubs(v);
  notifyPlaybackControl();
  if(index<0){ me._subOff=null; return; }
  const off=me.offset||0;
  me._subOff=off;                       // set before awaiting: stops pbTick re-firing
  const s=(me.subs||[]).find(x=>x.index===index);
  const nativeOrdinal=nativeHlsSubtitleOrdinal(me,index);
  if(me.sessionId&&nativeOrdinal>=0){
    if(me.hls){
      me.hls.subtitleTrack=nativeOrdinal;
    }else if(v.textTracks&&v.textTracks[nativeOrdinal]){
      v.textTracks[nativeOrdinal].mode="showing";
    }
    return;
  }
  if(!off){
    const tr=document.createElement("track");
    tr.kind="subtitles"; tr.dataset.idx=index; tr.label=subLabelFor(s,index); tr.srclang=(s&&s.language)||"und";
    tr.src=subUrl(index);
    v.appendChild(tr);
    // Setting mode triggers the fetch; do it after the element registers.
    setTimeout(()=>{ if(PLAYER===me && me.curSub===index && tr.track) tr.track.mode="showing"; },30);
    return;
  }
  let text;
  try{
    const r=await fetch(subUrl(index));
    if(!r.ok) throw new Error("HTTP "+r.status);
    text=await r.text();
  }catch(err){
    if(PLAYER===me && me.curSub===index) toast("Couldn't load that subtitle track");
    return;
  }
  if(PLAYER!==me || me.curSub!==index || (me.offset||0)!==off) return;  // switched away mid-fetch
  // One script track per <video>, reused: a script-created track can't be
  // removed, so re-adding one per seek would pile them up over a session.
  if(!v._vsubs) v._vsubs=v.addTextTrack("subtitles","Subtitles");
  const track=v._vsubs;
  // A disabled track reports `cues` as null, so the old cues would survive the
  // clear and stack up behind the new ones — wake it before emptying it.
  if(track.mode==="disabled") track.mode="hidden";
  try{ while(track.cues && track.cues.length) track.removeCue(track.cues[0]); }catch(err){}
  const Cue=window.VTTCue||window.TextTrackCue;
  for(const c of vttParse(text)){
    const en=c.end-off;
    if(en<=0) continue;                                 // cue ends before playback began
    try{ track.addCue(new Cue(Math.max(0,c.start-off), en, c.text)); }catch(err){}
  }
  track.mode="showing";
}
// Re-open the stream as a transcode with `index` burned into the picture, or
// with nothing burned when `index` is null.
//
// This is the one subtitle action that costs a stream restart, and it is
// unavoidable: a burned subtitle is part of the frames, so changing it means
// changing the frames. It also forces a transcode even on a file that was
// direct-playing happily — you cannot draw on a stream you are copying.
async function burnSub(index){
  const v=document.getElementById("video"); if(!v||!PLAYER) return;
  endWait(false);
  const me=PLAYER;
  const pos=positionForPlaybackIntent(v,me);
  // Burning nothing is not a transcode wish. This used to open another
  // transcode session with no burn in it — so a viewer who turned subtitles
  // OFF stayed in the re-encode the burn had forced, at the burn's resolution,
  // forever. Go back through the decision instead: that is what restores the
  // direct play / remux (and the picture) the burn took away. The one-shot
  // flag stops play() from re-applying the default track, which on a film
  // whose default is the forced bitmap track would burn it straight back in.
  if(index==null){
    me.burnedSub=null;
    me.pendingMediaChange=null;
    PENDING_ATTEMPT_REASON="subtitle-off";
    PENDING_DEFAULT_SUB_OFF=true;
    toast("Subtitles off");
    play(me.fileId, me.title||"", Math.round(pos*1000), me.knownDur||0, me.meta);
    return;
  }
  me.curSub=index;
  me.burnedSub=index;
  pbSyncSubIcon();
  const s=(me.subs||[]).find(x=>x.index===index);
  raisePlaybackSurface("client_preparing",{title:"Burning in subtitles…",
    detail:`${subLabelFor(s,index)} — this format is a picture, so the server draws it in`});
  // The overlay's Reason row otherwise keeps quoting the /decision verdict —
  // "forced original quality (no video transcode)" — over a session that IS a
  // transcode, which sent a whole debugging session in the wrong direction.
  // The decision was about the FILE; this line is about the session.
  me.reasons=[`subtitle burn-in (${subLabelFor(s,index)}) — drawing subtitles into the picture forces a re-encode`
              +(sessionHeight()?(qualityForce()==="original"
                  ?" · kept at source resolution (Original)"
                  :" · kept at source resolution (the verdict was to send the source)"):"")];
  const attached=await requestPlaybackMediaChange(me,{method:'transcode',copyHls:false,reason:"subtitle-burn"});
  if(attached) toast("Subtitles: "+subLabelFor(s,index));
}
// Cycle Off → each track → Off (the 'c' hotkey and the CC button's long game).
async function cycleSub(){
  if(!PLAYER||!PLAYER.subs||!PLAYER.subs.length) return;
  const idxs=PLAYER.subs.map(s=>s.index);
  const at=idxs.indexOf(PLAYER.curSub);
  const next=at<0?idxs[0]:(at+1>=idxs.length?-1:idxs[at+1]);
  const changed=await setSub(next);
  if(changed===false) return;
  const s=PLAYER.subs.find(x=>x.index===next);
  toast("Subtitles: "+(next<0?"off":subLabelFor(s,next)));
}
async function switchAudio(idx){
  closeMenu();
  const me=PLAYER, v=document.getElementById("video");
  if(!me||!v||idx===me.curAudio) return;
  endWait(false); // cancel a timer armed for the stream this switch replaces
  me.curAudio=idx;
  renderPlayerInfo();
  const pos=positionForPlaybackIntent(v,me);
  const track=me.audio[idx]; const aidx=track?track.index:idx;
  rememberPlaybackSelection("audio", aidx);
  beginPlaybackControlSeek(me,pos);
  if(restartPendingPlaybackOpen(me,"audio")) return;
  const attached=await requestPlaybackMediaChange(me,{method:me.method==='transcode'?'transcode':'remux',
    copyHls:!!me.copyHls,reason:"audio"});
  if(attached) toast("Audio: "+(langName(track&&track.language)||("Track "+(idx+1))));
}

