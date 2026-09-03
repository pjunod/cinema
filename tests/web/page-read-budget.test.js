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
let started = 0;
let finished = 0;
// An async body that never settles prints neither PASS nor FAIL and vanishes
// from the run, which reads exactly like a clean pass. Count starts against
// finishes and fail the run on the difference.
// Queued and run in order, not fired off as they are declared. A bare
// `async function test()` called without `await` yields at the first `await`
// inside the body and lets the next declaration start, so these ran
// concurrently and in a different order each time. Nothing here shares mutable
// state today, which is the only reason it was harmless — and it is not a
// property this file can keep on purpose, because each new test would have to
// re-establish it.
const QUEUE = [];
function test(name, run) {
  QUEUE.push({ name, run });
}

async function main() {
  for (const { name, run } of QUEUE) {
    started += 1;
    try {
      await run();
      process.stdout.write(`PASS ${name}\n`);
    } catch (error) {
      failures += 1;
      process.stderr.write(`FAIL ${name}\n${error && error.stack}\n`);
    }
    finished += 1;
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
  assert.match(activity.innerHTML, /class="activitytext"/,
    "the header gives long activity copy its truncation hook");

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
     ${shippedSource("nodeLabel")};
     ${shippedSource("activityNodeFailureText")};
     ${shippedSource("detailActivitySummary")};
     return {activityNodeFailures,activityNodeFailureText,detailActivitySummary};`,
  )();
  const detail = {
    activity_nodes: [
      { node_id: "node-a", status: "answered" },
      { node_id: "node-b", status: "timed_out" },
      { node_id: "node-c", status: "unhealthy" },
      { node_id: "node-d", status: "refused" },
      { node_id: "node-e", status: "http_error" },
      { node_id: "node-f", status: "unsupported" },
    ],
    scans: [], producing: null, offline: [], trakt: { syncing: false },
  };
  const missing = harness.activityNodeFailures(detail);
  assert.deepEqual(missing.map((node) => node.node_id), [
    "node-b", "node-c", "node-d", "node-e", "node-f",
  ]);
  assert.equal(
    harness.activityNodeFailureText(missing[0], {}),
    "Node node-b · timed out",
  );
  // Named when the roster could name it. tests/web/activity-node-names.test.js
  // owns the rest of this contract; this is the budget page's own call site.
  assert.equal(
    harness.activityNodeFailureText(missing[0], { "node-b": "m6" }),
    "Node m6 · timed out",
  );
  assert.equal(
    harness.activityNodeFailureText(missing[2], {}),
    "Node node-d · refused the activity request",
  );
  assert.equal(
    harness.activityNodeFailureText(missing[3], {}),
    "Node node-e · returned an HTTP error",
  );
  assert.equal(
    harness.activityNodeFailureText(missing[4], {}),
    "Node node-f · does not publish Activity HTTP",
  );
  assert.deepEqual(harness.detailActivitySummary(detail, []), [{
    label: "Activity incomplete",
    detail: "5 cluster nodes did not answer",
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

test("Activity keeps polling its detail body after the route settles", () => {
  // The structural golden stops this timer at the settled boundary, because
  // letting its second read land changes the captured DOM as well as the
  // request count. That makes the poll invisible to the golden, so it is
  // asserted here instead: the route arms it, at the interval it ships with,
  // through the generation-guarded timer that a route change cancels.
  const view = shippedSource("viewActivity");
  assert.match(view, /setPageTimer\(\(\)=>renderActivityBody\(generation\),\s*3000,\s*generation\)/);
  assert.match(shippedSource("renderActivityBody"), /api\("\/activity\/detail"\)/);
  assert.match(
    shippedSource("setPageTimer"),
    /if\(generation!==PAGE_RENDER_GENERATION\)\{ clearInterval\(timer\)/,
  );
});

test("Activity's detail poll survives what the golden cannot capture", () => {
  // The structural golden cannot assert this. Activity's captured request
  // inventory alternates between two and three /activity/detail calls
  // depending on how loaded the runner is, and the second read is not merely
  // an extra request — the fixture's session table arrives with it, so the
  // same golden key captures two different pages. Pin the behaviour where it
  // is deterministic instead: the route arms the poll, at the interval it
  // ships with, through the generation-guarded timer a route change cancels.
  const view = shippedSource("viewActivity");
  assert.match(view, /setPageTimer\(\(\)=>renderActivityBody\(generation\),\s*3000,\s*generation\)/);
  assert.match(shippedSource("renderActivityBody"), /api\("\/activity\/detail"\)/);
  assert.match(
    shippedSource("setPageTimer"),
    /if\(generation!==PAGE_RENDER_GENERATION\)\{ clearInterval\(timer\)/,
  );
});

test("Settings polls only the visible data panel and never overlaps", async () => {
  const tick = shippedSource("settingsTick");
  assert.match(tick, /SETTINGS_TICKING\.generation===generation/);
  assert.match(tick, /if\(tab==="libraries"\)\{[\s\S]*api\("\/scan\/status"\)/);
  assert.match(tick, /if\(tab==="integrations"&&!TRAKT_EDIT\)\{[\s\S]*api\("\/trakt\/status"\)/);
  assert.match(tick, /!isSettingsRoute\(h\)/, "the tick recognises every settings section route");
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
     ${shippedSource("isSettingsRoute")}
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
  await harness.settingsTick();
  assert.equal(requests.length, 1, "Metadata holds provider keys only; Trakt polling moved with the card");
  tab = "integrations";
  const integrations = harness.settingsTick();
  assert.equal(requests[1].url, "/trakt/status");
  requests[1].resolve({ configured: false });
  await integrations;
  document.visibilityState = "hidden";
  await harness.settingsTick();
  assert.equal(requests.length, 2);
});

test("cockpit Settings makes the active tab and long System values distinct", async () => {
  // Panoptic and Redline once painted every tab through the generic accent
  // button rule. The selected tab therefore differed only by a two-pixel edge
  // in the same hue. Its own filled state must outrank that generic rule.
  assert.match(
    SHIPPED_UI,
    /\.settabs>button\.settab\.active:not\(\.ghost\):not\(\.pbtn\)[\s\S]*?background:var\(--accent\);color:var\(--btn-ink\)/,
  );
  assert.match(
    SHIPPED_UI,
    /\.settabs>button\.settab:not\(\.ghost\):not\(\.pbtn\)[\s\S]*?background:rgba\(255,255,255,\.025\)/,
  );

  // System's values are prose and paths, not cockpit labels. Keep the label
  // treatment on dt, restore natural case/wrapping on dd, and give storage's
  // potentially huge root list the shrinking column rather than max-content.
  assert.match(SHIPPED_UI, /\.kvgrid dd\{[\s\S]*?text-transform:none;word-break:normal/);
  assert.match(SHIPPED_UI, /\.storage-table\{grid-template-columns:minmax\(0,1fr\) max-content/);
  assert.match(shippedSource("storageHtml"), /class="stgtable storage-table"/);
  assert.match(shippedSource("systemPanel"), /class="kvgrid system-grid"/);
  // Watch-state durability is reported once, on the Cluster tab, and nowhere
  // else. Two readings of the same projection on two screens is how an
  // operator ends up comparing a stale copy against a fresh one.
  assert.doesNotMatch(shippedSource("systemPanel"), /system-now-watch|Watch state/);
  assert.doesNotMatch(shippedSource("systemPanel"), /replicationText/);
  assert.doesNotMatch(SHIPPED_UI, /\.system-now-watch\{/);
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
     ${shippedSource("isSettingsRoute")}
     ${shippedSource("settingsTick")};
     return {settingsTick,switchGeneration:()=>{PAGE_RENDER_GENERATION=2;},busy:()=>SETTINGS_TICKING};`,
  )(
    document,location,api,()=>tab,()=>true,async()=>{},async()=>{},()=>{},
  );
  const old=harness.settingsTick(1,"libraries");
  tab="integrations"; harness.switchGeneration();
  const current=harness.settingsTick(2,"integrations");
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
    libraries: { required: ["settings", "libs", "status", "dvConversions"], secondary: [] },
    metadata: { required: ["settings"], secondary: ["libs"] },
    playback: { required: ["settings"], secondary: [] },
    analysis: { required: ["settings", "analysis"], secondary: [] },
    maintenance: { required: ["settings", "dvConversions"], secondary: [] },
    users: { required: ["users"], secondary: [] },
    system: { required: ["sys"], secondary: ["playbackEvents"] },
    cluster: { required: ["cluster"], secondary: ["clusterOps"] },
    integrations: { required: ["settings", "trakt"], secondary: [] },
  });
  const view = shippedSource("viewSettings");
  assert.doesNotMatch(view, /Promise\.all\(\[\s*api/,
    "Settings no longer blocks every tab on a seven-endpoint page-wide wave");
  assert.match(view, /layoutChrome\("settings",settingsShell\(tab\)\)/);
  assert.ok(view.indexOf("await loadSettingsTab") < view.indexOf("setPageTimer"),
    "polling starts only after the required tab load, never over the initial read");
  // A section switch is a route change: the address bar names the section
  // and render() keeps the cached aggregate because the previous route was
  // also Settings. Nothing short-circuits around the router any more.
  const switchTab = shippedSource("setSettingsTab");
  assert.match(switchTab, /if\(t===settingsTab\(\)\) return/);
  assert.match(switchTab, /location\.hash=`#\/settings\/\$\{t\}`/);
  assert.doesNotMatch(switchTab, /viewSettings\(/);
  const router = shippedSource("render");
  assert.match(router, /const stayingInSettings=isSettingsRoute\(h\)&&isSettingsRoute\(LAST_ROUTE\)/);
  assert.match(router, /else if\(isSettingsRoute\(h\)\) await viewSettings\(generation,!stayingInSettings\)/);
  assert.match(router, /history\.replaceState\(null,"",h\)/, "a bare #\/settings is rewritten to its section");
  const loadTab = shippedSource("loadSettingsTab");
  assert.match(loadTab, /manifest\.required/);
  assert.match(loadTab, /patchSettingsSecondary/);
  assert.match(loadTab, /settingsCurrent\(generation,tab\)/);
  assert.match(loadTab, /patchSettingsSecondaryError/);
  for (const endpoint of ["/libraries", "/settings", "/analysis/summary", "/scan/status", "/dv-conversions", "/system", "/users", "/trakt/status", "/cluster/nodes"]) {
    assert.match(SHIPPED_UI, new RegExp(`api\\(${JSON.stringify(endpoint).replace("/", "\\/")}`),
      `Settings endpoint map includes ${endpoint}`);
  }
});

test("Dolby Vision progress polling runs only for active durable work", async () => {
  const conversionActive = new Function(
    `${shippedSource("dvConversionIsActive")}; return dvConversionIsActive;`,
  )();
  const snapshotActive = new Function(
    `${shippedSource("dvSnapshotHasActive")}; return dvSnapshotHasActive;`,
  )();
  for (const state of ["queued", "running", "verified"])
    assert.equal(conversionActive({ state }), true, `${state} remains pollable`);
  for (const state of ["committed", "failed"])
    assert.equal(conversionActive({ state }), false, `${state} stops polling`);
  assert.equal(snapshotActive({ progress: { queued: 1 } }), true);
  assert.equal(snapshotActive({ progress: { committed: 4, failed: 1 } }), false);

  const hydrate = shippedSource("hydrateDvFileActions");
  assert.match(hydrate,
    /const cap=DV_CONVERSION_LEDGER_READ_MAX\*DV_CONVERSION_LEDGER_BATCH_MAX/);
  assert.match(hydrate, /const ids=uniqueIds\.slice\(0,cap\)/);
  assert.match(hydrate, /at\+=DV_CONVERSION_LEDGER_READ_MAX/);
  assert.match(hydrate, /for\(const batch of batches\)/);
  assert.doesNotMatch(hydrate, /Promise\.all/,
    "ledger batches execute with request concurrency one");
  assert.match(hydrate,
    /api\(`\/dv-conversions\?file_ids=\$\{encodeURIComponent\(batch\.join\(","\)\)\}`\)/);
  assert.equal((hydrate.match(/api\(/g)||[]).length, 1,
    "item hydration has one batch request site rather than a per-file fallback");
  assert.match(hydrate, /Object\.assign\(eligibility,snapshot\.eligible_by_file\|\|\{\}\)/,
    "item actions consume server-computed Dolby Vision eligibility");
  assert.match(hydrate, /eligible:eligibility\[id\]===true/,
    "the client never infers Profile 7 conversion eligibility from its item DTO");
  assert.match(hydrate, /const capabilities=snapshots\[0\]\.capabilities\|\|\{\}/,
    "item actions consume the shared conversion-tool capability snapshot");
  assert.match(hydrate, /const libraryModes=snapshots\[0\]\.library_modes\|\|\{\}/,
    "item actions consume the shared per-library conversion mode snapshot");
  assert.match(hydrate, /capabilities,/,
    "the batch response supplies one shared tool capability snapshot");
  assert.match(hydrate, /library_modes:libraryModes/,
    "each file action receives the authoritative library mode map");
  assert.doesNotMatch(hydrate, /Number\(file\.dv_profile\)===7/,
    "the UI does not enable an action from the numeric profile alone");
  assert.doesNotMatch(hydrate, /\/files\/\$\{id\}\/dv-conversion/,
    "item hydration never falls back to one request per file");

  const mounts = new Map();
  const files = Array.from(
    { length: 257 },
    (_, index) => ({ id: index + 1, library_id: 7 }),
  );
  for (const file of files) mounts.set(`dv-file-${file.id}`, {
    dataset: { dvActive: "false" }, innerHTML: "Checking",
  });
  const requests = [];
  const api = async (url) => {
    requests.push(url);
    const ids = new URL(url, "http://plurx.test").searchParams
      .get("file_ids").split(",");
    return {
      conversions_by_file: ids.includes("1") ? { 1: { state: "queued" } } : {},
      eligible_by_file: Object.fromEntries(ids.map((id) => [id, id === "257"])),
      capabilities: { available: true },
      library_modes: { "7": "manual" },
    };
  };
  const hydrate257 = new Function(
    "ME", "document", "exactWireId", "api", "dvConversionIsActive",
    "dvConversionStateHtml", "esc", "DV_CONVERSION_LEDGER_READ_MAX",
    "DV_CONVERSION_LEDGER_BATCH_MAX",
    `${hydrate}; return hydrateDvFileActions;`,
  )(
    { is_admin: true },
    { getElementById: (id) => mounts.get(id) || null },
    (file) => String(file.id),
    api,
    (conversion) => conversion && ["queued", "running", "verified"].includes(conversion.state),
    (file, snapshot) => `${file.id}:${snapshot.conversion?.state || "none"}:${snapshot.eligible}:${snapshot.library_modes[String(file.library_id)]}`,
    String,
    256,
    4,
  );
  assert.equal(await hydrate257(files), true);
  assert.equal(requests.length, 2, "257 visible files use exactly two bounded reads");
  const requestedIds = requests.map((url) => new URL(url, "http://plurx.test")
    .searchParams.get("file_ids").split(","));
  assert.deepEqual(requestedIds.map((ids) => ids.length), [256, 1]);
  assert.deepEqual(requestedIds.flat(), files.map((file) => String(file.id)));
  assert.equal(mounts.get("dv-file-1").innerHTML, "1:queued:false:manual",
    "the first batch survives the deterministic merge");
  assert.equal(mounts.get("dv-file-257").innerHTML, "257:none:true:manual",
    "the final batch survives the deterministic merge");

  const cappedFiles = Array.from({ length: 1025 }, (_, index) => ({ id: index + 1 }));
  const cappedMounts = new Map(cappedFiles.map((file) => [`dv-file-${file.id}`, {
    dataset: { dvActive: "false" }, innerHTML: "Checking",
  }]));
  const cappedRequests = [];
  let inFlight = 0;
  let maxInFlight = 0;
  const hydrateAboveCap = new Function(
    "ME", "document", "exactWireId", "api", "dvConversionIsActive",
    "dvConversionStateHtml", "esc", "DV_CONVERSION_LEDGER_READ_MAX",
    "DV_CONVERSION_LEDGER_BATCH_MAX",
    `${hydrate}; return hydrateDvFileActions;`,
  )(
    { is_admin: true },
    { getElementById: (id) => cappedMounts.get(id) || null },
    (file) => String(file.id),
    async (url) => {
      cappedRequests.push(url);
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      await nextTurn();
      inFlight -= 1;
      return { conversions_by_file: {}, eligible_by_file: {}, capabilities: {} };
    },
    () => false,
    (file) => `loaded ${file.id}`,
    String,
    256,
    4,
  );
  assert.equal(await hydrateAboveCap(cappedFiles), false);
  assert.equal(cappedRequests.length, 4, "the page never exceeds four ledger reads");
  assert.equal(maxInFlight, 1, "ledger reads execute sequentially");
  const cappedRequestIds = cappedRequests.map((url) => new URL(url, "http://plurx.test")
    .searchParams.get("file_ids").split(","));
  assert.deepEqual(cappedRequestIds.map((ids) => ids.length), [256, 256, 256, 256]);
  assert.equal(cappedRequestIds.flat().at(-1), "1024");
  assert.equal(cappedMounts.get("dv-file-1024").innerHTML, "loaded 1024");
  assert.match(cappedMounts.get("dv-file-1025").innerHTML, /checks at most 1024/,
    "files beyond the fixed page budget get an explicit state");

  for (const mount of cappedMounts.values()) mount.innerHTML = "Checking";
  let failedCalls = 0;
  const rejectFinalBatch = new Function(
    "ME", "document", "exactWireId", "api", "dvConversionIsActive",
    "dvConversionStateHtml", "esc", "DV_CONVERSION_LEDGER_READ_MAX",
    "DV_CONVERSION_LEDGER_BATCH_MAX",
    `${hydrate}; return hydrateDvFileActions;`,
  )(
    { is_admin: true },
    { getElementById: (id) => cappedMounts.get(id) || null },
    (file) => String(file.id),
    async () => {
      failedCalls += 1;
      if (failedCalls === 4) throw new Error("final bounded batch failed");
      return { conversions_by_file: { 1: { state: "queued" } } };
    },
    () => true,
    () => "partial result must not paint",
    String,
    256,
    4,
  );
  assert.equal(await rejectFinalBatch(cappedFiles), false);
  assert.equal(failedCalls, 4);
  assert.match(cappedMounts.get("dv-file-1").innerHTML, /final bounded batch failed/,
    "one failed batch makes every selected result explicitly unavailable");
  assert.match(cappedMounts.get("dv-file-1024").innerHTML, /final bounded batch failed/);
  assert.match(cappedMounts.get("dv-file-1025").innerHTML, /checks at most 1024/);
  assert.doesNotMatch(cappedMounts.get("dv-file-1").innerHTML, /partial result must not paint/);

  const refresh = shippedSource("refreshDvConversions");
  assert.match(refresh, /DV_SETTINGS_POLL_AT=Date\.now\(\)\+DV_PROGRESS_POLL_MS/,
    "the next gate is stamped before the network request");
  const tick = shippedSource("settingsTick");
  assert.match(tick, /dvSnapshotHasActive\(SETTINGS_DATA\.dvConversions\)/);
  assert.match(tick, /Date\.now\(\)>=DV_SETTINGS_POLL_AT/);
  assert.match(tick, /paintDvConversionProgress\(snapshot\)/);

  const item = shippedSource("viewItem");
  assert.match(item, /hydrateDvFileActions\(DV_FILE_PAGE_FILES\)\.then\(active=>/);
  assert.match(item, /if\(active[\s\S]*armDvFilePoll/,
    "the item timer starts only after an active ledger row is observed");
  const poll = shippedSource("pollDvFileActions");
  assert.match(poll, /if\(!active[\s\S]*clearInterval\(PAGE_TIMER\)/,
    "the item timer stops after the first all-terminal snapshot");
  const loadItem = shippedSource("loadItem");
  assert.match(loadItem, /f\.library_id=it\.library_id/,
    "item detail binds its already-loaded library id to every file action");
  const queueLibrary = shippedSource("convertDvLibrary");
  assert.match(queueLibrary, /result\.saturated/,
    "a cap-hit batch tells the operator another bounded pass may be needed");

  const fileMount = { innerHTML: "", dataset: {} };
  const fileEvents = [];
  const fileResults = [
    { queued: true, conversion: { state: "queued" } },
    { queued: false, conversion: { state: "running" } },
    { queued: false, conversion: { state: "failed" } },
    new Error("file is not Dolby Vision Profile 7"),
  ];
  let fileApiCalls = 0;
  let filePollArms = 0;
  const fileHarness = new Function(
    "api", "toast", "document", "exactWireId", "dvConversionStateHtml", "armDvFilePoll",
    `let DV_FILE_PAGE_FILES=[{id:"42"}];
     ${shippedSource("dvConversionIsActive")}
     ${shippedSource("queueDvFile")}
     return {queue:queueDvFile};`,
  )(
    async () => {
      fileApiCalls += 1;
      const result = fileResults.shift();
      if (result instanceof Error) throw result;
      return result;
    },
    (message) => fileEvents.push(message),
    { getElementById: () => fileMount },
    (file) => String(file.id),
    (_file, snapshot) => `${snapshot.conversion.state}:${snapshot.eligible}`,
    () => { filePollArms += 1; },
  );
  await fileHarness.queue("42", { disabled: false });
  assert.equal(fileMount.innerHTML, "queued:true",
    "the file mutation response paints success without a follow-up read");
  assert.equal(fileMount.dataset.dvActive, "true");
  assert.equal(fileApiCalls, 1,
    "file queue success never depends on a second status request");
  assert.equal(filePollArms, 1,
    "the active mutation response arms terminal-bounded item polling");
  assert.deepEqual(fileEvents, ["Dolby Vision conversion queued"]);

  await fileHarness.queue("42", { disabled: false });
  assert.equal(fileMount.innerHTML, "running:true");
  assert.equal(fileEvents.at(-1), "Dolby Vision conversion is already active",
    "an idempotent active response is never announced as newly queued");
  assert.equal(filePollArms, 2);

  await fileHarness.queue("42", { disabled: false });
  assert.equal(fileMount.innerHTML, "failed:false");
  assert.equal(fileEvents.at(-1), "Dolby Vision conversion was not queued",
    "a defensive non-active queued:false response cannot claim success");
  assert.equal(filePollArms, 2, "a terminal response does not arm polling");

  await fileHarness.queue("42", { disabled: false });
  assert.equal(fileEvents.at(-1), "file is not Dolby Vision Profile 7",
    "the API's ineligible refusal remains visible rather than becoming a queued toast");
  assert.equal(fileApiCalls, 4);

  const libraryEvents = [];
  let libraryApiCalls = 0;
  const libraryHarness = new Function(
    "api", "toast", "renderSettings",
    `let SETTINGS_DATA={dvConversions:{progress:{},progress_by_library:{}}};
     let DV_SETTINGS_POLL_AT=1234;
     ${shippedSource("noteDvLibraryQueueResult")}
     ${shippedSource("convertDvLibrary")}
     return {queue:convertDvLibrary,state:()=>({data:SETTINGS_DATA,pollAt:DV_SETTINGS_POLL_AT})};`,
  )(
    async () => {
      libraryApiCalls += 1;
      return { queued: 2, saturated: false };
    },
    (message) => libraryEvents.push(message),
    () => libraryEvents.push("rendered"),
  );
  await libraryHarness.queue(7, { disabled: false });
  const queuedState = libraryHarness.state();
  assert.equal(queuedState.data.dvConversions.progress.queued, 2);
  assert.equal(queuedState.data.dvConversions.progress_by_library["7"].queued, 2);
  assert.equal(queuedState.pollAt, 0,
    "an accepted library batch makes the next bounded settings tick eligible");
  assert.equal(libraryApiCalls, 1,
    "library queue success never depends on an immediate progress refresh");
  assert.deepEqual(libraryEvents, ["2 Dolby Vision files queued", "rendered"],
    "an accepted queue is displayed as success without depending on a refresh");
});

test("Dolby Vision settings controls have accessible names", () => {
  const file = shippedSource("dvConversionStateHtml");
  assert.match(file, /aria-label="Convert file .* from Dolby Vision Profile 7 to Profile 8\.1 on disk"/);
  assert.match(file, /aria-label="Retry on-disk Dolby Vision conversion for file/);
  assert.ok((file.match(/dvRecoveryGuardStatusHtml\(conversion\.recovery_guard\)/g)||[]).length>=3,
    "attached guard state is visible before commit, after failure, and at the terminal row");
  const mode = shippedSource("dvModeSelect");
  assert.match(mode, /aria-label="Dolby Vision conversion mode for/);
  assert.match(mode, /aria-label="Convert Dolby Vision files in/);
  assert.doesNotMatch(mode, /aria-label="Save Dolby Vision conversion mode for/,
    "the drawer's one Save owns the mode; the select has no Save button of its own");
  const renderMode = new Function(
    "esc", "dvProgressText",
    `${mode}; return dvModeSelect;`,
  )((value) => String(value), () => "idle");
  const unavailable = renderMode(
    { id: 7, name: "Movies" },
    {
      library_modes: { "7": "auto" },
      capabilities: { available: false, reason: "tools missing" },
      progress_by_library: {},
    },
  );
  assert.doesNotMatch(unavailable, /<select[^>]* disabled/,
    "a missing tool never traps an Automatic library in its stored mode");
  assert.match(unavailable, /<option value="off">Off<\/option>/,
    "Off remains selectable without conversion tools");
  assert.match(unavailable, /<option value="auto" selected disabled>Automatic<\/option>/,
    "unavailable conversion modes cannot be newly selected");

  // One Save owns the whole library drawer now (saveLibDrawer writes the DV
  // mode with identity and schedule), so the mode select has no Save button of
  // its own — only Convert now, which stays refused without the tools.
  const select = { value: "off", dataset: { dvToolsAvailable: "false" } };
  const convert = { disabled: true };
  const updateMode = new Function(
    "document",
    `${shippedSource("updateDvModeControls")}; return updateDvModeControls;`,
  )({ getElementById: (id) => id === "dv-mode-7" ? select : id === "dv-convert-7" ? convert : null });
  updateMode(7);
  assert.equal(convert.disabled, true, "conversion remains refused while tools are unavailable and mode is off");
  select.value = "auto";
  updateMode(7);
  assert.equal(convert.disabled, true, "Automatic without tools still cannot convert");
  assert.doesNotMatch(shippedSource("dvModeSelect"), /dv-mode-save-/,
    "the mode select no longer carries its own Save — the drawer's Save owns it");
  assert.match(shippedSource("saveLibDrawer"), /libraries\/\$\{id\}\/dv-conversion/,
    "the drawer Save writes the Dolby Vision mode");
  const panel = shippedSource("dvDiskPanel");
  assert.match(panel, /<label class="schedpair" for="dv-parallel">Parallel files/);
  assert.match(panel, /aria-label="Save Dolby Vision conversion settings"/);

  const escapeHtml = (value) => String(value)
    .replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;").replaceAll("'", "&#39;");
  const renderFile = new Function(
    "exactWireId", "fmtBytes", "dvRecoveryGuardStatusHtml", "esc",
    `${file}; return dvConversionStateHtml;`,
  )(
    (value) => String(value.id),
    () => "0 B",
    () => "",
    escapeHtml,
  );
  const eligibleOff = renderFile(
    { id: 42, library_id: 7 },
    {
      conversion: null,
      eligible: true,
      capabilities: { available: true },
      library_modes: {},
    },
  );
  assert.match(eligibleOff, /<button[^>]* disabled>Convert on disk<\/button>/,
    "an eligible file cannot bypass its library's default-Off mutation gate");
  assert.match(eligibleOff, /library Dolby Vision conversion mode is Off/,
    "the disabled file action explains the exact policy gate");
  for (const mode of ["manual", "auto"]) {
    const eligibleEnabled = renderFile(
      { id: 42, library_id: 7 },
      {
        conversion: null,
        eligible: true,
        capabilities: { available: true },
        library_modes: { "7": mode },
      },
    );
    assert.doesNotMatch(eligibleEnabled, /<button[^>]* disabled>Convert on disk<\/button>/,
      `${mode} enables an eligible file when the required tools are available`);
  }
  const failedIneligible = renderFile(
    { id: 42, library_id: 7 },
    {
      conversion: { state: "failed", error: "controlled failure" },
      eligible: false,
      capabilities: { available: true },
    },
  );
  assert.match(failedIneligible,
    /Retry unavailable: the current scan no longer meets the Profile 7 conversion requirements/);
  assert.doesNotMatch(failedIneligible, /<button[^>]*>Retry conversion<\/button>/,
    "a failed row that is no longer eligible offers no false retry action");
  const failedEligible = renderFile(
    { id: 42, library_id: 7 },
    {
      conversion: { state: "failed", error: "controlled failure" },
      eligible: true,
      capabilities: { available: true },
      library_modes: { "7": "manual" },
    },
  );
  assert.match(failedEligible, /<button[^>]*>Retry conversion<\/button>/,
    "a still-eligible failed row keeps its retry action");
  const failedOff = renderFile(
    { id: 42, library_id: 7 },
    {
      conversion: { state: "failed", error: "controlled failure" },
      eligible: true,
      capabilities: { available: true },
      library_modes: {},
    },
  );
  assert.match(failedOff, /<button[^>]* disabled>Retry conversion<\/button>/,
    "a failed row cannot retry while its library mode is Off");
  assert.match(failedOff, /library Dolby Vision conversion mode is Off/);
  const guardStatus = new Function(
    "esc",
    `${shippedSource("dvRecoveryGuardStatusHtml")}; return dvRecoveryGuardStatusHtml;`,
  )(escapeHtml);
  assert.match(guardStatus({ state: "active", recovery_path: "</code><script>" }),
    /&lt;\/code&gt;&lt;script&gt;/,
    "file detail escapes the operator-visible recovery path");
  assert.equal(guardStatus({ state: "guard_removed" }),
    " · recovery guard removed; private scratch cleanup pending");
  assert.equal(guardStatus({ state: "scratch_removed" }),
    " · recovery cleanup complete");

  const renderRecovery = new Function(
    "esc",
    `${shippedSource("dvRecoveryGuardsHtml")}; return dvRecoveryGuardsHtml;`,
  )(escapeHtml);
  const recovery = renderRecovery(
    {
      capabilities: { available: true },
      recovery_guards: {
        summary: { intent: 1, active: 2, guard_removed: 3, scratch_removed: 4, orphaned: 9 },
        orphans: [{
          guard_id: "guard-<1>",
          state: "active",
          source_path: "/media/<former>.mkv",
          recovery_path: "/media/.<guard>.mkv",
        }],
        orphans_truncated: true,
      },
    }
  );
  assert.match(recovery, /<b>2<\/b> active guards · <b>4<\/b> transitions pending · 1 recording · 3 awaiting scratch cleanup · 4 ready for ledger retirement/);
  assert.match(recovery, /Review orphaned recovery records \(9\)/);
  assert.match(recovery, /Showing 1 of 9 orphaned records in this bounded snapshot/);
  for (const escaped of ["guard-&lt;1&gt;", "/media/&lt;former&gt;.mkv", "/media/.&lt;guard&gt;.mkv"])
    assert.match(recovery, new RegExp(escaped));
  assert.doesNotMatch(recovery, /<former>|<guard>/,
    "guard ids and paths remain inert admin text");
  const failVisibleRecovery = renderRecovery({
    recovery_guards: {
      summary: { orphaned: 0 },
      orphans: [{ guard_id: "visible", state: "active", source_path: "/former" }],
    },
  });
  assert.match(failVisibleRecovery, /Review orphaned recovery records \(1\)/,
    "a concrete orphan row remains visible even if a future projection regresses its count");
  assert.doesNotMatch(failVisibleRecovery, /No orphaned recovery records/);
  assert.match(panel, /id="dv-recovery-guards"/);
  const paint = shippedSource("paintDvConversionProgress");
  assert.match(paint, /recovery\.innerHTML=dvRecoveryGuardsHtml\(snapshot\)/,
    "an already-bounded active conversion refresh also repaints its guard lifecycle");
});

test("Analysis workspace uses server pages and separates expected outcomes", () => {
  const main={innerHTML:"",querySelectorAll:()=>[]};
  const document={activeElement:null,body:{contains:()=>true},getElementById:(id)=>id==="main"?main:null};
  const harness=new Function(
    "document","esc","fmtAgo","fmtBytes",
    `let ANALYSIS_SNAPSHOT=null,ANALYSIS_ROW_LOOKUP=new Map();
     let ANALYSIS_VIEW={filter:"all",query:"",page:1,pageSize:25,auto:true,cursors:[""]};
     ${shippedSource("analysisStateLabel")}
     ${shippedSource("analysisPhase")}
     ${shippedSource("analysisErrorInfo")}
     ${shippedSource("analysisErrorHtml")}
     ${shippedSource("analysisRows")}
     ${shippedSource("analysisCounts")}
     ${shippedSource("nodeLabel")}
     ${shippedSource("analysisNodeCell")}
     ${shippedSource("analysisNodeDetail")}
     ${shippedSource("analysisRowKey")}
     ${shippedSource("analysisDisposition")}
     ${shippedSource("analysisErrors")}
     ${shippedSource("analysisAttemptHistory")}
     ${shippedSource("analysisAttemptHistoryHtml")}
     ${shippedSource("analysisAction")}
     ${shippedSource("analysisCanRetry")}
     ${shippedSource("analysisPageUrl")}
     ${shippedSource("paintAnalysis")}
     return {
       paint:(snapshot)=>{ANALYSIS_SNAPSHOT=snapshot;paintAnalysis(snapshot);},
       page:(page,snapshot)=>{ANALYSIS_VIEW.page=page;paintAnalysis(snapshot);},
       url:(view)=>{Object.assign(ANALYSIS_VIEW,view);return analysisPageUrl();},
       disposition:analysisDisposition,retry:analysisCanRetry,
       error:analysisErrorInfo,html:()=>document.getElementById("main").innerHTML,
     };`,
  )(document,(value)=>String(value??""),()=>"just now",(value)=>`${value} B`);
  const rows=Array.from({length:25},(_,index)=>({
    row_key:`job:job-${index}`,request_id:"",job_id:`job-${index}`,
    file_id:String(index+1),item_id:String(index+100),title:`Movie ${index}`,
    state:"ready",request_state:"",job_state:"ready",disposition:"ready",
    updated_at_ms:100-index,attempts:1,owner_node_id:"node-a",
    target_node_id:"",pipeline_version:"0123456789ab",source_size:123,
    request_error_code:"",job_error_code:"",action:"rebuild",
  }));
  const summary={available:true,enabled:true,scope:"active_and_recent_terminal",terminal_window:8192,total:26,active:0,attention:0,expected:0,ready:26};
  harness.paint({enabled:true,now_ms:200,filtered_total:26,next_cursor:"5|76|job:job-24",rows,summary});
  assert.match(harness.html(),/Showing 1–25 of 26/);
  assert.match(harness.html(),/Page 1 of 2/);
  assert.match(harness.html(),/Summary window/);
  assert.match(harness.html(),/All <b>26<\/b>/);
  assert.match(harness.html(),/<th scope="col">Actions<\/th>/);
  assert.doesNotMatch(harness.html(),/Attention <b>/,
    "sampled summary values are never presented as exact full-history filter counts");
  assert.doesNotMatch(SHIPPED_UI,/Analysis is caught up/);
  harness.page(2,{enabled:true,now_ms:200,filtered_total:26,next_cursor:null,rows:[rows[0]],summary});
  assert.match(harness.html(),/Showing 26–26 of 26/);
  assert.match(harness.html(),/Page 2 of 2/);
  assert.equal(
    harness.url({filter:"attention",query:"mount offline",page:2,pageSize:50,cursors:["","3|20|request:r1"]}),
    "/analysis/jobs?limit=50&filter=attention&q=mount+offline&cursor=3%7C20%7Crequest%3Ar1",
  );
  const unsupported={state:"failed",request_error_code:"unsupported",job_error_code:"",action:"none"};
  const deleted={state:"cancelled",request_error_code:"source_deleted",job_error_code:"",action:"none"};
  const failed={state:"failed",request_error_code:"source_unavailable",job_error_code:"",action:"retry"};
  const superseded={state:"cancelled",request_error_code:"source_superseded",job_error_code:"",action:"analyze_current"};
  assert.equal(harness.disposition(unsupported),"unsupported");
  assert.equal(harness.disposition(deleted),"expected");
  assert.equal(harness.retry(unsupported),false);
  assert.equal(harness.retry(deleted),false);
  assert.equal(harness.retry(failed),true);
  assert.equal(harness.retry(superseded),true);
  harness.page(1,{enabled:true,now_ms:200,filtered_total:0,next_cursor:null,rows:[],summary:{...summary,total:0,ready:0}});
  assert.match(harness.html(),/Showing 0–0 of 0/);
  assert.doesNotMatch(harness.html(),/Showing 0–-1/);
  const sourceFailure=harness.error("source_unavailable");
  assert.equal(sourceFailure.title,"Source file is unavailable");
  assert.match(sourceFailure.next,/mount is online/);
  const unknown=harness.error("future_failure_code");
  assert.match(unknown.detail,/unrecognized error code/);
  assert.match(unknown.next,/Copy the diagnostics/);
});

test("Analysis row Retry targets the exact durable request", () => {
  const action=shippedSource("actOnAnalysisRow");
  assert.match(action,/action==="retry"&&row\.request_id/);
  assert.match(action,/`\/analysis\/jobs\/\$\{row\.request_id\}\/retry`/);
  assert.match(action,/components:\[row\.component\|\|"fragment_index"\]/,
    "non-retry row actions stay scoped to the row component");
  const paint=shippedSource("paintAnalysis");
  assert.match(paint,/onclick='actOnAnalysisRow\(/);
  assert.doesNotMatch(paint,/onclick='requestAnalysis\(/,
    "row Retry never falls through to a broad forced file request");
});

test("Analysis refresh preserves stale data, focus, and accessible state", () => {
  const refresh=shippedSource("renderAnalysis");
  assert.match(refresh,/if\(main&&ANALYSIS_SNAPSHOT\)/);
  assert.match(refresh,/Showing the last update — refresh failed/);
  assert.match(refresh,/aria-live","polite"/);
  const paint=shippedSource("paintAnalysis");
  assert.match(paint,/dataset\.analysisFocus/);
  assert.match(paint,/target\.focus\(\)/);
  assert.match(paint,/setSelectionRange/);
  assert.match(paint,/aria-pressed=/);
  assert.match(paint,/role="status" aria-live="polite"/);
  const view=shippedSource("viewAnalysis");
  assert.match(view,/setPageTimer\(\(\)=>\{ if\(ANALYSIS_VIEW\.auto\) renderAnalysisSummary\(generation\)/,
    "the workspace polls only the compact summary, never the full history query");
  assert.doesNotMatch(view,/setPageTimer\([^\n]*renderAnalysis\(generation\)/);
});

test("Analysis summary polling refreshes queue state and countdown time", async () => {
  const api=async()=>({available:true,enabled:false,now_ms:42_000,active:0,total:0});
  const document={visibilityState:"visible"};
  const harness=new Function(
    "api","document","location",
    `let PAGE_RENDER_GENERATION=1,ANALYSIS_SUMMARY_BUSY=null;
     let ANALYSIS_SUMMARY=null;
     let ANALYSIS_SNAPSHOT={enabled:true,now_ms:1_000,rows:[],summary:{enabled:true,now_ms:1_000}};
     const painted=[];
     const paintAnalysis=(snapshot)=>painted.push({...snapshot});
     ${shippedSource("renderAnalysisSummary")}
     return {run:()=>renderAnalysisSummary(1),painted,snapshot:()=>ANALYSIS_SNAPSHOT};`,
  )(api,document,{hash:"#/analysis"});

  await harness.run();
  assert.equal(harness.snapshot().enabled,false,"the paused banner follows the compact summary");
  assert.equal(harness.snapshot().now_ms,42_000,"lease and retry countdowns advance with the summary clock");
  assert.equal(harness.painted.length,1);
});

test("Analysis refresh queues changed views and never paints an obsolete response", async () => {
  const jobReads=[];
  const api=(url)=>{
    if(url==="/analysis/summary") return Promise.resolve({available:true,enabled:true});
    return new Promise(resolve=>jobReads.push({url,resolve}));
  };
  const main={innerHTML:"",prepend:()=>{}};
  const document={
    visibilityState:"visible",activeElement:null,body:{contains:()=>true},
    getElementById:(id)=>id==="main"?main:null,createElement:()=>({setAttribute:()=>{}}),
  };
  const harness=new Function(
    "api","document","location",
    `let PAGE_RENDER_GENERATION=1,ANALYSIS_BUSY=null,ANALYSIS_PENDING=null;
     let ANALYSIS_SNAPSHOT=null,ANALYSIS_SUMMARY=null;
     let ANALYSIS_VIEW={filter:"all",query:"",page:1,pageSize:25,auto:false,cursors:[""]};
     const painted=[];
     const paintAnalysis=(snapshot)=>painted.push(snapshot.marker);
     const setPageFailure=()=>{},setPagePhase=()=>{},esc=String;
     ${shippedSource("analysisPageUrl")}
     ${shippedSource("renderAnalysis")}
     return {
       start:()=>renderAnalysis(1),
       change:()=>{ANALYSIS_VIEW.filter="attention";return renderAnalysis(1,true);},
       painted,
     };`,
  )(api,document,{hash:"#/analysis"});

  const first=harness.start();
  assert.match(jobReads[0].url,/filter=all/);
  await harness.change();
  jobReads[0].resolve({marker:"obsolete",rows:[],filtered_total:0,next_cursor:null});
  await first;
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(jobReads.length,2,"the changed filter is fetched after the active read settles");
  assert.match(jobReads[1].url,/filter=attention/);
  assert.deepEqual(harness.painted,[],"the old filter response is never painted");
  jobReads[1].resolve({marker:"current",rows:[],filtered_total:0,next_cursor:null});
  await Promise.resolve();
  await Promise.resolve();
  assert.deepEqual(harness.painted,["current"]);
});

test("Analysis re-entry aborts an older generation and immediately loads the current one", async () => {
  const reads=[];
  const api=(url,{signal}={})=>new Promise((resolve,reject)=>{
    const read={url,resolve,reject,signal}; reads.push(read);
    if(signal) signal.addEventListener("abort",()=>{
      const error=new Error("aborted"); error.name="AbortError"; reject(error);
    },{once:true});
  });
  const document={visibilityState:"visible",getElementById:()=>null};
  const harness=new Function(
    "api","document","location",
    `let PAGE_RENDER_GENERATION=1,ANALYSIS_BUSY=null,ANALYSIS_PENDING=null;
     let ANALYSIS_SNAPSHOT=null,ANALYSIS_SUMMARY=null;
     let ANALYSIS_VIEW={filter:"all",query:"",page:1,pageSize:25,auto:false,cursors:[""]};
     const paintAnalysis=()=>{},setPageFailure=()=>{},setPagePhase=()=>{},esc=String;
     ${shippedSource("analysisPageUrl")}
     ${shippedSource("renderAnalysis")}
     return {
       start:()=>renderAnalysis(1),
       reenter:()=>{PAGE_RENDER_GENERATION=2;return renderAnalysis(2);},
     };`,
  )(api,document,{hash:"#/analysis"});

  const old=harness.start();
  assert.equal(reads.length,2,"history and compact summary start together");
  await harness.reenter();
  await old;
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(reads[0].signal.aborted,true);
  assert.equal(reads[1].signal.aborted,true);
  assert.equal(reads.length,4,"the current generation starts without waiting for a timer");
  reads[2].resolve({rows:[],filtered_total:0,next_cursor:null});
  reads[3].resolve({available:true,enabled:true});
  await Promise.resolve();
  await Promise.resolve();
});

test("Analysis repaint restores row-link and disclosure focus with stable keys", () => {
  let focusKey="item:job:one",focused=[];
  const target={dataset:{analysisFocus:focusKey},focus:()=>focused.push(focusKey)};
  const openDetail={dataset:{analysisKey:"job:one"}};
  const main={
    innerHTML:"",
    querySelectorAll:(selector)=>selector.startsWith("details")?[openDetail]:[target],
  };
  const active={dataset:{analysisFocus:focusKey}};
  const document={activeElement:active,body:{contains:()=>true},getElementById:(id)=>id==="main"?main:null};
  const harness=new Function(
    "document","esc","fmtAgo","fmtBytes",
    `let ANALYSIS_SNAPSHOT=null,ANALYSIS_ROW_LOOKUP=new Map();
     let ANALYSIS_VIEW={filter:"all",query:"",page:1,pageSize:25,auto:false,cursors:[""]};
     ${shippedSource("analysisStateLabel")}
     ${shippedSource("analysisPhase")}
     ${shippedSource("analysisErrorInfo")}
     ${shippedSource("analysisErrorHtml")}
     ${shippedSource("analysisRows")}
     ${shippedSource("analysisCounts")}
     ${shippedSource("nodeLabel")}
     ${shippedSource("analysisNodeCell")}
     ${shippedSource("analysisNodeDetail")}
     ${shippedSource("analysisRowKey")}
     ${shippedSource("analysisDisposition")}
     ${shippedSource("analysisErrors")}
     ${shippedSource("analysisAttemptHistory")}
     ${shippedSource("analysisAttemptHistoryHtml")}
     ${shippedSource("analysisAction")}
     ${shippedSource("analysisCanRetry")}
     ${shippedSource("paintAnalysis")}
     return (snapshot)=>{ANALYSIS_SNAPSHOT=snapshot;paintAnalysis(snapshot);};`,
  )(document,String,()=>"just now",value=>`${value} B`);
  const row={
    row_key:"job:one",request_id:"",job_id:"one",file_id:"1",item_id:"2",title:"Movie",
    state:"ready",request_state:"",job_state:"ready",disposition:"ready",action:"rebuild",
    updated_at_ms:100,attempts:1,owner_node_id:"node-a",target_node_id:"",
    pipeline_version:"0123456789ab",source_size:123,request_error_code:"",job_error_code:"",
  };
  const snapshot={enabled:false,now_ms:200,filtered_total:1,next_cursor:null,rows:[row],summary:{available:true,total:1,ready:1}};
  harness(snapshot);
  assert.deepEqual(focused,["item:job:one"]);
  assert.match(main.innerHTML,/data-analysis-focus="item:job:one"/);
  assert.match(main.innerHTML,/data-analysis-focus="details:job:one"/);
  assert.match(main.innerHTML,/data-analysis-focus="paused-settings"/);
  assert.match(main.innerHTML,/data-analysis-key="job:one" open/);


  focusKey="details:job:one";
  active.dataset.analysisFocus=focusKey;
  target.dataset.analysisFocus=focusKey;
  harness(snapshot);
  assert.deepEqual(focused,["item:job:one","details:job:one"]);
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
    libraries:{required:["/settings","/libraries","/scan/status","/dv-conversions"],secondary:[]},
    metadata:{required:["/settings"],secondary:["/libraries"]},
    playback:{required:["/settings"],secondary:[]},
    analysis:{required:["/settings","/analysis/summary"],secondary:[]},
    maintenance:{required:["/settings","/dv-conversions"],secondary:[]},
    users:{required:["/users"],secondary:[]},
    system:{required:["/system"],secondary:["playback-events","system-log"]},
    cluster:{required:["/cluster/nodes"],secondary:["/cluster/status","cluster-log"]},
    integrations:{required:["/settings","/trakt/status"],secondary:[]},
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
       ${shippedSource("isSettingsRoute")};
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
     ${shippedSource("isSettingsRoute")};
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

test("marker actions emit bounded playback telemetry at the shipped controls", () => {
  const check = shippedSource("checkMarkers");
  const eligibility = shippedSource("markerAutoSkipEligible");
  const offer = shippedSource("renderSkip");
  const current = shippedSource("skipCurrent");
  const skip = shippedSource("skipMarker");
  const seek = shippedSource("seekTo");
  assert.match(check, /skipMarker\(m,true\)/);
  assert.match(check, /markerAutoSkipEligible\(m\)/);
  assert.match(eligibility, /provenance==="authored"\|\|m\.provenance==="manual"/);
  assert.match(current, /skipMarker\(m,false\)/);
  assert.match(offer, /event:"marker_offer"/);
  assert.match(offer, /_markerOffers\.has\(offerKey\)/);
  assert.match(skip, /marker_automatic_skip/);
  assert.match(skip, /marker_manual_skip/);
  assert.match(skip, /event:"marker_prewarm",detail:"miss"/);
  assert.match(seek, /event:"marker_seek_back",detail:"undo"/);
});

main().catch((error) => {
  failures += 1;
  process.stdout.write(`FAIL the suite itself threw\n${error && error.stack}\n`);
});

let reported = false;
process.on("beforeExit", () => {
  if (reported) return;
  reported = true;
  if (started !== finished) {
    failures += started - finished;
    process.stderr.write(`FAIL ${started - finished} test(s) never finished\n`);
  }
  if (failures) process.exitCode = 1;
});

test("A failed row lists the code every charged attempt ended with", () => {
  // `job_error_code` on an exhausted budget is always `attempt_limit`, which
  // names no cause. The row now carries the history the operator text has
  // always promised.
  const main={innerHTML:"",querySelectorAll:()=>[]};
  const document={activeElement:null,body:{contains:()=>true},getElementById:(id)=>id==="main"?main:null};
  const harness=new Function(
    "document","esc","fmtAgo","fmtBytes",
    `let ANALYSIS_SNAPSHOT=null,ANALYSIS_ROW_LOOKUP=new Map();
     let ANALYSIS_VIEW={filter:"all",query:"",page:1,pageSize:25,auto:false,cursors:[""]};
     ${shippedSource("analysisStateLabel")}
     ${shippedSource("analysisPhase")}
     ${shippedSource("analysisErrorInfo")}
     ${shippedSource("analysisErrorHtml")}
     ${shippedSource("analysisRows")}
     ${shippedSource("analysisCounts")}
     ${shippedSource("nodeLabel")}
     ${shippedSource("analysisNodeCell")}
     ${shippedSource("analysisNodeDetail")}
     ${shippedSource("analysisRowKey")}
     ${shippedSource("analysisDisposition")}
     ${shippedSource("analysisErrors")}
     ${shippedSource("analysisAttemptHistory")}
     ${shippedSource("analysisAttemptHistoryHtml")}
     ${shippedSource("analysisAction")}
     ${shippedSource("analysisCanRetry")}
     ${shippedSource("paintAnalysis")}
     return (snapshot)=>{ANALYSIS_SNAPSHOT=snapshot;paintAnalysis(snapshot);};`,
  )(document,String,()=>"just now",value=>`${value} B`);
  const base={
    row_key:"job:one",request_id:"",job_id:"one",file_id:"1",item_id:"2",title:"Movie",
    state:"failed",request_state:"",job_state:"failed",disposition:"attention",action:"retry",
    updated_at_ms:100,attempts:5,owner_node_id:"node-a",target_node_id:"",
    pipeline_version:"0123456789ab",source_size:123,request_error_code:"",
    job_error_code:"attempt_limit",
    job_attempt_errors:["source_unavailable","lease_expired","source_attestation_failed"],
  };
  const snapshot={enabled:false,now_ms:200,filtered_total:1,next_cursor:null,rows:[base],
    summary:{available:true,total:1,ready:0}};
  harness(snapshot);
  assert.match(main.innerHTML,/analysis-attempts/);
  assert.match(main.innerHTML,/source_unavailable/);
  assert.match(main.innerHTML,/lease_expired/);
  assert.match(main.innerHTML,/source_attestation_failed/);
  // The terminal code alone is still shown, and still says nothing on its own.
  assert.match(main.innerHTML,/attempt_limit/);

  // A row that has charged nothing renders no list at all.
  harness({...snapshot,rows:[{...base,job_attempt_errors:[]}]});
  assert.ok(!/analysis-attempts/.test(main.innerHTML));
});
