"use strict";
// ---- cards ----------------------------------------------------------------
function progressPct(w){ if(!w||!w.duration_ms||!w.position_ms) return 0; return Math.min(100,100*w.position_ms/w.duration_ms); }
// Time-left + projected finish clock ("ends ~10:45 PM"), in the browser's local
// time. new Date()/Date.now() are fine here — this is browser JS, not a workflow.
function itemDurMs(it){ return (it&&it.watch&&it.watch.duration_ms) || (it&&it.runtime_ms) || 0; }
function fmtClock(d){ try{ return d.toLocaleTimeString([], {hour:"numeric",minute:"2-digit"}); }catch(e){ return ""; } }
function endsAt(remMs){ if(!remMs||remMs<0) return ""; return fmtClock(new Date(Date.now()+remMs)); }
// "2022-02-18" → "Feb 18, 2022" (plain text; callers escape). Falls back to the
// raw string for anything that isn't an ISO date. Parsed by hand rather than
// through `new Date("2022-02-18")`, which is UTC midnight and so reads a day
// early in every timezone west of Greenwich.
function fmtDate(s){ const m=/^(\d{4})-(\d{2})-(\d{2})/.exec(s||""); if(!m) return s||"";
  const mo=["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"][(+m[2])-1]||""; return `${mo} ${+m[3]}, ${m[1]}`; }
// The 4-digit year from an ISO date/timestamp, or null.
function airYear(s){ const m=/^(\d{4})/.exec(s||""); return m?+m[1]:null; }
// One meta line for a card in a watch-oriented row (Continue watching / Next up):
// time left on a partial item (or full runtime), plus when it would finish now.
function watchLine(it, watchCtx){
  if(!watchCtx) return "";
  const dur=itemDurMs(it); if(!dur) return "";
  const w=it.watch;
  if(w && w.watched) return "";
  const partial = w && w.position_ms>3000;
  const rem = partial ? Math.max(0, dur - w.position_ms) : dur;
  const end = endsAt(rem);
  return `<div class="cleft">${partial?fmtDur(rem)+" left":fmtDur(rem)}${end?" · ends ~"+end:""}</div>`;
}
// Resolution helpers. `resolution` is the item's best file height (movies only,
// on the library grid). Labels reuse resLabel() (height-only here) so a card's
// badge and its section header read the same as the video spec badges.
// Tier class for the badge/dot colour ramp. Thresholds match resLabel() exactly
// so the colour and the text can never disagree.
function resClass(h){ h=h||0;
  return h>=1700?"r4k" : h>=1300?"r1440" : h>=900?"r1080" : h>=650?"r720" : h>=400?"r480" : ""; }
function resBucket(it){ const h=it.resolution||0;
  const r = h>=1700?0 : h>=1300?1 : h>=900?2 : h>=650?3 : h>=400?4 : h>0?5 : 6;
  return { r, l: h?resLabel(0,h):"Unknown resolution", c: resClass(h) }; }
// Group items (already server-sorted height-desc) into resolution sections.
function resSections(items){
  const groups=new Map();
  for(const it of items){ const b=resBucket(it); if(!groups.has(b.r)) groups.set(b.r,{l:b.l,c:b.c,items:[]}); groups.get(b.r).items.push(it); }
  return [...groups.keys()].sort((a,b)=>a-b).map(r=>{ const g=groups.get(r);
    return `<h2 class="section ressection"><span class="resdot resgrad ${g.c}"></span>${esc(g.l)}<span class="muted" style="font-size:12px">${g.items.length}</span></h2>${grid(g.items)}`;
  }).join("");
}
function card(it, watchCtx){
  // Home-library cards: a folder leads with what's inside, a clip with when it
  // was taken and how long it runs, a photo with a square thumb you click to
  // open full-screen.
  if(it.kind==='folder'||it.kind==='photo') return homeCard(it);
  const img = artHtml(it);
  const resb = it.resolution
      ? `<span class="resbadge resgrad ${resClass(it.resolution)}">${esc(resLabel(0,it.resolution))}</span>` : "";
  // On rail cards the series is the headline and the episode (SxEy · name) is
  // the secondary line — a show's name matters more than which episode this is.
  // `show_title` is only set on rail rows; the season page renders episodes via
  // episodeRow() (no show_title), so those keep the episode name as the title.
  const isEp = it.kind==='episode';
  const ep = isEp ? `S${it.season_number||0}E${it.episode_number||0}` : '';
  const main = isEp && it.show_title ? it.show_title : it.title;
  const sub = isEp
      ? esc(it.show_title ? (it.title ? `${ep} · ${it.title}` : ep) : ep)
      : it.kind==='video'
        ? esc(fmtDate(it.recorded_at))
        : it.kind==='audiobook'
          ? esc([it.author||'Audiobook',it.runtime_ms?fmtDur(it.runtime_ms):''].filter(Boolean).join(' · '))
        : it.kind==='book'
          ? esc(it.author||'Ebook')
        : esc(it.year? String(it.year): (it.kind==='show'?'Series':''));
  const dur = (it.kind==='video'||it.kind==='audiobook')&&it.runtime_ms? `<span class="durbadge">${esc(fmtDur(it.runtime_ms))}</span>`:"";
  const pct = progressPct(it.watch);
  const watched = it.watch&&it.watch.watched? `<span class="badge">✓</span>`:"";
  const prog = pct>0&&pct<100? `<div class="prog"><i style="width:${pct}%"></i></div>`:"";
  // Watch state and item kind as classes, so a layout that marks unwatched
  // titles can ask a question instead of inferring the answer from which
  // children happen to be absent. The :not(:has(.badge)):not(:has(.prog))…
  // chain this replaces was both fragile and wrong: a folder card has no watch
  // state either, so it got flagged as unwatched.
  const cls = `poster k-${esc(it.kind||"item")}${(!watched&&!prog)?" unw":""}`;
  const itemId=exactWireId(it);
  return `<div class="${cls}" onclick="location.hash='#/item/${itemId}'">
    <div class="artbox">${img}${dur}${watched}</div><div class="meta"><div class="t">${esc(main)}</div><div class="s"><span class="stxt">${sub}</span>${resb}</div>${watchLine(it,watchCtx)}</div>${prog}</div>`;
}
// Folder and photo cards. A folder opens its page; a photo opens the lightbox
// in place, because paging through a shoebox of stills via a detail page each
// time is nobody's idea of looking at photos.
function homeCard(it){
  const itemId=exactWireId(it);
  if(it.kind==='folder'){
    const n=it.child_count;
    const sub=n==null? "Folder" : `${n} item${n===1?'':'s'}`;
    return `<div class="poster k-folder" onclick="location.hash='#/item/${itemId}'">
      <div class="artbox">${artHtml(it)}</div>
      <div class="meta"><div class="t">${esc(it.title)}</div><div class="s"><span class="stxt">${esc(sub)}</span></div></div></div>`;
  }
  return `<div class="poster k-photo" onclick="openLightbox(${it.id})">
    <div class="artbox">${artHtml(it,'sq')}</div>
    <div class="meta"><div class="t">${esc(it.title)}</div><div class="s"><span class="stxt">${esc(fmtDate(it.recorded_at))}</span></div></div></div>`;
}
function grid(items){
  // Remember this grid's photos so the lightbox can walk them with ←/→.
  PHOTO_SET=(items||[]).filter(i=>i.kind==='photo').map(i=>({id:exactWireId(i),title:i.title,recorded_at:i.recorded_at}));
  return items.length? `<div class="grid">${items.map(i=>card(i)).join("")}</div>` : `<div class="empty">Nothing here yet.</div>`;
}

