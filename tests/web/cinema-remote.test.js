"use strict";
const test=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const web=path.join(__dirname,"../../crates/plurxd/src/web");
const policy=require(path.join(web,"playback-policy.js"));
function harness(route="#/"){
  const elements=new Map(),events=new Map();
  let context;
  function node(id,tag="DIV",x=0,y=0){
    const classes=new Set(),attributes=new Map(),children=[];
    const el={id,tagName:tag,isConnected:true,hidden:false,disabled:false,dataset:{},children,textContent:id,style:{},
      classList:{contains:c=>classes.has(c),add:c=>classes.add(c),remove:c=>classes.delete(c)},
      contains:other=>other===el||children.some(child=>child.contains(other)),
      closest:()=>null,hasAttribute:k=>attributes.has(k),getAttribute:k=>attributes.get(k)||null,setAttribute:(k,v)=>attributes.set(k,v),
      getClientRects:()=>el.hidden||el.style.display==="none"?[]:[{}],
      getBoundingClientRect:()=>({left:x,top:y,width:80,height:80}),
      focus:()=>{context.document.activeElement=el;},scrollIntoView:()=>{},
      querySelectorAll:selector=>children.flatMap(child=>[child,...child.querySelectorAll(selector)]).filter(child=>
        selector==="a[href]"?child.tagName==="A"&&child.hasAttribute("href"):
        selector==="[data-remote-item]"?child.dataset.remoteItem!=null:
        selector==="[data-remote-page]"?child.dataset.remotePage!=null:
        selector==="[data-remote-track]"?child.dataset.remoteTrack!=null:
        selector==="[data-remote-live-stop]"?child.dataset.remoteLiveStop!=null:false),
      querySelector:selector=>selector===".t,.eptitle"?{textContent:el.textContent}:el.querySelectorAll(selector)[0]||null};
    elements.set(id,el);return el;
  }
  const body=node("body"),app=node("app"),main=node("main"),q=node("q","INPUT",200,0),modal=node("modal"),player=node("player"),menu=node("pmenu"),host=node("live-tv-host");
  modal.style.display="none";host.hidden=true;
  app.children.push(main,q);body.children.push(app,modal,host);modal.children.push(player);player.children.push(menu);
  const document={body,activeElement:body,getElementById:id=>elements.get(id)||null,
    querySelectorAll:()=>Array.from(elements.values()).filter(el=>el.overlay),
    addEventListener:(name,fn)=>{const list=events.get(name)||[];list.push(fn);events.set(name,list);}};
  const window={getComputedStyle:el=>({display:el.style.display||"block",visibility:"visible"}),addEventListener:()=>{}};
  const calls=[];
  context=vm.createContext({document,window,crypto:require("node:crypto").webcrypto,TextEncoder,
    TOKEN:"token",ME:{id:1,is_admin:true},location:{hash:route},PLAYER:{fileId:1,sessionId:"session",_seekPending:null},
    LIVE_TV:{serial:1,pendingChannel:null},LIVE_TV_LEASE:{current:null},LIB_VIEW:null,LIB_PER:"all",LIB_PAGE_AT:0,WATCH_ITEM_PAGE:null,AUTOPLAY:null,
    PlaybackPolicy:policy,liveTvHost:()=>host,playerInputState:()=>player.classList.contains("idle")?"hidden":"transport",
    playerSeekPending:()=>context.PLAYER._seekPending!=null,
    applyPlayerOutcome:(outcome,ctx)=>{calls.push(outcome);if(outcome==="reveal")player.classList.remove("idle");if(outcome==="preview")context.PLAYER._seekPending=20;return true;},
    liveTvInputState:()=>"fullscreen_controls",liveTvApplyOutcome:outcome=>{calls.push(outcome);return true;},
    pbPosSec:()=>20,pbTotalSec:()=>100,seekTo:at=>{calls.push(["seek",at]);},setPlayerPlaying:playing=>calls.push(["playing",playing]),playerWantsPlayback:()=>true,
    toggleMenu:(kind)=>{menu.dataset.kind=kind;menu.classList.add("on");},switchAudio:id=>calls.push(["audio",id]),setSub:id=>calls.push(["subs",id]),setQuality:id=>calls.push(["quality",id]),
    toggleLiveTvPlayback:()=>{},resumeLiveTv:()=>{},pauseLiveTv:()=>calls.push("pause_live"),stopLiveTv:()=>{calls.push("stop_live");return Promise.resolve();},
    closePlayer:()=>{calls.push("stop_vod");return Promise.resolve();},
    cancelPendingSeek:()=>{calls.push("cancel_seek");context.PLAYER._seekPending=null;},cancelLiveTvChannelGesture:()=>{calls.push("cancel_channel");context.LIVE_TV.pendingChannel=null;},
    libPageCount:()=>2,libGoPage:()=>{},exactWireId:it=>it.id,playbackMetaFor:()=>({}),play:()=>{calls.push("play");return Promise.resolve();}});
  vm.runInContext(fs.readFileSync(path.join(web,"core/remote-navigation.js"),"utf8"),context);
  vm.runInContext(fs.readFileSync(path.join(web,"core/remote-router.js"),"utf8"),context);
  const remote=vm.runInContext("CinemaRemote",context);
  const item=(id,x,y)=>{const el=node("card"+id,"DIV",x,y);el.dataset.remoteItem=String(id);main.children.push(el);return el;};
  const dispatch=(action,extra={})=>remote.dispatch(action,{...remote.snapshot(),...extra});
  return {context,remote,node,app,main,q,modal,player,menu,host,item,dispatch,calls,fire:(type,event)=>{for(const fn of events.get(type)||[])fn(event);}};
}
test("semantic navigation reaches header search and library without activating arbitrary controls",()=>{
  const h=harness(),link=h.node("library","A",0,0);link.setAttribute("href","#/library/2");h.app.children.push(link);
  const admin=h.node("admin","A");admin.setAttribute("href","#/settings/users");h.app.children.push(admin);
  h.remote.focusById("route:#/library/2:0");assert.equal(h.dispatch({type:"select"}),"applied");assert.equal(h.context.location.hash,"#/library/2");
  assert.equal(h.remote.focusById("search"),"applied");assert.equal(h.context.document.activeElement,h.q);
  assert.equal(h.remote.focusById("route:#/settings/users:0"),"unavailable");
});
test("locally opened admin modal rejects select home back and publishes no label",()=>{
  const h=harness();h.item(1,0,100);h.remote.focusById("item:1");const state=h.remote.snapshot();
  const modal=h.node("editwrap");modal.overlay=true;
  for(const type of ["select","home","back"])assert.equal(h.remote.dispatch({type},state),"restricted_surface");
  assert.equal(h.remote.snapshot().focused_label,null);assert.equal(h.context.location.hash,"#/");
});
test("physical focus change fences an old Select",()=>{
  const h=harness();const one=h.item(1,0,100),two=h.item(2,100,100);h.remote.focusById("item:1");const old=h.remote.snapshot();two.focus();
  assert.equal(h.remote.dispatch({type:"select"},old),"stale_focus");assert.equal(h.context.location.hash,"#/");assert.notEqual(one,h.context.document.activeElement);
});
test("rerendered card cannot receive an old registered Select",()=>{
  const h=harness();const oldNode=h.item(1,0,100);h.remote.focusById("item:1");const state=h.remote.snapshot();oldNode.isConnected=false;h.main.children=[];h.item(1,0,100);
  assert.equal(h.remote.dispatch({type:"select"},state),"stale_context");assert.equal(h.context.location.hash,"#/");
});
test("library Back restores the stable item without a raw browser Back",()=>{
  const h=harness("#/library/2");h.item(7,0,100);h.remote.focusById("item:7");assert.equal(h.dispatch({type:"select"}),"applied");assert.equal(h.context.location.hash,"#/item/7");
  assert.equal(h.dispatch({type:"back"}),"applied");assert.equal(h.context.location.hash,"#/library/2");
  const pending=vm.runInContext("CINEMA_REMOTE_RETURN",h.context);assert.equal(pending.id,"item:7");assert.equal(h.remote.focusById(pending.id),"applied");
});
test("trusted physical arrows preserve physical scrub but cancel network-owned scrub",()=>{
  const h=harness();h.context.PLAYER._seekPending=10;
  h.fire("keydown",{isTrusted:true,key:"ArrowRight"});assert.equal(h.context.PLAYER._seekPending,10);
  h.context.PLAYER._seekPending+=10;h.fire("keydown",{isTrusted:true,key:"ArrowRight"});assert.equal(h.context.PLAYER._seekPending,20);
  vm.runInContext('CinemaRemoteRememberGesture({source:"network"})',h.context);
  h.fire("keydown",{isTrusted:true,key:"ArrowLeft"});assert.equal(h.context.PLAYER._seekPending,null);assert.deepEqual(h.calls,["cancel_seek"]);
});
test("network gesture cancellation cannot erase a newer physical pending scrub",()=>{
  const h=harness();h.context.PLAYER._seekPending=10;vm.runInContext('CinemaRemoteRememberGesture({source:"network"})',h.context);
  h.context.PLAYER._seekPending=30;h.remote.physicalInput();assert.equal(h.context.PLAYER._seekPending,30);
});
test("CEC hidden horizontal input reveals instead of using desktop seek policy",()=>{
  const h=harness();h.modal.classList.add("open");h.modal.style.display="block";h.player.classList.add("idle");
  assert.equal(h.dispatch({type:"navigate",direction:"left"},{source:"local_cec"}),"applied");assert.deepEqual(h.calls,["reveal"]);
  assert.equal(policy.routeInput("desktop","hidden","left"),"skip");
});
test("typed pause and stop enter the current owner without an API round trip",()=>{
  const h=harness();h.modal.classList.add("open");h.modal.style.display="block";
  assert.equal(h.dispatch({type:"set_playing",playing:false}),"applied");assert.equal(h.dispatch({type:"stop"}),"applied");
  assert.deepEqual(h.calls,[["playing",false],"stop_vod"]);
  h.modal.classList.remove("open");h.modal.style.display="none";h.host.hidden=false;h.context.LIVE_TV_LEASE.current={};
  assert.equal(h.dispatch({type:"set_playing",playing:false}),"applied");assert.equal(h.dispatch({type:"stop"}),"applied");
  assert.deepEqual(h.calls.slice(2),["pause_live","stop_live"]);
});
test("prototype action names extra fields and malformed fields reject without throwing",()=>{
  const h=harness();for(const type of ["constructor","__proto__","toString"])assert.equal(h.dispatch({type}),"unsupported");
  assert.equal(h.dispatch({type:"navigate",direction:"left",selector:"#admin"}),"invalid");assert.equal(h.dispatch({type:"navigate",direction:"upward"}),"invalid");assert.equal(h.dispatch({type:{toString:{}}}),"invalid");
});
test("search text requires current nonce and byte bound",()=>{
  const h=harness(),state=h.remote.snapshot();assert.equal(h.dispatch({type:"text_replace",text_nonce:"old",text:"Film"}),"stale_context");
  assert.equal(h.dispatch({type:"text_replace",text_nonce:state.text_nonce,text:"😀".repeat(129)}),"invalid");
  assert.equal(h.dispatch({type:"text_replace",text_nonce:state.text_nonce,text:"Film"}),"applied");assert.equal(h.context.location.hash,"#/search/Film");
});
test("play_item uses the existing authorized autoplay detail path",()=>{
  const h=harness();assert.equal(h.dispatch({type:"play_item",item_id:12}),"applied");assert.equal(h.context.AUTOPLAY,"12");assert.equal(h.context.location.hash,"#/item/12");
});
// Exercise the actual changed owner function, not a mock transport adapter.
function shipped(name,file){const source=fs.readFileSync(path.join(web,file),"utf8"),start=source.indexOf("function "+name+"(");assert.ok(start>=0);const tail=source.slice(source.slice(Math.max(0,start-6),start)==="async "?start-6:start);const end=tail.slice(1).search(/\n(?:async function |function |const |let |window\.|document\.)/);return end<0?tail:tail.slice(0,end+1);}
test("desired playing is idempotent and respects pending transport intent",()=>{
  let wants=true,toggles=0;const v={};const fn=new Function("document","playerWantsPlayback","togglePlay",shipped("setPlayerPlaying","player/transport.js")+";return setPlayerPlaying;")({getElementById:()=>v},()=>wants,()=>{toggles++;wants=!wants;});
  fn(false);fn(false);assert.equal(toggles,1);fn(true);fn(true);assert.equal(toggles,2);
});
test("Live TV Stop pauses immediately and preserves fullscreen teardown while server is offline",async()=>{
  const calls=[],live={serial:1,pendingChannel:null},host={dataset:{mode:"full"},hidden:false};
  const video={pause:()=>calls.push("pause")};
  let exit;const pending=new Promise(resolve=>{exit=resolve;});
  const stop=new Function("cancelLiveTvChannelGesture","LIVE_TV","liveTvPlaybackState","document","location","exitLiveTvPresentation","detachLiveTvMedia","LIVE_TV_LEASE",shipped("stopLiveTv","pages/live-tv-controls.js")+";return stopLiveTv;")(()=>{},live,()=>{}, {getElementById:id=>id==="live-tv-video"?video:host},{hash:"#/live-tv"},()=>pending,()=>calls.push("detach"),{stop:()=>Promise.reject(new Error("offline"))});
  const stopped=stop();assert.deepEqual(calls,["pause"]);assert.equal(host.hidden,false);exit();await assert.rejects(stopped,/offline/);assert.deepEqual(calls,["pause","detach"]);
});
test("actual VOD desired pause remains local while progress reporting cannot reach the API",()=>{
  const h=harness(),video=h.node("video","VIDEO");video.paused=false;video.ended=false;video.pause=()=>{video.paused=true;};
  Object.assign(h.context,{endWait:()=>{},supersedePlaybackControlIntent:()=>{},queuePlaybackTransportCommand:()=>{},
    PLAY_OPEN_GATE:{current:()=>false},play:()=>{},playerActivity:()=>{},reportProgress:()=>new Promise(()=>{}),notifyPlaybackControl:()=>{},
    pauseHlsStartup:()=>{},resumeHlsStartup:()=>{},playbackTransportMarker:()=>({}),PlaybackPolicy:policy});
  for(const name of ["playbackTransportEvents","pausePlaybackInternally","rememberPlaybackTransportIntent","applyPlaybackTransportIntent"])
    vm.runInContext(shipped(name,"player/decode-margin.js"),h.context);
  for(const name of ["playerWantsPlayback","togglePlay","setPlayerPlaying"])vm.runInContext(shipped(name,"player/transport.js"),h.context);
  h.context.setPlayerPlaying(false);assert.equal(video.paused,true);assert.equal(h.context.PLAYER.wantsPlayback,false);
  h.context.setPlayerPlaying(false);assert.equal(video.paused,true);
});
test("actual VOD close pauses and removes local source before pending progress or release completes",()=>{
  const h=harness(),video=h.node("video","VIDEO");video.paused=false;video.pause=()=>{video.paused=true;};video.removeAttribute=()=>{video.sourceRemoved=true;};video.load=()=>{};
  h.modal.classList.add("open");
  const noop=()=>{},play=()=>{};
  Object.assign(h.context,{WATCH_CLOSE_PROMISE:null,WATCH_GENERATION:0,WATCH:null,watchDetach:()=>null,PLAY_OPEN_GATE:{invalidate:noop},play,
    reportProgress:()=>new Promise(()=>{}),beginPlaybackPreparation:{},retirePlaybackPredecessor:noop,exitPresentationModes:noop,
    clearPlayerMediaSession:noop,supersedePlaybackControlIntent:noop,teardownHls:noop,releaseSession:()=>new Promise(()=>{}),
    finishPlaybackSeekTelemetry:noop,stopPlayerTimers:noop,clearInterval:noop,STATS_TIMER:null,resetPlaybackSurface:noop,
    cancelHlsStartup:noop,playbackTransportMarker:()=>({})});
  for(const name of ["playbackTransportEvents","pausePlaybackInternally","rememberPlaybackTransportIntent","resetPlaybackTransportEvents","resetMediaSource"])
    vm.runInContext(shipped(name,"player/decode-margin.js"),h.context);
  vm.runInContext(shipped("closePlayer","player/stats.js"),h.context);
  h.context.closePlayer();assert.equal(video.paused,true);assert.equal(video.sourceRemoved,true);assert.equal(h.modal.classList.contains("open"),false);
});
test("network pause cannot acquire a physical pending scrub",()=>{
  const h=harness();h.modal.classList.add("open");h.modal.style.display="block";h.context.PLAYER._seekPending=20;
  h.dispatch({type:"set_playing",playing:false},{source:"network"});h.remote.physicalInput();assert.equal(h.context.PLAYER._seekPending,20);
});

