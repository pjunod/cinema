"use strict";
const {test}=require("node:test"),assert=require("node:assert/strict"),vm=require("node:vm"),fs=require("node:fs"),path=require("node:path");
const source=fs.readFileSync(path.join(__dirname,"../../crates/plurxd/src/web/core/theme.js"),"utf8");
function engine(initial={}){
 const stored=new Map(Object.entries(initial)),vars=new Map(),root={dataset:{},style:{setProperty:(k,v)=>vars.set(k,v),removeProperty:k=>vars.delete(k)}};
 const c=vm.createContext({localStorage:{getItem:k=>stored.get(k)||null,setItem:(k,v)=>stored.set(k,v)},document:{documentElement:root,getElementById:()=>null},window:{innerWidth:1440,matchMedia:()=>({matches:false,addEventListener:()=>{}})}});
 vm.runInContext(source,c);return {c,stored,vars,root,run:code=>vm.runInContext(code,c)};
}
test("Meridian preferences preserve defaults, unknown fallback and legacy Catalog migration",()=>{
 assert.equal(engine().root.dataset.layout,"classic");
 assert.equal(engine({plurx_layout:"unknown"}).root.dataset.layout,"classic");
 const legacy=engine({plurx_layout:"plex"});assert.equal(legacy.root.dataset.layout,"catalog");assert.equal(legacy.stored.get("plurx_layout"),"catalog");
 const current=engine({plurx_layout:"meridian",plurx_theme:"meridian",plurx_appearance:"light"});assert.equal(current.root.dataset.layout,"meridian");assert.equal(current.root.dataset.theme,"meridian");assert.equal(current.root.dataset.mode,"light");
 const restarted=engine(Object.fromEntries(current.stored));assert.equal(restarted.root.dataset.layout,"meridian");assert.equal(restarted.vars.get("--selected-bg"),"#29292d");
});
test("layout, palette, appearance, poster and library preferences remain independent",()=>{
 const e=engine({plurx_layout:"meridian",plurx_theme:"terminal",plurx_appearance:"light",plurx_iconsize:"xl",plurx_library_view:"grid",plurx_home_group:"share"});
 e.stored.set("plurx_theme","meridian");e.run("applyTheme()");assert.equal(e.root.dataset.layout,"meridian");assert.equal(e.vars.get("--poster"),"226px");
 e.stored.set("plurx_layout","catalog");e.run("applyLayout()");assert.equal(e.root.dataset.theme,"meridian");assert.equal(e.stored.get("plurx_appearance"),"light");assert.equal(e.stored.get("plurx_library_view"),"grid");assert.equal(e.stored.get("plurx_home_group"),"share");
});
test("semantic selection and action roles follow every shipped active palette",()=>{
 const e=engine(),themes=e.run("Object.keys(THEMES)");
 for(const id of themes)for(const mode of ["dark","light"]){
  e.stored.set("plurx_theme",id);e.stored.set("plurx_appearance",mode);e.run("applyTheme()");
  const ratio=(a,b)=>e.run(`themeContrast(${JSON.stringify(e.vars.get(a))},${JSON.stringify(e.vars.get(b))})`);
  assert.ok(ratio("--selected-ink","--selected-bg")>=4.5,`${id}/${mode} selected`);
  assert.ok(ratio("--action-ink","--action-bg")>=4.5,`${id}/${mode} action`);
  assert.equal(e.vars.get("--hero-ink"),e.vars.get("--text"));assert.equal(e.vars.get("--hero-scrim"),e.vars.get("--bg"));
  for(const bg of ["--bg","--panel","--panel2"])assert.ok(ratio("--layout-muted",bg)>=4.5,`${id}/${mode} muted on ${bg}`);
  assert.ok(ratio("--focus-gap","--selected-bg")>=3,`${id}/${mode} selected focus gap`);
 }
 e.stored.set("plurx_theme","meridian");e.run("applyTheme()");e.stored.set("plurx_theme","terminal");e.run("applyTheme()");assert.notEqual(e.vars.get("--selected-bg"),"#f5f5f6");assert.equal(e.vars.get("--selected-bg"),e.vars.get("--text"));
});
