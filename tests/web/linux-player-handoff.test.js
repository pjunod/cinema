"use strict";
// Settings → Playback → "Dolby Vision playback on Linux" and the plurx Player handoff
// (docs/clients/LINUX-PLAYER-PLAN.md §4.1, milestone M3). The shipped
// functions are lifted out of the web shell with acorn and run in a vm
// against a fake api(), document and localStorage, the same way
// hdr-display-peak.test.js runs the display-peak card.
const assert=require("node:assert/strict"),fs=require("node:fs"),vm=require("node:vm"),{test}=require("node:test"),acorn=require("../vendor/acorn.js");
const root="crates/plurxd/src/web/";
const parsed=new Map();
function declaration(file,name){
  if(!parsed.has(file)){const source=fs.readFileSync(root+file,"utf8");parsed.set(file,{source,body:acorn.parse(source,{ecmaVersion:"latest"}).body});}
  const {source,body}=parsed.get(file);
  const node=body.find(n=>(n.type==="FunctionDeclaration"&&n.id.name===name)
    ||(n.type==="VariableDeclaration"&&n.declarations.some(d=>d.id.name===name)));
  assert.ok(node,`${file}: ${name}`);return source.slice(node.start,node.end);
}
const HANDOFF=["OPEN_IN_KEY","PLURX_PLAYER_INSTALL_URL","PLAYER_HANDOFF_POLL_MS","PLAYER_HANDOFF","openInPreference",
  "linuxPlayerClient","playerHandoffWanted","playerHandoffBody","playerHandoffUrl","pollPlayerHandoff","handOffToPlayer","playerHandoffHtml",
  "renderPlayerHandoff","dismissPlayerHandoff","playHereInstead"].map(name=>declaration("detail/preplay-selection.js",name));

function harness({stored=null,status=()=>({redeemed:false}),mint=()=>({id:"h-1",code:"c0de_-XYZ",expires_in:120}),selection=null,navigator={platform:"Linux x86_64",userAgent:"Mozilla/5.0 (X11; Linux x86_64)"}}={}){
  const values=new Map(stored==null?[]:[["plurx.openIn",stored]]);
  const requests=[],browserPath=[],timers=[],toasts=[],elements=new Map();
  const element=id=>({id,hidden:false,innerHTML:"",textContent:"",dataset:{},attrs:{},className:"",
    setAttribute(k,v){this.attrs[k]=v;}});
  const modal={open:false,classList:{contains:c=>c==="open"&&modal.open}};
  elements.set("modal",modal);elements.set("open-in-error",element("open-in-error"));
  const ctx=vm.createContext({console,Promise,Error,navigator,values,requests,browserPath,timers,toasts,status,mint,selection,
    localStorage:{getItem:k=>values.get(k)??null,setItem:(k,v)=>values.set(k,String(v)),removeItem:k=>values.delete(k)},
    location:{origin:"http://media1:8096",href:"http://media1:8096/#/item/9"},
    document:{getElementById:id=>elements.get(id)??null,createElement:()=>element(""),
      body:{appendChild:el=>elements.set(el.id,el)}},
    // The poll's one-second sleep runs at once; anything longer (the opened
    // notice's auto-dismiss) is only recorded.
    setTimeout:(fn,ms)=>{timers.push(ms);if(ms<=1000)Promise.resolve().then(fn);return timers.length;},
    toast:message=>toasts.push(message)});
  vm.runInContext(`
    function setCard(html){return html;}
    function cardHead(title,sub){return "<h2>"+title+"</h2><p>"+sub+"</p>";}
    function prePlaySelection(){return selection;}
    function playbackFileContextForPlay(fileId,meta){
      if(meta&&meta.shared) return {source_ref:{kind:"shared"}};
      return {source_ref:{kind:"local",file_id:fileId}};
    }
    function beginPlayAttempt(...args){browserPath.push(args);throw new Error("browser path");}
    async function api(path,options={}){
      const method=options.method||"GET";requests.push({path,method,body:options.body??null});
      if(method==="POST"&&path==="/playback-handoffs") return mint(options.body);
      if(method==="GET"&&path.startsWith("/playback-handoffs/")) return status(requests.filter(r=>r.method==="GET").length);
      throw new Error("unexpected "+method+" "+path);
    }
    ${declaration("player/measurements.js","esc")}
    ${declaration("detail/dynamic-range.js","sourceDynamicRange")}
    ${declaration("detail/preplay-selection.js","playbackMetaFor")}
    ${HANDOFF.join("\n")}
    ${declaration("pages/settings-playback.js","openInCard")}
    ${declaration("pages/settings-playback.js","saveOpenIn")}
    ${declaration("player/decode-tiers.js","play")}
    this.h={linuxPlayerClient,playbackMetaFor,openInPreference,openInCard,saveOpenIn,play,playerHandoffWanted,playerHandoffBody,playerHandoffUrl,
      playHereInstead,dismissPlayerHandoff,handoffState:()=>PLAYER_HANDOFF};`,ctx);
  const notice=()=>elements.get("playerHandoff")||null;
  const playBrowserSafe=async(...args)=>{try{return await ctx.h.play(...args);}catch(e){if(e.message!=="browser path")throw e;return "browser";}};
  return {...ctx.h,ctx,values,requests,browserPath,timers,toasts,elements,modal,notice,playBrowserSafe};
}
const polls=h=>h.requests.filter(r=>r.method==="GET");

