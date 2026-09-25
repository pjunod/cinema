#!/usr/bin/env node
"use strict";

// The Settings page after its reorganisation: one route per section, a
// grouped rail, one Save per card, and row drawers instead of controls inside
// table cells. These drive the shipped functions rather than pinning their
// text, so a change that keeps the behaviour is free to change the source.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {shellSource} = require("./shell-source.js");
// The shipped app is a tree now, so the string these assertions slice is
// the app's body rows, joined in served order. See tests/web/shell-source.js.
const SHIPPED_UI = shellSource().bodyScript;
const DECLARATIONS = ["\nfunction ", "\nasync function "];
// A slice ends at the next top-level declaration of ANY kind, not only the
// next function. A row that declares `const X=…` between two functions used to
// be swallowed into the slice above it, so composing that same const beside the
// function — which this file does for `DEV_READINESS_LABEL` — declared it twice
// and the panel harness died on a SyntaxError instead of an assertion.
const TERMINATORS = DECLARATIONS.concat(["\nconst ", "\nlet ", "\nvar "]);

function shippedSource(name) {
  const start = DECLARATIONS.map((kind) =>
    SHIPPED_UI.indexOf(`${kind}${name}(`),
  ).find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = TERMINATORS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
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
    "libraries", "metadata", "livetv", "playback", "analysis",
    "maintenance", "users", "system", "cluster", "integrations", "developer",
  ]);
  const dispatch = {
    metadata: "metadataPanel(d.settings,d.developerReadiness)",
    playback: "playbackPanel(d.settings,d.developerReadiness)",
    livetv: "liveTvPanel(d.settings,d.developerReadiness)",
    analysis: "analysisSettingsPanel(d.settings,d.analysis)",
    maintenance: "maintenancePanel(d.settings,d.dvConversions,d.developerReadiness)",
    users: "usersPanel(d.users,d.settings)",
    system: "systemPanel(d.sys,d.playbackEvents)",
    cluster: "clusterPanel(d)",
    integrations: "integrationsPanel(d.settings,d.trakt)",
    developer: "developerPanel(d.settings,d.developerReadiness)",
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
  // Every routable section must also declare what data it needs. A section
  // that routes and dispatches but has no manifest entry renders
  // "Cannot read properties of undefined (reading 'required')" — the page is
  // reachable from the rail and broken on arrival, and every other assertion
  // in this file still passes. That shipped once.
  const manifest = shippedConst("SETTINGS_MANIFEST");
  for (const [id] of r.SET_TABS) {
    assert.match(manifest, new RegExp(`\\b${id}\\s*:\\s*\\{\\s*required:`),
      `${id} routes but declares no SETTINGS_MANIFEST entry`);
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
    dataset: {},
    classList: { add: (c) => card.classes.add(c), remove: (c) => card.classes.delete(c), contains: (c) => card.classes.has(c) },
    classes: new Set(["card", "setcard"]),
    querySelector: (sel) => (sel.includes("button") ? save : state),
  };
  card.closest = (sel) => (sel === ".setcard" ? card : null);
  const input = { closest: card.closest };
  const api = new Function(`${shippedSource("markSetCard")}\n${shippedSource("setCardSaved")}\nreturn {markSetCard,setCardSaved};`)();
  api.markSetCard(input);
  assert.equal(card.dataset.revision, "1");
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
    "api", "document", "cacheSettings", "toast", "setCardSaved", "SERVER", "SETTINGS", "verifiedDecodeCard", "decodeRecoveryCard",
    // The newline matters: a shipped function may be followed by a line
    // comment, and `shippedSource` returns everything up to the next
    // declaration. Without it the injected `return` lands inside that comment
    // and the composed source silently returns nothing.
    `${shippedSource(fn)}\nreturn ${fn};`,
  )(
    async (path, opts) => { writes[fn] = { path, body: opts.body }; return {}; },
    { getElementById: (id) => { assert.ok(ids.includes(id), `${fn} reads ${id}`); return { value: "v", checked: true, textContent: "" }; } },
    (v) => v, () => {}, () => {}, {}, {}, () => "", () => "",
  );
  const defaults = ["pal", "psl", "psm", "perr"];
  // Protocol and quality switching have separate cards. Streaming must not
  // write either field: a card that saves a field it does not show can turn
  // something back on that an operator deliberately turned off.
  const streaming = ["prr", "phr", "phb", "pha", "pvod", "pvws", "pvmb", "pvbg", "serr"];
  const autoQuality = ["pabr", "aqerr", "aqstate"];
  const liveRecovery = ["dvlr", "dvlrerr"];
  const developer = ["pcpv1", "dverr"];
  const prepared = ["pqh", "pqherr", "pqhstate"];
  // `vdcard` is read too: this handler replaces its own card rather than
  // re-rendering the panel, because the four cards beside it stage unsaved
  // edits. The handler's own catch would swallow a missing-id assertion, so
  // the id has to be listed here for the guard to mean anything.
  const verifiedDecode = ["dhqa", "dhqerr", "vdcard"];
  const automaticRecovery = ["adr", "adrerr", "drcard"];
  return Promise.all([
    run("savePlaybackDefaults", defaults)({ disabled: false }),
    run("saveStreaming", streaming)({ disabled: false }),
    run("saveAutoQuality", autoQuality)({ disabled: false }),
    run("saveLiveHlsRecovery", liveRecovery)({ disabled: false }),
    run("savePlaybackCompatibility", developer)({ disabled: false }),
    run("savePreparedQuality", prepared)({ disabled: false }),
    run("saveVerifiedDecode", verifiedDecode)({ disabled: false }),
    run("saveAutomaticDecoderRecovery", automaticRecovery)({ disabled: false }),
  ]).then(() => {
    assert.deepEqual(Object.keys(writes.savePlaybackDefaults.body).sort(), ["default_audio_lang", "default_sub_lang", "sub_mode"]);
    assert.deepEqual(Object.keys(writes.saveStreaming.body).sort(), [
      "hls_ahead_max_secs", "hls_burst_secs", "hls_readrate",
      "stream_readrate", "vod_block_budget_secs", "vod_blocked_get_cap",
      "vod_materialize_budget_secs", "vod_presentation",
      "vod_working_set_bytes",
    ]);
    assert.deepEqual(
      Object.keys(writes.saveLiveHlsRecovery.body),
      ["vod_live_recovery"],
    );
    assert.deepEqual(Object.keys(writes.savePlaybackCompatibility.body).sort(), ["playback_control_protocol_v1"]);
    assert.deepEqual(Object.keys(writes.savePreparedQuality.body), ["prepared_quality_handoff"]);
    assert.deepEqual(Object.keys(writes.saveAutoQuality.body), ["playback_auto_abr"]);
    assert.equal(writes.saveAutoQuality.path, "/settings");
    // Its own card, its own field. The verified-decode request renames cached
    // transcodes on covered paths, so it must never ride along with a save an
    // operator made for something else.
    assert.deepEqual(Object.keys(writes.saveVerifiedDecode.body).sort(), ["decoder_health_qualified_artifacts"]);
    assert.equal(writes.saveVerifiedDecode.path, "/settings");
    assert.deepEqual(Object.keys(writes.saveAutomaticDecoderRecovery.body).sort(), ["automatic_decoder_recovery"]);
    assert.equal(writes.saveAutomaticDecoderRecovery.path, "/settings");
    assert.equal(writes.savePlaybackDefaults.path, "/settings");
    assert.equal(writes.saveStreaming.path, "/settings");
    assert.equal(writes.savePlaybackCompatibility.path, "/settings");
  });
});

