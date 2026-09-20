"use strict";
function watchRenderTitle(){
  if(!WATCH?.page)return;
  const page=WATCH.page,m=page.meta;
  const facts=[m.show,m.season!=null?`S${String(m.season).padStart(2,"0")}E${String(m.episode).padStart(2,"0")}`:null,m.year,m.runtime_ms?fmtDur(m.runtime_ms):null].filter(Boolean);
  const title=document.getElementById("watch-title");
  if(title)title.innerHTML=`<span class="muted">${WATCH.accepted?"NOW PLAYING":"STARTING"}</span><h2>${esc(page.item.title)}</h2><p class="muted">${facts.map(esc).join(" · ")}</p><div class="vbadges">${WATCH.accepted?playerFactBadges():""}</div><p>${esc(m.overview||"")}</p><button class="ghost" onclick="watchShowFacts()">Title details</button>`;
  const caption=document.getElementById("watch-caption");
  if(caption)caption.innerHTML=`<button class="ghost watch-facts" onclick="watchShowFacts()"><strong>${esc(page.item.title)}</strong><span>${facts.map(esc).join(" · ")} · <span id="watch-remaining">${esc(fmtDur(Math.max(0,(page.runtime||0)-(WATCH.accepted?pbPosSec()*1000:0))))} left</span> · Title details</span></button>${page.item.kind==="episode"?'<button class="ghost watch-play-next" onclick="playNextEpisode()">Play next</button>':""}`;
}
function watchShowFacts(){
  const d=document.getElementById("watch-details");if(!d||!WATCH?.page)return;
  d.innerHTML=`<button class="ghost" onclick="this.closest('dialog').close()">Close</button><h2>${esc(WATCH.page.item.title)}</h2><div class="vbadges">${playerFactBadges()}</div>${specBlock(WATCH.page.files.find(f=>String(f.id)===String(PLAYER?.fileId))||WATCH.page.playable)}`;d.showModal();
}
function watchShowTitleInfo(){
  const d=document.getElementById("watch-details"),title=document.getElementById("watch-title");
  if(!d||!title)return;
  d.innerHTML=`<button class="ghost" onclick="this.closest('dialog').close()">Close title info</button>${title.innerHTML}`;d.showModal();
}
function watchToggleEpisodeRows(){
  if(!WATCH)return;
  WATCH.rows=!WATCH.rows;
  document.getElementById("watch-episodes").classList.toggle("watch-rows",WATCH.rows);
  const button=document.getElementById("watch-row-toggle");button.textContent=WATCH.rows?"Cards":"Rows";button.setAttribute("aria-pressed",String(WATCH.rows));
}
async function watchPlayEpisode(id){
  const w=WATCH;if(!w||w.accepted===String(id))return false;
  const load=++w.load;
  const attempt=PLAY_OPEN_GATE.begin("episode");
  const current=()=>WATCH===w&&w.load===load&&PLAY_OPEN_GATE.current(attempt);
  try{
    const page=await loadItem(id,current);
    if(!current()||!page)return false;
    const pf=page.playable;
    if(!pf){toast("This episode has no available file.");return false;}
    if(w.accepted===exactWireId(page.item))return false;
    if(PLAYER?.fileId)reportProgress(PLAYER.fileId,false,PLAYER);
    const meta=playbackMetaFor(page,pf);meta._watchPage=page;
    await play(pf.id,page.item.title,page.playStart,pf.duration_ms||0,meta,attempt);
    return WATCH===w&&w.accepted===exactWireId(page.item);
  }catch(e){if(current())toast("Could not load this episode. Try again.");return false;}
}
async function watchLoadSeasons(page){
  const w=WATCH;
  const show=page.ancestors.find(a=>a.kind==="show"),season=page.ancestors.find(a=>a.kind==="season");
  if(!season)return;
  try{
    const showData=show?await api(`/items/${exactWireId(show)}`):null;
    if(WATCH!==w)return;
    const seasons=(showData?.children||[season]).filter(s=>s.kind==="season");
    const lower=document.getElementById("watch-lower");
    lower.innerHTML=`<div class="watch-season"><h2>Episodes</h2><button id="watch-row-toggle" class="ghost" onclick="watchToggleEpisodeRows()" aria-pressed="false">Rows</button><label>Season <select id="watch-season">${seasons.map(s=>`<option value="${esc(exactWireId(s))}" ${exactWireId(s)===exactWireId(season)?"selected":""}>${esc(s.title)}</option>`).join("")}</select></label></div><div id="watch-episodes" class="watch-episodes"></div>`;
    document.getElementById("watch-season").addEventListener("change",e=>watchSelectSeason(e.target.value));
    await watchSelectSeason(exactWireId(season));
  }catch(e){if(WATCH===w)document.getElementById("watch-lower").innerHTML='<p>Episodes could not load. <button onclick="watchLoadSeasons(WATCH.page)">Retry</button></p>';}
}
async function watchSelectSeason(id){
  const w=WATCH;if(!w)return;
  const load=++w.seasonLoad;
  const target=document.getElementById("watch-episodes");target.innerHTML='<p class="muted">Loading episodes…</p>';
  try{
    const data=await api(`/items/${id}`);
    if(WATCH!==w||load!==w.seasonLoad)return;
    w.episodes=(data.children||[]).filter(e=>e.kind==="episode");
    target.innerHTML=w.episodes.map(e=>{
      const id=exactWireId(e),art=e.backdrop||e.poster;
      return `<article class="watch-episode"><button class="watch-inspect" data-watch-details="${esc(id)}">${art?`<img src="${esc(tok(art))}" alt="" loading="lazy">`:'<span class="watch-art-fallback">▶</span>'}<strong>${esc(`E${e.episode_number||0} · ${e.title}`)}</strong><span class="muted">${e.watch?.watched?"✓ Watched":e.watch?.position_ms>3000&&e.runtime_ms?esc(fmtDur(Math.max(0,e.runtime_ms-e.watch.position_ms)))+" left":e.runtime_ms?esc(fmtDur(e.runtime_ms)):"Episode"}</span></button><button class="ghost" data-watch-play="${esc(id)}">Play</button></article>`;
    }).join("");
    target.querySelectorAll('[data-watch-details]').forEach(b=>b.addEventListener('click',()=>watchInspectEpisode(b.dataset.watchDetails)));
    target.querySelectorAll('[data-watch-play]').forEach(b=>b.addEventListener('click',()=>watchPlayEpisode(b.dataset.watchPlay)));
    watchRenderPlaying();
  }catch(e){if(WATCH===w&&load===w.seasonLoad)target.innerHTML='<p>Episodes could not load. Change season to retry.</p>';}
}
function watchRenderPlaying(){
  if(!WATCH)return;
  document.querySelectorAll('[data-watch-play]').forEach(b=>{const playing=b.dataset.watchPlay===WATCH.accepted;b.disabled=playing;b.textContent=playing?"Playing":(WATCH.episodes?.find(e=>exactWireId(e)===b.dataset.watchPlay)?.watch?.position_ms>3000?"Resume":"Play");b.closest('article')?.classList.toggle('playing',playing);});
}
function watchInspectEpisode(id){
  const e=WATCH?.episodes?.find(e=>exactWireId(e)===id);if(!e)return;
  const d=document.getElementById("watch-details");
  d.innerHTML=`<button class="ghost" onclick="this.closest('dialog').close()">Close details</button><h2>${esc(e.title)}</h2><p>${esc(e.overview||"No synopsis available.")}</p><button data-watch-play="${esc(id)}">Play</button>`;
  d.querySelector('[data-watch-play]').addEventListener('click',()=>{d.close();watchPlayEpisode(id);});watchRenderPlaying();d.showModal();
}
function watchRenderChapters(){
  const w=WATCH;if(!w?.page||w.page.item.kind==="episode")return;
  const file=w.page.files.find(f=>String(f.id)===String(PLAYER?.fileId))||w.page.playable;
  const chapters=file?.chapters||[];
  const lower=document.getElementById("watch-lower");if(!lower)return;
  lower.hidden=!chapters.length;
  lower.innerHTML=chapters.length?`<h2>Chapters</h2><div class="watch-chapters">${chapters.map((c,i)=>`<button class="ghost" data-watch-chapter="${i}" data-start-ms="${c.start_ms}"><span class="watch-art-fallback">${String(i+1).padStart(2,"0")}</span><strong>${esc(c.title||`Chapter ${i+1}`)}</strong><span>${esc(clockFromSec(c.start_ms/1000))}</span></button>`).join("")}</div>`:"";
  lower.querySelectorAll('[data-watch-chapter]').forEach(b=>b.addEventListener('click',()=>{if(WATCH===w&&w.accepted)seekTo(Number(b.dataset.startMs)/1000);}));
}
function watchMarkChapter(){
  if(!WATCH?.accepted)return;
  const buttons=Array.from(document.querySelectorAll('[data-watch-chapter]'));
  const position=pbPosSec()*1000;
  const remaining=document.getElementById("watch-remaining");if(remaining)remaining.textContent=fmtDur(Math.max(0,pbTotalSec()*1000-position))+" left";
  buttons.forEach((b,i)=>{const current=position>=Number(b.dataset.startMs)&&position<(buttons[i+1]?Number(buttons[i+1].dataset.startMs):pbTotalSec()*1000);if(current)b.setAttribute('aria-current','true');else b.removeAttribute('aria-current');});
}
