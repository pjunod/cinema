'use strict';
const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const html=fs.readFileSync('crates/plurxd/src/web/index.html','utf8');
function source(name){let at=html.indexOf(`async function ${name}(`);if(at<0)at=html.indexOf(`function ${name}(`);assert.ok(at>=0);return html.slice(at,html.indexOf('\n}',at)+2);}
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
