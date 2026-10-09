"use strict";
const test=require("node:test"),assert=require("node:assert/strict"),fs=require("node:fs"),path=require("node:path"),vm=require("node:vm");
const root=path.resolve(__dirname,"../.."),fixture=JSON.parse(fs.readFileSync(path.join(root,"crates/plurx-core/tests/fixtures/remote-control-v1.json")));
function harness(){const context=vm.createContext({TextEncoder});vm.runInContext(fs.readFileSync(path.join(root,"crates/plurxd/src/web/core/remote-guard.js"),"utf8"),context);return vm.runInContext("({Wire:CinemaRemoteWire,Guard:CinemaRemoteGuard})",context);}
const clone=value=>JSON.parse(JSON.stringify(value));
function current(command=fixture.valid[0].command){return {target:clone(command.target),grant_id:command.grant_id,control_epoch:command.control_epoch,context_revision:4,focus_revision:7,text_nonce:"00000000-0000-4000-8000-000000000006"};}
test("canonical B01 wire fixtures accept every valid action and reject every malformed envelope",()=>{
  const {Wire}=harness();for(const value of fixture.valid)assert.equal(Wire.creditKind(Wire.decode(JSON.stringify(value.command)).action),value.credit_kind,value.name);
  for(const value of fixture.invalid)assert.throws(()=>Wire.decode(JSON.stringify(value.command)),/invalid/,value.name);
});
test("canonical B01 receiver scenarios have identical outcome and no rejected effects",()=>{
  const {Guard}=harness();
  for(const scenario of fixture.scenarios){
    const guard=new Guard(),baseline=clone(fixture.valid[0].command),command=clone(scenario.command),context=current(baseline);let effects=0;
    guard.setContext(context);guard.mint(scenario.credit_kind,command.credit,scenario.issued_ms);
    if(scenario.preapply)assert.equal(guard.apply(command,scenario.issued_ms,null,()=>{effects++;return "applied";}),"applied");
    if(scenario.new_control_epoch)guard.setContext({...context,control_epoch:scenario.new_control_epoch});
    const before=effects,outcome=guard.apply(command,scenario.now_ms,null,()=>{effects++;return "applied";});
    assert.equal(outcome,scenario.expected,scenario.name);assert.equal(effects-before,scenario.expected==="applied"?1:0,scenario.name);
  }
});
test("strict bounded JSON rejects duplicate nested fields prototype action names and malformed Unicode",()=>{
  const {Wire}=harness();const valid=JSON.stringify(fixture.valid[0].command);
  assert.throws(()=>Wire.decode(valid.replace('"direction":"down"','"direction":"down","direction":"up"')),/invalid/);
  for(const type of ["constructor","__proto__"])assert.throws(()=>Wire.decode({...fixture.valid[0].command,action:{type}}),/invalid/);
  assert.throws(()=>Wire.parse('"\\ud800"'),/invalid/);assert.throws(()=>Wire.parse("[".repeat(34)+"]".repeat(34)),/invalid/);
  assert.throws(()=>Wire.decode(" ".repeat(16385)+valid),/invalid/);
});
test("sequence is consumed before reentrant effect and result eviction cannot resurrect Select",()=>{
  const {Guard}=harness(),guard=new Guard(),command=clone(fixture.valid[1].command);guard.setContext(current(command));guard.mint("interaction",command.credit,0);let effects=0;
  assert.equal(guard.apply(command,1,null,()=>{effects++;assert.equal(guard.apply(command,1,null,()=>{effects++;return "applied";}),"duplicate_or_old");return "applied";}),"applied");
  assert.equal(effects,1);assert.equal(guard.result(command.control_epoch,1,10001),null);assert.equal(guard.apply(command,10001,null,()=>{effects++;return "applied";}),"duplicate_or_old");assert.equal(effects,1);
});
test("backward clock invalidates credits and physical invalidation preserves replay high water",()=>{
  const {Guard}=harness(),guard=new Guard(),command=clone(fixture.valid[1].command);guard.setContext(current(command));guard.mint("interaction",command.credit,100);
  assert.equal(guard.apply(command,99,null,()=>"applied"),"invalid");assert.equal(guard.apply(command,101,null,()=>"applied"),"expired");
  guard.mint("interaction",command.credit,102);assert.equal(guard.apply(command,103,null,()=>"applied"),"applied");guard.invalidate();guard.mint("interaction",command.credit,104);assert.equal(guard.apply(command,105,null,()=>"applied"),"duplicate_or_old");
});

test("whole body parser rejects decimal exponent unsafe and negative-zero lexemes at every depth",()=>{
  const {Wire}=harness(),raw=JSON.stringify(fixture.valid[0].command);
  for(const spelling of ["1.0","1e0","-0","9007199254740992"]){
    const command=raw.replace('"sequence":1','"sequence":'+spelling);assert.throws(()=>Wire.decode(command),/invalid/,spelling);
    assert.throws(()=>Wire.parse('{"version":"cinema.remote.v1","commands":['+command+']}'),/invalid/,"nested "+spelling);
  }
  assert.equal(Wire.parse('{"text":"Search 1e0 and -0"}').text,"Search 1e0 and -0");
});

test("UUID letter case has Rust value semantics while opaque owner IDs retain their spelling",()=>{
  const {Guard,Wire}=harness(),command=clone(fixture.valid[0].command);for(const key of ["grant_id","control_epoch","credit"])command[key]="abcdefab"+command[key].slice(8);const guard=new Guard();assert.equal(guard.setContext(current(command)),"applied");guard.mint("interaction",command.credit.toUpperCase(),0);const upper=clone(command);for(const key of ["grant_id","control_epoch","credit"])upper[key]=upper[key].toUpperCase();assert.equal(guard.apply(upper,1,null,()=>"applied"),"applied");assert.equal(Wire.decode(upper).target.owner_node_id,command.target.owner_node_id);assert.equal(guard.setContext({...current(command),text_nonce:3}),"invalid");
});
