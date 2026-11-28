"use strict";

let AI_CAPTION_PLAYBACK=null;
const AI_CAPTION_OPTIONS=new WeakMap();
const AI_CAPTION_ITEM=new WeakMap();
const AI_CAPTION_PLAYER_FILES=new WeakMap();

function aiCaptionPath(file){return `/files/${encodeURIComponent(file.id)}/ai-subtitles`;}
function aiCaptionOperationPath(id){return `/ai-subtitles/operations/${encodeURIComponent(id)}`;}
function aiCaptionLocal(file){return !!file?.available&&!!file.video_codec&&playbackFileContextForFile(file).source_ref.kind==="local";}
function aiCaptionActive(operation){return operation?.state==="queued"||operation?.state==="working";}

function aiCaptionPlayerFile(){
  const player=PLAYER;
  if(!player?.fileId||player.libraryChannel||playbackFileContextForPlayer(player).source_ref.kind!=="local")return null;
  const detail=typeof watchCurrentFile==="function"?watchCurrentFile():null;
  if(detail&&String(detail.id)===String(player.fileId))return detail;
  const cached=AI_CAPTION_PLAYER_FILES.get(player);
  if(cached)return cached;
  const file={
    id:player.fileId,fileContext:playbackFileContextForPlayer(player),
    available:true,video_codec:player.source?.video_codec,duration_ms:player.durMs||player.knownDur
  };
  AI_CAPTION_PLAYER_FILES.set(player,file);
  return file;
}

function aiCaptionReadOptions(file,audio=null){
  if(!aiCaptionLocal(file))return Promise.resolve(null);
  const auth=AUTH_GENERATION;
  const cached=AI_CAPTION_OPTIONS.get(file);
  if(cached?.auth===auth&&cached.audio===audio&&Date.now()-cached.at<15000)return cached.request;
  const request=api(aiCaptionPath(file)+(audio==null?"":`?audio_index=${encodeURIComponent(audio)}`)).catch(error=>{
    if(AI_CAPTION_OPTIONS.get(file)?.request===request)AI_CAPTION_OPTIONS.delete(file);
    throw error;
  });
  AI_CAPTION_OPTIONS.set(file,{auth,audio,request,at:Date.now(),options:null});
  request.then(options=>{
    const entry=AI_CAPTION_OPTIONS.get(file);
    if(entry?.request===request&&auth===AUTH_GENERATION)entry.options=options;
  },()=>{});
  return request;
}

function aiCaptionMenu(file){
  if(!aiCaptionLocal(file)||!PLAYER?.mediaAttachment)return "";
  const current=AI_CAPTION_PLAYBACK;
  if(current?.player===PLAYER){
    const alternatives=current.operation?.route==="text"
      ? `<button type="button" onclick="aiCaptionStartPlayback('audio')">Use the audio instead</button>`
      : '<button type="button" onclick="aiCaptionLanguageOptions()">Wrong language?</button>';
    return `<button class="sel" type="button" onclick="aiCaptionStopPlayback()">AI subtitles · ${esc(current.status)}<small>Stop</small></button>
      ${alternatives}<div id="ai-language-options"></div>
      <div class="amsec">Timing</div><button type="button" onclick="aiCaptionShift(-250)">Earlier by 250 ms</button>
      <button type="button" onclick="aiCaptionShift(250)">Later by 250 ms</button>`;
  }
  const audio=translationAttachedAudioOrdinal(PLAYER);
  const entry=AI_CAPTION_OPTIONS.get(file),options=entry?.auth===AUTH_GENERATION&&entry.audio===audio?entry.options:null;
  if(!options){
    const owner=PLAYER;
    void aiCaptionReadOptions(file,audio).then(()=>{
      const mount=document.getElementById("ai-caption-menu");
      if(PLAYER===owner&&mount&&aiCaptionPlayerFile()===file){
        mount.innerHTML=aiCaptionMenu(file);
        mount.querySelector("button")?.setAttribute("role","menuitem");
      }
    },()=>{});
    return '<div id="ai-caption-menu"></div>';
  }
  if(!options.allowed||options.existing_index!=null||!options.route)return "";
  return `<button type="button" onclick="aiCaptionStartPlayback()">AI subtitles<small>${esc(options.label||"Create English subtitles")}</small></button>`;
}

