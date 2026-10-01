"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const fs = require("node:fs");
const vm = require("node:vm");

test("a05 prepared control sends only its explicit per-call nonce", async () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/prepared-switch-measurement.js","utf8");
  const begin=source.indexOf("async function sendPlaybackControl(");
  const end=source.indexOf("\nfunction ",begin+1);
  const calls=[];
  const context=vm.createContext({TOKEN:"ordinary-token",fetch:async(url,request)=>{
    calls.push(request); return {ok:true,json:async()=>({})}; }});
  vm.runInContext(source.slice(begin,end),context);
  const nonce="00000000-0000-0000-0000-000000000001";
  await context.sendPlaybackControl("/control",{sequence:1},null,nonce);
  await context.sendPlaybackControl("/control",{sequence:2},null);
  await context.sendPlaybackControl("/control",{sequence:3},null,"invalid");
  assert.equal(calls[0].headers["X-Plurx-Link-Receipt"],nonce);
  assert.equal(calls[1].headers["X-Plurx-Link-Receipt"],undefined);
  assert.equal(calls[2].headers["X-Plurx-Link-Receipt"],undefined);
  assert.equal(calls[0].headers.authorization,"Bearer ordinary-token");
  assert.deepEqual(JSON.parse(calls[0].body),{sequence:1},"observation does not enter control JSON");
});

test("a05 staged completed body reports only its own attachment nonce", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/prepared-replacement.js","utf8");
  const begin=source.indexOf("function notePreparedHlsFragmentLoaded(");
  const end=source.indexOf("\nfunction ",begin+1);
  const player={},stage={sessionId:"staged-session"},sent=[];
  let active=stage, proof={receipt:"own-nonce",etag:"own-etag",bytes:4096,elapsed_ms:1000,atMs:100,
    object_name:"seg00001.m4s",server_media_duration_ms:4001};
  let actualBytes;
  const context=vm.createContext({URL,location:{href:"http://server/"},PlaybackPolicy:{qualityTransferBps:()=>1000},
    performance:{now:()=>200},preparedState:()=>active,attachedPreparedHls:()=>false,
    candidateTransferOriginCurrent:()=>true,completedQualityTransfer:(response,url,loading,now,bytes)=>{
      actualBytes=bytes; return proof; },clientLog:row=>sent.push(row),notePreparedBuffer:()=>{}});
  vm.runInContext(source.slice(begin,end),context);
  const data={frag:{type:"main",url:"/hls/staged-session/seg00001.m4s",duration:999,
    stats:{loaded:4096,loading:{start:0,end:100}}},networkDetails:{}};
  context.notePreparedHlsFragmentLoaded(player,stage,data);
  context.notePreparedHlsFragmentLoaded(player,stage,data);
  assert.equal(sent.length,1,"nonce raw claim is bounded and idempotent");
  assert.equal(actualBytes,4096,"completed loader actual body bytes are passed");
  assert.equal(sent[0].session_id,"staged-session");
  assert.equal(sent[0].link_sample.media_duration_ms,4001,"server duration, not guessed fragment duration");
  assert.equal(sent[0].link_sample.presenting,false);
  proof={...proof,receipt:"cross-session"};
  context.notePreparedHlsFragmentLoaded(player,stage,{...data,frag:{...data.frag,url:"/hls/incumbent/seg00001.m4s"}});
  active={sessionId:"replacement"};
  context.notePreparedHlsFragmentLoaded(player,stage,data);
  assert.equal(sent.length,1,"other session or retired attachment cannot lend completion");
});

