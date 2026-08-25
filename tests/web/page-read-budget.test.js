#!/usr/bin/env node
"use strict";

// Request-shape regression tests against the shipped single-file web app.
// Cluster latency is dominated by how many authoritative reads a page fans
// out and how many sequential waves it creates, so these are behavior gates,
// not wall-clock benchmarks on a noisy CI runner.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const SHIPPED_UI = fs.readFileSync(INDEX, "utf8");
const DECLARATIONS = ["\nfunction ", "\nasync function "];

function shippedSource(name) {
  const start = DECLARATIONS.map((kind) =>
    SHIPPED_UI.indexOf(`${kind}${name}(`),
  ).find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter(
    (at) => at !== -1,
  );
  const end = ends.length ? Math.min(...ends) : -1;
  return (end === -1 ? rest : rest.slice(0, end)).trimEnd();
}

function shippedTopLevelSource(name) {
  const source = shippedSource(name);
  const end = source.indexOf("\n}");
  assert.notEqual(end, -1, `${name} has no top-level closing brace`);
  return source.slice(0, end + 2);
}

let failures = 0;
async function test(name, run) {
  try {
    await run();
    process.stdout.write(`PASS ${name}\n`);
  } catch (error) {
    failures += 1;
    process.stderr.write(`FAIL ${name}\n${error && error.stack}\n`);
  }
}

function nextTurn() {
  return new Promise((resolve) => setImmediate(resolve));
}