test("Linux Dolby Vision playback defaults to This browser and saves plurx Player in this browser only",()=>{
  const h=harness();
  assert.equal(h.openInPreference(),"browser");
  let card=h.openInCard();
  assert.match(card,/Dolby Vision playback on Linux/);
  assert.match(card,/only Dolby Vision titles open/);
  assert.match(card,/Other titles play in this browser/);
  assert.match(card,/<option value="browser" selected>This browser<\/option>/);
  assert.match(card,/<option value="player" >plurx Player \(Linux\)<\/option>/);
  assert.match(card,/Signing out of the web app also stops plurx Player playback/);
  h.saveOpenIn({value:"player"});
  assert.equal(h.values.get("plurx.openIn"),"player");
  assert.equal(h.openInPreference(),"player");
  assert.match(h.openInCard(),/<option value="player" selected>/);
  h.saveOpenIn({value:"surprise"});
  assert.equal(h.values.get("plurx.openIn"),"player","an unknown choice changes nothing");
  h.saveOpenIn({value:"browser"});
  assert.equal(h.values.has("plurx.openIn"),false,"the default is stored as no key");
  assert.equal(h.openInPreference(),"browser");
  assert.equal(h.requests.length,0,"the preference never reaches the server");
});

test("a malformed or unreadable preference reads as This browser, and a failed save says so",()=>{
  for(const value of ["","Player","true","browser"]) assert.equal(harness({stored:value}).openInPreference(),"browser");
  const h=harness();
  vm.runInContext('localStorage.getItem=()=>{throw new Error("blocked")};localStorage.setItem=()=>{throw new Error("blocked")}',h.ctx);
  assert.equal(h.openInPreference(),"browser");
  const control={value:"player"};h.saveOpenIn(control);
  assert.equal(control.value,"browser");
  assert.match(h.elements.get("open-in-error").textContent,/could not save/);
});

test("the setting is a card on Settings → Playback",()=>{
  const panels=fs.readFileSync(root+"pages/settings-panels.js","utf8");
  assert.match(panels,/\$\{openInCard\(\)\}\$\{hdrDisplayPeakCard\(\)\}/);
});

test("with the default setting Play takes the browser path, untouched",async()=>{
  for(const stored of [null,"browser","garbage"]){
    const h=harness({stored});
    assert.equal(await h.playBrowserSafe(7,"Film",1234,5000,{x:1,source_dynamic_range:"dolby_vision"}),"browser");
    assert.equal(h.browserPath.length,1);
    assert.deepEqual(JSON.parse(JSON.stringify(h.browserPath[0].slice(0,4))),[7,"Film",1234,5000]);
    assert.equal(h.requests.length,0,"no handoff is minted");
    assert.equal(h.notice(),null,"no handoff notice");
    assert.equal(h.ctx.location.href,"http://media1:8096/#/item/9");
  }
});

