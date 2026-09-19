"use strict";
// ---- global activity indicator --------------------------------------------
// Always-visible pill: whatever the server is doing right now (scans,
// metadata, streams — later: moves, backups). Empty response = hidden.
let ACT_TIMER=null, ACT_POLLING=false;
function paintActivity(acts){
  const el=document.getElementById("activity");
  if(!el) return;
  // the cursor is the status light: blinking = working, solid = idle (noirr)
  document.documentElement.classList.toggle("working", acts.length>0);
  if(!acts.length){ el.style.display="none"; el.innerHTML=""; return; }
  const a=acts[0];
  const bits=[esc(a.label)];
  if(a.detail) bits.push(esc(a.detail));
  if(a.percent!=null) bits.push(a.percent+"%");
  const extra=acts.length>1?` &nbsp;+${acts.length-1}`:"";
  el.innerHTML=`<span class="spin"></span><span class="activitytext">${bits.join(" · ")}${extra}</span>`;
  el.style.display="flex";
}
async function pollActivity(){
  // The Activity page has a richer poll of its own. Do not make both pollers
  // pay separate auth and leader-read costs for the same screen, and never
  // stack requests when a slow cluster round outlives the timer interval.
  if(!TOKEN||!ME||location.hash==="#/activity"||document.visibilityState==="hidden"||ACT_POLLING) return;
  const el=document.getElementById("activity");
  if(!el) return;
  const generation=PAGE_RENDER_GENERATION;
  const route=location.hash;
  ACT_POLLING=true;
  try{
    const acts=await api("/activity");
    if(generation!==PAGE_RENDER_GENERATION||route!==location.hash||location.hash==="#/activity") return;
    paintActivity(acts);
  }catch(e){}
  finally{ ACT_POLLING=false; }
}

