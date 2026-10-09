"use strict";
// Semantic navigation only. This registry does not authenticate a sender.
// registerScope({id,root,back,home,text,nextPage}) replaces the active scope.
// registerAction({id,element,label,activate}) installs a typed app closure.
// Registration is local trusted application code; no wire selectors/closures.
const CinemaRemote=(()=>{
  let scope=null, actions=new Map(), focused=null, contextRevision=1, focusRevision=1;
  let physicalDispatch=false;
  let refreshing=false, dispatching=false, refresh=()=>{}, guard=()=>false;
  const listeners=new Set();
  function advance(value){ if(value>=Number.MAX_SAFE_INTEGER) throw new Error("remote revision exhausted"); return value+1; }
  function invalidate(reason="context"){
    contextRevision=advance(contextRevision);
    for(const listener of listeners) listener(reason);
  }
  function visible(el){
    if(!el||!el.isConnected||el.hidden||el.disabled||el.closest('[hidden],[inert]')) return false;
    const style=window.getComputedStyle(el);
    return style.display!=="none"&&style.visibility!=="hidden"&&el.getClientRects().length>0;
  }
  function focusChanged(id){
    if(focused===id) return;
    const old=actions.get(focused); old?.element.classList.remove("cinema-remote-focus");
    focused=id; focusRevision=advance(focusRevision);
  }
  function registerScope(next){
    if(!next||!next.id||!next.root) throw new Error("invalid remote scope");
    if(!scope||scope.id!==next.id||scope.root!==next.root){
      actions.clear(); focusChanged(null); scope=next; invalidate("scope");
    }else scope=next;
  }
  function registerAction(entry){
    if(!scope||!entry||!entry.id||!entry.element||typeof entry.activate!=="function") throw new Error("invalid remote action");
    if(actions.size>=2048&&!actions.has(entry.id)) return false;
    const old=actions.get(entry.id);
    if(old&&old.element!==entry.element){ invalidate("registration"); if(focused===entry.id) focusChanged(null); }
    actions.set(entry.id,entry); return true;
  }
  function owns(element){return !!scope&&[scope.root,...(scope.roots||[])].some(root=>root.contains(element));}
  function reconcile(){
    if(refreshing) return;
    refreshing=true;
    try{ refresh(); }finally{ refreshing=false; }
    for(const [id,entry] of actions){
      if(!entry.element.isConnected||!owns(entry.element)){
        actions.delete(id); if(focused===id) focusChanged(null); invalidate("registration");
      }
    }
    const active=document.activeElement;
    const at=[...actions.values()].find(entry=>entry.element===active);
    if(at) focusChanged(at.id);
    else if(active&&active!==document.body) focusChanged(null);
  }
  function boundedLabel(text){
    let label="",bytes=0;const encoder=new TextEncoder();
    for(const character of String(text||"")){
      const size=encoder.encode(character).length;if(bytes+size>256)break;
      label+=character;bytes+=size;
    }
    return label;
  }
  function snapshot(physical=false){
    reconcile();
    const blocked=!scope||!guard();
    const entry=actions.get(focused);
    const offered=physical||!entry?.localOnly;
    return {context_revision:contextRevision,focus_revision:focusRevision,
      route_category:scope?.category||"unsupported",blocked,
      focused_id:blocked||!offered?null:focused,focused_label:blocked||!offered?null:boundedLabel(entry?.label),
      text_nonce:blocked?null:scope?.text?.nonce||null};
  }
  function focus(entry){
    if(!visible(entry?.element)) return "unavailable";
    focusChanged(entry.id);
    entry.element.classList.add("cinema-remote-focus");
    if(!entry.element.hasAttribute("tabindex")&&!/^(A|BUTTON|INPUT|SELECT)$/.test(entry.element.tagName)) entry.element.tabIndex=0;
    entry.element.focus({preventScroll:true});
    entry.element.scrollIntoView({block:"nearest",inline:"nearest"});
    return "applied";
  }
  function navigate(direction){
    const entries=[...actions.values()].filter(entry=>(physicalDispatch||!entry.localOnly)&&visible(entry.element));
    if(!entries.length) return "unavailable";
    const current=actions.get(focused);
    if(!current||!visible(current.element)) return focus(entries[0]);
    const box=current.element.getBoundingClientRect(),x=box.left+box.width/2,y=box.top+box.height/2;
    const ranked=entries.filter(entry=>entry!==current).map(entry=>{
      const b=entry.element.getBoundingClientRect(),dx=b.left+b.width/2-x,dy=b.top+b.height/2-y;
      const along=direction==="left"?-dx:direction==="right"?dx:direction==="up"?-dy:dy;
      const across=direction==="left"||direction==="right"?Math.abs(dy):Math.abs(dx);
      return {entry,along,score:along+across*3};
    }).filter(item=>item.along>1).sort((a,b)=>a.score-b.score||a.entry.id.localeCompare(b.entry.id));
    if(ranked.length) return focus(ranked[0].entry);
    if(scope.nextPage&&(direction==="down"||direction==="right")) return scope.nextPage();
    return "applied";
  }
  function activate(){
    const entry=actions.get(focused);
    if(!entry||!owns(entry.element)||!visible(entry.element)||document.activeElement!==entry.element) return "stale_focus";
    if(entry.localOnly&&!physicalDispatch)return "restricted_surface";
    const result=entry.activate();
    return typeof result==="string"?result:"applied";
  }
  function dispatch(action,context){
    const state=snapshot(physicalDispatch);
    if(state.blocked) return "restricted_surface";
    if(!context||context.context_revision!==contextRevision) return "stale_context";
    if(action?.type==="select"&&context.focus_revision!==focusRevision) return "stale_focus";
    if(dispatching) return "busy";
    dispatching=true;
    const before=CinemaRemoteGestureState();
    try{ return CinemaRemoteRouteAction(action,context,{scope,navigate,activate,focus}); }
    finally{ CinemaRemoteRememberGesture(context,before);dispatching=false; }
  }
  // Physical adapters invalidate network work BEFORE taking their context.
  function physicalInput(){ invalidate("physical_input"); CinemaRemoteCancelGestures(); }
  document.addEventListener("focusin",()=>{ if(!dispatching){ reconcile(); invalidate("physical_focus"); } },true);
  for(const type of ["keydown","pointerdown"]) document.addEventListener(type,event=>{ if(event.isTrusted) physicalInput(); },true);
  window.addEventListener("blur",()=>{ physicalInput(); });
  document.addEventListener("visibilitychange",()=>invalidate("visibility"));
  function dispatchPhysical(action,context){if(dispatching)return "busy";physicalDispatch=true;try{return dispatch(action,context);}finally{physicalDispatch=false;}}
  return {snapshot:()=>snapshot(false),snapshotPhysical:()=>snapshot(true),dispatch,dispatchPhysical,invalidate,registerScope,registerAction,physicalInput,
    onInvalidate(listener){ listeners.add(listener); return ()=>listeners.delete(listener); },
    configure({refresh:nextRefresh,guard:nextGuard}){ refresh=nextRefresh; guard=nextGuard; },
    focusById(id){ reconcile(); return focus(actions.get(id)); },
    clear(){ actions.clear(); focusChanged(null); scope=null; invalidate("clear"); }};
})();
