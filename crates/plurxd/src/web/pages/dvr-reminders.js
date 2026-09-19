"use strict";
// ---- the due-reminder overlay ---------------------------------------------
// Shown on any page while `?due=1` is not empty, and it is buttons the whole
// way down: no key handler here, on purpose. The Live TV adapter is the only
// place on this page that may read a key, and `scripts/player-input-fence`
// keeps it that way.
let DVR_REMINDER_TIMER=null, DVR_REMINDER_POLLING=false, DVR_REMINDER_RETIRE=null;
let DVR_DUE=[];
async function pollDvrReminders(){
  if(!TOKEN||!ME||document.visibilityState==="hidden"||DVR_REMINDER_POLLING) return;
  DVR_REMINDER_POLLING=true;
  try{
    const due=await api("/dvr/reminders?due=1");
    DVR_DUE=Array.isArray(due)?due:[];
    paintDvrReminder();
  }catch(e){}
  finally{ DVR_REMINDER_POLLING=false; }
}
function paintDvrReminder(){
  let box=document.getElementById("dvr-reminder");
  const now=liveTvNowSeconds();
  // The soonest one wins: two reminders competing for the corner is a second
  // problem, and the one that is about to start is the one worth the space.
  const due=DVR_DUE.slice().sort((a,b)=>a.airing_start-b.airing_start)[0]||null;
  clearTimeout(DVR_REMINDER_RETIRE); DVR_REMINDER_RETIRE=null;
  if(!due){ if(box) box.hidden=true; return; }
  const count=PlurxLiveTv.dvrReminderCountdown(due,now);
  // At the start there is nothing left to remind anybody of. Retiring it here
  // rather than waiting for the next poll is what "auto-dismissed at the
  // start" means, and it costs one timeout rather than a second interval.
  if(count.expired){
    DVR_DUE=DVR_DUE.filter(row=>row!==due);
    if(box) box.hidden=true;
    if(DVR_DUE.length) paintDvrReminder();
    return;
  }
  if(!box){
    box=document.createElement("div"); box.id="dvr-reminder"; box.className="dvr-remind";
    box.setAttribute("role","status"); box.setAttribute("aria-live","polite");
    document.body.appendChild(box);
  }
  const id=JSON.stringify(due.id);
  const watch=`<button type="button" onclick="dvrReminderWatch(${esc(id)},${esc(JSON.stringify(due.channel_id))})">Watch</button>`;
  // A reminder the schedule already covers must not offer to schedule it
  // again; the route answers with the row it already has, which reads as a
  // press that did nothing.
  const record=due.covered_by_recording
    ?`<button type="button" disabled title="A recording already covers this">Recording</button>`
    :`<button type="button" onclick="dvrReminderRecord(${esc(id)},${esc(JSON.stringify(due.channel_id))},${Number(due.airing_start)})">Record</button>`;
  box.innerHTML=`<b>${esc(due.title)}</b>
    <span class="meta">${esc(due.guide_number)} · ${esc(liveTvClock(due.airing_start))} · starts ${esc(count.label)}</span>
    <div class="acts">${watch}${record}<button type="button" onclick="dvrReminderDismiss(${esc(id)})">Dismiss</button></div>
    <div class="bar"><i style="width:${Math.round(count.fraction*100)}%;animation-duration:${Math.max(1,Math.round(count.seconds))}s"></i></div>`;
  box.hidden=false;
  DVR_REMINDER_RETIRE=setTimeout(paintDvrReminder,Math.max(1,count.seconds)*1000);
}
// Dismiss acks: a second device must not show the same reminder again.
async function dvrReminderAck(id){
  DVR_DUE=DVR_DUE.filter(row=>row.id!==id);
  paintDvrReminder();
  try{ await api(`/dvr/reminders/${encodeURIComponent(id)}/ack`,{method:"POST",body:{}}); }catch(e){}
}
function dvrReminderDismiss(id){ return dvrReminderAck(id); }
async function dvrReminderWatch(id,channelId){
  await dvrReminderAck(id);
  if(location.hash!=="#/live-tv"){
    // The lineup arrives with the route. Asking for a channel this document
    // has not heard of yet is a no-op, so the tune waits for the list.
    LIVE_TV.watchOnArrival=channelId;
    location.hash="#/live-tv";
    return;
  }
  liveTvSelect(channelId);
}
async function dvrReminderRecord(id,channelId,airingStart){
  try{
    await api("/dvr/recordings",{method:"POST",body:{channel_id:channelId,airing_start:airingStart}});
    toast("Scheduled to record");
    await liveTvDvrReload();
  }catch(e){ toast(e.message); return; }
  await dvrReminderAck(id);
}
