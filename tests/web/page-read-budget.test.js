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

function shippedConstant(name) {
  const match = SHIPPED_UI.match(new RegExp(`\\bconst\\s+${name}\\s*=\\s*([^;]+);`));
  assert.ok(match, `index.html no longer declares ${name}`);
  return { source: match[0], value: new Function(`${match[0]}; return ${name};`)() };
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

test("Home bounds preview concurrency, isolates failures, and preserves grouping", async () => {
  const libraries = Array.from({ length: 14 }, (_, index) => ({
    id: index + 1,
    name: `Library ${index + 1}`,
    kind: index % 2 === 0 ? "movies" : "shows",
  }));
  const started = [];
  const releases = new Map();
  let inFlight = 0;
  let maxInFlight = 0;
  const api = (url) => {
    if (url === "/libraries") return Promise.resolve(libraries);
    if (url === "/hubs") return Promise.resolve({ continue_watching: [] });
    if (url === "/coming-soon") return Promise.resolve({ configured: false, entries: [] });
    const match = /^\/libraries\/(\d+)\/items/.exec(url);
    assert.ok(match, `unexpected request ${url}`);
    const id = Number(match[1]);
    started.push(id);
    inFlight += 1;
    maxInFlight = Math.max(maxInFlight, inFlight);
    return new Promise((resolve, reject) => releases.set(id, {
      resolve(value) { inFlight -= 1; releases.delete(id); resolve(value); },
      reject(error) { inFlight -= 1; releases.delete(id); reject(error); },
    }));
  };
  const homeGroup = () => "category";
  const libCategory = (lib) => lib.kind === "movies"
    ? { key: "movie", name: "Movies" }
    : { key: "show", name: "TV Shows" };
  const catOrder = (key) => key === "movie" ? 0 : 1;
  const loadHome = new Function(
    "api", "homeGroup", "libCategory", "catOrder",
    `${shippedConstant("HOME_PREVIEW_CONCURRENCY").source}
     ${shippedSource("loadHomePreviews")};
     ${shippedSource("loadHome")}; return loadHome;`,
  )(api, homeGroup, libCategory, catOrder);

  const pending = loadHome();
  await nextTurn();
  assert.deepEqual(started, [1, 2, 3, 4, 5, 6]);

  // Finish requests in reverse order. Each completion admits one new request;
  // the failed library must become an empty preview rather than reject Home.
  while (releases.size) {
    const id = Math.max(...releases.keys());
    const release = releases.get(id);
    if (id === 7) release.reject(new Error("one library is unavailable"));
    else release.resolve({ items: [{ id: id * 10 }], total: 1 });
    await nextTurn();
  }
  const page = await pending;
  assert.equal(shippedConstant("HOME_PREVIEW_CONCURRENCY").value, 6);
  assert.equal(maxInFlight, 6);
  assert.deepEqual(started, libraries.map((library) => library.id));
  assert.deepEqual(page.sections.map((section) => section.key), ["movie", "show"]);
  assert.equal(page.sections[0].unavailable, true);
  assert.equal(page.sections[0].total, null, "partial inventory is not reported as a true total");
  assert.deepEqual(page.sections[0].items.map((item) => item.id), [10, 30, 50, 90, 110, 130]);
  assert.deepEqual(page.sections[1].items.map((item) => item.id), [20, 40, 60, 80, 100, 120, 140]);
});

test("Home stops the preview queue and rejects when authorization expires", async () => {
  const libraries = Array.from({ length: 12 }, (_, index) => ({
    id: index + 1, name: `Library ${index + 1}`, kind: "movies",
  }));
  const started = [];
  const releases = new Map();
  const api = (url) => {
    if (url === "/libraries") return Promise.resolve(libraries);
    if (url === "/hubs") return Promise.resolve({ continue_watching: [], recently_added: [] });
    if (url === "/coming-soon") return Promise.resolve({ configured: false, entries: [] });
    const id = Number(/^\/libraries\/(\d+)\/items/.exec(url)[1]);
    started.push(id);
    return new Promise((resolve, reject) => releases.set(id, { resolve, reject }));
  };
  const loadHome = new Function(
    "api", "homeGroup", "libCategory", "catOrder",
    `${shippedConstant("HOME_PREVIEW_CONCURRENCY").source}
     ${shippedSource("loadHomePreviews")};
     ${shippedSource("loadHome")}; return loadHome;`,
  )(api, () => "category", () => ({ key: "movie", name: "Movies" }), () => 0);

  const pending = loadHome();
  await nextTurn();
  assert.deepEqual(started, [1, 2, 3, 4, 5, 6]);
  const unauthorized = new Error("unauthorized"); unauthorized.status = 401;
  releases.get(1).reject(unauthorized);
  await assert.rejects(pending, (error) => error === unauthorized);
  await nextTurn();
  assert.deepEqual(started, [1, 2, 3, 4, 5, 6], "no queued request starts after revocation");
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

test("Settings polls only the visible data panel and never overlaps", async () => {
  const tick = shippedSource("settingsTick");
  assert.match(tick, /\|\|SETTINGS_TICKING\) return/);
  assert.match(tick, /if\(tab==="libraries"\)\{[\s\S]*api\("\/scan\/status"\)/);
  assert.match(tick, /if\(tab==="metadata"&&!TRAKT_EDIT\)\{[\s\S]*api\("\/trakt\/status"\)/);
  assert.match(tick, /await Promise\.all\(secondary\)/);
  assert.match(tick, /finally\{ SETTINGS_TICKING=false; \}/);

  const location = { hash: "#/settings" };
  const document = { visibilityState: "visible", getElementById: () => null };
  const requests = [];
  let tab = "libraries";
  const api = (url) => new Promise((resolve) => requests.push({ url, resolve }));
  const harness = new Function(
    "document", "location", "api", "settingsTab", "refreshLogs", "refreshClusterLogs", "paintTrakt",
    `let SETTINGS_TICKING=false,TRAKT_EDIT=false,TRAKT=null;
     ${tick}; return {settingsTick,busy:()=>SETTINGS_TICKING};`,
  )(
    document, location, api, () => tab,
    async () => {}, async () => {}, () => {},
  );

  const first = harness.settingsTick();
  const overlap = harness.settingsTick();
  assert.deepEqual(requests.map((request) => request.url), ["/scan/status"]);
  assert.equal(harness.busy(), true);
  requests[0].resolve({});
  await Promise.all([first, overlap]);
  assert.equal(harness.busy(), false);

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
    `let LOGS_PROMISE=null,LOGS_ACTIVE=null,LOGS_PENDING=null,
       CLUSTER_LOGS_PROMISE=null,CLUSTER_LOGS_ACTIVE=null,CLUSTER_LOGS_PENDING=null;
     ${shippedSource("sameLogRequest")};
     ${shippedSource("runLogRequest")};
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

test("Home render generations refuse out-of-order route completions", async () => {
  const location = { hash: "" };
  const main = { innerHTML: "" };
  const document = { getElementById: () => main };
  const loads = [];
  const phases = [];
  const loadHome = () => new Promise((resolve) => loads.push(resolve));
  const harness = new Function(
    "document", "location", "loadHome", "layoutChrome", "layoutView", "wireRails", "restoreScroll", "setPagePhase",
    `let PAGE_RENDER_GENERATION=0;
     ${shippedTopLevelSource("viewHome")};
     return {viewHome,navigate:(hash)=>{location.hash=hash;PAGE_RENDER_GENERATION++;}};`,
  )(
    document, location, loadHome, () => {}, (_, page) => page.marker,
    () => {}, () => {}, (_, generation, phase) => phases.push({ generation, phase }),
  );

  const old = harness.viewHome();
  assert.deepEqual(phases, [{ generation: 1, phase: "shell" }],
    "the shell phase commits before the delayed endpoint resolves");
  harness.navigate("#/settings");
  harness.navigate("");
  const current = harness.viewHome();
  assert.deepEqual(phases.filter((entry) => entry.generation === 4), [
    { generation: 4, phase: "shell" },
  ]);
  loads[1]({ marker: "current" });
  await current;
  assert.equal(main.innerHTML, "current");
  assert.deepEqual(phases.filter((entry) => entry.generation === 4), [
    { generation: 4, phase: "shell" },
    { generation: 4, phase: "content" },
    { generation: 4, phase: "settled" },
  ], "the delayed response produces ordered content and settled phases");
  loads[0]({ marker: "stale" });
  await old;
  assert.equal(main.innerHTML, "current");
  assert.deepEqual(phases.filter((entry) => entry.generation === 1), [
    { generation: 1, phase: "shell" },
  ], "the stale delayed response cannot commit a later phase");

  harness.navigate("#/unknown-fallback");
  const fallback = harness.viewHome();
  loads[2]({ marker: "fallback" });
  await fallback;
  assert.equal(main.innerHTML, "fallback");
});

test("Activity and Settings stale loads cannot install the page timer", async () => {
  const activityLocation = { hash: "#/activity" };
  const activityLoads = [];
  const activityPhases = [];
  const timers = [];
  const activityHarness = new Function(
    "location", "layoutChrome", "renderActivityBody", "setPageTimer", "setPagePhase",
    `let PAGE_RENDER_GENERATION=0;
     ${shippedTopLevelSource("viewActivity")};
     return {viewActivity,navigate:(hash)=>{location.hash=hash;PAGE_RENDER_GENERATION++;}};`,
  )(
    activityLocation, () => {},
    (generation) => new Promise((resolve) => activityLoads.push({ generation, resolve })),
    (_, __, generation) => timers.push(generation),
    (_, generation, phase) => activityPhases.push({ generation, phase }),
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

  const settingsLocation = { hash: "#/settings" };
  const calls = [];
  const commits = [];
  const settingsTimers = [];
  const api = (url) => new Promise((resolve) => calls.push({ url, resolve }));
  const settingsHarness = new Function(
    "location", "api", "layoutChrome", "forgetJoinToken", "renderSettings", "setPageTimer", "settingsTick", "setPagePhase",
    `let ME={is_admin:true},PAGE_RENDER_GENERATION=0,SETTINGS=null,TRAKT=null,TRAKT_EDIT=false,
       CLUSTER_LOADED=false,SETTINGS_DATA=null;
     ${shippedTopLevelSource("viewSettings")};
     return {viewSettings,navigate:(hash)=>{location.hash=hash;PAGE_RENDER_GENERATION++;},
       data:()=>SETTINGS_DATA};`,
  )(
    settingsLocation, api, () => {}, () => {}, () => commits.push("render"),
    (_, __, generation) => settingsTimers.push(generation), () => {}, () => {},
  );
  const oldSettings = settingsHarness.viewSettings();
  await nextTurn();
  const expected = ["/libraries", "/settings", "/scan/status", "/system", "/users", "/trakt/status"];
  assert.deepEqual(calls.slice(0, 6).map((call) => call.url), expected);
  assert.match(calls[6].url, /^\/system\/playback-events\?/);
  settingsHarness.navigate("#/");
  settingsHarness.navigate("#/settings");
  const currentSettings = settingsHarness.viewSettings();
  await nextTurn();
  const resolveWave = (wave, marker) => {
    const offset = wave * 7;
    const values = [[], { marker }, {}, {}, [], {}, []];
    values.forEach((value, index) => calls[offset + index].resolve(value));
  };
  resolveWave(1, "current");
  await currentSettings;
  assert.equal(settingsHarness.data().settings.marker, "current");
  assert.equal(commits.length, 1);
  assert.equal(settingsTimers.length, 1);
  resolveWave(0, "stale");
  await oldSettings;
  assert.equal(settingsHarness.data().settings.marker, "current");
  assert.equal(commits.length, 1);
  assert.equal(settingsTimers.length, 1);
});

test("Settings System content precedes its delayed visible-log settlement", async () => {
  const location = { hash: "#/settings" };
  const phases = [];
  const timers = [];
  let releaseLogs;
  const logs = new Promise((resolve) => { releaseLogs = resolve; });
  const api = (url) => Promise.resolve(
    url === "/settings" ? {} : url === "/trakt/status" ? {} : [],
  );
  const harness = new Function(
    "location", "api", "layoutChrome", "forgetJoinToken", "renderSettings",
    "setPageTimer", "settingsTick", "setPagePhase",
    `let ME={is_admin:true},PAGE_RENDER_GENERATION=0,SETTINGS=null,TRAKT=null,TRAKT_EDIT=false,
       CLUSTER_LOADED=false,SETTINGS_DATA=null;
     ${shippedTopLevelSource("viewSettings")};
     return {viewSettings};`,
  )(
    location, api, () => {}, () => {}, () => logs,
    (_, __, generation) => timers.push(generation), () => {},
    (_, generation, phase) => phases.push({ generation, phase }),
  );

  const pending = harness.viewSettings();
  await nextTurn();
  assert.deepEqual(phases, [
    { generation: 1, phase: "shell" },
    { generation: 1, phase: "content" },
  ]);
  assert.deepEqual(timers, [1], "measurement does not delay normal timer installation");
  await pending;
  releaseLogs();
  await nextTurn();
  assert.deepEqual(phases.at(-1), { generation: 1, phase: "settled" });
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

  for (const [name, route] of [["viewHome", "route"], ["viewSettings", "route"]]) {
    const source = shippedTopLevelSource(name);
    assert.match(source, new RegExp(`setPagePhase\\(${route},generation,"shell"\\)`));
    assert.match(source, new RegExp(`setPagePhase\\(${route},generation,"content"\\)`));
    assert.match(source, new RegExp(`setPagePhase\\(${route},generation,"settled"\\)`));
  }
  const activity = shippedSource("renderActivityBody");
  assert.match(activity, /setPagePhase\("#\/activity",generation,"content"\)/);
  assert.match(activity, /setPagePhase\("#\/activity",generation,"settled"\)/);
});

process.on("beforeExit", () => {
  if (failures) process.exitCode = 1;
});