function aiCaptionOwnsSession(session){
  return AI_CAPTION_PLAYBACK===session&&AUTH_GENERATION===session.auth;
}
function aiCaptionPlaybackCurrent(session){
  return aiCaptionOwnsSession(session)&&!session.rebinding&&
    PLAYER===session.player&&PLAYER?.mediaAttachment===session.attachment&&
    PLAYER.captionIntentSerial===session.serial&&session.video.isConnected;
}
function aiCaptionPosition(session){
  return Math.max(0,Math.min(14399999,Math.floor(positionForPlaybackIntent(session.video,session.player)*1000)));
}
function aiCaptionRelease(session,id){
  if(id&&session.auth===AUTH_GENERATION){
    const path=aiCaptionOperationPath(id);
    void api(`${path}/viewer?playback_id=${encodeURIComponent(session.playbackId)}`,{method:"DELETE"}).catch(()=>{});
  }
}
function aiCaptionCoverage(session,result){
  const bar=document.getElementById("ptimeline");
  if(!bar)return;
  const duration=session.duration||Number(session.player.durMs)||0;
  const ready=Number(result?.ready_to_ms)||0;
  const begin=Number(result?.ready_from_ms)||0;
  if(duration>0&&ready>begin){
    bar.style.setProperty("--ai-caption-progress",Math.min(100,ready/duration*100)+"%");
    bar.style.setProperty("--ai-caption-start",Math.max(0,Math.min(100,begin/duration*100))+"%");
  }else{
    bar.style.removeProperty("--ai-caption-progress");
    bar.style.removeProperty("--ai-caption-start");
  }
}
function aiCaptionRetirePlayback(){
  const session=AI_CAPTION_PLAYBACK;
  if(!session)return;
  AI_CAPTION_PLAYBACK=null;
  clearTimeout(session.timer);
  session.controller.abort();
  session.video.removeEventListener("seeked",session.seek);
  if(session.track)session.track.mode="disabled";
  aiCaptionCoverage(session,null);
  if(session.operation?.route!=="download")aiCaptionRelease(session,session.operation?.id);
  pbSyncSubIcon();
}
function aiCaptionAttachmentWillChange(player){
  const session=AI_CAPTION_PLAYBACK;
  if(!session)return;
  if(String(player?.fileId)!==String(session.file.id)||
     playbackFileContextForPlayer(player).source_ref.kind!=="local"||
     translationAudioOrdinal(player)!==session.audioIndex){
    aiCaptionRetirePlayback();
    return;
  }
  session.rebinding=true;
  session.window++;
  session.controller.abort();
  session.controller=new AbortController();
  clearTimeout(session.timer);
  if(session.track)session.track.mode="disabled";
}
function aiCaptionAttachmentDidBegin(player,token){
  const session=AI_CAPTION_PLAYBACK;
  if(!session?.rebinding||String(player.fileId)!==String(session.file.id))return;
  const video=document.getElementById("video");
  if(!video)return;
  if(video!==session.video){
    session.video.removeEventListener("seeked",session.seek);
    session.video=video;
    session.video.addEventListener("seeked",session.seek);
    session.track=null;
  }
  session.player=player;
  session.attachment=token;
  session.serial=player.captionIntentSerial;
}
function aiCaptionAttachmentPositionReady(player,token){
  const session=AI_CAPTION_PLAYBACK;
  if(!session?.rebinding||session.player!==player||session.attachment!==token||!token?.captionPositionReady)return;
  session.rebinding=false;
  session.heartbeat=0;
  void aiCaptionPoll(session);
}

function aiCaptionStopPlayback(){
  aiCaptionRetirePlayback();
  closeMenu();
}

