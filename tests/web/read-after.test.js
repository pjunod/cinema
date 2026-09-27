#!/usr/bin/env node
"use strict";
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const test=require("node:test");
const source=fs.readFileSync("crates/plurxd/src/web/core/api.js","utf8");
function fixture(){
  let now=1000;
  const calls=[];
  const context=vm.createContext({AUTH_GENERATION:1,TOKEN:"one",API:"/api/v1",window:{},
    performance:{now:()=>now},fetch:(url,options)=>new Promise((resolve,reject)=>calls.push({url,options,resolve,reject}))});
  vm.runInContext(source,context);
  return {context,calls,advance:ms=>{now+=ms;},
    start:(method="GET")=>vm.runInContext(`api("/watch",{method:${JSON.stringify(method)}})`,context),
    reply:(i,index=null)=>calls[i].resolve({ok:true,status:200,headers:{get:()=>index},json:async()=>({ok:true})})};
}
test("write floor survives node changes and preserves full u64 indexes",async()=>{
  const f=fixture();let pending=f.start("PUT");f.reply(0,"18446744073709551614");await pending;
  f.context.API="https://second/api/v1";
  pending=f.start();assert.equal(f.calls[1].options.headers["x-plurx-read-after"],"18446744073709551614");f.reply(1);await pending;
  f.advance(60001);pending=f.start();assert.equal(f.calls[2].options.headers["x-plurx-read-after"],undefined);f.reply(2);await pending;
});
test("out-of-order indexed replies never lower the floor",async()=>{
  const f=fixture();const first=f.start("PUT"),second=f.start("PUT");
  f.reply(1,"42");await second;f.reply(0,"41");await first;
  const read=f.start();assert.equal(f.calls[2].options.headers["x-plurx-read-after"],"42");f.reply(2);await read;
});
test("unknown invalidates receipts from writes already in flight",async()=>{
  const f=fixture();const first=f.start("PUT"),second=f.start("PUT");
  f.reply(0,"unknown");await first;f.reply(1,"43");await second;
  let read=f.start();assert.equal(f.calls[2].options.headers["x-plurx-read-after"],undefined);f.reply(2);await read;
  const fresh=f.start("PUT");f.reply(3,"44");await fresh;
  read=f.start();assert.equal(f.calls[4].options.headers["x-plurx-read-after"],"44");f.reply(4);await read;
});
test("unindexed and failed writes discard an older floor",async()=>{
  for(const failure of [false,true]){
    const f=fixture();let request=f.start("PUT");f.reply(0,"10");await request;
    request=f.start("PUT");
    if(failure){f.calls[1].reject(new Error("connection lost"));await assert.rejects(request,/connection lost/);}
    else{f.reply(1);await request;}
    request=f.start();assert.equal(f.calls[2].options.headers["x-plurx-read-after"],undefined);f.reply(2);await request;
  }
});
test("authentication changes discard the prior user's floor and replies",async()=>{
  const f=fixture();let request=f.start("PUT");f.reply(0,"12");await request;
  const late=f.start("PUT");f.context.AUTH_GENERATION=2;f.context.TOKEN="two";
  request=f.start();assert.equal(f.calls[2].options.headers["x-plurx-read-after"],undefined);f.reply(2);await request;
  f.reply(1,"99");await assert.rejects(late,/stale authorization/);
  request=f.start();assert.equal(f.calls[3].options.headers["x-plurx-read-after"],undefined);f.reply(3);await request;
});
test("malformed and overflowing receipts never become read headers",async()=>{
  for(const index of ["0","01","-1","18446744073709551616","12, 13"]){
    const f=fixture();let request=f.start("PUT");f.reply(0,index);await request;
    request=f.start();assert.equal(f.calls[1].options.headers["x-plurx-read-after"],undefined);f.reply(1);await request;
  }
});
