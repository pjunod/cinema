"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const {shellSource} = require("./shell-source.js");
// Offload: Curator deleted the files to free disk, the title stays. The web
// shell dims the card, replaces Play with Get it back for admins, and filters
// the library on presence.
const shell = shellSource();
const source = shell.bodyScript;
function declaration(name) {
  const start = source.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, name);
  const tail = source.slice(start);
  return tail.slice(0, tail.indexOf("\n}") + 2);
}
function context(me) {
  const ctx = vm.createContext({ME: me, esc: v => String(v ?? ""), exactWireId: it => String(it.id)});
  for (const name of ["offloadedBadge", "offloadActions", "offloadContainerActions", "libOpt"]) {
    vm.runInContext(declaration(name), ctx);
  }
  return ctx;
}

test("an offloaded card carries the badge and the dimming class", () => {
  const ctx = context({is_admin: false});
  assert.match(vm.runInContext("offloadedBadge({offloaded:true})", ctx), /Offloaded/);
  assert.match(vm.runInContext("offloadedBadge({rollup:{offloaded:3}})", ctx), /3 offloaded/);
  assert.equal(vm.runInContext("offloadedBadge({title:'x'})", ctx), "");
  assert.match(declaration("card"), /offloaded/);
  assert.match(shell.css, /\.poster\.offloaded/);
});

test("Get it back replaces Play for admins and is absent for everyone else", () => {
  const admin = context({is_admin: true});
  assert.match(vm.runInContext("offloadActions({id:7})", admin), /Get it back/);
  assert.match(vm.runInContext("offloadActions({id:7})", admin), /id="offstatus"/);
  const viewer = context({is_admin: false});
  assert.doesNotMatch(vm.runInContext("offloadActions({id:7})", viewer), /Get it back/);
  assert.equal(vm.runInContext("offloadContainerActions({id:7,rollup:{offloaded:0}})", admin), "");
  assert.match(vm.runInContext("offloadContainerActions({id:7,rollup:{offloaded:2}})", admin), /Get 2 back/);
  assert.equal(vm.runInContext("offloadContainerActions({id:7,rollup:{offloaded:2}})", viewer), "");
});

test("the library asks for a presence and remembers it without storage being required", () => {
  assert.match(source, /presence=\$\{LIB_PRESENCE\}/);
  assert.match(source, /let LIB_PRESENCE="disk";\ntry\{[^\n]*localStorage[^\n]*catch\(e\)\{\}/);
  for (const layout of ["classicLibraryShell", "catalogLibraryShell", "theaterLibraryShell"]) {
    if (source.includes(`function ${layout}(`)) assert.match(declaration(layout), /libPresenceSelect\(m\.reload\)/, layout);
  }
  assert.equal((source.match(/libPresenceSelect\(m\.reload\)/g) || []).length, 3);
});

// The status read runs the page's own loop against a fake DOM, timers and api.
function watchContext(apiImpl) {
  const timers = [];
  const line = {textContent: "Offloaded"};
  const dom = {present: false};
  const ctx = vm.createContext({
    document: {hidden: false, getElementById: id => (id === "offstatus" && dom.present ? line : null),
               addEventListener() {}, removeEventListener() {}},
    location: {hash: "#/item/7"},
    api: apiImpl, render: () => { ctx.rendered = (ctx.rendered || 0) + 1; },
    setInterval: fn => { timers.push(fn); return timers.length; }, clearInterval() {},
    setTimeout: fn => { timers.push(fn); return timers.length; },
    offloadLine: a => a.message || a.state || "",
  });
  const start = source.indexOf("let OFFWATCH=null;");
  const end = source.indexOf("\n}\n", source.indexOf("function watchOffload(")) + 3;
  assert.notEqual(start, -1);
  vm.runInContext(source.slice(start, end), ctx);
  return {ctx, line, dom, timers};
}

test("the status read starts after the page is inserted and shows Curator's answer", async () => {
  const w = watchContext(async path => { assert.match(path, /\/items\/7\/offload-status/); return {state: "downloading", message: "Curator is downloading it."}; });
  vm.runInContext("watchOffload(7)", w.ctx);
  assert.equal(w.line.textContent, "Offloaded", "nothing is read before the body exists");
  w.dom.present = true; // the renderer inserts the page after watchOffload returns
  await w.timers[w.timers.length - 1]();
  assert.equal(w.line.textContent, "Offloaded · Curator is downloading it.");
});

test("an unreachable Curator reads so, and a title no longer offloaded reloads the page", async () => {
  let mode = "down";
  const w = watchContext(async () => {
    if (mode === "down") throw new Error("boom");
    const e = new Error("not offloaded"); e.code = "not_offloaded"; throw e;
  });
  w.dom.present = true;
  vm.runInContext("watchOffload(7)", w.ctx);
  await w.timers[w.timers.length - 1]();
  assert.equal(w.line.textContent, "Offloaded · curator unreachable");
  assert.equal(w.ctx.rendered, undefined);
  vm.runInContext("stopOffloadWatch()", w.ctx);
  mode = "back";
  vm.runInContext("watchOffload(7)", w.ctx);
  await w.timers[w.timers.length - 1]();
  assert.equal(w.ctx.rendered, 1, "a 409 not_offloaded reloads the item");
});
