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
const TEST_FILTER = process.env.SETTINGS_TEST_FILTER;

test("media support warning remains prominent for a working encoder with missing required Metal", () => {
  const render = new Function("esc", `${shippedSource("ffmpegSupportWarning")}\nreturn ffmpegSupportWarning;`)(esc);
  const html=render({encoders:{videotoolbox:true},ffmpeg_support:{executable:'/operator/ffmpeg<wrong>',package_problems:[],rows:[
    {label:"Metal HDR tone mapping",required:true,included:false},
    {label:"NVENC",required:false,included:false}
  ]}});
  assert.match(html,/role="alert"/);
  assert.match(html,/FFmpeg lacks required support/);
  assert.match(html,/Metal HDR tone mapping/);
  assert.doesNotMatch(html,/NVENC/);
  assert.match(html,/ffmpeg&lt;wrong&gt;/);
  assert.equal(render({ffmpeg_support:{executable:"ffmpeg",package_problems:[],rows:[{required:true,included:true}]}}),"");
});

test("media support matrix separates build inclusion from untested execution and unrelated hardware", () => {
  const render = new Function("esc", `${shippedSource("ffmpegSupportCard")}\nreturn ffmpegSupportCard;`)(esc);
  const html=render({ffmpeg_support:{executable:"ffmpeg",checked_at:1,package_verified:true,rows:[
    {id:"metal",label:"Metal",applicable:true,required:true,included:true,runtime:"not_tested",missing:[]},
    {id:"nvenc",label:"NVENC",applicable:false,required:false,included:false,runtime:"not_applicable",missing:["h264_nvenc"]}
  ]}});
  assert.match(html,/✓ Included/);
  assert.match(html,/Not tested/);
  assert.match(html,/Other hardware · not required on this node/);
  assert.doesNotMatch(html,/>Passed</);
  assert.doesNotMatch(html,/h264_nvenc/);
});

test("media support warns when a required included device fails execution", () => {
  const render = new Function("esc", `${shippedSource("ffmpegSupportWarning")}\nreturn ffmpegSupportWarning;`)(esc);
  const html=render({ffmpeg_support:{executable:"ffmpeg",package_problems:[],rows:[
    {label:"VideoToolbox",required:true,included:true,runtime:"failed"}
  ]}});
  assert.match(html,/Required media hardware check failed/);
  assert.match(html,/Execution failed for VideoToolbox/);
  assert.doesNotMatch(html,/Install the complete/);
});

function renderTranscoder(sys){
  const helpers=["systemCapabilityHtml","systemGraphicsHtml","systemToneMapDetails","transcoderCard"].map(shippedSource).join("\n");
  return new Function("esc",`${helpers}\nreturn transcoderCard;`)(esc)(sys);
}
test("node transcoder selector hides unsupported backends and explains the unavailable saved choice", () => {
  const html = renderTranscoder({node_id:'rog"node', hwaccel_pref:"nvenc", hwaccel_requested:"qsv", encoder_selected:"NVIDIA NVENC", encoders:{nvenc:true,qsv:false}});
  assert.doesNotMatch(html, /value="qsv"/);
  assert.match(html, /saved backend is unavailable/);
  assert.match(html, /value="auto" selected/);
  assert.match(html, /waiting for this node to restart/);
  assert.match(html, /data-node-id="rog&quot;node"/);
  assert.doesNotMatch(html, / disabled/);
});

test("transcoder preference keeps available hardware independent of synthetic speed results", () => {
  const html = renderTranscoder({node_id:"node",hwaccel_pref:"auto",encoders:{nvenc:true,videotoolbox:true},transcoder_optimization:{report:{results:[
    {backend:"software",fps:480,relative_to_cpu:1},
    {backend:"videotoolbox",fps:120,relative_to_cpu:.25}
  ]}}});
  assert.match(html, /value="auto" selected>Auto/);
  assert.match(html, /value="software">CPU \(software\)/);
  assert.match(html, /value="videotoolbox">Apple VideoToolbox/);
  assert.match(html, /value="nvenc">NVIDIA · NVENC/);
  assert.doesNotMatch(html, /fps|fastest|benchmark on startup/);
});

test("transcoder polling discards a response from before leaving and reentering System", async () => {
  let generation=1, calls=0, resolve;
  const original={node_id:"node",transcoder_optimization:{running:true}};
  const data={sys:original};
  const poll=new Function("settingsCurrent","api","SETTINGS_DATA","document","transcoderCard",`${shippedSource("pollTranscoderOptimization")}\nreturn pollTranscoderOptimization;`)(
    expected=>generation===expected,
    ()=>{calls++;return new Promise(done=>{resolve=done;});},data,
    {getElementById:()=>{throw Error("stale response touched the page");}},()=>"");
  const pending=poll("node",1);
  generation=3; // leave System, then return to a new System render
  resolve({node_id:"node",transcoder_optimization:{running:false}});
  await pending;
  assert.equal(data.sys,original);
  await poll("node",1);
  assert.equal(calls,1,"obsolete timer generations must not fetch");
  assert.doesNotMatch(shippedSource("pollTranscoderOptimization"),/setTimeout|setInterval/);
});

test("Mac processing card preserves enabled choice with unavailable compatibility and names graduation evidence", () => {
  const render=new Function("setCard","cardHead","togRow","devReq","devGraduation","setCardFoot",
    `${shippedSource("macosVideoProcessingCard")}\nreturn macosVideoProcessingCard;`)(
      value=>value, title=>title, (id,label,note,on)=>`TOG:${id}:${on}`,
      ()=>"unavailable", (waiting,destination)=>`${waiting} ${destination}`, name=>`SAVE:${name}`);
  const html=render({macos_video_processing_enabled:true},{unavailable:"not observed"});
  assert.match(html,/TOG:pmacosvideo:true/);
  assert.match(html,/SAVE:saveMacosVideoProcessing/);
  assert.match(html,/visual checks on a named display/);
  assert.match(html,/encoded VOD pass seek\/resume/);
  assert.match(html,/Playback → Advanced server delivery/);
  assert.doesNotMatch(html,/ disabled(?:[=>\s]|$)/);
});

test("Mac processing save writes only the operator choice despite unavailable readiness", async () => {
  const card={outerHTML:""}, button={disabled:false,closest:()=>card}, calls=[];
  const save=new Function("document","api","cacheSettings","macosVideoProcessingCard","DEVELOPER_READINESS","toast",
    `${shippedSource("saveMacosVideoProcessing")}\nreturn saveMacosVideoProcessing;`)(
      {getElementById:id=>id==="pmacosvideo"?{checked:true}:{textContent:""}},
      async(path,options)=>{calls.push({path,options});return {macos_video_processing_enabled:true};},
      value=>value, settings=>`SAVED:${settings.macos_video_processing_enabled}`,
      {unavailable:"runtime unknown"},()=>{});
  await save(button);
  assert.deepEqual(calls,[{path:"/settings",options:{method:"PUT",body:{macos_video_processing_enabled:true}}}]);
  assert.equal(card.outerHTML,"SAVED:true");
});

test("Mac HEVC output remains enabled with unavailable compatibility and has independent graduation", () => {
  const render=new Function("setCard","cardHead","togRow","devReq","devGraduation","setCardFoot",
    `${shippedSource("macosHevcOutputCard")}\nreturn macosHevcOutputCard;`)(
      value=>value, title=>title, (id,label,note,on)=>`TOG:${id}:${on}`,
      ()=>"unavailable", (waiting,destination)=>`${waiting} ${destination}`, name=>`SAVE:${name}`);
  const html=render({macos_hevc_output_enabled:true,macos_video_processing_enabled:false},{unavailable:"not observed"});
  assert.match(html,/TOG:pmacoshevc:true/);
  assert.match(html,/SAVE:saveMacosHevcOutput/);
  assert.match(html,/named HDR display/);
  assert.match(html,/permanent HEVC output switch moves to Playback/);
  assert.doesNotMatch(html,/ disabled(?:[=>\s]|$)/);
});

test("Mac HEVC output save accepts the operator choice without changing processing", async () => {
  const card={outerHTML:""}, button={disabled:false,closest:()=>card}, calls=[];
  const save=new Function("document","api","cacheSettings","macosHevcOutputCard","DEVELOPER_READINESS","toast",
    `${shippedSource("saveMacosHevcOutput")}\nreturn saveMacosHevcOutput;`)(
      {getElementById:id=>id==="pmacoshevc"?{checked:true}:{textContent:""}},
      async(path,options)=>{calls.push({path,options});return {macos_hevc_output_enabled:true,macos_video_processing_enabled:false};},
      value=>value, settings=>`SAVED:${settings.macos_hevc_output_enabled}:${settings.macos_video_processing_enabled}`,
      {unavailable:"runtime unknown"},()=>{});
  await save(button);
  assert.deepEqual(calls,[{path:"/settings",options:{method:"PUT",body:{macos_hevc_output_enabled:true}}}]);
  assert.equal(card.outerHTML,"SAVED:true:false");
});

test("Mac compatibility reprobe never sends or changes the saved switch", async () => {
  const calls=[], button={disabled:false};
  const reprobe=new Function("document","api","applyDeveloperReadiness","toast",
    `${shippedSource("reprobeMacosVideoProcessing")}\nreturn reprobeMacosVideoProcessing;`)(
      {getElementById:()=>({textContent:""})},async(path,options)=>{calls.push([path,options]);return {};},()=>{},()=>{});
  await reprobe(button);
  assert.deepEqual(calls,[["/developer/macos-video-processing/reprobe",{method:"POST",body:{}}],["/developer/readiness",undefined]]);
  assert.equal(button.disabled,false);
});

