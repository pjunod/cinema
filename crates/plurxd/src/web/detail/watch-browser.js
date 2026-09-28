"use strict";
// The watch page beside and below the picture: the title panel with its
// media ledger, the chapter rail, and the episode browser. The player owns
// playback; this file only reads it (PLAYER) and asks it for things (seekTo,
// switchAudio, setSub).
//
// Folds — the chapter rail and the Audio / Subtitles rows — remember their
// state per browser the way the player size does. A folded track row shows
// the selected track and a count, because a file can carry a dozen tracks
// and the row must not grow the panel past the picture.
const WATCH_FOLD_KEY="plurx_watch_folds";
function watchFolds(){
  if(WATCH&&WATCH.folds)return WATCH.folds;
  let saved={};
  try{saved=JSON.parse(localStorage.getItem(WATCH_FOLD_KEY)||"{}")||{};}catch(e){saved={};}
  const folds={rail:saved.rail!==false,audio:saved.audio===true,subs:saved.subs===true};
  if(WATCH)WATCH.folds=folds;
  return folds;
}
function watchToggleFold(which){
  const folds=watchFolds();
  folds[which]=!folds[which];
  try{localStorage.setItem(WATCH_FOLD_KEY,JSON.stringify(folds));}catch(e){}
  if(which==="rail")watchRenderChapters();else watchRenderLedger();
}
function watchCurrentFile(){
  if(!WATCH?.page)return null;
  return WATCH.page.files.find(f=>String(f.id)===String(PLAYER?.fileId))||WATCH.page.playable||null;
}
function watchMetaLine(page){
  const m=page.meta;
  const facts=[m.show,m.season!=null?`S${String(m.season).padStart(2,"0")}E${String(m.episode).padStart(2,"0")}`:null,m.year,m.runtime_ms?fmtDur(m.runtime_ms):null].filter(Boolean).map(esc);
  const left=Math.max(0,(page.runtime||0)-(WATCH?.accepted?pbPosSec()*1000:0));
  if(page.runtime)facts.push(`<b class="watch-remaining">${esc(fmtDur(left))} left</b>`);
  return facts.join(" · ");
}
// The same block feeds the compact side panel and the wide band under the
// rail; CSS shows whichever the state calls for.
function watchTitleHtml(){
  const page=WATCH.page;
  const next=page.item.kind==="episode"?`<div class="watch-actions"><button type="button" class="ghost watch-play-next" onclick="playNextEpisode()">Play next</button></div>`:"";
  return `<span class="watch-np-kicker">${WATCH.accepted?"Now playing":"Starting"}</span><h2>${esc(page.item.title)}</h2><p class="watch-meta">${watchMetaLine(page)}</p><div class="vbadges">${WATCH.accepted?playerFactBadges():""}</div><p class="watch-synopsis">${esc(page.meta.overview||"")}</p>${next}`;
}
// The heading is the item page's breadcrumb. It is re-rendered with the
// title, because playing another episode from the grid changes the last
// crumb and the season above it.
function watchHeadHtml(){
  const page=WATCH.page;
  const trail=[{href:"#/",label:"Home"}]
    .concat(NAV_ORIGIN?[{href:NAV_ORIGIN.href,label:NAV_ORIGIN.label}]:[])
    .concat((page.ancestors||[]).map(a=>({href:`#/item/${exactWireId(a)}`,label:a.title})))
    .concat([{label:page.item.title}]);
  return pageHead(trail);
}
function watchRenderTitle(){
  if(!WATCH?.page)return;
  const html=watchTitleHtml();
  const head=document.getElementById("watch-head");if(head)head.innerHTML=watchHeadHtml();
  const title=document.getElementById("watch-title");if(title)title.innerHTML=html;
  const band=document.getElementById("watch-band-title");if(band)band.innerHTML=html;
  watchRenderLedger();
}
// Which stream the player is on right now, by container stream index, or the
// file's default when nothing is playing yet.
function watchSelectedAudio(file){
  if(PLAYER&&PLAYER.audio&&PLAYER.curAudio>=0){const t=PLAYER.audio[PLAYER.curAudio];if(t)return t.index;}
  const d=file.playback_defaults?.audio;return d&&d.selected_index!=null?d.selected_index:-1;
}
function watchSelectedSub(file){
  if(PLAYER){if(PLAYER.burnedSub!=null)return PLAYER.burnedSub;if(PLAYER.curSub!=null)return PLAYER.curSub;}
  const d=file.playback_defaults?.subtitle;return d&&d.selected_index!=null?d.selected_index:-1;
}
function watchTrackChip(label,on,action,title){
  const cls=`trk${on?" on":""}`;
  if(!action)return `<span class="${cls}" ${title?`title="${esc(title)}"`:""}>${esc(label)}</span>`;
  return `<button type="button" class="${cls}" ${action} aria-pressed="${on?"true":"false"}">${esc(label)}</button>`;
}
function watchFoldButton(which,open){
  return `<button type="button" class="watch-fold" data-watch-fold="${which}" aria-expanded="${open?"true":"false"}" aria-label="${open?"Show only the selected track":"Show every track"}">${open?"▴":"▾"}</button>`;
}
function watchTrackRow(which,tracks,selected,label,offChip){
  const open=!!watchFolds()[which];
  const chips=[];
  if(offChip)chips.push(offChip(selected<0));
  tracks.forEach(t=>chips.push(label(t,t.index===selected)));
  if(open||chips.length<=1)return `<div class="watch-trks">${chips.join("")}${chips.length>1?watchFoldButton(which,true):""}</div>`;
  const chosen=tracks.find(t=>t.index===selected);
  const current=chosen?label(chosen,true):offChip?offChip(true):label(tracks[0],false);
  const rest=tracks.filter(t=>t!==(chosen||(offChip?null:tracks[0])));
  const names=rest.slice(0,3).map(t=>langName(t.language)||"Untagged");
  const summary=which==="audio"
    ?`<b>${rest.length}</b> more`
    :`<b>${rest.length}</b> available${names.length?" · "+esc(names.join(", "))+(rest.length>names.length?` +${rest.length-names.length}`:""):""}`;
  return `<div class="watch-trks">${current}<button type="button" class="watch-trkmore" data-watch-fold="${which}">${summary}</button>${watchFoldButton(which,false)}</div>`;
}
function watchDeliveryRow(f){
  const s=f.vod_index_status;
  if(s==="indexed")return `<span class="mode-chip vod">VOD HLS</span><span class="mode-detail">Fixed, seekable timeline · analysis ready</span>`;
  if(s==="partial")return `<span class="mode-chip vod">VOD HLS on some devices</span><span class="mode-detail">Analysis ready for some delivery routes</span>`;
  if(s==="pending")return `<span class="mode-chip live">Live HLS fallback</span><span class="mode-detail">${f.vod_index_refusal?esc(f.vod_index_refusal)+" — it will be tried again":"VOD analysis pending"}</span>`;
  if(s==="refused")return `<span class="mode-chip live">Live HLS only</span><span class="mode-detail">VOD analysis ${f.vod_index_refusal?esc(f.vod_index_refusal):"was refused for this file"}</span>`;
  if(s==="unsupported")return `<span class="mode-chip live">Live HLS fallback</span><span class="mode-detail">This codec cannot use the VOD indexer</span>`;
  return "";
}
function watchLedgerHtml(){
  const f=watchCurrentFile();if(!f)return "";
  const vid=[f.video_codec&&f.video_codec.toUpperCase(),(f.width&&f.height)?`${f.width}×${f.height}`:null,f.bit_depth?`${f.bit_depth}-bit`:null,(f.hdr_format||f.hdr||(f.video_codec?"SDR":null)),fmtMbps(f.bitrate)].filter(Boolean).join(" · ");
  const auds=f.audio_streams||[],subs=f.subtitle_streams||[];
  const playing=!!(PLAYER&&WATCH?.accepted);
  const offered=kind=>new Set(((PLAYER&&PLAYER[kind])||[]).map(t=>t.index));
  const audioOffered=offered("audio"),subOffered=offered("subs");
  const selA=watchSelectedAudio(f),selS=watchSelectedSub(f);
  const audioLabel=(a,on)=>watchTrackChip(audioFactLabel(a),on,playing&&audioOffered.has(a.index)?`data-watch-audio="${a.index}"`:null,playing?"Not offered for this playback":"Starts when playback does");
  const subLabel=(s,on)=>watchTrackChip(subFactLabel(s),on,playing&&subOffered.has(s.index)?`data-watch-sub="${s.index}"`:null,playing?"Not offered for this playback":"Starts when playback does");
  const offChip=on=>watchTrackChip("Off",on,playing?'data-watch-sub="-1"':null);
  const delivery=watchDeliveryRow(f);
  const file=[f.container&&f.container.toUpperCase(),fmtSize(f.size),fmtDur(f.duration_ms)].filter(Boolean).join(" · ");
  return `<dl class="watch-ledger">
    ${vid?`<dt>Video</dt><dd>${esc(vid)}</dd>`:""}
    <dt>Audio</dt><dd>${auds.length?watchTrackRow("audio",auds,selA,audioLabel,null):"None"}</dd>
    ${(subs.length||f.video_codec)?`<dt>Subtitles</dt><dd>${watchTrackRow("subs",subs,selS,subLabel,offChip)}</dd>`:""}
    ${delivery?`<dt>Delivery</dt><dd>${delivery}</dd>`:""}
    <dt>File</dt><dd><span class="watch-fn">${esc(f.filename||"")}</span>${file?`<span class="watch-fmeta">${esc(file)}</span>`:""}</dd></dl>`;
}
// One binder for every place the ledger renders — the side panel, the wide
// band and the ⓘ dialog — so a chip is a control wherever it appears.
function watchBindLedger(node){
  node.querySelectorAll("[data-watch-audio]").forEach(b=>b.addEventListener("click",()=>{
    const want=Number(b.dataset.watchAudio);const i=(PLAYER?.audio||[]).findIndex(t=>t.index===want);
    if(i>=0&&i!==PLAYER.curAudio)switchAudio(i);
  }));
  node.querySelectorAll("[data-watch-sub]").forEach(b=>b.addEventListener("click",()=>{
    const want=Number(b.dataset.watchSub),f=watchCurrentFile();
    // Re-applying the selected subtitle is not a no-op in the player: a
    // burned track would start a fresh transcode. Refuse it here.
    if(PLAYER&&f&&want!==watchSelectedSub(f))setSub(want);
  }));
  node.querySelectorAll("[data-watch-fold]").forEach(b=>b.addEventListener("click",()=>watchToggleFold(b.dataset.watchFold)));
}
function watchRenderLedger(){
  if(!WATCH?.page)return;
  const html=watchLedgerHtml();
  // A re-render replaces the control that had focus; put focus back on the
  // same control so a keyboard fold or switch does not drop to <body>.
  const active=document.activeElement,keep=active&&active.closest&&active.closest(".watch-ledger-host")?["watchAudio","watchSub","watchFold"].map(k=>active.dataset[k]!=null?`[data-${k.replace(/[A-Z]/g,m=>"-"+m.toLowerCase())}="${active.dataset[k]}"]`:null).find(Boolean):null;
  for(const id of ["watch-ledger","watch-band-ledger"]){
    const node=document.getElementById(id);if(!node)continue;
    node.innerHTML=html;
    watchBindLedger(node);
    if(keep&&active.closest(`#${id}`)){const again=node.querySelector(keep);if(again)again.focus({preventScroll:true});}
  }
  const f=watchCurrentFile();
  WATCH.ledgerSig=f?`${watchSelectedAudio(f)}|${watchSelectedSub(f)}|${WATCH.accepted}`:"";
}
// The player bar's ⓘ in the compact and wide states. The facts are on the
// page, but the transport row is pinned by the surface contract, so the
// button stays and shows the same panel as a dialog.
function watchShowTitleInfo(){
  const d=document.getElementById("watch-details");
  if(!d||!WATCH?.page)return;
  d.innerHTML=`<button class="ghost" onclick="this.closest('dialog').close()">Close title info</button><div class="watch-title">${watchTitleHtml()}</div><div class="watch-ledger-host">${watchLedgerHtml()}</div>`;
  watchBindLedger(d);d.showModal();
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
  const lowerHost=document.getElementById("watch-lower");if(lowerHost)lowerHost.hidden=false;
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
// Chapters.
//
// A rail under the picture: a ruler whose segments are each chapter's share
// of the runtime with the playhead drawn across it, then a strip of
// thumbnails. Folded, the strip goes and the ruler moves into the header
// line, so the chapter map stays visible at one line of height.
function watchChapters(){
  const file=watchCurrentFile();
  return file?.chapters||[];
}
function watchChapterThumbUrl(file,i){
  return tok(`/api/v1/files/${encodeURIComponent(file.id)}/chapters/${i}/thumb?v=${encodeURIComponent(String(file.mtime||file.size||0))}`);
}
function watchRulerHtml(chapters,totalMs,cls){
  if(!chapters.length||!totalMs)return "";
  const marks=chapters.slice(1).map(c=>`<em style="left:${(100*Math.min(1,Math.max(0,c.start_ms/totalMs))).toFixed(2)}%"></em>`).join("");
  return `<div class="watch-ruler${cls?" "+cls:""}" aria-hidden="true"><i data-watch-playhead style="width:0%"></i>${marks}</div>`;
}
function watchRenderChapters(){
  const w=WATCH;if(!w?.page)return;
  const rail=document.getElementById("watch-rail");if(!rail)return;
  const chapters=w.page.item.kind==="episode"?[]:watchChapters();
  rail.hidden=!chapters.length;
  if(!chapters.length){rail.innerHTML="";return;}
  const file=watchCurrentFile();
  const open=!!watchFolds().rail;
  const totalMs=(file&&file.duration_ms)||w.page.runtime||0;
  rail.classList.toggle("watch-rail-open",open);
  rail.innerHTML=`<div class="watch-rail-head"><h2>Chapters</h2><span class="watch-rail-now muted"><b id="watch-chapter-now"></b> · ${chapters.length} chapters · <span id="watch-chapter-pos"></span></span>${open?"":watchRulerHtml(chapters,totalMs,"watch-ruler-mini")}<button type="button" class="watch-fold" data-watch-fold="rail" aria-expanded="${open?"true":"false"}" aria-label="${open?"Hide chapter thumbnails":"Show chapter thumbnails"}">${open?"▴":"▾"}</button></div>${open?watchRulerHtml(chapters,totalMs,"")+`<div class="watch-chapters">${chapters.map((c,i)=>{
    const len=(chapters[i+1]?chapters[i+1].start_ms:totalMs)-c.start_ms;
    return `<button type="button" class="watch-chapter" data-watch-chapter="${i}" data-start-ms="${c.start_ms}" title="${esc(c.title||`Chapter ${i+1}`)} · ${esc(clockFromSec(c.start_ms/1000))}${len>0?" · "+esc(fmtDur(len))+" long":""}"><span class="watch-chapter-art" data-thumb="${esc(watchChapterThumbUrl(file,i))}"><span class="watch-art-fallback">${String(i+1).padStart(2,"0")}</span><span class="watch-chapter-n">${String(i+1).padStart(2,"0")}</span><span class="watch-chapter-tc">${esc(clockFromSec(c.start_ms/1000))}</span><span class="watch-chapter-prog"><i></i></span></span><strong>${esc(c.title||`Chapter ${i+1}`)}</strong></button>`;
  }).join("")}</div>`:""}`;
  rail.querySelectorAll('[data-watch-chapter]').forEach(b=>b.addEventListener('click',()=>{if(WATCH===w&&w.accepted)seekTo(Number(b.dataset.startMs)/1000);}));
  rail.querySelectorAll('[data-watch-fold]').forEach(b=>b.addEventListener('click',()=>watchToggleFold(b.dataset.watchFold)));
  w.chapterMarked=-1;
  watchMarkChapter();
  watchLoadThumbs();
}
// Thumbnails load two at a time, in order, and only once playback has been
// accepted. Eight <img> tags firing together would take most of the
// browser's connections to a plain-HTTP origin while the stream is starting,
// and each first-time request holds its connection for an ffmpeg seek. A
// failed load gets one retry after a pause (a 503 means the node's two
// extraction slots were busy); after that the tile keeps its number.
const WATCH_THUMB_LANES=2;
function watchLoadThumbs(){
  const w=WATCH;if(!w?.accepted)return;
  const rail=document.getElementById("watch-rail");if(!rail||rail.hidden)return;
  const pending=Array.from(rail.querySelectorAll(".watch-chapter-art[data-thumb]"));
  if(!pending.length||w.thumbLoading)return;
  w.thumbLoading=true;
  let active=0;
  const next=()=>{
    while(active<WATCH_THUMB_LANES&&pending.length){
      const art=pending.shift();
      if(WATCH!==w||!art.isConnected)continue;
      const url=art.dataset.thumb;delete art.dataset.thumb;
      active++;
      const attempt=(retry)=>{
        const img=new Image();img.decoding="async";
        img.onload=()=>{if(art.isConnected){img.alt="";art.prepend(img);}active--;next();};
        img.onerror=()=>{
          if(retry&&WATCH===w){setTimeout(()=>{if(WATCH===w&&art.isConnected)attempt(false);else{active--;next();}},4000);return;}
          art.classList.add("watch-art-missing");active--;next();
        };
        img.src=url;
      };
      attempt(true);
    }
    if(!pending.length&&active===0)w.thumbLoading=false;
  };
  next();
}
function watchMarkChapter(){
  if(!WATCH?.page)return;
  const position=WATCH.accepted?pbPosSec()*1000:0;
  const total=WATCH.accepted?pbTotalSec()*1000:0;
  if(total)document.querySelectorAll(".watch-remaining").forEach(r=>{r.textContent=fmtDur(Math.max(0,total-position))+" left";});
  const f=watchCurrentFile();
  if(f){const sig=`${watchSelectedAudio(f)}|${watchSelectedSub(f)}|${WATCH.accepted}`;if(sig!==WATCH.ledgerSig)watchRenderLedger();}
  const rail=document.getElementById("watch-rail");if(!rail||rail.hidden)return;
  const chapters=watchChapters();if(!chapters.length)return;
  const totalMs=(f&&f.duration_ms)||WATCH.page.runtime||total||0;
  let current=-1;
  chapters.forEach((c,i)=>{if(position>=c.start_ms)current=i;});
  const pos=document.getElementById("watch-chapter-pos");if(pos)pos.textContent=`${clockFromSec(position/1000)} / ${clockFromSec((total||totalMs)/1000)}`;
  const now=document.getElementById("watch-chapter-now");if(now)now.textContent=current>=0?(chapters[current].title||`Chapter ${current+1}`):"—";
  rail.querySelectorAll("[data-watch-playhead]").forEach(p=>{p.style.width=totalMs?`${(100*Math.min(1,Math.max(0,position/totalMs))).toFixed(2)}%`:"0%";});
  const buttons=rail.querySelectorAll('[data-watch-chapter]');
  buttons.forEach((b,i)=>{
    const start=Number(b.dataset.startMs),end=chapters[i+1]?chapters[i+1].start_ms:totalMs;
    if(i===current){b.setAttribute('aria-current','true');const prog=b.querySelector('.watch-chapter-prog i');if(prog)prog.style.width=end>start?`${(100*Math.min(1,Math.max(0,(position-start)/(end-start)))).toFixed(1)}%`:"0%";}
    else b.removeAttribute('aria-current');
  });
  if(current!==WATCH.chapterMarked){
    WATCH.chapterMarked=current;
    // Keep the current chapter in view along the strip only — never scroll the
    // page, which is what scrollIntoView would do while somebody is watching.
    const b=buttons[current],strip=b&&b.parentElement;
    if(b&&strip&&WATCH.accepted){
      const left=b.offsetLeft-strip.offsetLeft,right=left+b.offsetWidth;
      if(left<strip.scrollLeft||right>strip.scrollLeft+strip.clientWidth)strip.scrollTo({left:Math.max(0,left-10),behavior:"smooth"});
    }
  }
}