test("a05 completed response join refuses ambiguous bodies and preserves EOF age", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/player.js","utf8");
  const start=source.indexOf("function completedQualityTransfer(");
  const end=source.indexOf("\nfunction ",start+1);
  const timing={encodedBodySize:4096,transferSize:4500,responseStart:100,responseEnd:5100,startTime:90};
  let entries=[timing];
  const context=vm.createContext({URL,location:{href:"http://server/"},
    performance:{getEntriesByName:()=>entries}});
  vm.runInContext(source.slice(start,end),context);
  const response={status:200,getResponseHeader:name=>({"X-Plurx-Producer-Paced":"0",
    "X-Plurx-Link-Receipt":"this-response",ETag:"this-etag"})[name]};
  const read=(now=5200,loading={start:90,end:5100},bytes=4096)=>
    context.completedQualityTransfer(response,"/hls/session/seg00001.m4s",loading,now,bytes);
  const sample=read();
  assert.equal(sample.atMs,5100,"callback delay cannot renew the response EOF");
  assert.equal(sample.receipt,"this-response");
  assert.equal(sample.etag,"this-etag");
  entries=[timing,{...timing,startTime:6000,responseStart:6010,responseEnd:7000}];
  assert.equal(read().atMs,5100,"latest same-URL response is not borrowed");
  entries=[timing,{...timing}];
  assert.equal(read(),undefined,"two matching responses are ambiguous");
  entries=[timing];
  assert.equal(read(20101),undefined,"age is measured from original EOF");
  assert.equal(read(5099),undefined,"future EOF cannot be accepted");
  assert.equal(read(5200,{start:90}),undefined,"missing completion interval is Unknown");
  assert.equal(read(5200,{start:90,end:5110}),undefined,"coarse unmatched interval is Unknown");
  assert.equal(read(5200,{start:90,end:5100},4095),undefined,"body size must identify this response");
});

test("a05 changed delivery origin cannot lend positive Link evidence", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const start=source.indexOf("function candidateTransferEvidence(");
  const end=source.indexOf("function measuredCandidateOutput(",start);
  const player={fileId:7,sessionId:"session",qualityCandidateId:"candidate",mediaAttachment:{},abr:{}};
  player.abr.qualityTransfer={bytes:4096,elapsed_ms:1000,atMs:100,completed:true,from_cache:false,
    producer_paced:false,attachment:player.mediaAttachment,session_id:"session",candidate_id:"candidate",
    receipt:"00000000-0000-0000-0000-000000000001",etag:"etag",linkPositiveReported:true,origin:"http://server"};
  const context=vm.createContext({URL,location:{href:"http://server/"},PLAYER:player,PlaybackPolicy:policy,
    playbackOwnsAttachedMedia:()=>true,qualityForce:()=>"auto",performance:{now:()=>200,
      getEntriesByName:()=>[{encodedBodySize:4096,transferSize:4500,responseStart:100,responseEnd:5100,startTime:90}]}});
  vm.runInContext(source.slice(start,end),context);
  assert.equal(context.candidateLinkReceipt(player,7),player.abr.qualityTransfer.receipt);
  player.abr.qualityTransfer.origin="http://another-serving-origin";
  assert.equal(context.candidateLinkReceipt(player,7),null);
  player.abr.qualityTransfer.origin=null;
  assert.equal(context.candidateLinkReceipt(player,7),null,"unknown delivery origin cannot be promoted");
  const playerSource=fs.readFileSync("crates/plurxd/src/web/player/player.js","utf8");
  const first=playerSource.indexOf("function completedQualityTransfer(");
  const last=playerSource.indexOf("\nfunction ",first+1);
  vm.runInContext(playerSource.slice(first,last),context);
  const response={status:200,getResponseHeader:name=>({"X-Plurx-Producer-Paced":"0",
    "X-Plurx-Link-Receipt":"00000000-0000-0000-0000-000000000001",ETag:"etag"})[name]};
  assert.equal(context.completedQualityTransfer(response,"http://another-serving-origin/hls/session/seg00001.m4s",{start:90,end:5100},5100,4096),undefined);
  assert.equal(context.completedQualityTransfer(response,"/hls/session/seg00001.m4s",{start:90,end:5100},5100,4096).origin,"http://server");
});

