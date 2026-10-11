"use strict";
function cinemaRemotePreferences(identity=cinemaRemoteIdentity()){
  if(!identity)return {cec:false,receiver:false,companion:false,suggestions:true};
  try{const saved=CinemaRemoteWire.parse(localStorage.getItem("cinema.remote.preferences:"+cinemaRemoteIdentityKey(identity))||"{}");
    return {cec:saved.cec===true,receiver:saved.receiver===true,companion:saved.companion===true,suggestions:saved.suggestions!==false};
  }catch(_){return {cec:false,receiver:false,companion:false,suggestions:true};}
}
function cinemaRemoteLocalEnabled(kind){return cinemaRemotePreferences()[kind]===true;}
function cinemaRemotePlaybackSummary(){
  const milliseconds=value=>Number.isFinite(value)&&value>=0?Math.min(Number.MAX_SAFE_INTEGER,Math.floor(value)):0;
  if(cinemaRemoteVod()){
    const id=Number(PLAYER?.meta?.item_id);if(!CinemaRemoteWire.positive(id)||String(id)!==String(PLAYER.meta.item_id))return null;
    const tracks=[];
    (PLAYER.audio||[]).forEach((value,index)=>tracks.push({kind:"audio",option_id:String(index),label:cinemaRemoteText(audioLabelMenu(value,index),256)}));
    if(PLAYER.subs?.length){tracks.push({kind:"subtitles",option_id:"-1",label:"Off"});for(const value of PLAYER.subs)tracks.push({kind:"subtitles",option_id:String(value.index),label:cinemaRemoteText(subLabelMenu(value),256)});}
    for(const [id,label] of qualityOptions())tracks.push({kind:"quality",option_id:String(id),label:cinemaRemoteText(label,256)});
    return {media:{type:"item",item_id:id},title:cinemaRemoteText(PLAYER.title,256).trim()||"Playing item",playing:playerWantsPlayback(document.getElementById("video")),position_ms:milliseconds(pbPosSec()*1000),duration_ms:milliseconds(pbTotalSec()*1000),tracks:tracks.slice(0,64)};
  }
  // A playable detail page describes the actual item and saved resume point;
  // it has no running playback owner, so desired playing is false.
  if(location.hash.startsWith("#/item/")&&WATCH_ITEM_PAGE?.playable&&!["book","photo"].includes(WATCH_ITEM_PAGE.shape)){
    const page=WATCH_ITEM_PAGE,id=Number(page.id);if(CinemaRemoteWire.positive(id)&&String(id)===page.id)return {media:{type:"item",item_id:id},title:cinemaRemoteText(page.item.title,256).trim()||"Selected item",playing:false,position_ms:milliseconds(page.resume),duration_ms:milliseconds(page.runtime),tracks:[]};
  }
  if(cinemaRemoteLive()){
    const channel=LIVE_TV.status?.channel||LIVE_TV.channels.find(value=>value.id===LIVE_TV.selected),id=channel?.id;
    if(id==null||CinemaRemoteWire.bytes(String(id))>128||!String(id))return null;
    const video=/** @type {HTMLVideoElement|null} */ (document.getElementById("live-tv-video"));
    return {media:{type:"live_channel",channel_id:String(id)},title:cinemaRemoteText(channel.name||channel.title||"Live TV",256),playing:!!video&&!video.paused,position_ms:milliseconds((video?.currentTime||0)*1000),duration_ms:0,tracks:[]};
  }
  return null;
}
function cinemaRemoteSafeState(){
  const snapshot=CinemaRemote.snapshot();
  const routes={home:"home",library:"library",category:"library",item:"details",search:"search",player:"playback","live-tv":"playback"};
  const route=snapshot.route_category.startsWith("tracks:")?"tracks":routes[snapshot.route_category];
  if(snapshot.blocked||!route)return {...snapshot,route:"restricted",capabilities:[],focused_label:null,text_nonce:null,playback:null};
  const capabilities=["navigate","back","home"];
  if(snapshot.focused_id)capabilities.push("select");
  if(snapshot.text_nonce)capabilities.push("text_replace");
  if(route==="details"&&WATCH_ITEM_PAGE?.playable&&!["book","photo"].includes(WATCH_ITEM_PAGE.shape))capabilities.push("play_item");
  if(cinemaRemoteVod()||cinemaRemoteLive())capabilities.push("set_playing","stop");
  if(cinemaRemoteVod()){
    capabilities.push("open_tracks");if(route==="tracks")capabilities.push("choose_track");
    if(!PLAYER?.libraryChannel&&pbTotalSec()>0)capabilities.push("seek_relative","seek_absolute");
  }
  return {...snapshot,route,capabilities:capabilities.slice(0,12),focused_label:snapshot.focused_id?(cinemaRemoteText(snapshot.focused_label,256).trim()||null):null,playback:cinemaRemotePlaybackSummary()};
}
function cinemaRemoteBoundPublishedState(state){
  const bounded={...state,playback:state.playback?{...state.playback,tracks:state.playback.tracks.slice(0,64)}:null};
  // Escaped JSON can be six bytes per source character. Keep an exact prefix
  // of real offered options; never truncate or synthesize an option identifier.
  // Reserve512 bytes below the server's normalized48KiB state budget.
  while(CinemaRemoteWire.bytes(JSON.stringify(bounded))>48640&&bounded.playback?.tracks.length)bounded.playback.tracks.pop();
  if(CinemaRemoteWire.bytes(JSON.stringify(bounded))>48640){bounded.playback=null;bounded.focused_label=null;}
  return bounded;
}
function cinemaRemoteSemanticCheck(action,state){
  if(state.route==="restricted"||state.blocked)return "restricted_surface";
  if(!state.capabilities.includes(action.type))return "unsupported";
  if(action.type==="choose_track"){
    const menu=document.getElementById("pmenu"),kind=action.kind==="subtitles"?"subs":action.kind;
    if(!menu?.classList.contains("on")||menu.dataset.kind!==kind||!Array.from(/** @type {NodeListOf<HTMLElement>} */ (menu.querySelectorAll("[data-remote-track]"))).some(value=>value.dataset.remoteTrack===action.option_id))return "stale_context";
  }
  return null;
}
class CinemaWebReceiver{
  constructor({identityReader=cinemaRemoteIdentity,clientFactory=identity=>new CinemaRemoteClient(identity),locks=typeof navigator!=="undefined"?navigator.locks:null,clock=()=>Math.floor(performance.now()),wall=()=>Date.now(),stateReader=cinemaRemoteSafeState,effect=(action,state)=>CinemaRemote.dispatch(action,{...state,source:"network"}),preferences=cinemaRemotePreferences}={}){
    this.identityReader=identityReader;this.clientFactory=clientFactory;this.locks=locks;this.clock=clock;this.wall=wall;this.stateReader=stateReader;this.effect=effect;this.preferences=preferences;
    this.guard=new CinemaRemoteGuard();
    this.removeInvalidation=CinemaRemote.onInvalidate(()=>{this.guard.invalidate();this.lastPresence=0;});this.identity=null;this.client=null;this.generation=0;this.target=null;this.control=null;this.foreground=null;this.foregroundIdentity=null;this.state="off";this.heldLock=false;this.releaseLock=null;this.timer=null;this.retryTimer=null;this.retryDelay=1000;this.delivery=0;this.responseRevision=0;this.stateRevision=1;this.lastPresence=0;this.presencePending=null;this.pairings=[];this.challenge=null;this.lastClock=clock();this.lastWall=wall();
  }
  eligible(){
    const now=this.clock(),wall=this.wall(),gap=wall-this.lastWall-(now-this.lastClock);
    const alive=now>=this.lastClock&&wall>=this.lastWall&&gap<=1500;this.lastClock=now;this.lastWall=wall;
    return !!(alive&&this.identity&&cinemaRemoteSameIdentity(this.identity,this.identityReader())&&document.visibilityState==="visible"&&document.hasFocus()&&this.preferences(this.identity).receiver&&this.heldLock);
  }
  fence(generation){if(generation!==this.generation||!this.eligible())throw new Error("receiver_retired");}
  retire(reason="unavailable",foregroundEnded=false){
    this.generation++;this.guard.deactivate();this.target=null;this.control=null;this.challenge=null;this.pairings=[];this.delivery=0;this.responseRevision=0;this.presencePending=null;
    clearInterval(this.timer);clearTimeout(this.retryTimer);this.timer=null;this.retryTimer=null;
    this.client?.retire();this.client=null;this.identity=null;this.heldLock=false;this.releaseLock?.();this.releaseLock=null;this.state=reason;
    if(foregroundEnded){this.foreground=null;this.foregroundIdentity=null;}
    CinemaRemoteCancelGestures();if(typeof cinemaRemoteUiChanged==="function")cinemaRemoteUiChanged();
  }
  async start(){
    const identity=this.identityReader();if(!identity||!this.preferences(identity).receiver)return;
    if(document.visibilityState!=="visible"||!document.hasFocus())return;
    if(this.identity&&cinemaRemoteSameIdentity(identity,this.identity)&&(this.heldLock||this.state==="connecting"))return;
    const changed=this.foregroundIdentity&&!cinemaRemoteSameIdentity(this.foregroundIdentity,identity);
    this.retire("connecting",!!changed);this.identity=identity;this.foreground=this.foreground||cinemaRemoteNewNonce();this.foregroundIdentity={...identity};const generation=this.generation;
    if(!this.locks?.request){this.state="web_locks_unavailable";return;}
    const lockName="cinema.remote.receiver:"+cinemaRemoteIdentityKey(identity);
    try{
      await this.locks.request(lockName,{mode:"exclusive",ifAvailable:true},async lock=>{
        if(generation!==this.generation)return;
        if(!lock){
          this.state="owned_by_another_tab";
          this.retryTimer=setTimeout(()=>{if(generation===this.generation&&cinemaRemoteSameIdentity(identity,this.identityReader())&&document.visibilityState==="visible"&&document.hasFocus()&&this.preferences(identity).receiver)this.start();},100);
          return;
        }
        this.heldLock=true;const released=new Promise(resolve=>{this.releaseLock=resolve;});
        try{
          this.fence(generation);this.client=this.clientFactory(identity);
          const receiver=cinemaRemoteLoad(identity).receiver;
          if(!receiver||!CinemaRemoteWire.id(receiver.receiver_id)||!cinemaRemoteSecret(receiver.receiver_secret)){this.state="registration_required";return;}
          this.receiver=receiver;
          const session=await this.client.request("sessions",{body:{receiver_id:receiver.receiver_id,foreground_id:this.foreground},proof:this.proof()});this.fence(generation);
          if(!CinemaRemoteWire.targetValid(session.target))throw new Error("invalid session target");
          this.target=session.target;this.retryDelay=1000;this.state="available";this.lastPresence=0;
          await this.publish(generation);this.fence(generation);
          this.timer=setInterval(()=>{if(!this.eligible()){this.retire("background",document.visibilityState!=="visible"||!cinemaRemoteSameIdentity(this.identity,this.identityReader()));return;}if(this.clock()-this.lastPresence>=(this.control?250:5000))this.publish(generation).catch(error=>this.fail(error,generation));},250);
          this.poll(generation).catch(error=>this.fail(error,generation));
          await released;
        }catch(error){this.fail(error,generation);}
        finally{if(generation===this.generation){this.heldLock=false;this.releaseLock=null;}}
      });
    }catch(error){this.fail(error,generation);}
    if(typeof cinemaRemoteUiChanged==="function")cinemaRemoteUiChanged();
  }
  proof(){return {kind:"receiver",secret:this.receiver.receiver_secret};}
  context(state){return {target:this.target,grant_id:this.control.active_grant_id,control_epoch:this.control.control_epoch,context_revision:state.context_revision,focus_revision:state.focus_revision,text_nonce:state.text_nonce};}
  async publish(generation){
    if(this.presencePending)return this.presencePending.promise;this.fence(generation);const state=this.stateReader();
    if(!CinemaRemoteWire.positive(++this.stateRevision))throw new Error("state revision exhausted");
    let credits=[];
    if(this.control&&state.route!=="restricted"){
      if(this.guard.setContext(this.context(state))!=="applied")throw new Error("invalid receiver context");
      for(const kind of ["interaction","playback"]){const credit=this.guard.mint(kind,cinemaRemoteNewNonce(),this.clock());if(credit)credits.push(credit);}
    }else this.guard.invalidate();
    const body={target:this.target,state:cinemaRemoteBoundPublishedState({state_revision:this.stateRevision,context_revision:state.context_revision,focus_revision:state.focus_revision,route:state.route,capabilities:state.capabilities,focused_label:state.focused_label,credits,text_nonce:state.text_nonce,playback:state.playback})};
    this.lastPresence=this.clock();const pending={promise:null};this.presencePending=pending;
    pending.promise=this.client.request("presence",{body,proof:this.proof()}).then(()=>this.fence(generation)).finally(()=>{if(this.presencePending===pending)this.presencePending=null;});return pending.promise;
  }
  apply(command,generation){
    if(generation!==this.generation||!this.eligible()){this.guard.deactivate();return "unavailable";}
    let decoded;try{decoded=CinemaRemoteWire.decode(command);}catch(_){return "invalid";}
    const state=this.stateReader();if(!this.control)return "stale_control";
    if(this.guard.setContext(this.context(state))!=="applied")return "unavailable";
    return this.guard.apply(decoded,this.clock(),cinemaRemoteSemanticCheck(decoded.action,state),action=>this.effect(action,state));
  }
  async poll(generation){
    for(;;){
      this.fence(generation);const result=await this.client.request("poll",{body:{target:this.target,after_delivery_id:this.delivery,after_response_revision:this.responseRevision,wait_ms:20000},proof:this.proof()});this.fence(generation);
      if(CinemaRemoteWire.targetKey(result.target)!==CinemaRemoteWire.targetKey(this.target)||!CinemaRemoteWire.positive(result.response_revision)||!CinemaRemoteWire.unsigned(result.delivery_id)||!Array.isArray(result.commands)||result.commands.length>32||!Array.isArray(result.pairings)||result.pairings.length>8)throw new Error("invalid remote poll");
      if(result.response_revision<this.responseRevision)continue;this.responseRevision=result.response_revision;
      const control=result.control;
      if(control&&!CinemaRemoteWire.exact(control,["control_epoch","active_grant_id","controller_name"]))throw new Error("invalid control");
      if(control&&(!CinemaRemoteWire.id(control.control_epoch)||!CinemaRemoteWire.id(control.active_grant_id)||CinemaRemoteWire.bytes(control.controller_name)>80))throw new Error("invalid control");
      if(this.control?.control_epoch!==control?.control_epoch){this.guard.deactivate();CinemaRemoteCancelGestures();this.lastPresence=0;}
      this.control=control;this.pairings=result.pairings.filter(value=>CinemaRemoteWire.exact(value,["pending_id","controller_name"])&&CinemaRemoteWire.id(value.pending_id)&&CinemaRemoteWire.bytes(value.controller_name)<=80);
      if(control){const state=this.stateReader();this.guard.setContext(this.context(state));}
      const outcomes=[];
      for(const command of result.commands){
        this.fence(generation);const outcome=this.apply(command,generation);
        if(CinemaRemoteWire.id(command.control_epoch)&&CinemaRemoteWire.positive(command.sequence)){
          const cached=outcome==="duplicate_or_old"?this.guard.result(command.control_epoch,command.sequence,this.clock()):null;
          outcomes.push({control_epoch:command.control_epoch,sequence:command.sequence,outcome:cached?.outcome||outcome});
        }
      }
      this.delivery=Math.max(this.delivery,result.delivery_id);
      if(outcomes.length){await this.client.request("ack",{body:{target:this.target,outcomes},proof:this.proof()});this.fence(generation);}
      if(typeof cinemaRemoteUiChanged==="function")cinemaRemoteUiChanged();
      if(this.pairings.length&&typeof cinemaRemotePairingPrompt==="function")cinemaRemotePairingPrompt(this);
    }
  }
  fail(error,generation){
    if(generation!==this.generation)return;const foreground=this.foreground;this.retire(error.code||error.message||"unavailable");this.foreground=foreground;
    if(["unauthorized","invalid"].includes(error.code))return;
    const delay=this.retryDelay+Math.floor(Math.random()*250);this.retryDelay=Math.min(30000,this.retryDelay*2);
    this.retryTimer=setTimeout(()=>this.start(),delay);
  }
}
let CINEMA_WEB_RECEIVER=null;
function cinemaRemoteReceiverSync(){
  if(!CINEMA_WEB_RECEIVER)CINEMA_WEB_RECEIVER=new CinemaWebReceiver();
  if(!cinemaRemoteIdentity()||!cinemaRemoteLocalEnabled("receiver")||document.visibilityState!=="visible"||!document.hasFocus()){const identity=cinemaRemoteIdentity(),changed=CINEMA_WEB_RECEIVER.foregroundIdentity&&!cinemaRemoteSameIdentity(CINEMA_WEB_RECEIVER.foregroundIdentity,identity);CINEMA_WEB_RECEIVER.retire("background",!identity||!!changed||document.visibilityState!=="visible");return;}
  CINEMA_WEB_RECEIVER.start();
}
function cinemaRemoteRetire(clearSecrets=false){
  const identity=CINEMA_WEB_RECEIVER?.identity||cinemaRemoteIdentity();CINEMA_WEB_RECEIVER?.retire("signed_out",true);
  if(typeof cinemaRemoteControllerRetire==="function")cinemaRemoteControllerRetire();
  if(clearSecrets)cinemaRemoteForget(identity);
}
