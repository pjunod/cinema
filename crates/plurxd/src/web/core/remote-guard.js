"use strict";
// B01-equivalent final UI-owner admission; this module grants no HTTP authority.
const CinemaRemoteWire=(()=>{
  const version="cinema.remote.v1",uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const encoder=new TextEncoder();
  const positive=value=>Number.isSafeInteger(value)&&value>0;
  const unsigned=value=>Number.isSafeInteger(value)&&value>=0&&!Object.is(value,-0);
  const bytes=value=>typeof value==="string"?encoder.encode(value).length:Infinity;
  const id=value=>typeof value==="string"&&uuid.test(value);
  function exact(value,keys){return !!(value&&typeof value==="object"&&!Array.isArray(value)&&Object.keys(value).sort().join(",")===keys.slice().sort().join(","));}
  // JSON.parse cannot reject duplicate fields. Scan bounded JSON into objects
  // without prototypes, reject duplicate keys and cap nesting before parsing.
  function parse(text,limit=16384){
    if(typeof text!=="string"||bytes(text)>limit)throw new Error("invalid");
    let at=0;
    const whitespace=()=>{while(/[ \t\r\n]/.test(text[at]||"\0"))at++;};
    function string(){
      const start=at++;let escaped=false;
      while(at<text.length){const c=text[at++];if(c==='"'&&!escaped){const value=JSON.parse(text.slice(start,at));
        for(let i=0;i<value.length;i++){const n=value.charCodeAt(i);if(n>=0xd800&&n<=0xdbff){const next=value.charCodeAt(++i);if(!(next>=0xdc00&&next<=0xdfff))throw new Error("invalid");}else if(n>=0xdc00&&n<=0xdfff)throw new Error("invalid");}return value;}
        escaped=c==="\\"&&!escaped;
      }throw new Error("invalid");
    }
    function value(depth=0){
      if(depth>32)throw new Error("invalid");whitespace();const c=text[at];
      if(c==='"')return string();
      if(c==="{"||c==="["){
        const object=c==="{",out=object?Object.create(null):[],keys=new Set();at++;whitespace();
        const end=object?"}":"]";if(text[at]===end){at++;return out;}
        for(;;){whitespace();let key;if(object){if(text[at]!=='"')throw new Error("invalid");key=string();if(keys.has(key))throw new Error("invalid");keys.add(key);whitespace();if(text[at++]!==":")throw new Error("invalid");}
          const next=value(depth+1);if(object)out[key]=next;else out.push(next);whitespace();const sep=text[at++];if(sep===end)return out;if(sep!==",")throw new Error("invalid");
        }
      }
      const match=/^(?:true|false|null|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)/.exec(text.slice(at));
      if(!match)throw new Error("invalid");at+=match[0].length;const primitive=JSON.parse(match[0]);if(typeof primitive==="number"&&(!Number.isSafeInteger(primitive)||/[.eE]/.test(match[0])||match[0]==="-0"))throw new Error("invalid");return primitive;
    }
    const result=value();whitespace();if(at!==text.length)throw new Error("invalid");return result;
  }
  const actionFields=new Map([
    ["navigate",["type","direction"]],["select",["type"]],["back",["type"]],["home",["type"]],
    ["set_playing",["type","playing"]],["seek_relative",["type","seconds"]],["seek_absolute",["type","position_ms"]],
    ["stop",["type"]],["open_tracks",["type","kind"]],["choose_track",["type","kind","option_id"]],
    ["text_replace",["type","text_nonce","text"]],["play_item",["type","item_id"]]
  ]);
  function actionValid(action){
    const fields=actionFields.get(action?.type);if(!fields||!exact(action,fields))return false;
    if(action.type==="navigate")return ["up","down","left","right"].includes(action.direction);
    if(action.type==="set_playing")return typeof action.playing==="boolean";
    if(action.type==="seek_relative")return [-30,-10,10,30].includes(action.seconds);
    if(action.type==="seek_absolute")return unsigned(action.position_ms);
    if(action.type==="play_item")return positive(action.item_id);
    if(["open_tracks","choose_track"].includes(action.type)&&!["audio","subtitles","quality"].includes(action.kind))return false;
    if(action.type==="choose_track")return bytes(action.option_id)>0&&bytes(action.option_id)<=128;
    if(action.type==="text_replace")return id(action.text_nonce)&&bytes(action.text)<=512;
    return true;
  }
  function targetValid(target){return exact(target,["owner_node_id","session_id","receiver_epoch"])&&bytes(target.owner_node_id)>0&&bytes(target.owner_node_id)<=128&&!/[\x00-\x1f\x7f]/.test(target.owner_node_id)&&id(target.session_id)&&id(target.receiver_epoch);}
  function targetKey(target){return target?JSON.stringify([target.owner_node_id,target.session_id.toLowerCase(),target.receiver_epoch.toLowerCase()]):"";}
  function decode(input){
    const command=typeof input==="string"?parse(input):parse(JSON.stringify(input));
    if(!exact(command,["version","target","grant_id","control_epoch","sequence","credit","context_revision","focus_revision","action"])||command.version!==version||!targetValid(command.target)||![command.grant_id,command.control_epoch,command.credit].every(id)||![command.sequence,command.context_revision,command.focus_revision].every(positive)||!actionValid(command.action))throw new Error("invalid");
    command.target.session_id=command.target.session_id.toLowerCase();command.target.receiver_epoch=command.target.receiver_epoch.toLowerCase();
    for(const key of ["grant_id","control_epoch","credit"])command[key]=command[key].toLowerCase();
    if(command.action.type==="text_replace")command.action.text_nonce=command.action.text_nonce.toLowerCase();
    return command;
  }
  function creditKind(action){return ["navigate","select","back","home","text_replace","open_tracks"].includes(action.type)?"interaction":"playback";}
  return {version,positive,unsigned,bytes,id,exact,parse,decode,actionValid,targetValid,targetKey,creditKind,actions:[...actionFields.keys()]};
})();
class CinemaRemoteGuard{
  constructor(){this.context=null;this.active=false;this.credits=[];this.results=[];this.lastSequence=0;this.lastNow=null;}
  invalidate(){this.credits=[];}
  deactivate(){this.active=false;this.invalidate();this.results=[];}
  setContext(context){
    if(!context||!CinemaRemoteWire.targetValid(context.target)||!CinemaRemoteWire.id(context.grant_id)||!CinemaRemoteWire.id(context.control_epoch)||!CinemaRemoteWire.positive(context.context_revision)||!CinemaRemoteWire.positive(context.focus_revision)||(context.text_nonce!=null&&!CinemaRemoteWire.id(context.text_nonce))){this.deactivate();return "invalid";}
    context={...context,target:{...context.target,session_id:context.target.session_id.toLowerCase(),receiver_epoch:context.target.receiver_epoch.toLowerCase()},grant_id:context.grant_id.toLowerCase(),control_epoch:context.control_epoch.toLowerCase(),text_nonce:context.text_nonce?.toLowerCase()||null};
    const old=this.context,epoch=!old||CinemaRemoteWire.targetKey(old.target)!==CinemaRemoteWire.targetKey(context.target)||old.control_epoch!==context.control_epoch;
    if(!epoch&&(!this.active||old.grant_id!==context.grant_id)){this.deactivate();return "invalid";}
    if(epoch){this.lastSequence=0;this.results=[];}
    if(epoch||old.context_revision!==context.context_revision||old.text_nonce!==context.text_nonce)this.invalidate();
    this.context=context;this.active=true;return "applied";
  }
  observe(now){
    if(!CinemaRemoteWire.unsigned(now)||(this.lastNow!==null&&now<this.lastNow)){this.invalidate();return false;}
    this.lastNow=now;this.credits=this.credits.filter(value=>now<value.deadline);this.results=this.results.filter(value=>now<value.deadline);return true;
  }
  mint(kind,nonce,now){
    if(typeof nonce==="string")nonce=nonce.toLowerCase();
    if(!this.observe(now)||!this.active||!CinemaRemoteWire.id(nonce)||!["interaction","playback"].includes(kind)||this.credits.some(value=>value.nonce===nonce))return null;
    const deadline=now+(kind==="interaction"?1000:3000);if(!Number.isSafeInteger(deadline))return null;
    const credit={nonce:nonce.toLowerCase(),kind,deadline};this.credits.push(credit);if(this.credits.length>16)this.credits.shift();return {nonce,kind};
  }
  result(epoch,sequence,now){if(!this.observe(now))return null;return this.results.find(value=>value.control_epoch===epoch&&value.sequence===sequence)||null;}
  apply(input,now,semantic,effect){
    if(!this.observe(now))return "invalid";let command;try{command=CinemaRemoteWire.decode(input);}catch(_){return "invalid";}
    if(!this.active||!this.context)return "unavailable";const current=this.context;
    if(CinemaRemoteWire.targetKey(command.target)!==CinemaRemoteWire.targetKey(current.target))return "stale_target";
    if(command.grant_id!==current.grant_id)return "unauthorized";
    if(command.control_epoch!==current.control_epoch)return "stale_control";
    if(command.sequence<=this.lastSequence)return "duplicate_or_old";
    const credit=this.credits.find(value=>value.nonce===command.credit);if(!credit)return "expired";
    if(credit.kind!==CinemaRemoteWire.creditKind(command.action))return "invalid";
    if(command.context_revision!==current.context_revision)return "stale_context";
    if(command.action.type==="select"&&command.focus_revision!==current.focus_revision)return "stale_focus";
    if(command.action.type==="text_replace"&&command.action.text_nonce!==current.text_nonce)return "stale_context";
    if(semantic!==null)return semantic==="applied"?"invalid":semantic;
    if(!Number.isSafeInteger(now+10000))return "invalid";
    this.lastSequence=command.sequence;let outcome;
    try{outcome=effect(command.action);}catch(_){outcome="unavailable";}
    this.results.push({control_epoch:command.control_epoch,sequence:command.sequence,outcome,deadline:now+10000});if(this.results.length>64)this.results.shift();return outcome;
  }
}
