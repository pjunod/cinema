"use strict";
// Safe application adapters. B05 supplies network admission; B08 supplies CEC.
let CINEMA_REMOTE_ROUTE=null,CINEMA_REMOTE_RETURN=null,CINEMA_REMOTE_TEXT=null;
let CINEMA_REMOTE_SIGNATURE=null;
const CINEMA_REMOTE_HISTORY=[];
let CINEMA_REMOTE_GESTURE_OWNER=null;
function cinemaRemoteNonce(){
  // getRandomValues also works on the existing HTTP LAN deployment.
  const bytes=crypto.getRandomValues(new Uint8Array(16));bytes[6]=(bytes[6]&15)|64;bytes[8]=(bytes[8]&63)|128;
  const hex=Array.from(bytes,b=>b.toString(16).padStart(2,"0")).join("");
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}
function cinemaRemoteSafeRoute(route){
  return route==="#/"||/^#\/(library|item)\/[1-9][0-9]*$/.test(route)||/^#\/(category|search)\/[^#]*$/.test(route);
}
function cinemaRemoteVisible(el){
  if(!el||!el.isConnected||el.hidden||el.closest("[hidden]")) return false;
  const style=window.getComputedStyle(el);
  return style.display!=="none"&&style.visibility!=="hidden"&&el.getClientRects().length>0;
}
function cinemaRemoteRestrictedOverlay(){
  // Check overlays synchronously at admission, including physically opened ones.
  for(const el of document.querySelectorAll('dialog[open],[aria-modal="true"],[role="dialog"],.amenu.on,.amenu.open,.lookmenu.on,.profilemenu.on,#editwrap,#live-tv-pop:not([hidden]),#live-tv-sheet:not([hidden]),#dvr-reminder')){
    if(el.id==="modal"&&el.classList.contains("open")) continue;
    if(cinemaRemoteVisible(el)) return true;
  }
  for(const id of ["lookmenu","profilemenu","statsov","live-tv-stats"]){
    const el=document.getElementById(id);
    if(cinemaRemoteVisible(el)&&(el.classList.contains("on")||el.classList.contains("open")||id==="live-tv-stats")) return true;
  }
  if(document.activeElement?.tagName==="SELECT")return true;
  const menu=document.getElementById("pmenu");
  return !!(menu?.classList.contains("on")&&!["audio","subs","quality"].includes(menu.dataset.kind));
}
function cinemaRemoteVod(){ return !!document.getElementById("modal")?.classList.contains("open"); }
function cinemaRemoteLive(){ return !!(LIVE_TV_LEASE.current&&cinemaRemoteVisible(liveTvHost())); }
function cinemaRemoteGuard(){
  return !!(TOKEN&&ME&&!cinemaRemoteRestrictedOverlay()&&
    (cinemaRemoteSafeRoute(location.hash||"#/")||location.hash==="#/live-tv"));
}
function cinemaRemoteRegister(id,element,label,activate){
  if(element) CinemaRemote.registerAction({id,element,label,activate});
}
function cinemaRemoteGo(route,returnItem=null,remember=true){
  if(!cinemaRemoteSafeRoute(route)&&route!=="#/live-tv") return "restricted_surface";
  if(remember&&route!==(location.hash||"#/")){
    const state=CinemaRemote.snapshot();
    CINEMA_REMOTE_HISTORY.push({route:location.hash||"#/",id:returnItem||state.focused_id,page:LIB_PAGE_AT});
    if(CINEMA_REMOTE_HISTORY.length>32)CINEMA_REMOTE_HISTORY.shift();
  }
  CinemaRemote.invalidate("route_intent"); location.hash=route; return "applied";
}
function cinemaRemoteBind(){
  const route=location.hash||"#/";
  const menu=document.getElementById("pmenu"),vod=cinemaRemoteVod(),live=cinemaRemoteLive();
  const root=vod?(menu?.classList.contains("on")?menu:document.getElementById("player")):live?liveTvHost():document.getElementById("app");
  if(!root) return;
  const kind=vod?(root===menu?`tracks:${menu.dataset.kind}`:"player"):live?"live-tv":route.split("/")[1]||"home";
  const signature=[route,kind,vod?String(PLAYER?.fileId)+":"+String(PLAYER?.sessionId):live?String(LIVE_TV.serial):"",cinemaRemoteRestrictedOverlay()].join("|");
  if(signature!==CINEMA_REMOTE_SIGNATURE){ CinemaRemote.invalidate("surface"); CINEMA_REMOTE_SIGNATURE=signature; }
  if(route!==CINEMA_REMOTE_ROUTE){ CINEMA_REMOTE_TEXT=null; CINEMA_REMOTE_ROUTE=route; }
  const q=document.getElementById("q");
  if(q&&(!CINEMA_REMOTE_TEXT||CINEMA_REMOTE_TEXT.element!==q)){
    CINEMA_REMOTE_TEXT={element:q,nonce:cinemaRemoteNonce(),replace:text=>cinemaRemoteGo(text?"#/search/"+encodeURIComponent(text):"#/")};
  }
  CinemaRemote.registerScope({id:signature,root,category:kind,text:!vod&&!live?CINEMA_REMOTE_TEXT:null,
    back:()=>{
      const target=CINEMA_REMOTE_HISTORY.pop();
      if(target){CINEMA_REMOTE_RETURN=target;return cinemaRemoteGo(target.route,null,false);}
      return cinemaRemoteGo("#/",null,false);
    },home:()=>{CINEMA_REMOTE_HISTORY.length=0;return cinemaRemoteGo("#/",null,false);},nextPage:()=>{
      if(!route.startsWith("#/library/")&&!route.startsWith("#/category/")) return "unsupported";
      if(!LIB_VIEW||LIB_PER==="all"||LIB_PAGE_AT+1>=libPageCount(LIB_VIEW.shown)) return "unavailable";
      CinemaRemote.invalidate("page");libGoPage(LIB_PAGE_AT+1);cinemaRemoteBind();
      const first=root.querySelector("[data-remote-item]");
      return first?CinemaRemote.focusById("item:"+first.dataset.remoteItem):"unavailable";
    }});
  if(vod){ cinemaRemoteBindPlayer(root,root===menu); return; }
  if(live){ cinemaRemoteBindLive(root);return; }
  if(!cinemaRemoteSafeRoute(route)) return;
  // Only this explicit href grammar registers navigation: handlers are never read.
  const counts=new Map();
  for(const el of document.getElementById("app").querySelectorAll("a[href]")){
    const href=el.getAttribute("href");
    if(!cinemaRemoteSafeRoute(href)) continue;
    const count=counts.get(href)||0;counts.set(href,count+1);
    cinemaRemoteRegister(`route:${href}:${count}`,el,el.textContent.trim(),()=>cinemaRemoteGo(href));
  }
  const itemIds=new Set();
  for(const el of root.querySelectorAll("[data-remote-item]")){
    const id=el.dataset.remoteItem;if(!/^[1-9][0-9]*$/.test(id)||itemIds.has(id))continue;
    itemIds.add(id);
    cinemaRemoteRegister("item:"+id,el,el.querySelector(".t,.eptitle")?.textContent||"Open title",()=>cinemaRemoteGo("#/item/"+id,"item:"+id));
  }
  for(const el of root.querySelectorAll("[data-remote-page]")){
    const page=Number(el.dataset.remotePage);
    cinemaRemoteRegister("page:"+page,el,el.textContent,()=>{CinemaRemote.invalidate("page");libGoPage(page);return "applied";});
  }
  cinemaRemoteRegister("search",q,"Search",()=>{q.focus();return "applied";});
  if(route.startsWith("#/item/")&&WATCH_ITEM_PAGE&&String(exactWireId(WATCH_ITEM_PAGE.item))===route.split("/")[2]){
    const page=WATCH_ITEM_PAGE,pf=page.playable;
    if(pf&&!["book","photo"].includes(page.shape)) cinemaRemoteRegister("play:"+page.id,root.querySelector(".btnplay"),"Play",()=>{
      if(WATCH_ITEM_PAGE!==page) return "stale_context";
      play(pf.id,page.item.title,page.playStart,pf.duration_ms||0,playbackMetaFor(page,pf)).catch(()=>{});return "applied";
    });
  }
}
function cinemaRemoteBindPlayer(root,isMenu){
  if(isMenu){
    for(const el of root.querySelectorAll("[data-remote-track]")){
      const kind=root.dataset.kind,id=el.dataset.remoteTrack;
      cinemaRemoteRegister("track:"+kind+":"+id,el,el.textContent,()=>cinemaRemoteChooseTrack(kind,id));
    }
    return;
  }
  const calls={pbplay:()=>setPlayerPlaying(!playerWantsPlayback(document.getElementById("video"))),
    pbback:()=>seekTo(Math.max(0,pbPosSec()-10)),pbforward:()=>seekTo(Math.min(pbTotalSec(),pbPosSec()+10)),
    pbback30:()=>seekTo(Math.max(0,pbPosSec()-30)),pbforward30:()=>seekTo(Math.min(pbTotalSec(),pbPosSec()+30)),
    pbclose:()=>{closePlayer().catch(()=>{});},pbaudio:()=>toggleMenu("audio",1),pbsubs:()=>toggleMenu("subs",1),pbquality:()=>toggleMenu("quality",1),
    pseek:()=>applyPlayerOutcome(playerSeekPending()?"commit":"toggle_play")};
  for(const [id,activate] of Object.entries(calls)){
    const el=document.getElementById(id);cinemaRemoteRegister(id,el,el?.getAttribute("aria-label")||id,()=>{activate();return "applied";});
  }
}
function cinemaRemoteBindLive(root){
  // Only known transport controls; guide/DVR/settings overlays remain restricted.
  for(const [id,activate] of Object.entries({"live-tv-transport":toggleLiveTvPlayback,"live-tv-status-play":resumeLiveTv})){
    const el=document.getElementById(id);if(el&&root.contains(el))cinemaRemoteRegister(id,el,el.textContent,()=>{activate();return "applied";});
  }
  Array.from(root.querySelectorAll("[data-remote-live-stop]")).forEach((el,index)=>cinemaRemoteRegister("live-stop:"+index,el,"Stop Live TV",()=>{stopLiveTv().catch(()=>{});return "applied";}));
}
function CinemaRemoteCancelGestures(){
  const owner=CINEMA_REMOTE_GESTURE_OWNER;CINEMA_REMOTE_GESTURE_OWNER=null;
  if(!owner)return;
  // A physical gesture may already have superseded the remembered network one.
  if(owner.seek!=null&&owner.player===PLAYER&&PLAYER&&PLAYER._seekPending===owner.seek)cancelPendingSeek();
  if(owner.live===LIVE_TV&&owner.channel!=null&&LIVE_TV.pendingChannel===owner.channel)cancelLiveTvChannelGesture();
}
function CinemaRemoteGestureState(){
  return {player:PLAYER,seek:PLAYER?._seekPending,live:LIVE_TV,channel:LIVE_TV.pendingChannel};
}
function CinemaRemoteRememberGesture(context,before){
  if(context?.source!=="network")return;
  const after=CinemaRemoteGestureState();
  if(!before||before.player!==after.player||before.seek!==after.seek||before.channel!==after.channel)
    CINEMA_REMOTE_GESTURE_OWNER=after.seek!=null||after.channel!=null?after:null;
}
function cinemaRemoteChooseTrack(kind,id){
  const menu=document.getElementById("pmenu");
  if(!menu?.classList.contains("on")||menu.dataset.kind!==kind||!Array.from(menu.querySelectorAll("[data-remote-track]")).some(el=>el.dataset.remoteTrack===id)) return "stale_context";
  if(kind==="audio") switchAudio(Number(id));
  else if(kind==="subs") setSub(Number(id));
  else if(kind==="quality") setQuality(id);
  else return "unsupported";
  return "applied";
}
function CinemaRemoteRouteAction(action,context,nav){
  if(!action||typeof action!=="object"||Array.isArray(action)||typeof action.type!=="string")return "invalid";
  const fields={navigate:["direction"],select:[],back:[],home:[],set_playing:["playing"],seek_relative:["seconds"],seek_absolute:["position_ms"],stop:[],open_tracks:["kind"],choose_track:["kind","option_id"],text_replace:["text_nonce","text"],play_item:["item_id"]};
  if(!Object.hasOwn(fields,action.type))return "unsupported";
  if(Object.keys(action).some(key=>key!=="type"&&!fields[action.type].includes(key))||fields[action.type].some(key=>!(key in action)))return "invalid";
  const vod=cinemaRemoteVod(),live=cinemaRemoteLive();
  if(action.type==="text_replace"){
    if(typeof action.text!=="string"||new TextEncoder().encode(action.text).length>512)return "invalid";
    if(!nav.scope.text||action.text_nonce!==nav.scope.text.nonce)return "stale_context";
    return nav.scope.text.replace(action.text);
  }
  if(action.type==="navigate"||action.type==="select"||action.type==="back"){
    const input=action.type==="navigate"?action.direction:action.type;
    if(action.type==="navigate"&&!["left","right","up","down"].includes(input))return "invalid";
    if(vod){
      const outcome=PlaybackPolicy.routeInput("ten-foot",playerInputState(),input);
      if(outcome==="activate")return nav.activate();
      if(outcome==="focus_row"||outcome==="menu_focus")return nav.navigate(input);
      if(outcome==="ignore")return "unsupported";
      return applyPlayerOutcome(outcome,{direction:input})?"applied":"unsupported";
    }
    if(live){
      const state=liveTvInputState();if(!state)return "unsupported";
      const outcome=PlaybackPolicy.routeLiveInput("ten-foot",state,input);
      if(["activate","focus_control","focus_cell","focus_panel","delegate"].includes(outcome))return action.type==="select"?nav.activate():nav.navigate(input);
      return liveTvApplyOutcome(outcome,{key:input})?"applied":"unsupported";
    }
    if(action.type==="navigate")return nav.navigate(input);
    if(action.type==="select")return nav.activate();
    return nav.scope.back?nav.scope.back():"unsupported";
  }
  if(action.type==="home"){
    if(vod)closePlayer({routeLeave:true}).catch(()=>{});
    else if(live)stopLiveTv().catch(()=>{});
    return nav.scope.home?nav.scope.home():"unsupported";
  }
  if(action.type==="play_item"){
    if(!Number.isSafeInteger(action.item_id)||action.item_id<=0)return "invalid";
    const route="#/item/"+action.item_id;
    if(location.hash===route){
      const page=WATCH_ITEM_PAGE,pf=page?.playable;
      if(!pf||String(exactWireId(page.item))!==String(action.item_id))return "unavailable";
      play(pf.id,page.item.title,page.playStart,pf.duration_ms||0,playbackMetaFor(page,pf)).catch(()=>{});
      return "applied";
    }
    AUTOPLAY=String(action.item_id);
    return cinemaRemoteGo(route);
  }
  if(!vod&&!live)return "unavailable";
  if(action.type==="set_playing"){
    if(typeof action.playing!=="boolean")return "invalid";
    if(vod)setPlayerPlaying(action.playing);else if(action.playing)resumeLiveTv();else pauseLiveTv();
    return "applied";
  }
  if(action.type==="stop"){
    if(vod)closePlayer().catch(()=>{});else stopLiveTv().catch(()=>{});
    return "applied";
  }
  if(action.type==="seek_relative"||action.type==="seek_absolute"){
    if(!vod||PLAYER?.libraryChannel)return "unsupported";
    if(action.type==="seek_relative"&&![-30,-10,10,30].includes(action.seconds))return "invalid";
    if(action.type==="seek_absolute"&&(!Number.isSafeInteger(action.position_ms)||action.position_ms<0))return "invalid";
    const total=pbTotalSec();if(!(total>0))return "unavailable";
    const at=action.type==="seek_relative"?pbPosSec()+action.seconds:action.position_ms/1000;
    seekTo(Math.max(0,Math.min(total,at)));return "applied";
  }
  if(action.type==="open_tracks"){
    if(!["audio","subtitles","quality"].includes(action.kind))return "invalid";
    if(!vod)return "unsupported";
    const kind=action.kind==="subtitles"?"subs":action.kind,menu=document.getElementById("pmenu");
    if(!menu?.classList.contains("on")||menu.dataset.kind!==kind)toggleMenu(kind,1);
    return "applied";
  }
  if(action.type==="choose_track"){
    if(!["audio","subtitles","quality"].includes(action.kind)||typeof action.option_id!=="string"||new TextEncoder().encode(action.option_id).length>128)return "invalid";
    return cinemaRemoteChooseTrack(action.kind==="subtitles"?"subs":action.kind,action.option_id);
  }
  return "unsupported";
}
CinemaRemote.configure({refresh:cinemaRemoteBind,guard:cinemaRemoteGuard});

// Route/modal/focus/lease invalidations retire only remembered network work.
CinemaRemote.onInvalidate(()=>CinemaRemoteCancelGestures());