test("quality switching renders the server's saved value", async () => {
  const elements = {
    pqh: { checked: true },
    pqherr: { textContent: "" },
    pqhstate: { textContent: "Enabled" },
  };
  let body;
  const save = new Function(
    "api", "document", "cacheSettings", "toast", "setCardSaved",
    `${shippedSource("savePreparedQuality")}\nreturn savePreparedQuality;`,
  )(
    async (_, options) => { body = options.body; return { prepared_quality_handoff: false }; },
    { getElementById: (id) => elements[id] }, (value) => value, () => {}, () => {},
  );
  await save({ disabled: false });
  assert.deepEqual(body, { prepared_quality_handoff: true });
  assert.equal(elements.pqh.checked, false, "the checkbox reflects the returned setting");
  assert.equal(elements.pqhstate.textContent, "Disabled", "the badge agrees with the checkbox");
});

test("an older quality save never overwrites a newer draft", async () => {
  let finish;
  const response = new Promise((resolve) => { finish = resolve; });
  const elements = {
    pqh: { checked: true },
    pqherr: { textContent: "" },
    pqhstate: { textContent: "Unsaved changes" },
  };
  const card = { dataset: { revision: "1" }, isConnected: true };
  const button = { disabled: false, closest: () => card };
  const notices = [];
  let savedCalls = 0, cached;
  const save = new Function(
    "api", "document", "cacheSettings", "toast", "setCardSaved",
    `${shippedSource("savePreparedQuality")}\nreturn savePreparedQuality;`,
  )(
    async () => response,
    { getElementById: (id) => elements[id] },
    (value) => { cached = value; return value; },
    (message) => notices.push(message),
    () => { savedCalls += 1; },
  );

  const pending = save(button);
  elements.pqh.checked = false;
  card.dataset.revision = "2";
  button.disabled = false;
  finish({ prepared_quality_handoff: true });
  assert.equal(await pending, true);

  assert.deepEqual(cached, { prepared_quality_handoff: true }, "the returned server snapshot still refreshes the cache");
  assert.equal(elements.pqh.checked, false, "the later visible draft wins over the older response");
  assert.equal(elements.pqhstate.textContent, "Unsaved changes");
  assert.equal(button.disabled, false, "the newer draft remains saveable");
  assert.equal(savedCalls, 0, "the card is not falsely marked saved");
  assert.deepEqual(notices, ["Earlier quality change saved; newer edit remains unsaved"]);
});

// Twice already — #309, and again when `liveTvDeinterlaceCard` shipped — a new
// card reached `developerPanel` without being composed into the harness below,
// and the whole gate died on a bare `ReferenceError: <name> is not defined`
// thrown from inside an evaluated `new Function`: one opaque failure in place
// of every assertion this file makes about the Developer tab, with the name it
// wanted legible only from a stack trace through two layers of eval.
//
// The composed list stays explicit, because what the panel may compose is the
// thing being pinned. This only makes the third time say what to do about it.
// Deciding statically which of a panel's calls need composing is not reliable
// — a save handler named inside an `onclick` string is text for the DOM, not a
// call this harness makes — so the question is answered where it is exact:
// after the call actually failed.
function renderComposedPanel(panelName, render) {
  try {
    return render();
  } catch (error) {
    const missing = /^(?:\w+ )?(?:ReferenceError: )?([A-Za-z_][\w$]*) is not defined$/
      .exec(error && error.message);
    if (!missing || !shippedDeclares(missing[1])) throw error;
    throw new Error(
      `${panelName} calls ${missing[1]}, which index.html declares and this `
      + `harness does not compose; add shippedSource("${missing[1]}") to the `
      + "list beside the other cards",
      { cause: error },
    );
  }
}
function shippedDeclares(name) {
  return DECLARATIONS.some((kind) => SHIPPED_UI.includes(`${kind}${name}(`));
}

