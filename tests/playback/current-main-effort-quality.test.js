"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const fs = require("node:fs");
const vm = require("node:vm");

test("a05 qualified full-output sidecar is required for positive candidate margin", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const begin=source.indexOf("function measuredCandidateOutput(");
  const end=source.indexOf("\nasync function ",begin+1);
  const context=vm.createContext({PlaybackPolicy:policy});
  vm.runInContext(source.slice(begin,end),context);
  const candidate={id:"a".repeat(32),recipe_digest:Array(32).fill(4),route:"encode",width:1920,height:1080,
    target_height:1080,decoder_compatible:true,complete_cache:true,sustainable:true,average_bps:1,peak_bps:1};
  const player={measuredCandidateOutputs:null};
  const transfer={bytes:3750000,elapsed_ms:1000,age_ms:0,completed:true,from_cache:false,producer_paced:false};
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
  const sample=context.completedQualityTransfer(response,"/hls/s/seg00001.m4s",{start:90},5100);
  assert.equal(sample.receipt,"nonce"); assert.equal(sample.etag,"etag");
  assert.equal(sample.object_name,"seg00001.m4s"); assert.equal(sample.elapsed_ms,5000);
  timing.transferSize=0;
  assert.equal(context.completedQualityTransfer(response,"/hls/s/seg00001.m4s",{start:90},5100),undefined);
});

test("a05 completed link sender permits only immutable two-stage active attachment claims", () => {
  const source=fs.readFileSync("crates/plurxd/src/web/player/stall-diagnosis.js","utf8");
  const begin=source.indexOf("function reportCandidateLinkSample(");
  const end=source.indexOf("\nasync function ",begin+1);
  const sent=[];
  const player={sessionId:"session",qualityCandidateId:"candidate",mediaAttachment:{},started:true,waitAt:1,
    abr:{qualityTransfer:{receipt:"nonce",etag:"etag",object_name:"seg00001.m4s",bytes:4096,elapsed_ms:5000,
      atMs:100,completed:true,from_cache:false,producer_paced:false,session_id:"session",candidate_id:"candidate",media_duration_ms:4000}}};
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