test("a05 Link stall echoes only immutable server body duration", () => {
  const playerSource=fs.readFileSync("crates/plurxd/src/web/player/player.js","utf8");
  const start=playerSource.indexOf("function completedQualityTransfer(");
  const finish=playerSource.indexOf("\nfunction ",start+1);
  const stallSource=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const report=stallSource.indexOf("function reportCandidateLinkSample(");
  const reportEnd=stallSource.indexOf("\nasync function ",report+1);
  const sent=[],player={sessionId:"session",qualityCandidateId:"candidate",mediaAttachment:{},started:true,waitAt:1,abr:{}};
  let observed="4001";
  const response={status:200,getResponseHeader:name=>({"X-Plurx-Producer-Paced":"0",
    "X-Plurx-Link-Receipt":"00000000-0000-0000-0000-000000000001",ETag:"etag",
    "X-Plurx-Link-Media-Duration-Ms":observed})[name]};
  const context=vm.createContext({URL,location:{href:"http://server/"},PLAYER:player,
    performance:{getEntriesByName:()=>[{encodedBodySize:4096,transferSize:4500,responseStart:100,responseEnd:5100,startTime:90}]},
    playbackOwnsAttachedMedia:()=>true,qualityForce:()=>"auto",bufferRunway:()=>1,clientLog:row=>sent.push(row)});
  vm.runInContext(playerSource.slice(start,finish),context);
  vm.runInContext(stallSource.slice(report,reportEnd),context);
  const install=()=>{
    const sample=context.completedQualityTransfer(response,"/hls/session/seg00001.m4s",{start:90,end:5100},5100,4096);
    player.abr.qualityTransfer={...sample,attachment:player.mediaAttachment,session_id:"session",
      candidate_id:"candidate",media_duration_ms:1};
  };
  install();
  context.reportCandidateLinkSample(player,{paused:false,seeking:false},"unknown",5100);
  context.reportCandidateLinkSample(player,{paused:false,seeking:false},"link",5200);
  assert.equal(sent.length,2);
  assert.equal(sent[1].link_sample.media_duration_ms,4001,"outward-rounded server value, not client duration");
  observed="6000";install();sent.length=0;
  context.reportCandidateLinkSample(player,{paused:false,seeking:false},"unknown",5100);
  context.reportCandidateLinkSample(player,{paused:false,seeking:false},"link",5200);
  assert.equal(sent.length,1,"client duration cannot fabricate slow-body negative");
  observed=null;install();sent.length=0;
  context.reportCandidateLinkSample(player,{paused:false,seeking:false},"unknown",5100);
  context.reportCandidateLinkSample(player,{paused:false,seeking:false},"link",5200);
  assert.equal(sent.length,1,"missing server duration remains Unknown for negatives");
});

test("a05 request header uses only this installed incumbent nonce", async () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const begin=source.indexOf("function candidateTransferEvidence(");
  const end=source.indexOf("function measuredCandidateOutput(",begin);
  const requests=[];
  const player={fileId:7,sessionId:"session",qualityCandidateId:"candidate",mediaAttachment:{},abr:{}};
  player.abr.qualityTransfer={bytes:4096,elapsed_ms:1000,atMs:100,completed:true,from_cache:false,
    producer_paced:false,attachment:player.mediaAttachment,session_id:"session",candidate_id:"candidate",
    receipt:"00000000-0000-0000-0000-000000000001",etag:"etag",linkPositiveReported:true,origin:"http://server"};
  const context=vm.createContext({URL,location:{href:"http://server/"},PLAYER:player,PlaybackPolicy:policy,playbackOwnsAttachedMedia:()=>true,
    qualityForce:()=>"auto",window:{},performance:{now:()=>200},AUTH_GENERATION:1,TOKEN:"token",API:"/api/v1",
    fetch:async (path,options)=>{requests.push(options);return {status:200,ok:true,headers:{get:()=>null},json:async()=>({})};}});
  vm.runInContext(source.slice(begin,end),context);
  vm.runInContext(fs.readFileSync("crates/plurxd/src/web/core/api.js","utf8"),context);
  const nonce=context.candidateLinkReceipt(player,7);
  assert.equal(nonce,player.abr.qualityTransfer.receipt);
  await context.api("/files/7/decision",{linkReceipt:nonce});
  assert.equal(requests.at(-1).headers["x-plurx-link-receipt"],nonce);
  assert.equal(context.candidateLinkReceipt(null,7),null,"detail preflight has no incumbent");
  assert.equal(context.candidateLinkReceipt(player,8),null,"another source is not this attachment");
  player.mediaAttachment={};
  const retired=context.candidateLinkReceipt(player,7);
  assert.equal(retired,null);
  await context.api("/files/7/decision",{linkReceipt:retired});
  assert.equal(requests.at(-1).headers["x-plurx-link-receipt"],undefined);
  player.mediaAttachment=player.abr.qualityTransfer.attachment;
  player.qualityCandidateId="successor";
  assert.equal(context.candidateLinkReceipt(player,7),null);
  player.qualityCandidateId="candidate";
  assert.equal(context.candidateLinkReceipt(player,7,15101),null,"original sample expires");
  await context.api("/files/7/decision",{linkReceipt:nonce+"\nspoof"});
  assert.equal(requests.at(-1).headers["x-plurx-link-receipt"],undefined);
});