test("Developer keeps only experiments; everyday controls retain their saves and advisory readiness", () => {
  assert.doesNotMatch(
    shippedSource("playbackPanel"),
    /preparedQualityCard/,
    "the server-wide experimental enable must not remain in everyday Playback settings",
  );
  // Joined with newlines, never bare interpolation: `shippedSource` here
  // stops at the next `\nfunction `, so a fragment can end inside a trailing
  // `//` comment and swallow whatever follows it.
  const composedBody = [
      shippedSource("preparedHandoffEnabled"), shippedSource("liveTvSettingsCard"),
      shippedSource("verifiedDecodeCard"), shippedSource("decodeRecoveryCard"),
      // #309's sibling problem, twice over: a card or fragment `developerPanel`
      // calls has to be composed here or the panel throws on the name and this
      // whole gate reports one failure instead of checking anything.
      shippedSource("subtitleNotReadyCard"),
      shippedSource("subtitleStoredSourcesCard"),
      shippedSource("subtitleClusterSourcesCard"),
      shippedSource("subtitleBackfillCard"),
      shippedSource("chapterThumbnailsCard"),
      shippedSource("seekScratchReservationsCard"),
      shippedSource("liveTvGuideCard"), shippedSource("liveTvDeinterlaceCard"),
      shippedConst("DEV_READINESS_LABEL"), shippedConst("LIVE_TV_GUIDE_DRAFT"),
      shippedSource("devReadinessRow"), shippedSource("devReadinessPill"),
      shippedSource("devReadinessEvidence"), shippedSource("devReq"),
      shippedSource("devStaticReq"), shippedSource("clusterTransportRecoveryCard"),
      // The fourth time (see above): `clusterBackupCard` shipped with the
      // portable backup and fenced restore and reached `developerPanel`
      // without being composed here, so this whole gate died on its name.
      shippedSource("clusterBackupCard"),
      shippedSource("autoQualityCard"), shippedSource("preparedQualityCard"), shippedSource("dvrCard"),
      shippedSource("libraryChannelsSettingsCard"),
      shippedSource("playbackProtocolCard"), shippedSource("liveHlsRecoveryCard"),
      shippedSource("playbackPanel"), shippedSource("metadataPanel"),
      shippedSource("searchSettingsCard"), shippedSource("windowsServerCard"),
      shippedSource("maintenancePanel"), shippedSource("presetOpts"),
      "const SERVER=null, RETRY_EVERY=[], ART_EVERY=[], CLEAN_EVERY=[];",
      "const langOpts=()=>'',autoNextOn=()=>true,decodeLimitsSummary=()=>'',keyBackfillHtml=()=>'',togSelect=()=>'',precachePanel=()=>'',subtitleStorePanel=()=>'',dvDiskPanel=()=>'',telemetryPanel=()=>'';",
      // `directedChangeDeveloperRows` reads the live player and returns ""
      // when there is none, which is exactly the state a settings page is in.
      shippedSource("directedChangeDeveloperRows"),
      "const SETTINGS_DATA=null,ME={is_admin:true},PLAYER=null;",
      shippedSource("seekScratchReservationsCard"),
      shippedSource("developerPanel"),
      shippedSource("liveTvPanel"),
      "return {developerPanel,preparedQualityCard,clusterTransportRecoveryCard,liveTvPanel,dvrCard,playbackPanel,metadataPanel,maintenancePanel};",
    ].join("\n");
  const panels = new Function(
    "setHead", "setCard", "cardHead", "togRow", "setCardFoot", "esc", "window", "Hls",
    "currentCapsDocument",
    composedBody,
  )(
    (title, sub) => `HEAD:${title}|${sub}`,
    (body) => `CARD[${body}]`,
    (title, sub) => `CARDHEAD:${title}|${sub || ""}`,
    // Every argument, because the last two are the switch's state and the
    // handler that makes it do anything — a stub that drops them lets an inert
    // control pass as a working one.
    (id, label, note, checked, attrs) =>
      `TOG:${id}|${label}|${note}|checked=${!!checked}|${attrs || ""}`,
    (fn) => `FOOT:${fn}`,
    esc,
    { Hls: { DefaultConfig: { loader: function StockLoader() {} } } },
    { DefaultConfig: { loader: function StockLoader() {} } },
    () => ({ progressive_hevc_sample_entries: ["hvc1"], transports: ["progressive", "hls"] }),
  );
  const readiness = { items: [{
    id: "prepared_quality_handoff",
    requirements: [
      { id: "server_preparation_is_real", status: "met", evidence: "This build attaches a running worker before announcing it." },
      { id: "client_two_player_handoff", status: "unobservable", evidence: "This node cannot prove a physical first-frame qualification." },
      { id: "fleet_receipt", status: "unobservable", evidence: "The fleet receipt is not visible to this daemon." },
    ],
  }] };
  const settings = {
    playback_control_protocol_v1: true,
    prepared_quality_handoff: true,
    automatic_decoder_recovery: true,
    vod_live_recovery: true,
    subtitle_not_ready_503: false,
    live_tv_guide_source: "hdhomerun",
    live_tv_guide_hours: 24,
    dvr_enabled: false,
    dvr_root: "/srv/plurx/recordings",
    dvr_free_floor_gb: 50,
    dvr_tuner_reserve: 1,
    dvr_pad_start_s: 60,
    dvr_pad_end_s: 120,
    dvr_reminder_lead_s: 300,
    dvr_webhook_url: "",
  };
  const html = renderComposedPanel(
    "developerPanel", () => panels.developerPanel(settings, readiness),
  );
  for (const id of ["pabr", "pqh", "pdp", "dhqa", "adr", "sub503", "subsrc", "subcluster", "subbackfill", "chthumb"])
    assert.match(html, new RegExp(`TOG:${id}\\|`), `Developer retains ${id}`);
  // Absent from the settings document is on: chapter thumbnails default on.
  assert.match(html, /TOG:chthumb\|[^|]*\|[^|]*\|checked=true/);
  assert.match(html, /FOOT:saveChapterThumbnails/);
  assert.match(
    panels.developerPanel({ ...settings, chapter_thumbnails: false }, readiness),
    /TOG:chthumb\|[^|]*\|[^|]*\|checked=false/,
  );
  // Absent from the settings document is on: the store's switch defaults on.
  assert.match(html, /TOG:subsrc\|[^|]*\|[^|]*\|checked=true/);
  assert.match(html, /FOOT:saveSubtitleStoredSources/);
  assert.match(
    panels.developerPanel({ ...settings, subtitle_stored_sources: false }, readiness),
    /TOG:subsrc\|[^|]*\|[^|]*\|checked=false/,
  );
  // Neither the unseen schema report nor an unmet queue reading may remove
  // the switch or force a saved cluster/backfill choice off.
  const unmet = {items:[
    {id:"subtitle_cluster_sources",requirements:[{id:"analysis_queue",status:"unmet",evidence:"Queue is off."}]},
    {id:"subtitle_backfill",requirements:[{id:"backfill_lease",status:"unobservable",evidence:"No holder between passes."}]},
  ]};
  const optedIn = renderComposedPanel("developerPanel", () => panels.developerPanel(
    {...settings,subtitle_cluster_sources:true,subtitle_backfill:true}, unmet));
  assert.match(optedIn, /TOG:subcluster\|[^|]*\|[^|]*\|checked=true/);
  assert.match(optedIn, /TOG:subbackfill\|[^|]*\|[^|]*\|checked=true/);
  assert.match(optedIn, /Queue is off\./);
  assert.match(optedIn, /No holder between passes\./);
  for (const id of ["backfill_lease","backfill_enqueued","backfill_remaining","backfill_bytes"])
    assert.match(optedIn,new RegExp(`data-devstat="subtitle_backfill:${id}"`));
  assert.doesNotMatch(html, /HDHomeRun Live TV|CARDHEAD:Programme guide/);
  for (const route of ["livetv", "playback", "cluster"])
    assert.ok(html.includes(`href="#/settings/${route}"`), `${route} has a destination link`);
  assert.match(html, /FOOT:saveAutoQuality/);
  assert.match(html, /FOOT:savePreparedQuality/);
  assert.match(html, /Seek scratch accounting/);
  for (const id of ["pcpv1", "dvlr", "dvrenabled", "lcenabled", "lcsubjectenabled", "ca-enabled", "dvwin"])
    assert.ok(!html.includes(`TOG:${id}|`), `Developer no longer owns ${id}`);
  assert.doesNotMatch(html, /Playback surface contract|Web HLS startup recovery|HEVC sample-entry admission|Source probe compatibility|Search and classification|id="ui-enable"/);
  const playback = panels.playbackPanel(settings, readiness);
  assert.doesNotMatch(playback, /TOG:pabr\|/, "Auto quality belongs to Developer");
  for (const id of ["pcpv1", "dvlr"])
    assert.match(playback, new RegExp(`TOG:${id}\\|[^|]*\\|[^|]*\\|checked=true`));
  assert.match(playback, /FOOT:savePlaybackCompatibility/);
  assert.match(playback, /FOOT:saveLiveHlsRecovery/);
  assert.match(playback, /Advanced server delivery/);
  assert.match(panels.metadataPanel(settings, readiness), /onclick="showSearchSettings\(\)"/);
  assert.match(panels.maintenancePanel({...settings,dolby_vision_convert:true}, null, readiness), /TOG:dvwin\|[^|]*\|[^|]*\|checked=true/);
  assert.match(panels.maintenancePanel(settings, null, readiness), /FOOT:saveWindowsCompatibility/);
  assert.match(html, /This saved switch is authoritative; readiness is advisory and never overrides your choice/);
  // The switch has to be wired to something. A control that renders and does
  // nothing is worse than no control: it reports a capability to the operator
  // that the server never hears about.
  assert.match(html, /TOG:pdp\|[^|]*\|[^|]*\|checked=true\|onchange="setPreparedHandoff\(this\.checked\)"/,
    "the prepared-handoff switch reflects the stored state and sets it");
  assert.match(html, /Automatic decode recovery/);
  assert.match(html, /TOG:adr\|[^|]*\|[^|]*\|checked=true/);
  assert.match(html, /FOOT:saveAutomaticDecoderRecovery/);
  assert.match(html, /missing measurements or retained contracts never turn it back off/);
  assert.match(html, /reopen loop/);
  assert.match(html, /One recovery per playback, and it is never given back/);
  assert.match(html, /best-effort selected-stream diagnostics/);
  assert.match(html, /Chrome shaped-network recovery[\s\S]*?not met/);
  assert.match(html, /These observations never gate this checkbox/);
  const quality = panels.preparedQualityCard(settings, readiness);
  assert.match(quality, /TOG:pqh\|[^|]*\|[^|]*\|checked=true/);
  assert.match(quality, /FOOT:savePreparedQuality/);
  assert.match(quality, /Throughput measurement[\s\S]*?>supported<\/span>/);
  assert.match(quality, /not proof that a particular session/);
  assert.match(quality, /Missing qualification does not disable/);
  assert.doesNotMatch(quality, /TOG:pcpv1|TOG:pdp| disabled/);
  assert.equal((quality.match(/>not observable<\/span>/g) || []).length, 2);
  const off = panels.preparedQualityCard({...settings,
    playback_control_protocol_v1: false, prepared_quality_handoff: false}, readiness);
  assert.match(off, /TOG:pqh\|[^|]*\|[^|]*\|checked=false/);
  assert.match(off, /Control protocol<\/strong>[\s\S]*?<span class="pill warn">not met<\/span>/);
  const cluster = panels.clusterTransportRecoveryCard(readiness);
  assert.match(cluster, /No enable switch is required/);
  assert.match(cluster, /Keep a ready voter majority/);
  assert.match(cluster, /\/cluster\/transport\/sqlite/);
  assert.match(cluster, /twenty learner plus twenty voter recovery cycles/);
  assert.doesNotMatch(cluster, /TOG:/);
  // Recording: one authoritative switch, the engine's settings beside it, and
  // readiness that is advice. A red row must never reach the control.
  const dvr = panels.dvrCard(settings, readiness);
  assert.match(dvr, /TOG:dvrenabled\|[^|]*\|[^|]*\|checked=false\|/);
  assert.doesNotMatch(dvr, / disabled/, "no readiness result may disable the switch");
  assert.match(dvr, /FOOT:saveDvrSettings/);
  for (const id of ["dvrroot", "dvrfloor", "dvrreserve", "dvrpadstart", "dvrpadend", "dvrlead", "dvrwebhook"])
    assert.ok(dvr.includes(`id="${id}"`), `the recording card carries ${id}`);
  // Five rows without a webhook, six with one: the webhook row only exists
  // when there is a URL for it to have an opinion about.
  for (const id of ["dvr_root_writable", "dvr_free_space", "guide_horizon", "tuner_reserve", "every_node_mounts_root"])
    assert.ok(dvr.includes(`data-devstat="dvr:${id}"`), `the recording card reports ${id}`);
  assert.doesNotMatch(dvr, /data-devstat="dvr:webhook_url_approved"/);
  assert.match(panels.dvrCard({...settings, dvr_webhook_url: "https://example.invalid/hook"}, readiness),
    /data-devstat="dvr:webhook_url_approved"/);
  assert.match(dvr, /may turn recording on over any amount of red/);
  // The DVR tuple is its own transaction boundary. A save that carried a
  // Live TV field with it would be refused by the server with a 400.
  const save = shippedSource("saveDvrSettings");
  assert.doesNotMatch(save, /live_tv_/, "dvr_* settings are saved on their own");
  assert.match(save, /dvr_enabled:/);
  assert.match(save, /dvr_webhook_url:/);

  const live = panels.liveTvPanel(settings, readiness);
  assert.match(live, /TOG:dvrenabled\|/);
  assert.match(live, /TOG:lcenabled\|/);
  assert.match(live, /TOG:lcsubjectenabled\|/);
  assert.match(live, /FOOT:saveDvrSettings/);
  assert.match(live, /FOOT:saveLibraryChannelsSettings/);
  assert.match(live, /HDHomeRun Live TV/);
  assert.match(live, /Save the configuration, check readiness, then enable/);
  assert.match(live, /Readiness is advice, not a gate/);
  assert.match(live, /Programme guide/);
  assert.match(live, /Saved-configuration evidence/);
  assert.match(live, /api\.hdhomerun\.com/);
  assert.match(live, /never stores, logs or relays that credential/);
  assert.match(live, /FOOT:saveLiveTvGuide/);
});

