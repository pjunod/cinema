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
        selector==="[data-remote-live-stop]"?child.dataset.remoteLiveStop!=null:
        selector==="[data-remote-start-over]"?child.dataset.remoteStartOver!=null:
        selector==="[data-remote-file]"?child.dataset.remoteFile!=null:false),
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
function vodPageHideHarness(){
  const h=harness(),events=new Map(),video=h.node("video","VIDEO"),calls=[];
  video.paused=false;video.pause=()=>{video.paused=true;};
  video.removeAttribute=()=>{video.sourceRemoved=true;};video.load=()=>{};
  h.modal.classList.add("open");
  const noop=()=>{},play=()=>{};
  Object.assign(h.context,{WATCH_CLOSE_PROMISE:null,WATCH_GENERATION:0,WATCH:null,
    watchDetach:()=>null,PLAY_OPEN_GATE:{invalidate:()=>calls.push("invalidate")},play,
    reportProgress:()=>new Promise(()=>{}),beginPlaybackPreparation:{},
    exitPresentationModes:noop,clearPlayerMediaSession:noop,supersedePlaybackControlIntent:noop,
    finishPlaybackSeekTelemetry:noop,stopPlayerTimers:noop,clearInterval:noop,
    STATS_TIMER:null,resetPlaybackSurface:noop,cancelPendingSeek:noop,
    PLAYER_WIRED:false,hasPendingPlaybackOpen:()=>false,pbWireSeek:noop,wirePlayerMedia:noop,notifyPlaybackControl:noop,
    cancelHlsStartup:noop,playbackTransportMarker:()=>({}),
    playbackSurfaceStep:noop,playbackSurfaceGeneration:()=>({}),
    supersedeDirectedChange:noop,cancelPreparedFirstFrame:noop,
    abandonPreparedReplacement:p=>{
      if(p?.prepared){calls.push("destroy:"+p.prepared.sessionId);p.prepared=null;}
    },
    API:"/api/v1",fetch:(url,options)=>{
      calls.push("release:"+url.split("/").pop());
      assert.equal(options.method,"DELETE");assert.equal(options.keepalive,true);
      assert.equal(options.headers.authorization,"Bearer token");
      return new Promise(()=>{});
    }});
  h.context.window.addEventListener=(event,fn)=>events.set(event,fn);
  for(const name of ["playbackTransportEvents","pausePlaybackInternally","rememberPlaybackTransportIntent",
    "resetPlaybackTransportEvents","resetMediaSource","destroyHlsInstance","teardownHls","retirePlaybackPredecessor"])
    vm.runInContext(shipped(name,"player/decode-margin.js"),h.context);
  vm.runInContext(shipped("releaseSession","player/directed-change.js"),h.context);
  vm.runInContext(shipped("closePlayer","player/stats.js"),h.context);
  vm.runInContext(shipped("closeVodForPageHide","player/transport.js"),h.context);
  vm.runInContext(shipped("wirePlayer","player/transport.js"),h.context);
  h.context.wirePlayer();
  function session(id){return {fileId:1,sessionId:id,hls:{destroy:()=>calls.push("destroy:"+id)}};}
  return {...h,video,calls,session,hide:persisted=>events.get("pagehide")({persisted})};
}
test("VOD pagehide destroys and releases current outgoing and staged sessions without awaiting progress",()=>{
  const h=vodPageHideHarness(),p=h.session("current");
  p.mediaPredecessor=h.session("outgoing");
  p.prepared={sessionId:"staged"};
  h.context.PLAYER=p;
  h.hide(false);
  for(const id of ["current","outgoing","staged"]){
    assert.equal(h.calls.filter(call=>call==="release:"+id).length,1);
    assert.ok(h.calls.indexOf("destroy:"+id)<h.calls.indexOf("release:"+id));
  }
  assert.equal(p.sessionId,null);assert.equal(p.mediaPredecessor,null);
  assert.equal(h.video.sourceRemoved,true);assert.equal(h.modal.classList.contains("open"),false);
  const count=h.calls.length;h.hide(false);assert.equal(h.calls.length,count);
});
test("a bfcache VOD pagehide restores a closed player instead of a retired session",()=>{
  const h=vodPageHideHarness(),p=h.session("cached");h.context.PLAYER=p;
  h.hide(true);
  assert.equal(p.sessionId,null);assert.equal(p.hls,null);assert.equal(p.wantsPlayback,false);
  assert.equal(h.video.sourceRemoved,true);assert.equal(h.modal.classList.contains("open"),false);
  assert.ok(h.calls.includes("release:cached"));
});
test("VOD pagehide releases both committing sessions when cancellation rolls back before Close captures its handle",()=>{
  const h=vodPageHideHarness(),p=h.session("successor"),predecessor=h.session("predecessor");
  const retired=h.node("video-retired","VIDEO");
  retired.pause=()=>{};retired.play=()=>Promise.resolve();retired.removeAttribute=()=>{};retired.load=()=>{};
  p.preparedCommitting={sessionId:"successor",hls:p.hls,
    predecessor:{sessionId:"predecessor",hls:predecessor.hls,element:retired}};
  h.context.PLAYER=p;
  const noop=()=>{},getElement=h.context.document.getElementById;
  Object.assign(h.context,{clearEffectiveProcessing:noop,resumePreparedIncumbentLoad:noop,
    restorePreparedOverlap:noop,queuePlaybackControlAcknowledgement:noop,
    disposeRetiredMediaElement:noop,renderPlayerInfo:noop,
    adoptPlaybackMediaElement:()=>{
      h.context.document.getElementById=id=>id==="video"?retired:getElement(id);
    }});
  for(const name of ["rollbackPreparedReplacement","cancelPreparedFirstFrame"])
    vm.runInContext(shipped(name,"player/prepared-switch-measurement.js"),h.context);
  // A preparation's synchronous cancellation may restore the predecessor
  // before Close snapshots sessionId. Exercise the shipped rollback, not a
  // stub that assumes which of the two handles Close will release.
  h.context.beginPlaybackPreparation.active={cancel:()=>h.context.cancelPreparedFirstFrame(p),finish:noop};
  h.hide(false);
  for(const id of ["successor","predecessor"]){
    assert.ok(h.calls.includes("release:"+id));
    assert.ok(h.calls.indexOf("destroy:"+id)<h.calls.indexOf("release:"+id));
  }
  // The captured preparation is bounded to two handles; its predecessor can
  // already have been released by Close, so a second idempotent DELETE is OK.
  assert.equal(h.calls.filter(call=>call==="release:predecessor").length,2);
  assert.equal(p.sessionId,null);assert.equal(p.hls,null);assert.equal(p.preparedCommitting,null);
});
test("VOD pagehide cancels an initial open before its session returns",()=>{
  const h=vodPageHideHarness();h.context.PLAYER=null;h.modal.classList.remove("open");
  let cancelled=0,finished=0;
  h.context.beginPlaybackPreparation.active={cancel:()=>cancelled++,finish:()=>finished++};
  h.context.play.pendingIntent={};h.hide(false);
  assert.equal(cancelled,1);assert.equal(finished,1);
  assert.ok(h.calls.includes("invalidate"));assert.equal(h.context.play.pendingIntent,null);
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

test("start over and version controls use exact current files and preserve resume semantics",()=>{
  const h=harness("#/item/7"),first={id:11,available:true,duration_ms:90000},second={id:12,available:true,duration_ms:120000};
  const page={id:"7",item:{id:"7",title:"Title"},shape:"versions",playable:first,files:[first,second],resume:15000,playStart:15000};h.context.WATCH_ITEM_PAGE=page;
  const start=h.node("start","BUTTON"),version=h.node("version","BUTTON",100,0);start.dataset.remoteStartOver="";version.dataset.remoteFile="12";version.dataset.remoteFileStart="resume";h.main.children.push(start,version);
  const plays=[];h.context.play=(...args)=>{plays.push(args);return Promise.resolve();};
  assert.equal(h.remote.focusById("start-over:7"),"applied");assert.equal(h.dispatch({type:"select"}),"applied");assert.equal(plays[0][0],11);assert.equal(plays[0][2],0);
  assert.equal(h.remote.focusById("version:7:12"),"applied");assert.equal(h.dispatch({type:"select"}),"applied");assert.equal(plays[1][0],12);assert.equal(plays[1][2],15000);
  second.available=false;assert.equal(h.dispatch({type:"select"}),"stale_context");assert.equal(plays.length,2);
});
test("explicit allowed roots retain watch controls without authorizing unrelated DOM",()=>{
  const h=harness(),allowed=h.node("owned"),extra=h.node("extra","BUTTON"),foreign=h.node("foreign","BUTTON");allowed.children.push(extra);h.context.document.body.children.push(allowed,foreign);
  h.remote.configure({refresh:()=>{},guard:()=>true});h.remote.registerScope({id:"owned",root:h.main,roots:[allowed]});
  h.remote.registerAction({id:"extra",element:extra,label:"Owned",activate:()=>"applied"});h.remote.registerAction({id:"foreign",element:foreign,label:"Foreign",activate:()=>{throw Error("foreign activated");}});
  assert.equal(h.remote.focusById("extra"),"applied");assert.equal(h.dispatch({type:"select"}),"applied");assert.equal(h.remote.focusById("foreign"),"unavailable");
});

test("local Pair entry is excluded from network navigation labels and source spoofing",()=>{
  const h=harness(),local=h.node("pair","BUTTON");h.main.children.push(local);let activated=0;
  h.remote.configure({refresh:()=>{},guard:()=>true});h.remote.registerScope({id:"home",root:h.main});h.remote.registerAction({id:"pair",element:local,label:"Pair a phone",localOnly:true,activate:()=>{activated++;return "applied";}});
  assert.equal(h.dispatch({type:"navigate",direction:"down"}),"unavailable");h.remote.focusById("pair");assert.equal(h.remote.snapshot().focused_id,null);assert.equal(h.remote.snapshot().focused_label,null);
  assert.equal(h.dispatch({type:"select"},{source:"local_cec"}),"restricted_surface");assert.equal(activated,0);
  assert.equal(h.remote.dispatchPhysical({type:"select"},h.remote.snapshotPhysical()),"applied");assert.equal(activated,1);
});

function playbackFailureLogHarness(){
  const reports=[],requests=[],diagnostics=[];
  const player={sessionId:"outgoing",attemptId:"a7",fileId:7,title:"Original",
    method:"remux",source:{video_codec:"hevc"},deliveredRange:"hdr10"};
  const scope=vm.createContext({PLAYER:player,TOKEN:"secret-token",API:"/api/v1",
    AUTH_GENERATION:1,ME:{id:1},window:{CinemaDiagnostics:{owns:()=>true}},
    observeDiagnosticRequest:(...args)=>diagnostics.push(args),browserLabel:()=>"Chrome",refreshClientErrorReporterAuth:()=>{},
    performance:{now:()=>0},clearTimeout:()=>{},
    document:{getElementById:()=>({currentTime:14})},
    positionForPlaybackIntent:()=>14,beginPlaybackControlSeek:()=>{},
    playbackContext:()=>({session:scope.PLAYER.sessionId,attempt:scope.PLAYER.attemptId}),
    fetch:(url,options)=>{
      if(url.endsWith("/client-log")){reports.push(JSON.parse(options.body));return Promise.resolve({ok:true,status:204});}
      requests.push({url,options});return scope.releaseFetch();
    }});
  vm.runInContext(shipped("clientLog","core/api.js"),scope);
  for(const name of ["releaseSession","fallBackDirectedChange"])
    vm.runInContext(shipped(name,"player/directed-change.js"),scope);
  function replace(){scope.PLAYER={sessionId:"incoming",attemptId:"a99",fileId:99,title:"Incoming",method:"transcode",
    controlReporter:{legacyContext:()=>({generation:"unrelated"})}};}
  async function drain(){for(let i=0;i<5;i++)await Promise.resolve();}
  return {scope,player,reports,requests,diagnostics,replace,drain};
}
test("failed session releases report HTTP rejection and immutable outgoing identity through clientLog",async()=>{
  const h=playbackFailureLogHarness();let finish;
  h.scope.releaseFetch=()=>new Promise(resolve=>{finish=resolve;});
  h.scope.releaseSession("outgoing");h.replace();h.player.attemptId="changed";
  finish({ok:false,status:503});await h.drain();
  assert.equal(h.reports.length,1);
  const report=h.reports[0];
  assert.equal(report.event,"session_release_failed");assert.equal(report.level,"error");
  assert.equal(report.session_id,"outgoing");assert.equal(report.attempt,"a7");
  assert.equal(report.file_id,7);assert.equal(report.title,"Original");
  assert.equal(report.code,503);assert.equal(report.detail,"http=503");
  assert.deepEqual(report.control,{});assert.equal(h.requests[0].options.keepalive,true);
  assert.equal(h.diagnostics.length,0,"the outgoing failure marked the incoming attachment failed");
  h.scope.clientLog({event:"playback_failed",level:"error",message:"current failure",session_id:"incoming"});
  assert.equal(h.diagnostics.length,1,"the current attachment's own failure was suppressed");
  h.scope.clientLog({event:"playback_failed",level:"error",message:"current failure without handle"});
  assert.equal(h.diagnostics.length,2,"legacy current-owner diagnostics were suppressed");
});
test("session release exceptions and promise rejections are bounded without copying URLs or tokens",async()=>{
  for(const synchronous of [false,true]){
    const h=playbackFailureLogHarness();
    const error=Object.assign(new TypeError("https://host/hls/private?token=secret-token"),{code:"NETWORK_FAILED"});
    h.scope.releaseFetch=()=>{if(synchronous)throw error;return Promise.reject(error);};
    assert.doesNotThrow(()=>h.scope.releaseSession("outgoing"));await h.drain();
    assert.equal(h.reports.length,1);assert.equal(h.reports[0].detail,"cause=TypeError:NETWORK_FAILED");
    assert.ok(!JSON.stringify(h.reports).includes("https://"));assert.ok(!JSON.stringify(h.reports).includes("secret-token"));
  }
});
test("an unmatched outgoing release never acquires the new player's identity and successful releases stay quiet",async()=>{
  const h=playbackFailureLogHarness();h.replace();
  h.scope.releaseFetch=()=>Promise.resolve({ok:false,status:500});
  h.scope.releaseSession("outgoing");await h.drain();
  assert.equal(h.reports[0].session_id,"outgoing");
  for(const key of ["attempt","file_id","title","method","vcodec"])assert.equal(h.reports[0][key],null,key);
  h.scope.releaseFetch=()=>Promise.resolve({ok:true,status:204});
  h.scope.releaseSession("outgoing");await h.drain();assert.equal(h.reports.length,1);
});
test("failed quality recovery preserves its original fault and logs the recovery failure against its original owner",async()=>{
  for(const mode of ["sync","promise","default"]){
    const h=playbackFailureLogHarness();
    const failure=()=>{h.replace();throw Object.assign(new TypeError("https://host/private?token=secret-token"),{code:"OPEN_FAILED"});};
    const change={reason:"manual",intentGeneration:0,fallback:mode==="sync"?failure:
      mode==="promise"?()=>Promise.resolve().then(failure):null};
    h.player.directedChange=change;
    h.scope.play=async()=>failure();
    assert.equal(h.scope.fallBackDirectedChange(h.player,change,"no_first_frame"),true);
    await h.drain();
    assert.deepEqual(h.reports.map(report=>report.event),["quality_switch","quality_fallback_failed"]);
    assert.equal(h.reports[0].level,"warn");assert.match(h.reports[0].detail,/no_first_frame/);
    const report=h.reports[1];
    assert.equal(report.level,"error");assert.equal(report.session_id,"outgoing");
    assert.equal(report.attempt,"a7");assert.equal(report.file_id,7);assert.equal(report.delivered_range,"hdr10");
    assert.equal(report.detail,"trigger=no_first_frame;cause=TypeError:OPEN_FAILED");
    assert.ok(!JSON.stringify(h.reports).includes("https://"));assert.ok(!JSON.stringify(h.reports).includes("secret-token"));
  }
});
test("successful quality recovery retains its recoverable fault without adding success or failure noise",async()=>{
  const h=playbackFailureLogHarness();let recovered=0;
  const change={reason:"manual",intentGeneration:0,fallback:async()=>{h.replace();recovered++;}};
  h.player.directedChange=change;
  assert.equal(h.scope.fallBackDirectedChange(h.player,change,"no_first_frame"),true);await h.drain();
  assert.equal(recovered,1);assert.deepEqual(h.reports.map(report=>report.event),["quality_switch"]);
  assert.match(h.reports[0].detail,/no_first_frame/);
});
test("failed quality recovery preserves typed API status and refusal code without copying its response message",async()=>{
  for(const error of [Object.assign(new Error("secret-token https://host/private"),{status:503,code:"insufficient_capacity"}),
    Object.assign(new Error("secret-token https://host/private"),{status_code:409,streamFailure:{code:"source_changed"}})]){
    const h=playbackFailureLogHarness();
    const change={reason:"manual",intentGeneration:0,fallback:async()=>{throw error;}};
    h.player.directedChange=change;
    h.scope.fallBackDirectedChange(h.player,change,"no_first_frame");await h.drain();
    const report=h.reports[1];assert.equal(report.event,"quality_fallback_failed");
    assert.equal(report.code,error.status||error.status_code);
    assert.match(report.detail,error.code?/cause=Error:insufficient_capacity;http=503/:/cause=Error:source_changed;http=409/);
    assert.ok(!JSON.stringify(report).includes("https://"));assert.ok(!JSON.stringify(report).includes("secret-token"));
  }
});
test("a fallback AbortError remains visible after replacement without proof of an owned cancellation",async()=>{
  const h=playbackFailureLogHarness();
  const change={reason:"manual",intentGeneration:0,fallback:async()=>{h.replace();throw Object.assign(new Error("cancelled"),{name:"AbortError"});}};
  h.player.directedChange=change;
  h.scope.fallBackDirectedChange(h.player,change,"no_first_frame");await h.drain();
  assert.deepEqual(h.reports.map(report=>report.event),["quality_switch","quality_fallback_failed"]);
  assert.match(h.reports[1].detail,/cause=AbortError/);
});
test("a sessionless outgoing fallback failure stays logged without marking the incoming diagnostic attachment",async()=>{
  const h=playbackFailureLogHarness();h.player.sessionId=null;h.player.method="remux";
  const change={reason:"manual",intentGeneration:0,fallback:async()=>{h.replace();throw new TypeError("recovery failed");}};
  h.player.directedChange=change;
  h.scope.fallBackDirectedChange(h.player,change,"no_first_frame");await h.drain();
  const report=h.reports.find(report=>report.event==="quality_fallback_failed");
  assert.ok(report);assert.equal(report.session_id,null);assert.equal(report.attempt,"a7");assert.equal(report.file_id,7);
  assert.equal(h.diagnostics.filter(([,kind])=>kind==="error").length,0,
    "an explicitly unknown outgoing handle acquired incoming ownership");
});
test("current direct and progressive playback context and surface reports retain local diagnostic ownership",()=>{
  for(const method of ["direct_play","remux"]){
    const h=playbackFailureLogHarness();h.player.sessionId=null;h.player.method=method;
    h.player.mediaAttachment={id:3};
    h.scope.clientLog({event:"playback_failed",level:"error",message:"current failure",
      session:null,attempt:"a7",file_id:7,attachment_id:3});
    h.scope.clientLog({event:"surface_error",level:"error",message:"current surface failure",
      session_id:null,attempt:"a7"});
    assert.equal(h.diagnostics.length,2,method+" current faults lost diagnostic ownership");
    assert.equal(h.reports.length,2);
    h.scope.clientLog({event:"playback_failed",level:"error",message:"old attachment failure",
      session:null,attempt:"a7",file_id:7,attachment_id:2});
    assert.equal(h.diagnostics.length,2,"an old attachment marked its replacement failed");
    assert.equal(h.reports.length,3,"an unowned local diagnostic dropped the server event");
  }
});
