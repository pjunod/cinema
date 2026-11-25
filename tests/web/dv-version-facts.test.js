"use strict";
// How a version plays on each signed-in device (Curator ADR 0031 §7): the
// title page says when the enhancement layer is dropped instead of showing
// every device the source's grade. The Dolby Vision facts line itself is
// worded once on the server (hdr_devices::dv_facts_line) and shown as sent.
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const test=require("node:test");
const trackSource=fs.readFileSync("crates/plurxd/src/web/detail/track-facts.js","utf8");
function ctx(){
  const c=vm.createContext({console,JSON,
    esc:s=>String(s??"").replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll('"','&quot;')});
  vm.runInContext(trackSource,c);
  return c;
}
test("each device says how the version plays, a dropped layer as a caution",()=>{
  const c=ctx();
  const html=c.playsOnRow({plays_on:[
    {id:"a",device:"Living room",label:"Full FEL",full_quality:true,el_dropped:false},
    {id:"b",device:"Phone",label:"EL dropped · plays as 8.1",full_quality:false,el_dropped:true},
  ]});
  assert.match(html,/<dt>Plays on<\/dt>/);
  assert.match(html,/<li class=""><span>Living room<\/span><span>Full FEL<\/span><\/li>/);
  assert.match(html,/<li class="plays-caution"><span>Phone<\/span><span>EL dropped · plays as 8.1<\/span><\/li>/);
  assert.equal(c.playsOnRow({}),"");
});
