"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const web=path.join(__dirname,"../../crates/plurxd/src/web"),fixture=JSON.parse(fs.readFileSync(path.join(__dirname,"../../crates/plurx-core/tests/fixtures/remote-control-v1.json"))),wire=fixture.valid[1].command;
function pending(){let resolve,reject;const promise=new Promise((a,b)=>{resolve=a;reject=b;});return {promise,resolve,reject};}
function harness(){
  const saved=[],connected=[],timers=[],identity={instance:"instance",origin:"https://cinema.test",user:"1",token:"one",generation:1};
  const context=vm.createContext({TextEncoder,URL,URLSearchParams,performance:{now:()=>100},setTimeout:fn=>{timers.push(fn);return timers.length;},clearTimeout:()=>{},document:{visibilityState:"visible",hasFocus:()=>true},CinemaRemote:{invalidate:()=>{}},cinemaRemoteIdentity:()=>identity,cinemaRemoteSecret:value=>/^[A-Za-z0-9_-]{43}$/.test(value),cinemaRemoteLoad:()=>({grants:[]}),cinemaRemoteSave:(who,value)=>saved.push(value),cinemaRemoteConnectSelected:async(device,grant)=>connected.push({device,grant}),cinemaRemoteUiChanged:()=>{}});
  vm.runInContext(fs.readFileSync(path.join(web,"core/remote-guard.js"),"utf8"),context);vm.runInContext(fs.readFileSync(path.join(web,"pages/remote-pairing.js"),"utf8"),context);
  return {context,saved,connected,timers,identity,run:code=>vm.runInContext(code,context)};
}
test("pairing links bind current server and exact target without selecting an API origin",()=>{
  const h=harness(),url=`cinema-remote://pair?server_instance_id=instance&owner_node_id=${wire.target.owner_node_id}&session_id=${wire.target.session_id}&receiver_epoch=${wire.target.receiver_epoch}&challenge_id=${wire.grant_id}#code=12345678`;
  h.context.link=url;assert.equal(h.run("cinemaRemoteParsePairLink(link).code"),"12345678");
  for(const invalid of [url.replace("instance","other"),url+"&secret=abc",url.replace("#code","&server=https://evil.test#code"),url.replace("cinema-remote://pair","https://evil.test/pair"),url.replace("12345678","123")]){h.context.link=invalid;assert.throws(()=>h.run("cinemaRemoteParsePairLink(link)"));}
});
test("closed pairing and changed selected screen fence late approved grant result",async()=>{
  for(const retirement of ["cinemaRemoteCancelPairFlow()","cinemaRemoteCancelPairFlow(); CINEMA_PAIR_FLOW={}"]){
    const h=harness(),reply=pending();h.context.device={receiver_id:wire.grant_id,name:"TV",target:wire.target};h.context.client={identity:h.identity,current:()=>true,retire:()=>{},request:()=>reply.promise};
    h.run("CINEMA_PAIR_FLOW=new CinemaRemotePairFlow(device,{client,changed:()=>{}});CINEMA_PAIR_FLOW.pending={pending_id:device.receiver_id,secret:'A'.repeat(43)}");const operation=h.run("CINEMA_PAIR_FLOW.result()");h.run(retirement);reply.resolve({status:"approved",grant_id:wire.grant_id,receiver_id:wire.grant_id,grant_secret:"A".repeat(43)});await operation;assert.equal(h.saved.length,0);assert.equal(h.connected.length,0);
  }
});
test("approved pairing saves grant once and connects without acquiring control",async()=>{
  const h=harness();h.context.device={receiver_id:wire.grant_id,name:"TV",target:wire.target};h.context.client={identity:h.identity,current:()=>true,retire:()=>{},request:async()=>({status:"approved",grant_id:wire.grant_id,receiver_id:wire.grant_id,grant_secret:"A".repeat(43)})};h.run("CINEMA_PAIR_FLOW=new CinemaRemotePairFlow(device,{client,changed:()=>{}});CINEMA_PAIR_FLOW.pending={pending_id:device.receiver_id,secret:'A'.repeat(43)}");await h.run("CINEMA_PAIR_FLOW.result()");assert.equal(h.saved.length,1);assert.equal(h.connected.length,1);assert.equal(h.connected[0].grant.grant_secret,"A".repeat(43));
});
test("QR canvas bounds modules and retains four module quiet zone without HTML parsing",()=>{
  const h=harness(),rectangles=[],canvas={getContext:()=>({set fillStyle(value){},fillRect:(...args)=>rectangles.push(args)})};h.context.canvas=canvas;h.context.rows=Array.from({length:21},()=>"1".repeat(21));assert.equal(h.run("cinemaRemoteQr(canvas,rows)"),true);assert.deepEqual(rectangles[1],[44,44,11,11]);h.context.rows=["<svg>"];assert.equal(h.run("cinemaRemoteQr(canvas,rows)"),false);h.context.rows=Array.from({length:178},()=>"0".repeat(178));assert.equal(h.run("cinemaRemoteQr(canvas,rows)"),false);
});
test("non-admin Developer renders only local cards before privileged settings or readiness reads",async()=>{
  const calls=[],context=vm.createContext({ME:{is_admin:false},location:{hash:"#/settings/developer"},api:url=>{calls.push(url);throw new Error("privileged read");},settingsRouteTab:hash=>hash==="#/settings/developer"?"developer":null,viewLocalRemoteSettings:generation=>calls.push(["local",generation])});
  vm.runInContext(fs.readFileSync(path.join(web,"pages/settings.js"),"utf8"),context);await vm.runInContext("viewSettings(7)",context);assert.deepEqual(calls,[["local",7]]);
  context.location.hash="#/settings/users";await vm.runInContext("viewSettings(8)",context);assert.equal(context.location.hash,"#/");assert.deepEqual(calls,[["local",7]]);
});

