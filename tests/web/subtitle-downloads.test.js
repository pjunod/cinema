"use strict";
const assert=require("node:assert/strict");
const fs=require("node:fs");
const path=require("node:path");
const vm=require("node:vm");
const {test}=require("node:test");
const source=fs.readFileSync(path.join(__dirname,"../../crates/plurxd/src/web/detail/preplay-selection.js"),"utf8");
const escape=value=>String(value).replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;").replaceAll('"',"&quot;").replaceAll("'","&#39;");
function app(overrides={}){
  const context=vm.createContext({esc:escape,document:{getElementById:()=>null},...overrides});
  vm.runInContext(source,context);return context;
}

test("media with no subtitle tracks still offers search",()=>{
  const context=app();
  const file={id:7,available:true,subtitle_search_enabled:true,audio_streams:[],subtitle_streams:[]};
  assert.match(context.prePlayPickers(file),/Find subtitles/);
  assert.match(context.prePlayPickers(file),/subtitle-search-7/);
  assert.equal(context.prePlayPickers({...file,available:false}),"");
  assert.equal(context.prePlayPickers({...file,subtitle_search_enabled:false}),"");
});

test("provider release text is escaped and downloaded results are not offered again",async()=>{
  const results={isConnected:true,innerHTML:"",textContent:""};
  const context=app({document:{getElementById:id=>id.startsWith("subtitle-results")?results:{value:"en"}},api:async()=>({results:[{file_id:42,language:"en",release:'<img src=x onerror="alert(1)">',hash_match:true}],downloaded:[42]})});
  await context.searchSubtitles(7);
  assert.match(results.innerHTML,/&lt;img/);
  assert.doesNotMatch(results.innerHTML,/<img/);
  assert.match(results.innerHTML,/Downloaded/);
  assert.doesNotMatch(results.innerHTML,/onclick="downloadSubtitle/);
});

test("search completion cannot repaint a closed panel",async()=>{
  const results={isConnected:true,innerHTML:"",textContent:""};
  let finish;
  const context=app({document:{getElementById:id=>id.startsWith("subtitle-results")?results:{value:"en"}},api:()=>new Promise(resolve=>{finish=resolve;})});
  const pending=context.searchSubtitles(7);results.isConnected=false;finish({results:[]});await pending;
  assert.equal(results.innerHTML,"");
});

test("download refreshes the track list and selects the returned ordinal",async()=>{
  const select={value:""};const calls=[];const button={isConnected:true};
  const context=app({location:{hash:"#/item/9"},ITEM_FOR_FILE:{7:"9"},document:{getElementById:()=>select},api:async(url,options)=>{calls.push([url,options.body]);return{subtitle_index:2};},toast:()=>{}});
  context.viewItem=async id=>{assert.equal(id,"9");};
  context.setPrePlay=(...args)=>calls.push(args);
  await context.downloadSubtitle(7,42,"en",button);
  assert.equal(select.value,"2");
  assert.equal(calls[0][0],"/files/7/subtitles/download");
  assert.equal(calls[0][1].provider_file_id,42);
  assert.deepEqual(calls[1],[7,"subtitle","2"]);
});
