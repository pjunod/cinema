"use strict";

// Sections with a server enable switch — Shared libraries, Live TV, Recordings,
// Library channels — are not offered in any layout's top-level nav while the
// switch is off. The routes stay reachable; only the nav follows /features.
// An unknown answer (an older server without the route, a failed read) shows
// every section, so a server that predates the route keeps its whole nav.
//
// The functions are sliced out of the shipped shell and executed, the same
// pattern tests/web/local-search.test.js uses for the three chromes.

const {test} = require("node:test");
const assert = require("node:assert/strict");
const vm = require("node:vm");
const {shellSource} = require("./shell-source.js");

const shell = shellSource().bodyScript;
function source(name) {
  const match = new RegExp(`\\n(?:async )?function ${name}\\(`).exec(shell);
  assert.ok(match, `the shell no longer declares ${name}()`);
  const start = match.index + 1;
  return shell.slice(start, shell.indexOf("\n}", start) + 2);
}
function constant(name) {
  const start = shell.indexOf(`\nconst ${name}=`);
  assert.ok(start >= 0, `the shell no longer declares ${name}`);
  return shell.slice(start + 1, shell.indexOf(";\n", start) + 1);
}

const GATED = ["#/shared", "#/live-tv", "#/recordings", "#/library-channels"];
const ALL_OFF = {live_tv: false, recordings: false, shared_libraries: false, library_channels: false};

function harness(features, {admin = true, hash = "#/", nodes = {}, api} = {}) {
  const calls = {navChanged: 0, requests: []};
  const context = vm.createContext({
    FEATURES: features, ME: {is_admin: admin, username: "viewer"}, location: {hash},
    catalogIcon: (name) => `<i data-icon="${name}"></i>`, esc: String,
    layoutNavChanged: () => { calls.navChanged += 1; },
    api: api || (async (path) => { calls.requests.push(path); return {}; }),
    document: {getElementById: (id) => nodes[id] || null},
    DVR_SHARED: {overview: {counts: {recording: 1}}, receivedAt: 0},
    dvrOverviewFresh: () => true, dvrIndicatorText: () => "1 recording",
  });
  vm.runInContext([
    constant("NAV_FEATURES"),
    // The settings cache's own bookkeeping globals, so the real cacheSettings()
    // runs: it is the path every admin save takes.
    "var SETTINGS=null, SETTINGS_DATA={}; const SETTINGS_LOADED=new Set();",
    ...["featureVisible", "classicNavHtml", "theaterNavItems", "catalogTabsHtml", "catalogMainNavHtml",
      "catalogHomeOn", "loadFeatures", "refreshFeatures", "cacheSettings", "dvrPaintGlobal"].map(source),
    // `let` bindings are not context properties; expose FEATURES for asserting.
    "this.features=()=>FEATURES;",
  ].join("\n"), context);
  return {context, calls};
}

function navsOf(context) {
  return {
    classic: context.classicNavHtml("home"),
    catalogSidebar: context.catalogMainNavHtml("home", true),
    catalogTabs: context.catalogTabsHtml("home", true),
    theater: context.theaterNavItems("home").map((item) => `href="${item.href}"`).join(" "),
  };
}

test("only the four switched sections map to a feature, and unknown shows all", () => {
  const unknown = harness(null).context;
  for (const tab of ["home", "shared", "live-tv", "recordings", "library-channels", "activity", "settings", "libs", "search"]) {
    assert.equal(unknown.featureVisible(tab), true, `${tab} with FEATURES unknown`);
  }
  const off = harness({...ALL_OFF}).context;
  for (const tab of ["shared", "live-tv", "recordings", "library-channels"]) assert.equal(off.featureVisible(tab), false, tab);
  for (const tab of ["home", "activity", "settings", "libs", "search"]) assert.equal(off.featureVisible(tab), true, tab);
  // A key the server did not send is not a "no".
  assert.equal(harness({live_tv: false}).context.featureVisible("recordings"), true);
});

test("every layout drops switched-off sections and keeps the always-on ones", () => {
  for (const [layout, html] of Object.entries(navsOf(harness({...ALL_OFF}).context))) {
    for (const href of GATED) assert.equal(html.includes(`href="${href}"`), false, `${layout} still links ${href}`);
    assert.match(html, /href="#\/"/, `${layout} lost Home`);
  }
  const off = navsOf(harness({...ALL_OFF}).context);
  for (const layout of ["classic", "catalogTabs", "theater"]) assert.match(off[layout], /href="#\/activity"/, layout);
  assert.match(off.classic, /href="#\/settings"/);
  assert.match(off.theater, /href="#\/settings"/);
  // The catalog tab bar's Libraries and Search are panels, not sections.
  assert.match(off.catalogTabs, /data-tab="libs"/);
  assert.match(off.catalogTabs, /data-tab="search"/);
});

test("each switch hides only its own section", () => {
  const pairs = {live_tv: "#/live-tv", recordings: "#/recordings", shared_libraries: "#/shared", library_channels: "#/library-channels"};
  for (const [key, hidden] of Object.entries(pairs)) {
    const features = {live_tv: true, recordings: true, shared_libraries: true, library_channels: true, [key]: false};
    for (const [layout, html] of Object.entries(navsOf(harness(features).context))) {
      for (const href of GATED) {
        // The catalog tab bar never listed Live TV; the sidebar beside it does.
        if (layout === "catalogTabs" && href === "#/live-tv") continue;
        assert.equal(html.includes(`href="${href}"`), href !== hidden, `${layout} ${key}=false ${href}`);
      }
    }
  }
});