test("with plurx Player, Play mints a handoff from the pre-play selection and resume point",async()=>{
  const states=[];
  const h=harness({stored:"player",selection:{audio:2,subtitle:-1},
    mint:body=>{states.push(h.notice()?.dataset.state);return {id:"h-1",code:"c0de_-XYZ",expires_in:120};},
    status:n=>({redeemed:n>=3})});
  assert.equal(await h.playBrowserSafe(7,"Film",1234.4,5000,{source_dynamic_range:"dolby_vision"}),true);
  assert.deepEqual(states,["opening"],"the viewer sees Opening… while the code is minted");
  assert.deepEqual(JSON.parse(JSON.stringify(h.requests[0])),
    {path:"/playback-handoffs",method:"POST",body:{file_id:7,start_ms:1234,audio:2,subtitle:-1}});
  assert.equal(h.browserPath.length,0,"the browser player is not opened");
  assert.equal(h.ctx.location.href,"plurx-player://play?server=http%3A%2F%2Fmedia1%3A8096&code=c0de_-XYZ");
  assert.deepEqual(polls(h).map(r=>r.path),["/playback-handoffs/h-1","/playback-handoffs/h-1","/playback-handoffs/h-1"]);
  assert.deepEqual(h.timers.slice(0,3),[1000,1000,1000],"one poll a second");
  assert.equal(h.notice().dataset.state,"opened");
  assert.match(h.notice().innerHTML,/Playing <b>Film<\/b> in plurx Player/);
  assert.equal(h.notice().attrs.role,"status");
  assert.equal(h.timers.at(-1),6000,"the opened notice dismisses itself");
});

test("the mint body carries nulls for unchosen tracks and never a negative start",()=>{
  const h=harness();
  assert.deepEqual(JSON.parse(JSON.stringify(h.playerHandoffBody("12",-50,null))),{file_id:12,start_ms:0,audio:null,subtitle:null});
  assert.deepEqual(JSON.parse(JSON.stringify(h.playerHandoffBody(12,NaN,{audio:null,subtitle:3}))),{file_id:12,start_ms:0,audio:null,subtitle:3});
});

test("the scheme URL carries the origin and the code, encoded, and nothing else",()=>{
  const h=harness();
  const url=h.playerHandoffUrl("https://plurx.example:8443","a+b/c=");
  assert.equal(url,"plurx-player://play?server=https%3A%2F%2Fplurx.example%3A8443&code=a%2Bb%2Fc%3D");
  const parsedUrl=new URL(url);
  assert.equal(parsedUrl.protocol,"plurx-player:");
  assert.deepEqual([...parsedUrl.searchParams.keys()],["server","code"]);
  assert.equal(parsedUrl.searchParams.get("server"),"https://plurx.example:8443");
});