test("a05 positive margin refuses bare metrics and retired candidate attachment", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const begin=source.indexOf("function candidateTransferOriginCurrent(");
  const end=source.indexOf("\nasync function ",begin+1);
  const context=vm.createContext({URL,location:{href:"http://server/"},PlaybackPolicy:policy}); vm.runInContext(source.slice(begin,end),context);
  const candidate={id:"a".repeat(32),recipe_digest:Array(32).fill(4),route:"encode"};
  const output={candidate_id:candidate.id,recipe_digest:[...candidate.recipe_digest],route:"encode",
    artifact_id:"00000000-0000-0000-0000-000000000001",output_identity:"b".repeat(64),
    qualification:"complete_full_mux_rfc8216_v1",average_bps:12000000,peak_bps:14000000};
  const player={sessionId:"session",qualityCandidateId:candidate.id,mediaAttachment:{},measuredCandidateOutputs:[output]};
  const transfer={bytes:3750000,elapsed_ms:1000,age_ms:0,completed:true,from_cache:false,producer_paced:false,
    attachment:player.mediaAttachment,session_id:player.sessionId,candidate_id:player.qualityCandidateId,origin:"http://server"};
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false);
  transfer.receipt="00000000-0000-0000-0000-000000000001"; transfer.etag="etag";
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),true);
  player.qualityCandidateId="c".repeat(32);
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false);
  player.qualityCandidateId=candidate.id; player.mediaAttachment={};
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false);
});

test("a05 qualified full-output sidecar is required for positive candidate margin", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const begin=source.indexOf("function candidateTransferOriginCurrent(");
  const end=source.indexOf("\nasync function ",begin+1);
  const context=vm.createContext({URL,location:{href:"http://server/"},PlaybackPolicy:policy});
  vm.runInContext(source.slice(begin,end),context);
  const candidate={id:"a".repeat(32),recipe_digest:Array(32).fill(4),route:"encode",width:1920,height:1080,
    target_height:1080,decoder_compatible:true,complete_cache:true,sustainable:true,average_bps:1,peak_bps:1};
  const player={measuredCandidateOutputs:null,sessionId:"session",qualityCandidateId:candidate.id,mediaAttachment:{}};
  const transfer={bytes:3750000,elapsed_ms:1000,age_ms:0,completed:true,from_cache:false,producer_paced:false,
    receipt:"00000000-0000-0000-0000-000000000001",etag:"etag",attachment:player.mediaAttachment,
    session_id:player.sessionId,candidate_id:player.qualityCandidateId,origin:"http://server"};
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false,"planned/cache fields are not proof");
  const output={candidate_id:candidate.id,recipe_digest:[...candidate.recipe_digest],route:"encode",
    artifact_id:"00000000-0000-0000-0000-000000000001",output_identity:"b".repeat(64),
    qualification:"complete_full_mux_rfc8216_v1",average_bps:12000000,peak_bps:14000000};
  player.measuredCandidateOutputs=[output];
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),true);
  output.peak_bps=20000000;
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false,"fresh 30M acquisition cannot prove 1.8x20M margin");
  output.peak_bps=14000000; output.recipe_digest[0]=5;
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false,"full recipe mismatch");
  output.recipe_digest[0]=4; output.qualification="planned";
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false);
  output.qualification="complete_full_mux_rfc8216_v1";
  player.measuredCandidateOutputs=[output,{...output}];
  assert.equal(context.candidatePositiveMargin(player,candidate,transfer),false,"ambiguous duplicate descriptor");
});

test("a05 genuine completed body carries the response receipt without cache promotion", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/player.js","utf8");
  const begin=source.indexOf("function completedQualityTransfer(");
  const end=source.indexOf("\nfunction ",begin+1);
  const timing={encodedBodySize:4096,transferSize:4500,responseStart:100,responseEnd:5100,startTime:90};
  const context=vm.createContext({URL,location:{href:"http://server/"},performance:{getEntriesByName:()=>[timing]}});
  vm.runInContext(source.slice(begin,end),context);
  const response={status:200,getResponseHeader:name=>({"X-Plurx-Producer-Paced":"0","X-Plurx-Link-Receipt":"nonce",ETag:"etag"})[name]};
  const sample=context.completedQualityTransfer(response,"/hls/s/seg00001.m4s",{start:90,end:5100},5100,4096);
  assert.equal(sample.receipt,"nonce"); assert.equal(sample.etag,"etag");
  assert.equal(sample.object_name,"seg00001.m4s"); assert.equal(sample.elapsed_ms,5000);
  timing.transferSize=0;
  assert.equal(context.completedQualityTransfer(response,"/hls/s/seg00001.m4s",{start:90,end:5100},5100,4096),undefined);
});