test("an unknown or all-on answer keeps the shipped nav markup", () => {
  const expected = `<a href="#/" class="active">Home</a><a href="#/shared" class="">Shared libraries</a><a href="#/live-tv" class="">Live TV</a><a href="#/recordings" class="">Recordings</a><a href="#/library-channels" class="">Library channels</a><a href="#/activity" class="">Activity</a><a href="#/settings" class="">Settings</a>`;
  assert.equal(harness(null).context.classicNavHtml("home"), expected);
  assert.equal(harness({live_tv: true, recordings: true, shared_libraries: true, library_channels: true}).context.classicNavHtml("home"), expected);
  assert.equal(harness(null, {admin: false}).context.classicNavHtml("live-tv").includes("Settings"), false);
  assert.match(harness(null).context.classicNavHtml("live-tv"), /<a href="#\/live-tv" class="active">/);
});

test("a failed /features read never hides a section and keeps the last answer", async () => {
  const fresh = harness(null, {api: async () => { throw new Error("not found"); }});
  assert.equal(await fresh.context.loadFeatures(), null);
  assert.equal(fresh.context.featureVisible("live-tv"), true);

  const known = harness({...ALL_OFF}, {api: async () => { throw new Error("network"); }});
  await known.context.loadFeatures();
  assert.deepEqual({...known.context.features()}, ALL_OFF);

  const malformed = harness(null, {api: async () => [false]});
  assert.equal(await malformed.context.loadFeatures(), null);

  const served = harness(null, {api: async (path) => { assert.equal(path, "/features"); return {...ALL_OFF, live_tv: true}; }});
  await served.context.loadFeatures();
  assert.equal(served.context.featureVisible("live-tv"), true);
  assert.equal(served.context.featureVisible("recordings"), false);
});

// Let a fire-and-forget refresh (api -> loadFeatures -> refreshFeatures) land.
const settle = () => new Promise((done) => setImmediate(done));

test("an admin's settings save re-reads /features and repaints the nav", async () => {
  let answer = {live_tv: true, recordings: true, shared_libraries: true, library_channels: true};
  const requests = [];
  const {context, calls} = harness({...answer}, {api: async (path) => { requests.push(path); return {...answer}; }});
  // The save's own payload is never read for switches: /features is the truth.
  answer = {...answer, live_tv: false, recordings: false};
  const saved = {live_tv_enabled: true, dvr_enabled: true};
  assert.equal(context.cacheSettings(saved), saved, "cacheSettings still returns the saved settings");
  await settle();
  assert.deepEqual(requests, ["/features"]);
  assert.equal(calls.navChanged, 1);
  assert.equal(context.featureVisible("live-tv"), false);
  assert.equal(context.featureVisible("recordings"), false);
  assert.equal(context.featureVisible("shared"), true);
  // Same answer again: re-read, but nothing to repaint.
  context.cacheSettings({});
  await settle();
  assert.deepEqual(requests, ["/features", "/features"]);
  assert.equal(calls.navChanged, 1);
});

test("a refused re-read keeps the nav as it was and never rejects", async () => {
  const {context, calls} = harness({...ALL_OFF}, {api: async () => { throw new Error("network"); }});
  context.cacheSettings({});
  await settle();
  assert.equal(calls.navChanged, 0);
  assert.deepEqual({...context.features()}, ALL_OFF);
  assert.equal(await context.refreshFeatures(), false);
  // An older server without the route: unknown stays unknown, everything shows.
  const old = harness(null, {api: async () => { throw new Error("not found"); }});
  old.context.cacheSettings({live_tv_enabled: false});
  await settle();
  assert.equal(old.context.features(), null);
  assert.equal(old.context.featureVisible("live-tv"), true);
  assert.equal(old.calls.navChanged, 0);
});

test("cacheSettings is safe without the features loader", async () => {
  const context = vm.createContext({});
  vm.runInContext(["var SETTINGS=null, SETTINGS_DATA={}; const SETTINGS_LOADED=new Set();",
    source("cacheSettings")].join("\n"), context);
  assert.deepEqual({...context.cacheSettings({a: 1})}, {a: 1});
  const partial = vm.createContext({});
  vm.runInContext(["var SETTINGS=null, SETTINGS_DATA={}; const SETTINGS_LOADED=new Set();",
    source("refreshFeatures"), source("cacheSettings")].join("\n"), partial);
  assert.deepEqual({...partial.cacheSettings({b: 2})}, {b: 2});
  assert.equal(await partial.refreshFeatures(), false);
});

test("the DVR header indicator stays dark while Recordings is off", () => {
  const mount = {className: "", innerHTML: "", title: ""};
  harness({...ALL_OFF}, {nodes: {"dvr-global": mount}}).context.dvrPaintGlobal();
  assert.equal(mount.className, "dvr-global");
  assert.equal(mount.innerHTML, "");
  harness({...ALL_OFF, recordings: true}, {nodes: {"dvr-global": mount}}).context.dvrPaintGlobal();
  assert.equal(mount.className, "dvr-global on");
  assert.match(mount.innerHTML, /1 recording/);
});
