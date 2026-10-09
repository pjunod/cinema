"use strict";
const CINEMA_REMOTE_SEQUENCES=new Map();
function cinemaRemoteControllerState(value){
  if(!CinemaRemoteWire.exact(value,["state_revision","context_revision","focus_revision","route","capabilities","focused_label","credits","text_nonce","playback"])||![value.state_revision,value.context_revision,value.focus_revision].every(CinemaRemoteWire.positive)||!["home","library","details","search","playback","tracks","restricted"].includes(value.route)||!Array.isArray(value.capabilities)||value.capabilities.length>12||value.capabilities.some(type=>!CinemaRemoteWire.actions.includes(type))||!Array.isArray(value.credits)||value.credits.length>16)throw new Error("invalid receiver state");
  if(value.focused_label!==null&&CinemaRemoteWire.bytes(value.focused_label)>256)throw new Error("invalid receiver label");
  if(value.text_nonce!==null&&!CinemaRemoteWire.id(value.text_nonce))throw new Error("invalid text context");
  if(value.credits.some(credit=>!CinemaRemoteWire.exact(credit,["nonce","kind"])||!CinemaRemoteWire.id(credit.nonce)||!["interaction","playback"].includes(credit.kind)))throw new Error("invalid receiver credit");
  if(value.playback!==null){
    const p=value.playback,m=p?.media,item=m?.type==="item"&&CinemaRemoteWire.exact(m,["type","item_id"])&&CinemaRemoteWire.positive(m.item_id),live=m?.type==="live_channel"&&CinemaRemoteWire.exact(m,["type","channel_id"])&&CinemaRemoteWire.bytes(m.channel_id)>0&&CinemaRemoteWire.bytes(m.channel_id)<=128;
    if(!CinemaRemoteWire.exact(p,["media","title","playing","position_ms","duration_ms","tracks"])||(!item&&!live)||CinemaRemoteWire.bytes(p.title)>256||typeof p.playing!=="boolean"||!CinemaRemoteWire.unsigned(p.position_ms)||!CinemaRemoteWire.unsigned(p.duration_ms)||!Array.isArray(p.tracks)||p.tracks.length>64||p.tracks.some(track=>!CinemaRemoteWire.exact(track,["kind","option_id","label"])||!["audio","subtitles","quality"].includes(track.kind)||CinemaRemoteWire.bytes(track.option_id)>128||!track.option_id||CinemaRemoteWire.bytes(track.label)>256))throw new Error("invalid playback summary");
  }
  if(value.route==="restricted")return {...value,capabilities:[],credits:[],focused_label:null,text_nonce:null,playback:null};return value;
}
class CinemaWebController{
  constructor({clientFactory=()=>new CinemaRemoteClient(),clock=()=>performance.now(),changed=()=>{if(typeof cinemaRemoteUiChanged==="function")cinemaRemoteUiChanged();},sequences=CINEMA_REMOTE_SEQUENCES}={}){
    this.clientFactory=clientFactory;this.clock=clock;this.changed=changed;this.sequences=sequences;this.generation=0;this.client=null;this.device=null;this.grant=null;this.control=null;this.state=null;this.responseRevision=0;this.renewTimer=null;this.sendPending=null;this.message="Choose a screen";this.holdTimer=null;this.renewPending=null;this.acquiredGeneration=null;this.acquisition=null;this.acquirePending=null;this.lastOutcomes=new Map();
  }
  sequenceKey(){return this.device&&this.grant&&this.control?JSON.stringify([CinemaRemoteWire.targetKey(this.device.target),this.grant.grant_id,this.control.control_epoch]):"";}
  ownsControl(){return !!(this.acquiredGeneration===this.generation&&this.control&&this.grant&&this.control.active_grant_id===this.grant.grant_id&&this.sequences.has(this.sequenceKey()));}
  alive(generation=this.generation){return generation===this.generation&&!!this.client?.current()&&document.visibilityState==="visible";}
  retire({release=true}={}){
    const old={device:this.device,grant:this.grant,control:this.control,identity:this.client?.identity};this.generation++;this.stopHold();clearInterval(this.renewTimer);this.renewTimer=null;this.client?.retire();this.client=null;this.control=null;this.state=null;this.sendPending=null;this.renewPending=null;this.acquiredGeneration=null;this.acquisition=null;this.acquirePending=null;this.lastOutcomes.clear();this.device=null;this.grant=null;this.responseRevision=0;
    if(release&&old.identity&&old.grant&&old.device&&old.control?.active_grant_id===old.grant.grant_id){
      const client=new CinemaRemoteClient(old.identity);client.request("control",{body:{target:old.device.target,grant_id:old.grant.grant_id,action:"release",control_epoch:old.control.control_epoch},proof:{kind:"grant",secret:old.grant.grant_secret}}).catch(()=>{}).finally(()=>client.retire());
    }
  }
  async connect(device,grant){
    this.retire();const generation=this.generation;
    if(!device.available||!CinemaRemoteWire.targetValid(device.target)||!CinemaRemoteWire.id(grant?.grant_id)||!cinemaRemoteSecret(grant?.grant_secret)||grant.receiver_id!==device.receiver_id)throw new Error("Screen or saved grant is unavailable");
    this.client=this.clientFactory();this.device=device;this.grant=grant;this.message="Connecting…";this.changed();
    await this.readState(generation,0);if(!this.alive(generation))return;
    this.poll(generation).catch(error=>{if(this.alive(generation)){this.retire({release:false});this.message=error.code||error.message||"Screen unavailable";this.changed();}});
  }
  proof(){return {kind:"grant",secret:this.grant.grant_secret};}
  acceptControl(value){
    if(value!==null&&(!CinemaRemoteWire.exact(value,["control_epoch","active_grant_id","controller_name"])||!CinemaRemoteWire.id(value.control_epoch)||!CinemaRemoteWire.id(value.active_grant_id)||CinemaRemoteWire.bytes(value.controller_name)>80))throw new Error("invalid controller lease");
    const changed=this.control?.control_epoch!==value?.control_epoch||this.control?.active_grant_id!==value?.active_grant_id;
    if(changed){this.stopHold();this.state=null;this.sendPending=null;this.renewPending=null;this.acquiredGeneration=null;this.acquisition=null;}this.control=value;
  }
  async readState(generation,wait_ms){
    if(!this.alive(generation))return;const target=this.device.target,result=await this.client.request("state",{body:{target,grant_id:this.grant.grant_id,after_revision:this.responseRevision,wait_ms},proof:this.proof()});
    if(!this.alive(generation))return;
    if(CinemaRemoteWire.targetKey(result.target)!==CinemaRemoteWire.targetKey(target)||!CinemaRemoteWire.positive(result.response_revision)||!Array.isArray(result.outcomes)||result.outcomes.length>64)throw new Error("invalid controller state");
    if(result.response_revision<this.responseRevision)return;this.responseRevision=result.response_revision;this.acceptControl(result.control);
    if(result.state!==null){const state=cinemaRemoteControllerState(result.state);if(!this.state||state.state_revision>=this.state.state_revision){if(this.state?.context_revision!==state.context_revision)this.stopHold();this.state=state;}}
    const outcomes=new Set(["applied","duplicate_or_old","expired","stale_target","stale_control","stale_context","stale_focus","restricted_surface","unauthorized","unsupported","busy","unavailable","invalid"]);
    for(const outcome of result.outcomes){if(!CinemaRemoteWire.exact(outcome,["control_epoch","sequence","outcome"])||!CinemaRemoteWire.id(outcome.control_epoch)||!CinemaRemoteWire.positive(outcome.sequence)||!outcomes.has(outcome.outcome))throw new Error("invalid command outcome");if(outcome.control_epoch===this.control?.control_epoch){this.lastOutcomes.set(outcome.control_epoch+":"+outcome.sequence,outcome.outcome);if(this.lastOutcomes.size>64)this.lastOutcomes.delete(this.lastOutcomes.keys().next().value);if(outcome.sequence===this.sequences.get(this.sequenceKey()))this.message=outcome.outcome;}}
    this.changed();
  }
  async poll(generation){while(this.alive(generation))await this.readState(generation,20000);}
  async acquire(takeover=false){
    const generation=this.generation;if(!this.alive(generation))throw new Error("Screen unavailable");if(this.acquirePending)throw new Error("Control request pending");
    const pending={generation};this.acquirePending=pending;this.acquiredGeneration=null;this.acquisition=null;this.stopHold();this.sendPending=null;
    try{
      let action="acquire",epoch=null;
      if(takeover)action="takeover";
      else if(this.control&&this.control.active_grant_id===this.grant.grant_id&&!this.sequences.has(this.sequenceKey())){action="takeover";epoch=this.control.control_epoch;}
      else if(this.control&&this.control.active_grant_id!==this.grant.grant_id)throw new Error("Another phone controls this screen. Choose Take over explicitly.");
      const target=this.device.target,grant=this.grant.grant_id;
      const request=(operation,expected)=>this.client.request("control",{body:{target,grant_id:grant,action:operation,control_epoch:expected},proof:this.proof()});
      let result=await request(action,epoch);if(!this.alive(generation))return;
      function validate(reply){if(CinemaRemoteWire.targetKey(reply.target)!==CinemaRemoteWire.targetKey(target)||!CinemaRemoteWire.positive(reply.response_revision)||!CinemaRemoteWire.exact(reply.control,["control_epoch","active_grant_id","controller_name"])||!CinemaRemoteWire.id(reply.control.control_epoch)||reply.control.active_grant_id!==grant||CinemaRemoteWire.bytes(reply.control.controller_name)>80)throw new Error("Control was not acquired");}
      validate(result);
      const responseKey=JSON.stringify([CinemaRemoteWire.targetKey(target),grant,result.control.control_epoch]);
      // Inspect the actual response, not only the pre-request snapshot. An
      // acquire may reveal an existing own lease created while state was stale.
      if(action==="acquire"&&!this.sequences.has(responseKey)){
        const observed=result.control.control_epoch;result=await request("takeover",observed);if(!this.alive(generation))return;validate(result);if(result.control.control_epoch===observed)throw new Error("Lease epoch did not rotate");
      }
      if(result.response_revision>=this.responseRevision){this.responseRevision=result.response_revision;this.acceptControl(result.control);}
      else if(result.control.control_epoch!==this.control?.control_epoch||result.control.active_grant_id!==this.control?.active_grant_id)return;
      const key=this.sequenceKey();if(!this.sequences.has(key))this.sequences.set(key,0);if(this.sequences.size>32)this.sequences.delete(this.sequences.keys().next().value);
      this.acquiredGeneration=generation;this.acquisition=pending;
      const acquisition=pending,renewalEpoch=this.control.control_epoch;
      clearInterval(this.renewTimer);this.renewTimer=setInterval(()=>this.renew(generation).catch(error=>{if(this.alive(generation)&&this.acquisition===acquisition&&this.control?.control_epoch===renewalEpoch){this.retire({release:false});this.message=error.code||error.message;this.changed();}}),5000);
      this.message="Remote active";this.changed();
    }finally{if(this.acquirePending===pending)this.acquirePending=null;}
  }
  async renew(generation){
    if(!this.alive(generation)||!this.ownsControl()||this.renewPending)return;const target=this.device.target,epoch=this.control.control_epoch,acquisition=this.acquisition,pending={generation,epoch};this.renewPending=pending;
    try{
      const result=await this.client.request("control",{body:{target,grant_id:this.grant.grant_id,action:"renew",control_epoch:epoch},proof:this.proof()});
      if(!this.alive(generation)||this.control?.control_epoch!==epoch)return;
      if(CinemaRemoteWire.targetKey(result.target)!==CinemaRemoteWire.targetKey(target)||!CinemaRemoteWire.positive(result.response_revision))throw new Error("invalid renewal");
      if(result.response_revision<this.responseRevision)return;this.responseRevision=result.response_revision;this.acceptControl(result.control);this.changed();
    }catch(error){if(this.alive(generation)&&this.acquisition===acquisition&&this.control?.control_epoch===epoch&&this.renewPending===pending)throw error;}
    finally{if(this.renewPending===pending)this.renewPending=null;}
  }
  async send(action){
    const generation=this.generation,state=this.state;if(!this.alive(generation)||!this.ownsControl()||!state||this.sendPending)return;
    if(!CinemaRemoteWire.actionValid(action)||!state.capabilities.includes(action.type)){this.stopHold();throw new Error("Unsupported control");}
    const credit=state.credits.slice().reverse().find(value=>value.kind===CinemaRemoteWire.creditKind(action));if(!credit){this.stopHold();throw new Error("Waiting for fresh screen state");}
    const key=this.sequenceKey(),sequence=this.sequences.get(key)+1;if(!CinemaRemoteWire.positive(sequence))throw new Error("Control sequence exhausted; acquire a new lease");
    this.sequences.set(key,sequence);
    const command={version:CinemaRemoteWire.version,target:this.device.target,grant_id:this.grant.grant_id,control_epoch:this.control.control_epoch,sequence,credit:credit.nonce,context_revision:state.context_revision,focus_revision:state.focus_revision,action};
    CinemaRemoteWire.decode(command);const pending={generation,sequence};this.sendPending=pending;this.message="Sending…";this.changed();
    try{
      const result=await this.client.request("commands",{body:command,proof:this.proof()});if(!this.alive(generation)||this.control?.control_epoch!==command.control_epoch||this.sendPending!==pending)return;
      if(!CinemaRemoteWire.exact(result,["version","queued","control_epoch","sequence"])||result.queued!==true||result.control_epoch!==command.control_epoch||result.sequence!==sequence)throw new Error("invalid command receipt");
      this.message=this.lastOutcomes.get(command.control_epoch+":"+sequence)||"Queued — waiting for screen outcome";
    }catch(error){if(this.alive(generation)&&this.control?.control_epoch===command.control_epoch&&this.sendPending===pending){this.stopHold();this.message=error.code||"Outcome unknown; this action will not be retried";}}
    finally{const current=this.sendPending===pending&&this.alive(generation)&&this.control?.control_epoch===command.control_epoch;if(this.sendPending===pending)this.sendPending=null;if(current)this.changed();}
  }
  stopHold(){clearTimeout(this.holdTimer);this.holdTimer=null;this.holdOperation=null;}
  hold(direction){
    this.stopHold();if(!this.alive()||!this.ownsControl()||!this.state?.capabilities.includes("navigate"))return;const operation={generation:this.generation,context:this.state?.context_revision};this.holdOperation=operation;
    const send=()=>this.send({type:"navigate",direction}).catch(error=>{if(this.holdOperation===operation){this.stopHold();this.message=error.message;this.changed();}});
    send();const repeat=()=>{this.holdTimer=null;if(this.holdOperation!==operation||!this.alive(operation.generation)||this.state?.context_revision!==operation.context){this.stopHold();return;}send();if(this.holdOperation===operation)this.holdTimer=setTimeout(repeat,125);};
    if(this.holdOperation===operation)this.holdTimer=setTimeout(repeat,350);
  }

}
let CINEMA_WEB_CONTROLLER=null;
function cinemaRemoteControllerRetire(){CINEMA_WEB_CONTROLLER?.retire();}
