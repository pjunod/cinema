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
    let AUTH_GENERATION=1,ANALYSIS_COVERAGE;
    const ANALYSIS_COVERAGE_POLL_MS=0;
    const location={hash:"#/analysis"},ANALYSIS_SNAPSHOT={node_hostnames:{n1:"node-one"}};
    const paintAnalysis=()=>{};
    ${["esc","fmtBytes","analysisNodeDetail","nodeLabel","repaintAnalysisReconciliation","analysisCoverageClassLabel","analysisCoverageHtml","resetAnalysisCoverage","followAnalysisCoverage","toggleAnalysisCoverage","measureAnalysisCoverage"].map(shipped).join("\n")}
    resetAnalysisCoverage();
    return {measure:measureAnalysisCoverage,html:analysisCoverageHtml,toggle:toggleAnalysisCoverage,
      state:()=>ANALYSIS_COVERAGE,logout:()=>{AUTH_GENERATION++;resetAnalysisCoverage();}};
  `)(api);
}
const counts=(files,extra={})=>({files,bytes:files*1024,identities:files*2,identities_indexed:files,indexed:0,partial:files,pending:0,refused:0,unsupported:0,unavailable:0,unobserved:0,...extra});
const report={node_id:"n1",elapsed_ms:1500,classes:{dolby_vision_p7:counts(3),sdr:counts(2,{indexed:2,partial:0})},total:counts(5,{unobserved:1,unavailable:2})};

test("opening the panel reads the last result once; nothing is measured until asked",async()=>{
  const calls=[];
  const h=harness(async(url,options)=>{calls.push([url,options&&options.method]);return {running:null,last:null};});
  await h.toggle();
  assert.deepEqual(calls,[["/analysis/coverage",undefined]]);
  assert.match(h.html(),/>Measure</);assert.doesNotMatch(h.html(),/<table/);
});

test("Measure starts a background run, follows its progress, then renders every class and the total",async()=>{
  const calls=[];let polls=0;
  const h=harness(async(url,options)=>{
    calls.push(options&&options.method||"GET");
    if(options&&options.method==="POST")return {started:true,running:{files_done:0,files_total:10},last:null};
    return ++polls<2?{running:{files_done:5,files_total:10},last:null}:{running:null,last:report};
  });
  h.state().open=true;
  const pending=h.measure();
  assert.match(h.html(),/disabled/,"Measure is disabled while a run is in flight");
  await pending;
  assert.deepEqual(calls,["POST","GET","GET"]);
  const html=h.html();
  assert.match(html,/Dolby Vision P7<\/td><td>3<\/td>/);
  assert.match(html,/SDR<\/td><td>2<\/td>/);
  assert.match(html,/All<\/td><td>5<\/td>/);
  assert.match(html,/3 \/ 6/);
  assert.match(html,/node-one/);
  assert.match(html,/Unobserved files did not answer/);
  assert.match(html,/Measure again/);
});

test("a failed run is shown as failed, and the server's text is escaped",async()=>{
  const h=harness(async()=>({running:null,last:{node_id:"n1",error:'<img src=x onerror="bad()">'}}));
  await h.toggle();
  assert.match(h.html(),/The last measurement failed/);
  assert.doesNotMatch(h.html(),/<img/);
});

test("signing out drops the panel, and a request that outlives the session paints nothing into the next one",async()=>{
  let release;
  const h=harness(()=>new Promise((resolve,reject)=>{release=reject;}));
  h.state().open=true;
  const pending=h.measure();
  h.logout();
  const stale=new Error("stale authorization");stale.staleAuth=true;
  release(stale);
  await pending;
  assert.deepEqual(h.state(),{open:false,busy:false,snapshot:null,error:""});
  assert.equal(h.html(),"");
});
