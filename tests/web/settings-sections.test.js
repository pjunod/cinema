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
    "api", "document", "cacheSettings", "toast", "setCardSaved", "SERVER", "SETTINGS", "verifiedDecodeCard",
    // The newline matters: a shipped function may be followed by a line
    // comment, and `shippedSource` returns everything up to the next
    // declaration. Without it the injected `return` lands inside that comment
    // and the composed source silently returns nothing.
    `${shippedSource(fn)}\nreturn ${fn};`,
  )(
    async (path, opts) => { writes[fn] = { path, body: opts.body }; return {}; },
    { getElementById: (id) => { assert.ok(ids.includes(id), `${fn} reads ${id}`); return { value: "v", checked: true, textContent: "" }; } },
    (v) => v, () => {}, () => {}, {}, {}, () => "",
  );
  const defaults = ["pal", "psl", "psm", "perr"];
  // The two switches that are off on purpose moved to Developer, so Streaming
  // no longer writes them: a card that saves a field it does not show can turn
  // something back on that an operator deliberately turned off.
  const streaming = ["prr", "pabr", "phr", "phb", "pha", "pvod", "pvlr", "pvws", "pvmb", "pvbg", "serr"];
  const developer = ["pcpv1", "dverr"];
  // `vdcard` is read too: this handler replaces its own card rather than
  // re-rendering the panel, because the four cards beside it stage unsaved
  // edits. The handler's own catch would swallow a missing-id assertion, so
  // the id has to be listed here for the guard to mean anything.
  const verifiedDecode = ["dhqa", "dhqerr", "vdcard"];
  const experimental = ["phs", "dxerr"];
  return Promise.all([
    run("savePlaybackDefaults", defaults)({ disabled: false }),
    run("saveStreaming", streaming)({ disabled: false }),
    run("saveDeveloper", developer)({ disabled: false }),
    run("saveVerifiedDecode", verifiedDecode)({ disabled: false }),
    run("saveExperimental", experimental)({ disabled: false }),
  ]).then(() => {
    assert.deepEqual(Object.keys(writes.savePlaybackDefaults.body).sort(), ["default_audio_lang", "default_sub_lang", "sub_mode"]);
    assert.deepEqual(Object.keys(writes.saveStreaming.body).sort(), [
      "hls_ahead_max_secs", "hls_burst_secs", "hls_readrate", "playback_auto_abr",
      "stream_readrate", "vod_block_budget_secs", "vod_blocked_get_cap",
      "vod_live_recovery", "vod_materialize_budget_secs", "vod_presentation",
      "vod_working_set_bytes",
    ]);
    assert.deepEqual(Object.keys(writes.saveDeveloper.body).sort(), ["playback_control_protocol_v1"]);
    // Its own card, its own field. The verified-decode request renames cached
    // transcodes on covered paths, so it must never ride along with a save an
    // operator made for something else.
    assert.deepEqual(Object.keys(writes.saveVerifiedDecode.body).sort(), ["decoder_health_qualified_artifacts"]);
    assert.equal(writes.saveVerifiedDecode.path, "/settings");
    assert.deepEqual(Object.keys(writes.saveExperimental.body).sort(), ["hls_typeless_sliding"]);
    assert.equal(writes.savePlaybackDefaults.path, "/settings");
    assert.equal(writes.saveStreaming.path, "/settings");
    assert.equal(writes.saveDeveloper.path, "/settings");
  });
});