test("unmet subtitle readiness cannot refuse the saved cluster or backfill switches", async () => {
  const writes=[];
  const nodes=new Map([
    ["subcluster",{checked:true}], ["subbackfill",{checked:true}],
    ["subclustererr",{textContent:""}], ["subbackfillerr",{textContent:""}],
    ["subclustercard",{outerHTML:""}], ["subbackfillcard",{outerHTML:""}],
  ]);
  const document={getElementById:id=>nodes.get(id)};
  const api=async (_path,request)=>{writes.push(request.body);return request.body;};
  const save=new Function("document","api",
    `const DEVELOPER_READINESS={items:[{id:"subtitle_cluster_sources",requirements:[{id:"analysis_queue",status:"unmet"}]}]};
     const cacheSettings=value=>value,toast=()=>{},setCardSaved=()=>{};
     const subtitleClusterSourcesCard=s=>\`cluster: \${s.subtitle_cluster_sources}\`;
     const subtitleBackfillCard=s=>\`backfill: \${s.subtitle_backfill}\`;
     ${shippedSource("saveSubtitleClusterSources")}
     ${shippedSource("saveSubtitleBackfill")}
     return {saveSubtitleClusterSources,saveSubtitleBackfill};`,
  )(document,api);
  await save.saveSubtitleClusterSources(null);
  await save.saveSubtitleBackfill(null);
  assert.deepEqual(writes,[{subtitle_cluster_sources:true},{subtitle_backfill:true}]);
  assert.equal(nodes.get("subclustercard").outerHTML,"cluster: true");
  assert.equal(nodes.get("subbackfillcard").outerHTML,"backfill: true");
  assert.equal(nodes.get("subclustererr").textContent,"");
  assert.equal(nodes.get("subbackfillerr").textContent,"");
});

