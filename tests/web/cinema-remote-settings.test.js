#!/usr/bin/env node
'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const test=require('node:test');
const source=fs.readFileSync('crates/plurxd/src/web/pages/settings-developer.js','utf8');
function declaration(name){
  const start=source.search(new RegExp('(?:async )?function '+name+'\\('));
  assert.ok(start>=0);const tail=source.slice(start);
  const end=tail.slice(1).search(/\n(?:async )?function /);
  return end<0?tail:tail.slice(0,end+1);
}
test('remote preference saves both choices without readiness gating',async()=>{
  const checkbox={checked:true};const error={textContent:''};const button={disabled:false};
  const calls=[];let saved;
  const context={document:{getElementById:id=>id==='cinema-remote-control'?checkbox:error},
    api:async(url,request)=>{calls.push({url,request});return request.body;},
    cacheSettings:value=>{saved=value;},toast:()=>{},setCardSaved:value=>{value.disabled=false;},
    setCard:value=>value,cardHead:()=>'',togRow:()=>'',devReq:()=>'<p>Waiting for client qualification</p>',
    devGraduation:(wait,moves)=>`${wait} ${moves}`,setCardFoot:()=>''};
  vm.createContext(context);vm.runInContext(declaration('cinemaRemoteCard')+'\n'+declaration('saveCinemaRemote'),context);
  const card=context.cinemaRemoteCard({cinema_remote_control:false},{features:[]});
  assert.match(card,/Waiting for client qualification/);assert.match(card,/Settings → Playback/);
  await context.saveCinemaRemote(button);
  assert.equal(calls[0].url,'/settings');assert.equal(saved.cinema_remote_control,true);assert.equal(button.disabled,false);
  checkbox.checked=false;await context.saveCinemaRemote(button);
  assert.equal(saved.cinema_remote_control,false);
  console.log('PASS remote preference saves both choices without readiness gating');
});
