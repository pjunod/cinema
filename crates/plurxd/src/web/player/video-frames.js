"use strict";

// One browser request per element. Subscribers retain their own attachment,
// intent, generation and proof-floor checks; this owner only transports frames.
const PLAYBACK_VIDEO_FRAMES=new WeakMap();
let PLAYBACK_VIDEO_FRAME_SEQUENCE=0;
function requestPlaybackVideoFrame(video,callback){
  let owner=PLAYBACK_VIDEO_FRAMES.get(video);
  if(!owner){
    owner={subscribers:new Map(),request:null,generation:0,dispatching:false};
    PLAYBACK_VIDEO_FRAMES.set(video,owner);
    const native=()=>!String(video.currentSrc||video.src||"").startsWith("blob:");
    const cancel=()=>{
      ++owner.generation;
      const request=owner.request;
      if(request==null)return;
      try{
        video.cancelVideoFrameCallback(request.id);
        if(owner.request===request)owner.request=null;
        owner.cancelFailed=false;
      }catch(error){
        // Cancellation is unconfirmed. Retain the sole request so another
        // subscriber cannot create a second browser-owned request. A later
        // native discontinuity may retry cancellation; a late delivery is
        // fenced below and can release this outstanding ownership.
        if(!owner.cancelFailed&&typeof clientLog==="function")clientLog({level:"warn",
          event:"native_frame_observation",detail:"cancel_failed",message:"Frame callback cancellation could not be confirmed."});
        owner.cancelFailed=true;
      }
    };
    owner.release=()=>{
      if(owner.subscribers.size||owner.dispatching)return;
      cancel();
      if(owner.request!=null)return;
      for(const name of owner.events)video.removeEventListener(name,owner.event);
      PLAYBACK_VIDEO_FRAMES.delete(video);
    };
    owner.arm=()=>{
      if(owner.request!=null||owner.dispatching||!owner.subscribers.size)return;
      if(native()){
        // Playing requests wait for a completed seek and HAVE_FUTURE_DATA.
        // Paused seeks can deliver their single landing frame before seeked;
        // the subscriber retains its own target/floor proof. A still-pending
        // paused request is renewed at seek completion below.
        if(video.paused?video.readyState<2:(video.seeking||video.readyState<3))return;
      }
      const generation=owner.generation,request={id:null,whileSeeking:!!video.seeking,
        generation,src:video.src,currentSrc:video.currentSrc};
      owner.request=request;
      try{request.id=video.requestVideoFrameCallback((now,metadata)=>{
        if(owner.request!==request)return;
        owner.request=null;
        if(owner.generation!==generation){owner.arm();owner.release();return;}
        owner.dispatching=true;
        let failure=null;
        try{
          for(const [token,subscriber] of [...owner.subscribers]){
            if(owner.generation!==generation)break;
            if(owner.subscribers.get(token)!==subscriber)continue;
            owner.subscribers.delete(token);
            try{subscriber(now,metadata);}catch(error){failure=failure||error;}
          }
        }finally{
          owner.dispatching=false;
          try{owner.arm();}finally{owner.release();}
        }
        if(failure)throw failure;
      });}catch(error){
        if(owner.request===request)owner.request=null;
        throw error;
      }
    };
    owner.event=event=>{
      if(!native())return; // MSE keeps its existing callback lifecycle.
      if(event.type==="seeking"&&video.paused&&!owner.dispatching){
        const request=owner.request;
        // A same-resource paused seek can present its only landing before
        // seeked. Keep a valid armed request, or gather one if none is pending.
        // Failed cancellation and source replacement retain their old fences.
        if(request&&request.generation===owner.generation
          &&request.src===video.src&&request.currentSrc===video.currentSrc)return;
        if(!request){owner.arm();return;}
      }
      if(event.type==="seeking"||event.type==="emptied"||event.type==="loadeddata"){
        // Cancelling the sole native request empties WebKit's callback list.
        // The next qualified readiness event starts a new gathering cycle.
        cancel();
      }
      if(event.type==="seeked"&&owner.request?.whileSeeking)cancel();
      if(event.type!=="seeking"&&event.type!=="emptied")owner.arm();
    };
    owner.events=["seeking","emptied","loadeddata","seeked","canplay","playing","pause"];
    for(const name of owner.events)video.addEventListener(name,owner.event);
  }
  const token=++PLAYBACK_VIDEO_FRAME_SEQUENCE;
  owner.subscribers.set(token,callback);
  try{owner.arm();}catch(error){owner.subscribers.delete(token);owner.release();throw error;}
  return token;
}
function cancelPlaybackVideoFrame(video,token){
  const owner=PLAYBACK_VIDEO_FRAMES.get(video);
  if(!owner)return;
  owner.subscribers.delete(token);owner.release();
}
