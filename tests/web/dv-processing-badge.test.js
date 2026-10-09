"use strict";
const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const path=require('node:path');
const root=path.resolve(__dirname,'../../crates/plurxd/src/web');
const generation='11111111-1111-4111-8111-111111111111';
const source={hdr:'dolby_vision',hdr_format:'Dolby Vision · Profile 7'};
function report(){return {generation,hdr10_enhanced:true,fel_contributed:true,
  applied_operations:['PolynomialReshape','RpuColorConversion','TargetMapping']};}
function setup(){
  const context=vm.createContext({PLAYER:{deliveredRange:'hdr10',effectiveProcessing:report(),
    controlReporter:{bootstrap:{generation}},reasons:[]},displayIsHdr:()=>true,renderPlayerInfo:()=>{},console});
  vm.runInContext(fs.readFileSync(path.join(root,'detail/helpers.js'),'utf8'),context);
  vm.runInContext(fs.readFileSync(path.join(root,'detail/dynamic-range.js'),'utf8'),context);
  return context;
}
function badge(context){return context.playerRangeBadge(source);}
test('HDR10-E needs the accepted report and matching active generation; FEL stays detail',()=>{
  const context=setup(), result=badge(context);
  assert.equal(result.arrow,'HDR10-E');
  assert.match(result.aria,/Dolby Vision–enhanced HDR10/);
  assert.match(result.aria,/FEL used: yes/);
  context.PLAYER.effectiveProcessing.fel_contributed=false;
  assert.match(badge(context).aria,/FEL used: no/);
  assert.equal(badge(context).arrow,'HDR10-E');
});
test('source/preferences/plans, stale generations and unsupported wire values never award HDR10-E',()=>{
  for(const change of [
    p=>{p.effectiveProcessing=null;p.dolby_vision_hdr_processing=true;},
    p=>{p.controlReporter.bootstrap.generation='22222222-2222-4222-8222-222222222222';},
    p=>{p.controlReporter=null;},p=>{p.deliveredRange='dolby_vision';},p=>{p.deliveredRange='sdr';},
    p=>{p.effectiveProcessing.hdr10_enhanced='true';},p=>{p.effectiveProcessing.fel_contributed=undefined;},
    p=>{p.effectiveProcessing.applied_operations=[];},p=>{p.effectiveProcessing.generation='invalid';},
  ]){const context=setup();change(context.PLAYER);assert.doesNotMatch(badge(context).text,/HDR10-E/);}
});
test('clearing a failed/replaced/fallback/seek receipt restores ordinary HDR10',()=>{
  const context=setup();context.clearEffectiveProcessing(context.PLAYER);
  assert.equal(context.PLAYER.effectiveProcessing,null);
  assert.equal(badge(context).arrow,'HDR10');
});
test('the actual seek boundary clears processing before optional seek machinery',()=>{
  const context=setup();
  const sourceText=fs.readFileSync(path.join(root,'player/transport.js'),'utf8');
  const start=sourceText.indexOf('function beginPlaybackControlSeek(');
  const end=sourceText.indexOf('\nfunction ',start+1);
  vm.runInContext(sourceText.slice(start,end),context);
  // Only presentation is under test; the next unrelated seek helper is absent.
  assert.throws(()=>context.beginPlaybackControlSeek(context.PLAYER,42),/supersedePlaybackControlIntent is not defined/);
  assert.equal(context.PLAYER.effectiveProcessing,null);
  assert.equal(badge(context).arrow,'HDR10');
});

test('accepted current controls adopt late processing and omission clears the same generation',()=>{
  const context=setup();
  Object.assign(context,{window:{},CONTROL_CLIENT_ID:'client',
    stopPlaybackControl:()=>{},playbackOwnsAttachedMedia:()=>true,
    continueStoppingPlaybackControl:()=>false,settlePlaybackControlWaiters:()=>{},
    settlePlaybackControlAcknowledgement:()=>{}});
  class Reporter {
    constructor(options){this.bootstrap=options.bootstrap;this.exchange=options.onExchange;}
    start(){}
  }
  context.window.PlurxPlaybackControl={Reporter};
  context.PlurxPlaybackControl=context.window.PlurxPlaybackControl;
  const sourceText=fs.readFileSync(path.join(root,'player/directed-change.js'),'utf8');
  const start=sourceText.indexOf('function startPlaybackControl(');
  const end=sourceText.indexOf('async function openSession(',start);
  vm.runInContext(sourceText.slice(start,end),context);
  const p=context.PLAYER;
  p.effectiveProcessing=null;
  const reporter=context.startPlaybackControl(null,p,{generation});
  assert.ok(reporter);
  const exchange=response=>reporter.exchange({request:{},response,capture:{owner:p.controlOwner,intentGeneration:0}});
  exchange({generation,effective_processing:report()});
  assert.equal(badge(context).arrow,'HDR10-E');
  exchange({generation});
  assert.equal(p.effectiveProcessing,null);
  exchange({generation,effective_processing:report()});
  p.controlIntentGeneration=1;
  exchange({generation});
  assert.equal(badge(context).arrow,'HDR10-E','obsolete intent cannot clear current authority');
  p.controlIntentGeneration=0;
  exchange({generation,effective_processing:{...report(),generation:'22222222-2222-4222-8222-222222222222'}});
  assert.equal(p.effectiveProcessing,null);
});
