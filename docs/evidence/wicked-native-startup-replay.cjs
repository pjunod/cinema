const fs=require('node:fs');
const vm=require('node:vm');
const assert=require('node:assert/strict');
const path=require('node:path');
if(!process.argv[2]) throw Error('Pass the extracted incident source directory');
const base=path.resolve(process.argv[2],'crates/plurxd/src/web')+'/';
const crypto=require('node:crypto');
const expectedHashes={"index.html": "451192ab60d71eb73a6b863ea1b7bee40ad9fdc3ccf1994f07e73c8ecc0e731d", "playback-policy.js": "2d8aeca162eb58c0bf3739dc49719ef1fc26000d30084e127ac99cac28fb5013"};
for(const [name,hash] of Object.entries(expectedHashes)) {
  assert.equal(crypto.createHash('sha256').update(fs.readFileSync(base+name)).digest('hex'),hash,
    'Replay requires the exact incident source: '+name);
}
const html=fs.readFileSync(base+'index.html','utf8');
const source=html.slice(html.indexOf('function wirePlayerMedia(v){'),html.indexOf('function markerNowMs(){'));
const policy=require(base+'playback-policy.js');
function replay(code) {
  const events={},calls=[];
  const v={error:{code,message:''},currentSrc:'http://example.invalid/master.m3u8',
    addEventListener:(name,fn)=>{(events[name]??=[]).push(fn)},getAttribute:()=>'/master.m3u8'};
  const ctx={PLAYER:{method:'remux',triedFallback:false,started:false},PlaybackPolicy:policy,
    document:{getElementById:()=>({})},console:{warn:()=>{}},
    playbackOwnsAttachedMedia:()=>true,notifyPlaybackControl:()=>null,clearStall:()=>{},
    finishStallRecovery:()=>false,playbackIsReal:()=>false,streamRejectionFacts:()=>({}),
    streamRejectionNote:()=>'',streamRejectionMessage:()=>'',streamRejectionReport:(_,x)=>x,
    clientLog:x=>calls.push(['log',x.code]),raisePlaybackSurface:()=>{},
    startTranscodeFallback:x=>calls.push(['transcode',x]),stopPlayerForExhaustion:()=>calls.push(['stop']),
    toast:()=>{},pbTick:()=>{},pbSyncPlayIcon:()=>{},
    fetch:()=>{calls.push(['fetch']);throw Error('unexpected network probe')},
  };
  vm.createContext(ctx);vm.runInContext(source,ctx);ctx.wirePlayerMedia(v);
  events.error[0]();
  return {code,calls,usedFallback:ctx.PLAYER.triedFallback};
}
const network=replay(2),unsupported=replay(4);
assert.equal(network.usedFallback,false);
assert.equal(unsupported.usedFallback,true);
assert.equal(unsupported.calls.some(c=>c[0]==='fetch'),false);
console.log(JSON.stringify({deployed_commit:'c9e4edf451e12247a7aa4188903e5ba36888e7e9',network,unsupported},null,2));
console.log('CONFIRMED: deployed native code 4 immediately transcodes without checking HTTP evidence.');
