"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "../../crates/plurxd/src/web/pages/curation.js"), "utf8");

test("saving a new row twice sends one create and moving it saves one entry", async () => {
  const calls = [];
  const button = {disabled: false};
  const body = {};
  const rowDialog = {
    open: true,
    close() { this.open = false; },
    querySelector(selector) { return selector === "#row-save" ? button : null; },
  };
  const homeDialog = {close() {}, querySelector() { return body; }};
  const creation = Promise.withResolvers();
  const context = vm.createContext({
    window: {addEventListener() {}},
    document: {getElementById: id => id === "row-editor" ? rowDialog : homeDialog},
    api(url, options) {
      calls.push({url, options});
      return url === "/rows" ? creation.promise : Promise.resolve(null);
    },
    esc: String,
    toast() {},
    render() {},
    location: {hash: "#/"},
  });
  vm.runInContext(source + `
    HOME_EDIT={entries:[{key:"recently_added",hidden:false}],rows:[]};
    ROW_EDIT={id:null,title:"Trips",scope:"private",spec:{all_of:[{tag:7}]},saving:false};
  `, context);

  const first = vm.runInContext("saveRow()", context);
  await vm.runInContext("saveRow()", context);
  assert.equal(button.disabled, true);
  assert.deepEqual(calls.map(call => call.url), ["/rows"]);
  creation.resolve({id: 21, title: "Trips", scope: "private", editable: true});
  await first;
  assert.equal(rowDialog.open, false);
  await vm.runInContext("moveHomeEntry(1,-1);saveHomeLayout(false)", context);
  assert.deepEqual(calls.map(call => call.url), ["/rows", "/home/layout"]);
  assert.deepEqual(Array.from(calls[1].options.body.entries, entry => entry.key), ["row:21", "recently_added"]);
});
