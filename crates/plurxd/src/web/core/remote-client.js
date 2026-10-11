"use strict";
// Header-only same-origin transport. QR/device data cannot select an API host.
function cinemaRemoteIdentity(){
  if(!TOKEN||!ME||!SERVER||typeof SERVER.instance_id!=="string"||!SERVER.instance_id)return null;
  return {origin:location.origin,instance:SERVER.instance_id,user:String(ME.id),token:TOKEN,generation:AUTH_GENERATION};
}
function cinemaRemoteIdentityKey(identity){return identity?JSON.stringify([identity.origin,identity.instance,identity.user]):"";}
function cinemaRemoteSameIdentity(a,b){return !!(a&&b&&cinemaRemoteIdentityKey(a)===cinemaRemoteIdentityKey(b)&&a.token===b.token&&a.generation===b.generation);}
function cinemaRemoteStorageKey(identity){return "cinema.remote.v1:"+cinemaRemoteIdentityKey(identity);}
function cinemaRemoteLoad(identity){
  try{
    const raw=localStorage.getItem(cinemaRemoteStorageKey(identity));if(!raw)return {grants:[]};
    const value=CinemaRemoteWire.parse(raw,65536);
    return value&&Array.isArray(value.grants)&&value.grants.length<=20?value:{grants:[]};
  }catch(_){return {grants:[]};}
}
function cinemaRemoteSave(identity,value){localStorage.setItem(cinemaRemoteStorageKey(identity),JSON.stringify(value));}
function cinemaRemoteForget(identity){if(identity)localStorage.removeItem(cinemaRemoteStorageKey(identity));}
function cinemaRemoteSecret(value){return typeof value==="string"&&/^[A-Za-z0-9_-]{43}$/.test(value);}
function cinemaRemoteNewNonce(){
  const bytes=new Uint8Array(16);crypto.getRandomValues(bytes);bytes[6]=(bytes[6]&15)|64;bytes[8]=(bytes[8]&63)|128;
  const hex=Array.from(bytes,b=>b.toString(16).padStart(2,"0")).join("");return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}
function cinemaRemoteText(value,limit){let text="",count=0;for(const character of String(value||"").replace(/[\u0000-\u001f\u007f-\u009f]/gu," ")){const size=CinemaRemoteWire.bytes(character);if(count+size>limit)break;text+=character;count+=size;}return text;}
class CinemaRemoteClient{
  constructor(identity=cinemaRemoteIdentity(),identityReader=cinemaRemoteIdentity,transport=fetch){
    if(!identity)throw new Error("Sign in to this Cinema server first.");
    this.identity=identity;this.identityReader=identityReader;this.transport=transport;this.generation=1;this.requests=new Set();
  }
  current(){return cinemaRemoteSameIdentity(this.identity,this.identityReader());}
  retire(){this.generation++;for(const request of this.requests)request.abort();this.requests.clear();}
  async request(path,{method="POST",body=null,proof=null,signal=null}={}){
    const get=method==="GET"&&["receivers","grants"].includes(path);
    const del=method==="DELETE"&&/^(receivers|grants)\/[0-9a-f-]{36}$/.test(path)&&CinemaRemoteWire.id(path.split("/")[1]);
    const post=method==="POST"&&["receivers","sessions","presence","poll","ack","state","control","commands","pairing/start","pairing/claim","pairing/approve","pairing/result"].includes(path);
    if(!get&&!del&&!post)throw new Error("invalid remote endpoint");
    if(!this.current())throw new Error("stale_identity");const generation=this.generation;
    const headers={authorization:"Bearer "+this.identity.token};
    if(proof){
      if(!CinemaRemoteWire.exact(proof,["kind","secret"])||!["receiver","grant","pairing"].includes(proof.kind)||!cinemaRemoteSecret(proof.secret))throw new Error("invalid remote proof");
      headers[{receiver:"X-Cinema-Receiver-Secret",grant:"X-Cinema-Grant-Secret",pairing:"X-Cinema-Pairing-Secret"}[proof.kind]]=proof.secret;
    }
    const payload=post?JSON.stringify({...body,version:CinemaRemoteWire.version}):null;
    if(payload&&CinemaRemoteWire.bytes(payload)>(path==="presence"?65536:16384))throw new Error("remote request too large");
    if(payload)headers["content-type"]="application/json";
    const abort=new AbortController();this.requests.add(abort);
    const cancel=()=>abort.abort();if(signal){if(signal.aborted)abort.abort();else signal.addEventListener("abort",cancel,{once:true});}
    const timeout=setTimeout(cancel,25000);
    try{
      const transport=this.transport;const response=await transport(this.identity.origin+"/api/remote/v1/"+path,{method,headers,body:payload,signal:abort.signal,redirect:"error",cache:"no-store",credentials:"same-origin"});
      if(!this.current()||generation!==this.generation)throw new Error("stale_identity");
      const length=Number(response.headers.get("content-length"));if(Number.isFinite(length)&&length>65536)throw new Error("remote response too large");
      if(!response.headers.get("content-type")?.toLowerCase().includes("application/json"))throw new Error("invalid remote response");
      const reader=response.body?.getReader();if(!reader)throw new Error("invalid remote response");let total=0;const chunks=[];
      for(;;){const {done,value}=await reader.read();if(done)break;total+=value.byteLength;if(total>65536){await reader.cancel();throw new Error("remote response too large");}chunks.push(value);}
      const bytes=new Uint8Array(total);let at=0;for(const chunk of chunks){bytes.set(chunk,at);at+=chunk.byteLength;}
      const value=CinemaRemoteWire.parse(new TextDecoder("utf-8",{fatal:true}).decode(bytes),65536);
      if(!this.current()||generation!==this.generation)throw new Error("stale_identity");
      if(value?.version!==CinemaRemoteWire.version)throw new Error("unsupported remote protocol");
      if(!response.ok){throw Object.assign(new Error(cinemaRemoteText(value.message||"Remote request failed",256)),{code:typeof value.code==="string"?value.code:"unavailable",status:response.status});}
      return value;
    }finally{clearTimeout(timeout);this.requests.delete(abort);if(signal)signal.removeEventListener("abort",cancel);}
  }
}