async function aiCaptionStartPlayback(route="auto",spokenLanguage=null){
  const player=PLAYER,file=aiCaptionPlayerFile(),video=document.getElementById("video");
  if(!player?.mediaAttachment||!video||!aiCaptionLocal(file)||player.pendingMediaChange)return;
  closeMenu();
  const auth=AUTH_GENERATION,attachment=player.mediaAttachment;
  await setSub(-1);
  if(PLAYER!==player||player.mediaAttachment!==attachment||AUTH_GENERATION!==auth)return;
  const session={
    player,video,attachment,auth,file,serial:player.captionIntentSerial,
    audioIndex:translationAttachedAudioOrdinal(player),
    playbackId:translationUuid(),controller:new AbortController(),status:"Starting…",
    duration:Number(file.duration_ms)||0,window:0,timer:null,track:null,operation:null,heartbeat:0,delayMs:0
  };
  session.seek=()=>{
    session.window++;
    session.heartbeat=0;
    session.cues=[];
    if(session.track)session.track.mode="disabled";
    clearTimeout(session.timer);
    void aiCaptionPoll(session);
  };
  AI_CAPTION_PLAYBACK=session;
  video.addEventListener("seeked",session.seek);
  pbSyncSubIcon();
  try{
    const request_id=translationUuid();
    const body={
      request_id,target:"en",route,playback_id:session.playbackId,
      position_ms:aiCaptionPosition(session),audio_index:session.audioIndex,
      spoken_language:spokenLanguage
    };
    // The start may have committed if the viewer switches tracks during the
    // request. Keep its receipt long enough to release that viewer.
    const operation=await api(aiCaptionPath(file),{method:"POST",body});
    if(!aiCaptionOwnsSession(session)){aiCaptionRelease(session,operation.id);return;}
    session.operation=operation;
    void aiCaptionPoll(session);
  }catch(error){
    if(aiCaptionPlaybackCurrent(session)){aiCaptionRetirePlayback();toast(error.message);}
  }
}

function aiCaptionShift(delta){
  const session=AI_CAPTION_PLAYBACK;
  if(!session||!aiCaptionPlaybackCurrent(session))return;
  session.delayMs=Math.max(-15000,Math.min(15000,session.delayMs+delta));
  aiCaptionPaint(session,session.cues||[]);
  toast(`Subtitle timing ${session.delayMs>0?"+":""}${session.delayMs} ms`);
}
function aiCaptionLanguageOptions(){
  const mount=document.getElementById("ai-language-options");
  if(!mount)return;
  const select=document.createElement("select");
  select.setAttribute("aria-label","Spoken language");
  for(const [language,label] of LANG_CHOICES)select.add(new Option(label,language));
  const go=document.createElement("button");
  go.type="button";go.textContent="Use this spoken language";
  go.addEventListener("click",()=>void aiCaptionStartPlayback("audio",select.value));
  mount.replaceChildren(select,go);
  select.focus();
}
function aiCaptionPaint(session,cues){
  session.cues=cues;
  const Cue=window.VTTCue||window.TextTrackCue;
  if(!Cue)return;
  if(!session.track){
    session.track=session.video._aiCaptionTrack||
      (session.video._aiCaptionTrack=session.video.addTextTrack("subtitles","English · AI","en"));
  }
  const track=session.track,wanted=new Map(),offset=(session.player.offset||0)-session.delayMs/1000;
  for(const cue of cues||[]){
    const start=Math.max(0,cue.start_ms/1000-offset),end=cue.end_ms/1000-offset;
    if(Number.isFinite(start)&&Number.isFinite(end)&&end>start&&typeof cue.text==="string"){
      wanted.set(JSON.stringify([start,end,cue.text]),{start,end,text:translationLiteral(cue.text)});
    }
  }
  if(track.mode==="disabled")track.mode="hidden";
  for(const cue of Array.from(track.cues||[])){
    if(wanted.has(cue.id))wanted.delete(cue.id);
    else track.removeCue(cue);
  }
  for(const [id,cue] of wanted){
    const rendered=new Cue(cue.start,cue.end,cue.text);
    rendered.id=id;
    track.addCue(rendered);
  }
  track.mode="showing";
}

