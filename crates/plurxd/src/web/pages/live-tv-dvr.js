"use strict";
// ---- recording on the Live TV page ----------------------------------------
// Three reads in one wave, after the guide has landed and again after every
// mutation. Never on a timer: a mark changes when somebody presses something
// or when a capture opens, and the owner's tick reaches this page through the
// refetch a press already causes. Polling a fortnight of schedule every few
// seconds to watch a dot that changes twice a day is not a poll, it is a bill.
async function loadLiveTvDvr(generation,route){
  const serial=++LIVE_TV_DVR.serial;
  const read=path=>api(path,{signal:AbortSignal.timeout(20000)}).catch(()=>null);
  const scheduleNow=Math.floor(Date.now()/1000);
  const [status,schedule,reminders]=await Promise.all([
    read("/dvr/status"),
    // The bounded default is the twelve hours either side of now. Terminal
    // rows remain visible for context, and a busy household can page rather
    // than returning an unbounded guide horizon.
    read(`/dvr/schedule?from=${scheduleNow-43200}&to=${scheduleNow+43200}&limit=100`),
    read("/dvr/reminders"),
  ]);
  // A newer wave, a newer page or a different route all mean this answer
  // describes a screen nobody is looking at.
  if(serial!==LIVE_TV_DVR.serial||generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  LIVE_TV_DVR.status=status; LIVE_TV_DVR.schedule=schedule;
  LIVE_TV_DVR.reminders=Array.isArray(reminders)?reminders:[];
  renderLiveTvChannels();
  // Library and Rules are their own reads and only an open segment pays for
  // one — but an open one has to be right after a mutation, not one press
  // behind it.
  const tab=liveTvRecordingsTab();
  if(tab==="library") liveTvLoadRecordings("library","/dvr/recordings?state=done,partial");
  if(tab==="rules") liveTvLoadRecordings("rules","/dvr/rules");
}
// Every mutation ends here: the routes write intent and the owner loop decides
// the rest, so the only honest thing a client can do afterwards is read the
// rows again rather than guess at the new state.
async function liveTvDvrReload(){
  dvrRefreshOverview(true);
  if(location.hash==="#/live-tv") await loadLiveTvDvr(PAGE_RENDER_GENERATION,location.hash);
  else if(location.hash.startsWith("#/recordings")) await viewRecordings(++PAGE_RENDER_GENERATION);
  else if(location.hash==="#/activity"){ACTIVITY_DVR.recentAt=0;await renderActivityBody(PAGE_RENDER_GENERATION);}
}
// Built once per pair of documents rather than once per cell. A fortnight of
// rules against a full lineup is thousands of cells, and rebuilding the index
// inside the cell loop made the render quadratic in the size of the schedule.
function liveTvDvrIndex(){
  const cache=LIVE_TV_DVR.index;
  if(cache&&cache.schedule===LIVE_TV_DVR.schedule&&cache.reminders===LIVE_TV_DVR.reminders) return cache.index;
  const index=PlurxLiveTv.dvrIndex(LIVE_TV_DVR.schedule,LIVE_TV_DVR.reminders);
  LIVE_TV_DVR.index={schedule:LIVE_TV_DVR.schedule,reminders:LIVE_TV_DVR.reminders,index};
  return index;
}
// The marks for one cell or one list row. `inline` is the list's third column,
// where the star and the lock already live; the grid's is absolutely placed
// inside the cell so that adding one moves nothing.
function liveTvMarks(channelId,programme,inline){
  if(!programme||typeof programme.start!=="number") return "";
  const index=liveTvDvrIndex();
  const row=index.row(channelId,programme.start);
  const marks=PlurxLiveTv.dvrMarks(row,index.reminder(channelId,programme.start),liveTvNowSeconds());
  if(!marks.length) return "";
  const glyphs=marks.map(mark=>{
    if(mark.shape==="rec"){
      const active=(DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).find(row=>row.channel_id===channelId&&row.airing_start===programme.start);
      return `<span class="rec">● ${esc(active?dvrRowPresentation(active).label:"Recording planned")}</span>`;
    }
    if(mark.shape==="bell") return `<span class="bell">🔔</span>`;
    if(mark.shape==="dots") return `<span class="dot"></span><span class="dot"></span>`;
    return `<span class="dot${mark.kind==="conflict"?" conflict":""}${mark.shape==="hollow"?" hollow":""}"></span>`;
  }).join("");
  const bar=marks.find(mark=>mark.shape==="rec");
  const active=(DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).find(row=>row.channel_id===channelId&&row.airing_start===programme.start);
  const label=esc(active?dvrRowPresentation(active).label:marks.map(mark=>mark.label).join(" · "));
  return `<span class="lt-mark${inline?" lt-rowmark":""}" data-dvr-mark="${programme.start}" data-dvr-channel="${esc(channelId)}" title="${label}" aria-label="${label}">${glyphs}</span>`
    +(bar&&!inline?`<span class="lt-recbar"><i style="width:${Math.round(bar.progress*100)}%"></i></span>`:"");
}
function liveTvTunerLine(){
  return PlurxLiveTv.dvrTunerLine(LIVE_TV_DVR.status,LIVE_TV_DVR.schedule);
}
function liveTvPopoverActions(programme,channelId){
  const now=liveTvNowSeconds(), index=liveTvDvrIndex();
  const row=index.row(channelId,programme.start);
  const reminder=index.reminder(channelId,programme.start);
  const acts=PlurxLiveTv.dvrAiringActions(programme,row,reminder,now);
  const at=JSON.stringify(channelId), start=Number(programme.start);
  const buttons=[];
  if(acts.watch==="now") buttons.push(`<button type="button" onclick="liveTvPopover(null);liveTvSelect(${esc(at)})">Watch</button>`);
  // A future programme cannot be tuned, so Watch becomes the promise to be
  // shown it when it starts — a reminder with no lead at all.
  else if(acts.watch==="at") buttons.push(`<button type="button" onclick="liveTvRemind(${esc(at)},${start},0)">Watch at ${esc(liveTvClock(programme.start))}</button>`);
  if(acts.record==="record") buttons.push(`<button type="button" onclick="liveTvRecord(${esc(at)},${start})">Record</button>`);
  else if(acts.record==="skip") buttons.push(`<button type="button" onclick="liveTvSkip(${esc(JSON.stringify(row.id))})">Skip</button>`);
  else if(acts.record==="stop") buttons.push(`<button type="button" onclick="liveTvStopRecording(${esc(JSON.stringify(row.id))},${esc(JSON.stringify(programme.title))})">Stop</button>`);
  if(acts.series) buttons.push(`<button type="button" onclick="liveTvRecordSeries(${esc(at)},${start})">Record series</button>`);
  if(acts.remind==="set") buttons.push(`<button type="button" onclick="liveTvRemind(${esc(at)},${start},null)">Remind me</button>`);
  else if(acts.remind==="clear") buttons.push(`<button type="button" onclick="liveTvForgetReminder(${esc(JSON.stringify(reminder.id))})">Forget reminder</button>`);
  const tuners=liveTvTunerLine();
  if(!buttons.length) return `<p class="hint">This programme has finished.</p>`;
  return `<div class="lt-acts">${buttons.join("")}</div>${tuners?`<p class="lt-tuners">${esc(tuners)}</p>`:""}`;
}
// One place for "ask, say what happened, read the rows again". Every DVR verb
// on this page is that shape, and a typed refusal — `dvr_disabled`,
// `airing_past`, `rule_limit` — carries its own sentence from the route.
async function liveTvDvrAct(request,done){
  try{
    const result=await request();
    liveTvPopover(null);
    toast(done);
    await liveTvDvrReload();
    return result;
  }catch(e){ toast(e.message); return null; }
}
function liveTvRecord(channelId,airingStart){
  return liveTvDvrAct(()=>api("/dvr/recordings",{method:"POST",body:{channel_id:channelId,airing_start:airingStart}}),
    "Scheduled to record");
}
function liveTvRecordSeries(channelId,airingStart){
  return liveTvDvrAct(()=>api("/dvr/rules",{method:"POST",body:{from_airing:{channel_id:channelId,airing_start:airingStart}}}),
    "Recording every episode");
}
function liveTvRemind(channelId,airingStart,lead){
  const body={channel_id:channelId,airing_start:airingStart};
  // An explicit zero lead is "show me this when it starts"; an absent one takes
  // the server's configured lead, and `lead_s:null` would not be the same
  // request at all.
  if(lead!==null&&lead!==undefined) body.lead_s=lead;
  return liveTvDvrAct(()=>api("/dvr/reminders",{method:"POST",body}),lead===0?"You will be shown it when it starts":"Reminder set");
}
function liveTvForgetReminder(id){
  return liveTvDvrAct(()=>api(`/dvr/reminders/${encodeURIComponent(id)}`,{method:"DELETE",raw:true}),"Reminder removed");
}
// A planned airing. `DELETE` on one is a cancel, and it is durable: expansion
// skips a cancelled row for good, which is why Restore has to be explicit.
function liveTvSkip(id){
  return liveTvDvrAct(()=>api(`/dvr/recordings/${encodeURIComponent(id)}`,{method:"DELETE",raw:true}),"Skipped");
}
function liveTvRestore(id){
  return liveTvDvrAct(()=>api(`/dvr/recordings/${encodeURIComponent(id)}/restore`,{method:"POST",body:{}}),"Back on the schedule");
}
// A running capture stops when the owner's next tick closes the file, so the
// route answers 202 and the row is still `recording` when this returns.
function liveTvStopRecording(id,title){
  const row=(DVR_SHARED.overview&&DVR_SHARED.overview.active||[]).find(row=>row.recording_id===id)||DVR_PAGE.selected;
  const window=row&&((row.recording_id||row.id)===id)?` (${liveTvClock(row.capture_start)}–${liveTvClock(row.capture_end)})`:"";
  if(!confirm(`Stop recording "${title||"this programme"}"${window}? The captured part will be kept and marked as stopped early. Watching continues.`)) return;
  return liveTvDvrAct(()=>api(`/dvr/recordings/${encodeURIComponent(id)}`,{method:"DELETE",raw:true}),
    "Stop requested — waiting for the recorder to close the capture");
}
// A finished recording has a file, and "take this off my schedule" must not be
// the same request as "delete the recording" by accident: the route refuses
// the first form with `delete_file_required` and this asks before repeating it.
async function liveTvDeleteRecording(id,title){
  try{
    await api(`/dvr/recordings/${encodeURIComponent(id)}`,{method:"DELETE",raw:true});
    toast("Deleted"); await liveTvDvrReload(); return;
  }catch(e){
    if(e.code!=="delete_file_required"){ toast(e.message); return; }
  }
  if(!confirm(`Delete "${title}" and its recorded file?`)) return;
  try{
    await api(`/dvr/recordings/${encodeURIComponent(id)}?delete_file=1`,{method:"DELETE",raw:true});
    toast("Deleted"); await liveTvDvrReload();
  }catch(e){ toast(e.message); }
}
function liveTvRecordingsTab(){
  return liveTvPref("plurx_live_tv_recordings","",["","library","scheduled","rules"]);
}
function liveTvSetRecordingsTab(tab){
  // These shipped Live TV segments are compatibility aliases. Their single
  // canonical destination now survives playback docking and can carry detail,
  // history, pagination and attention without rebuilding the Live TV host.
  liveTvSetPref("plurx_live_tv_recordings","");
  location.hash=`#/recordings/${tab==="library"?"saved":tab==="scheduled"?"upcoming":tab}`;
}
async function liveTvLoadRecordings(key,path,append=false){
  const generation=PAGE_RENDER_GENERATION, route=location.hash;
  if(key==="library") LIVE_TV_DVR.libraryLoading=true;
  let answer=null, failure=null;
  try{ answer=await api(path,{signal:AbortSignal.timeout(20000)}); }catch(e){ failure=e; }
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  if(key==="rules"){
    // Rules intentionally remain an array. They do not inherit the recordings
    // envelope merely because both lists happen to sit in one panel.
    LIVE_TV_DVR.rules=Array.isArray(answer)?answer:[];
  }else{
    try{
      if(failure) throw failure;
      const page=dvrRecordingsPage(answer);
      const prior=append&&Array.isArray(LIVE_TV_DVR.library)?LIVE_TV_DVR.library:[];
      const seen=new Set(prior.map(row=>row.id));
      LIVE_TV_DVR.library=prior.concat(page.rows.filter(row=>!seen.has(row.id)));
      LIVE_TV_DVR.libraryNext=page.next; LIVE_TV_DVR.libraryError=null;
    }catch(e){
      LIVE_TV_DVR.libraryError=e&&e.message?e.message:"The recordings could not be loaded.";
    }
    LIVE_TV_DVR.libraryLoading=false;
  }
  renderLiveTvChannels();
}
function liveTvLoadMoreRecordings(){
  if(!LIVE_TV_DVR.libraryNext||LIVE_TV_DVR.libraryLoading) return;
  return liveTvLoadRecordings("library",`/dvr/recordings?state=done,partial&after=${encodeURIComponent(LIVE_TV_DVR.libraryNext)}`,true);
}
const DVR_STATE_LABEL={scheduled:"Scheduled",conflict:"No tuner",withdrawn:"Withdrawn",stale:"Programme moved",
  recording:"Recording",done:"Recorded",partial:"Recorded with a gap",failed:"Failed",missed:"Missed",
  cancelled:"Skipped",deleted:"Deleted"};
function liveTvRecordingLine(row){
  const when=`${esc(liveTvClock(row.airing_start))}–${esc(liveTvClock(row.airing_end))}`;
  const episode=[row.episode,row.episode_title].filter(Boolean).join(" · ");
  return `<span class="meta">${esc(row.guide_number)} · ${esc(row.channel_name)} · ${when}${episode?" · "+esc(episode):""}</span>`;
}
function liveTvRecordingsMarkup(tab,selected){
  const body=tab==="library"?liveTvLibraryRows():tab==="rules"?liveTvRuleRows():liveTvScheduleRows();
  const tuners=liveTvTunerLine();
  return `<div class="lt-split">
    ${liveTvStageMarkup(selected,false)}
    <div>
      ${tuners?`<p class="muted" style="margin:0 0 8px;font-size:12.5px">${esc(tuners)}</p>`:""}
      <div class="lt-recs" aria-label="Recordings">${body}</div>
    </div>
  </div>`;
}
function liveTvEmptyRecs(text){
  return `<div style="padding:18px" class="muted">${esc(text)}</div>`;
}
function liveTvLibraryRows(){
  const rows=LIVE_TV_DVR.library;
  if(rows===null) return liveTvEmptyRecs("Loading recordings…");
  const notice=LIVE_TV_DVR.libraryError
    ?`<div class="clrefusal" role="status"><b>${rows.length?"Showing the last successful recording list":"Recordings are unavailable"}</b> ${esc(LIVE_TV_DVR.libraryError)}</div>`
    :"";
  if(!rows.length) return notice||liveTvEmptyRecs("Nothing has been recorded yet.");
  const list=rows.map(row=>{
    // A finished recording is an ordinary library item once the scan has
    // linked it, and it belongs on the ordinary item page — this list is a way
    // in, not a second player.
    const title=row.item_id?`<a href="#/item/${row.item_id}">${esc(row.title)}</a>`:esc(row.title);
    const pending=row.item_id?"":`<span class="why">Waiting for the library scan to pick up the file</span>`;
    return `<div class="lt-rec"><span><b>${title}</b>${liveTvRecordingLine(row)}${pending}
        ${row.state==="partial"?`<span class="why">${esc(row.state_reason||"Captured with a gap")}</span>`:""}</span>
      <span class="acts"><button type="button" onclick="liveTvDeleteRecording(${esc(JSON.stringify(row.id))},${esc(JSON.stringify(row.title))})">Delete</button></span></div>`;
  }).join("");
  const more=LIVE_TV_DVR.libraryNext
    ?`<div class="row" style="justify-content:flex-end"><button type="button" onclick="liveTvLoadMoreRecordings()"${LIVE_TV_DVR.libraryLoading?" disabled":""}>${LIVE_TV_DVR.libraryLoading?"Loading…":"Load more"}</button></div>`:"";
  return notice+list+more;
}
function liveTvScheduleRows(){
  const schedule=LIVE_TV_DVR.schedule;
  if(!schedule) return liveTvEmptyRecs("Loading the schedule…");
  const rows=(schedule.rows||[]).slice().sort((a,b)=>a.capture_start-b.capture_start);
  if(!rows.length) return liveTvEmptyRecs("Nothing is scheduled. Record a programme from the guide.");
  return rows.map(row=>{
    const acts=[];
    if(row.state==="recording") acts.push(`<button type="button" onclick="liveTvStopRecording(${esc(JSON.stringify(row.id))},${esc(JSON.stringify(row.title))})">Stop</button>`);
    else if(row.state==="cancelled") acts.push(`<button type="button" onclick="liveTvRestore(${esc(JSON.stringify(row.id))})">Restore</button>`);
    else acts.push(`<button type="button" onclick="liveTvSkip(${esc(JSON.stringify(row.id))})">Skip</button>`);
    const reason=row.state==="conflict"||row.state==="stale"||row.state==="withdrawn";
    return `<div class="lt-rec"><span><b>${esc(row.title)}</b> <span class="pill">${esc(DVR_STATE_LABEL[row.state]||row.state)}</span>
        ${liveTvRecordingLine(row)}
        ${reason?`<span class="why">${esc(row.state_reason||"No tuner is free for this")}</span>`:""}</span>
      <span class="acts">${acts.join("")}</span></div>`;
  }).join("");
}
function liveTvRuleRows(){
  const rows=LIVE_TV_DVR.rules;
  if(rows===null) return liveTvEmptyRecs("Loading rules…");
  if(!rows.length) return liveTvEmptyRecs("No series rules yet. Use “Record series” on a programme in the guide.");
  return rows.map(rule=>{
    const scope=rule.channel_id?liveTvChannelLabel(rule.channel_id):"any channel";
    const keep=rule.keep_mode==="last_n"?`keep the last ${rule.keep_value}`
      :rule.keep_mode==="days"?`keep ${rule.keep_value} days`
      :rule.keep_mode==="until_watched"?"keep until watched":"keep everything";
    return `<div class="lt-rec"><span><b>${esc(rule.name)}</b>${rule.enabled?"":' <span class="pill">off</span>'}
        <span class="meta">${esc(scope)} · ${rule.new_only?"new episodes only":"every showing"} · ${esc(keep)} · priority ${esc(String(rule.priority))}</span></span>
      <span class="acts"><button type="button" onclick="liveTvDeleteRule(${esc(JSON.stringify(rule.id))})">Delete</button></span></div>`;
  }).join("");
}
function liveTvChannelLabel(channelId){
  const channel=liveTvChannelById(channelId);
  return channel?`${channel.guide_number} · ${channel.guide_name}`:channelId;
}
// Deleting a rule withdraws its pending airings on the next tick — it does not
// cancel them, because `cancelled` is the viewer's word and a rule does not
// get to say it on their behalf.
function liveTvDeleteRule(id){
  return liveTvDvrAct(()=>api(`/dvr/rules/${encodeURIComponent(id)}`,{method:"DELETE",raw:true}),"Rule deleted");
}

