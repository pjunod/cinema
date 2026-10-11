"use strict";
const {test}=require("node:test"),assert=require("node:assert/strict");
const fs=require("node:fs"),vm=require("node:vm"),path=require("node:path");
const root=path.resolve(__dirname,"../../crates/plurxd/src/web");
function harness(saved){
  const storage=new Map(saved===undefined?[]:[["plurx_library_view",saved]]);
  const ctx=vm.createContext({window:{addEventListener(){}},localStorage:{getItem:k=>storage.get(k),setItem:(k,v)=>storage.set(k,v)},
    exactWireId:it=>String(it.id),resLabel:(_,h)=>h>=1700?"4K":h>=1300?"1440p":h>=900?"1080p":h>=650?"720p":h>=400?"480p":`${h}p`});
  for(const file of ["layouts/renderers.js","core/cards.js","layouts/library-grids.js"])
    vm.runInContext(fs.readFileSync(path.join(root,file),"utf8"),ctx);
  return {ctx,storage,run:code=>vm.runInContext(code,ctx)};
}
const plain=value=>JSON.parse(JSON.stringify(value));
test("library rows group the entire listing by the effective sort",()=>{
  const {ctx}=harness();
  const items=Array.from({length:450},(_,i)=>({id:i+1,title:i<420?`The Atlas ${i}`:"Zodiac",year:i%2?2024:2023}));
  const groups=plain(ctx.libraryGroups(items,"title",new Date()));
  assert.deepEqual(groups.map(g=>[g.label,g.items.length]),[["A",420],["Z",30]]);
  assert.deepEqual(plain(ctx.libraryGroups(items,"year",new Date())).map(g=>[g.label,g.items.length]),[["2024",225],["2023",225]]);
});
test("library groups retain unknown metadata and authoritative title keys",()=>{
  const {ctx}=harness();
  const items=[{id:1,title:"The wrong title",sort_title:"arrival",resolution:2160},
    {id:2,title:"東京",year:null},{id:3,title:"",year:0},{id:4,title:"9 Lives",year:2024,resolution:1080}];
  assert.deepEqual(plain(ctx.libraryGroups(items,"title",new Date())).map(g=>[g.label,g.items.map(i=>i.id)]),[["#",[2,3,4]],["A",[1]]]);
  assert.deepEqual(plain(ctx.libraryGroups(items,"year",new Date())).map(g=>[g.label,g.items.length]),[["2024",1],["Unknown year",3]]);
  assert.deepEqual(plain(ctx.libraryGroups(items,"resolution",new Date())).map(g=>[g.label,g.items.length]),[["4K",1],["1080p",1],["Unknown resolution",2]]);
  assert.equal(ctx.libraryGroupFor({recorded_at:"2024-12-31T23:30:00-10:00"},"recorded",new Date()).label,"2024");
  assert.equal(ctx.libraryGroupFor({},"recorded",new Date()).label,"Unknown recording date");
});
test("recently added uses calendar periods without inventing missing dates",()=>{
  const {ctx}=harness(),now=new Date(2026,9,4,12);
  const item=date=>({added_at:date.getTime()/1000});
  const label=date=>ctx.libraryGroupFor(item(date),"added",now).label;
  assert.equal(label(new Date(2026,9,4,1)),"Today");
  assert.equal(label(new Date(2026,9,1,1)),"Previous 6 days");
  assert.equal(label(new Date(2026,9,5)),"Future dates");
  const old=ctx.libraryGroupFor(item(new Date(2026,7,18)),"added",now);
  assert.equal(old.key,"added-2026-8");
  assert.equal(ctx.libraryGroupFor({},"added",now).label,"Unknown date added");
  assert.equal(ctx.libraryGroupFor({added_at:"invalid"},"added",now).label,"Unknown date added");
});
test("merged library order matches the shared server sort fixture",()=>{
  const {ctx}=harness();
  const fixture=require("../contracts/library-sort-cases.json");
  const items=fixture.items.map(it=>({...it,id:String(it.index),added_at:it.index}));
  for(const [sort,expected] of Object.entries(fixture.expected_order)){
    const ordered=items.slice().reverse().sort((a,b)=>ctx.libraryItemCompare(a,b,sort));
    assert.deepEqual(ordered.map(it=>it.index),expected,sort);
  }
  const tied=[{id:"9007199254740993",sort_title:"same"},{id:"9007199254740992",sort_title:"same"}];
  assert.equal(tied.sort((a,b)=>ctx.libraryItemCompare(a,b,"title"))[0].id,"9007199254740992");
  assert.ok(ctx.libraryTextCompare("\uE000","😀")<0,"compare scalar values, not UTF-16 units");
});
test("row choice is persistent and group expansion does not change that preference",()=>{
  const {run,storage}=harness();
  assert.equal(run("LIB_PRESENTATION"),"rows");
  run("setLibraryPresentation('grid')");assert.equal(storage.get("plurx_library_view"),"grid");
  run("setLibraryPresentation('rows')");assert.equal(storage.get("plurx_library_view"),"rows");
  assert.equal(harness("grid").run("LIB_PRESENTATION"),"grid");
  assert.equal(harness("invalid").run("LIB_PRESENTATION"),"rows");
  run("setLibraryPresentation('invalid')");assert.equal(run("LIB_PRESENTATION"),"rows");
});