async function aiCaptionPoll(session){
  if(session.polling){session.pollAgain=true;return;}
  if(!aiCaptionPlaybackCurrent(session)||!session.operation?.id)return;
  session.polling=true;
  try{await aiCaptionPollWindow(session);}
  finally{
    session.polling=false;
    if(session.pollAgain&&aiCaptionPlaybackCurrent(session)){
      session.pollAgain=false;
      void aiCaptionPoll(session);
    }
  }
}
async function aiCaptionPollWindow(session){
  clearTimeout(session.timer);
  const windowId=++session.window;
  const current=()=>aiCaptionPlaybackCurrent(session)&&windowId===session.window;
  const pos=aiCaptionPosition(session),path=aiCaptionOperationPath(session.operation.id);
  try{
    if(session.operation.state==="complete"&&session.operation.route==="download"){
      if(await aiCaptionUseStored(session,session.operation,current))return;
      if(current()){aiCaptionRetirePlayback();toast("The downloaded subtitle is unavailable.");}
      return;
    }
    if(Date.now()>=session.heartbeat&&aiCaptionActive(session.operation)){
      const operation=await api(path+"/viewer",{method:"PUT",signal:session.controller.signal,body:{
        playback_id:session.playbackId,position_ms:pos,
        playback_rate:Math.max(0.25,Math.min(4,session.video.playbackRate||1))
      }});
      if(!current())return;
      session.operation=operation;
      session.heartbeat=Date.now()+10000;
    }
    const query=new URLSearchParams({from_ms:String(Math.max(0,pos-10000)),to_ms:String(Math.min(14400000,pos+35000))});
    const result=await api(path+`/cues?${query}`,{signal:session.controller.signal});
    if(!current())return;
    if(result.state==="failed"||result.state==="stopped"){
      aiCaptionRetirePlayback();
      toast(result.state==="failed"?"AI subtitles could not finish.":"AI subtitles stopped.");
      return;
    }
    if(result.state==="complete"&&result.track_id){
      if(await aiCaptionUseStored(session,result,current))return;
    }
    session.status=result.ready_to_ms>pos?"Working":"Preparing…";
    aiCaptionCoverage(session,result);
    aiCaptionPaint(session,result.cues);
  }catch(error){
    if(!current())return;
    if(error.status===401||error.status===403){aiCaptionRetirePlayback();return;}
    session.status="Reconnecting…";
  }
  if(current())session.timer=setTimeout(()=>void aiCaptionPoll(session),2500);
}

async function aiCaptionUseStored(session,result,current){
  const decision=await askDecision(session.file.id,"auto",session.player.preplay||{},session.controller.signal,session.player);
  if(!current())return false;
  const track=(decision.subtitles||[]).find(track=>result.track_id
    ? track.downloaded_subtitle_id===result.track_id
    : result.route==="download"&&track.index===result.track_index);
  if(!track)return false;
  session.player.subs=decision.subtitles;
  await setSub(track.index);
  return true;
}

function aiCaptionItemMarkup(file){
  if(!aiCaptionLocal(file))return "";
  const showAdd=!(file.subtitle_streams||[]).some(track=>["en","eng"].includes(track.language)&&!track.forced);
  const stored=(file.subtitle_streams||[]).filter(track=>track.ai&&track.track_id);
  return `<span class="ai-caption-item" data-ai-caption-file="${esc(String(file.id))}">
    ${showAdd?'<button type="button" class="ghost sm" data-ai-caption-open>+ English</button>':""}
    ${stored.map(track=>`<button type="button" class="ghost sm" data-ai-caption-track="${track.index}" aria-label="Manage English AI subtitle">English · AI ⋯</button>`).join("")}
    <span data-ai-caption-actions></span><span data-ai-caption-status role="status"></span></span>`;
}
async function aiCaptionSearch(file,panel){
  panel.textContent="Searching OpenSubtitles…";
  try{
    const data=await api(`/files/${file.id}/subtitles/search?language=en`);
    if(!panel.isConnected)return;
    panel.replaceChildren();
    for(const match of (data.results||[]).slice(0,20)){
      const button=document.createElement("button");
      button.type="button";button.className="ghost sm";button.textContent=match.release||"Subtitle match";
      button.addEventListener("click",()=>void downloadSubtitle(file.id,match.file_id,match.language,button));
      panel.appendChild(button);
    }
    if(!panel.childElementCount)panel.textContent="No English subtitle matches found.";
  }catch(error){if(panel.isConnected)panel.textContent=error.message;}
}