function phoneHarness(){
  const h=harness(),nodes=new Map(["cinema-remote-controls","cinema-phone-pair","cinema-phone-pair-status","cinema-remote-status"].map(id=>[id,{textContent:"",innerHTML:"",replaceChildren(){this.innerHTML="";},querySelectorAll(){return [];}}]));
  h.context.document.getElementById=id=>nodes.get(id)||null;h.context.document.querySelectorAll=()=>[];h.context.document.addEventListener=()=>{};h.context.window={addEventListener:()=>{}};h.context.setInterval=()=>0;h.context.location={hash:"#/remote"};h.context.CINEMA_WEB_CONTROLLER=null;h.context.CINEMA_WEB_RECEIVER=null;h.context.cinemaRemoteText=value=>String(value);h.context.esc=value=>String(value);
  vm.runInContext(fs.readFileSync(path.join(web,"pages/remote.js"),"utf8"),h.context);return {...h,nodes};
}
test("unpaired phone status updates before controller-only rendering returns",()=>{
  const h=phoneHarness();h.run("CINEMA_PAIR_FLOW={message:'Waiting for physical approval on the TV'};cinemaRemoteUiChanged()");assert.equal(h.nodes.get("cinema-phone-pair-status").textContent,"Waiting for physical approval on the TV");
});
test("late connection failure after another screen selection cannot repaint the new controller",async()=>{
  const h=phoneHarness(),reply=pending(),device={receiver_id:wire.grant_id,name:"TV",target:wire.target};h.context.device=device;h.context.grant={};h.context.oldController={generation:1,message:"old",connect(){this.generation++;return reply.promise;}};h.run("CINEMA_REMOTE_SELECTED=device;CINEMA_WEB_CONTROLLER=oldController");const connection=h.run("cinemaRemoteConnectSelected(device,grant)");h.context.oldController.generation++;h.context.freshController={generation:1,message:"new screen"};h.run("CINEMA_WEB_CONTROLLER=freshController;CINEMA_REMOTE_SELECTED=null;CINEMA_REMOTE_PAGE_GENERATION++");reply.reject(new Error("old screen failed"));await connection;assert.equal(h.context.freshController.message,"new screen");
});
test("late acquire failure after close cannot repaint a replacement phone controller",async()=>{
  const h=phoneHarness(),reply=pending(),device={receiver_id:wire.grant_id,target:wire.target};h.context.device=device;h.context.oldController={generation:1,device,message:"old",acquire:()=>reply.promise};h.run("CINEMA_REMOTE_SELECTED=device;CINEMA_WEB_CONTROLLER=oldController");const acquisition=h.run("cinemaRemoteAcquire(false,{isTrusted:true})");h.context.oldController.generation++;h.context.freshController={generation:1,message:"new acquisition"};h.run("CINEMA_WEB_CONTROLLER=freshController;CINEMA_REMOTE_SELECTED=null;CINEMA_REMOTE_PAGE_GENERATION++");reply.reject(new Error("old acquisition failed"));await acquisition;assert.equal(h.context.freshController.message,"new acquisition");
});

