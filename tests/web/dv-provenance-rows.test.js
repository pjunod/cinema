"use strict";
// Playback info `Dolby Vision metadata` and `Display target` rows on the web:
// the server writes every sub-row's text, the client pairs it with the
// contract's sub-row labels, and a processed delivery's rows live and die with
// its current, generation-matching effective_processing report.
const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const path=require('node:path');
const root=path.resolve(__dirname,'../../crates/plurxd/src/web');
const fields=require('../playback/playback-info-fields.json').fields;
const generation='11111111-1111-4111-8111-111111111111';

const metadata=(applied,left)=>({
  in_file:{levels:[1,2,5,6,8,9,11,254],cm_version:'4.0',text:'L1 · L2 · L5 · L6 · L8 · L9 · L11 (CM v4.0)'},
  applied:{levels:[],text:applied},
  left_for_display:{levels:null,delivered:'hdr10',text:left}});
const target=(state,mapped)=>({reported_peak:{nits:1000,source:'viewer'},
  reported_peak_text:'1000 nits · your setting',state,mapped_for_text:mapped,
  title_peak_nits:1412,title_peak_text:'1412 nits'});
function report(rows){
  return {generation,hdr10_enhanced:true,fel_contributed:false,creative_trims_applied:true,
    applied_operations:['RpuColorConversion','TargetMapping',{DisplayTargetMapping:{nits:1000}},'CreativeTrimApplication'],
    ...rows};
}
const processed={active:['RPU','DYN'],requires_effective_processing:true,details:[
  {badge:'RPU',active:true,text:'RPU: base reshaping (polynomial and MMR) applied by the server'},
  {badge:'DYN',active:true,text:'DYN: per-shot Dolby display mapping for a 1000-nit display by the server'}]};
const native={active:['DYN'],details:[{badge:'DYN',active:true,text:'DYN: Dolby Vision dynamic metadata carried to the display'}],
  dv_metadata:{in_file:metadata().in_file,applied:{levels:[],text:'None'},
    left_for_display:{levels:[1,2,5,6,8,9,11,254],text:'L1 · L2 · L5 · L6 · L8 · L9 · L11'}}};

function setup(enhancement,effectiveProcessing,deliveredRange='hdr10'){
  const context=vm.createContext({PLAYER:{deliveredRange,effectiveProcessing,
    controlReporter:{bootstrap:{generation}},reasons:[]},displayIsHdr:()=>true,renderPlayerInfo:()=>{},console,
    PLAYBACK_INFO_FIELDS:fields});
  vm.runInContext(fs.readFileSync(path.join(root,'detail/helpers.js'),'utf8'),context);
  vm.runInContext(fs.readFileSync(path.join(root,'detail/dynamic-range.js'),'utf8'),context);
  context.PLAYER.enhancement=context.playbackEnhancementFromWire(enhancement);
  return context;
}
const rows=context=>{const r=context.playerDvRows(context.PLAYER);return {metadata:r.metadata&&{...r.metadata},target:r.target&&{...r.target}};};
const lines=row=>row.note.split('\n');

test('a targeted processed stream shows both rows, sub-rows in contract order and wording',()=>{
  const context=setup(processed,report({dv_metadata:metadata('L1 · L8','None; delivered as HDR10'),
    display_target:target({state:'targeted',mapped_nits:1000},'1000 nits')}));
  const {metadata:dv,target:display}=rows(context);
  assert.equal(dv.value,'L1 · L8');
  assert.deepEqual(lines(dv),['In the file: L1 · L2 · L5 · L6 · L8 · L9 · L11 (CM v4.0)',
    'Applied by Cinema: L1 · L8','Left for the display: None; delivered as HDR10']);
  assert.equal(display.value,'1000 nits');
  assert.deepEqual(lines(display),['Reported peak: 1000 nits · your setting',
    'Mapped for: 1000 nits','Title peak: 1412 nits']);
  // The labels are the contract's, not this client's.
  for(const [id,row] of [['dv_metadata',dv],['display_target',display]]){
    const labels=fields.find(f=>f.id===id).sub_rows;
    assert.deepEqual(lines(row).map(l=>l.slice(0,l.indexOf(':'))),labels);
  }
});

test('a stream that is not targeted says why in words',()=>{
  const reason='your display did not report a supported HDR peak (400–4999 nits), so the master grade is sent';
  const context=setup(processed,report({dv_metadata:metadata('None','None; delivered as HDR10'),
    display_target:{...target({state:'not_targeted',reason},`Not targeted — ${reason}`),
      reported_peak:null,reported_peak_text:'Not reported'}}));
  const {target:display}=rows(context);
  assert.equal(display.value,`Not targeted — ${reason}`);
  assert.equal(lines(display)[0],'Reported peak: Not reported');
});

test('a title that already fits says so rather than not targeted',()=>{
  const context=setup(processed,report({dv_metadata:metadata('None','None; delivered as HDR10'),
    display_target:target({state:'already_fits',bucket_nits:1000},'Not needed — the title already fits a 1000-nit display')}));
  assert.equal(rows(context).target.value,'Not needed — the title already fits a 1000-nit display');
});

test('dropping the report (seek, fallback, replacement) clears both processed rows',()=>{
  const context=setup(processed,report({dv_metadata:metadata('L1 · L8','None; delivered as HDR10'),
    display_target:target({state:'targeted',mapped_nits:1000},'1000 nits')}));
  assert.ok(rows(context).metadata&&rows(context).target);
  context.clearEffectiveProcessing(context.PLAYER);
  assert.deepEqual(rows(context),{metadata:null,target:null});
  // A report for another generation is no report at all.
  const stale=setup(processed,{...report({dv_metadata:metadata('L1','None; delivered as HDR10')}),
    generation:'22222222-2222-4222-8222-222222222222'});
  assert.deepEqual(rows(stale),{metadata:null,target:null});
});

test('an unprocessed delivery carries its metadata row in the enhancement and no display target',()=>{
  const context=setup(native,null,'dolby_vision');
  const {metadata:dv,target:display}=rows(context);
  assert.equal(dv.value,'None');
  assert.equal(lines(dv)[2],'Left for the display: L1 · L2 · L5 · L6 · L8 · L9 · L11');
  assert.equal(display,null);
});

test('out-of-shape server text is not shown',()=>{
  for(const text of ['','x'.repeat(513),'bad\ntext',7]){
    const context=setup(processed,report({dv_metadata:metadata(text,'None; delivered as HDR10')}));
    assert.equal(rows(context).metadata,null,JSON.stringify(text));
  }
  assert.deepEqual(rows(setup(null,null)),{metadata:null,target:null});
});

test('the stats panel lists both rows in details and debug only',()=>{
  for(const id of ['dv_metadata','display_target']){
    const field=fields.find(f=>f.id===id);
    assert.deepEqual(field.modes,['details','debug']);
    assert.ok(field.available_on.includes('web'));
  }
  const stats=fs.readFileSync(path.join(root,'player/stats.js'),'utf8');
  assert.match(stats,/dv_metadata:dvRows\.metadata\?\.value\?\?null,dv_metadata_note:dvRows\.metadata\?\.note\?\?null/);
  assert.match(stats,/display_target:dvRows\.target\?\.value\?\?null,display_target_note:dvRows\.target\?\.note\?\?null/);
});
