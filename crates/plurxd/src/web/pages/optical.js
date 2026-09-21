"use strict";
// ---- optical discs --------------------------------------------------------
// Availability is process-local UI state. It never enables the server feature
// and never substitutes for the owner checks repeated by decision/session.
let OPTICAL_NAV=false;
function opticalRememberAvailability(drives){
  if(Array.isArray(drives)&&drives.length&&!OPTICAL_NAV){
    OPTICAL_NAV=true;
    opticalRevealNav();
  }
}
function opticalNavEnabled(){ return OPTICAL_NAV; }
function opticalRevealNav(){
  const current=location.hash.startsWith("#/discs");
  const add=(nav,before,html)=>{
    if(!nav||nav.querySelector('[href="#/discs"]')) return;
    const target=before&&nav.querySelector(before);
    if(target) target.insertAdjacentHTML("beforebegin",html); else nav.insertAdjacentHTML("beforeend",html);
  };
  add(document.querySelector("header.top nav"),'[href="#/activity"]',
    `<a href="#/discs" class="${current?"active":""}"${current?' aria-current="page"':""}>Discs</a>`);
  for(const nav of document.querySelectorAll(".th-nav,.th-pills"))
    add(nav,'[data-tab="activity"]',`<a href="#/discs" data-tab="discs" class="${current?"on":""}"${current?' aria-current="page"':""}>Discs</a>`);
  add(document.querySelector('.px-nav[aria-label="Main"]'),null,
    `<a href="#/discs" class="${current?"on":""}"${current?' aria-current="page"':""}>${catalogIcon("film")}<span class="px-lbl">Discs</span></a>`);
  add(document.querySelector(".px-tabs"),'[data-tab="libs"]',
    `<a href="#/discs" data-tab="discs" class="px-tab${current?" on":""}"${current?' aria-current="page"':""}>${catalogIcon("film")}<span>Discs</span></a>`);
}
async function loadOpticalDrives(){
  const drives=await api("/optical/drives");
  opticalRememberAvailability(drives);
  return Array.isArray(drives)?drives:[];
}
function opticalFormatLabel(value){ return value==="bluray"?"Blu-ray":value==="dvd"?"DVD":"Optical disc"; }
function opticalStateName(drive){ return String(drive?.state?.state||"unavailable").replaceAll("_"," "); }
function opticalDiscName(drive){
  return drive?.disc?.display_title||drive?.disc?.volume_label||"Inserted disc";
}
function opticalDriveHref(drive){ return `#/discs/${encodeURIComponent(drive.id)}`; }
function opticalHomeHtml(page){
  const drives=(page&&page.optical)||[];
  const shown=drives.filter(drive=>drive.disc||drive.state?.state==="inspecting");
  let body="";
  if(shown.length){
    body=`<h2 class="section">Inserted disc${shown.length===1?"":"s"} <a class="muted optical-all" href="#/discs">· all drives</a></h2>`+
      `<div class="optical-shelf">${shown.map(drive=>{
        if(!drive.disc) return `<a class="optical-card reading" href="${opticalDriveHref(drive)}"><span class="optical-mark">◉</span><span><strong>${esc(drive.name)}</strong><small>Reading disc…</small></span></a>`;
        return `<a class="optical-card" href="${opticalDriveHref(drive)}">
          <span class="optical-mark">${drive.disc.format==="bluray"?"BD":"DVD"}</span>
          <span class="optical-copy"><small>${esc(opticalFormatLabel(drive.disc.format))} · ${esc(drive.name)}</small>
          <strong>${esc(opticalDiscName(drive))}</strong><span>Explore disc</span></span></a>`;
      }).join("")}</div>`;
  }
  return `<div data-home-region="optical" data-home-slot="optical">${body}</div>`;
}
function opticalRequirementSummary(drive){
  const missing=(drive.requirements||[]).filter(row=>row.status!=="met");
  return missing.length?missing.map(row=>row.detail).join(" · "):"Reader requirements met";
}
function opticalDriveRow(drive){
  const state=opticalStateName(drive);
  const title=drive.disc?opticalDiscName(drive):drive.name;
  const sub=drive.disc
    ? `${opticalFormatLabel(drive.disc.format)} · ${drive.name}`
    : `${state} · ${opticalRequirementSummary(drive)}`;
  return `<a class="optical-drive-row" href="${opticalDriveHref(drive)}">
    <span class="optical-mark">${drive.disc?(drive.disc.format==="bluray"?"BD":"DVD"):"◉"}</span>
    <span><strong>${esc(title)}</strong><small>${esc(sub)}</small></span><span class="optical-chevron">›</span></a>`;
}
async function viewDiscs(generation=++PAGE_RENDER_GENERATION){
  const route=location.hash;
  let navPainted=opticalNavEnabled();
  layoutChrome("discs",pageHead([{href:"#/",label:"Home"},{href:null,label:"Discs"}],"Discs")+
    `<div class="empty">Checking configured optical drives…</div>`);
  setPagePhase(route,generation,"shell");
  const draw=async()=>{
    const drives=await loadOpticalDrives();
    if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
    const head=pageHead([{href:"#/",label:"Home"},{href:null,label:"Discs"}],"Discs");
    const content=head+(drives.length
      ? `<div class="optical-drive-list">${drives.map(opticalDriveRow).join("")}</div>`
      : `<div class="empty">No optical drives are configured for this account.</div>`);
    if(!navPainted&&opticalNavEnabled()){
      layoutChrome("discs",content);navPainted=true;
    }else{
      const main=document.getElementById("main");if(main) main.innerHTML=content;
    }
    setPagePhase(route,generation,"content");
  };
  await draw();
  if(generation!==PAGE_RENDER_GENERATION) return;
  setPagePhase(route,generation,"settled");
  setPageTimer(()=>draw().catch(()=>{}),5000,generation);
}
function opticalDuration(ms){ return ms==null?"Duration unknown":fmtDur(Math.round(ms/1000)); }
function opticalTrackLabel(track){
  const language=track.language?langName(track.language):null;
  return [language,codecLabel(track.codec),track.channels?fmtChannels(track.channels):null,track.title]
    .filter(Boolean).join(" · ")||"Unknown track";
}
function opticalChapterRows(chapters,drive,title,detail){
  if(!Array.isArray(chapters)||!chapters.length) return `<div class="hint">No chapter markers were reported.</div>`;
  return `<div class="optical-chapters">${chapters.map((chapter,index)=>{
    const at=Number(chapter.start_ms??chapter.start_time_ms??chapter.start)||0;
    return `<button class="ghost" onclick="opticalPlayAt(${esc(JSON.stringify(drive.id))},${esc(JSON.stringify(title.id))},${Math.max(0,at)})">Chapter ${index+1}<small>${clockFromSec(at/1000)}</small></button>`;
  }).join("")}</div>`;
}
function opticalTitleCard(drive,title){
  const href=`${opticalDriveHref(drive)}/${encodeURIComponent(title.id)}`;
  return `<a class="optical-title-row" href="${href}"><span><strong>${esc(title.id)}</strong>
    <small>${esc(opticalDuration(title.duration_ms))}${title.angles>1?` · ${title.angles} angles`:""}${title.match_kind?` · ${esc(title.match_kind)}`:""}</small></span><span class="optical-chevron">›</span></a>`;
}
async function opticalLoadDrive(driveId){ return api(`/optical/drives/${encodeURIComponent(driveId)}/disc`); }
async function viewDisc(driveId,titleId=null,generation=++PAGE_RENDER_GENERATION){
  const route=location.hash;
  layoutChrome("discs",`<div class="empty">Reading disc information…</div>`);
  setPagePhase(route,generation,"shell");
  const data=await opticalLoadDrive(driveId);
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  opticalRememberAvailability([data.drive]);
  if(titleId){ await viewOpticalTitle(data,titleId,generation,route); return; }
  const drive=data.drive;
  const disc=drive.disc;
  const trail=[{href:"#/discs",label:"Discs"},{href:null,label:disc?opticalDiscName(drive):drive.name}];
  let body=pageHead(trail,disc?opticalDiscName(drive):drive.name);
  if(!disc){
    const state=drive.state?.state;
    body+=`<div class="empty">${state==="inspecting"?"Reading disc…":state==="failed"?esc(drive.state.reason||"The disc could not be read."):state==="busy"?"Drive in use.":"No disc is inserted."}</div>`;
  }else{
    body+=`<section class="optical-hero"><div class="optical-mark large">${disc.format==="bluray"?"BD":"DVD"}</div><div>
      <div class="vbadges"><span>${esc(opticalFormatLabel(disc.format))}</span><span>${esc(drive.name)}</span><span>${esc(opticalStateName(drive))}</span></div>
      <h1>${esc(opticalDiscName(drive))}</h1><p class="muted">Choose a title. Generic title identifiers are retained when no verified metadata match exists.</p>
      ${ME.is_admin?`<button class="ghost sm" onclick="opticalEject(${esc(JSON.stringify(drive.id))})">Eject</button>`:""}</div></section>
      <h2 class="section">Titles &amp; extras</h2><div class="optical-title-list">${data.titles.length?data.titles.map(title=>opticalTitleCard(drive,title)).join(""):`<div class="empty">No playable titles were reported.</div>`}</div>`;
  }
  layoutChrome("discs",body);
  setPagePhase(route,generation,"content");setPagePhase(route,generation,"settled");
  setPageTimer(()=>opticalRefreshDisc(driveId,generation,route),5000,generation);
}
async function viewOpticalTitle(data,titleId,generation,route){
  const drive=data.drive, disc=drive.disc;
  if(!disc) throw new Error("The disc is no longer inserted.");
  const detail=await api(`/optical/discs/${encodeURIComponent(disc.id)}/titles/${encodeURIComponent(titleId)}`);
  if(generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  const title=data.titles.find(row=>row.id===titleId)||{id:titleId,duration_ms:detail.title.duration_ms,angles:detail.title.angles};
  const resume=Math.max(0,Number(detail.progress?.position_ms)||0);
  const name=title.match_kind?`${title.match_kind} · ${title.id}`:title.id;
  const facts=detail.title.facts||{};
  const trail=[{href:"#/discs",label:"Discs"},{href:opticalDriveHref(drive),label:opticalDiscName(drive)},{href:null,label:name}];
  const sourceArgs=`${esc(JSON.stringify(drive.id))},${esc(JSON.stringify(title.id))}`;
  const actions=`<div class="optical-actions">${resume>3000?`<button onclick="opticalPlayAt(${sourceArgs},${resume})">Resume at ${clockFromSec(resume/1000)}</button>`:""}<button class="${resume>3000?"ghost":""}" onclick="opticalPlayAt(${sourceArgs},0)">Play from start</button></div>`;
  const audio=(facts.audio_streams||[]).map((track,index)=>`<li>${esc(opticalTrackLabel(Object.assign({index},track)))}</li>`).join("");
  const subs=(facts.subtitle_streams||[]).map((track,index)=>`<li>${esc(opticalTrackLabel(Object.assign({index},track)))}</li>`).join("");
  const body=pageHead(trail,name)+`<section class="optical-detail">
    <div class="optical-mark large">${disc.format==="bluray"?"BD":"DVD"}</div><div><div class="vbadges"><span>${esc(opticalFormatLabel(disc.format))}</span><span>${esc(opticalDuration(title.duration_ms))}</span>${facts.height?`<span>${facts.height}p source</span>`:""}</div>
    <h1>${esc(name)}</h1>${actions}</div></section>
    <div class="optical-columns"><section class="card"><h2 class="section">Chapters</h2>${opticalChapterRows(detail.chapters,drive,title,detail)}</section>
    <section class="card"><h2 class="section">Audio &amp; subtitles</h2>${audio?`<h3>Audio</h3><ul>${audio}</ul>`:`<div class="hint">No audio tracks were reported.</div>`}${subs?`<h3>Subtitles</h3><ul>${subs}</ul>`:`<div class="hint">No subtitle tracks were reported.</div>`}</section></div>`;
  layoutChrome("discs",body);
  setPagePhase(route,generation,"content");setPagePhase(route,generation,"settled");
  setPageTimer(()=>opticalRefreshDisc(drive.id,generation,route),5000,generation);
}
async function opticalPlayAt(driveId,titleId,startMs){
  const data=await opticalLoadDrive(driveId);
  const drive=data.drive, disc=drive.disc;
  if(!disc) return toast("The disc is no longer inserted");
  const title=data.titles.find(row=>row.id===titleId);
  if(!title) return toast("That title is no longer available");
  const prefix=`${drive.owner_node_id}:`;
  const localDrive=drive.id.startsWith(prefix)?drive.id.slice(prefix.length):drive.id;
  const source={source:"optical",owner_node_id:drive.owner_node_id,drive_id:localDrive,
    route_drive_id:drive.id,media_generation:disc.media_generation,disc_id:disc.id,
    title_id:title.id,angle:1};
  const meta={title:opticalDiscName(drive),overview:`${opticalFormatLabel(disc.format)} · ${drive.name}`};
  await play(source,opticalDiscName(drive),Math.max(0,Number(startMs)||0),title.duration_ms||0,meta);
}
async function opticalEject(driveId){
  const data=await opticalLoadDrive(driveId), drive=data.drive, disc=drive.disc;
  if(!disc) return toast("No disc is inserted");
  if(!confirm(`Eject ${opticalDiscName(drive)} from ${drive.name}?`)) return;
  await api(`/optical/drives/${encodeURIComponent(drive.id)}/eject`,{method:"POST",body:{
    expected_disc_id:disc.id,media_generation:disc.media_generation,stop_active:false,session_id:null
  }});
  toast("Disc ejected");
  if(location.hash.startsWith("#/discs/")) location.hash="#/discs";
}
async function opticalRefreshDisc(driveId,generation,route){
  if(document.hidden||generation!==PAGE_RENDER_GENERATION||location.hash!==route) return;
  const modal=document.getElementById("modal");
  if(modal&&modal.classList.contains("open")) return;
  try{
    const data=await opticalLoadDrive(driveId);
    const state=data.drive.state?.state;
    if(state==="empty"||state==="failed") await render();
  }catch(e){}
}
