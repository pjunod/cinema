#!/usr/bin/env node
"use strict";

// The Settings page after its reorganisation: one route per section, a
// grouped rail, one Save per card, and row drawers instead of controls inside
// table cells. These drive the shipped functions rather than pinning their
// text, so a change that keeps the behaviour is free to change the source.

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
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  const end = ends.length ? Math.min(...ends) : -1;
  return (end === -1 ? rest : rest.slice(0, end)).trimEnd();
}
function shippedConst(name) {
  const match = SHIPPED_UI.match(new RegExp(`\\nconst ${name}=([\\s\\S]*?);\\n`));
  assert.ok(match, `index.html no longer declares const ${name}`);
  return match[0];
}
const esc = (value) => String(value)
  .replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;").replaceAll("'", "&#39;");

let failures = 0, started = 0, finished = 0;
const QUEUE = [];
function test(name, run) { QUEUE.push({ name, run }); }
async function main() {
  for (const { name, run } of QUEUE) {
    started += 1;
    try { await run(); process.stdout.write(`PASS ${name}\n`); }
    catch (error) { failures += 1; process.stderr.write(`FAIL ${name}\n${error && error.stack}\n`); }
    finished += 1;
  }
}

// The section registry and the route helpers, as shipped.
function routing(storage, hash) {
  const location = { hash };
  return new Function(
    "location", "localStorage",
    `${shippedConst("SET_GROUPS")}${shippedConst("SET_TABS")}
     ${shippedSource("isSettingsRoute")}
     ${shippedSource("settingsRouteTab")}
     ${shippedSource("settingsTab")}
     ${shippedSource("setSettingsTab")}
     return {SET_GROUPS,SET_TABS,isSettingsRoute,settingsRouteTab,settingsTab,setSettingsTab,location};`,
  )(location, storage);
}
function memoryStorage(initial) {
  const map = new Map(Object.entries(initial || {}));
  return { getItem: (k) => (map.has(k) ? map.get(k) : null), setItem: (k, v) => map.set(k, String(v)), map };
}

test("every section is a route, grouped in the rail's order", () => {
  const r = routing(memoryStorage(), "#/settings/playback");
  assert.deepEqual(r.SET_GROUPS.map(([group]) => group), ["Content", "Playback", "Server", "Outside", "Developer"]);
  assert.deepEqual(r.SET_TABS.map(([id]) => id), [
    "libraries", "metadata", "playback", "analysis",
    "maintenance", "users", "system", "cluster", "integrations", "developer",
  ]);
  const dispatch = {
    metadata: "metadataPanel(d.settings)",
    playback: "playbackPanel(d.settings)",
    analysis: "analysisSettingsPanel(d.settings,d.analysis)",
    maintenance: "maintenancePanel(d.settings,d.dvConversions)",
    users: "usersPanel(d.users)",
    system: "systemPanel(d.sys,d.playbackEvents)",
    cluster: "clusterPanel(d)",
    integrations: "integrationsPanel(d.settings,d.trakt)",
    developer: "developerPanel(d.settings)",
  };
  const panel = shippedSource("settingsPanel");
  for (const [id] of r.SET_TABS) {
    assert.equal(r.settingsRouteTab(`#/settings/${id}`), id);
    if (id === "libraries")
      assert.match(panel, /return librariesPanel\(d\.libs,d\.status,d\.settings,d\.dvConversions\)/, "libraries is the fall-through panel");
    else
      assert.ok(panel.includes(`if(tab==="${id}") `) && panel.includes(dispatch[id]),
        `${id} routes to ${dispatch[id]}`);
  }
  assert.equal(r.settingsRouteTab("#/settings/nothere"), null, "an unknown section is not a route");
  assert.equal(r.settingsRouteTab("#/settings/"), null);
  assert.equal(r.settingsRouteTab("#/settings/Playback"), null, "routes are lower-case");
});