test("pairing deadline begins at claim and expiry gives a fresh bounded retry",async()=>{
  const h=harness(),calls=[];h.context.now=100;h.context.performance.now=()=>h.context.now;h.context.device={receiver_id:wire.grant_id,name:"TV",target:wire.target};h.context.client={identity:h.identity,current:()=>true,retire:()=>{},request:async path=>{calls.push(path);return path==="pairing/claim"?{pending_id:wire.grant_id,poll_secret:"A".repeat(43)}:{status:"pending"};}};
  h.run("CINEMA_PAIR_FLOW=new CinemaRemotePairFlow(device,{client,changed:()=>{}})");h.context.now=150000;await h.run("CINEMA_PAIR_FLOW.claim('12345678')");assert.equal(h.run("CINEMA_PAIR_FLOW.deadline"),270000);assert.match(h.run("CINEMA_PAIR_FLOW.message"),/Waiting/);
  h.context.now=270000;h.run("CINEMA_PAIR_FLOW.expire()");assert.match(h.run("CINEMA_PAIR_FLOW.message"),/expired.*fresh TV code/);assert.equal(h.run("CINEMA_PAIR_FLOW.pending"),null);
  h.context.now=300000;await h.run("CINEMA_PAIR_FLOW.claim('87654321')");assert.equal(h.run("CINEMA_PAIR_FLOW.deadline"),420000);assert.equal(calls.filter(path=>path==="pairing/claim").length,2);assert.match(h.run("CINEMA_PAIR_FLOW.message"),/Waiting/);
});

test("restricted TV state explains disabled phone controls instead of reporting active",()=>{
  const h=phoneHarness();h.context.CINEMA_WEB_CONTROLLER={message:"Remote active",state:{route:"restricted",capabilities:[],playback:null,focused_label:null},ownsControl:()=>true,alive:()=>true};h.run("cinemaRemoteUiChanged()");assert.equal(h.nodes.get("cinema-remote-status").textContent,"Use the TV directly on this screen");
});

test("explicit refresh fetches a new screen target and never silently acquires",()=>{
  const h=phoneHarness(),calls=[];h.context.PAGE_RENDER_GENERATION=4;h.context.viewRemote=generation=>calls.push(generation);h.run("cinemaRemoteRefreshScreens({isTrusted:false})");assert.equal(calls.length,0);h.run("cinemaRemoteRefreshScreens({isTrusted:true})");assert.deepEqual(calls,[5]);h.context.location.hash="#/";h.run("cinemaRemoteRefreshScreens({isTrusted:true})");assert.deepEqual(calls,[5]);
});

test("trusted assistive directional clicks work once without duplicating pointer or keyboard gestures",()=>{
  const h=phoneHarness(),listeners={},actions=[],holds=[],button={dataset:{remoteDirection:"right"},setPointerCapture:()=>{},addEventListener:(type,fn)=>{listeners[type]=fn;}};h.context.button=button;h.context.CINEMA_WEB_CONTROLLER={hold:direction=>holds.push(direction),stopHold:()=>{},send:async action=>actions.push(action)};h.run("cinemaRemoteWireDirection(button)");
  listeners.click({isTrusted:false,detail:0});assert.equal(actions.length,0);listeners.click({isTrusted:true,detail:0});assert.equal(actions.length,1);assert.equal(actions[0].type,"navigate");assert.equal(actions[0].direction,"right");
  listeners.pointerdown({isTrusted:true,button:0,pointerId:1,preventDefault:()=>{}});listeners.pointerup({isTrusted:true});listeners.click({isTrusted:true,detail:1});assert.equal(actions.length,1);assert.deepEqual(holds,["right"]);
  let prevented=0;listeners.keydown({isTrusted:true,key:"Enter",repeat:false,preventDefault:()=>prevented++});listeners.keydown({isTrusted:true,key:"Enter",repeat:true,preventDefault:()=>prevented++});listeners.keyup({isTrusted:true,key:"Enter",preventDefault:()=>prevented++});listeners.click({isTrusted:true,detail:0});assert.equal(actions.length,1);assert.equal(holds.length,2);assert.equal(prevented,3);
  listeners.click({isTrusted:true,detail:1});assert.equal(actions.length,2);
});