test("not redeemed within 15 s: plurx Player didn't open, with an install link and Play here instead",async()=>{
  const h=harness({stored:"player"});
  assert.equal(await h.playBrowserSafe(7,"Film",90000,5000,{m:1,source_dynamic_range:"dolby_vision"}),false);
  assert.equal(polls(h).length,15,"fifteen polls");
  assert.equal(h.timers.filter(ms=>ms===1000).length,15,"one second apart: 15 s");
  assert.equal(h.notice().dataset.state,"missing");
  assert.match(h.notice().innerHTML,/plurx Player didn't open/);
  assert.match(h.notice().innerHTML,/docs\/clients\/LINUX-PLAYER-PLAN\.md/);
  assert.match(h.notice().innerHTML,/onclick="playHereInstead\(\)">Play here instead</);
  // Play here instead opens this title in the browser with the same inputs,
  // and mints nothing.
  const before=h.requests.length;
  assert.equal(await h.playHereInstead().catch(e=>e.message),"browser path");
  assert.equal(h.requests.length,before);
  assert.deepEqual(JSON.parse(JSON.stringify(h.browserPath[0].slice(0,4))),[7,"Film",90000,5000]);
  assert.equal(h.browserPath[0][4].m,1,"the same metadata");
  assert.equal(h.notice().hidden,true);
  // The bypass is one-shot: the next Play hands off again.
  await h.playBrowserSafe(7,"Film",90000,5000,{m:1,source_dynamic_range:"dolby_vision"});
  assert.equal(h.requests.filter(r=>r.method==="POST").length,2);
});

test("a failed poll is retried, and a dismissed handoff stops polling",async()=>{
  let n=0;
  const h=harness({stored:"player",status:()=>{n++;if(n<3)throw new Error("503");return {redeemed:true};}});
  assert.equal(await h.playBrowserSafe(7,"Film",0,0,{source_dynamic_range:"dolby_vision"}),true);
  assert.equal(h.notice().dataset.state,"opened");
  const d=harness({stored:"player",status:count=>{if(count===2)d.dismissPlayerHandoff();return {redeemed:false};}});
  assert.equal(await d.playBrowserSafe(7,"Film",0,0,{source_dynamic_range:"dolby_vision"}),false);
  assert.equal(polls(d).length,2,"no poll after Dismiss");
  assert.equal(d.notice().hidden,true);
});

test("a refused mint says why and still offers Play here instead",async()=>{
  const h=harness({stored:"player",mint:()=>{throw new Error("handoffs are not available on this server");}});
  assert.equal(await h.playBrowserSafe(7,"Film",0,0,{source_dynamic_range:"dolby_vision"}),false);
  assert.equal(h.notice().dataset.state,"failed");
  assert.match(h.notice().innerHTML,/handoffs are not available on this server/);
  assert.match(h.notice().innerHTML,/Play here instead/);
  assert.equal(h.ctx.location.href,"http://media1:8096/#/item/9","nothing navigates without a code");
  const e=harness({stored:"player",mint:()=>({expires_in:120})});
  assert.equal(await e.playBrowserSafe(7,"Film",0,0,{source_dynamic_range:"dolby_vision"}),false);
  assert.equal(e.notice().dataset.state,"failed");
});

test("only a cold start of a local title is handed off",()=>{
  const h=harness({stored:"player"});
  assert.equal(h.playerHandoffWanted(7,{source_dynamic_range:"dolby_vision"},null,null),true);
  assert.equal(h.playerHandoffWanted(7,{source_dynamic_range:"dolby_vision"},{token:1},null),false,"Watch layout reserved attempt");
  assert.equal(h.playerHandoffWanted(7,{source_dynamic_range:"dolby_vision"},null,{fileId:7}),false,"retry");
  assert.equal(h.playerHandoffWanted(7,{audiobook:true,source_dynamic_range:"dolby_vision"},null,null),false,"audiobook");
  assert.equal(h.playerHandoffWanted(7,{return_channel_id:3,source_dynamic_range:"dolby_vision"},null,null),false,"Library channel");
  assert.equal(h.playerHandoffWanted("s:9",{shared:true,source_dynamic_range:"dolby_vision"},null,null),false,"shared source");
  assert.equal(h.playerHandoffWanted("abc",{},null,null),false,"not a file id");
  h.modal.open=true;
  assert.equal(h.playerHandoffWanted(7,{source_dynamic_range:"dolby_vision"},null,null),false,"an open player reopening itself");
});


test("only Dolby Vision on Linux can mint a handoff despite a saved player preference",async()=>{
  const platforms=[
    {platform:"MacIntel",userAgent:"Mozilla/5.0 (Macintosh; Intel Mac OS X)"},
    {platform:"Win32",userAgent:"Mozilla/5.0 (Windows NT 10.0)"},
    {platform:"Linux armv8l",userAgent:"Mozilla/5.0 (Linux; Android 14)"},
    {platform:"Linux x86_64",userAgent:"Mozilla/5.0 (X11; CrOS x86_64)"},
    {platform:"iPad",userAgent:"Mozilla/5.0 (iPad)"},
    {platform:"",userAgent:""},
    {platform:"Linux x86_64",userAgentData:{platform:"Windows"}},
    undefined
  ];
  for(const navigator of platforms){
    const h=harness({stored:"player",navigator});
    if(navigator===undefined)vm.runInContext("delete this.navigator",h.ctx);
    assert.equal(h.openInPreference(),"browser");
    assert.match(h.openInCard(),/disabled/);
    assert.match(h.openInCard(),/Available only in a Linux browser/);
    const control={value:"player"};h.saveOpenIn(control);
    assert.equal(control.value,"browser");
    assert.equal(await h.playBrowserSafe(7,"DV",0,0,{source_dynamic_range:"dolby_vision"}),"browser");
    assert.equal(h.requests.length,0);
    assert.equal(h.notice(),null);
    assert.equal(h.ctx.location.href,"http://media1:8096/#/item/9");
  }
  for(const range of ["sdr","hdr10","hlg",null,undefined]){
    const h=harness({stored:"player"});
    assert.equal(await h.playBrowserSafe(7,"Film",123,5000,{source_dynamic_range:range}),"browser");
    assert.equal(h.requests.length,0);
    assert.equal(h.browserPath[0][2],123);
    assert.equal(h.notice(),null);
    assert.equal(h.ctx.location.href,"http://media1:8096/#/item/9");
  }
});

test("modern Linux platform detection enables the Linux-only setting",()=>{
  const h=harness({stored:"player",navigator:{userAgentData:{platform:"Linux"}}});
  assert.equal(h.linuxPlayerClient(),true);
  assert.equal(h.openInPreference(),"player");
  assert.doesNotMatch(h.openInCard(),/disabled/);
});

test("selected file metadata determines handoff and clears a different version's range",()=>{
  const h=harness({stored:"player",status:()=>({redeemed:true})});
  const page={item:{kind:"movie"},meta:{source_dynamic_range:"dolby_vision",episode_id:9}};
  for(const [file,range] of [
    [{hdr:"dolby_vision"},"dolby_vision"],
    [{hdr_format:"Dolby Vision, Profile 7"},"dolby_vision"],
    [{hdr:"hdr10",hdr_format:"HDR10+"},"hdr10"],
    [{hdr:"hlg"},"hlg"],
    [{hdr:"sdr"},"sdr"],
    [{},null]
  ]){
    const meta=h.playbackMetaFor(page,file);
    assert.equal(meta.source_dynamic_range,range);
    assert.equal(meta.episode_id,9);
    assert.equal(h.playerHandoffWanted(7,meta),range==="dolby_vision");
  }
  assert.equal(page.meta.source_dynamic_range,"dolby_vision","the model was not mutated");
});


test("every layout's Play and Start over buttons route the clicked version only",()=>{
  for(const [file,name] of [
    ["detail/preplay-selection.js","classicItemBody"],
    ["layouts/catalog.js","catalogItemBody"],
    ["layouts/theater.js","theaterItemBody"]
  ]){
    const h=harness({stored:"player"});
    vm.runInContext(`
      var ME={is_admin:false},NAV_ORIGIN=null,observed=[];
      function pageHead(){return "";}function itemKindLabel(){return "Movie";}
      function watchControls(){return "";}function specBadges(){return "";}
      function specBlock(){return "";}function unprobedNote(){return "";}
      function dvFileActionMount(){return "";}function fileAdminActions(){return "";}
      function bookEditionSection(){return "";}function artHtml(){return "";}
      function bookByline(){return "";}function ratingsSlot(){return "";}
      function fmtDur(){return "1:00";}function endsAt(){return "";}
      function playbackFileContextForFile(f){return {source_ref:{kind:"local",file_id:f.id}};}
      function play(...args){observed.push(args);}
      ${declaration("detail/preplay-selection.js","playCall")}
      ${declaration(file,name)}
    `,h.ctx);
    for(const firstRange of ["dolby_vision","hdr10"]){
      const files=[{id:7,hdr:firstRange,available:true,duration_ms:5000},
        {id:8,hdr:firstRange==="dolby_vision"?"hdr10":"dolby_vision",available:true,duration_ms:5000}];
      h.ctx.page={id:"9",item:{id:9,kind:"movie",title:"Two versions"},files,ancestors:[],children:[],
        best:files[0],playable:files[0],multi:true,shape:"versions",resume:1234,meta:{}};
      const html=vm.runInContext(`${name}(page)`,h.ctx);
      const buttons=[...html.matchAll(/onclick='(play\([^']+)'/g)];
      assert.equal(buttons.length,4,`${name}: Play, Start over, two versions`);
      h.ctx.observed.length=0;
      for(const [,encoded]of buttons){
        const code=encoded.replaceAll("&#39;","'").replaceAll("&quot;",'"').replaceAll("&amp;","&");
        vm.runInContext(code,h.ctx);
      }
      assert.deepEqual(Array.from(h.ctx.observed,args=>Number(args[0])),[7,7,7,8]);
      for(const args of h.ctx.observed){
        const expected=files.find(f=>f.id===Number(args[0])).hdr==="dolby_vision";
        assert.equal(h.playerHandoffWanted(args[0],args[4]),expected,name);
      }
    }
  }
});