function aiCaptionBindItem(node,file){
  if(!aiCaptionLocal(file))return;
  const mount=node.querySelector("[data-ai-caption-file]");
  if(!mount)return;
  const panel=mount.querySelector("[data-ai-caption-actions]");
  const status=mount.querySelector("[data-ai-caption-status]");
  const owner=WATCH,auth=AUTH_GENERATION;
  const current=()=>mount.isConnected&&owner===WATCH&&watchCurrentFile()===file&&auth===AUTH_GENERATION;
  const itemState=AI_CAPTION_ITEM.get(file)||{};
  AI_CAPTION_ITEM.set(file,itemState);
  const show=()=>{
    const op=itemState.operation;
    status.textContent=aiCaptionActive(op)?`English · AI · ${Math.round((op.progress||0)*100)}%`:
      op?.state==="complete"?"English · AI is ready.":op?.state==="failed"?"AI subtitles could not finish.":"";
  };
  const poll=async()=>{
    if(!current()||!aiCaptionActive(itemState.operation))return;
    try{
      const options=await api(aiCaptionPath(file));
      if(!current())return;
      itemState.operation=options.operation;
      show();
      if(options.operation?.state==="complete"){
        await viewItem(ITEM_FOR_FILE[file.id]);
        return;
      }
    }catch(error){if(current())status.textContent=error.message;}
    if(current()&&aiCaptionActive(itemState.operation))setTimeout(()=>void poll(),4000);
  };
  const start=async(route,quality)=>{
    panel.replaceChildren();
    status.textContent="Preparing English subtitles…";
    try{
      const body={request_id:translationUuid(),route,target:"en"};
      if(quality)body.quality=quality;
      const operation=await api(aiCaptionPath(file),{method:"POST",body});
      if(!current())return;
      itemState.operation=operation;
      AI_CAPTION_OPTIONS.delete(file);
      show();
      if(aiCaptionActive(operation))void poll();
      if(operation.state==="complete")await viewItem(ITEM_FOR_FILE[file.id]);
    }catch(error){if(current())status.textContent=error.message;}
  };
  const button=(text,action)=>{
    const control=document.createElement("button");
    control.type="button";control.className="ghost sm";control.textContent=text;
    control.addEventListener("click",action);
    panel.appendChild(control);
  };
  mount.querySelector("[data-ai-caption-open]")?.addEventListener("click",async()=>{
    panel.replaceChildren();
    try{
      const options=await aiCaptionReadOptions(file);
      if(!current())return;
      if(options?.allowed&&options.route){
        button(options.label||"AI subtitles",()=>void start("auto"));
        for(const choice of options.alternatives||[])button(choice.label,()=>void start(choice.route));
      }
      if(options?.search_online&&file.subtitle_search_enabled)button("Search online",()=>aiCaptionSearch(file,panel));
      if(!panel.childElementCount)status.textContent=options?.blocked?.message||"No subtitle sources are available.";
    }catch(error){if(current())status.textContent=error.message;}
  });
  mount.querySelectorAll("[data-ai-caption-track]").forEach(control=>control.addEventListener("click",()=>{
    panel.replaceChildren();
    const track=(file.subtitle_streams||[]).find(track=>track.index===Number(control.dataset.aiCaptionTrack));
    if(!track)return;
    const timing=`/files/${file.id}/subtitles/${track.index}/timing?track_id=${encodeURIComponent(track.track_id)}`;
    button("Redo at Best quality",()=>void start("audio","best"));
    const shift=async(delta)=>{
      try{
        const before=await api(timing);
        if(!current())return;
        const offset_ms=Math.max(-15000,Math.min(15000,before.offset_ms+delta));
        const saved=await api(timing,{method:"PUT",body:{offset_ms}});
        if(current())status.textContent=`Subtitle timing ${saved.offset_ms>0?"+":""}${saved.offset_ms} ms`;
      }catch(error){if(current())status.textContent=error.message;}
    };
    button("Earlier by 250 ms",()=>void shift(-250));
    button("Later by 250 ms",()=>void shift(250));
    button("Delete",async()=>{
      try{
        await api(`/files/${file.id}/subtitles/${track.index}?track_id=${encodeURIComponent(track.track_id)}`,{method:"DELETE"});
        if(current())await viewItem(ITEM_FOR_FILE[file.id]);
      }catch(error){if(current())status.textContent=error.message;}
    });
  }));
  show();
  if(aiCaptionActive(itemState.operation))void poll();
}