test("server guidance sends disabled Live TV features to their current settings", () => {
  const sourceRoot = path.resolve(__dirname, "../../crates/plurxd/src");
  for (const file of ["http/dvr.rs", "http/library_channels.rs", "channel_subjects.rs", "http/live_tv.rs", "live_tv.rs"]) {
    const source = fs.readFileSync(path.join(sourceRoot, file), "utf8");
    assert.match(source, /Settings → Live TV/, `${file} names the current destination`);
    assert.doesNotMatch(source, /Settings → Developer|Developer settings|use Developer recovery|Run the Developer readiness check/,
      `${file} must not send an operator to the retired destination`);
  }
});

test("late readiness success and failure update evidence without repainting moved settings", () => {
  const updates=[];
  const patch = new Function("applyDeveloperReadiness", "esc", `
    ${shippedConst("SETTINGS_MANIFEST")}
    ${shippedSource("patchSettingsSecondary")}
    ${shippedSource("patchSettingsSecondaryError")}
    return {ok:patchSettingsSecondary,fail:patchSettingsSecondaryError};
  `)(value=>updates.push(value),esc);
  // No document or renderSettings stub: touching the surrounding panel would
  // throw instead of silently losing an edit when the diagnostics arrive.
  for(const tab of ["metadata","playback","livetv","maintenance","developer","cluster"]){
    const evidence={items:[]};
    patch.ok(tab,"developerReadiness",evidence);
    assert.equal(updates.at(-1),evidence);
    patch.fail(tab,"developerReadiness",new Error("offline"));
    assert.deepEqual(updates.at(-1),{unavailable:"offline"});
  }
  assert.equal(updates.length,12);
});

test("the guide's readiness rows are advisory and never disable the save", () => {
  const view = new Function(
    "esc",
    `${shippedSource("liveTvGuideReadyView")} return liveTvGuideReadyView;`,
  )(esc);
  const html = view({
    source: "xmltv",
    guide_hours: 24,
    freshness: "unavailable",
    age_seconds: 0,
    matched_channels: 0,
    lineup_channels: 12,
    programmes: 0,
    refresh_interval_seconds: 1200,
    refresh_error: "the XMLTV host refused the connection",
    checks: [
      { id: "live_tv_enabled", ready: false, message: "Live TV is off." },
      { id: "outbound_host", ready: true, message: "The owner can reach the URL." },
    ],
  });
  assert.match(html, /Not met yet/);
  assert.match(html, /Met/);
  assert.match(html, /Saved configuration checked:<\/b> XMLTV · 24 hour look-ahead/);
  assert.match(html, /None of this blocks the switch/);
  assert.match(html, /matched 0 of 12 lineup channels/);
  assert.match(html, /Last refresh failed/);
  assert.doesNotMatch(html, /disabled/, "an advisory panel must not render a disabled control");
});

test("changing the guide source dirties the replacement card after its repaint", () => {
  let rendered = false;
  const save = { disabled: true };
  const state = { textContent: "Saved" };
  const classes = new Set();
  const card = {
    dataset: { revision: "0" },
    classList: {
      contains: (name) => classes.has(name),
      add: (name) => classes.add(name),
    },
    querySelector: (selector) => selector.includes("button.primary") ? save : state,
  };
  const replacement = { closest: () => card, focus: () => {} };
  const oldUrl = { value: "https://guide.example/listings.xml" };
  const oldHours = { value: "48" };
  const oldCard = { isConnected: true, set outerHTML(value) { rendered = value === "replacement"; } };
  const document = {
    getElementById(id) {
      if (id === "ltgurl") return rendered ? null : oldUrl;
      if (id === "ltghours") return rendered ? null : oldHours;
      if (id === "ltgsrc") return rendered ? replacement : null;
      if (id === "live-tv-guide-settings") return oldCard;
      return null;
    },
  };
  const guide = new Function(
    "document", "liveTvGuideCard", "SETTINGS",
    `${shippedConst("LIVE_TV_GUIDE_DRAFT")}
     ${shippedSource("markSetCard")}
     ${shippedSource("liveTvGuideFieldDraft")}
     ${shippedSource("liveTvGuideSourceDraft")}
     return {change:liveTvGuideSourceDraft,draft:LIVE_TV_GUIDE_DRAFT};`,
  )(document, () => "replacement", {});

  guide.draft.checked = 42;
  guide.change("hdhomerun");

  assert.equal(guide.draft.source, "hdhomerun");
  assert.equal(guide.draft.url, oldUrl.value);
  assert.equal(guide.draft.hours, 48);
  assert.equal(guide.draft.revision, 1);
  assert.equal(guide.draft.checked, null, "the replacement readiness panel runs a current check");
  assert.ok(classes.has("dirty"), "the repainted card carries the unsaved state");
  assert.equal(save.disabled, false, "the repainted card's Save is enabled");
  assert.equal(state.textContent, "Unsaved changes");
});

test("saving the guide rechecks the saved source and retires its draft", async () => {
  let written;
  const fields = {
    ltgsrc: { value: "hdhomerun" },
    ltgurl: null,
    ltghours: { value: "24" },
  };
  const guide = new Function(
    "document", "liveTvSettingsWrite", "SETTINGS", "replaceLiveTvCard", "liveTvGuideCard",
    `${shippedConst("LIVE_TV_GUIDE_DRAFT")}
     ${shippedSource("liveTvGuideFieldDraft")}
     ${shippedSource("saveLiveTvGuide")}
     return {save:saveLiveTvGuide,draft:LIVE_TV_GUIDE_DRAFT};`,
  )(
    { getElementById: (id) => fields[id] },
    async (body) => { written = body; return true; },
    { live_tv_config_generation: 7, live_tv_xmltv_url: "" },
    () => {}, () => "",
  );
  Object.assign(guide.draft, { source: "hdhomerun", url: "old", hours: 48, checked: 42 });

  assert.equal(await guide.save({}), true);
  assert.deepEqual(written, {
    live_tv_config_generation: 7,
    live_tv_guide_source: "hdhomerun",
    live_tv_xmltv_url: "",
    live_tv_guide_hours: 24,
  });
  assert.deepEqual(guide.draft, { source: null, url: null, hours: null, checked: null, revision: 1 });
});