test("node transcoder save binds the request to the displayed node and preserves the active backend", async () => {
  const status = {textContent:""}, button = {disabled:false};
  const sys = {node_id:"rog",hwaccel_pref:"nvenc"};
  const calls=[];
  const save = new Function("document","api","SETTINGS_DATA","toast", `${shippedSource("saveNodeHwaccel")}\nreturn saveNodeHwaccel;`)(
    {getElementById:id=>id==="node-hwaccel"?{value:"qsv",dataset:{nodeId:"rog"}}:status},
    // The value api() receives, not a parse of it: api() serializes `body`
    // itself. This fake once JSON.parse'd it, which made the test demand the
    // double-encoded string that broke Save and Optimize (2026-10-11).
    async (url,options)=>{calls.push([url,options.body]);return {restart_required:true};},
    {sys},()=>{});
  await save(button);
  assert.deepEqual(calls, [["/system/transcoder",{node_id:"rog",preference:"qsv"}]]);
  assert.equal(sys.hwaccel_pref,"nvenc");
  assert.equal(sys.hwaccel_requested,"qsv");
  assert.match(status.textContent,/Restart this node/);
  assert.equal(button.disabled,false);
});
async function main() {
  // Test registrations continue below this call site; let the script finish
  // loading before deciding which names to run.
  await Promise.resolve();
  const filters = TEST_FILTER ? TEST_FILTER.split("|") : [];
  let matched = 0;
  for (const { name, run } of QUEUE) {
    if (TEST_FILTER && !filters.some((part) => name.includes(part))) continue;
    matched += 1;
    started += 1;
    try { await run(); process.stdout.write(`PASS ${name}\n`); }
    catch (error) { failures += 1; process.stderr.write(`FAIL ${name}\n${error && error.stack}\n`); }
    finished += 1;
  }
  if (TEST_FILTER && matched === 0) {
    process.stderr.write(`No settings tests matched SETTINGS_TEST_FILTER=${TEST_FILTER}\n`);
    failures += 1;
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

test("legacy subtitle setup links route to Maintenance", () => {
  const r=routing(memoryStorage(),"#/settings/developer/enable-subtitle-transcription");
  assert.equal(r.settingsRouteTab(r.location.hash),"maintenance");
  assert.equal(r.settingsTab(),"maintenance");
  assert.match(shippedConst("SETTINGS_KEPT_MOUNTS"),/maintenance:\["enable-subtitle-transcription"\]/);
});

test("subtitle setup belongs to Maintenance without a temporary Developer card", () => {
  const panels=developerPanels(), settings={}, readiness={features:[]};
  assert.doesNotMatch(panels.developerPanel(settings,readiness),/Configure transcription|Generate subtitles offline/);
  const maintenance=panels.maintenancePanel(settings,null,readiness);
  assert.match(maintenance,/Generate subtitles offline/);
  assert.match(maintenance,/Configure transcription/);
  assert.match(maintenance,/id="subtitle-transcription-form"/);
  const card=shippedSource("subtitleTranscriptionCard");
  assert.match(card,/id:"enable-subtitle-transcription"/);
  assert.doesNotMatch(card,/devGraduation|Leaves Developer when/);
});

test("English translation setup graduates to Playback and preserves legacy links", () => {
  for(const route of ["#/settings/playback/enable-subtitle-translation","#/settings/developer/enable-subtitle-translation"]){
    const r=routing(memoryStorage(),route);
    assert.equal(r.settingsRouteTab(route),"playback");
    assert.equal(r.settingsTab(),"playback");
  }
  const panels=developerPanels(),settings={},readiness={features:[]};
  assert.doesNotMatch(panels.developerPanel(settings,readiness),/Configure English translation|Set up subtitle text translation/);
  const html=panels.playbackPanel(settings,readiness);
  assert.match(html,/Set up speech captions/);
  assert.match(html,/Set up subtitle text translation/);
  assert.match(html,/id="subtitle-translation-form"/);
  assert.match(html,/selected audio track and the current playback timeline/);
  assert.match(html,/Shared viewers can read committed AI captions/);
  assert.match(html,/Original captions and Off remain available/);
  assert.doesNotMatch(html,/Leaves Developer when|pill warn">experimental/);
  const card=shippedSource("subtitleTranslationCard");
  assert.match(card,/Accuracy needs checking/);
  assert.match(card,/Generate captions from speech, translate speech to English, or translate an existing text subtitle/);
  assert.doesNotMatch(card,/devGraduation|Leaves Developer when/);
  assert.match(shippedSource("translationOpen"),/#\/settings\/playback\/enable-subtitle-translation/);
});

test("every section is a route, grouped in the rail's order", () => {
  const r = routing(memoryStorage(), "#/settings/playback");
  assert.deepEqual(r.SET_GROUPS.map(([group]) => group), ["Content", "Playback", "Server", "Outside", "Developer"]);
  assert.deepEqual(r.SET_TABS.map(([id]) => id), [
    "libraries", "metadata", "livetv", "playback", "analysis",
    "maintenance", "users", "system", "cluster", "integrations", "sharing", "developer",
  ]);
  const dispatch = {
    metadata: "metadataPanel(d.settings,d.developerReadiness)",
    playback: "playbackPanel(d.settings,d.developerReadiness)",
    livetv: "liveTvPanel(d.settings,d.developerReadiness)",
    analysis: "analysisSettingsPanel(d.settings,d.analysis,d.developerReadiness)",
    maintenance: "maintenancePanel(d.settings,d.dvConversions,d.developerReadiness)",
    users: "usersPanel(d.users,d.settings)",
    system: "systemPanel(d.sys)",
    cluster: "clusterPanel(d)",
    sharing: "sharingManagementPanel(d)",
    integrations: "integrationsPanel(d.settings,d.trakt)",
    developer: "developerPanel(d.settings,d.developerReadiness,httpsSettingsMount(d.https))",
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
    "SETTINGS_DATA", "esc", "PlurxClusterPanel", "SERVER",
    `${shippedConst("SET_GROUPS")}${shippedConst("SET_TABS")}
     ${shippedSource("settingsClusterEnabled")}
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
  assert.match(sqlite, /Cluster<span class="setn">off<\/span>/);
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
    "api", "document", "cacheSettings", "toast", "setCardSaved", "SERVER", "SETTINGS", "verifiedDecodeCard", "decodeRecoveryCard", "pgsOverlayCard", "DEVELOPER_READINESS", "sdrMasterCodecsCard", "networkPriorsCard",
    // The newline matters: a shipped function may be followed by a line
    // comment, and `shippedSource` returns everything up to the next
    // declaration. Without it the injected `return` lands inside that comment
    // and the composed source silently returns nothing.
    `${shippedSource(fn)}\nreturn ${fn};`,
  )(
    async (path, opts) => { writes[fn] = { path, body: opts.body }; return {}; },
    { getElementById: (id) => { assert.ok(ids.includes(id), `${fn} reads ${id}`); return { value: "v", checked: true, textContent: "" }; } },
    (v) => v, () => {}, () => {}, {}, {}, () => "", () => "", () => "", null, () => "",
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
  // re-rendering the panel, because the other Playback cards (Streaming and
  // the rest of Advanced server delivery) stage unsaved edits. The handler's own catch would swallow a missing-id assertion, so
  // the id has to be listed here for the guard to mean anything.
  const verifiedDecode = ["dhqa", "dhqerr", "vdcard"];
  const automaticRecovery = ["adr", "adrerr", "drcard"];
  const pgsOverlay = ["pgsoverlay", "pgsoverlayerr", "pgsoverlaycard"];
  // D6: the priors switch had no control at all; it gets its own card and
  // writes only its own field.
  const networkPriors = ["network-priors", "network-priors-error", "network-priors-card"];
  return Promise.all([
    run("savePlaybackDefaults", defaults)({ disabled: false }),
    run("saveStreaming", streaming)({ disabled: false }),
    run("saveAutoQuality", autoQuality)({ disabled: false }),
    run("saveDisplayAwareAuto", ["pdisplayauto", "daqerr", "daqstate"])({ disabled: false }),
    run("saveLiveHlsRecovery", liveRecovery)({ disabled: false }),
    run("savePlaybackCompatibility", developer)({ disabled: false }),
    run("savePreparedQuality", prepared)({ disabled: false }),
    run("saveVerifiedDecode", verifiedDecode)({ disabled: false }),
    run("saveAutomaticDecoderRecovery", automaticRecovery)({ disabled: false }),
    run("savePgsOverlay", pgsOverlay)({ disabled: false }),
    // S-10's SDR master CODECS switch: its own card, its own field.
    run("saveSdrMasterCodecs", ["sdr-master-codecs", "sdr-codecs-error", "sdr-codecs-card"])({ disabled: false }),
    run("saveNetworkPriors", networkPriors)({ disabled: false }),
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
    assert.deepEqual(writes.saveDisplayAwareAuto.body, {playback_display_aware_auto:true});
    assert.equal(writes.saveDisplayAwareAuto.path,"/settings");
    // Its own card, its own field. The verified-decode request renames cached
    // transcodes on covered paths, so it must never ride along with a save an
    // operator made for something else.
    assert.deepEqual(Object.keys(writes.saveVerifiedDecode.body).sort(), ["decoder_health_qualified_artifacts"]);
    assert.equal(writes.saveVerifiedDecode.path, "/settings");
    assert.deepEqual(Object.keys(writes.saveAutomaticDecoderRecovery.body).sort(), ["automatic_decoder_recovery"]);
    assert.equal(writes.saveAutomaticDecoderRecovery.path, "/settings");
    assert.deepEqual(writes.savePgsOverlay.body, { pgs_overlay: true });
    assert.deepEqual(writes.saveSdrMasterCodecs.body, { playback_sdr_master_codecs: true });
    assert.equal(writes.saveSdrMasterCodecs.path, "/settings");
    assert.equal(writes.savePgsOverlay.path, "/settings");
    assert.deepEqual(writes.saveNetworkPriors.body, { playback_network_priors: true });
    assert.equal(writes.saveNetworkPriors.path, "/settings");
    assert.equal(writes.savePlaybackDefaults.path, "/settings");
    assert.equal(writes.saveStreaming.path, "/settings");
    assert.equal(writes.savePlaybackCompatibility.path, "/settings");
  });
});

test("PGS overlay saves either choice despite unmet readiness and reports a failed save", async () => {
  const nodes = {
    pgsoverlay: { checked: false },
    pgsoverlayerr: { textContent: "" },
    pgsoverlaycard: { outerHTML: "original" },
  };
  const readiness = { items: [{ id: "pgs_overlay", requirements: [
    { id: "clients_render_overlays", status: "unmet" },
    { id: "overlay_acceptance", status: "unobservable" },
  ] }] };
  const writes = [], cached = [], notices = [];
  let reject = false;
  const save = new Function(
    "document", "api", "cacheSettings", "toast", "setCardSaved", "pgsOverlayCard", "DEVELOPER_READINESS",
    `${shippedSource("savePgsOverlay")}\nreturn savePgsOverlay;`,
  )(
    { getElementById: (id) => { assert.ok(id in nodes); return nodes[id]; } },
    async (path, request) => {
      assert.equal(path, "/settings");
      assert.equal(request.method, "PUT");
      writes.push(request.body);
      if (reject) throw new Error("Setting write refused");
      return { pgs_overlay: request.body.pgs_overlay };
    },
    (value) => { cached.push(value); return value; },
    (message) => notices.push(message),
    () => {},
    (value, evidence) => { assert.equal(evidence, readiness); return `saved:${value.pgs_overlay}`; },
    readiness,
  );
  for (const enabled of [true, false]) {
    nodes.pgsoverlay.checked = enabled;
    await save({ disabled: false });
    assert.deepEqual(writes.at(-1), { pgs_overlay: enabled });
    assert.deepEqual(cached.at(-1), { pgs_overlay: enabled });
    assert.equal(nodes.pgsoverlaycard.outerHTML, `saved:${enabled}`);
    assert.equal(nodes.pgsoverlayerr.textContent, "");
  }
  reject = true;
  nodes.pgsoverlay.checked = true;
  const button = { disabled: false };
  await save(button);
  assert.equal(nodes.pgsoverlayerr.textContent, "Setting write refused");
  assert.equal(button.disabled, false, "a failed save can be retried");
  assert.equal(nodes.pgsoverlaycard.outerHTML, "saved:false", "failure cannot repaint a saved value");
  assert.equal(cached.length, 2, "failure cannot update the settings cache");
  assert.equal(notices.length, 2, "failure cannot report success");
});

test("display Auto saves either choice with pending readiness and retains newer edits", async () => {
  const nodes={pdisplayauto:{checked:false},daqerr:{textContent:""},daqstate:{textContent:""}};
  const card={dataset:{revision:"1"},isConnected:true};
  const writes=[];let resolveWrite;
  const server={};
  const save=new Function("document","api","cacheSettings","toast","setCardSaved","SERVER",
    `${shippedSource("saveDisplayAwareAuto")}\nreturn saveDisplayAwareAuto;`)(
      {getElementById:id=>{assert.ok(id in nodes);return nodes[id];}},
      async(path,request)=>{writes.push(request.body);return await new Promise(resolve=>{resolveWrite=resolve;});},
      value=>value,()=>{},()=>{},server);
  for(const enabled of [true,false]){
    nodes.pdisplayauto.checked=enabled;
    const pending=save({disabled:false,closest:()=>card});
    resolveWrite({playback_display_aware_auto:enabled});await pending;
    assert.deepEqual(writes.at(-1),{playback_display_aware_auto:enabled});
    assert.equal(server.playback_display_aware_auto,enabled);
    assert.equal(nodes.daqstate.textContent,enabled?"Enabled":"Disabled");
  }
  nodes.pdisplayauto.checked=true;
  const pending=save({disabled:false,closest:()=>card});
  card.dataset.revision="2";nodes.pdisplayauto.checked=false;
  resolveWrite({playback_display_aware_auto:true});await pending;
  assert.equal(server.playback_display_aware_auto,true,"accepted saved value updates shared state");
  assert.equal(nodes.pdisplayauto.checked,false,"late response preserves newer unsaved draft");
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

test("Durable retry preserves its UUID after a transport failure and sends an object body", async () => {
  const writes=[],queue={retries:new Map(),epoch:0};
  const retry=new Function("DURABLE_ACTIVITY","crypto","api","toast","refreshDurableActivity",
    `${shippedSource("retryDurableJob")}\nreturn retryDurableJob;`)(queue,{randomUUID:()=>"retry-uuid"},
    async(path,options)=>{writes.push({path,...options});if(writes.length===1)throw new Error("response lost");return {outcome:"existing"};},
    ()=>{},async()=>{});
  const button={disabled:false};
  await retry("job",button);assert.equal(button.disabled,false);
  await retry("job",button);
  assert.deepEqual(writes.map(write=>write.body),[{request_id:"retry-uuid"},{request_id:"retry-uuid"}]);
  assert.equal(queue.retries.size,0);
});

function developerPanels(){
  const composedBody = [
      shippedSource("preparedHandoffEnabled"), shippedSource("liveTvSettingsCard"),
      shippedSource("liveTvEnableCard"), shippedSource("jellyfinCompatibilityCard"),
      "const document={getElementById:()=>null};",
      shippedSource("verifiedDecodeCard"), shippedSource("decodeRecoveryCard"), shippedSource("hevcCopyCard"),
      // #309's sibling problem, twice over: a card or fragment `developerPanel`
      // calls has to be composed here or the panel throws on the name and this
      // whole gate reports one failure instead of checking anything.
      shippedSource("contentEncodingCard"), shippedSource("vodReorderCard"),
      shippedSource("dolbyVisionHdrProcessingCard"), shippedSource("dolbyVisionFelReencodeCard"),
      shippedSource("displayTargetHdr10Card"),
      shippedSource("sdrMasterCodecsCard"),
      // The main-merge defects build (2026-10-04): complete-output
      // preparation and rolling retention arrived with their Developer cards.
      shippedSource("outputPreparationCard"), shippedSource("rollingRetentionCard"),
      shippedSource("subtitleTranscriptionCard"),
      shippedSource("subtitleTranslationCard"),
      shippedSource("clusterClockCard"),
      shippedSource("pgsOverlayCard"),
      shippedSource("subtitleStoredSourcesCard"),
      shippedSource("subtitleClusterSourcesCard"),
      shippedSource("subtitleBackfillCard"),
      shippedSource("chapterThumbnailsCard"),
      shippedSource("liveTvGuideCard"), shippedSource("liveTvDeinterlaceCard"),
      shippedConst("DEV_READINESS_LABEL"), shippedConst("LIVE_TV_GUIDE_DRAFT"),
      shippedSource("devReadinessRow"), shippedSource("devReadinessPill"),
      shippedSource("devReadinessEvidence"), shippedSource("devReq"),
      shippedSource("devStaticReq"), shippedSource("devGraduation"),
      shippedSource("clusterTransportRecoveryCard"),
      // The fourth time (see above): `clusterBackupCard` shipped with the
      // portable backup and fenced restore and reached `developerPanel`
      // without being composed here, so this whole gate died on its name.
      shippedSource("clusterBackupCard"),
      shippedSource("clusterPlacementCard"), shippedSource("boundedCatalogueCard"), shippedSource("cinemaSharingCard"), shippedSource("cinemaRemoteCard"), shippedSource("cinemaRemoteInvitationsCard"),
      shippedSource("diagnosticDeveloperCard"),
      shippedSource("autoQualityCard"), shippedSource("displayAwareAutoCard"), shippedSource("preparedQualityCard"), shippedSource("dvrCard"),
      // 2026-10-10: the graduated cards are drawn by their own sections.
      shippedSource("preparedHandoffBrowserCard"), shippedSource("clusterSettingsSection"),
      shippedSource("analysisSettingsPanel"),
      // D6 (2026-10-04): the network priors switch sits beside display Auto.
      shippedSource("networkPriorsCard"),
      shippedSource("macosVideoProcessingCard"), shippedSource("macosHevcOutputCard"),
      shippedSource("libraryChannelsSettingsCard"),
      shippedSource("playbackProtocolCard"), shippedSource("liveHlsRecoveryCard"),
      shippedSource("rateControlCard"),
      shippedSource("playbackPanel"), shippedSource("metadataPanel"),
      shippedSource("searchSettingsCard"), shippedSource("windowsServerCard"),
      shippedSource("maintenancePanel"), shippedSource("presetOpts"),
      "const SERVER={cluster_enabled:true}, RETRY_EVERY=[], ART_EVERY=[], CLEAN_EVERY=[];",
      shippedSource("settingsClusterEnabled"),
      "const hdrDisplayPeakCard=()=>'',openInCard=()=>'',langOpts=()=>'',autoNextOn=()=>true,decodeLimitsSummary=()=>'',keyBackfillHtml=()=>'',togSelect=()=>'',precachePanel=()=>'',subtitleStorePanel=()=>'',dvDiskPanel=()=>'',telemetryPanel=()=>'',analysisSummaryCard=()=>'';",
      // `directedChangeRows` reads the live player and returns ""
      // when there is none, which is exactly the state a settings page is in.
      shippedSource("directedChangeRows"),
      "const SETTINGS_DATA=null,ME={is_admin:true},PLAYER=null;",
      shippedSource("developerPanel"),
      shippedSource("liveTvPanel"),
      "return {server:SERVER,developerPanel,preparedQualityCard,clusterTransportRecoveryCard,liveTvPanel,dvrCard,playbackPanel,metadataPanel,maintenancePanel,analysisSettingsPanel,clusterSettingsSection};",
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
    { location:{href:"https://plurx.example/"}, Hls: { DefaultConfig: { loader: function StockLoader() {} } } },
    { DefaultConfig: { loader: function StockLoader() {} } },
    () => ({ progressive_hevc_sample_entries: ["hvc1"], transports: ["progressive", "hls"] }),
  );
  return panels;
}

test("Developer keeps only experiments; everyday controls retain their saves and advisory readiness", () => {
  // Joined with newlines, never bare interpolation: `shippedSource` here
  // stops at the next `\nfunction `, so a fragment can end inside a trailing
  // `//` comment and swallow whatever follows it.
  const panels = developerPanels();
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
    pgs_overlay: false,
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
  const httpsCard = '<div id="settings-https">Node-local HTTPS</div>';
  const html = renderComposedPanel(
    "developerPanel", () => panels.developerPanel(settings, readiness, httpsCard),
  );
  assert.ok(html.indexOf("HEAD:Developer") < html.indexOf(httpsCard), "the page heading precedes HTTPS");
  assert.ok(html.indexOf("</nav>") < html.indexOf(httpsCard), "everyday destinations precede HTTPS");
  assert.ok(html.indexOf(httpsCard) < html.indexOf("CARDHEAD:Diagnostic ZIP reports"), "HTTPS stays inside the Developer card list");
  for (const id of ["hevc-unverified", "pabr"])
    assert.match(html, new RegExp(`TOG:${id}\\|`), `Developer retains ${id}`);
  // Graduated 2026-10-10 (Paul: "clean up developer settings"). Each switch
  // left with its Save, its saved value and its advisory rows; Developer no
  // longer draws any of them, and none of the automatic no-switch cards.
  for (const id of ["lt-enable", "dev-live-tv-enable", "ltdeint", "pqh", "pdp", "pgsoverlay", "subsrc", "subcluster", "subbackfill",
    "cluster-placement-enabled", "cluster-takeover-enabled", "bounded-catalogue-reads", "cluster-clock-enforced"])
    assert.ok(!html.includes(`TOG:${id}|`), `Developer no longer owns ${id}`);
  assert.doesNotMatch(html, /Seek scratch accounting|Parallel playback subtitle ranges|Portable cluster backup|backup-destination|Live TV deinterlace cadence|CARDHEAD:Enable Live TV|Quality switching|Second player in this browser/);
  assert.doesNotMatch(shippedSource("developerPanel"),
    /liveTvEnableCard|liveTvDeinterlaceCard|clusterPlacementCard|boundedCatalogueCard|clusterClockCard|clusterBackupCard|preparedQualityCard|pgsOverlayCard|subtitle(StoredSources|ClusterSources|Backfill|PlaybackRanges)Card|seekScratchReservationsCard/,
    "no graduated card is drawn twice");
  for (const gone of ["seekScratchReservationsCard", "subtitlePlaybackRangesCard"])
    assert.equal(shippedDeclares(gone), false, `${gone} had no switch and was removed outright`);
  // The clock guard is an operator switch, off by default, whose readiness
  // rows are advisory: no hard gate in code. It lives in Cluster now.
  const clusterHtml = panels.clusterSettingsSection({ settings, developerReadiness: readiness });
  const clocks = /Cluster clock guard[\s\S]*?(?=CARD\[|$)/.exec(clusterHtml);
  assert.ok(clocks, "Cluster shows the clock guard switch");
  assert.match(clocks[0], /TOG:cluster-clock-enforced\|[^|]*\|[^|]*\|checked=false/, "enforcement is off by default");
  assert.match(clocks[0], /FOOT:saveClusterClockGuard/);
  assert.match(clocks[0], /advisory and never prevent saving/);
  assert.doesNotMatch(clusterHtml, /Leaves Developer when/, "a graduated card no longer waits on anything");
  assert.match(
    panels.clusterSettingsSection({ settings: { ...settings, cluster_clock_guard_enforced: true }, developerReadiness: readiness }),
    /TOG:cluster-clock-enforced\|[^|]*\|[^|]*\|checked=true/,
  );
  for (const [id, save] of [["cluster-placement-enabled", "saveClusterPlacement"], ["cluster-takeover-enabled", "saveClusterPlacement"],
    ["bounded-catalogue-reads", "saveBoundedCatalogueReads"], ["subcluster", "saveSubtitleClusterSources"]]) {
    assert.match(clusterHtml, new RegExp(`TOG:${id}\\|`), `Cluster owns ${id}`);
    assert.match(clusterHtml, new RegExp(`FOOT:${save}`));
  }
  assert.match(clusterHtml, /id="backup-destination"/);
  assert.match(clusterHtml, /FOOT:saveClusterBackup/);
  assert.match(panels.clusterSettingsSection({}), /id="cluster-settings"[\s\S]*Loading cluster settings/,
    "the board never waits for /settings: the cards fill their mount when it lands");
  // Complete-output preparation and rolling retention: both off by default,
  // both advisory, both graduate (main-merge defects build, 2026-10-04).
  const preparation = /Complete-output preparation[\s\S]*?(?=Rolling output retention)/.exec(html);
  assert.ok(preparation, "Developer shows the complete-output preparation card");
  assert.match(preparation[0], /<option value="off" selected>/, "preparation is off by default");
  assert.match(preparation[0], /FOOT:saveOutputPreparation/);
  assert.match(preparation[0], /advisory and never prevent saving/);
  assert.match(preparation[0], /Leaves Developer when/);
  assert.match(
    panels.developerPanel({ ...settings, vod_output_preparation: "copy_and_encoded" }, readiness),
    /<option value="copy_and_encoded" selected>/,
  );
  const retention = /Rolling output retention[\s\S]*?(?=<div class="setsection")/.exec(html);
  assert.ok(retention, "Developer shows the rolling retention card");
  assert.match(retention[0], /TOG:vod-rolling-retention\|[^|]*\|[^|]*\|checked=false/, "retention is off by default");
  assert.match(retention[0], /FOOT:saveRollingRetention/);
  assert.match(retention[0], /Leaves Developer when/);
  assert.match(
    panels.developerPanel({ ...settings, vod_rolling_retention: true }, readiness),
    /TOG:vod-rolling-retention\|[^|]*\|[^|]*\|checked=true/,
  );
  // D6: Fit Auto to display moves up only with network priors on. The card
  // names that prerequisite and the two limits beside it, and the priors
  // switch is a Developer card of its own. Red rows never move either switch.
  const displayAuto = /CARDHEAD:Fit Auto to display\|[\s\S]*?(?=CARDHEAD:Network priors)/.exec(html);
  assert.ok(displayAuto, "Developer shows Fit Auto to display followed by Network priors");
  for (const id of ["auto_abr", "network_priors", "local_session_owner", "ipv4_client"])
    assert.match(displayAuto[0], new RegExp(`data-devstat="display_aware_auto:${id}"`), `display Auto reports ${id}`);
  assert.match(displayAuto[0], /TOG:pdisplayauto\|/);
  assert.match(displayAuto[0], /FOOT:saveDisplayAwareAuto/);
  const priors = /CARDHEAD:Network priors\|[\s\S]*?(?=<div class="setsection")/.exec(html);
  assert.ok(priors, "Developer shows the network priors card");
  assert.match(priors[0], /TOG:network-priors\|[^|]*\|[^|]*\|checked=false/, "priors are off by default");
  assert.match(priors[0], /per user, client and IPv4 \/24 network/);
  assert.match(priors[0], /starting \(cold-start\) rung/);
  assert.match(priors[0], /FOOT:saveNetworkPriors/);
  assert.match(priors[0], /Leaves Developer when/);
  const priorsOff = { items: [
    { id: "display_aware_auto", requirements: [
      { id: "network_priors", status: "unmet", evidence: "Off: Auto never upgrades and a link stall retries the same quality; producer and decoder recovery still work" },
    ] },
    { id: "network_priors", requirements: [] },
  ] };
  const savedOn = renderComposedPanel("developerPanel", () => panels.developerPanel(
    { ...settings, playback_display_aware_auto: true, playback_network_priors: false }, priorsOff));
  assert.match(savedOn, /TOG:pdisplayauto\|[^|]*\|[^|]*\|checked=true/, "unmet priors never turn display Auto off");
  assert.match(savedOn, /Auto never upgrades and a link stall retries the same quality/);
  assert.match(
    panels.developerPanel({ ...settings, playback_network_priors: true }, priorsOff),
    /TOG:network-priors\|[^|]*\|[^|]*\|checked=true/,
  );
  const unverified = panels.developerPanel({...settings, hevc_unverified_copy:true,
    hevc_header_trace_available:false, vod_index_cluster_cache:false, vod_index_mins:0}, readiness);
  assert.match(unverified, /TOG:hevc-unverified\|[^|]*\|[^|]*\|checked=true\|/);
  assert.match(unverified, /FOOT:saveHevcCopy/);
  assert.match(unverified, /not configured/);
  // S-10: SDR master CODECS is an operator switch, off by default, whose one
  // readiness row (the Apple device re-qualification) is advisory.
  const sdrCodecs = /CODECS on SDR master playlists[\s\S]*?(?=<div class="setsection"|$)/.exec(html);
  assert.ok(sdrCodecs, "Developer shows the SDR master CODECS switch");
  assert.match(sdrCodecs[0], /TOG:sdr-master-codecs\|[^|]*\|[^|]*\|checked=false/, "off by default");
  assert.match(sdrCodecs[0], /FOOT:saveSdrMasterCodecs/);
  assert.match(sdrCodecs[0], /data-devstat="sdr_master_codecs:sdr_codecs_device_requalification"/);
  assert.match(sdrCodecs[0], /never refused/);
  assert.doesNotMatch(sdrCodecs[0], / disabled/, "no readiness result may disable the switch");
  assert.match(
    panels.developerPanel({ ...settings, playback_sdr_master_codecs: true }, readiness),
    /TOG:sdr-master-codecs\|[^|]*\|[^|]*\|checked=true/,
  );
  const liveTv = panels.liveTvPanel(settings, readiness);
  assert.match(liveTv, /TOG:lt-enable\|/);
  assert.match(liveTv, /FOOT:saveLiveTvEnable/);
  assert.match(liveTv, /Readiness observations never disable the control/);
  assert.match(liveTv, /TOG:lt-enable\|[\s\S]*HDHomeRun Live TV[\s\S]*id="ltdeint"[\s\S]*Programme guide/, "the switch first, then tuner, cadence and guide");
  assert.match(liveTv, /FOOT:saveLiveTvDeinterlace/);
  assert.doesNotMatch(liveTv, /Leaves Developer when|settings\/developer/);
  // Graduated 2026-09-28 at Paul's word: chapter thumbnails and both decoder
  // controls are permanent Playback settings now, each with its own Save.
  // Absent from the settings document is on: chapter thumbnails default on.
  const graduatedPlayback = panels.playbackPanel(settings, readiness);
  assert.match(graduatedPlayback, /TOG:chthumb\|[^|]*\|[^|]*\|checked=true/);
  assert.match(graduatedPlayback, /FOOT:saveChapterThumbnails/);
  assert.match(
    panels.playbackPanel({ ...settings, chapter_thumbnails: false }, readiness),
    /TOG:chthumb\|[^|]*\|[^|]*\|checked=false/,
  );
  // The decoder controls sit inside Advanced server delivery: they change how
  // the server transcodes, not what a viewer picks.
  const advanced = graduatedPlayback.slice(graduatedPlayback.indexOf("<summary>Advanced server delivery</summary>"));
  for (const id of ["dhqa", "adr"])
    assert.match(advanced, new RegExp(`TOG:${id}\\|`), `Advanced server delivery owns ${id}`);
  assert.doesNotMatch(graduatedPlayback.slice(0, graduatedPlayback.indexOf("<summary>Advanced server delivery</summary>")), /TOG:(dhqa|adr)\|/);
  for (const id of ["chthumb", "dhqa", "adr"])
    assert.ok(!html.includes(`TOG:${id}|`), `Developer no longer owns ${id}`);
  assert.doesNotMatch(html, /Decoder experiments|Chapter thumbnails|Verified decode artifacts|Automatic decode recovery/);
  assert.match(graduatedPlayback, /TOG:pgsoverlay\|[^|]*\|[^|]*\|checked=false/);
  assert.match(graduatedPlayback, /FOOT:savePgsOverlay/);
  assert.match(panels.playbackPanel({ ...settings, pgs_overlay: true }, readiness),
    /TOG:pgsoverlay\|[^|]*\|[^|]*\|checked=true/);
  assert.match(graduatedPlayback, /TOG:pqh\|[^|]*\|[^|]*\|checked=true/);
  assert.match(graduatedPlayback, /FOOT:savePreparedQuality/);
  const subtitleCaptionsCard=/CARD\[CARDHEAD:AI captions[\s\S]*?(?=CARD\[CARDHEAD:|<details class="setdetails"|$)/.exec(graduatedPlayback);
  assert.ok(subtitleCaptionsCard,"Playback keeps the unfinished subtitle operations together in their card");
  assert.match(shippedSource("subtitleTranslationCard"),/Accuracy needs checking/,"the authored card keeps its accuracy qualification warning");
  assert.doesNotMatch(graduatedPlayback, /Leaves Developer when|pill warn">experimental/);
  // Absent from the settings document is on: the store's switch defaults on.
  // It sits in Maintenance beside the store it controls.
  const maintenance = panels.maintenancePanel(settings, null, readiness);
  assert.match(maintenance, /TOG:subsrc\|[^|]*\|[^|]*\|checked=true/);
  assert.match(maintenance, /FOOT:saveSubtitleStoredSources/);
  assert.match(
    panels.maintenancePanel({ ...settings, subtitle_stored_sources: false }, null, readiness),
    /TOG:subsrc\|[^|]*\|[^|]*\|checked=false/,
  );
  // Neither the unseen schema report nor an unmet queue reading may remove
  // the switch or force a saved cluster/backfill choice off.
  const unmet = {items:[
    {id:"subtitle_cluster_sources",requirements:[{id:"analysis_queue",status:"unmet",evidence:"Queue is off."}]},
    {id:"subtitle_backfill",requirements:[{id:"backfill_lease",status:"unobservable",evidence:"No holder between passes."}]},
  ]};
  const optedInSettings = {...settings,subtitle_cluster_sources:true,subtitle_backfill:true};
  const optedIn = panels.clusterSettingsSection({ settings: optedInSettings, developerReadiness: unmet })
    + panels.analysisSettingsPanel(optedInSettings, null, unmet);
  assert.match(optedIn, /TOG:subcluster\|[^|]*\|[^|]*\|checked=true/);
  assert.match(optedIn, /TOG:subbackfill\|[^|]*\|[^|]*\|checked=true/);
  assert.match(optedIn, /FOOT:saveSubtitleBackfill/);
  assert.match(optedIn, /Queue is off\./);
  assert.match(optedIn, /No holder between passes\./);
  for (const id of ["backfill_lease","backfill_enqueued","backfill_remaining","backfill_bytes"])
    assert.match(optedIn,new RegExp(`data-devstat="subtitle_backfill:${id}"`));
  assert.doesNotMatch(html, /HDHomeRun Live TV|CARDHEAD:Programme guide/);
  for (const route of ["livetv", "playback", "cluster"])
    assert.ok(html.includes(`href="#/settings/${route}"`), `${route} has a destination link`);
  assert.match(html, /FOOT:saveAutoQuality/);
  // Graduated 2026-09-28: durable cluster work is deployed and its plan has no
  // fleet receipt to wait for. Its two switches were copies of the permanent
  // ones in Analysis and Maintenance, and the storage-domain editor moved to
  // Libraries beside the roots it names.
  assert.doesNotMatch(html, /Durable cluster work|Shared storage budgets|storageDomains/);
  assert.match(shippedSource("librariesPanel"), /\$\{storageDomainsCard\(\)\}/, "Libraries owns the storage-domain editor");
  assert.match(shippedSource("analysisSettingsPanel"), /togRow\("an-enabled"/, "Analysis keeps the analysis-worker switch");
  assert.match(shippedSource("savePrecache"), /cache_produce_mins/, "Maintenance keeps the pre-transcoding cadence");
  for (const id of ["pcpv1", "dvlr", "dvrenabled", "lcenabled", "lcsubjectenabled", "ca-enabled", "dvwin", "durable-analysis", "durable-pretranscode"])
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
  assert.match(playback, /This saved switch is authoritative; readiness is advisory and never overrides your choice/);
  // The switch has to be wired to something. A control that renders and does
  // nothing is worse than no control: it reports a capability to the operator
  // that the server never hears about.
  assert.match(playback, /TOG:pdp\|[^|]*\|[^|]*\|checked=true\|onchange="setPreparedHandoff\(this\.checked\)"/,
    "the prepared-handoff switch reflects the stored state and sets it");
  assert.match(playback, /Automatic decode recovery/);
  assert.match(playback, /TOG:adr\|[^|]*\|[^|]*\|checked=true/);
  assert.match(playback, /FOOT:saveAutomaticDecoderRecovery/);
  assert.match(playback, /FOOT:saveVerifiedDecode/);
  assert.match(playback, /missing measurements or retained contracts never turn it back off/);
  assert.match(playback, /reopen loop/);
  assert.match(playback, /One recovery per playback, and it is never given back/);
  assert.match(playback, /best-effort selected-stream diagnostics/);
  assert.match(html, /Native controllers[\s\S]*?not met in this build/);
  assert.match(html, /HDR playback[\s\S]*?not measured/);
  assert.match(html, /These observations never gate this checkbox/);
  // Paul's Developer lifecycle (2026-09-28): a card lives on this page only
  // while its feature is not fully active or not fully tested, and it says
  // what it is waiting on and where it goes when that lands. A card added
  // without saying so fails here, whatever else it renders.
  const developerCards = html.split("CARD[").slice(1);
  assert.ok(developerCards.length >= 15, `Developer renders its cards (${developerCards.length})`);
  assert.ok(developerCards.length <= 20, `Developer holds only what still waits (${developerCards.length})`);
  for (const card of developerCards) {
    const title = (/CARDHEAD:([^|]*)/.exec(card) || [])[1] || card.slice(0, 80);
    assert.match(card, /<b>Leaves Developer when:<\/b> \S[^<]*<b>Then:<\/b> \S/,
      `the Developer card "${title}" names what it waits on and where it graduates`);
  }
  // Each condition names only what is still owed on main. Already-shipped
  // work named as a wait teaches the reader to distrust every other line
  // (#591 review, checked against 5ba02212f, run 3205, 87ca67c0e and L-02).
  const waitsOf = (title) => {
    const card = developerCards.find((c) => c.startsWith(`CARDHEAD:${title}|`));
    assert.ok(card, `Developer renders ${title}`);
    return /Leaves Developer when:<\/b> ([^<]*)<b>Then:/.exec(card)[1];
  };
  assert.doesNotMatch(waitsOf("Unverified HEVC copy"), /containment is deployed and/);
  assert.match(waitsOf("Unverified HEVC copy"), /containment itself is deployed \(87ca67c0e\)/);
  assert.match(waitsOf("CODECS on SDR master playlists"), /Apple TV and iPhone device check confirms every SDR variant is still offered/);
  // Adaptive Auto's graduation is Paul's choice between two destinations.
  const autoCard = developerCards.find((card) => card.startsWith("CARDHEAD:Adaptive Auto quality|"));
  assert.ok(autoCard, "Developer renders the adaptive Auto card");
  assert.match(autoCard, /Leaves Developer when:<\/b> A-04's D3 matrix[^<]*A-05's native controllers[^<]*<b>Then:<\/b> Paul chooses: the switch returns to Playback as a permanent toggle, or it is removed/);
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
  assert.match(live, /tuner itself belongs to the cluster/);
  assert.doesNotMatch(live, /Developer → Enable Live TV/);
  assert.doesNotMatch(live, /ltowner|ltfenced|setLiveTvEnabled/);
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
  for (const file of ["http/dvr.rs", "http/library_channels.rs", "channel_subjects.rs"]) {
    const source = fs.readFileSync(path.join(sourceRoot, file), "utf8");
    assert.match(source, /Settings → Live TV/, `${file} names the current destination`);
    assert.doesNotMatch(source, /Settings → Developer|Developer settings|use Developer recovery|Run the Developer readiness check/,
      `${file} must not send an operator to the retired destination`);
  }
  for (const file of ["http/live_tv.rs", "live_tv.rs"]) {
    const source = fs.readFileSync(path.join(sourceRoot, file), "utf8");
    assert.match(source, /Live TV is disabled[^"\n]*Settings → Live TV/,
      `${file} sends enablement to the Live TV section, where the switch lives`);
    assert.doesNotMatch(source, /Live TV is disabled[^"\n]*Settings → Developer/,
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
  for(const tab of ["metadata","playback","livetv","analysis","maintenance","developer","cluster"]){
    const evidence={items:[]};
    patch.ok(tab,"developerReadiness",evidence);
    assert.equal(updates.at(-1),evidence);
    patch.fail(tab,"developerReadiness",new Error("offline"));
    assert.deepEqual(updates.at(-1),{unavailable:"offline"});
  }
  assert.equal(updates.length,14);
});

test("Cluster's settings cards fill their own mount when /settings lands, and say so when it cannot", () => {
  const mount={outerHTML:"",innerHTML:""}, revealed=[];
  const patch=new Function("applyDeveloperReadiness","esc","document","clusterSettingsSection","clusterSettingsUnavailable","revealSettingsAnchor","SETTINGS_DATA",`
    ${shippedConst("SETTINGS_MANIFEST")}
    ${shippedSource("patchSettingsSecondary")}
    ${shippedSource("patchSettingsSecondaryError")}
    return {ok:patchSettingsSecondary,fail:patchSettingsSecondaryError};
  `)(()=>{},esc,{getElementById:id=>id==="cluster-settings"?mount:null},d=>`cards:${d.settings.x}`,
    message=>`unavailable:${message}`,tab=>revealed.push(tab),{settings:{x:1}});
  // No renderSettings: the roster above is interactive while this lands, and
  // repainting it would close an open drill-down or an election dialog.
  patch.ok("cluster","settings",{x:1});
  assert.equal(mount.outerHTML,"cards:1");
  assert.deepEqual(revealed,["cluster"],"a link that named a card in here lands on it once it exists");
  patch.fail("cluster","settings",new Error("offline <now>"));
  assert.equal(mount.outerHTML,"unavailable:offline &lt;now&gt;");
  const unavailable=new Function(`${shippedSource("clusterSettingsUnavailable")} return clusterSettingsUnavailable;`)()("offline");
  assert.match(unavailable,/data-state="error"/);
  assert.match(unavailable,/onclick="retryClusterSettings\(this\)"/,"the section retries its own read, not the board's");
  // Another tab's settings read never reaches the cluster mount.
  mount.outerHTML="untouched"; patch.ok("playback","settings",{x:2});
  assert.equal(mount.outerHTML,"untouched");
});

test("a section repaint carries kept settings cards across as live nodes", () => {
  // The cluster board repaints the whole section on status changes and
  // roster refreshes; the analysis summary does after its Save. A settings
  // card on the same page must come through with its unsaved edit, its
  // enabled Save, open disclosures and focus — so the node itself is kept.
  const make=(id,state)=>{
    const node={id,dataset:state?{state}:{},isConnected:true,contains:other=>other===node||other===node.child,replaced:null};
    node.child={isConnected:true,focused:0,focus(){this.focused+=1;}};
    node.replaceWith=other=>{node.replaced=other;};
    return node;
  };
  let dom={}, active=null;
  const document={getElementById:id=>dom[id]||null,get activeElement(){return active;}};
  const keep=new Function("document",`${shippedConst("SETTINGS_KEPT_MOUNTS")}
    ${shippedSource("captureSettingsMounts")}
    ${shippedSource("restoreSettingsMounts")}
    return {capture:captureSettingsMounts,restore:restoreSettingsMounts};`)(document);
  const editing=make("cluster-settings","ready"); dom={"cluster-settings":editing}; active=editing.child;
  const captured=keep.capture("cluster");
  assert.deepEqual(captured.nodes,[editing]);
  const repainted=make("cluster-settings","ready"); dom={"cluster-settings":repainted};
  keep.restore(captured);
  assert.equal(repainted.replaced,editing,"the repaint's copy is replaced by the node being edited");
  assert.equal(editing.child.focused,1,"focus goes back where it was");
  // Waiting for /settings is never kept over the cards themselves…
  dom={"cluster-settings":make("cluster-settings","loading")};
  assert.deepEqual(keep.capture("cluster").nodes,[]);
  // …and an error stays until the data arrives, then gives way to it.
  const failed=make("cluster-settings","error"); dom={"cluster-settings":failed};
  const errorKeep=keep.capture("cluster");
  const stillLoading=make("cluster-settings","loading"); dom={"cluster-settings":stillLoading};
  keep.restore(errorKeep); assert.equal(stillLoading.replaced,failed);
  const arrived=make("cluster-settings","ready"); dom={"cluster-settings":arrived};
  keep.restore(errorKeep); assert.equal(arrived.replaced,null);
  // Analysis keeps the backfill card across its own panel's Save.
  const backfill=make("subbackfillcard"); dom={subbackfillcard:backfill}; active=null;
  assert.deepEqual(keep.capture("analysis").nodes,[backfill]);
  const setup=make("enable-subtitle-transcription"); setup.savedEnabled=false; setup.unsavedLanguage="sv";
  dom={"enable-subtitle-transcription":setup}; active=setup.child;
  const setupKeep=keep.capture("maintenance");
  const setupReplacement=make("enable-subtitle-transcription"); dom={"enable-subtitle-transcription":setupReplacement};
  keep.restore(setupKeep);
  assert.equal(setupReplacement.replaced,setup,"Maintenance keeps the live setup and its saved/unsaved controls");
  assert.equal(setupReplacement.replaced.savedEnabled,false);
  assert.equal(setupReplacement.replaced.unsavedLanguage,"sv");
  assert.equal(setup.child.focused,1);

  const translation=make("enable-subtitle-translation");
  translation.savedEnabled=false;translation.draft={enabled:true,profile:"saved-model-choice",language:"de"};
  dom={"enable-subtitle-translation":translation};active=translation.child;
  const translationKeep=keep.capture("playback");
  const translationReplacement=make("enable-subtitle-translation");dom={"enable-subtitle-translation":translationReplacement};
  keep.restore(translationKeep);
  assert.equal(translationReplacement.replaced,translation,"Playback keeps the live translation setup across aggregate repaint");
  assert.equal(translationReplacement.replaced.savedEnabled,false);
  assert.deepEqual(translationReplacement.replaced.draft,{enabled:true,profile:"saved-model-choice",language:"de"});
  assert.equal(translation.child.focused,1,"translation focus survives the repaint");

  assert.deepEqual(keep.capture("integrations").nodes,[],"unowned sections redraw everything, as before");
  const render=shippedSource("renderSettings");
  assert.ok(render.indexOf("captureSettingsMounts(tab)")<render.indexOf('innerHTML=`<div class="setlayout">'),"captured before the repaint");
  assert.ok(render.indexOf("restoreSettingsMounts(keep)")>render.indexOf('innerHTML=`<div class="setlayout">'),"restored after it");
  assert.match(shippedSource("viewSettings"),/SETTINGS_PENDING_KEEP=!reset&&SETTINGS_SHOWN_TAB===tab\?captureSettingsMounts\(tab\):null/,
    "a same-section reload (Refresh nodes) carries them across the loading shell too");
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
    /replaceLiveTvCard\("live-tv-settings",liveTvSettingsCard\(saved\),"ltip"\)/);
  assert.doesNotMatch(shippedSource("saveLiveTvEnable"), /renderSettings\(/,
    "saving the on/off switch cannot erase a tuner or guide draft");
  assert.match(shippedSource("saveLiveTvEnable"),
    /replaceLiveTvCard\("live-tv-enable",liveTvEnableCard\(saved\),"lt-enable"\)/,
    "it repaints only its own card");
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
  assert.match(html, /href="#\/settings\/maintenance\/subsrccard"/, "the link lands on the switch");
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
  const location = { hash: "#/settings/maintenance/subsrccard" };
  const replaced = [];
  const history = { replaceState: (_s, _t, url) => { replaced.push(url); location.hash = url; } };
  let scrolled = 0;
  const document = { getElementById: (id) => (id === "subsrccard" ? { scrollIntoView: () => { scrolled += 1; } } : null) };
  const api = r(location, history, document);
  assert.equal(api.settingsRouteTab(location.hash), "maintenance", "the section still routes");
  assert.equal(api.settingsRouteAnchor(location.hash), "subsrccard");
  assert.equal(api.settingsRouteAnchor("#/settings/maintenance"), null);
  assert.equal(api.settingsRouteTab("#/settings/nothere/subsrccard"), null);
  api.revealSettingsAnchor("maintenance");
  assert.equal(scrolled, 1, "scrolled to the named card");
  assert.deepEqual(replaced, ["#/settings/maintenance"], "the address drops the anchor");
  api.revealSettingsAnchor("maintenance");
  assert.equal(scrolled, 1, "once: a repaint does not scroll the reader back");
  assert.match(shippedSource("renderSettings"), /settingsPanel\(tab,d\)\}<\/div><\/div>`;\s*restoreSettingsMounts\(keep\);\s*revealSettingsAnchor\(tab\);/);
  assert.match(shippedSource("subtitleStoredSourcesCard"), /\{id:"subsrccard"\}/, "the anchor is the switch's own card");
  assert.match(shippedSource("maintenancePanel"), /subtitleStoredSourcesCard\(settings,readiness\)/, "and Maintenance draws it");
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
  assert.equal((SHIPPED_UI.match(/id="lookmenu"/g) || []).length, 4, "one menu per layout chrome");
  assert.equal((SHIPPED_UI.match(/id="thememenu"|id="appmenu"|id="sizemenu"/g) || []).length, 0);
  const menu = shippedSource("lookMenuHtml");
  assert.match(menu, /themeMenuHtml\(\)/); assert.match(menu, /appMenuHtml\(\)/); assert.match(menu, /sizeMenuHtml\(\)/);
  assert.match(shippedSource("themeMenuHtml"), /layoutMenuHtml\(\)/);
  assert.match(shippedSource("closeMenus"), /\["lookmenu","profilemenu"\]/);
  for (const fn of ["setTheme", "setAppearance", "setIconSize"])
    assert.match(shippedSource(fn), /repaintLookMenu\(\)/, `${fn} repaints the shared menu`);
  assert.match(shippedSource("sizeMenuHtml"), /Poster size/, "named as the mobile apps name it");
});

test("HEVC override saves either choice without consulting advisory readiness", async () => {
  for (const enabled of [true, false]) {
    const calls=[];
    const err={textContent:""}, card={outerHTML:""};
    const save = new Function("api","document","cacheSettings","hevcCopyCard","toast",
      `${shippedSource("saveHevcCopy")}\nreturn saveHevcCopy;`)(
      async (path, opts) => {calls.push([path,opts.body]);return {hevc_unverified_copy:enabled,hevc_header_trace_available:false};},
      {getElementById:(id)=>id==="hevc-copy-error"?err:id==="hevc-copy-card"?card:{checked:enabled}},
      ()=>{}, s=>`saved:${s.hevc_unverified_copy}`, ()=>{});
    await save({disabled:false});
    assert.deepEqual(calls, [["/settings",{hevc_unverified_copy:enabled}]]);
    assert.equal(card.outerHTML, `saved:${enabled}`);
    assert.equal(err.textContent, "");
  }
});

test("Cinema sharing saves both choices with unknown network qualification", async () => {
  const readiness={items:[{id:"cinema_sharing",requirements:[{id:"network",status:"unknown",evidence:"No qualified network receipt."}]}]};
  const writes=[];
  const nodes={"cinema-sharing-enabled":{checked:false},"cinema-sharing-error":{textContent:""},"cinema-sharing-settings":{outerHTML:""}};
  let reject=false;
  const save=new Function("document","api","cacheSettings","cinemaSharingCard","DEVELOPER_READINESS",
    `${shippedSource("saveCinemaSharing")}\nreturn saveCinemaSharing;`)(
      {getElementById:id=>{assert.ok(id in nodes);return nodes[id];}},
      async(path,request)=>{writes.push([path,request.body]);if(reject)throw new Error("Write unavailable");return request.body;},
      value=>value,(settings,evidence)=>{assert.equal(evidence,readiness);return `saved:${settings.sharing_enabled}`;},readiness);
  for(const enabled of [true,false]){
    nodes["cinema-sharing-enabled"].checked=enabled;
    const button={disabled:false};await save(button);
    assert.deepEqual(writes.at(-1),["/settings",{sharing_enabled:enabled}]);
    assert.equal(nodes["cinema-sharing-settings"].outerHTML,`saved:${enabled}`);
    assert.equal(button.disabled,false);
  }
  reject=true;await save({disabled:false});
  assert.equal(nodes["cinema-sharing-error"].textContent,"Write unavailable");
  assert.equal(nodes["cinema-sharing-settings"].outerHTML,"saved:false");
});

test("SDR master CODECS saves either choice without consulting advisory readiness", async () => {
  for (const enabled of [true, false]) {
    const calls=[], err={textContent:""}, card={outerHTML:""}, btn={disabled:false};
    const save = new Function("api","document","cacheSettings","sdrMasterCodecsCard","toast","DEVELOPER_READINESS",
      `${shippedSource("saveSdrMasterCodecs")}\nreturn saveSdrMasterCodecs;`)(
      async (path, opts) => {calls.push([path,opts.body]);return {playback_sdr_master_codecs:enabled};},
      {getElementById:(id)=>id==="sdr-codecs-error"?err:id==="sdr-codecs-card"?card:{checked:enabled}},
      (s)=>s, (s,r)=>`saved:${s.playback_sdr_master_codecs}:${r.items[0].requirements[0].status}`, ()=>{},
      {items:[{id:"sdr_master_codecs",requirements:[{id:"sdr_codecs_device_requalification",status:"unmet"}]}]});
    await save(btn);
    assert.deepEqual(calls, [["/settings",{playback_sdr_master_codecs:enabled}]]);
    assert.equal(card.outerHTML, `saved:${enabled}:unmet`, "the card redraws with the readiness it already had");
    assert.equal(err.textContent, "");
  }
});

test("Jellyfin compatibility saves both explicit choices without a readiness veto", async () => {
  for(const enabled of [true,false]) {
    const calls=[],err={textContent:""},btn={disabled:false};
    const save=new Function("api","document","cacheSettings","toast","setCardSaved",
      `${shippedSource("saveJellyfinCompatibility")}\nreturn saveJellyfinCompatibility;`)(
        async(path,opts)=>{calls.push([path,opts.body]);return {jellyfin_compatibility_enabled:enabled};},
        {getElementById:id=>id==="jellyfin-compatibility-error"?err:{checked:enabled}},()=>{},()=>{},()=>{});
    await save(btn);
    assert.deepEqual(calls,[["/settings",{jellyfin_compatibility_enabled:enabled}]]);
    assert.equal(err.textContent,"");
  }
});

test("Rate control round-trips an unset request as unset and keeps explicit choices", async () => {
  const card = new Function("setCard","cardHead","setCardFoot","esc",
    `${shippedSource("rateControlCard")}\nreturn rateControlCard;`)(
    (body, opts) => `CARD#${(opts||{}).id}[${body}]`, (title) => `HEAD:${title}`, (fn) => `FOOT:${fn}`, esc);
  const request = new Function(`${shippedSource("rateControlRequest")}\nreturn rateControlRequest;`)();
  const selected = (html) => {
    const chosen = [...html.matchAll(/<option value="([^"]*)" (selected)?>/g)].filter((m) => m[2]);
    assert.equal(chosen.length, 1, html);
    return chosen[0][1];
  };
  const quality = (html) => /id="prq"[^>]*value="([^"]*)"/.exec(html)[1];
  const unset = {transcode_rate_mode:null, transcode_quality:null, transcode_rate_mode_default:"bitrate",
    transcode_rate_mode_default_encoder:"qsv", transcode_quality_default:22};
  const html = card(unset);
  assert.match(html, /Default \(per encoder\) — on this node, qsv uses bitrate/);
  assert.match(html, /id="prq"[^>]* disabled>/, "the quality value is inert outside Quality mode");
  assert.match(html, /placeholder="family default \(22\)"/);
  assert.match(html, /FOOT:saveRateControl/);
  // A Save that never touched the control sends the clear, never a bitrate pin.
  assert.deepEqual(request(selected(html), quality(html)), {transcode_rate_mode:null, transcode_quality:null});
  for (const [mode, q] of [["bitrate", null], ["quality", 21]]) {
    const explicit = card({...unset, transcode_rate_mode:mode, transcode_quality:q});
    assert.deepEqual(request(selected(explicit), quality(explicit)), {transcode_rate_mode:mode, transcode_quality:q});
  }
  assert.doesNotMatch(card({...unset, transcode_rate_mode:"quality", transcode_quality:21}), /id="prq"[^>]* disabled>/);

  const calls = [];
  const fields = {prc:{value:""}, prq:{value:""}, rcerr:{textContent:""}, rccard:{outerHTML:""}};
  const save = new Function("api","document","cacheSettings","toast","setCardSaved","rateControlCard","rateControlRequest",
    `${shippedSource("saveRateControl")}\nreturn saveRateControl;`)(
    async (path, opts) => { calls.push([path, opts.method, opts.body]); return unset; },
    {getElementById:(id) => fields[id]}, () => {}, () => {}, () => {}, (s) => `rerendered:${s.transcode_rate_mode}`, request);
  await save({disabled:false});
  assert.deepEqual(calls, [["/settings", "PUT", {transcode_rate_mode:null, transcode_quality:null}]]);
  assert.equal(fields.rccard.outerHTML, "rerendered:null");
  assert.equal(fields.rcerr.textContent, "");
});

test("Default plus a typed value sends {null, null}", async () => {
  const request = new Function(`${shippedSource("rateControlRequest")}\nreturn rateControlRequest;`)();
  // `effective_for` reads the quality only when the mode resolves to Quality,
  // so outside Quality a value changes no output — but it would move the
  // speculative key's quality component and cancel queued rows cluster-wide.
  assert.deepEqual(request("", "22"), {transcode_rate_mode:null, transcode_quality:null});
  assert.deepEqual(request("bitrate", "22"), {transcode_rate_mode:"bitrate", transcode_quality:null});
  assert.deepEqual(request("quality", "22"), {transcode_rate_mode:"quality", transcode_quality:22});

  const calls = [];
  const fields = {prc:{value:""}, prq:{value:"22", disabled:false}, rcerr:{textContent:""}, rccard:{outerHTML:""}};
  const save = new Function("api","document","cacheSettings","toast","setCardSaved","rateControlCard","rateControlRequest",
    `${shippedSource("saveRateControl")}\nreturn saveRateControl;`)(
    async (path, opts) => { calls.push(opts.body); return {}; },
    {getElementById:(id) => fields[id]}, () => {}, () => {}, () => {}, () => "", request);
  await save({disabled:false});
  assert.deepEqual(calls, [{transcode_rate_mode:null, transcode_quality:null}]);

  const changed = new Function("document", `${shippedSource("rateControlModeChanged")}\nreturn rateControlModeChanged;`)(
    {getElementById:(id) => fields[id]});
  changed({value:"bitrate"});
  assert.deepEqual([fields.prq.disabled, fields.prq.value], [true, ""]);
  changed({value:"quality"});
  assert.equal(fields.prq.disabled, false);
});

test("the version stamp dates a release by its source date, not a compile time", () => {
  // built_at is SOURCE_DATE_EPOCH, else the commit time, else the compile
  // clock (crates/plurxd/build_support/source_date.rs). For an exact-tag
  // release it is the tagged commit's date, so the label must not say "built".
  const make = (server) => new Function("SERVER",
    `${shippedSource("sourceDateLabel")}\n${shippedSource("buildLabel")}\nreturn buildLabel;`)(server);
  const at = "2026-10-02T14:44:46Z";
  assert.equal(make({version:"0.3.0", build:"v0.3.0", built_at:at})(), "0.3.0 · dated 02 Oct 14:44Z");
  assert.equal(make({version:"0.3.0", build:"unknown", built_at:at})(), "0.3.0 · dated 02 Oct 14:44Z");
  assert.equal(make({version:"0.3.0", build:"v0.3.0-5-gabc", built_at:at})(), "0.3.0 · v0.3.0-5-gabc");
  const tag = new Function("esc",
    `${shippedSource("sourceDateLabel")}\n${shippedSource("buildTag")}\nreturn buildTag;`)(esc);
  const unstamped = tag({version:"0.3.0", build:"unknown", built_at:at});
  assert.match(unstamped, /\(unstamped · dated 02 Oct 14:44Z\)/);
  assert.doesNotMatch(unstamped, /build time|· built/);
});

test("tone-map probe failures stay collapsed beneath the selected pipeline", () => {
  const render = new Function("esc", `${shippedSource("toneMapHtml")}\nreturn toneMapHtml;`)(esc);
  const rejected = {pipeline:"vaapi",label:"GPU tone-map (VA-API)",passed:false,rejected:"Driver refused <HDR> & output"};
  for (const selected of ["libplacebo_vaapi", "cpu"]) {
    const label = selected === "cpu" ? "CPU tone-map" : "GPU tone-map (Vulkan / VA-API)";
    const html = render({ran:true,selected,selected_label:label,verdicts:[rejected,
      {pipeline:selected,label,passed:true}]});
    const disclosure = html.match(/<details\b([^>]*)>([\s\S]*?)<\/details>/);
    assert.ok(disclosure, "rejected probes remain available for diagnosis");
    assert.doesNotMatch(disclosure[1], /\bopen(?:\s|=|$)/, "probe details start collapsed");
    assert.match(disclosure[2], /<summary[^>]*>Probe details<\/summary>/);
    assert.ok(disclosure[2].includes("Driver refused &lt;HDR&gt; &amp; output"));
    const visible = html.replace(disclosure[0], "");
    assert.ok(visible.includes(label), "the selected pipeline stays visible");
    assert.doesNotMatch(visible, /Driver refused|GPU tone-map \(VA-API\)/);
    assert.equal(visible.includes("fell back"), selected === "cpu", "CPU fallback stays explicit");
  }
  assert.doesNotMatch(render({ran:true,selected:"libplacebo_vaapi",selected_label:"GPU tone-map (Vulkan / VA-API)",verdicts:[]}), /<details/,
    "no empty disclosure when no probe was rejected");
  assert.match(render({ran:false,selected:"cpu",verdicts:[{rejected:"software encoder"}]}), /software encoder/,
    "an unprobed node keeps its short explanation");
});

test("a Mac node reports the Metal tone-map through Mac video processing and never calls a missing build feature a fallback", () => {
  const render = new Function("esc", `${shippedSource("toneMapHtml")}\nreturn toneMapHtml;`)(esc);
  const vulkan = {pipeline:"libplacebo",label:"GPU tone-map (Vulkan)",passed:false,unavailable:true,
    rejected:"not available on this platform — this FFmpeg build has no vulkan support"};
  const metal = {pipeline:"vt_tonemap_metal",label:"GPU tone-map (Metal)",passed:true,speedup:2.16};
  // A passing Metal graph ends the race with node routing on the CPU chain.
  const tm = {ran:true,selected:"cpu",selected_label:"CPU tone-map",
    verdicts:[metal,vulkan,{pipeline:"cpu",label:"CPU tone-map",passed:true,speedup:1}]};
  const visible = (html) => html.replace(/<details\b[\s\S]*?<\/details>/, "");

  const off = render(tm, {enabled:false,hdr10_metal:{availability:"available",reason:"available"}});
  assert.match(visible(off), /^CPU tone-map /);
  assert.match(visible(off), /GPU tone-map \(Metal\) matched the CPU picture at 2\.16× the CPU chain; HDR uses it once Mac video processing is on in Settings → Developer/);
  assert.doesNotMatch(off, /fell back|Cannot allocate/);
  assert.match(off, /<details[\s\S]*GPU tone-map \(Vulkan\): not available on this platform — this FFmpeg build has no vulkan support/);
  assert.match(render(tm), /HDR uses it once Mac video processing is on/, "no Mac data reads as off");

  const on = render(tm, {enabled:true,hdr10_metal:{availability:"available",reason:"available"}});
  assert.match(visible(on), /^<span style="color:var\(--good\)">GPU tone-map \(Metal\)<\/span>/);
  assert.doesNotMatch(visible(on), /matched the CPU picture|fell back/);

  const refused = render(tm, {enabled:true,hdr10_metal:{availability:"unavailable",reason:"cpu_parity_failed"}});
  assert.match(visible(refused), /Mac video processing reports it unavailable \(cpu_parity_failed\)/);
  const pending = render(tm, {enabled:true,hdr10_metal:{availability:"pending",reason:"probe_pending"}});
  assert.match(visible(pending), /Mac video processing is still checking it/);

  const cpu = render({ran:true,selected:"cpu",selected_label:"CPU tone-map",verdicts:[vulkan]}, null);
  assert.doesNotMatch(cpu, /fell back/, "a candidate the build cannot express is not a fallback");
});

test("a standalone server keeps every local switch; cluster switches live only in Cluster", () => {
  const panels=developerPanels();
  const settings=Object.freeze({cluster_media_pool_enabled:true,cluster_session_takeover_enabled:true,
    cluster_clock_guard_enforced:true,bounded_replica_reads:true,backup_destination:"/backups",
    subtitle_cluster_sources:true,subtitle_stored_sources:true,subtitle_backfill:true,sharing_enabled:true});
  panels.server.cluster_enabled=false;
  const html=panels.developerPanel(settings,{items:[]});
  assert.doesNotMatch(html,/Cluster work|Portable cluster backup|Cluster clock guard|Cluster media placement|Local catalogue reads|Share stored subtitle tracks|href="#\/settings\/cluster"/);
  assert.match(html,/TOG:cinema-sharing-enabled\|/,"sharing still applies to this server");
  assert.match(panels.maintenancePanel(settings,null,{items:[]}),/TOG:subsrc\|[^|]*\|[^|]*\|checked=true/,"stored tracks are a local choice");
  assert.match(panels.analysisSettingsPanel(settings,null,{items:[]}),/TOG:subbackfill\|[^|]*\|[^|]*\|checked=true/,"so is backfill");
  assert.match(panels.liveTvPanel(settings,{items:[]}),/TOG:lt-enable\|/,"and the Live TV switch");
  assert.doesNotMatch(panels.liveTvPanel(settings,{items:[]}),/keep a ready voter majority/);
  panels.server.cluster_enabled=true;
  panels.server.cluster_advertisement=false; // Persisted member without an explicit advertised host.
  const clustered=panels.clusterSettingsSection({settings,developerReadiness:{items:[]}});
  assert.match(clustered,/Cluster media placement[\s\S]*Portable cluster backup/);
  assert.match(clustered,/TOG:subcluster\|[^|]*\|[^|]*\|checked=true/,
    "moving a control never changes its saved value");
});

test("transcription readiness never overrides enable or disables Save", async () => {
  const helpers=["subtitleLanguageName","subtitleLanguageOptions","subtitleModelSize","subtitleModelOptions","subtitleModelEvidence","subtitleReason","subtitleTranscriptionForm"].map(shippedSource).join("\n");
  const form=new Function("esc",`${helpers} return subtitleTranscriptionForm;`)(esc);
  const missing={version:1,revision:"1",mode:"managed",enabled:true,model_id:"base",advanced:{command:"whisper-cli",model_path:""},language:"en",interval_mins:0};
  const html=form(missing);
  assert.match(html,/id="st-enabled" type="checkbox" checked/);
  assert.doesNotMatch(html,/<button[^>]*disabled[^>]*>Save/);
  assert.match(html,/Readiness never changes this saved choice/);
  const nodes={"subtitle-transcription-form":{isConnected:true,innerHTML:"",_subtitleSetup:{capture:{},edit:0,catalog:{models:[],workers:[]},revision:"1"}},"st-status":{isConnected:true,textContent:""},
    "st-enabled":{checked:true},"st-mode":{value:"managed"},"st-model-id":{value:"base"},"st-command":{value:"whisper-cli"},"st-model":{value:""},"st-language":{value:"en"},"st-interval":{value:"0"}};
  let request;
  const save=new Function("document","api","subtitleTranscriptionForm","subtitleUiCurrent","subtitleSetupPanel",`${shippedSource("saveSubtitleTranscription")} return saveSubtitleTranscription;`)(
    {getElementById:id=>nodes[id]},async(path,options)=>{request={path,...options};return missing;},form,()=>true,()=>nodes["subtitle-transcription-form"]);
  await save({isConnected:true,disabled:false});
  assert.equal(request.path,"/subtitle-transcription/managed");
  assert.equal(request.body.enabled,true);
  assert.equal(request.body.interval_mins,0);
  assert.equal(request.body.expected_revision,"1");
  assert.match(nodes["st-status"].textContent,/Saved/);
});

test("transcription queue refusal is not displayed as successful work", async () => {
  const status={isConnected:true,textContent:"",innerHTML:""},button={isConnected:true,disabled:false};
  const state={capture:{},options:{file_id:"42",source_size:"100",source_mtime:"10",model_id:"base",audio_tracks:[{index:0,language:"en",normalized_language:"en"}]},models:[{id:"base",languages:["en"]}]};
  const nodes={"subtitle-generate-42":{_subtitleGeneration:state},"sg-status-42":status,"sg-a-42":{value:"0"},"sg-l-42":{value:"en"}};
  let outcome="queue_full";
  const generate=new Function("document","api","subtitleGenerationCurrent","subtitleUiCurrent","subtitleReason","subtitleJobLink","subtitleGenerationLanguage","pollSubtitleGeneration","subtitleGenerationPanel","subtitleGenerationInput",`${shippedSource("submitSubtitleGeneration")}\nreturn submitSubtitleGeneration;`)(
    {getElementById:id=>nodes[id]},async()=>({outcome,job_id:"job-a",artifact_key:"artifact",file_id:"42",source_size:"100",source_mtime:"10"}),()=>true,()=>true,String,id=>`<a href="#/activity/jobs/${id}">Activity</a>`,track=>track.normalized_language,()=>{},id=>nodes[`subtitle-generate-${id}`],id=>nodes[id]);
  await generate("42",button);
  assert.match(status.textContent,/queue is full/);
  assert.equal(status.innerHTML,"");
  assert.equal(button.disabled,false);
  outcome="existing";
  await generate("42",button);
  assert.match(status.innerHTML,/already queued or running/);
  assert.match(status.innerHTML,/#\/activity\/jobs\/job-a/);
  assert.equal(button.disabled,false);
});

test("the census row only paints the census of the node Settings loaded", () => {
  const SETTINGS_DATA={sys:{node_id:"node-a",tone_map:{census:{state:"idle",node_id:"node-a"}}}};
  const mount={innerHTML:"",querySelector:()=>null,insertAdjacentHTML(_where,html){this.innerHTML+=html;}};
  const paint=new Function("SETTINGS_DATA","document","toneMapCensusHtml",
    `${shippedSource("censusFromThisNode")}\n${shippedSource("paintToneMapCensus")}\nreturn paintToneMapCensus;`)(
    SETTINGS_DATA,{getElementById:()=>mount},c=>`STATE:${c.state}`);
  paint({state:"running",node_id:"node-b"});
  assert.equal(SETTINGS_DATA.sys.tone_map.census.state,"idle","another node's census is not stored as this one's");
  assert.match(mount.innerHTML,/Another node answered/);
  paint({state:"running",node_id:"node-a"});
  assert.equal(SETTINGS_DATA.sys.tone_map.census.state,"running");
  assert.equal(mount.innerHTML,"STATE:running");
});

test("the tone-map census row is attributable and stoppable", () => {
  const sources = ["fmtAgo", "censusMount", "toneMapCensusHtml", "toneMapCensusResultHtml", "toneMapHtml"]
    .map(shippedSource).join("\n");
  const {toneMapHtml, toneMapCensusHtml} = new Function("esc",
    `${sources}\nreturn {toneMapHtml, toneMapCensusHtml};`)(esc);
  const key = {ffmpeg_version:"ffmpeg version 8.1.3", encoder_family:"qsv", gpu_device:"/dev/dri/renderD128"};
  // Idle: a Run control and the cost stated; no Stop.
  const idle = toneMapCensusHtml({state:"idle", key, result:null});
  assert.match(idle, /startToneMapCensus\(this\)">Run census</);
  assert.doesNotMatch(idle, /stopToneMapCensus/);
  assert.match(idle, /Uses the GPU/);
  // Queued and running say what they are doing and offer Stop, not Run.
  for (const state of ["queued", "running"]) {
    const html = toneMapCensusHtml({state, step:"vpp_qsv · maxcll-4000 <b>", candidate:"vpp_qsv", key, result:null});
    assert.match(html, /stopToneMapCensus\(this\)">Stop</, state);
    assert.doesNotMatch(html, /Run census/, state);
    assert.ok(html.includes("vpp_qsv · maxcll-4000 &lt;b&gt;"), "the step is shown, escaped");
  }
  // Yielding to playback is reported as such, not as a failure.
  const yielded = toneMapCensusHtml({state:"yielded", since:Date.now(), key, result:null});
  assert.match(yielded, /Yielded to playback/);
  assert.doesNotMatch(yielded, /Failed/);
  assert.match(toneMapCensusHtml({state:"failed", reason:"no <libx265>", key, result:null}), /Failed.*no &lt;libx265&gt;/s);
  // A result lists the baseline and every candidate per signal, with stills.
  const result = {key, noise_floor_zero:true,
    baseline:[{signal:"maxcll-1000", brightest_block_luma:235, noise_floor:0, still:"cpu-maxcll-1000.png"}],
    candidates:[
      {pipeline:"vpp_qsv", label:"GPU tone-map (QSV)", selected:true, signals:[
        {signal:"maxcll-1000", started:true, distance:{max:[9.5,2,1], p95:[4,1,0.5]}, brightest_block_luma:230, still:"vpp_qsv-maxcll-1000.png"}]},
      {pipeline:"tonemap_opencl", label:"GPU tone-map (OpenCL)", selected:false, signals:[
        {signal:"maxcll-1000", started:false, reason:"no OpenCL <device>"}]}]};
  const done = toneMapCensusHtml({state:"finished", since:Date.now(), key, result});
  assert.match(done, /Finished/);
  assert.match(done, /9\.5 \/ 2\.0 \/ 1\.0/);
  assert.match(done, /GPU tone-map \(QSV\) <span class="muted">\(selected\)/);
  assert.ok(done.includes("did not start: no OpenCL &lt;device&gt;"));
  assert.ok(done.includes("showCensusStill(this,&quot;vpp_qsv-maxcll-1000.png&quot;)"));
  assert.match(done, /noise floor 0/);
  // The row carries the census beneath the probe's own line, and only when the
  // server reports one.
  const tm = {ran:true, selected:"vpp_qsv", selected_label:"GPU tone-map (QSV)", verdicts:[]};
  assert.doesNotMatch(toneMapHtml(tm), /tone-map-census/);
  assert.match(toneMapHtml({...tm, census:{state:"idle", key, result:null}}), /id="tone-map-census"[\s\S]*Run census/);
});

main().then(() => {
  if (started !== finished) failures += started - finished;
  process.stdout.write(`${started - failures}/${started} passed\n`);
  process.exit(failures ? 1 : 0);
});

test("Cinema sharing reports current activation during rolling upgrade without a restart gate", () => {
  const requirements=[];
  const card=new Function("setCard","cardHead","togRow","devReq","devGraduation","setCardFoot",
    `${shippedSource("cinemaSharingCard")} return cinemaSharingCard;`)(
      value=>value,()=>"",()=>"",
      (readiness,item,id,title,detail)=>{
        requirements.push({id,title,detail});
        return readiness.items[0].requirements.find(row=>row.id===id)?.evidence || detail;
      },()=>"",()=>"");
  for(const ready of [false,true]) {
    const evidence=ready?"Current layout verified.":"Source activation is pending.";
    const html=card({sharing_enabled:true},{items:[{id:"cinema_sharing",requirements:[{id:"source_activation",status:ready?"met":"unmet",evidence}]}]});
    assert.ok(html.includes(evidence));
    assert.ok(!html.includes("coordinated drained restart"));
  }
  const activation=requirements.find(row=>row.id==="source_activation");
  assert.equal(activation.title,"Current Source activation");
  assert.match(activation.detail,/cluster compatibility and authority checks/);
  assert.match(activation.detail,/coordinated restart is not required/);
  assert.match(activation.detail,/never changes your saved choice/);
});

test("Cinema sharing lists endpoint setup and unverified host network without gating the switch", () => {
  const requirements=[];let enabled;
  const card=new Function("setCard","cardHead","togRow","devReq","devGraduation","setCardFoot",
    `${shippedSource("cinemaSharingCard")} return cinemaSharingCard;`)(
      value=>value,()=>"",(id,title,detail,choice)=>{enabled=choice;return detail;},
      (readiness,item,id,title,detail)=>{requirements.push({id,detail});return detail;},()=>"",()=>"");
  const html=card({sharing_enabled:true},{items:[]});
  assert.equal(enabled,true);
  assert.match(requirements.find(row=>row.id==="endpoints").detail,/Settings → Sharing/);
  assert.match(html,/Saved endpoints do not prove connectivity/);
  assert.match(requirements.find(row=>row.id==="network").detail,/Tailscale installed and signed in/);
  assert.match(html,/A container cannot infer host installation or remote reachability/);
  assert.match(html,/Readiness observations never prevent saving/);
});




test("DV preferences stay independent and editable with unmet or unavailable readiness", () => {
  const panels=developerPanels();
  for(const readiness of [{items:[]},{unavailable:"Reading failed"},{items:[
    {id:"dolby_vision_hdr_processing",requirements:[{id:"dv_hdr_backend_reference",status:"unmet",evidence:"No qualified backend."}]},
    {id:"dolby_vision_fel_reencode",requirements:[{id:"dv_fel_backend_reference",status:"unmet",evidence:"No qualified authoring."}]}
  ]}]) {
    for(const hdr of [false,true]) for(const fel of [false,true]) {
      const html=panels.developerPanel({dolby_vision_convert:false,dolby_vision_hdr_processing:hdr,dolby_vision_fel_reencode:fel},readiness);
      assert.match(html,new RegExp(`TOG:dv-hdr-processing\\|[^|]*\\|[^|]*\\|checked=${hdr}`));
      assert.match(html,new RegExp(`TOG:dv-fel-reencode\\|[^|]*\\|[^|]*\\|checked=${fel}`));
      assert.match(html,/FOOT:saveDolbyVisionHdrProcessing/);
      assert.match(html,/FOOT:saveDolbyVisionFelReencode/);
      assert.match(html,/Unsupported sources or processing failures use compatible playback/);
      assert.match(html,/Unsupported sources or processing failures keep the existing permitted conversion fallback/);
      assert.doesNotMatch(html,/not implemented in production yet|when qualified/);
      assert.match(html,/This preference never enables conversion by itself/);
      assert.match(html,/HDR backend\/reference proof/);
      assert.match(html,/FEL\/P8.1 backend\/reference proof/);
      assert.match(html,/moves independently to Settings → Playback/);
    }
  }
  for(const name of ["dolbyVisionHdrProcessingCard","dolbyVisionFelReencodeCard"])
    assert.doesNotMatch(shippedSource(name),/disabled|HDR10-E/);
});

test("DV preference saves roundtrip either choice and preserve advisory failures", async () => {
  for(const [name,cardName,field,id] of [
    ["saveDisplayTargetHdr10","displayTargetHdr10Card","dolby_vision_display_target","dv-display-target"],
    ["saveDolbyVisionHdrProcessing","dolbyVisionHdrProcessingCard","dolby_vision_hdr_processing","dv-hdr-processing"],
    ["saveDolbyVisionFelReencode","dolbyVisionFelReencodeCard","dolby_vision_fel_reencode","dv-fel-reencode"]
  ]) {
    for(const readiness of [{unavailable:"Reading failed"},{items:[{id:field,requirements:[{status:"unmet"}]}]}]) {
      const writes=[],nodes={[id]:{checked:false},[`${id}-error`]:{textContent:""},[`${id}-card`]:{outerHTML:""}};
      let reject=false;
      const save=new Function("document","api","cacheSettings",cardName,"DEVELOPER_READINESS","toast",
        `${shippedSource(name)}\nreturn ${name};`)(
        {getElementById:key=>{assert.ok(key in nodes);return nodes[key];}},
        async(path,request)=>{assert.equal(request.method,"PUT");writes.push([path,request.body]);if(reject)throw new Error("Write failed");return {...request.body,dolby_vision_convert:false};},
        value=>value,(settings,evidence)=>{assert.equal(evidence,readiness);return `saved:${settings[field]}`;},readiness,()=>{});
      for(const enabled of [true,false]) {
        nodes[id].checked=enabled;const button={disabled:false};await save(button);
        assert.deepEqual(writes.at(-1),["/settings",{[field]:enabled}]);
        assert.equal(nodes[`${id}-card`].outerHTML,`saved:${enabled}`);
        assert.equal(button.disabled,false);
      }
      reject=true;const button={disabled:false};await save(button);
      assert.equal(nodes[`${id}-error`].textContent,"Write failed");
      assert.equal(nodes[`${id}-card`].outerHTML,"saved:false");
      assert.equal(button.disabled,false);
    }
  }
});


test("DV preference responses preserve newer edits and replacement cards", async () => {
  for(const [name,cardName,field,id] of [
    ["saveDisplayTargetHdr10","displayTargetHdr10Card","dolby_vision_display_target","dv-display-target"],
    ["saveDolbyVisionHdrProcessing","dolbyVisionHdrProcessingCard","dolby_vision_hdr_processing","dv-hdr-processing"],
    ["saveDolbyVisionFelReencode","dolbyVisionFelReencodeCard","dolby_vision_fel_reencode","dv-fel-reencode"]
  ]) {
    for(const change of ["edit","replace","leave"]) {
      const card={dataset:{revision:"1"},outerHTML:"unsaved"};
      const replacement={dataset:{revision:"0"},outerHTML:"new page"};
      const nodes={[id]:{checked:true},[`${id}-error`]:{textContent:""},[`${id}-card`]:card};
      let resolve,requested,cached;
      const pending=new Promise(done=>{resolve=done;});
      const save=new Function("document","api","cacheSettings",cardName,"DEVELOPER_READINESS","toast",
        `${shippedSource(name)}\nreturn ${name};`)(
        {getElementById:key=>nodes[key]},
        (_path,request)=>{requested=request.body;return pending;},
        value=>{cached=value;return value;},()=>"saved:true",{},()=>{});
      const button={disabled:false};const work=save(button);
      assert.deepEqual(requested,{[field]:true});
      if(change==="edit"){nodes[id].checked=false;card.dataset.revision="2";}
      if(change==="replace")nodes[`${id}-card`]=replacement;
      if(change==="leave")nodes[`${id}-card`]=null;
      resolve({[field]:true});await work;
      assert.equal(cached[field],true,"the confirmed write refreshes shared settings");
      assert.equal(card.outerHTML,"unsaved","an earlier reply cannot erase a newer draft");
      assert.equal(replacement.outerHTML,"new page","a reply cannot repaint a replacement card");
      if(change==="edit")assert.equal(nodes[id].checked,false);
      assert.equal(button.disabled,false);
    }
  }
});

test("Mac processing exposes every independent implemented graph while preserving the enabled choice", () => {
  const requirements=[];let enabled;let graduation;
  const card=new Function("setCard","cardHead","togRow","devReq","devGraduation","setCardFoot",
    `${shippedSource("macosVideoProcessingCard")} return macosVideoProcessingCard;`)(
      value=>value,()=>"",(id,title,detail,choice)=>{enabled=choice;return detail;},
      (readiness,item,id,title,detail)=>{requirements.push({id,title,detail});return detail;},
      waiting=>{graduation=waiting;return waiting;},()=>"");
  const html=card({macos_video_processing_enabled:true},{items:[]});
  assert.equal(enabled,true,"missing compatibility never overrides saved enable");
  for(const id of ["effective_encoder","sdr_scale","hdr10_metal","hlg_metal","subtitle_burns","deinterlace","dolby_vision","live_upload"])
    assert.ok(requirements.some(row=>row.id===id),id);
  assert.match(graduation,/moving-field deinterlacing, strict Dolby Vision and Live TV/);
  assert.match(html,/Live TV keeps H264 output/);
  assert.equal(requirements.find(row=>row.id==="subtitle_burns").title,"SDR, HDR10 and HLG subtitle burns");
  assert.match(requirements.find(row=>row.id==="deinterlace").detail,/HDR interlaced sources keep the existing processing path/);
  assert.doesNotMatch(html,/Dolby Vision, HLG, burns and interlaced sources retain their existing routes/);
});

test("HTTPS card keeps exact node revision, env ownership, public facts and a token-free IPv6 link",()=>{
  const url=new Function("location",`${shippedSource("httpsConnectionUrl")} return httpsConnectionUrl;`)({href:"http://user:secret@[::1]:32400/path?token=SECRET#/settings/developer"});
  assert.equal(url({active:{enabled:true,port:32443}}),"https://[::1]:32443/");
  assert.equal(url({active:{enabled:false,port:32443}}),null);
  assert.equal(url({active:{enabled:true,port:32443}},"https://media.example:8443/settings?token=SECRET"),"https://media.example:8443/");
  assert.equal(url({active:{enabled:true,port:32443}},"https://media.example/settings"),"https://media.example/");
  const render=new Function("esc","setCard","cardHead","devGraduation","httpsConnectionUrl",`${shippedSource("httpsSettingsCard")} return httpsSettingsCard;`)(esc,value=>value,title=>title,(waiting,then)=>waiting+then,url);
  const value={node_id:'node"local',revision:"9007199254740993",saved:{enabled:false,port:32443,cert_file:"",key_file:"",sans:["localhost"]},desired:{enabled:true,port:32443,cert_file:"",key_file:"",sans:["localhost"]},active:{enabled:false,port:32443,cert_file:"",key_file:"",sans:["localhost"]},restart_required:true,overrides:["port"],durability_confirmed:false};
  const html=render(value);
  assert.match(html,/node&quot;local/);assert.match(html,/9007199254740993/);
  assert.match(html,/id="https-port"[^>]* disabled/);assert.match(html,/id="https-enabled"[^>]* checked/);
  assert.match(html,/require this node to restart/);assert.match(html,/server or inside its container/);
  assert.match(html,/directory sync/);assert.match(html,/physical-device qualification/);assert.match(html,/Settings → System/);
  assert.doesNotMatch(html,/PRIVATE KEY|token=|Download active/);
  value.active.enabled=true;value.certificate={mode:"generated",expires_at_seconds:1900000000,sha256:"abc",sans:["localhost"]};
  assert.match(render(value),/Download active public certificate chain/);
  assert.match(render(value),/unverified HTTPS address/);
});

test("HTTPS reads cannot overwrite a newer successful PUT or paint errors after a newer PUT",async()=>{
  for(const failure of [false,true]){
  let resolveRead,rejectRead,request;
  const before={node_id:"node-one",revision:"9007199254740993",desired:{enabled:false,port:32443,cert_file:"",key_file:"",sans:["localhost"]},overrides:["port"]};
  const saved={...before,revision:"9007199254740994"};
  const mount={innerHTML:"old"};const error={textContent:""};
  const nodes={"settings-https":mount,"https-error":error,"https-enabled":{checked:true},"https-port":{value:"1"},"https-cert":{value:""},"https-key":{value:""},"https-sans":{value:"localhost"}};
  const harness=new Function("document","api","settingsCurrent","applyHttpsSettings","toast",`
    let SETTINGS_HTTPS_EPOCH=0;const PAGE_RENDER_GENERATION=1;
    const SETTINGS_DATA={https:arguments[5]},SETTINGS_LOADED=new Set(),SETTINGS_LOADS=new Map();
    const SETTINGS_ENDPOINTS={https:()=>arguments[6]()};
    ${shippedSource("loadSettingsKey")}
    ${shippedSource("saveHttpsSettings")}
    return {read:()=>loadSettingsKey("https",1),save:saveHttpsSettings,data:SETTINGS_DATA};
  `)({getElementById:id=>nodes[id]},async(path,options)=>{assert.equal(path,"/system/https");request=options.body;return saved;},()=>true,value=>{mount.innerHTML=value.revision;},()=>{},before,()=>new Promise((done,reject)=>{resolveRead=done;rejectRead=reject;}));
  const oldRead=harness.read().catch(error=>error);
  await harness.save({disabled:false});
  assert.equal(request.revision,"9007199254740993");assert.equal(request.node_id,"node-one");
  assert.equal(request.desired.port,32443,"env-owned input must be sent unchanged");
  assert.equal(request.desired.enabled,true);
  if(failure)rejectRead(Error("obsolete GET failed"));else resolveRead(before);
  const stale=await oldRead;
  assert.equal(stale.staleHttps,true);assert.equal(harness.data.https,saved);assert.equal(mount.innerHTML,saved.revision);
  }
});

test("HTTPS obsolete PUT completions cannot replace a newer reload or save",async()=>{
  for(const saveAgain of [false,true])for(const failure of [false,true]){
    const before={node_id:"node-one",revision:"9007199254740993",desired:{enabled:false,port:32443,cert_file:"",key_file:"",sans:["localhost"]},overrides:[]};
    const committed={...before,revision:"9007199254740994",desired:{...before.desired,enabled:true}};
    const reloaded=saveAgain?committed:{...before,node_id:"node-two",revision:"0",desired:{...before.desired,port:34443}};
    const saved={...committed,revision:"9007199254740995",desired:{...committed.desired,enabled:false,port:32444}};
    const effects=[],mount={innerHTML:"initial"},error={textContent:""};
    const nodes={"settings-https":mount,"https-error":error,"https-enabled":{checked:true},"https-port":{value:"32443"},"https-cert":{value:""},"https-key":{value:""},"https-sans":{value:"localhost"}};
    const button=name=>{let disabled=false;return Object.defineProperty({},"disabled",{get:()=>disabled,set:value=>{disabled=value;effects.push(["button",name,value]);}});};
    let resolveOld,rejectOld,calls=0;
    const harness=new Function("document","api","settingsCurrent","applyHttpsSettings","toast",`
      let SETTINGS_HTTPS_EPOCH=0;const PAGE_RENDER_GENERATION=1;
      const SETTINGS_DATA={https:arguments[5]},SETTINGS_LOADED=new Set(["https"]),SETTINGS_LOADS=new Map();
      const SETTINGS_ENDPOINTS={https:async()=>arguments[6]};
      ${shippedSource("loadSettingsKey")}
      ${shippedSource("loadHttpsCard")}
      ${shippedSource("reloadHttpsSettings")}
      ${shippedSource("saveHttpsSettings")}
      return {save:saveHttpsSettings,reload:reloadHttpsSettings,data:SETTINGS_DATA,epoch:()=>SETTINGS_HTTPS_EPOCH};
    `)({getElementById:id=>nodes[id]},(path,options)=>{
      assert.equal(path,"/system/https");assert.equal(options.method,"PUT");
      if(++calls===1)return new Promise((resolve,reject)=>{resolveOld=resolve;rejectOld=reject;});
      assert.equal(options.body.node_id,"node-one");assert.equal(options.body.revision,committed.revision);
      assert.equal(options.body.desired.port,32444);return Promise.resolve(saved);
    },()=>true,value=>{
      effects.push(["paint",value.node_id,value.revision]);
      mount.innerHTML=value.node_id+":"+value.revision+":"+value.desired.port;
      nodes["https-enabled"].checked=value.desired.enabled;nodes["https-port"].value=String(value.desired.port);
      error.textContent="";
    },message=>effects.push(["toast",message]),before,reloaded);
    const oldButton=button("old"),oldSave=harness.save(oldButton);
    await harness.reload(button("reload"));
    let latest=reloaded;
    if(saveAgain){
      nodes["https-enabled"].checked=false;nodes["https-port"].value="32444";
      await harness.save(button("new"));latest=saved;
    }
    assert.equal(harness.data.https,latest);
    const markup=mount.innerHTML,epoch=harness.epoch(),priorEffects=effects.slice();
    error.textContent="newer operation status";
    if(failure)rejectOld(Object.assign(Error("obsolete PUT failed"),{status:409}));else resolveOld(committed);
    await oldSave;
    assert.equal(harness.data.https,latest,"obsolete PUT must not replace the newer node/revision/settings");
    assert.equal(mount.innerHTML,markup,"obsolete PUT must not repaint the current card");
    assert.equal(harness.epoch(),epoch,"obsolete PUT must not supersede newer operations");
    assert.deepEqual(effects,priorEffects,"obsolete PUT must not paint, toast or modify buttons");
    assert.equal(error.textContent,"newer operation status","obsolete rejection must not replace newer feedback");
    assert.equal(calls,saveAgain?2:1);
  }
});

test("HTTPS isolated loader ignores late navigation and reports its own failure",async()=>{
  let resolve,generation=1,paints=[];
  const load=new Function("loadSettingsKey","settingsCurrent","applyHttpsSettings",`${shippedSource("loadHttpsCard")} return loadHttpsCard;`)(
    ()=>new Promise(done=>{resolve=done;}),expected=>expected===generation,(...args)=>paints.push(args));
  const pending=load(1);generation=2;resolve({node_id:"old"});await pending;assert.equal(paints.length,0);
  const fail=new Function("loadSettingsKey","settingsCurrent","applyHttpsSettings",`${shippedSource("loadHttpsCard")} return loadHttpsCard;`)(
    async()=>{throw Error("local read failed");},()=>true,(...args)=>paints.push(args));
  await fail(2);assert.deepEqual(paints,[[null,"local read failed"]]);
});

test("Developer keeps replicated settings error and Retry while loading only its independent HTTPS card",async()=>{
  const body={innerHTML:""};let localLoads=0,current=true,painted=0;
  const load=new Function("location","settingsManifest","loadSettingsKey","settingsCurrent","document","esc","httpsSettingsMount","loadHttpsCard","setPageFailure","setPagePhase","SETTINGS_DATA",`${shippedSource("loadSettingsTab")} return loadSettingsTab;`)(
    {hash:"#/settings/developer"},()=>({required:["settings"],secondary:["developerReadiness","https"]}),async()=>{throw Error("replicated settings unavailable");},()=>current,
    {getElementById:()=>body},esc,()=>'<div id="settings-https">local card</div>',async()=>{localLoads++;painted++;},()=>{},()=>{},{});
  assert.equal(await load(1,"developer"),false);assert.equal(localLoads,1);assert.equal(painted,1);
  assert.match(body.innerHTML,/replicated settings unavailable/);assert.match(body.innerHTML,/retrySettingsTab/);assert.match(body.innerHTML,/settings-https/);
  current=false;await load(1,"developer");assert.equal(localLoads,1,"stale required failure must never mount local controls");
});

test("HTTPS save conflicts require explicit reload and certificate download stays authenticated on this origin",async()=>{
  const nodes={"settings-https":{},"https-error":{textContent:""},"https-enabled":{checked:false},"https-port":{value:"32443"},"https-cert":{value:""},"https-key":{value:""},"https-sans":{value:"localhost"}};
  let calls=0;
  const save=new Function("document","api","settingsCurrent",`
    let SETTINGS_HTTPS_EPOCH=0;const PAGE_RENDER_GENERATION=1;
    const SETTINGS_DATA={https:{node_id:"addressed-node",revision:"1",desired:{port:32443},overrides:[]}};
    ${shippedSource("saveHttpsSettings")} return saveHttpsSettings;`)(
      {getElementById:id=>nodes[id]},async()=>{calls++;throw Object.assign(Error("changed elsewhere"),{status:409});},()=>true);
  const button={disabled:false};await save(button);
  assert.equal(calls,1);assert.equal(button.disabled,false);assert.match(nodes["https-error"].textContent,/Reload this node/);
  let clicked=0,revoked,route,options;
  const link={click(){clicked++;}};
  const download=new Function("api","settingsCurrent","document","URL","PAGE_RENDER_GENERATION",`${shippedSource("downloadHttpsCertificate")}
return downloadHttpsCertificate;`)(
    async(path,request)=>{route=path;options=request;return {blob:async()=>"PUBLIC CERTIFICATE"};},()=>true,
    {createElement:()=>link,getElementById:id=>nodes[id]},
    {createObjectURL:blob=>{assert.equal(blob,"PUBLIC CERTIFICATE");return "blob:local-certificate";},revokeObjectURL:url=>{revoked=url;}},1);
  await download({disabled:false,isConnected:true});
  assert.equal(route,"/system/https/certificate");assert.deepEqual(options,{raw:true});
  assert.equal(clicked,1);assert.equal(link.href,"blob:local-certificate");assert.equal(revoked,link.href);
  assert.equal(link.download,"plurx-https-certificate.pem");
});

test("HTTPS unavailable card retries independently of replicated settings",async()=>{
  let calls=0;const paints=[];const value={node_id:"local-node",revision:"5"};
  const retry=new Function("applyHttpsSettings",`
    let SETTINGS_HTTPS_EPOCH=0;const PAGE_RENDER_GENERATION=1;
    const SETTINGS_DATA={},SETTINGS_LOADED=new Set(),SETTINGS_LOADS=new Map();
    const SETTINGS_ENDPOINTS={https:async()=>{if(++arguments[1].calls===1)throw Error("local unavailable");return arguments[2];}};
    const settingsCurrent=()=>true;
    ${shippedSource("loadSettingsKey")}
    ${shippedSource("loadHttpsCard")}
    ${shippedSource("reloadHttpsSettings")}
    return {first:()=>loadHttpsCard(1),retry:reloadHttpsSettings,data:SETTINGS_DATA};
  `)((...args)=>paints.push(args),{calls},value);
  await retry.first();assert.deepEqual(paints,[[null,"local unavailable"]]);
  await retry.retry({disabled:false});assert.deepEqual(paints,[[null,"local unavailable"],[value]]);
  assert.equal(retry.data.https,value);
});

test("display targeting remains editable with unavailable or unmet advice",()=>{
  const panels=developerPanels();
  for(const readiness of [{unavailable:"offline"},{items:[{id:"dolby_vision_display_target",requirements:[{id:"dv_target_helper",status:"unmet",evidence:"helper absent"}]}]}]){
    for(const enabled of [false,true]){
      const html=panels.developerPanel({dolby_vision_display_target:enabled},readiness);
      assert.match(html,new RegExp(`TOG:dv-display-target\\|[^|]*\\|[^|]*\\|checked=${enabled}`));
      assert.match(html,/FOOT:saveDisplayTargetHdr10/);
      assert.match(html,/creative trims \(CM v4 L8, CM v2\.9 L2\)/);
      assert.match(html,/every shot, some shots or none/);
      assert.match(html,/Pre-encodes and retained VOD stay master-only/);
    }
  }
  assert.doesNotMatch(shippedSource("displayTargetHdr10Card"),/disabled/);
});
