// SPDX-License-Identifier: Apache-2.0
// These fixed functions run in MAIN. No string evaluation or page-to-host API.
export function installBridge(epoch,workerId="development",operation=1){
  if(typeof cinemaRemoteLocalEnabled==="function"&&!cinemaRemoteLocalEnabled("cec"))return {ready:false};
  if(document.visibilityState!=="visible")return {ready:false};
  if(!document.hasFocus())return {ready:false,focus_pending:true};
  if(typeof CinemaRemote==="undefined"||typeof TOKEN==="undefined"||!TOKEN||typeof ME==="undefined"||!ME)return {ready:false};
  const previous=globalThis.CinemaDesktopBridge;
  if(previous?.workerId===workerId&&previous.operation>operation)return {ready:false};
  if(previous)previous.disable();
  const token=TOKEN,user=String(ME.id),origin=location.origin,api=typeof API==="undefined"?null:API,instance=typeof SERVER==="undefined"?null:SERVER?.instance_id;
  let enabled=true,lastClock=performance.now(),lastWall=Date.now(),lastProbe=lastClock;
  const credits=new Map();
  function alive(){
    const now=performance.now(),wall=Date.now();
    if(!enabled||(typeof SERVER!=="undefined"&&SERVER?.instance_id!==instance)||(typeof cinemaRemoteLocalEnabled==="function"&&!cinemaRemoteLocalEnabled("cec"))||TOKEN!==token||!ME||String(ME.id)!==user||location.origin!==origin||(typeof API!=="undefined"&&API!==api)||document.visibilityState!=="visible"||!document.hasFocus()||now<lastClock||wall<lastWall||wall-lastWall>1500){enabled=false;credits.clear();return false;}
    lastClock=now;lastWall=wall;return true;
  }
  function disable(){enabled=false;credits.clear();CinemaRemote.physicalInput();}
  function probe(credit){
    if(!alive())return {ready:false};
    const now=performance.now();lastProbe=now;
    for(const [nonce,value] of credits)if(now>=value.deadline)credits.delete(nonce);
    if(credits.size>=8)credits.delete(credits.keys().next().value);
    const context=typeof CinemaRemoteLocalPhysical!=="undefined"?CinemaRemoteLocalPhysical.snapshot():CinemaRemote.snapshotPhysical?.()||CinemaRemote.snapshot();
    credits.set(credit,{deadline:now+750,context});return {ready:true};
  }
  function input(key,credit,bindingEpoch){
    const now=performance.now();
    if(bindingEpoch!==epoch||!alive()||now-lastProbe>=1000)return "unavailable";
    const offered=credits.get(credit);if(!offered||now>=offered.deadline)return "expired";
    if(key==="select"){
      const current=typeof CinemaRemoteLocalPhysical!=="undefined"?CinemaRemoteLocalPhysical.snapshot():CinemaRemote.snapshotPhysical?.()||CinemaRemote.snapshot();
      if(JSON.stringify(current)!==JSON.stringify(offered.context))return "stale_focus";
    }
    const directions=["up","down","left","right"];
    if(![...directions,"select","back","home","play","pause","play_pause","stop"].includes(key))return "invalid";
    // Local physical input retires network work, then captures its own context.
    CinemaRemote.physicalInput();
    let action=directions.includes(key)?{type:"navigate",direction:key}:{type:key};
    if(["play","pause","play_pause"].includes(key)){
      let playing=key==="play";
      if(key==="play_pause"){
        if(typeof cinemaRemoteVod==="function"&&cinemaRemoteVod())playing=!playerWantsPlayback(document.getElementById("video"));
        else if(typeof cinemaRemoteLive==="function"&&cinemaRemoteLive())playing=!!document.getElementById("live-tv-video")?.paused;
        else return "unavailable";
      }
      action={type:"set_playing",playing};
    }
    if(typeof CinemaRemoteLocalPhysical!=="undefined")return CinemaRemoteLocalPhysical.dispatch(action,offered.context);
    const context={...(CinemaRemote.snapshotPhysical?.()||CinemaRemote.snapshot()),source:"local_cec"};
    return typeof CinemaRemote.dispatchPhysical==="function"?CinemaRemote.dispatchPhysical(action,context):CinemaRemote.dispatch(action,context);
  }
  globalThis.CinemaDesktopBridge={epoch,workerId,operation,probe,input,disable};
  return {ready:true,epoch};
}
export function probeBridge(credit){return globalThis.CinemaDesktopBridge?.probe(credit)||{ready:false};}
export function deliverBridge(key,credit,epoch){return globalThis.CinemaDesktopBridge?.input(key,credit,epoch)||"unavailable";}
export function disableBridge(epoch){const bridge=globalThis.CinemaDesktopBridge;if(bridge?.epoch===epoch)bridge.disable();}
