"use strict";
// ---- views ----------------------------------------------------------------
// ---- get-the-app install prompt -------------------------------------------
// Android can sideload the native APK (served by this server when published);
// iOS can't, so it gets "Add to Home Screen" (PWA) + a pointer to the Xcode
// source. Shown as a dismissible home banner and a header button.
function appPlatform(){
  const ua=navigator.userAgent||"";
  if(/android/i.test(ua)) return /(\btv\b|bravia|shield|aft[a-z]|googletv|chromecast)/i.test(ua)?"androidtv":"android";
  if(/iphone|ipad|ipod/i.test(ua) || (/Macintosh/.test(ua)&&navigator.maxTouchPoints>1)) return "ios";
  return "other";
}
function appStandalone(){
  try{ return (window.matchMedia&&window.matchMedia("(display-mode: standalone)").matches)||window.navigator.standalone===true; }catch(e){ return false; }
}
function installAvailable(){
  if(appStandalone()) return false;
  const p=appPlatform();
  if(p==="ios") return true;                                   // Add to Home Screen
  if(p==="android"||p==="androidtv") return !!(SERVER&&SERVER.android_app);
  return false;
}
function installDismissed(){ try{ return localStorage.getItem("plurx_getapp_dismissed")==="1"; }catch(e){ return false; } }
function dismissInstall(){ try{ localStorage.setItem("plurx_getapp_dismissed","1"); }catch(e){} const b=document.getElementById("getapp"); if(b) b.remove(); }
function showInstall(){ try{ localStorage.removeItem("plurx_getapp_dismissed"); }catch(e){} location.hash="#/"; render(); }
function installBannerHtml(){
  if(!installAvailable()||installDismissed()) return "";
  const p=appPlatform();
  if(p==="android"||p==="androidtv"){
    const tvNote = p==="androidtv"
      ? `On Android TV, sideload it with the <b>Downloader</b> app (enter this server's address) or <code>adb install</code>.`
      : `Open the downloaded file to install — allow installs from your browser if prompted.`;
    return `<div class="getapp" id="getapp">
      <div><b>Get the Cinema Android app</b><div class="muted" style="font-size:13px">Native playback — direct-plays MKV · HEVC · 4K that a browser can't. ${tvNote}</div></div>
      <div class="row" style="gap:8px">
        <a class="btn" href="/download/plurx-android.apk">Download APK</a>
        <button class="ghost sm" onclick="dismissInstall()">Dismiss</button>
      </div></div>`;
  }
  return `<div class="getapp" id="getapp">
      <div><b>Install Cinema</b><div class="muted" style="font-size:13px">Tap <b>Share</b> → <b>Add to Home&nbsp;Screen</b> for a full-screen app. A native iOS / Apple&nbsp;TV app can be built from <code>clients/apple</code> in the source (Xcode).</div></div>
      <button class="ghost sm" onclick="dismissInstall()">Dismiss</button></div>`;
}

// Home library grouping: "category" (Movies / TV Shows, merging shares of the
// same kind — the default, since what you want to watch is a kind of thing, not
// a share name) or "share" (one section per library). Persisted in localStorage
// like the other viewing prefs.
// A coming-soon card.
//
// There is still nothing to PLAY — the file does not exist yet — but there is
// something to show. The server prefers a local poster resolved by external
// id, then downloads and caches the provider poster path Curator supplied. The
// browser only ever sees plurx's image route; a title whose provider named no
// poster gets the same initials tile the grids use.
//
// It clicks through to the item when there is one, and is inert when there is
// not: a card that navigates nowhere is worse than one that plainly cannot.
function comingCard(e){
  const when = e.has_file ? "Arrived early" : expectedOn(e.date);
  const chip = e.has_file ? "Here" : shortWhen(e.date);
  const art = artHtml({poster:e.poster, title:e.title, kind:"soon"});
  const go = e.item_id ? ` onclick="location.hash='#/item/${e.item_id}'" style="cursor:pointer"`
                       : ' style="cursor:default"';
  return `<div class="poster"${go}>
    <div class="artbox">${art}<div class="soonwhen${e.has_file?" here":""}" title="${esc(when)}">${esc(chip)}</div></div>
    <div class="meta">
      <div class="t" title="${esc(e.title||"")}">${esc(e.title||"")}</div>
      <div class="cleft soondetail">${esc(e.detail||"")}</div>
    </div>
  </div>`;
}

// The chip form: just the when, because the rail above it already says
// "Coming soon" and a badge 120px wide cannot spend 60 of them on the word
// "Expected" without truncating the part that carries the information. The
// full sentence stays in the tooltip.
function shortWhen(date){
  const full = expectedOn(date);
  return full.replace(/^Expected /, "").replace(/^expected /, "") || full;
}

// "Expected Friday" beats "expected 2026-08-01" for anything inside a week —
// that is the horizon a person actually holds in their head.
function expectedOn(date){
  if(!date) return "Expected";
  const d = new Date(date+"T00:00:00");
  if(isNaN(d)) return "Expected "+date;
  const days = Math.round((d - new Date(new Date().toDateString()))/86400000);
  if(days<=0) return "Expected today";
  if(days===1) return "Expected tomorrow";
  if(days<7) return "Expected "+d.toLocaleDateString(undefined,{weekday:"long"});
  return "Expected "+d.toLocaleDateString(undefined,{month:"short",day:"numeric"});
}

function homeGroup(){ try{ return localStorage.getItem("plurx_home_group")||"category"; }catch(e){ return "category"; } }
function setHomeGroup(v){ try{ localStorage.setItem("plurx_home_group",v); }catch(e){} viewHome(); }
function libCategory(lib){
  if(lib.kind==="movie"||lib.kind==="movies") return {key:"movie", name:"Movies"};
  if(lib.kind==="show"||lib.kind==="shows")   return {key:"show",  name:"TV Shows"};
  if(lib.kind==="book"||lib.kind==="books")   return {key:"book",  name:"Books"};
  if(lib.kind==="home")  return {key:"home",  name:"Home videos"};
  const k=lib.kind||"other";
  return {key:k, name:k.charAt(0).toUpperCase()+k.slice(1)};
}
function catOrder(key){ return key==="movie"?0:key==="show"?1:key==="book"?2:key==="home"?3:4; }
function groupToggleHtml(group){
  return `<div class="libbar" style="margin-top:22px"><div class="row">
    <span class="lbl">Group by</span>
    <select onchange="setHomeGroup(this.value)">${libOpt("category","Category",group)}${libOpt("share","Share name",group)}</select>
  </div></div>`;
}