test("Developer is where the switches that cost something live", () => {
  const panel = new Function(
    "setHead", "setCard", "cardHead", "togRow", "setCardFoot", "esc",
    // Joined with newlines, never bare interpolation: `shippedSource` here
    // stops at the next `\nfunction `, so a fragment can end inside a trailing
    // `//` comment and swallow whatever follows it.
    [
      shippedSource("preparedHandoffEnabled"), shippedSource("liveTvSettingsCard"),
      shippedSource("verifiedDecodeCard"), shippedSource("decodeRecoveryCard"),
      shippedSource("developerPanel"), "return developerPanel;",
    ].join("\n"),
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
  );
  const html = panel({ playback_control_protocol_v1: true, hls_typeless_sliding: false });
  for (const id of ["pcpv1", "phs", "dhqa"]) {
    assert.match(html, new RegExp(`TOG:${id}\\|`), `Developer is missing the ${id} switch`);
  }
  assert.match(html, /FOOT:saveDeveloper/);
  assert.match(html, /FOOT:saveExperimental/);
  assert.match(html, /Enable prepared quality handoff/);
  assert.match(html, /encoder: staged/);
  assert.match(html, /twenty consecutive commits/);
  assert.match(html, /Android and web remain unqualified/);
  assert.match(html, /no separate hidden server flag/);
  // The prepared card says what has to be true *and whether it is*, because
  // a requirement an operator cannot check is a requirement they will skip.
  // None of it gates the toggle: the switch is in the card above and this one
  // has no input at all.
  assert.match(html, /What must be true first, and whether it is/);
  assert.match(html, /The server primes the successor it stages/);
  assert.match(html, /503 media_owner_transition/);
  assert.match(html, /nothing on this card prevents you enabling it now/);
  // The card is advisory AND it carries the switch. Those are not in tension:
  // the list says what enabling costs and whether each part is true, and
  // nothing in it disables the control. A page that refuses to let an operator
  // turn something on tells them less than one that says what will happen.
  const preparedCard = html
    .slice(html.indexOf("Enable prepared quality handoff"))
    .split("Experimental delivery")[0];
  assert.match(preparedCard, /TOG:pdp\|/, "the prepared card carries the enable switch");
  assert.doesNotMatch(preparedCard, /FOOT:/,
    "…and no save: the switch is this browser's, not a server setting");
  assert.doesNotMatch(preparedCard, /disabled/,
    "nothing in the readiness list disables it");
  const off = panel({ playback_control_protocol_v1: false, hls_typeless_sliding: false });
  assert.match(html, /The control endpoint is advertised<small>[\s\S]*?<span class="pill" style="color:var\(--good\)/);
  assert.match(off, /The control endpoint is advertised<small>[\s\S]*?<span class="pill warn">not met<\/span>/);
  assert.match(html, /Enable cluster transport recovery/);
  assert.match(html, /there is no hidden production feature flag/);
  assert.match(html, /Keep a ready voter majority/);
  assert.match(html, /\/cluster\/transport\/sqlite/);
  assert.match(html, /twenty learner plus twenty voter recovery cycles/);
  // The section says what it is for, so a capability that costs something has
  // somewhere honest to land rather than being buried under Streaming. The
  // transport is always compiled and automatic; this must not imply a gate.
  assert.match(html, /compiled in and activates automatically/);
  assert.doesNotMatch(html, /special build/);
  // The switch has to be wired to something. A control that renders and does
  // nothing is worse than no control: it reports a capability to the operator
  // that the server never hears about.
  assert.match(html, /TOG:pdp\|[^|]*\|[^|]*\|checked=false\|onchange="setPreparedHandoff\(this\.checked\)"/,
    "the prepared-handoff switch reflects the stored state and sets it");
  assert.match(html, /dual_player_preparation/,
    "…and says which field it sets, because that is the whole of Gate A");
  assert.match(html, /Nothing above blocks this switch/);
  // Readiness pills, counted rather than matched, because "not met" contains
  // "met": an assertion that only looks for the word cannot tell a met row from
  // an unmet one, and would pass with the two renderings swapped.
  const pills = html.match(/>(met|not met|partly met)<\/span>/g) || [];
  const counted = (word) => pills.filter((pill) => pill === `>${word}</span>`).length;
  assert.ok(counted("not met") >= 2,
    "the unmet requirements say so beside the switch — the staged route has no worker");
  assert.ok(counted("met") >= 2, "…and the ones this page checked and found true say that");
  assert.ok(counted("partly met") >= 1, "…and a half-answered one is not rounded either way");
  // Automatic decode recovery. The section exists because the effort that
  // built the recovery was required to say what safe enablement depends on,
  // and the honest answer today starts with "it cannot fire yet".
  assert.match(html, /Automatic decode recovery/);
  assert.match(html, /There is no switch here/);
  assert.match(html, /reopen loop/);
  // The tripwire. This sentence is true only while no contract is qualified
  // against a hardware decoder; when one is, this card is wrong and this
  // assertion is what says so.
  assert.match(html, /Not true on any node today/);
  assert.match(html, /One recovery per playback, and it is never given back/);
  assert.match(html, /qualify diagnostic contracts against this node's measured hardware decoders/);
  assert.match(html, /HDHomeRun Live TV/);
  assert.match(html, /Save the configuration, check readiness, then enable/);
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
  assert.match(bare, /checks above are advisory and never disable this control/);
  assert.doesNotMatch(bare, /This node can honour the request/);
  // Why the feature exists at all, in the words the failure actually takes.
  assert.match(bare, /drop every frame of a file and still exit successfully/);
  assert.equal((bare.match(/✗/g) || []).length, 3, "three unmet checks, each shown");
  assert.match(bare, /Not requested on this node\./);

  // Requested and enabled, with the uncovered state still shown as advice.
  const refused = card({ decoder_health_qualified_artifacts: true, decoder_health_qualification: {
    namespace: "decoder-plan-v1-unqualified", enforcing: false, policy_enabled: true, eligible: false,
    measured_build: "ffmpeg version 5.1.9", measured_decoders: ["h264/software/h264", "hevc/software/hevc"],
    covered_decoders: [], refusal: "no_contract_covers_this_build",
    explanation: "No retained diagnostic contract covers this node's FFmpeg build under the qualified log flags.",
  } });
  assert.match(refused, /Enabled · covered paths/);
  assert.match(refused, /Coverage advisory/);
  assert.match(refused, /No retained diagnostic contract covers/);
  assert.match(refused, /<code>h264\/software\/h264<\/code>/, "what it did measure is still shown");
  assert.equal((refused.match(/✓/g) || []).length, 2);
  assert.equal((refused.match(/✗/g) || []).length, 1);

  // In force.
  const on = card({ decoder_health_qualified_artifacts: true, decoder_health_qualification: {
    namespace: "decoder-plan-v1-health-qualified-r1", enforcing: true, policy_enabled: true, eligible: true,
    measured_build: "ffmpeg version 9.0.1", measured_decoders: ["h264/software/h264"],
    covered_decoders: ["h264/software/h264"], refusal: null, explanation: null,
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
    measured_build: "ffmpeg version 9.0.1", measured_decoders: ["h264/software/h264"],
    covered_decoders: ["h264/software/h264"], refusal: "not_requested",
    explanation: "Not requested on this node.", pending_restart: true,
  } });
  assert.match(pending, /Saved · restart to apply/);
  assert.match(pending, /applies the request when it next starts/);
  assert.match(pending, /move an affected key space under work already in flight/);
  assert.doesNotMatch(pending, /every cache key/);

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

test("Automatic recovery requires a covered hardware and software pair for one codec", () => {
  const card = new Function(
    "setCard", "cardHead", "esc",
    `${shippedSource("decodeRecoveryCard")}\nreturn decodeRecoveryCard;`,
  )(
    (body) => `CARD[${body}]`,
    (title, sub, tools) => `CARDHEAD:${title}|${sub || ""}|${tools || ""}`,
    esc,
  );

  const crossed = card({ decoder_health_qualification: {
    measured_build: "ffmpeg version 9.0.1",
    covered_decoders: ["h264/software/h264", "hevc/videotoolbox/hevc"],
  } });
  assert.match(crossed, /✗ The same codec has covered hardware and software paths/);
  assert.match(crossed, /different codecs do not form a recovery/);

  const paired = card({ decoder_health_qualification: {
    measured_build: "ffmpeg version 9.0.1",
    covered_decoders: ["h264/software/h264", "h264/videotoolbox/h264"],
  } });
  assert.match(paired, /✓ The same codec has covered hardware and software paths/);
  assert.match(paired, /h264\/videotoolbox\/h264/);
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