test("the route names the section; storage is only the landing default", () => {
  const storage = memoryStorage({ plurx_settings_tab: "users" });
  assert.equal(routing(storage, "#/settings/playback").settingsTab(), "playback", "the route wins");
  assert.equal(routing(storage, "#/settings").settingsTab(), "users", "the bare route lands on the last section");
  assert.equal(routing(storage, "#/admin").settingsTab(), "users", "the legacy route lands there too");
  assert.equal(routing(memoryStorage({ plurx_settings_tab: "gone" }), "#/settings").settingsTab(), "libraries",
    "a stale stored section falls back to the first one");
  const throwing = { getItem: () => { throw new Error("private mode"); }, setItem: () => { throw new Error("private mode"); } };
  assert.equal(routing(throwing, "#/settings").settingsTab(), "libraries", "storage failures never break the page");
});

test("switching sections is a navigation, never a repaint around the router", () => {
  const storage = memoryStorage();
  const r = routing(storage, "#/settings/libraries");
  r.setSettingsTab("playback");
  assert.equal(r.location.hash, "#/settings/playback");
  r.setSettingsTab("playback");
  assert.equal(r.location.hash, "#/settings/playback", "the current section is a no-op");
  r.setSettingsTab("bogus");
  assert.equal(r.location.hash, "#/settings/playback", "an unknown section is refused");
  // viewSettings, not setSettingsTab, records the landing default — so that a
  // deep link someone followed also becomes where Settings opens next time.
  assert.equal(storage.map.has("plurx_settings_tab"), false);
  assert.match(shippedSource("viewSettings"), /localStorage\.setItem\("plurx_settings_tab",tab\)/);
});