test("search contexts work on HTTP LAN without randomUUID secure-context API",()=>{
  const h=harness();h.context.crypto={getRandomValues:require("node:crypto").webcrypto.getRandomValues.bind(require("node:crypto").webcrypto)};
  assert.match(h.remote.snapshot().text_nonce,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
});
test("repeated title rails retain stable revisions",()=>{
  const h=harness();h.item(2,0,100);h.item(2,100,100);const first=h.remote.snapshot();const second=h.remote.snapshot();assert.equal(second.context_revision,first.context_revision);
});

test("focused labels respect UTF-8 bounds without truncating a code point",()=>{
  const h=harness(),card=h.item(2,0,100);card.textContent="😀".repeat(100);h.remote.focusById("item:2");const label=h.remote.snapshot().focused_label;
  assert.equal(new TextEncoder().encode(label).length,256);assert.equal(label,"😀".repeat(64));
});
test("empty network gesture markers never cancel physical work",()=>{
  const h=harness();vm.runInContext('CinemaRemoteRememberGesture({source:"network"})',h.context);h.remote.physicalInput();assert.deepEqual(h.calls,[]);
});

test("physical focus and takeover invalidate network scrubs without touching physical scrubs",()=>{
  const h=harness();h.context.PLAYER._seekPending=10;vm.runInContext('CinemaRemoteRememberGesture({source:"network"})',h.context);
  h.fire("focusin",{});assert.equal(h.context.PLAYER._seekPending,null);
  h.context.PLAYER._seekPending=20;vm.runInContext('CinemaRemoteRememberGesture({source:"network"})',h.context);
  h.remote.invalidate("takeover");assert.equal(h.context.PLAYER._seekPending,null);
  h.context.PLAYER._seekPending=30;h.remote.invalidate("focus");assert.equal(h.context.PLAYER._seekPending,30);
});
