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
  applied_operations:['PolynomialReshape','LinearNlqResidual','RpuColorConversion','TargetMapping',{DisplayTargetMapping:{nits:1600}}]};}
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

// ---- Dolby Vision enhancement pills (StartResponse.enhancement) ----------
const nativeEnhancement={active:['DYN'],details:[
  {badge:'FEL',active:false,text:'FEL: delivered for the display to decode'},
  {badge:'RPU',active:false,text:'RPU: carried to the display, which applies it'},
  {badge:'DYN',active:true,text:'DYN: L1/L2/L8 dynamic metadata carried to the display'}]};
const processedEnhancement={active:['FEL','RPU','DYN'],requires_effective_processing:true,details:[
  {badge:'FEL',active:true,text:'FEL: full enhancement layer reconstructed into the picture'},
  {badge:'RPU',active:true,text:'RPU: polynomial reshaping applied by the server'},
  {badge:'DYN',active:true,text:'DYN: per-shot Dolby display mapping for a 1600-nit display by the server (L1 only)'}]};
function enhancementSetup(wire){
  const context=setup();
  vm.runInContext(fs.readFileSync(path.join(root,'player/measurements.js'),'utf8')
    .match(/^function esc\(s\)\{.*$/m)[0],context);
  Object.assign(context.PLAYER,{source:{...source,width:3840,height:2160},audio:[],curAudio:0});
  context.PLAYER.enhancement=context.playbackEnhancementFromWire(wire);
  return context;
}
const pills=context=>[...context.playerFactBadges()
  .matchAll(/<span class="vbadge enh"[^>]*>([^<]*)<\/span>/g)].map(m=>m[1]);
test('enhancement pills follow the dynamic-range chip, one per active badge, labels exact',()=>{
  const context=enhancementSetup(processedEnhancement);
  assert.deepEqual(pills(context),['FEL','RPU','DYN']);
  const html=context.playerFactBadges();
  assert.ok(html.indexOf('HDR10-E')<html.indexOf('vbadge enh'),'pills come after the range chip');
  const native=enhancementSetup(nativeEnhancement);
  native.PLAYER.effectiveProcessing=null;
  assert.deepEqual(pills(native),['DYN'],'inactive enhancements get no pill and need no report');
  assert.doesNotMatch(native.playerFactBadges(),/delivered for the display|which applies it/,
    'no reason text in the compact row');
});
test('processed-HDR10 pills need a current, generation-matching effective_processing report',()=>{
  for(const change of [p=>{p.effectiveProcessing=null;},
    p=>{p.controlReporter.bootstrap.generation='22222222-2222-4222-8222-222222222222';},
    p=>{p.deliveredRange='dolby_vision';}]){
    const context=enhancementSetup(processedEnhancement);change(context.PLAYER);
    assert.deepEqual(pills(context),[]);
    assert.equal(context.playerEnhancementRow(context.PLAYER),null,'no enhancement row either');
  }
});
test('clearing the report (seek, fallback, replacement) hides the pills and the row',()=>{
  const context=enhancementSetup(processedEnhancement);
  assert.equal(pills(context).length,3);
  context.clearEffectiveProcessing(context.PLAYER);
  assert.deepEqual(pills(context),[]);
  assert.equal(context.playerEnhancementRow(context.PLAYER),null);
});
test('an empty active list or an absent field renders nothing extra',()=>{
  const empty=enhancementSetup({active:[],details:[{badge:'FEL',active:false,text:'FEL: not in this source'}]});
  assert.doesNotMatch(empty.playerFactBadges(),/vbadge enh/);
  const absent=enhancementSetup(undefined);
  assert.equal(absent.PLAYER.enhancement,null);
  assert.doesNotMatch(absent.playerFactBadges(),/vbadge enh/);
  assert.equal(absent.playerEnhancementRow(absent.PLAYER),null);
  assert.equal(enhancementSetup({active:['FEL']}).PLAYER.enhancement,null,'malformed wire is refused');
});
test('the Enhancement info row lists each details[].text on its own neutral line',()=>{
  const context=enhancementSetup(nativeEnhancement);
  const statsSource=fs.readFileSync(path.join(root,'player/stats.js'),'utf8');
  vm.runInContext(statsSource.slice(statsSource.indexOf('const PLAYBACK_INFO_FIELDS='),
    statsSource.indexOf('function playbackInfoOverview(')),context);
  const row=context.playerEnhancementRow(context.PLAYER);
  const telemetry={enhancement:row.value,enhancement_note:row.note};
  const rendered=context.playbackInfoRows('details',telemetry).find(r=>r.id==='enhancement');
  assert.ok(rendered,'the shared contract row reaches the web ledger');
  assert.equal(rendered.label,'Enhancement');
  assert.equal(rendered.value,'DYN');
  assert.deepEqual(rendered.note.split('\n'),nativeEnhancement.details.map(d=>d.text));
  assert.equal(rendered.tone,'','neutral, never warn/bad');
  assert.match(statsSource,/enhancement_note:enhancement\?enhancement\.note:null/,'telemetry carries the lines');
  const css=fs.readFileSync(path.join(root,'app.css'),'utf8');
  assert.match(css,/\.pi-row small\[data-stats-note="enhancement"\]\{white-space:pre-line\}/);
  assert.match(css,/\.vbadge\.enh\{[^}]*var\(--enhance\)/);
});
test('every session attach replaces the enhancement; prepared swaps adopt the successor own, never the predecessor one',()=>{
  const directed=fs.readFileSync(path.join(root,'player/directed-change.js'),'utf8');
  const start=directed.indexOf('function attachSession(');
  const attach=directed.slice(start,directed.indexOf('\nfunction ',start+1));
  assert.match(attach,/t\.enhancement=playbackEnhancementFromWire\(info\.enhancement\);/);
  assert.match(fs.readFileSync(path.join(root,'player/prepared-replacement.js'),'utf8'),
    /clearEffectiveProcessing\(p\);\n[^]*?p\.enhancement=state\.enhancement\|\|null;\n {2}p\.hls=state\.hls;/);
  assert.match(fs.readFileSync(path.join(root,'player/prepared-switch-measurement.js'),'utf8'),
    /p\.enhancement=predecessor\.enhancement\|\|null;/);
});
test('pills carry an aria-label; unknown-badge detail lines survive but never draw a pill',()=>{
  const wire={active:['FEL','XYZ','DYN'],details:[
    {badge:'FEL',active:true,text:'FEL: full enhancement layer reconstructed into the picture'},
    {badge:'DYN',active:true,text:'DYN: L1/L2/L8 dynamic metadata carried as HDR10+'},
    {badge:'XYZ',active:false,text:'XYZ: something a newer server explains'}]};
  const context=enhancementSetup(wire);
  assert.deepEqual([...context.PLAYER.enhancement.active],['FEL','DYN'],'no pill for an unknown badge');
  assert.deepEqual([...context.PLAYER.enhancement.details.map(d=>d.badge)],['FEL','DYN','XYZ']);
  context.PLAYER.effectiveProcessing=null;
  context.PLAYER.enhancement.requires_effective_processing=false;
  const html=context.playerFactBadges();
  assert.match(html,/class="vbadge enh" title="FEL: full enhancement layer reconstructed into the picture" aria-label="FEL: full enhancement layer reconstructed into the picture">FEL</);
  assert.match(html,/aria-label="DYN: L1\/L2\/L8 dynamic metadata carried as HDR10\+">DYN</);
  assert.doesNotMatch(html,/XYZ/);
  assert.equal(context.playerEnhancementRow(context.PLAYER).note.split('\n').pop(),
    'XYZ: something a newer server explains','the Playback info row keeps the unknown line');
  assert.equal(enhancementSetup({active:[],details:[{badge:7,active:true,text:'x'},
    {badge:'FEL',active:true,text:''}]}).PLAYER.enhancement.details.length,0,'malformed lines still drop');
  const unlabelled=enhancementSetup({active:['RPU'],details:[]});
  unlabelled.PLAYER.enhancement.requires_effective_processing=false;
  assert.match(unlabelled.playerFactBadges(),/aria-label="RPU">RPU</,'falls back to the badge name');
});
test('a prepared successor adopts the enhancement on its action, null when absent',()=>{
  const source_=fs.readFileSync(path.join(root,'player/prepared-replacement.js'),'utf8');
  assert.match(source_,/enhancement:typeof playbackEnhancementFromWire==="function"\?playbackEnhancementFromWire\(action\.enhancement\):null,/);
  const context=enhancementSetup(processedEnhancement);
  const adopted=context.playbackEnhancementFromWire(nativeEnhancement);
  assert.deepEqual([...adopted.active],['DYN']);
  assert.equal(context.playbackEnhancementFromWire(undefined),null,'absent on the successor is null');
  context.PLAYER.enhancement=adopted;
  context.PLAYER.effectiveProcessing=null;
  assert.deepEqual(pills(context),['DYN'],'an unprocessed successor still shows what it carries');
  const gated=enhancementSetup(processedEnhancement);
  gated.clearEffectiveProcessing(gated.PLAYER);
  assert.deepEqual(pills(gated),[],'FEL/RPU wait for a current effective_processing report');
});


test('current display-target mapping operation objects retain the accepted HDR10 report',()=>{
  const context=setup();
  context.PLAYER.effectiveProcessing.applied_operations.push({DisplayTargetMapping:{nits:1600}});
  assert.equal(badge(context).arrow,'HDR10-E');
  for(const operation of [{DisplayTargetMapping:{nits:100}}, {DisplayTargetMapping:{nits:'1600'}}, {DisplayTargetMapping:{nits:1600,other:1}}]) {
    context.PLAYER.effectiveProcessing.applied_operations=[operation];
    assert.equal(context.playerEffectiveProcessing(context.PLAYER),null);
  }
});


test('SDR-E uses only the current typed 100-nit conversion report and clears on seek',()=>{
  const context=setup(); const p=context.PLAYER;
  p.deliveredRange='sdr'; p.effectiveProcessing={...report(),hdr10_enhanced:false,sdr_processed:true,
    applied_operations:['RpuColorConversion','TargetMapping','Bt709Gamma24Conversion',{DisplayTargetMapping:{nits:100}}]};
  assert.equal(badge(context).arrow,'SDR-E'); assert.match(badge(context).aria,/BT.709 gamma 2.4, L1 only/);
  p.effectiveProcessing.creative_trims_applied=true; p.effectiveProcessing.applied_operations.splice(2,0,'CreativeTrimApplication');
  assert.equal(badge(context).arrow,'SDR-E'); assert.match(badge(context).aria,/BT.709 gamma 2.4, creative trims applied/);
  assert.doesNotMatch(badge(context).aria,/L1 only/);
  p.effectiveProcessing.creative_trims_applied=false; p.effectiveProcessing.applied_operations.splice(2,1);
  p.effectiveProcessing.creative_trims_partial=true;
  assert.equal(badge(context).arrow,'SDR-E'); assert.match(badge(context).aria,/BT.709 gamma 2.4, creative trims on some shots/);
  delete p.effectiveProcessing.creative_trims_partial;
  p.effectiveProcessing.applied_operations.pop(); assert.equal(badge(context).arrow,'SDR');
  p.effectiveProcessing.applied_operations.push({DisplayTargetMapping:{nits:100}});
  context.clearEffectiveProcessing(p); assert.equal(badge(context).arrow,'SDR');
});


test('processed enhancement pills follow the current execution window, not the initial snapshot',()=>{
  const context=enhancementSetup({...processedEnhancement,active:[],details:processedEnhancement.details.map(d=>({...d,active:false}))});
  assert.deepEqual(pills(context),['FEL','RPU','DYN']);
  let row=context.playerEnhancementRow(context.PLAYER);
  assert.match(row.note,/polynomial reshaping, RPU color conversion applied by the server/);
  assert.match(row.note,/per-shot Dolby display mapping for a 1600-nit display/);
  assert.doesNotMatch(row.note,/carried as HDR10|HDR10 has no Dolby Vision/);
  context.PLAYER.effectiveProcessing.fel_contributed=false;
  context.PLAYER.effectiveProcessing.applied_operations=['MmrReshape','RpuColorConversion','TargetMapping'];
  assert.deepEqual(pills(context),['RPU'],'inactive FEL and DYN from a later window cannot retain pills');
  row=context.playerEnhancementRow(context.PLAYER);
  assert.match(row.note,/no enhancement-layer detail/);
  assert.match(row.note,/MMR reshaping/);
  assert.match(row.note,/no per-shot Dolby display mapping applied/);
});

test('DYN details distinguish full, partial and absent creative trims on the current report',()=>{
  const context=enhancementSetup(processedEnhancement),p=context.PLAYER;
  assert.match(context.playerEnhancementRow(p).note,/L1 only/);
  p.effectiveProcessing.creative_trims_partial=true;
  assert.match(context.playerEnhancementRow(p).note,/creative trims on some shots/);
  p.effectiveProcessing.applied_operations.push('CreativeTrimApplication');
  const note=context.playerEnhancementRow(p).note;
  assert.match(note,/with the colourist's creative trims/);
  assert.doesNotMatch(note,/some shots|L1 only/);
});

for(const [mode,size] of [['slot','compact'],['slot','wide'],['full','wide']])test(`late processing refreshes the ${mode}/${size} watch facts and open dialog without replacing controls`,()=>{
  const context=enhancementSetup(processedEnhancement),p=context.PLAYER;
  const nodes=[{innerHTML:''},{innerHTML:''},{innerHTML:''}];
  const focused={id:'subtitle-generation-language'},ledger={innerHTML:'retained ledger'},dialog={open:true};
  let writes=0;
  for(const node of nodes){let html='';Object.defineProperty(node,'innerHTML',{get:()=>html,set:value=>{writes++;html=value;}});}
  Object.assign(context,{
    WATCH:{mode,size,accepted:true,page:{item:{kind:'movie',title:'The Beekeeper'},meta:{}}},pbPosSec:()=>0,
    document:{activeElement:focused,
      getElementById:id=>id==='watch-ledger'?ledger:id==='watch-details'?dialog:null,
      querySelectorAll:selector=>{
        assert.equal(selector,'#watch-title [data-watch-facts], #watch-band-title [data-watch-facts], #watch-details [data-watch-facts]');
        return nodes;
      }}
  });
  vm.runInContext(fs.readFileSync(path.join(root,'detail/watch-browser.js'),'utf8'),context);
  vm.runInContext(fs.readFileSync(path.join(root,'player/projection-chrome.js'),'utf8'),context);
  assert.match(context.watchTitleHtml(),/data-watch-facts/,'every title mirror exposes the narrow update mount');
  p.effectiveProcessing=null;
  context.renderPlayerInfo();
  nodes.forEach(node=>assert.doesNotMatch(node.innerHTML,/HDR10-E|vbadge enh/));
  p.effectiveProcessing=report();
  context.renderPlayerInfo();
  nodes.forEach(node=>{
    assert.match(node.innerHTML,/HDR10-E/);
    assert.deepEqual([...node.innerHTML.matchAll(/class="vbadge enh"[^>]*>([^<]+)/g)].map(m=>m[1]),['FEL','RPU','DYN']);
  });
  p.controlReporter.bootstrap.generation='22222222-2222-4222-8222-222222222222';
  context.renderPlayerInfo();
  nodes.forEach(node=>assert.doesNotMatch(node.innerHTML,/HDR10-E|vbadge enh/,'stale generation clears every mirror'));
  p.controlReporter.bootstrap.generation=generation;
  context.renderPlayerInfo();
  const previousWrites=writes;
  context.renderPlayerInfo();
  assert.equal(writes,previousWrites,'unchanged reports do not replace even the badge nodes');
  context.clearEffectiveProcessing(p);
  nodes.forEach(node=>assert.doesNotMatch(node.innerHTML,/HDR10-E|vbadge enh/));
  assert.equal(context.document.activeElement,focused);
  assert.equal(ledger.innerHTML,'retained ledger');
  assert.equal(dialog.open,true);
});
