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
    let AUTH_GENERATION=1,ANALYSIS_COVERAGE={open:true,busy:false,report:null,error:""};
    const location={hash:"#/analysis"},ANALYSIS_SNAPSHOT={node_hostnames:{n1:"nynuc"}};
    const paintAnalysis=()=>{};
    ${["esc","fmtBytes","analysisNodeDetail","nodeLabel","repaintAnalysisReconciliation","analysisCoverageClassLabel","analysisCoverageHtml","toggleAnalysisCoverage","measureAnalysisCoverage"].map(shipped).join("\n")}
    return {measure:measureAnalysisCoverage,html:analysisCoverageHtml,toggle:toggleAnalysisCoverage,
      state:()=>ANALYSIS_COVERAGE,logout:()=>{AUTH_GENERATION++;ANALYSIS_COVERAGE={open:false,busy:false,report:null,error:""};}};
  `)(api);
}
const counts=(files,extra={})=>({files,bytes:files*1024,identities:files*2,identities_indexed:files,indexed:0,partial:files,pending:0,refused:0,unsupported:0,unobserved:0,...extra});

test("coverage renders every class and the total from one on-demand read",async()=>{
  const calls=[];
  const h=harness(async url=>{calls.push(url);return {node_id:"n1",elapsed_ms:1500,
    classes:{dolby_vision_p7:counts(3),sdr:counts(2,{indexed:2,partial:0})},total:counts(5,{unobserved:1})};});
  assert.doesNotMatch(h.html(),/<table/,"nothing is measured until asked");
  await h.measure();
  assert.deepEqual(calls,["/analysis/coverage"]);
  const html=h.html();
  assert.match(html,/Dolby Vision P7<\/td><td>3<\/td>/);
  assert.match(html,/SDR<\/td><td>2<\/td>/);
  assert.match(html,/All<\/td><td>5<\/td>/);
  assert.match(html,/3 \/ 6/);
  assert.match(html,/nynuc/);
  assert.match(html,/Unobserved files could not be checked/);
  assert.match(html,/Measure again/);
});

test("a refused or failed measurement is shown, not swallowed, and Measure stays usable",async()=>{
  const h=harness(async()=>{throw Error("an index coverage measurement is already running on this node");});
  await h.measure();
  assert.equal(h.state().busy,false);
  assert.match(h.html(),/role="alert">an index coverage measurement is already running/);
  assert.doesNotMatch(h.html(),/button class="ghost" disabled/);
});

test("a measurement that outlives the session does not paint into the next one",async()=>{
  let release;
  const h=harness(()=>new Promise(resolve=>{release=resolve;}));
  const pending=h.measure();
  assert.match(h.html(),/Measuring…/);
  h.logout();
  release({node_id:"n1",elapsed_ms:1,classes:{},total:counts(0)});
  await pending;
  assert.equal(h.state().report,null);
});