test("render() keeps the cached aggregate across a section switch and rewrites bare routes", () => {
  const render = shippedSource("render");
  assert.match(render, /const stayingInSettings=isSettingsRoute\(h\)&&isSettingsRoute\(LAST_ROUTE\)/);
  assert.match(render, /LAST_ROUTE=h;/);
  assert.match(render, /viewSettings\(generation,!stayingInSettings\)/);
  assert.match(render, /if\(isSettingsRoute\(h\)&&!settingsRouteTab\(h\)&&ME\.is_admin\)\{\s*h=`#\/settings\/\$\{settingsTab\(\)\}`;\s*try\{ history\.replaceState\(null,"",h\); \}catch\(e\)\{\}/);
  assert.match(SHIPPED_UI, /\nlet LAST_ROUTE=null;\nwindow\.addEventListener\("hashchange",render\);/);
  // Every route comparison in the settings machinery goes through the helper.
  for (const fn of ["settingsCurrent", "renderSettings", "settingsTick", "pagePhaseName"]) {
    assert.match(shippedSource(fn), /isSettingsRoute\(/, `${fn} recognises section routes`);
    assert.doesNotMatch(shippedSource(fn), /"#\/settings"|"#\/admin"/, `${fn} has no literal route`);
  }
});

test("the rail marks the active section, shows counts it already has, and never fetches", () => {
  const rail = new Function(
    "SETTINGS_DATA", "esc", "PlurxClusterPanel",
    `${shippedConst("SET_GROUPS")}${shippedConst("SET_TABS")}
     ${shippedSource("settingsTabAside")}
     ${shippedSource("settingsTabsHtml")}
     return settingsTabsHtml;`,
  );
  const data = { libs: [{ kind: "movies" }, { kind: "home" }], users: [{}, {}, {}], settings: { tmdb_configured: false } };
  const html = rail(data, esc, { clusterStateView: () => ({ tone: "good" }) })("users");
  assert.equal((html.match(/class="setgroup"/g) || []).length, 5);
  assert.match(html, /class="settab active" role="link" aria-current="page" onclick="setSettingsTab\('users'\)">Users<span class="setn">3<\/span>/);
  assert.match(html, /onclick="setSettingsTab\('libraries'\)">Libraries<span class="setn">2<\/span>/);
  assert.match(html, /Metadata<span class="setdot"/, "a missing TMDB key with a provider library is flagged before the section is opened");
  assert.doesNotMatch(html, /Cluster<span/, "cluster shows nothing until its data has been read");
  const quiet = rail({ libs: [{ kind: "home" }], settings: { tmdb_configured: false } }, esc, {})("libraries");
  assert.doesNotMatch(quiet, /setdot/, "home and books libraries never want a key");
  assert.doesNotMatch(shippedSource("settingsTabAside"), /api\(/, "the rail spends no request of its own");
  const sqlite = rail({ cluster: { unavailable: true, code: "membership_unavailable" } }, esc, {})("cluster");
  assert.match(sqlite, /Cluster<span class="setn">sqlite<\/span>/);
});

test("a card's Save wakes on a change and sleeps again once saved", () => {
  function element(tag, cls) {
    return { tag, classes: new Set(cls || []), disabled: true, textContent: "", children: [] };
  }
  const save = element("button", ["primary"]), state = element("span", ["setstate"]);
  const card = {
    classList: { add: (c) => card.classes.add(c), remove: (c) => card.classes.delete(c), contains: (c) => card.classes.has(c) },
    classes: new Set(["card", "setcard"]),
    querySelector: (sel) => (sel.includes("button") ? save : state),
  };
  card.closest = (sel) => (sel === ".setcard" ? card : null);
  const input = { closest: card.closest };
  const api = new Function(`${shippedSource("markSetCard")}\n${shippedSource("setCardSaved")}\nreturn {markSetCard,setCardSaved};`)();
  api.markSetCard(input);
  assert.equal(save.disabled, false);
  assert.equal(state.textContent, "Unsaved changes");
  assert.ok(card.classes.has("dirty"));
  api.setCardSaved(input);
  assert.equal(save.disabled, true);
  assert.equal(state.textContent, "Saved");
  assert.ok(!card.classes.has("dirty"));
  // A browser-local card saves on change; it never advertises unsaved state.
  card.classes.add("local"); save.disabled = true; state.textContent = "";
  api.markSetCard(input);
  assert.equal(save.disabled, true);
  assert.equal(state.textContent, "");
  assert.doesNotThrow(() => api.markSetCard(null));
  assert.doesNotThrow(() => api.setCardSaved(null));
});

test("Playback saves per card, and each card writes only its own fields", () => {
  const writes = {};
  const run = (fn, ids) => new Function(
    "api", "document", "cacheSettings", "toast", "setCardSaved", "SERVER", "SETTINGS",
    `${shippedSource(fn)} return ${fn};`,
  )(
    async (path, opts) => { writes[fn] = { path, body: opts.body }; return {}; },
    { getElementById: (id) => { assert.ok(ids.includes(id), `${fn} reads ${id}`); return { value: "v", checked: true, textContent: "" }; } },
    (v) => v, () => {}, () => {}, {}, {},
  );
  const defaults = ["pal", "psl", "psm", "perr"];
  // The two switches that are off on purpose moved to Developer, so Streaming
  // no longer writes them: a card that saves a field it does not show can turn
  // something back on that an operator deliberately turned off.
  const streaming = ["prr", "pabr", "phr", "phb", "pha", "pvod", "pvlr", "pvws", "pvmb", "serr"];
  const developer = ["pcpv1", "dverr"];
  const experimental = ["phs", "dxerr"];
  return Promise.all([
    run("savePlaybackDefaults", defaults)({ disabled: false }),
    run("saveStreaming", streaming)({ disabled: false }),
    run("saveDeveloper", developer)({ disabled: false }),
    run("saveExperimental", experimental)({ disabled: false }),
  ]).then(() => {
    assert.deepEqual(Object.keys(writes.savePlaybackDefaults.body).sort(), ["default_audio_lang", "default_sub_lang", "sub_mode"]);
    assert.deepEqual(Object.keys(writes.saveStreaming.body).sort(), [
      "hls_ahead_max_secs", "hls_burst_secs", "hls_readrate", "playback_auto_abr",
      "stream_readrate", "vod_block_budget_secs", "vod_live_recovery",
      "vod_materialize_budget_secs", "vod_presentation", "vod_working_set_bytes",
    ]);
    assert.deepEqual(Object.keys(writes.saveDeveloper.body).sort(), ["playback_control_protocol_v1"]);
    assert.deepEqual(Object.keys(writes.saveExperimental.body).sort(), ["hls_typeless_sliding"]);
    assert.equal(writes.savePlaybackDefaults.path, "/settings");
    assert.equal(writes.saveStreaming.path, "/settings");
    assert.equal(writes.saveDeveloper.path, "/settings");
  });
});

test("Developer is where the switches that cost something live", () => {
  const panel = new Function(
    "setHead", "setCard", "cardHead", "togRow", "setCardFoot", "esc",
    `${shippedSource("developerPanel")} return developerPanel;`,
  )(
    (title, sub) => `HEAD:${title}|${sub}`,
    (body) => `CARD[${body}]`,
    (title, sub) => `CARDHEAD:${title}|${sub || ""}`,
    (id, label, note) => `TOG:${id}|${label}|${note}`,
    (fn) => `FOOT:${fn}`,
    esc,
  );
  const html = panel({ playback_control_protocol_v1: true, hls_typeless_sliding: false });
  for (const id of ["pcpv1", "phs"]) {
    assert.match(html, new RegExp(`TOG:${id}\\|`), `Developer is missing the ${id} switch`);
  }
  assert.match(html, /FOOT:saveDeveloper/);
  assert.match(html, /FOOT:saveExperimental/);
  // The section says what it is for, so a switch that costs something has
  // somewhere honest to land rather than being buried under Streaming.
  assert.match(html, /off on purpose/);
});

test("Maintenance owns the timers, and each of its cards saves its own fields", () => {
  const panel = shippedSource("maintenancePanel");
  for (const card of ["precachePanel", "dvDiskPanel", "telemetryPanel"]) assert.match(panel, new RegExp(`${card}\\(`));
  for (const id of ["job-probe", "job-art", "job-clean", "job-boot"]) assert.match(panel, new RegExp(`"${id}"`));
  const libraries = shippedSource("librariesPanel");
  for (const gone of ["maintenancePanel", "dvDiskPanel", "precachePanel", "telemetry"])
    assert.doesNotMatch(libraries, new RegExp(gone), `Libraries no longer carries ${gone}`);
  const fields = (fn) => {
    const body = shippedSource(fn);
    return [...body.matchAll(/^\s*([a-z_]+):(?:Number\()?document\.getElementById/gm)].map((m) => m[1]).sort();
  };
  assert.deepEqual(fields("saveMaintenance"), ["artwork_retry_mins", "probe_retry_mins", "scan_on_startup", "transcode_cleanup_mins"]);
  assert.deepEqual(fields("savePrecache"), ["cache_max_gb", "cache_produce_mins"]);
  assert.deepEqual(fields("saveTelemetry"), ["telemetry_retain_days"]);
  assert.deepEqual(fields("saveDvDiskSettings"), ["dv_disk_convert_parallel", "dv_disk_keep_original"]);
  for (const fn of ["saveMaintenance", "savePrecache", "saveTelemetry", "saveDvDiskSettings"])
    assert.doesNotMatch(shippedSource(fn), /\brender\(\)/, `${fn} no longer repaints the whole app to show a save`);
});

test("Metadata holds the providers; Integrations holds the services", () => {
  const metadata = shippedSource("metadataPanel");
  const integrations = shippedSource("integrationsPanel");
  assert.match(metadata, /"tk"/); assert.match(metadata, /"ok"/);
  assert.doesNotMatch(metadata, /traktCardHtml|monarrCardHtml/);
  assert.match(integrations, /traktCardHtml\(trakt\)/);
  assert.match(integrations, /monarrCardHtml\(\)/);
  assert.match(SHIPPED_UI, /if\(tab==="integrations"\)\s*return integrationsPanel\(d\.settings,d\.trakt\)/);
  assert.match(SHIPPED_UI, /if\(tab==="metadata"\)\s*return metadataPanel\(d\.settings\)/);
  // The Trakt device-link poll follows the card to its new section.
  assert.match(shippedSource("settingsTick"), /if\(tab==="integrations"&&!TRAKT_EDIT\)/);
  assert.match(shippedSource("paintTrakt"), /traktcard/);
});

test("a library row reports; its drawer configures; only one drawer is open", () => {
  const dvModeSelect = () => "<dv/>";
  const build = (drawer) => new Function(
    "esc", "statusText", "dvModeSelect", "agoLabel", "everySelect", "SCAN_EVERY", "REFRESH_EVERY",
    `let LIB_DRAWER=${JSON.stringify(drawer)};
     ${[shippedSource("libKindSelect"), shippedSource("libScheduleFields"), shippedSource("libDrawerHtml"), shippedSource("libRow")].join("\n")}
     return libRow;`,
  )(esc, () => "idle", dvModeSelect, () => "an hour ago", (id) => `<select id="${id}"></select>`, [], []);
  const movies = { id: 1, name: "Movies", kind: "movies", paths: ["/m"] };
  const home = { id: 2, name: "Home", kind: "home", paths: ["/h"] };
  const closed = build(null)(movies, {}, {});
  assert.doesNotMatch(closed, /sched-scan-1|<dv\/>|setdrawer/, "a closed row carries no configuration controls");
  assert.match(closed, /aria-expanded="false" onclick="openLibDrawer\(1\)"/);
  assert.doesNotMatch(closed, /aria-controls/, "a closed row does not point aria-controls at a drawer that is not in the DOM");
  const open = build(1)(movies, {}, {});
  assert.match(open, /<tr id="librow-1" class="setopen">/);
  assert.match(open, /aria-expanded="true" aria-controls="libdrawer-1"/, "an open row points aria-controls at its live drawer");
  assert.match(open, /<tr class="setdrawer" id="libdrawer-1">/);
  for (const id of ["eln-1", "elk-1", "elp-1", "sched-scan-1", "sched-ref-1"]) assert.match(open, new RegExp(`id="${id}"`));
  assert.match(open, /<dv\/>/, "the Dolby Vision mode lives in the drawer");
  assert.match(open, /saveLibDrawer\(1,this\)/);
  assert.match(open, /delLib\(1\)/);
  assert.doesNotMatch(build(1)(home, {}, {}), /setdrawer/, "another row's drawer stays shut");
  // The Configure toggle opens a row, closes it on a second click, and never
  // stacks two open drawers.
  const toggle = new Function(
    "renderSettings", "document",
    `let LIB_DRAWER=null; ${shippedSource("openLibDrawer")} return {open:(id)=>openLibDrawer(id),get:()=>LIB_DRAWER};`,
  )(() => {}, { querySelector: () => null });
  toggle.open(1); assert.equal(toggle.get(), 1, "a row opens");
  toggle.open(1); assert.equal(toggle.get(), null, "the same row closes");
  toggle.open(1); toggle.open(2); assert.equal(toggle.get(), 2, "opening another switches, never stacks");
  assert.match(build(null)(home, {}, {}), /disabled title="This kind of library has no metadata provider"/);
  assert.doesNotMatch(build(null)({ id: 3, name: "Anime", kind: "shows", anime: true, paths: [] }, {}, {}), /no metadata provider/,
    "anime has a provider (AniList), so Refresh art stays live");
});

test("the drawer's Save writes identity then schedule, and stops on the first refusal", async () => {
  const calls = [];
  let fail = false;
  const values = { "eln-1": "Movies ", "elk-1": "anime", "elp-1": " /a, /b ,", "sched-scan-1": "60", "sched-ref-1": "10080", "dv-mode-1": "auto" };
  const err = { textContent: "" };
  const save = new Function(
    "api", "document", "invalidateLibs", "toast", "viewSettings",
    `let LIB_DRAWER=1; ${shippedSource("saveLibDrawer")} return {saveLibDrawer,drawer:()=>LIB_DRAWER};`,
  )(
    async (path, opts) => { calls.push([opts.method, path, opts.body]); if (fail && calls.length === 1) throw new Error("no such path"); return {}; },
    { getElementById: (id) => (id === "ele-1" ? err : id in values ? { value: values[id] } : null) },
    () => calls.push(["invalidate"]), () => {}, () => calls.push(["view"]),
  );
  const btn = { disabled: false };
  await save.saveLibDrawer(1, btn);
  assert.deepEqual(calls[0], ["PUT", "/libraries/1", { name: "Movies", kind: "shows", paths: ["/a", "/b"], anime: true }]);
  assert.deepEqual(calls[1], ["PUT", "/libraries/1/schedule", { scan_interval_mins: 60, refresh_interval_mins: 10080 }]);
  assert.deepEqual(calls[2], ["PUT", "/libraries/1/dv-conversion", { mode: "auto" }]);
  assert.deepEqual(calls.slice(3), [["invalidate"], ["view"]]);
  assert.equal(save.drawer(), null, "a successful save closes the drawer");
  calls.length = 0; fail = true;
  await save.saveLibDrawer(1, btn);
  assert.equal(calls.length, 1, "a refused identity write never reaches the schedule");
  assert.equal(err.textContent, "no such path");
  assert.equal(btn.disabled, false, "the button comes back for a retry");
});

test("password reset is a form with a confirm field, not two prompt() dialogs", async () => {
  assert.doesNotMatch(shippedSource("resetPw"), /prompt\(/);
  assert.doesNotMatch(shippedSource("addUser"), /prompt\(/);
  const calls = [];
  const err = { textContent: "" };
  const values = { "up-7": "hunter2hunter2", "up2-7": "hunter2hunter2" };
  const reset = new Function(
    "api", "document", "toast", "renderSettings",
    `let USER_DRAWER=7; ${shippedSource("resetPw")} return {resetPw,drawer:()=>USER_DRAWER};`,
  )(
    async (path, opts) => { calls.push([path, opts.body]); return {}; },
    { getElementById: (id) => (id === "uerr" ? err : { value: values[id] }) },
    () => {}, () => calls.push(["render"]),
  );
  values["up-7"] = "short"; values["up2-7"] = "short";
  await reset.resetPw(7, "guest", { disabled: false });
  assert.equal(calls.length, 0, "a password under 8 characters writes nothing");
  assert.match(err.textContent, /at least 8/);
  values["up-7"] = "hunter2hunter2"; values["up2-7"] = "different";
  await reset.resetPw(7, "guest", { disabled: false });
  assert.equal(calls.length, 0, "a mismatch writes nothing");
  assert.match(err.textContent, /don't match/);
  values["up2-7"] = "hunter2hunter2";
  await reset.resetPw(7, "guest", { disabled: false });
  assert.deepEqual(calls[0], ["/users/7", { password: "hunter2hunter2" }]);
  assert.equal(reset.drawer(), null);
});

test("the viewer's four appearance choices share one popover", () => {
  assert.equal((SHIPPED_UI.match(/id="lookmenu"/g) || []).length, 3, "one menu per layout chrome");
  assert.equal((SHIPPED_UI.match(/id="thememenu"|id="appmenu"|id="sizemenu"/g) || []).length, 0);
  const menu = shippedSource("lookMenuHtml");
  assert.match(menu, /themeMenuHtml\(\)/); assert.match(menu, /appMenuHtml\(\)/); assert.match(menu, /sizeMenuHtml\(\)/);
  assert.match(shippedSource("themeMenuHtml"), /layoutMenuHtml\(\)/);
  assert.match(shippedSource("closeMenus"), /\["lookmenu","profilemenu"\]/);
  for (const fn of ["setTheme", "setAppearance", "setIconSize"])
    assert.match(shippedSource(fn), /repaintLookMenu\(\)/, `${fn} repaints the shared menu`);
  assert.match(shippedSource("sizeMenuHtml"), /Poster size/, "named as the mobile apps name it");
});

main().then(() => {
  if (started !== finished) failures += started - finished;
  process.stdout.write(`${started - failures}/${started} passed\n`);
  process.exit(failures ? 1 : 0);
});