test("Live TV mutations repaint only the card that owns the saved fields", () => {
  assert.doesNotMatch(shippedSource("liveTvGuideSourceDraft"), /renderSettings\(/,
    "changing guide source cannot erase a dirty tuner card");
  assert.doesNotMatch(shippedSource("liveTvSettingsWrite"), /renderSettings\(/,
    "a successful write cannot erase a sibling card's draft");
  assert.match(shippedSource("saveLiveTvGuide"),
    /replaceLiveTvCard\("live-tv-guide-settings",liveTvGuideCard\(saved\),"ltgsrc"\)/);
  assert.match(shippedSource("saveLiveTvSettings"),
    /replaceLiveTvCard\("live-tv-settings",liveTvSettingsCard\(saved\),"ltenable"\)/);
  assert.match(shippedSource("setLiveTvEnabled"),
    /replaceLiveTvCard\("live-tv-settings",liveTvSettingsCard\(saved\),"ltenable"\)/);
});

test("every asynchronous Live TV settings response is fenced to the Live TV route", () => {
  for (const name of ["checkLiveTvGuide", "refreshLiveTvGuide", "liveTvSettingsWrite", "checkLiveTvReadiness"]) {
    const source = shippedSource(name);
    assert.match(source, /settingsCurrent\(generation,"livetv"\)/, `${name} follows the new route`);
    assert.doesNotMatch(source, /settingsCurrent\(generation,"developer"\)/,
      `${name} cannot repaint Developer after navigation`);
  }
});

test("an off-route Live TV write refreshes the shared settings cache without repainting", async () => {
  const result = { live_tv_config_generation: 9, live_tv_guide_source: "xmltv" };
  let cached, toasted = false;
  const write = new Function(
    "ME", "PAGE_RENDER_GENERATION", "api", "settingsCurrent", "cacheSettings", "toast", "document", "AbortSignal",
    `${shippedSource("liveTvSettingsWrite")}\nreturn liveTvSettingsWrite;`,
  )(
    { is_admin: true }, 4, async () => result, () => false,
    (value) => { cached = value; return value; }, () => { toasted = true; },
    { getElementById: () => null }, AbortSignal,
  );

  assert.equal(await write({ live_tv_guide_source: "xmltv" }, null, "ltgerr"), result);
  assert.equal(cached, result, "a later Settings section must reuse the new generation, not the pre-save cache");
  assert.equal(toasted, false, "an off-route response cannot paint even a toast onto the destination section");
});

test("a rejected guide save reports into the replacement card", async () => {
  let rejectWrite;
  const response = new Promise((_, reject) => { rejectWrite = reject; });
  const oldError = { textContent: "", isConnected: true };
  const currentError = { textContent: "", isConnected: true };
  let errorNode = oldError;
  const button = { disabled: false, isConnected: false };
  const write = new Function(
    "ME", "PAGE_RENDER_GENERATION", "api", "settingsCurrent", "cacheSettings", "toast", "document", "AbortSignal",
    `${shippedSource("liveTvSettingsWrite")}\nreturn liveTvSettingsWrite;`,
  )(
    { is_admin: true }, 8, async () => response, () => true,
    (value) => value, () => {}, { getElementById: () => errorNode }, AbortSignal,
  );

  const pending = write({ live_tv_guide_source: "xmltv" }, button, "ltgerr");
  oldError.isConnected = false;
  errorNode = currentError;
  rejectWrite(new Error("saved source was refused"));
  assert.equal(await pending, false);
  assert.equal(oldError.textContent, "", "the detached source card is not the error destination");
  assert.equal(currentError.textContent, "saved source was refused");
});

test("a rejected guide refresh reports into the replacement card", async () => {
  let rejectRefresh;
  const response = new Promise((_, reject) => { rejectRefresh = reject; });
  const oldError = { textContent: "", isConnected: true };
  const currentError = { textContent: "", isConnected: true };
  let errorNode = oldError;
  const button = { disabled: false, isConnected: false };
  const refresh = new Function(
    "PAGE_RENDER_GENERATION", "api", "settingsCurrent", "checkLiveTvGuide", "document", "AbortSignal",
    `${shippedSource("refreshLiveTvGuide")}\nreturn refreshLiveTvGuide;`,
  )(
    11, async () => response, () => true, async () => {},
    { getElementById: () => errorNode }, AbortSignal,
  );

  const pending = refresh(button);
  oldError.isConnected = false;
  errorNode = currentError;
  rejectRefresh(new Error("guide host timed out"));
  await pending;
  assert.equal(oldError.textContent, "", "the detached source card is not the error destination");
  assert.equal(currentError.textContent, "guide host timed out");
});

test("Verified decode states its cost, its prerequisites, and what this node measured", () => {
  const card = new Function(
    "setCard", "cardHead", "togRow", "setCardFoot", "esc",
    `${shippedSource("verifiedDecodeCard")}\nreturn verifiedDecodeCard;`,
  )(
    (body) => `CARD[${body}]`,
    (title, sub, tools) => `CARDHEAD:${title}|${sub || ""}|${tools || ""}`,
    (id, label, note) => `TOG:${id}|${label}|${note}`,
    (fn) => `FOOT:${fn}`,
    esc,
  );

  // A node with nothing measured: every check fails, and each one says what
  // it means rather than only that it failed.
  const bare = card({ decoder_health_qualified_artifacts: false, decoder_health_qualification: {
    namespace: "decoder-plan-v1-unqualified", enforcing: false, eligible: false,
    measured_build: null, measured_decoders: [], covered_decoders: [],
    refusal: "not_requested", explanation: "Not requested on this node.",
  } });
  assert.match(bare, /TOG:dhqa\|/);
  assert.match(bare, /FOOT:saveVerifiedDecode/);
  // No covered path pays a rename yet, but the control remains enabled: the
  // prerequisites are advice, not a disabled switch.
  assert.match(bare, /No measured path can produce a verified artifact today/);
  assert.match(bare, /Readiness checks are advisory and never disable this control/);
  assert.match(bare, /drop every frame of a file and still exit successfully/);
  assert.doesNotMatch(bare, /This node can honour the request/);
  assert.match(bare, /evidence of a clean decode before reusing transcodes/);
  assert.match(bare, /<details class="setdetails"><summary>Cache impact and diagnostic evidence/);
  assert.equal((bare.match(/✗/g) || []).length, 3, "three unmet checks, each shown");
  assert.match(bare, /Not requested on this node\./);

  // Requested and enabled, with the uncovered state still shown as advice.
  const refused = card({ decoder_health_qualified_artifacts: true, decoder_health_qualification: {
    namespace: "decoder-plan-v1-unqualified", enforcing: false, policy_enabled: true, eligible: false,
    measured_build: "ffmpeg version 5.1.9", measured_decoders: ["h264/h264", "hevc/hevc"],
    covered_decoders: [], measured_paths_v2: ["h264/software/h264", "hevc/software/hevc"],
    covered_paths_v2: [], refusal: "no_contract_covers_this_build",
    explanation: "No retained diagnostic contract covers this node's FFmpeg build under the qualified log flags.",
  } });
  assert.match(refused, /Enabled · covered paths/);
  assert.match(refused, /Coverage advisory/);
  assert.match(refused, /No retained diagnostic contract covers/);
  assert.match(refused, /<code>h264\/software\/h264 · hevc\/software\/hevc<\/code>/, "what it did measure is still shown in one host-independent container");
  assert.equal((refused.match(/✓/g) || []).length, 2);
  assert.equal((refused.match(/✗/g) || []).length, 1);

  // In force.
  const on = card({ decoder_health_qualified_artifacts: true, decoder_health_qualification: {
    namespace: "decoder-plan-v1-health-qualified-r1", enforcing: true, policy_enabled: true, eligible: true,
    measured_build: "ffmpeg version 9.0.1", measured_decoders: ["h264/h264"],
    covered_decoders: ["h264/h264"], measured_paths_v2: ["h264/software/h264"],
    covered_paths_v2: ["h264/software/h264"], refusal: null, explanation: null,
  } });
  assert.match(on, /Enabled · covered paths/);
  assert.equal((on.match(/✗/g) || []).length, 0);
  assert.doesNotMatch(on, /Not in force/);
  // An eligible node is the one that actually pays, so it is the one told.
  assert.match(on, /This node has covered decode paths/);
  assert.match(on, /renames cached transcodes that use those paths/);
  assert.match(on, /paths pay the same rename a second time/);
  assert.doesNotMatch(on, /every transcode/);

  // Saved and not yet applied is a third state, and it is neither of the
  // other two: reporting the request as the state would claim a change that
  // has not happened, and reporting only the published answer would hide one
  // an operator just made.
  const pending = card({ decoder_health_qualified_artifacts: true, decoder_health_qualification: {
    namespace: "decoder-plan-v1-unqualified", enforcing: false, policy_enabled: false, eligible: true,
    measured_build: "ffmpeg version 9.0.1", measured_decoders: ["h264/h264"],
    covered_decoders: ["h264/h264"], measured_paths_v2: ["h264/software/h264"],
    covered_paths_v2: ["h264/software/h264"], refusal: "not_requested",
    explanation: "Not requested on this node.", pending_restart: true,
  } });
  assert.match(pending, /Saved · restart to apply/);
  assert.match(pending, /applies the request when it next starts/);
  assert.match(pending, /move an affected key space under work already in flight/);
  assert.doesNotMatch(pending, /every cache key/);

  // A current response prefers the backend-aware sibling, while an older
  // node with only the pair list remains readable.
  const legacy = card({ decoder_health_qualified_artifacts: false, decoder_health_qualification: {
    namespace: "decoder-plan-v1-unqualified", enforcing: false, eligible: true,
    measured_build: "ffmpeg version 8.0", measured_decoders: ["h264/h264"],
    covered_decoders: ["h264/h264"], refusal: "not_requested", explanation: null,
  } });
  assert.match(legacy, /<code>h264\/h264<\/code>/);
  assert.match(legacy, /older nodes report software-only/);

  // FFmpeg's version banner and decoder names are somebody else's strings.
  const hostile = card({ decoder_health_qualified_artifacts: false, decoder_health_qualification: {
    namespace: "decoder-plan-v1-unqualified", enforcing: false, eligible: false,
    measured_build: "<img src=x onerror=alert(1)>", measured_decoders: ["<script>/x"],
    covered_decoders: [], refusal: "not_requested", explanation: "<b>x</b>",
  } });
  assert.doesNotMatch(hostile, /<img src=x/);
  assert.doesNotMatch(hostile, /<script>/);
  assert.match(hostile, /&lt;img src=x/);

  // A node that has never answered still renders. The settings payload is the
  // only source for this card, and a page that throws on a missing field is a
  // Settings section that cannot be opened at all.
  assert.doesNotThrow(() => card({}));
});

test("Automatic recovery is directly enabled and coverage remains advisory", () => {
  const card = new Function(
    "setCard", "cardHead", "togRow", "setCardFoot", "esc",
    `${shippedSource("decodeRecoveryCard")}\nreturn decodeRecoveryCard;`,
  )(
    (body) => `CARD[${body}]`,
    (title, sub, tools) => `CARDHEAD:${title}|${sub || ""}|${tools || ""}`,
    (id, label, note, checked) => `TOG:${id}|${label}|${note}|checked=${!!checked}`,
    (fn) => `FOOT:${fn}`,
    esc,
  );

  const crossed = card({ automatic_decoder_recovery: false, decoder_health_qualification: {
    measured_build: "ffmpeg version 9.0.1",
    covered_decoders: ["h264/h264"],
    covered_paths_v2: ["h264/software/h264", "hevc/videotoolbox/hevc"],
  } });
  assert.match(crossed, /✗ The same codec has covered hardware and software paths/);
  assert.match(crossed, /TOG:adr\|[^|]*\|[^|]*\|checked=false/);
  assert.match(crossed, /disabled/);
  assert.match(crossed, /does not block the switch/);

  const paired = card({ automatic_decoder_recovery: true, decoder_health_qualification: {
    measured_build: "ffmpeg version 9.0.1",
    covered_decoders: ["h264/h264"],
    covered_paths_v2: ["h264/software/h264", "h264/videotoolbox/h264"],
  } });
  assert.match(paired, /✓ The same codec has covered hardware and software paths/);
  assert.match(paired, /h264\/videotoolbox\/h264/);
  assert.match(paired, /TOG:adr\|[^|]*\|[^|]*\|checked=true/);
  assert.match(paired, /enabled/);
  assert.match(paired, /FOOT:saveAutomaticDecoderRecovery/);

  const off = card({ automatic_decoder_recovery: false, decoder_health_qualification: {
    measured_build: "ffmpeg version 9.0.1",
    covered_paths_v2: ["h264/software/h264", "h264/videotoolbox/h264"],
  } });
  assert.match(off, /✓ The same codec has covered hardware and software paths/);
  assert.match(off, /checked=false/);

  const legacy = card({ automatic_decoder_recovery: true, decoder_health_qualification: {
    enforcing: true,
    measured_build: "ffmpeg version 8.0",
    covered_decoders: ["h264/h264"],
  } });
  assert.match(legacy, /✗ The same codec has covered hardware and software paths/);
  assert.doesNotMatch(legacy, /✓ The same codec has covered hardware and software paths/);
  assert.match(legacy, /checked=true/);
});

// Fix C PR 3: the subtitle-source store's footprint and its producer's work,
// on the card where background work says what it costs and where it stops.
test("Maintenance shows the stored subtitle tracks: size, what is riding now, and where to turn it off", () => {
  const render = new Function(
    "esc",
    `const setCard=(html,o)=>"CARD["+o.id+"]"+html;
     const cardHead=(t,d,s)=>"HEAD:"+t+"|"+s+"|";
     const fmtBytes=(n)=>n?n+" B":"";
     const fmtDur=(ms)=>ms?Math.round(ms/60000)+"m":"";
     ${shippedSource("subtitleStorePanel")}
     return subtitleStorePanel;`,
  )(esc);
  const html = render({
    subtitle_stored_sources: true,
    vod_index_mins: 15,
    subtitle_store: {
      footprint: { bytes: 18866, directories: 3, measured_at_ms: 1 },
      footprint_age_ms: 5 * 60000,
      cap_bytes: 34359738368,
      riding: [
        { file_id: 5208, item_id: 77, title: "Bad <Boys>", tracks: 2, bytes_written: 4096, started_at_ms: 1, running_ms: 3 * 60000 },
        { file_id: 5209, item_id: 0, title: "", tracks: 1, bytes_written: 0, started_at_ms: 1, running_ms: 0 },
      ],
      tracks_attempted: 5, kept: 3, empty: 1, malformed: 1, transient: 0,
      bytes_written: 9000, manifests_published: 2, files_not_riding: 1, discarded_switch_off: 2,
      gate: { open: true, reason: null },
    },
  });
  assert.match(html, /CARD\[subsrcstore\]/);
  assert.match(html, /HEAD:Stored subtitle tracks\|<span class="pill ok">18866 B · 3 files<\/span>/, "an open gate: the size in a good pill");
  assert.match(html, /On this node the store holds <b>18866 B · 3 files<\/b> \(measured 5m ago\)/, "the size, on this node, and when it was measured");
  assert.match(html, /<b><a href="#\/item\/77">Bad &lt;Boys&gt;<\/a><\/b>: keeping 2 PGS tracks, 4096 B written so far · running 3m/,
    "a running ride by its escaped, linked title and how long it has run");
  assert.match(html, /<b>File 5209<\/b>: keeping 1 PGS track, 0 B written so far · running just now/, "an untitled ride falls back to its file");
  assert.match(html, /5 tracks attempted — 3 kept, 1 with no cues, 1 malformed, 0 to retry/);
  assert.match(html, /1 file indexed without it after a riding pass failed/);
  assert.match(html, /2 riding passes finished after it was turned off and kept nothing/);
  assert.match(html, /href="#\/settings\/developer\/enable-subtitle-sources"/, "the link lands on the switch");
  assert.match(html, /a pass already running finishes its index but publishes none of the tracks it kept/, "what off does, exactly");
  assert.doesNotMatch(html, /setwarn/);

  // Switch on with a readiness concern: the card explains it without
  // treating the observation as a feature gate.
  const blocked = render({
    subtitle_stored_sources: true,
    subtitle_store: { riding: [], gate: { open: false, reason: "the startup self-test failed: ffprobe <7.1> and ffmpeg 8.0 differ" } },
  });
  assert.match(blocked, /HEAD:Stored subtitle tracks\|<span class="pill warn">readiness concern<\/span>/);
  assert.match(blocked, /class="setwarn">⚠ <b>Review stored-track readiness:<\/b> the startup self-test failed: ffprobe &lt;7\.1&gt; and ffmpeg 8\.0 differ\./);

  const idle = render({ subtitle_stored_sources: false, vod_index_mins: 15, subtitle_store: { riding: [], gate: { open: false, reason: "subtitles.stored_sources is off" } } });
  assert.match(idle, /HEAD:Stored subtitle tracks\|<span class="pill">off<\/span>/);
  assert.doesNotMatch(idle, /setwarn/, "off is a choice, not a fault");
  assert.match(idle, /No index pass on this node is keeping PGS tracks right now/);
  assert.match(idle, /not measured yet: the next background analysis pass's sweep measures it/);
  const paused = render({ subtitle_stored_sources: true, vod_index_mins: 0, subtitle_store: { riding: [] } });
  assert.match(paused, /not measured: background analysis is paused/, "true even when the sweep never runs");
});

test("a section route may name the element it lands on", () => {
  const r = new Function(
    "location", "history", "document",
    `${shippedConst("SET_GROUPS")}${shippedConst("SET_TABS")}
     ${shippedSource("settingsRouteTab")}
     ${shippedSource("settingsRouteAnchor")}
     ${shippedSource("revealSettingsAnchor")}
     return {settingsRouteTab,settingsRouteAnchor,revealSettingsAnchor};`,
  );
  const location = { hash: "#/settings/developer/enable-subtitle-sources" };
  const replaced = [];
  const history = { replaceState: (_s, _t, url) => { replaced.push(url); location.hash = url; } };
  let scrolled = 0;
  const document = { getElementById: (id) => (id === "enable-subtitle-sources" ? { scrollIntoView: () => { scrolled += 1; } } : null) };
  const api = r(location, history, document);
  assert.equal(api.settingsRouteTab(location.hash), "developer", "the section still routes");
  assert.equal(api.settingsRouteAnchor(location.hash), "enable-subtitle-sources");
  assert.equal(api.settingsRouteAnchor("#/settings/developer"), null);
  assert.equal(api.settingsRouteTab("#/settings/nothere/enable-subtitle-sources"), null);
  api.revealSettingsAnchor("developer");
  assert.equal(scrolled, 1, "scrolled to the named card");
  assert.deepEqual(replaced, ["#/settings/developer"], "the address drops the anchor");
  api.revealSettingsAnchor("developer");
  assert.equal(scrolled, 1, "once: a repaint does not scroll the reader back");
  assert.match(shippedSource("renderSettings"), /settingsPanel\(tab,d\)\}<\/div><\/div>`;\s*revealSettingsAnchor\(tab\);/);
  assert.match(shippedSource("subtitleStoredSourcesCard") + shippedSource("developerPanel"), /id="enable-subtitle-sources"/, "the anchor exists on Developer");
});

test("Maintenance owns the timers, and each of its cards saves its own fields", () => {
  const panel = shippedSource("maintenancePanel");
  for (const card of ["precachePanel", "subtitleStorePanel", "dvDiskPanel", "telemetryPanel"]) assert.match(panel, new RegExp(`${card}\\(`));
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
  assert.match(SHIPPED_UI, /if\(tab==="metadata"\)\s*return metadataPanel\(d\.settings,d\.developerReadiness\)/);
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
