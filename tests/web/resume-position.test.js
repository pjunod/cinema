"use strict";
const {test}=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),vm=require("node:vm");
const source=fs.readFileSync("crates/plurxd/src/web/detail/preplay-selection.js","utf8");
function declaration(name){
  const at=source.indexOf(`function ${name}(`);
  assert.ok(at>=0,`missing shipped ${name}`);
  return source.slice(at,source.indexOf("\n}",at)+2);
}
function harness(){
  const context=vm.createContext({
    playbackFileContextForFile:file=>({source_ref:{file_id:file.id}}),
    playerMeta:item=>({title:item.title}),ME:null,
  });
  vm.runInContext(declaration("playbackResumeMs")+"\n"+declaration("itemPageModel"),context);
  return context;
}
function page(watch,overrides={}){
  return {item:{id:93,library_id:1,kind:"movie",title:"Film",runtime_ms:6314506,watch,...overrides},
    files:[{id:93,duration_ms:6314506,available:true}],children:[]};
}
test("a watched title resumes its current partial rewatch after close",()=>{
  const c=harness(),watch={position_ms:4348498,duration_ms:6314506,watched:true};
  const model=c.itemPageModel("93",page(watch),[]);
  assert.equal(model.resume,4348498);
  assert.equal(model.playStart,4348498,"the button and playback input use the same position");
  assert.equal(model.item.watch.watched,true,"resuming does not clear watched history");
});
test("unstarted and completed positions retain start-over behavior",()=>{
  const c=harness();
  for(const watched of [false,true]){
    for(const position_ms of [0,3000,95000,100000,100001])
      assert.equal(c.playbackResumeMs({position_ms,watched},100000),0);
    assert.equal(c.playbackResumeMs({position_ms:3001,watched},100000),3001);
    assert.equal(c.playbackResumeMs({position_ms:94999,watched},100000),94999);
  }
});
test("known playback runtime takes priority and missing runtime retains conservative watched behavior",()=>{
  const c=harness();
  assert.equal(c.playbackResumeMs({position_ms:60000,duration_ms:60000,watched:true},100000),60000);
  assert.equal(c.playbackResumeMs({position_ms:60000,duration_ms:100000,watched:true},0),60000);
  assert.equal(c.playbackResumeMs({position_ms:60000,watched:true},0),0);
  assert.equal(c.playbackResumeMs({position_ms:60000,watched:false},0),60000);
  for(const position_ms of [-1,NaN,Infinity,"60000",Number.MAX_SAFE_INTEGER+1])
    assert.equal(c.playbackResumeMs({position_ms,watched:false},100000),0);
});
test("a partial watched audiobook chooses its current part and local resume",()=>{
  const c=harness(),data=page({position_ms:150000,duration_ms:300000,watched:true},
    {kind:"audiobook",runtime_ms:300000});
  data.files=[{id:1,available:true,part_offset_ms:0,duration_ms:100000},
    {id:2,available:true,part_offset_ms:100000,duration_ms:200000}];
  const model=c.itemPageModel("93",data,[]);
  assert.equal(model.resume,150000);
  assert.equal(model.playable.id,2);
  assert.equal(model.playStart,50000);
});
