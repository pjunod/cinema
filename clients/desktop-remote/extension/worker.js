// SPDX-License-Identifier: Apache-2.0
import {installBridge,probeBridge,deliverBridge,disableBridge} from "./bridge.js";
export const HOST="tv.plurx.cinema_remote";
const UUID=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const KEYS=new Set(["up","down","left","right","select","back","home","play","pause","play_pause","stop"]);
export function senderMatches(sender,binding){
  return !!(binding&&sender.frameId===0&&sender.tab?.id===binding.tabId&&sender.documentId===binding.documentId&&sender.origin===binding.origin);
}
export function nativeInputValid(message){
  return !!(message&&Object.keys(message).sort().join(",")==="credit,epoch,key,sequence,type"&&message.type==="input"&&KEYS.has(message.key)&&UUID.test(message.epoch)&&UUID.test(message.credit)&&Number.isSafeInteger(message.sequence)&&message.sequence>0);
}
export class DesktopWorker{
  constructor(browser,clock=()=>performance.now(),nonce=()=>crypto.randomUUID()){
    this.browser=browser;this.clock=clock;this.nonce=nonce;
    this.binding=null;this.host=null;this.documentPort=null;this.credits=new Map();this.sequence=0;
    this.pending=null;this.probing=null;this.workerId=nonce();this.generation=0;this.state="unbound";this.enabled=false;this.lastKey=null;this.lastOutcome=null;
  }
  async main(binding,func,args=[]){
    const results=await this.browser.scripting.executeScript({target:{tabId:binding.tabId,documentIds:[binding.documentId]},world:"MAIN",func,args});
    const result=results.find(value=>value.frameId===0&&value.documentId===binding.documentId);
    if(!result)throw new Error("document_replaced");return result.result;
  }
  async clearBinding(reason="unbound"){
    const binding=this.binding,port=this.host,documentPort=this.documentPort;
    this.binding=null;this.host=null;this.documentPort=null;this.credits.clear();this.sequence=0;this.pending=null;this.probing=null;
    this.state=reason;
    if(documentPort)documentPort.disconnect();
    if(port){try{if(binding)port.postMessage({type:"unbind",epoch:binding.epoch});}catch(_){}port.disconnect();}
    if(binding){try{await this.main(binding,disableBridge,[binding.epoch]);}catch(_){}}
  }
  async unbind(reason="unbound"){this.generation++;await this.clearBinding(reason);}
  async bind(tab){
    // Own the operation before the first await, including cleanup/permissions.
    const generation=++this.generation,epoch=this.nonce();
    const operationWindow={generation,windowId:tab.windowId,tabId:tab.id};this.operationWindow=operationWindow;
    let binding=null,port=null;
    const fence=()=>{if(generation!==this.generation)throw new Error("binding_superseded");};
    try{
      await this.clearBinding();fence();
      if(!this.enabled)throw new Error("local_cec_disabled");
      const origin=new URL(tab.url).origin;
      if(!/^https?:\/\//.test(origin))throw new Error("unsupported_origin");
      const allowed=await this.browser.permissions.contains({origins:[origin+"/*"]});fence();
      if(!allowed)throw new Error("origin_not_granted");
      // An explicit popup Bind closes the popup. Give focus restoration a
      // bounded setup window; no input or automatic rebind exists in this state.
      const focusDeadline=this.clock()+500;
      let result,pinnedDocument=null;
      for(let attempt=0;attempt<11;attempt++){
        const target=pinnedDocument?{tabId:tab.id,documentIds:[pinnedDocument]}:{tabId:tab.id,frameIds:[0]};
        const results=await this.browser.scripting.executeScript({target,world:"MAIN",func:installBridge,args:[epoch,this.workerId,generation]});
        result=results.find(value=>value.frameId===0);
        if(!result?.documentId||(pinnedDocument&&result.documentId!==pinnedDocument))throw new Error("document_replaced");
        pinnedDocument=result.documentId;
        if(result?.documentId)binding={origin,tabId:tab.id,windowId:tab.windowId,documentId:result.documentId,epoch};
        fence();
        if(!result?.result?.focus_pending||this.clock()>=focusDeadline||attempt===10)break;
        await new Promise(resolve=>setTimeout(resolve,50));fence();
      }
      if(!result?.result?.ready||result.result.epoch!==epoch||!binding)throw new Error("receiver_unavailable");
      this.binding=binding;
      await this.browser.storage.local.set({origin});fence();
      port=this.browser.runtime.connectNative(HOST);this.host=port;
      port.onMessage.addListener(message=>{if(this.host===port)this.receive(message);});
      port.onDisconnect.addListener(()=>{if(this.host===port)this.unbind("helper_disconnected");});
      port.postMessage({type:"bind",epoch});
      await this.browser.scripting.executeScript({target:{tabId:tab.id,documentIds:[binding.documentId]},world:"ISOLATED",files:["content.js"]});fence();
      await this.probe(binding);fence();
      if(this.binding!==binding)throw new Error("receiver_unavailable");
      return this.status();
    }catch(error){
      if(generation===this.generation)await this.unbind(error.message);
      else{
        if(port&&this.host!==port)port.disconnect();
        if(binding){try{await this.main(binding,disableBridge,[epoch]);}catch(_){}}
      }
      throw error;
    }finally{if(this.operationWindow===operationWindow)this.operationWindow=null;}
  }
  status(){return {enabled:this.enabled,state:this.state,bound:!!this.binding,origin:this.binding?.origin||null,last_key:this.lastKey,last_outcome:this.lastOutcome};}
  async probe(binding=this.binding){
    if(!binding||this.probing?.binding===binding)return;
    const probing={binding};this.probing=probing;
    const credit=this.nonce(),issued=this.clock(),generation=this.generation;
    this.credits.set(credit,{issued,deadline:issued+750,binding});
    if(this.credits.size>8)this.credits.delete(this.credits.keys().next().value);
    try{
      const result=await this.main(binding,probeBridge,[credit]);
      const now=this.clock();
      if(this.binding!==binding||generation!==this.generation)return;
      if(!result.ready||now<issued||now>=issued+750){await this.unbind("receiver_unavailable");return;}
      this.host?.postMessage({type:"heartbeat",epoch:binding.epoch,credit});
    }catch(_){if(this.binding===binding)await this.unbind("receiver_unavailable");}
    finally{if(this.probing===probing)this.probing=null;}
  }
  receive(message){
    if(message?.type==="status"){
      const states=new Set(["connecting","bound","unbound","missing_dependency","version_mismatch","no_adapter","permission_denied","adapter_busy","waiting_for_tv","disconnected","unsupported_backend","invalid_configuration","backend_error","open_failed","busy","receiver_unavailable"]);
      if(Object.keys(message).sort().join(",")==="epoch,state,type"&&states.has(message.state)&&message.epoch===this.binding?.epoch)this.state=message.state;
      return;
    }
    if(!nativeInputValid(message)||message.epoch!==this.binding?.epoch||message.sequence<=this.sequence)return;
    const credit=this.credits.get(message.credit),now=this.clock();
    if(!credit||credit.binding!==this.binding||now<credit.issued||now>=credit.deadline)return;
    this.sequence=message.sequence;this.lastKey=message.key;
    if(this.pending){this.state="busy";return;}
    const binding=this.binding,generation=this.generation,pending={binding,generation};this.pending=pending;
    this.main(binding,deliverBridge,[message.key,message.credit,binding.epoch]).then(outcome=>{
      if(this.binding===binding&&generation===this.generation){
        const outcomes=new Set(["applied","expired","unavailable","invalid","restricted_surface","unsupported","stale_context","stale_focus"]);
        this.lastOutcome=outcomes.has(outcome)?outcome:"unavailable";
        this.state=outcome==="applied"?"bound":outcome==="restricted_surface"?"restricted_surface":"receiver_unavailable";
      }
    }).catch(()=>{if(this.binding===binding)this.unbind("receiver_unavailable");}).finally(()=>{if(this.pending===pending)this.pending=null;});
  }
  documentConnection(port){
    if(port.name!=="cinema-cec-document"||!senderMatches(port.sender,this.binding)){port.disconnect();return;}
    if(this.documentPort&&this.documentPort!==port){port.disconnect();return;}
    this.documentPort=port;
    const binding=this.binding;
    port.onMessage.addListener(message=>{
      if(this.documentPort!==port||!senderMatches(port.sender,this.binding))return;
      if(Object.keys(message||{}).join(",")!=="type")return;
      if(message.type==="tick")this.probe(binding);
      else if(message.type==="hidden")this.unbind("receiver_unavailable");
    });
    port.onDisconnect.addListener(()=>{if(this.documentPort===port)this.unbind("receiver_unavailable");});
  }
  start(){
    const b=this.browser;
    const preferences=b.storage.local.get("enabled").then(value=>{this.enabled=value.enabled===true;});
    b.runtime.onConnect.addListener(port=>this.documentConnection(port));
    b.runtime.onMessage.addListener((message,sender,respond)=>{
      // Only packaged extension UI is allowed to bind or change local settings.
      if(sender.id!==b.runtime.id||sender.tab||sender.url!==b.runtime.getURL("popup.html"))return;
      const run=async()=>{
        await preferences;
        if(message.type==="status")return this.status();
        if(message.type==="enable"&&typeof message.enabled==="boolean"){
          this.enabled=message.enabled;await b.storage.local.set({enabled:this.enabled});
          if(!this.enabled)await this.unbind("disabled");return this.status();
        }
        if(message.type==="unbind"){await this.unbind();return this.status();}
        if(message.type==="bind"){
          const tabs=await b.tabs.query({active:true,currentWindow:true});if(!tabs[0])throw new Error("no_active_tab");return this.bind(tabs[0]);
        }
        throw new Error("invalid_command");
      };
      run().then(respond,error=>respond({error:error.message}));return true;
    });
    b.webNavigation.onCommitted.addListener(details=>{
      if(details.frameId===0&&(details.tabId===this.operationWindow?.tabId||(details.tabId===this.binding?.tabId&&details.documentId!==this.binding.documentId)))this.unbind("document_replaced");
    });
    b.tabs.onRemoved.addListener(id=>{if(id===this.binding?.tabId)this.unbind("tab_closed");});
    b.tabs.onActivated.addListener(info=>{const selected=this.operationWindow||this.binding;if(selected&&info.windowId===selected.windowId&&info.tabId!==selected.tabId)this.unbind("tab_inactive");});
    b.windows.onFocusChanged.addListener(id=>{const windowId=this.operationWindow?.windowId??this.binding?.windowId;if(windowId!==undefined&&(id===b.windows.WINDOW_ID_NONE||id!==windowId))this.unbind("window_inactive");});
    b.runtime.onStartup.addListener(()=>this.unbind("browser_restarted"));
  }
}
if(typeof chrome!=="undefined")new DesktopWorker(chrome).start();
