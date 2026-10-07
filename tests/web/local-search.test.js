'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const {shellSource} = require("./shell-source.js");
const html=shellSource().bodyScript;
function source(name){let at=html.indexOf(`async function ${name}(`);if(at<0)at=html.indexOf(`function ${name}(`);assert.ok(at>=0);return html.slice(at,html.indexOf('\n}',at)+2);}

function headerSearch(layout){
 const timers=new Map(),listeners=new Map();let nextTimer=0;
 const q={value:'',isConnected:true,addEventListener:(name,fn)=>listeners.set(name,fn)};
 const app={innerHTML:''},location={hash:'#/settings/metadata'};
 const document={activeElement:null,getElementById:id=>id==='q'?q:id==='app'?app:null};
 const noop=()=>{};
 const c=vm.createContext({document,location,ME:{is_admin:true,username:'cinema-admin'},APP_NAME:'Cinema',
  captureSearchFocus:()=>null,restoreSearchFocus:noop,installAvailable:()=>false,esc:String,getQ:()=>'',buildLabel:()=>'',
  navKeyboardWireOnce:noop,theaterWireOnce:noop,catalogWireOnce:noop,pollActivity:noop,
  theaterNavHtml:()=>'',catalogIcon:()=>'',catalogTabsHtml:()=>'',catalogFillNav:noop,
  setTimeout:fn=>{timers.set(++nextTimer,fn);return nextTimer;},clearTimeout:id=>timers.delete(id)});
 // Execute each shipped renderer, including its real search listener.
 vm.runInContext(source('wireSearchInput')+'\n'+source(layout),c);c[layout]('settings','Metadata');
 return {q,app,document,location,input:value=>{q.value=value;listeners.get('input')();},
  flush:()=>{const pending=[...timers.values()];timers.clear();pending.forEach(fn=>fn());}};
}

test('saved-login autofill cannot redirect Metadata to a username search in any layout',()=>{
 for(const layout of ['classicChrome','catalogChrome','theaterChrome']){
  const h=headerSearch(layout);
  h.input('cinema-admin');h.flush();
  assert.equal(h.location.hash,'#/settings/metadata');
  const field=h.app.innerHTML.match(/<input\b[^>]*\bid="q"[^>]*>/)[0];
  assert.match(field,/type="search"/);assert.match(field,/autocomplete="off"/);
 }
});
for(const layout of ['classicChrome','catalogChrome','theaterChrome']){
 test(`${layout}: focused search still debounces typing, paste and clearing`,()=>{
  const h=headerSearch(layout);h.document.activeElement=h.q;
  h.input('Dune');h.input('Dune / Part Two');
  assert.equal(h.location.hash,'#/settings/metadata');h.flush();
  assert.equal(h.location.hash,'#/search/Dune%20%2F%20Part%20Two');
  h.input('');h.flush();assert.equal(h.location.hash,'#/');
 });
 test(`${layout}: pending search cannot override navigation or a replaced header`,()=>{
  const h=headerSearch(layout);h.document.activeElement=h.q;h.input('Dune');
  h.location.hash='#/settings/libraries';h.flush();assert.equal(h.location.hash,'#/settings/libraries');
  h.input('Alien');h.q.isConnected=false;h.flush();assert.equal(h.location.hash,'#/settings/libraries');
 });
 test(`${layout}: later background autofill cancels a pending search`,()=>{
  const h=headerSearch(layout);h.document.activeElement=h.q;h.input('Dune');
  h.document.activeElement=null;h.input('cinema-admin');h.flush();
  assert.equal(h.location.hash,'#/settings/metadata');
 });
}

test('Metadata API keys discourage saved-login autofill and retain provider links',()=>{
 const c=vm.createContext({SETTINGS_DATA:{libs:[]},esc:String,setCard:body=>body,
  cardHead:()=>'',setCardFoot:()=>'',setHead:()=>'',keyBackfillHtml:()=>'',searchSettingsCard:()=>''});
 vm.runInContext(source('metadataPanel'),c);
 const panel=c.metadataPanel({tmdb_configured:false,omdb_configured:false},{});
 for(const id of ['tk','ok']){
  const field=panel.match(new RegExp(`<input\\b[^>]*\\bid="${id}"[^>]*>`))[0];
  assert.match(field,/type="password"/);assert.match(field,/autocomplete="new-password"/);
 }
 assert.match(panel,/href="https:\/\/www.themoviedb.org\/settings\/api"/);
 assert.match(panel,/href="https:\/\/www.omdbapi.com\/apikey.aspx"/);
});

test('search settings send a boolean object and leave the dialog open on failure',async()=>{
 let closed=false,request,error={textContent:''};const dialog={isConnected:true,querySelector:id=>id==='#semantic-enabled'?{checked:false}:error,close:()=>closed=true};
 const c=vm.createContext({document:{getElementById:()=>dialog},api:async(url,r)=>{request={url,...r};throw Error('Save failed');},toast:()=>{}});vm.runInContext(source('saveSearchSettings'),c);await c.saveSearchSettings();
 assert.equal(request.url,'/search/settings');assert.equal(typeof request.body,'object');assert.equal(request.body.semantic_enabled,false);assert.equal(closed,false);assert.equal(error.textContent,'Save failed');
});
test('classification correction carries the revision and separate additions and suppressions',async()=>{
 let request;const dialog={isConnected:true,dataset:{itemId:'123',revision:'7'},querySelector:id=>({value:id==='#classification-include'?'topic:space, format:documentary':'format:stand-up'}),close:()=>{}};
 const c=vm.createContext({document:{getElementById:()=>dialog},api:async(url,r)=>{request={url,...r};},toast:()=>{}});vm.runInContext(source('saveClassification'),c);await c.saveClassification();
 assert.equal(request.url,'/items/123/classification');assert.equal(request.body.expected_revision,7);assert.deepEqual(Array.from(request.body.include),['topic:space','format:documentary']);assert.deepEqual(Array.from(request.body.exclude),['format:stand-up']);
});
test('semantic results never replace exact search or paint after navigation',async()=>{
 let finish,painted=false;const original=[{id:1,title:'Exact title'}];const c=vm.createContext({PAGE_RENDER_GENERATION:1,location:{hash:'#/search/test'},SEARCH_DATA:{results:original,related:[]},api:()=>new Promise(r=>finish=r),paintSemanticResults:()=>painted=true});vm.runInContext(source('loadSemanticResults'),c);
 const pending=c.loadSemanticResults('test',1,'#/search/test');c.location.hash='#/';finish({results:[{id:2}]});await pending;
 assert.equal(painted,false);assert.equal(c.SEARCH_DATA.results,original);assert.equal(c.SEARCH_DATA.related.length,0);
});
test('related suggestions deduplicate exact hits and honor the selected media type',()=>{
 const host={innerHTML:''};const c=vm.createContext({document:{getElementById:id=>id==='search-related'?host:{value:'movie'}},SEARCH_DATA:{results:[{id:1,kind:'movie'}],related:[{id:1,kind:'movie'},{id:2,kind:'show'},{id:3,kind:'movie'}]},exactWireId:i=>String(i.id),grid:items=>items.map(i=>`item-${i.id}`).join(',')});vm.runInContext(source('paintSemanticResults'),c);c.paintSemanticResults();assert.match(host.innerHTML,/Related by meaning/);assert.match(host.innerHTML,/item-3/);assert.doesNotMatch(host.innerHTML,/item-[12]/);
});
