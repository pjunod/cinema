"use strict";
const assert=require("node:assert/strict");
const {test}=require("node:test");
const {shellSource}=require("./shell-source.js");
const source=shellSource().bodyScript;
function shipped(name){
  const start=source.search(new RegExp(`\\n(?:async )?function ${name}\\(`));
  assert.ok(start>=0,name);
  const tail=source.slice(start+1),end=tail.slice(1).search(/\n(?:async )?function /);
  return end<0?tail:tail.slice(0,end+1);
}
function harness(api){
  return new Function("api",`
    let AUTH_GENERATION=1,ANALYSIS_RECONCILE;
    const location={hash:"#/analysis"},ANALYSIS_SNAPSHOT={},PAGE_RENDER_GENERATION=1;
    const paintAnalysis=()=>{},renderAnalysis=async()=>{};
    ${["esc","analysisNodeDetail","nodeLabel","analysisReconcileReason","analysisReconcileHtml","resetAnalysisReconciliation","repaintAnalysisReconciliation","setAnalysisReconcileReassign","previewAnalysisReconciliation","applyAnalysisReconciliation","analysisErrorInfo"].map(shipped).join("\n")}
    resetAnalysisReconciliation();
    return {preview:previewAnalysisReconciliation,apply:applyAnalysisReconciliation,
      html:analysisReconcileHtml,option:setAnalysisReconcileReassign,errorInfo:analysisErrorInfo,
      state:()=>ANALYSIS_RECONCILE,logout:()=>{AUTH_GENERATION++;resetAnalysisReconciliation();}};
  `)(api);
}
const rows=(count,start=0)=>Array.from({length:count},(_,i)=>({request_id:`r${start+i}`,candidate_id:`hash${start+i}`,title:'<img src=x onerror="bad()">',reason:"engine_changed",eligible:true,target_node_id:"a",replacement_node_id:"b"}));

test("reconciliation previews the full backlog and applies only exact previewed selections",async()=>{
  const calls=[];
  const h=harness(async(url,{body})=>{
    assert.equal(url,"/analysis/reconcile");calls.push(body);
    if(body.dry_run)return body.cursor?{candidates:rows(37,100),next_cursor:null}:{candidates:rows(100),next_cursor:"r99"};
    return {results:body.candidates.map(({request_id},i)=>({request_id,status:i===0?"changed":"reconciled"}))};
  });
  await h.preview();
  assert.equal(h.state().scanned,137);assert.equal(h.state().complete,true);
  assert.deepEqual(calls.map(x=>x.cursor),["","r99"]);
  assert.match(h.html(),/Apply 137 repairs/);assert.doesNotMatch(h.html(),/<img/);
  await h.apply();
  assert.deepEqual(calls.slice(2).map(x=>x.candidates.length),[100,37]);
  assert.deepEqual(calls[3].candidates[0],{request_id:"r100",candidate_id:"hash100"});
  assert.deepEqual(h.state().result,{reconciled:135,changed:2});
  await h.apply();assert.equal(calls.length,4,"a completed preview cannot be applied twice");
});

test("an incomplete or repeated preview cannot enable Apply",async()=>{
  let calls=0;
  const h=harness(async()=>{calls++;return {candidates:rows(1),next_cursor:"repeat"};});
  await h.preview();await h.apply();
  assert.equal(calls,2);assert.equal(h.state().complete,false);
  assert.match(h.state().error,/repeated/);assert.match(h.html(),/button disabled onclick="apply/);
});

test("a partial apply retains acknowledged progress and requires a fresh preview",async()=>{
  let applies=0;
  const h=harness(async(url,{body})=>{
    if(body.dry_run)return {candidates:rows(101)};
    if(++applies===2)throw Error("connection lost");
    return {results:body.candidates.map(x=>({...x,status:"reconciled"}))};
  });
  await h.preview();await h.apply();await h.apply();
  assert.equal(applies,2);assert.equal(h.state().result.reconciled,100);
  assert.match(h.html(),/connection lost/);assert.equal(h.state().complete,false);
});

test("changing target policy invalidates the preview",async()=>{
  const h=harness(async()=>({candidates:rows(2)}));
  await h.preview();h.option(true);
  assert.equal(h.state().complete,false);assert.equal(h.state().rows.length,0);
});

test("sign-out stops batch repair before it can use another session",async()=>{
  let applies=0,h;
  h=harness(async(url,{body})=>{
    if(body.dry_run)return {candidates:rows(101)};
    applies++;h.logout();
    return {results:body.candidates.map(x=>({...x,status:"reconciled"}))};
  });
  await h.preview();await h.apply();
  assert.equal(applies,1);assert.equal(h.state().open,false);assert.equal(h.state().rows.length,0);
});

test("a retained deferral does not assert that playback is active",()=>{
  const info=harness(async()=>{}).errorInfo("foreground_preempted");
  assert.equal(info.title,"Analysis deferred");
  assert.match(JSON.stringify(info),/saved interruption reason/);
  assert.doesNotMatch(JSON.stringify(info),/Paused for playback/);
});