test("a05 completed link sender permits only immutable two-stage active attachment claims", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const begin=source.indexOf("function reportCandidateLinkSample(");
  const end=source.indexOf("\nasync function ",begin+1);
  const sent=[];
  const player={sessionId:"session",qualityCandidateId:"candidate",mediaAttachment:{},started:true,waitAt:1,
    abr:{qualityTransfer:{receipt:"nonce",etag:"etag",object_name:"seg00001.m4s",bytes:4096,elapsed_ms:5000,
      atMs:100,completed:true,from_cache:false,producer_paced:false,session_id:"session",candidate_id:"candidate",media_duration_ms:4000,server_media_duration_ms:4000}}};
  player.abr.qualityTransfer.attachment=player.mediaAttachment;
  const context=vm.createContext({PLAYER:player,playbackOwnsAttachedMedia:()=>true,qualityForce:()=>"auto",
    bufferRunway:()=>1,clientLog:value=>sent.push(value)});
  vm.runInContext(source.slice(begin,end),context);
  const video={paused:false,seeking:false};
  context.reportCandidateLinkSample(player,video,"encode",200);
  assert.equal(sent.length,0);
  context.reportCandidateLinkSample(player,video,"unknown",200);
  context.reportCandidateLinkSample(player,video,"link",300);
  context.reportCandidateLinkSample(player,video,"link",400);
  assert.equal(sent.length,2); assert.equal(sent[0].link_sample.negative,false);
  assert.equal(sent[1].link_sample.negative,true); assert.equal(sent[1].link_sample.age_ms,200);
  player.abr.qualityTransfer.linkPositiveReported=false;
  player.mediaAttachment={};
  context.reportCandidateLinkSample(player,video,"link",500);
  assert.equal(sent.length,2);
});

test("current main switch budget composes with effort decode-once protection", () => {
  const sample = {
    ladder: [
      {height: 720, total_kbps: 3000, peak_kbps: 4000},
      {height: 1080, total_kbps: 6000, peak_kbps: 8000},
    ],
    currentHeight: 1080,
    decodeStalls: 1,
    causeEvidence: {kind: "decode-failed", ageMs: 0},
    nowMs: 1000,
  };
  const exhausted = policy.decideRung({...sample, switchesThisPlaybackHour: 6});
  assert.equal(exhausted.height, 1080);
  assert.equal(exhausted.reason, "switch-budget");
  assert.equal(exhausted.action, "suppressed");
  const first = policy.decideRung(sample);
  assert.equal(first.height, 720);
  assert.equal(first.action, "switch");
  assert.deepEqual(first.blockedHeights, [1080]);
  const repeated = policy.decideRung({...sample, decodeStepConsumed: true});
  assert.equal(repeated.height, 1080);
  assert.equal(repeated.action, "suppressed");
});

test("synchronous decode adapter consumes actual hourly switch budget", () => {
  const source = fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js", "utf8");
  const begin = source.indexOf("function autoDecodeMediaError(");
  const end = source.indexOf("\nfunction ", begin + 1);
  assert.ok(begin >= 0 && end > begin, "actual shipped adapter exists");
  const now = 4000000;
  let switches = 0;
  const context = vm.createContext({PlaybackPolicy: policy, performance: {now: () => now},
    qualityForce: () => "auto", hasPendingPlaybackOpen: () => false,
    switchAutoRung: () => { switches += 1; return Promise.resolve(); }});
  vm.runInContext(source.slice(begin, end), context);
  const player = {method: "transcode", health: {target_height: 1080},
    ladder: [{height: 720, total_kbps: 3000}, {height: 1080, total_kbps: 6000}],
    abr: {switchBudgetTimes: Array(6).fill(now - 1000), failedHeights: new Set()}};
  assert.equal(context.autoDecodeMediaError(player, {videoHeight: 1080}), false);
  assert.equal(switches, 0);
  assert.equal(player.abr.decodeStepConsumed, undefined);
  player.abr.switchBudgetTimes[0] = now - 3600001;
  assert.equal(context.autoDecodeMediaError(player, {videoHeight: 1080}), true);
  assert.equal(switches, 1);
  assert.equal(player.abr.decodeStepConsumed, true);
});