test("Home uses one fixed preview batch and preserves category grouping", async () => {
  const libraries = Array.from({ length: 14 }, (_, index) => ({
    id: index + 1,
    name: `Library ${index + 1}`,
    kind: index % 2 === 0 ? "movies" : "shows",
  }));
  const homeGroup = () => "category";
  const libCategory = (lib) => lib.kind === "movies"
    ? { key: "movie", name: "Movies" }
    : { key: "show", name: "TV Shows" };
  const catOrder = (key) => key === "movie" ? 0 : 1;
  const buildHomeSections = new Function(
    "homeGroup", "libCategory", "catOrder",
    `${shippedSource("buildHomeSections")}; return buildHomeSections;`,
  )(homeGroup, libCategory, catOrder);
  const page = buildHomeSections({ libraries: libraries.map((library) => ({
    library, items: [{ id: library.id * 10 }], total: 1,
  })) });
  assert.deepEqual(page.sections.map((section) => section.key), ["movie", "show"]);
  assert.equal(page.sections[0].total, 7);
  assert.deepEqual(page.sections[0].items.map((item) => item.id), [10, 30, 50, 70, 90, 110, 130]);
  assert.deepEqual(page.sections[1].items.map((item) => item.id), [20, 40, 60, 80, 100, 120, 140]);
  const source = shippedSource("viewHome");
  assert.match(source, /api\("\/hubs"\)/);
  assert.match(source, /api\("\/home\/previews"\)/);
  assert.match(source, /api\("\/coming-soon"\)/);
  assert.doesNotMatch(source, /\/libraries\/\$\{/);
});

test("Home independently commits three regions and settles after all finish", async () => {
  const source = shippedSource("viewHome");
  assert.match(source, /Promise\.allSettled/);
  assert.match(source, /if\(error&&error\.status===401\) throw error/);
  assert.match(source, /const meaningful=apply\(value\); commit\(name,meaningful\)/);
  assert.match(source, /region\("soon"[\s\S]*return !!\(value\.entries\|\|\[\]\)\.length/);
  assert.doesNotMatch(source, /commit\(true\)/,
    "an empty optional response cannot claim meaningful content");
  assert.match(source, /main\.querySelector\([\s\S]*old\.replaceWith\(next\)/,
    "region completions replace only their stable mount");
  assert.match(source, /if\(name==="previews"\) restoreScroll\(\)/,
    "scroll restoration waits for the height-owning preview region");
  assert.match(source, /keepPhotos=name==="previews"\?null:PHOTO_SET/,
    "late optional regions cannot reset an open photo lightbox");
  assert.match(source, /setPagePhase\(route,generation,"settled"\)/);
});

test("Activity suppresses duplicate and overlapping cluster polls", async () => {
  const globalPoll = shippedSource("pollActivity");
  assert.match(globalPoll, /location\.hash==="#\/activity"/);
  assert.match(globalPoll, /document\.visibilityState==="hidden"/);
  assert.match(globalPoll, /\|\|ACT_POLLING\) return/);
  assert.match(globalPoll, /finally\{ ACT_POLLING=false; \}/);

  const activity = { style: {}, innerHTML: "" };
  const document = {
    visibilityState: "visible",
    getElementById: (id) => id === "activity" ? activity : null,
    documentElement: { classList: { toggle() {} } },
  };
  const location = { hash: "#/" };
  const requests = [];
  const api = (url) => new Promise((resolve) => requests.push({ url, resolve }));
  const harness = new Function(
    "document", "location", "api", "esc",
    `let TOKEN="token",ME={is_admin:false},ACT_POLLING=false,PAGE_RENDER_GENERATION=1;
     ${shippedSource("paintActivity")};
     ${globalPoll};
     return {pollActivity,busy:()=>ACT_POLLING,navigate:(hash)=>{location.hash=hash;PAGE_RENDER_GENERATION++;}};`,
  )(document, location, api, (value) => String(value));

  const first = harness.pollActivity();
  const overlap = harness.pollActivity();
  assert.equal(requests.length, 1);
  assert.equal(harness.busy(), true);
  activity.innerHTML = "detail-owned";
  harness.navigate("#/activity");
  requests[0].resolve([{ label: "Scanning" }]);
  await Promise.all([first, overlap]);
  assert.equal(harness.busy(), false);
  assert.equal(activity.innerHTML, "detail-owned", "a stale global response cannot replace detail state");

  harness.navigate("#/");
  const current = harness.pollActivity();
  assert.equal(requests.length, 2);
  requests[1].resolve([{ label: "Scanning" }]);
  await current;
  assert.equal(activity.style.display, "flex");

  harness.navigate("#/activity");
  await harness.pollActivity();
  assert.equal(requests.length, 2, "detail page owns its own activity read");
  harness.navigate("#/");
  document.visibilityState = "hidden";
  await harness.pollActivity();
  assert.equal(requests.length, 2, "hidden pages do not poll");
});

test("Activity detail response keeps the shared header indicator current", () => {
  const activity = { style: {}, innerHTML: "" };
  const working = [];
  const document = {
    getElementById: () => activity,
    documentElement: { classList: { toggle: (_, value) => working.push(value) } },
  };
  const harness = new Function(
    "document", "esc",
    `${shippedSource("paintActivity")};
     ${shippedSource("activityNodeFailures")};
     ${shippedSource("detailActivitySummary")};
     return {paintActivity,detailActivitySummary};`,
  )(document, (value) => String(value));
  const detail = {
    scans: [{ library: "Films", status: { running: true } }],
    producing: null,
    offline: [{ id: "one" }],
    trakt: { syncing: false },
  };
  harness.paintActivity(harness.detailActivitySummary(detail, [{ id: "stream" }]));
  assert.equal(working.at(-1), true);
  assert.equal(activity.style.display, "flex");
  assert.match(activity.innerHTML, /Scanning library/);
  assert.match(shippedSource("paintActivityBody"), /paintActivity\(detailActivitySummary\(d,dels\)\)/);

  harness.paintActivity([]);
  assert.equal(working.at(-1), false);
  assert.equal(activity.style.display, "none");
});

test("Activity names missing cluster nodes and attributes delivered rows", () => {
  const harness = new Function(
    `${shippedSource("activityNodeFailures")};
     ${shippedSource("activityNodeStatusText")};
     ${shippedSource("activityNodeFailureText")};
     ${shippedSource("detailActivitySummary")};
     return {activityNodeFailures,activityNodeFailureText,detailActivitySummary};`,
  )();
  const detail = {
    activity_nodes: [
      { node_id: "node-a", status: "answered" },
      { node_id: "node-b", status: "timed_out" },
      { node_id: "node-c", status: "unhealthy" },
    ],
    scans: [], producing: null, offline: [], trakt: { syncing: false },
  };
  const missing = harness.activityNodeFailures(detail);
  assert.deepEqual(missing.map((node) => node.node_id), ["node-b", "node-c"]);
  assert.equal(harness.activityNodeFailureText(missing[0]), "Node node-b · timed out");
  assert.deepEqual(harness.detailActivitySummary(detail, []), [{
    label: "Activity incomplete",
    detail: "2 cluster nodes did not answer",
  }]);

  const painter = shippedSource("paintActivityBody");
  assert.match(painter, /role="status" aria-live="polite"/);
  assert.match(painter, /de\.node_id/);
  assert.match(painter, /<th>Node<\/th>/);
  assert.match(painter, /Streams on those nodes may be missing/);
});

test("Activity detail request guard executes one current request and releases", async () => {
  const location = { hash: "#/activity" };
  const main = { innerHTML: "" };
  const document = { visibilityState: "visible", getElementById: () => main };
  const requests = [];
  const painted = [];
  const phases = [];
  const api = (url) => new Promise((resolve, reject) => requests.push({ url, resolve, reject }));
  const harness = new Function(
    "document", "location", "api", "paintActivityBody", "esc", "setPagePhase", "setPageFailure",
    `let PAGE_RENDER_GENERATION=1,ACTIVITY_DETAIL_BUSY=0;
     ${shippedSource("renderActivityBody")};
     return {renderActivityBody,busy:()=>ACTIVITY_DETAIL_BUSY,
       navigate:(hash)=>{location.hash=hash;PAGE_RENDER_GENERATION++;},generation:()=>PAGE_RENDER_GENERATION};`,
  )(
    document, location, api, (detail) => painted.push(detail),
    (value) => String(value), (_, generation, phase) => phases.push({ generation, phase }),
    () => {},
  );

  const first = harness.renderActivityBody();
  const overlap = harness.renderActivityBody();
  assert.deepEqual(requests.map((request) => request.url), ["/activity/detail"]);
  assert.equal(harness.busy(), 1);
  assert.deepEqual(phases, [], "Activity content waits for its delayed detail response");
  requests[0].resolve({ marker: "current" });
  await Promise.all([first, overlap]);
  assert.deepEqual(painted, [{ marker: "current" }]);
  assert.deepEqual(phases, [
    { generation: 1, phase: "content" },
    { generation: 1, phase: "settled" },
  ]);
  assert.equal(harness.busy(), 0);

  const stale = harness.renderActivityBody();
  assert.equal(requests.length, 2);
  harness.navigate("#/settings");
  requests[1].resolve({ marker: "stale" });
  await stale;
  assert.deepEqual(painted, [{ marker: "current" }]);
  assert.equal(harness.busy(), 0);

  harness.navigate("#/activity");
  document.visibilityState = "hidden";
  await harness.renderActivityBody(harness.generation());
  assert.equal(requests.length, 2);
});

test("Activity keeps its last successful body on poll failures and never paints a 401", async () => {
  const location = { hash: "#/activity" };
  let stale = null;
  const main = {
    innerHTML: "last successful body",
    prepend(node) { stale = node; },
  };
  const document = {
    visibilityState: "visible",
    getElementById: (id) => id === "main" ? main : id === "activity-stale" ? stale : null,
    createElement: () => ({ id: "", className: "", textContent: "" }),
  };
  const requests = [];
  const phases = [];
  const api = () => new Promise((resolve, reject) => requests.push({ resolve, reject }));
  const harness = new Function(
    "document", "location", "api", "paintActivityBody", "esc", "setPagePhase", "setPageFailure",
    `let PAGE_RENDER_GENERATION=1,ACTIVITY_DETAIL_BUSY=0,ACTIVITY_SNAPSHOT={marker:"old"};
     ${shippedSource("renderActivityBody")};
     return {renderActivityBody,clearSnapshot:()=>{ACTIVITY_SNAPSHOT=null;}};`,
  )(
    document, location, api, () => { throw new Error("failure must not repaint"); },
    String, (_, generation, phase) => phases.push({ generation, phase }), () => {},
  );
  const failed = harness.renderActivityBody();
  requests[0].reject(new Error("cluster timeout"));
  await failed;
  assert.equal(main.innerHTML, "last successful body");
  assert.match(stale.textContent, /Showing the last update/);
  assert.deepEqual(phases.map((entry) => entry.phase), ["content", "settled"]);

  phases.length = 0; stale = null; harness.clearSnapshot();
  const unauthorized = harness.renderActivityBody();
  const error = new Error("unauthorized"); error.status = 401;
  requests[1].reject(error);
  await unauthorized;
  assert.equal(stale, null);
  assert.deepEqual(phases, [], "logout owns the 401 transition; Activity commits nothing");
});

test("Settings polls only the visible data panel and never overlaps", async () => {
  const tick = shippedSource("settingsTick");
  assert.match(tick, /SETTINGS_TICKING\.generation===generation/);
  assert.match(tick, /if\(tab==="libraries"\)\{[\s\S]*api\("\/scan\/status"\)/);
  assert.match(tick, /if\(tab==="metadata"&&!TRAKT_EDIT\)\{[\s\S]*api\("\/trakt\/status"\)/);
  assert.match(tick, /await Promise\.all\(secondary\)/);
  assert.match(tick, /finally\{ if\(SETTINGS_TICKING===owner\) SETTINGS_TICKING=null; \}/);

  const location = { hash: "#/settings" };
  const document = { visibilityState: "visible", getElementById: () => null };
  const requests = [];
  let tab = "libraries";
  const api = (url) => new Promise((resolve) => requests.push({ url, resolve }));
  const harness = new Function(
    "document", "location", "api", "settingsTab", "refreshLogs", "refreshClusterLogs", "paintTrakt", "settingsCurrent",
    `let PAGE_RENDER_GENERATION=1,AUTH_GENERATION=1,SETTINGS_TICKING=null,TRAKT_EDIT=false,TRAKT=null,
       SETTINGS_DATA={}; const SETTINGS_LOADED=new Set();
     const cacheTrakt=(value)=>{TRAKT=value;SETTINGS_DATA.trakt=value;return value;};
     ${tick}; return {settingsTick,busy:()=>SETTINGS_TICKING};`,
  )(
    document, location, api, () => tab,
    async () => {}, async () => {}, () => {}, (_, expected) => expected === tab,
  );

  const first = harness.settingsTick();
  const overlap = harness.settingsTick();
  assert.deepEqual(requests.map((request) => request.url), ["/scan/status"]);
  assert.equal(!!harness.busy(), true);
  requests[0].resolve({});
  await Promise.all([first, overlap]);
  assert.equal(harness.busy(), null);

  tab = "playback";
  await harness.settingsTick();
  assert.equal(requests.length, 1, "static settings tabs do not poll store state");
  tab = "metadata";
  const metadata = harness.settingsTick();
  assert.equal(requests[1].url, "/trakt/status");
  requests[1].resolve({ configured: false });
  await metadata;
  document.visibilityState = "hidden";
  await harness.settingsTick();
  assert.equal(requests.length, 2);
});

test("an old Settings tick cannot block or release a newer generation", async () => {
  const location={hash:"#/settings"}, requests=[]; let tab="libraries";
  const document={visibilityState:"visible",getElementById:()=>null};
  const api=(url)=>new Promise(resolve=>requests.push({url,resolve}));
  const harness=new Function(
    "document","location","api","settingsTab","settingsCurrent","refreshLogs","refreshClusterLogs","paintTrakt",
    `let PAGE_RENDER_GENERATION=1,AUTH_GENERATION=1,SETTINGS_TICKING=null,TRAKT_EDIT=false,TRAKT=null,
       SETTINGS_DATA={},SETTINGS_LOADED=new Set();
     const cacheTrakt=(value)=>{TRAKT=value;SETTINGS_DATA.trakt=value;return value;};
     ${shippedSource("settingsTick")};
     return {settingsTick,switchGeneration:()=>{PAGE_RENDER_GENERATION=2;},busy:()=>SETTINGS_TICKING};`,
  )(
    document,location,api,()=>tab,()=>true,async()=>{},async()=>{},()=>{},
  );
  const old=harness.settingsTick(1,"libraries");
  tab="metadata"; harness.switchGeneration();
  const current=harness.settingsTick(2,"metadata");
  assert.deepEqual(requests.map(request=>request.url),["/scan/status","/trakt/status"]);
  requests[0].resolve({}); await old;
  assert.equal(harness.busy().generation,2,"old finally cannot release the current tick owner");
  requests[1].resolve({configured:false}); await current;
  assert.equal(harness.busy(),null);
});

test("Settings log refreshes coalesce across initial, manual, and timer callers", async () => {
  const boxes = {
    logbox: { scrollHeight: 0, scrollTop: 0, clientHeight: 0, innerHTML: "", textContent: "" },
    cllogbox: { scrollHeight: 0, scrollTop: 0, clientHeight: 0, innerHTML: "", textContent: "" },
    loglvl: { value: "info" },
    clloglvl: { value: "warn" },
  };
  const document = { getElementById: (id) => boxes[id] || null };
  const requests = [];
  const api = (url) => new Promise((resolve) => requests.push({ url, resolve }));
  const harness = new Function(
    "document", "api", "esc", "fmtTs",
    `let AUTH_GENERATION=1,LOGS_RUN=null,CLUSTER_LOGS_RUN=null;
     ${shippedSource("sameLogRequest")};
     ${shippedSource("runLogRequest")};
     ${shippedSource("refreshLogStream")};
     ${shippedSource("refreshLogs")};
     ${shippedSource("refreshClusterLogs")};
     return {refreshLogs,refreshClusterLogs};`,
  )(document, api, (value) => String(value), (value) => String(value));

  const logInitial = harness.refreshLogs();
  const logManual = harness.refreshLogs();
  assert.deepEqual(requests.map((request) => request.url), ["/system/logs?level=info&limit=300"]);
  boxes.loglvl.value = "error";
  const changedLevel = harness.refreshLogs();
  const oldBox = boxes.logbox;
  boxes.logbox = { scrollHeight: 0, scrollTop: 0, clientHeight: 0, innerHTML: "", textContent: "" };
  const changedBox = harness.refreshLogs();
  assert.equal(requests.length, 1, "changed filters coalesce behind the active request");
  requests[0].resolve([{ ts_ms: 1, level: "info", target: "test", message: "one" }]);
  await nextTurn();
  assert.equal(requests.length, 2, "only the latest pending target starts next");
  assert.equal(requests[1].url, "/system/logs?level=error&limit=300");
  requests[1].resolve([{ ts_ms: 3, level: "error", target: "new", message: "current" }]);
  await Promise.all([logInitial, logManual, changedLevel, changedBox]);
  assert.equal(oldBox.innerHTML, "", "a detached log box is not repainted");
  assert.match(boxes.logbox.innerHTML, /current/);

  const clusterInitial = harness.refreshClusterLogs();
  const clusterTimer = harness.refreshClusterLogs();
  assert.equal(requests.length, 3);
  assert.equal(requests[2].url, "/system/logs?scope=cluster&level=warn&limit=300");
  requests[2].resolve([]);
  await Promise.all([clusterInitial, clusterTimer]);

  const later = harness.refreshLogs();
  assert.equal(requests.length, 4, "a completed request releases the coalescing guard");
  requests[3].resolve([]);
  await later;
});

test("an old log run cannot consume or clear a new session's queue", async () => {
  let box={scrollHeight:0,scrollTop:0,clientHeight:0,innerHTML:"",textContent:""};
  const document={getElementById:(id)=>id==="logbox"?box:id==="loglvl"?{value:"info"}:null};
  const requests=[];
  const api=(url)=>new Promise((resolve,reject)=>requests.push({url,resolve,reject}));
  const harness=new Function("document","api","esc","fmtTs",
    `let AUTH_GENERATION=1,LOGS_RUN=null,CLUSTER_LOGS_RUN=null;
     ${shippedSource("sameLogRequest")};${shippedSource("runLogRequest")};
     ${shippedSource("refreshLogStream")};${shippedSource("refreshLogs")};
     return {refreshLogs,newSession:()=>{AUTH_GENERATION++;LOGS_RUN=null;}};`,
  )(document,api,String,String);
  const old=harness.refreshLogs();
  harness.newSession(); box={scrollHeight:0,scrollTop:0,clientHeight:0,innerHTML:"",textContent:""};
  const current=harness.refreshLogs();
  assert.equal(requests.length,2);
  const unauthorized=new Error("unauthorized"); unauthorized.status=401;
  requests[0].reject(unauthorized); await assert.rejects(old,/unauthorized/);
  const overlap=harness.refreshLogs();
  assert.equal(requests.length,2,"old finally leaves the new log run's coalescer intact");
  requests[1].resolve([]); await Promise.all([current,overlap]);
});

test("Home render generations refuse out-of-order route completions", async () => {
  const location = { hash: "#/" };
  const main = {
    slots: {},
    get innerHTML() { return JSON.stringify(this.slots); },
    set innerHTML(value) {
      const page = JSON.parse(value);
      for (const region of ["hubs", "previews", "soon"]) this.slots[region] = page;
    },
    querySelector(selector) {
      const region = /data-home-region="([^"]+)"/.exec(selector)[1];
      return { replaceWith: (next) => { this.slots[region] = next.page; } };
    },
  };
  const document = {
    getElementById: () => main,
    createElement: () => {
      let page;
      return {
        set innerHTML(value) { page = JSON.parse(value); },
        content: { querySelectorAll(selector) {
          const region = /data-home-region="([^"]+)"/.exec(selector)[1];
          return [{ dataset: { homeSlot: region }, page }];
        } },
      };
    },
  };
  const requests = [];
  const phases = [];
  const api = (url) => new Promise((resolve, reject) => requests.push({ url, resolve, reject }));
  const harness = new Function(
    "document", "location", "api", "homeGroup", "buildHomeSections", "layoutChrome", "layoutView", "wireRails", "restoreScroll", "setPagePhase",
    `let PAGE_RENDER_GENERATION=0,PHOTO_SET=[],LB_AT=-1;
     ${shippedTopLevelSource("viewHome")};
     return {viewHome,navigate:(hash)=>{location.hash=hash;PAGE_RENDER_GENERATION++;}};`,
  )(
    document, location, api, () => "share",
    (batch, group) => ({ libs: batch.libraries.map((row) => row.library), group, sections: batch.libraries }),
    (_, inner) => { main.innerHTML = inner; }, (_, page) => JSON.stringify(page),
    () => {}, () => {}, (_, generation, phase) => phases.push({ generation, phase }),
  );

  const current = harness.viewHome();
  assert.deepEqual(phases, [{ generation: 1, phase: "shell" }],
    "the shell phase commits before any delayed endpoint resolves");
  assert.deepEqual(requests.map((request) => request.url), ["/hubs", "/home/previews", "/coming-soon"]);
  requests[1].resolve({ libraries: [{ library: { id: 7 }, items: [], total: 0 }] });
  await nextTurn();
  assert.match(main.innerHTML, /\"id\":7/);
  requests[0].resolve({ continue_watching: [{ id: 9 }], next_up: [], recently_added: [] });
  await nextTurn();
  assert.match(main.innerHTML, /\"id\":7/);
  assert.match(main.innerHTML, /\"id\":9/,
    "a later region commit preserves the preview region already received");
  requests[2].reject(new Error("optional dependency unavailable"));
  await current;
  assert.deepEqual(phases, [
    { generation: 1, phase: "shell" },
    { generation: 1, phase: "content" },
    { generation: 1, phase: "content" },
    { generation: 1, phase: "settled" },
  ]);

  const stale = harness.viewHome();
  harness.navigate("#/settings");
  harness.navigate("#/");
  const latest = harness.viewHome();
  requests.slice(6, 9).forEach((request) => request.resolve(
    request.url === "/home/previews" ? { libraries: [{ library: { id: 42 }, items: [], total: 0 }] }
      : request.url === "/hubs" ? { continue_watching: [{ id: 43 }], next_up: [], recently_added: [] }
        : { configured: false, entries: [] },
  ));
  await latest;
  const latestHtml = main.innerHTML;
  requests.slice(3).forEach((request) => request.resolve(
    request.url === "/home/previews" ? { libraries: [] }
      : request.url === "/hubs" ? { continue_watching: [], next_up: [], recently_added: [] }
        : { configured: false, entries: [] },
  ));
  await stale;
  assert.equal(main.innerHTML, latestHtml,
    "an old Home response cannot repaint a later Home generation after an A→B→A route race");

  const before=requests.length, fifty=harness.viewHome();
  const cohort=Array.from({length:50},(_,index)=>({library:{id:index+1},items:[],total:0}));
  requests.slice(before).forEach((request) => request.resolve(
    request.url === "/home/previews" ? {libraries:cohort}
      : request.url === "/hubs" ? {continue_watching:[],next_up:[],recently_added:[]}
        : {configured:false,entries:[]},
  ));
  await fifty;
  assert.deepEqual(requests.slice(before).map((request)=>request.url),
    ["/hubs","/home/previews","/coming-soon"],
    "fifty libraries retain the same three-request Home budget as one library");
});

test("Activity stale loads cannot install the page timer", async () => {
  const activityLocation = { hash: "#/activity" };
  const activityLoads = [];
  const activityPhases = [];
  const timers = [];
  const activityHarness = new Function(
    "location", "layoutChrome", "renderActivityBody", "setPageTimer", "setPagePhase", "paintActivityBody",
    `let PAGE_RENDER_GENERATION=0,ACTIVITY_SNAPSHOT=null;
     ${shippedTopLevelSource("viewActivity")};
     return {viewActivity,navigate:(hash)=>{location.hash=hash;PAGE_RENDER_GENERATION++;}};`,
  )(
    activityLocation, () => {},
    (generation) => new Promise((resolve) => activityLoads.push({ generation, resolve })),
    (_, __, generation) => timers.push(generation),
    (_, generation, phase) => activityPhases.push({ generation, phase }),
    () => {},
  );
  const oldActivity = activityHarness.viewActivity();
  assert.deepEqual(activityPhases, [{ generation: 1, phase: "shell" }],
    "Activity shell commits before its delayed detail body");
  activityHarness.navigate("#/settings");
  activityLoads[0].resolve();
  await oldActivity;
  assert.deepEqual(timers, []);
  activityHarness.navigate("#/activity");
  const currentActivity = activityHarness.viewActivity();
  activityLoads[1].resolve();
  await currentActivity;
  assert.equal(timers.length, 1);
});

test("Activity paints its cached body before a slow current refresh", async () => {
  const location={hash:"#/activity"}, phases=[], painted=[], timers=[];
  let release;
  const harness=new Function(
    "location","layoutChrome","renderActivityBody","setPageTimer","setPagePhase","paintActivityBody",
    `let PAGE_RENDER_GENERATION=1,ACTIVITY_SNAPSHOT={marker:"cached"};
     ${shippedTopLevelSource("viewActivity")}; return {viewActivity};`,
  )(
    location,()=>{},()=>new Promise(resolve=>{release=resolve;}),
    (_,__,generation)=>timers.push(generation),(_,__,phase)=>phases.push(phase),
    (value)=>painted.push(value.marker),
  );
  const pending=harness.viewActivity(1);
  assert.deepEqual(painted,["cached"]);
  assert.deepEqual(phases,["shell","content"]);
  assert.deepEqual(timers,[]);
  release(); await pending;
  assert.deepEqual(timers,[1]);
});

test("Settings loads only the active tab manifest", () => {
  const declaration = SHIPPED_UI.match(/const SETTINGS_MANIFEST=({[\s\S]*?\n});/);
  assert.ok(declaration, "Settings endpoint manifest remains explicit and testable");
  const manifest = new Function(`${declaration[0]}; return SETTINGS_MANIFEST;`)();
  assert.deepEqual(manifest, {
    libraries: { required: ["settings", "libs", "status"], secondary: [] },
    metadata: { required: ["settings", "trakt"], secondary: ["libs"] },
    playback: { required: ["settings"], secondary: [] },
    users: { required: ["users"], secondary: [] },
    system: { required: ["sys"], secondary: ["playbackEvents"] },
    cluster: { required: ["cluster"], secondary: [] },
  });
  const view = shippedSource("viewSettings");
  assert.doesNotMatch(view, /Promise\.all\(\[\s*api/,
    "Settings no longer blocks every tab on a seven-endpoint page-wide wave");
  assert.match(view, /layoutChrome\("settings",settingsShell\(tab\)\)/);
  assert.ok(view.indexOf("await loadSettingsTab") < view.indexOf("setPageTimer"),
    "polling starts only after the required tab load, never over the initial read");
  const switchTab = shippedSource("setSettingsTab");
  assert.match(switchTab, /if\(t===settingsTab\(\)\) return/);
  assert.match(switchTab, /\+\+PAGE_RENDER_GENERATION/);
  assert.match(switchTab, /viewSettings\(\+\+PAGE_RENDER_GENERATION,false\)/);
  const loadTab = shippedSource("loadSettingsTab");
  assert.match(loadTab, /manifest\.required/);
  assert.match(loadTab, /patchSettingsSecondary/);
  assert.match(loadTab, /settingsCurrent\(generation,tab\)/);
  assert.match(loadTab, /patchSettingsSecondaryError/);
  for (const endpoint of ["/libraries", "/settings", "/scan/status", "/system", "/users", "/trakt/status", "/cluster/nodes"]) {
    assert.match(SHIPPED_UI, new RegExp(`api\\(${JSON.stringify(endpoint).replace("/", "\\/")}`),
      `Settings endpoint map includes ${endpoint}`);
  }
});

test("Settings drops a node-local scan error superseded by replicated success", () => {
  const currentScanStatus = new Function(
    `${shippedSource("currentScanStatus")}; return currentScanStatus;`,
  )();
  const failed = {
    running: false,
    finished_at: 100,
    error: "replicated store operation timed out",
  };
  assert.equal(currentScanStatus(failed, 100), null,
    "a success in the same server second supersedes the local failure");
  assert.equal(currentScanStatus(failed, 101), null);
  assert.equal(currentScanStatus(failed, 99), failed,
    "a newer failure remains actionable");
  assert.equal(currentScanStatus({ ...failed, running: true }, 101).running, true,
    "replicated history cannot hide live work");

  const statusText = new Function(
    "currentScanStatus", "esc", "fmtAgo",
    `${shippedSource("statusText")}; return statusText;`,
  )(currentScanStatus, (value) => String(value), () => "now");
  assert.equal(statusText(failed, 101), "idle");
  assert.match(statusText(failed, 99), /error: replicated store operation timed out/);
  assert.match(shippedSource("libRow"),
    /statusText\(status\[l\.id\],l\.last_scan_at\)/,
    "the first settings paint compares local status with durable history");
  assert.match(shippedSource("settingsTick"),
    /statusText\(st,lastScans\.get\(String\(id\)\)\)/,
    "polls retain the same stale-status projection");
});

test("Settings executes exact required and secondary waves for every tab", async () => {
  const endpointDeclaration=SHIPPED_UI.match(/const SETTINGS_ENDPOINTS=({[\s\S]*?\n};)/);
  const manifestDeclaration=SHIPPED_UI.match(/const SETTINGS_MANIFEST=({[\s\S]*?\n});/);
  assert.ok(endpointDeclaration&&manifestDeclaration);
  const cases={
    libraries:{required:["/settings","/libraries","/scan/status"],secondary:[]},
    metadata:{required:["/settings","/trakt/status"],secondary:["/libraries"]},
    playback:{required:["/settings"],secondary:[]},
    users:{required:["/users"],secondary:[]},
    system:{required:["/system"],secondary:["playback-events","system-log"]},
    cluster:{required:["/cluster/nodes"],secondary:["cluster-log"]},
  };
  for(const [tab,expected] of Object.entries(cases)){
    const requests=[], phases=[], logReleases=[];
    const api=(url)=>new Promise((resolve,reject)=>requests.push({url,resolve,reject}));
    const SETTINGS_ENDPOINTS=new Function("api",`${endpointDeclaration[0]};return SETTINGS_ENDPOINTS;`)(api);
    const SETTINGS_MANIFEST=new Function(`${manifestDeclaration[0]};return SETTINGS_MANIFEST;`)();
    const location={hash:"#/settings"};
    const harness=new Function(
      "location","SETTINGS_ENDPOINTS","SETTINGS_MANIFEST","settingsTab","renderSettings",
      "patchSettingsSecondary","patchSettingsSecondaryError","refreshLogs","refreshClusterLogs",
      "setPageFailure","setPagePhase","document",
      `let PAGE_RENDER_GENERATION=1,SETTINGS=null,TRAKT=null,CLUSTER_LOADED=false,
         SETTINGS_DATA={},SETTINGS_LOADED=new Set(),SETTINGS_LOADS=new Map();
       ${shippedSource("settingsCurrent")};
       ${shippedSource("cacheSettings")};
       ${shippedSource("cacheTrakt")};
       ${shippedSource("loadSettingsKey")};
       ${shippedSource("loadSettingsTab")};
       return {load:()=>loadSettingsTab(1,${JSON.stringify(tab)})};`,
    )(
      location,SETTINGS_ENDPOINTS,SETTINGS_MANIFEST,()=>tab,()=>phases.push("render"),
      ()=>phases.push("secondary-patch"),()=>phases.push("secondary-error"),
      ()=>new Promise(resolve=>logReleases.push({kind:"system-log",resolve})),
      ()=>new Promise(resolve=>logReleases.push({kind:"cluster-log",resolve})),
      ()=>{},(_,__,phase)=>phases.push(phase),{getElementById:()=>null},
    );
    const load=harness.load(); await nextTurn();
    assert.deepEqual(requests.map(request=>request.url),expected.required,`${tab} required wave`);
    for(const request of requests.slice()) request.resolve(
      request.url==="/settings"?{}:request.url==="/trakt/status"?{}:
        request.url==="/system"?{}:request.url==="/cluster/nodes"?{nodes:[]}:[],
    );
    await nextTurn();
    assert.ok(phases.includes("content"),`${tab} commits content after only required data`);
    const secondaryRequests=requests.slice(expected.required.length);
    const secondary=[...secondaryRequests.map(request=>request.url.includes("playback-events")?"playback-events":request.url),
      ...logReleases.map(release=>release.kind)];
    assert.deepEqual(secondary,expected.secondary,`${tab} secondary wave`);
    assert.equal(phases.includes("settled"),expected.secondary.length===0,
      `${tab} cannot settle while visible secondary work is pending`);
    for(const request of secondaryRequests) request.resolve([]);
    for(const release of logReleases) release.resolve();
    await load;
    assert.ok(phases.includes("settled"),`${tab} settles after its complete visible wave`);
  }
});

test("Settings cannot paint an old tab after a tab switch", async () => {
  const requests=[], phases=[];
  const SETTINGS_ENDPOINTS={
    settings:()=>new Promise(resolve=>requests.push({key:"settings",resolve})),
    libs:()=>new Promise(resolve=>requests.push({key:"libs",resolve})),
    status:()=>new Promise(resolve=>requests.push({key:"status",resolve})),
  };
  const SETTINGS_MANIFEST={libraries:{required:["settings","libs","status"],secondary:[]}};
  const location={hash:"#/settings"}; let tab="libraries";
  const harness=new Function(
    "location","SETTINGS_ENDPOINTS","SETTINGS_MANIFEST","settingsTab","renderSettings",
    "patchSettingsSecondary","patchSettingsSecondaryError","refreshLogs","refreshClusterLogs",
    "setPageFailure","setPagePhase","document",
    `let PAGE_RENDER_GENERATION=1,SETTINGS=null,TRAKT=null,CLUSTER_LOADED=false,
       SETTINGS_DATA={},SETTINGS_LOADED=new Set(),SETTINGS_LOADS=new Map();
     ${shippedSource("settingsCurrent")};${shippedSource("cacheSettings")};${shippedSource("cacheTrakt")};
     ${shippedSource("loadSettingsKey")};${shippedSource("loadSettingsTab")};
     return {load:()=>loadSettingsTab(1,"libraries"),switchAway:()=>{PAGE_RENDER_GENERATION=2;}};`,
  )(
    location,SETTINGS_ENDPOINTS,SETTINGS_MANIFEST,()=>tab,()=>phases.push("render"),()=>{},()=>{},
    async()=>{},async()=>{},()=>{},(_,__,phase)=>phases.push(phase),{getElementById:()=>null},
  );
  const old=harness.load(); await nextTurn();
  tab="playback"; harness.switchAway();
  for(const request of requests) request.resolve(request.key==="settings"?{}:[]);
  await old;
  assert.deepEqual(phases,[],"old required data cannot render, phase, or launch secondary work");
});

test("Settings coalesces shared transports while fencing each generation's commit", async () => {
  const requests=[];
  const deferred=()=>new Promise((resolve,reject)=>requests.push({resolve,reject}));
  const endpoints={settings:deferred,sys:deferred};
  const harness=new Function("SETTINGS_ENDPOINTS",
    `let PAGE_RENDER_GENERATION=1,SETTINGS=null,TRAKT=null,CLUSTER_LOADED=false,
       SETTINGS_DATA={},SETTINGS_LOADED=new Set(),SETTINGS_LOADS=new Map();
     const cacheSettings=(value)=>{SETTINGS=value;SETTINGS_DATA.settings=value;SETTINGS_LOADED.add("settings");return value;};
     const cacheTrakt=(value)=>{TRAKT=value;SETTINGS_DATA.trakt=value;SETTINGS_LOADED.add("trakt");return value;};
     ${shippedSource("loadSettingsKey")};
     return {loadSettingsKey,setGeneration:(value)=>{PAGE_RENDER_GENERATION=value;},
       reset:()=>{SETTINGS_DATA={};SETTINGS_LOADED.clear();SETTINGS_LOADS.clear();},data:()=>SETTINGS_DATA};`,
  )(endpoints);

  const old=harness.loadSettingsKey("settings",1);
  harness.setGeneration(2);
  const current=harness.loadSettingsKey("settings",2);
  assert.equal(requests.length,1,"a fast tab switch shares the pending /settings transport");
  requests[0].resolve({marker:"shared"});
  await Promise.all([old,current]);
  assert.equal(harness.data().settings.marker,"shared");

  harness.reset(); harness.setGeneration(3);
  const stale=harness.loadSettingsKey("sys",3);
  harness.reset(); harness.setGeneration(5);
  const fresh=harness.loadSettingsKey("sys",5);
  assert.equal(requests.length,3,"logout-style cache reset starts a new credential transport");
  requests[1].resolve({marker:"old"}); await stale;
  assert.equal(harness.data().sys,undefined,"an old generation cannot repopulate cleared protected data");
  requests[2].resolve({marker:"new"}); await fresh;
  assert.equal(harness.data().sys.marker,"new");
});

test("stale authorization responses cannot revoke or feed a newer session", async () => {
  const responses=[];
  const fetch=()=>new Promise((resolve)=>responses.push(resolve));
  const harness=new Function("fetch",
    `let API="/api/v1",TOKEN="old",AUTH_GENERATION=1,logoutCount=0;
     const PlaybackPolicy={parseStreamFailure:()=>null};
     function logout(){logoutCount++;TOKEN=null;AUTH_GENERATION++;}
     ${shippedSource("api")};
     return {api,login:(token)=>{TOKEN=token;AUTH_GENERATION++;},state:()=>({TOKEN,AUTH_GENERATION,logoutCount})};`,
  )(fetch);
  const first=harness.api("/hubs"), second=harness.api("/home/previews");
  responses[0]({status:401,ok:false});
  await assert.rejects(first,/unauthorized/);
  assert.equal(harness.state().logoutCount,1);
  harness.login("new");
  responses[1]({status:401,ok:false});
  await assert.rejects(second,(error)=>error.staleAuth===true);
  assert.deepEqual(harness.state(),{TOKEN:"new",AUTH_GENERATION:3,logoutCount:1});

  const staleSuccess=harness.api("/system");
  harness.login("newer");
  responses[2]({status:200,ok:true,json:async()=>({secret:"old-user"})});
  await assert.rejects(staleSuccess,(error)=>error.staleAuth===true);
  assert.equal(harness.state().TOKEN,"newer");
});

test("logout clears every protected page cache before rendering auth", () => {
  const main={innerHTML:"protected"}, removed=[];
  const document={getElementById:(id)=>id==="main"?main:null,documentElement:{classList:{remove(){}}}};
  const localStorage={removeItem:(key)=>removed.push(key)};
  const harness=new Function("document","localStorage",
    `let READER=null,TOKEN="token",AUTH_GENERATION=4,ME={id:1},NATIVE_READER_BOOT=false,
       PAGE_RENDER_GENERATION=7,PAGE_TIMER=1,ACTIVITY_SNAPSHOT={secret:true},ACTIVITY_DETAIL_BUSY=7,
       SETTINGS={secret:true},TRAKT={secret:true},TRAKT_EDIT=true,SETTINGS_TICKING={generation:7},
       SETTINGS_DATA={secret:true},SETTINGS_LOADED=new Set(["settings"]),SETTINGS_LOADS=new Map([["settings",{}]]),
       LOGS_RUN={},CLUSTER_LOGS_RUN={},CLUSTER_LOADED=true,CLUSTER_LEAVING=true,
       CLUSTER_TOKEN={token:"secret"},CLUSTER_REFUSAL={message:"secret"},ACT_TIMER=2,rendered=0;
     function forgetJoinToken(){CLUSTER_TOKEN=null;CLUSTER_REFUSAL=null;}
     function render(){rendered++;}
     ${shippedSource("logout")};
     return {logout,state:()=>({TOKEN,ME,PAGE_RENDER_GENERATION,PAGE_TIMER,ACTIVITY_SNAPSHOT,
       SETTINGS_DATA,loaded:SETTINGS_LOADED.size,loads:SETTINGS_LOADS.size,LOGS_RUN,CLUSTER_LOGS_RUN,
       CLUSTER_TOKEN,CLUSTER_REFUSAL,CLUSTER_LOADED,CLUSTER_LEAVING,rendered,main:document.getElementById("main").innerHTML})};`,
  )(document,localStorage);
  harness.logout();
  assert.deepEqual(harness.state(),{TOKEN:null,ME:null,PAGE_RENDER_GENERATION:8,PAGE_TIMER:null,
    ACTIVITY_SNAPSHOT:null,SETTINGS_DATA:{},loaded:0,loads:0,LOGS_RUN:null,CLUSTER_LOGS_RUN:null,
    CLUSTER_TOKEN:null,CLUSTER_REFUSAL:null,CLUSTER_LOADED:false,CLUSTER_LEAVING:false,rendered:1,main:""});
  assert.deepEqual(removed,["plurx_token"]);
});

test("Page phases are generation-fenced, ordered, and wired to measured routes", () => {
  const main = { dataset: {} };
  const document = { getElementById: (id) => id === "main" ? main : null };
  const location = { hash: "#/activity" };
  const marks = [{ name: "plurx-page-phase:6:home:settled" }];
  const performance = {
    getEntriesByType: () => [...marks],
    clearMarks: (name) => {
      const index = marks.findIndex((entry) => entry.name === name);
      if (index !== -1) marks.splice(index, 1);
    },
    mark: (name) => marks.push({ name }),
  };
  const harness = new Function(
    "document", "location", "performance",
    `let PAGE_RENDER_GENERATION=7;
     ${shippedSource("pagePhaseName")};
     ${shippedSource("setPagePhase")};
     ${shippedSource("setPageFailure")};
     return {setPagePhase,setPageFailure,setGeneration:(value)=>{PAGE_RENDER_GENERATION=value;}};`,
  )(document, location, performance);

  assert.equal(harness.setPagePhase("#/activity", 6, "shell"), false);
  assert.equal(harness.setPagePhase("#/settings", 7, "shell"), false);
  assert.equal(harness.setPagePhase("#/activity", 7, "unknown"), false);
  assert.equal(harness.setPagePhase("#/activity", 7, "shell"), true);
  assert.deepEqual(main.dataset, {
    page: "activity", pageGeneration: "7", phase: "shell",
  });
  assert.deepEqual(marks.map((entry) => entry.name), [
    "plurx-page-phase:7:activity:shell",
  ], "a new generation discards prior page marks before adding its shell");
  assert.equal(harness.setPageFailure("#/activity", 7, "render_error"), true);
  assert.equal(main.dataset.pageFailure, "render_error");
  assert.equal(harness.setPageFailure("#/activity", 7, null), true);
  assert.equal(main.dataset.pageFailure, undefined);
  assert.equal(harness.setPagePhase("#/activity", 7, "content"), true);
  assert.equal(harness.setPagePhase("#/activity", 7, "settled"), true);
  assert.equal(harness.setPagePhase("#/activity", 7, "content"), false,
    "a poll cannot regress a settled route to content");
  assert.equal(main.dataset.phase, "settled");

  harness.setGeneration(8);
  location.hash = "#/settings";
  assert.equal(harness.setPagePhase("#/settings", 8, "shell"), true);
  assert.deepEqual(main.dataset, {
    page: "settings", pageGeneration: "8", phase: "shell",
  });
  assert.equal(marks.length, 1, "normal navigation keeps only the current phase marks");

  const home = shippedTopLevelSource("viewHome");
  assert.match(home, /setPagePhase\(route,generation,"shell"\)/);
  assert.match(home, /setPagePhase\(route,generation,"content"\)/);
  assert.match(home, /setPagePhase\(route,generation,"settled"\)/);
  const settingsView = shippedTopLevelSource("viewSettings");
  assert.match(settingsView, /setPagePhase\(route,generation,"shell"\)/);
  const settingsLoad = shippedSource("loadSettingsTab");
  assert.match(settingsLoad, /setPagePhase\(route,generation,"content"\)/);
  assert.match(settingsLoad, /setPagePhase\(route,generation,"settled"\)/);
  const activity = shippedSource("renderActivityBody");
  assert.match(activity, /setPagePhase\("#\/activity",generation,"content"\)/);
  assert.match(activity, /setPagePhase\("#\/activity",generation,"settled"\)/);
});

process.on("beforeExit", () => {
  if (failures) process.exitCode = 1;
});
