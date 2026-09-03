"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const inputContract = require("./player-input-contract.json");

// The pure policy module is only half of the Auto counter: index.html decides
// which instant identifies a stall episode before it ever reaches
// `recordStallEpisode`. Calling the policy helper directly cannot see that
// wiring, so the counter regressions below run the shipped UI's own function.
const SHIPPED_UI = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/index.html"),
  "utf8",
);

// Every function these tests borrow is declared at column zero in one inline
// <script>, so the next top-level `function` is a reliable terminator and no
// brace/string parsing is needed. A rename fails loudly rather than silently
// testing nothing.
const DECLARATIONS = ["\nfunction ", "\nasync function "];
// A slice ends at the next top-level declaration of ANY kind, not only the next
// function. A `const` table sitting between two functions would otherwise be
// swallowed by whichever function precedes it, and a harness that also asks for
// that table by name gets "already declared" — a confusing failure about the
// slicer rather than about the code under test.
// `window.`/`document.` are not declarations, but they are the other thing that
// appears at column zero in this script: top-level listener registration. A
// function followed by one (closePlayer is) would otherwise be sliced together
// with every handler after it, and those RUN at build time — the harness fails
// on an undefined `window` instead of on the function under test.
const TERMINATORS = DECLARATIONS.concat([
  "\nconst ",
  "\nlet ",
  "\nwindow.",
  "\ndocument.",
]);
function sliceDeclaration(start) {
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = TERMINATORS.map((kind) => rest.indexOf(kind, 1)).filter(
    (at) => at !== -1,
  );
  const end = ends.length ? Math.min(...ends) : -1;
  return (end === -1 ? rest : rest.slice(0, end)).trimEnd();
}
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) =>
    SHIPPED_UI.indexOf(`${kind}${name}(`),
  ).find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  return sliceDeclaration(start);
}

// Deliberately NOT `shippedSource`. The rescue-collision regression has to be
// able to run against a build with no guard at all, or reverting the correction
// would fail it on a missing declaration instead of on the two sessions it
// opens — a name check dressed up as a behaviour check. A build that names the
// guard differently but keeps one automatic session-open still passes, which is
// the contract that actually matters.
function shippedSourceIfPresent(name) {
  const declared = DECLARATIONS.some((kind) =>
    SHIPPED_UI.includes(`${kind}${name}(`),
  );
  return declared ? shippedSource(name) : "";
}

// Rescue paths are async and interleave, so they cannot be judged by a
// synchronous call. Registered here and drained in order at the end of the
// file; a rejection is left unhandled exactly like a synchronous failure, so
// the process still exits nonzero.
const ASYNC_TESTS = [];
function asyncTest(name, run) {
  ASYNC_TESTS.push([name, run]);
}

function test(name, run) {
  try {
    run();
    process.stdout.write(`PASS ${name}\n`);
  } catch (error) {
    error.message = `${name}: ${error.message}`;
    throw error;
  }
}

test("playback info exposes and remembers the shared three-mode contract", () => {
  for (const mode of ["mini", "standard", "debug"]) {
    assert.match(
      SHIPPED_UI,
      new RegExp(`data-stats-mode=["']${mode}["']`),
      `${mode} must remain selectable in the shipped player`,
    );
  }
  assert.match(SHIPPED_UI, /localStorage\.setItem\("plurx_stats_mode",mode\)/);
  assert.match(SHIPPED_UI, /patchPlaybackInfoRows\(body,STATS_MODE,contractRows/);
  for (const tone of ["good", "warn", "bad", "muted"]) {
    assert.match(
      SHIPPED_UI,
      new RegExp(`\\.statsov \\.stat-${tone}`),
      `${tone} playback diagnostics must have a visible text treatment`,
    );
  }
  assert.match(SHIPPED_UI, /function statsRunwayTone\(seconds,suspended\)/);
  assert.match(SHIPPED_UI, /function statsRateTone\(rate,ahead,suspended,final\)/);
});

test("Activity renders explicit lease and demand-window instrumentation", () => {
  // The Stream cell reads the lease and the demand window off the session and
  // paints them as a state pill, a Server ahead meter against its target, and
  // a Lease/Demand/Policy row set behind the disclosure — the run-on sentence
  // is gone, the facts are not.
  const helpers = new Function(
    `${shippedSource("esc")}\n${shippedSource("clockFromSec")}\n${shippedSource("fmtBytes")}\n${shippedSource("fmtMbps")}\n` +
      `${shippedSource("activityMethodLabel")}\n${shippedSource("activityStreamState")}\n${shippedSource("activityStreamMeters")}\n` +
      `${shippedSource("activityStreamDetails")}\n${shippedSource("activityStreamCell")}\n` +
      "return {activityStreamState,activityStreamMeters,activityStreamDetails,activityStreamCell};",
  )();
  const session = {
    lease_mode: "explicit",
    lease_state: "active",
    lease_timeout_ms: 30_000,
    control_demand: "active",
    reported_position_ms: 12_000,
    client_runway_ms: 8_000,
    production_policy: "explicit_demand",
    production_ahead_seconds: -3,
    production_target_seconds: 18,
  };
  assert.deepEqual(helpers.activityStreamState(session), { cls: "bad", label: "Behind", why: "3 s deficit" });
  assert.deepEqual(helpers.activityStreamMeters({ method: "transcode" }, session), [
    { k: "Position", v: "0:12" },
    { k: "Demand window", v: "−3 s", of: "of 18 s", tone: "bad", bar: 0 },
    { k: "Client runway", v: "8 s", tone: "warn" },
  ]);
  assert.deepEqual(helpers.activityStreamDetails(session).slice(0, 4), [
    ["Lease", "explicit · 30 s · active"],
    ["Demand", "active"],
    ["Policy", "explicit demand"],
    ["Target", "18 s"],
  ]);
  const cell = helpers.activityStreamCell({ method: "transcode", presentation: "live-recovery", session_id: "s1" }, session, new Set());
  assert.match(cell, /<span class="stream-state bad">Behind <span class="why">· 3 s deficit<\/span><\/span>/);
  assert.match(cell, /<span class="k">Demand window<\/span><span class="v">−3 s<span class="of">of 18 s<\/span>/);
  assert.doesNotMatch(cell, /production deficit 3s|explicit lease 30s/);
});

test("playback info explicitly separates playback mode from delivery method", () => {
  const modes = new Function(
    `${shippedSource("playbackModeName")}\n${shippedSource("playbackModeDetail")}\nreturn {playbackModeName,playbackModeDetail};`,
  )();
  assert.equal(modes.playbackModeName({ vod: true }), "VOD HLS");
  assert.equal(modes.playbackModeName({ sessionId: "live-1" }), "Live HLS");
  assert.equal(modes.playbackModeName({}), "Progressive file");
  assert.match(modes.playbackModeDetail({ vod: true }), /fixed, seekable timeline/);
  assert.match(modes.playbackModeDetail({ sessionId: "live-1" }), /growing recovery timeline/);
  const stats = shippedSource("playbackStatsTelemetry");
  assert.match(stats, /playback_mode:playbackModeDetail/);
  assert.match(stats, /method,playback_mode/);
});

// `openSession` reaches two more shipped helpers than it used to, and every
// caller of it here has to hand them the same fakes — otherwise the difference
// between two cases is the harness rather than the behaviour.
const USABLE_CAPS_DOCUMENT = Object.freeze({
  v: 2,
  client: { kind: "web", build: "v0.3.0-466" },
  video: [{ codec: "hevc", present: ["sdr", "pq"], dv_profiles: [5, 8] }],
  audio: ["aac"],
  containers: ["mp4", "mkv"],
});
// For harnesses that slice `openSession` for something other than its caps:
// the two shipped helpers, over a stub document.
const CAPS_DOCUMENT_PRELUDE = [
  `const PLAY_CAPS=${JSON.stringify({ vcodec: "hevc", dvprofile: "5,8" })};`,
  "function decodeLimits(){return {};}",
  `function capsDocument(){return ${JSON.stringify(USABLE_CAPS_DOCUMENT)};}`,
  shippedSource("currentCapsDocument"),
  shippedSource("capsDocumentIsUsable"),
].join("\n");
function buildOpenSession(overrides) {
  const options = Object.assign(
    {
      api: async () => ({}),
      newRequestId: () => "request-1",
      vodClientContract: () => ({
        session: { presentation: "vod", block_budget_secs: 8 },
        fragLoadPolicy: {},
      }),
      PLAYER: {},
      capsDocument: () => USABLE_CAPS_DOCUMENT,
      PLAY_CAPS: { vcodec: "hevc,hevc10", dvprofile: "5,8" },
      decodeLimits: () => ({}),
    },
    overrides || {},
  );
  const build = new Function(
    "api",
    "newRequestId",
    "vodClientContract",
    "PLAYER",
    "capsDocument",
    "PLAY_CAPS",
    "decodeLimits",
    [
      'const PLAYBACK_ID="playback-1";',
      shippedSource("currentCapsDocument"),
      shippedSource("capsDocumentIsUsable"),
      shippedSource("openSession"),
      "return {openSession};",
    ].join("\n"),
  );
  return build(
    options.api,
    options.newRequestId,
    options.vodClientContract,
    options.PLAYER,
    options.capsDocument,
    options.PLAY_CAPS,
    options.decodeLimits,
  );
}

asyncTest("every web HLS session requests the bounded VOD presentation", async () => {
  const requests = [];
  let requestId = 0;
  const { openSession } = buildOpenSession({
    api: async (url, options) => { requests.push({ url, options }); return { vod: true }; },
    newRequestId: () => `request-${++requestId}`,
    PLAYER: { controlReporter: { sequence: 17 } },
  });

  await Promise.all([
    openSession(7, { start: 12, height: null }),
    // Callers cannot silently turn the central presentation contract off.
    openSession(8, { presentation: "live", block_budget_secs: 60 }),
  ]);
  assert.equal(requests.length, 2);
  for (const request of requests) {
    assert.equal(request.options.body.presentation, "vod");
    assert.equal(request.options.body.block_budget_secs, 8);
    assert.match(request.options.body.request_id, /^request-/);
    assert.equal(request.options.body.control_sequence, 17);
  }
  assert.equal("height" in requests[0].options.body, false);
});

asyncTest("every web create carries the capabilities its plan was derived from", async () => {
  const requests = [];
  const capsCalls = [];
  const limits = { "hevc/main10/3840x2160/10/pq/60": { lost: 12 } };
  const { openSession } = buildOpenSession({
    api: async (url, options) => { requests.push({ url, options }); return { vod: true }; },
    capsDocument: (caps, seen) => { capsCalls.push([caps, seen]); return USABLE_CAPS_DOCUMENT; },
    decodeLimits: () => limits,
  });

  // The remux open, field for field from the copy-HLS path.
  await openSession(70, {
    copy: true, aac: false, preserve_dolby_vision: true,
    start: 0, audio: 0, audio_offset_ms: 0,
  });

  assert.equal(requests.length, 1);
  const body = requests[0].options.body;
  // Without this the create lands in the server's `legacy_trusted` arm, which
  // derives `convert_dolby_vision` for a build that enumerated nothing — the
  // straggler population `plan_derivation.legacy_trusted` counts, and the web
  // player is the last member of it.
  assert.deepEqual(body.caps, USABLE_CAPS_DOCUMENT);
  // …and everything the body already said still says it. A create that gained
  // caps and lost its echo would be re-derived from the document alone, which
  // is a different plan rather than a better-evidenced one — `/decision`'s
  // force, and Apple's compatible-base retry, both live in the echo.
  assert.equal(body.copy, true);
  assert.equal(body.preserve_dolby_vision, true);
  assert.equal(body.aac, false);
  assert.equal(body.audio, 0);

  // The same question `/decision` asked, argument for argument. They are one
  // helper precisely so they cannot drift; this asserts the helper is what
  // ran, and that `askDecision` still calls it rather than rebuilding its own.
  assert.equal(capsCalls.length, 1);
  assert.deepEqual(capsCalls[0][0], { vcodec: "hevc,hevc10", dvprofile: "5,8" });
  assert.equal(capsCalls[0][1], limits);
});

asyncTest("the decision and the create it acts on ask one question", async () => {
  // Not a grep for `currentCapsDocument()` in `askDecision`: that passes
  // against a build where the helper is dead. This RUNS both requests through
  // one set of fakes and compares the two bodies.
  //
  // The two halves of the question are the document and the force. A create
  // that sends the document but not the force is re-derived under
  // `Force::Auto` while the decision was taken under the viewer's actual
  // choice — and on `Quality → Original` for a title above this browser's
  // HEVC ceiling that flips `preserve_dolby_vision` to false, stripping Dolby
  // Vision from the one request that explicitly asked for the original.
  const requests = [];
  const api = async (url, options) => { requests.push({ url, options }); return {}; };
  const shipped = new Function(
    "api",
    "newRequestId",
    "vodClientContract",
    "PLAYER",
    "capsDocument",
    "PLAY_CAPS",
    "decodeLimits",
    "prePlaySelectionQuery",
    "decisionUrl",
    "qualityForce",
    [
      'const PLAYBACK_ID="playback-1";',
      shippedSource("currentCapsDocument"),
      shippedSource("capsDocumentIsUsable"),
      shippedSource("askDecision"),
      shippedSource("openSession"),
      "return {askDecision,openSession};",
    ].join("\n"),
  )(
    api,
    () => "request-1",
    () => ({ session: { presentation: "vod", block_budget_secs: 8 } }),
    {},
    () => USABLE_CAPS_DOCUMENT,
    { vcodec: "hevc", dvprofile: "5,8" },
    () => ({}),
    () => "",
    (id) => `/files/${id}/decision?legacy`,
    () => "original",
  );

  await shipped.askDecision(70, "original", null);
  await shipped.openSession(70, { copy: true, preserve_dolby_vision: true });
  assert.equal(requests.length, 2);
  const [decision, create] = requests;
  assert.match(decision.url, /\/files\/70\/decision\?/);
  assert.deepEqual(
    create.options.body.caps,
    decision.options.body.caps,
    "the create must act on the document the decision was taken from",
  );
  assert.match(decision.url, /force=original/);
  assert.equal(
    create.options.body.overrides && create.options.body.overrides.force,
    "original",
    "…and on the same force, or the server re-derives under Auto",
  );

  // Auto is the absence of a force, on both sides. Sending `force=auto` would
  // put an `override force=auto` note on every ordinary create and move the
  // `overridden` counter for nothing.
  requests.length = 0;
  const auto = new Function(
    "api", "newRequestId", "vodClientContract", "PLAYER",
    "capsDocument", "PLAY_CAPS", "decodeLimits", "qualityForce",
    [
      'const PLAYBACK_ID="playback-1";',
      shippedSource("currentCapsDocument"),
      shippedSource("capsDocumentIsUsable"),
      shippedSource("openSession"),
      "return {openSession};",
    ].join("\n"),
  )(
    api, () => "request-1",
    () => ({ session: { presentation: "vod", block_budget_secs: 8 } }),
    {}, () => USABLE_CAPS_DOCUMENT, { vcodec: "hevc" }, () => ({}),
    () => "auto",
  );
  await auto.openSession(70, { copy: true });
  assert.equal("overrides" in requests[0].options.body, false);
});

asyncTest("a create never sends an empty capabilities document", async () => {
  const requests = [];
  const { openSession } = buildOpenSession({
    api: async (url, options) => { requests.push({ url, options }); return { vod: true }; },
    // What a browser whose probe answered nothing would produce. The server
    // counts an empty document as `unusable_caps` and returns NO review, so
    // sending it is strictly worse than sending none — which still gets the
    // conversion derived.
    capsDocument: () => ({ v: 2, video: [], audio: [], containers: [] }),
  });
  await openSession(70, { copy: true });
  assert.equal("caps" in requests[0].options.body, false);

  // …and only an EMPTY one. A browser that enumerated no containers has still
  // told the server which codecs it decodes, and that document is worth
  // re-deriving from. (This is also what fails when the predicate's `||`
  // becomes `&&`, which every other case here survives.)
  const usable = new Function(
    shippedSource("capsDocumentIsUsable") + "\nreturn capsDocumentIsUsable;",
  )();
  assert.equal(usable({ v: 2, video: [{ codec: "hevc" }], audio: [], containers: [] }), true);
  assert.equal(usable({ v: 2, video: [], audio: ["aac"], containers: [] }), true);
  assert.equal(usable({ v: 2, video: [], audio: [], containers: ["mp4"] }), true);
  assert.equal(usable({ v: 2, video: [], audio: [], containers: [] }), false);
  assert.equal(usable(null), false);
});

test("the document this browser actually builds is one the server can read", () => {
  // The guard above is a backstop, and a backstop nobody can reach is worth
  // saying so about: `buildPlayCaps` seeds `containers` unconditionally, so
  // the real `capsDocument` cannot produce an empty document however the
  // probe answers. This is the assertion that would go red if that changed —
  // at which point the guard stops being decoration and starts being load
  // bearing, and either way somebody finds out here rather than from a
  // `plan_derivation.unusable_caps` counter climbing on the fleet.
  const built = new Function(
    "SERVER",
    "navigator",
    "document",
    "window",
    "displayIsHdr",
    [
      shippedSource("buildPlayCaps"),
      shippedSource("capsDocument"),
      shippedSource("capsDocumentIsUsable"),
      "return {buildPlayCaps,capsDocument,capsDocumentIsUsable};",
    ].join("\n"),
  )(
    { build: "v0.3.0-466" },
    { userAgent: "test" },
    // The bleakest browser this code can meet: no `<video>` to ask, no
    // MediaSource, no HDR display, and a synchronous HEVC ladder that found
    // nothing. `buildPlayCaps` still seeds H.264, AAC/MP3 and the base
    // container list, which is why the guard is a backstop rather than a
    // branch anyone reaches.
    { createElement: () => { throw new Error("no DOM"); } },
    {},
    () => false,
  );
  for (const hevc of [
    { depth8: false, depth10: false, pq10: false, maxheight: null },
    { depth8: true, depth10: true, pq10: true, maxheight: 2160 },
  ]) {
    const doc = built.capsDocument(built.buildPlayCaps(hevc), {});
    assert.equal(
      built.capsDocumentIsUsable(doc),
      true,
      `the shipped document must always be worth sending: ${JSON.stringify(doc)}`,
    );
  }
});

test("the caps document stays bounded now that every create carries it", () => {
  // `openSession` runs on every seek and every audio switch. The server keeps
  // at most 256 learned limits and clips each label to 160, so anything past
  // that is bytes nobody reads — and the route's 64 KiB body limit is what
  // they would eventually run into.
  const capsDocument = new Function(
    "SERVER",
    "navigator",
    `${shippedSource("capsDocument")}\nreturn capsDocument;`,
  )({ build: "v0.3.0-466" }, { userAgent: "x".repeat(500) });
  const limits = {};
  for (let i = 0; i < 400; i += 1) {
    limits[`identity-${i}`] = { label: "L".repeat(400), lost: 1, secs: 60, rate: 2, at: i };
  }
  const doc = capsDocument({ vcodec: "hevc", acodec: "aac", container: "mp4" }, limits);
  assert.equal(doc.learned_limits.length, 256);
  assert.equal(doc.learned_limits[0].label.length, 160);
  assert.equal(
    doc.learned_limits[0].identity,
    "identity-399",
    "at the cap it keeps the newest, which are the ones still describing this machine",
  );
  assert.equal(doc.client.ua.length, 160);
  assert.ok(
    JSON.stringify(doc).length < 64 * 1024,
    `a capped document must fit the create route's body limit: ${JSON.stringify(doc).length} bytes`,
  );
});

test("the VOD fetch contract stays below hls.js and beyond the producer watchdog", () => {
  const contract = new Function(
    `${shippedSource("vodClientContract")}\nreturn vodClientContract();`,
  )();
  assert.equal(contract.session.presentation, "vod");
  assert.equal(contract.session.block_budget_secs, 8);
  assert.equal(contract.fragLoadPolicy.default.maxTimeToFirstByteMs, 10_000);
  assert.equal(contract.fragLoadPolicy.default.maxLoadTimeMs, 120_000);
  assert.ok(
    contract.session.block_budget_secs * 1000
      <= contract.fragLoadPolicy.default.maxTimeToFirstByteMs - 2_000,
    "the server must answer before the browser aborts the request",
  );
  assert.ok(
    contract.fragLoadPolicy.default.errorRetry.maxNumRetry >= 7,
    "503 backoff must outlive the 30-second producer watchdog with margin",
  );
  assert.match(
    shippedSource("attachHls"),
    /fragLoadPolicy:vodClientContract\(\)\.fragLoadPolicy/,
    "every HLS response uses the VOD materialization policy",
  );
  assert.doesNotMatch(
    shippedSource("attachHls"),
    /LEVEL_LOADING[\s\S]*_keeperFires/,
    "the removed live-playlist keeper must not survive the VOD-only cutover",
  );
});

asyncTest("a temporary live recovery presentation remains playable", async () => {
  const { openSession } = buildOpenSession({
    api: async () => ({ vod: false }),
    vodClientContract: () => ({ session: { presentation: "vod", block_budget_secs: 8 } }),
    PLAYER: null,
  });
  const started = await openSession(42, { copy: true });
  assert.equal(started.vod, false);
});

test("an initial VOD refusal stays visible instead of closing the player", () => {
  const play = shippedSource("play");
  assert.match(play, /showSessionOpenFailure\(e\)/);
  assert.doesNotMatch(
    play,
    /openSession[\s\S]{0,500}return closePlayer\(\)/,
    "a typed VOD refusal must remain on the playback surface",
  );
});

test("VOD diagnostics describe materialization instead of claiming a cache hit", () => {
  const status = new Function(
    "health",
    `${shippedSource("vodServerState")}\nreturn vodServerState(health);`,
  );
  assert.equal(status(null), "VOD HLS · Waiting for demand");
  assert.equal(status({ producer_state: "running" }), "VOD HLS · Materializing");
  assert.equal(
    status({ producer_state: "held", producer_hold: "working_set" }),
    "VOD HLS · Holding working set",
  );
  assert.equal(
    status({ producer_state: "held", producer_hold: "ahead" }),
    "VOD HLS · Holding ahead window",
  );
  assert.equal(status({ producer_state: "complete" }), "VOD HLS · Complete");
  assert.doesNotMatch(status({ producer_state: "complete" }), /cache/i);
});

test("analysis controls are first-class settings separate from playback mode controls", () => {
  const panel = shippedSource("playbackPanel");
  // Playback saves per card: the player defaults and the streaming tuning are
  // two writes with two buttons, and neither carries an analysis field.
  const save = shippedSource("saveStreaming");
  const saveDefaults = shippedSource("savePlaybackDefaults");
  const analysisPanel = shippedSource("analysisSettingsPanel");
  assert.match(saveDefaults, /default_audio_lang:/);
  assert.match(saveDefaults, /sub_mode:/);
  assert.doesNotMatch(saveDefaults, /vod_presentation:|hls_readrate:|stream_readrate:/,
    "the defaults card never writes a streaming field");
  assert.doesNotMatch(save, /default_audio_lang:|default_sub_lang:|sub_mode:/,
    "the streaming card never writes a player default");
  assert.doesNotMatch(saveDefaults, /vod_index_mins:/);
  const saveAnalysis = shippedSource("saveAnalysisSettings");
  assert.match(panel, /togRow\("pvod"/);
  assert.match(panel, /togRow\("pvlr"/);
  assert.doesNotMatch(panel, /"pvi"/);
  assert.match(analysisPanel, /id="an-every"/);
  assert.match(analysisPanel, /togRow\("an-enabled"/);
  assert.match(panel, /id="pvws"/);
  assert.match(panel, /id="pvmb"/);
  assert.match(save, /vod_presentation:/);
  assert.match(save, /vod_live_recovery:/);
  assert.doesNotMatch(save, /vod_index_mins:/);
  assert.match(saveAnalysis, /vod_index_mins:/);
  assert.match(saveAnalysis, /vod_index_cluster_cache:/);
  assert.match(save, /vod_materialize_budget_secs:/);
  assert.match(save, /vod_block_budget_secs:"8"/);
  assert.match(panel, /VOD HLS/);
  assert.match(panel, /Live HLS/);
});

test("analysis settings render the numeric retry policy returned by the API", () => {
  const esc = new Function(`${shippedSource("esc")}\nreturn esc;`)();
  const presetOpts = new Function(
    "esc",
    `${shippedSource("presetOpts")}\nreturn presetOpts;`,
  )(esc);
  const render = new Function(
    "analysisSummaryCard",
    "presetOpts",
    "esc",
    [
      shippedSource("setHead"), shippedSource("setCard"), shippedSource("cardHead"),
      shippedSource("togRow"), shippedSource("setCardFoot"), shippedSource("analysisSettingsPanel"),
    ].join("\n") + "\nreturn analysisSettingsPanel;",
  )(() => "summary", presetOpts, esc);

  const html = render(
    {
      vod_index_cluster_cache: true,
      vod_index_mins: 15,
      analysis_max_attempts: 3,
      analysis_lease_secs: 90,
      analysis_backoff_base_secs: 7,
      analysis_backoff_max_secs: 77,
    },
    { enabled: true },
  );

  assert.match(html, /id="an-attempts"[^>]*value="3"/);
  assert.match(html, /id="an-lease"[^>]*value="90"/);
  assert.match(html, /id="an-backoff-base"[^>]*value="7"/);
  assert.match(html, /id="an-backoff-max"[^>]*value="77"/);
});

test("estimated skip markers are hedged without rebuilding each tick", () => {
  let writes = 0;
  const markerEvents = [];
  const skip = {
    dataset: {},
    value: "",
    get innerHTML() {
      return this.value;
    },
    set innerHTML(value) {
      writes += 1;
      this.value = value;
    },
  };
  const player = { autoskip: false, _markerOffers: new Set() };
  const renderSkip = new Function(
    "document",
    "esc",
    "PLAYER",
    "clientLog",
    `${shippedSource("markerIsEstimated")}
${shippedSource("markerAutoSkipEligible")}
${shippedSource("renderSkip")}
return renderSkip;`,
  )(
    { getElementById: (id) => (id === "pskip" ? skip : null) },
    (value) => value,
    player,
    (event) => markerEvents.push(event),
  );
  const exact = {
    kind: "credits",
    start_ms: 6_000,
    chapter: true,
  };
  renderSkip(exact);
  assert.equal(
    skip.innerHTML,
    '<button onclick="skipCurrent()">Skip Credits ›</button>',
  );
  renderSkip(exact);
  assert.equal(writes, 1, "the exact marker should not rebuild on timeupdate");
  assert.deepEqual(markerEvents.map((event) => event.event), ["marker_offer"]);

  const estimated = { ...exact, chapter: false, provenance: "estimated" };
  renderSkip(estimated);
  assert.equal(
    skip.innerHTML,
    '<button onclick="skipCurrent()">Skip Credits · Estimated ›</button>',
  );
  assert.equal(markerEvents.length, 1, "one boundary is offered once per playback");
  renderSkip(estimated);
  assert.equal(writes, 2, "the estimated marker should not rebuild on timeupdate");

  const manual = {
    ...exact,
    start_ms: 7_000,
    chapter: false,
    provenance: "manual",
    confidence: 1_000,
  };
  renderSkip(manual);
  assert.equal(
    skip.innerHTML,
    '<button onclick="skipCurrent()">Skip Credits ›</button>',
    "manual corrections are exact even though their legacy chapter bit may be false",
  );

  player.autoskip = true;
  const ineligible = { ...estimated, start_ms: 8_000 };
  renderSkip(ineligible);
  assert.equal(
    markerEvents.at(-1).event,
    "marker_offer",
    "an ineligible marker stays manually offerable when global auto-skip is on",
  );
});

test("auto-skip only seeks exact marker provenance", () => {
  const markerAutoSkipEligible = new Function(
    `${shippedSource("markerAutoSkipEligible")}
return markerAutoSkipEligible;`,
  )();
  const skipped = [];
  const player = { autoskip: true, markers: [] };
  const checkMarkers = new Function(
    "PLAYER",
    "markerNowMs",
    "renderSkip",
    "skipMarker",
    "markerAutoSkipEligible",
    `${shippedSource("checkMarkers")}
return checkMarkers;`,
  )(
    player,
    () => 1_500,
    () => {},
    (marker, automatic) => skipped.push({ marker, automatic }),
    markerAutoSkipEligible,
  );
  const marker = (provenance, chapter = false) => ({
    kind: "credits",
    label: "Skip Credits",
    start_ms: 1_000,
    end_ms: 2_000,
    chapter,
    provenance,
  });

  player.markers = [marker("estimated")];
  checkMarkers();
  player.markers = [marker("detected")];
  checkMarkers();
  assert.equal(skipped.length, 0);

  player.markers = [marker("manual")];
  checkMarkers();
  assert.equal(skipped.length, 1);
  assert.equal(skipped[0].automatic, true);

  player.markers = [marker("authored", true)];
  checkMarkers();
  assert.equal(skipped.length, 2);
});

test("a preview is offered but never auto-skipped", () => {
  // The preference this gates is spelled "Auto-skip intro & credits" on every
  // surface that offers it. A preview is the one kind that is new footage each
  // week, so auto-skipping it would silently widen an opt-in the viewer made
  // about repeated material. The button still appears — that is the point.
  const eligible = new Function(
    `${shippedSource("markerAutoSkipEligible")}
return markerAutoSkipEligible;`,
  )();
  const marker = (kind, provenance) => ({ kind, provenance, chapter: true });

  assert.equal(eligible(marker("credits", "authored")), true);
  assert.equal(eligible(marker("intro", "authored")), true);
  assert.equal(eligible(marker("credits", "manual")), true);
  assert.equal(eligible(marker("preview", "authored")), false);
  assert.equal(eligible(marker("preview", "manual")), false);
  // Not a provenance rule — a preview from an older server without provenance
  // is refused on kind alone.
  assert.equal(eligible({ kind: "preview", chapter: true }), false);
  assert.equal(eligible({ kind: "credits", chapter: true }), true);
});

test("only a tail kind that runs to the end finishes playback", () => {
  // `kind !== "intro"` also caught `recap`, and any marker whose kind this
  // build does not recognise. Marking an episode watched and advancing to the
  // next one is not a thing to do on a kind we cannot name.
  const finished = [];
  const sought = [];
  const skipMarker = new Function(
    "PLAYER",
    "document",
    "clientLog",
    "reportProgress",
    "finishPlayback",
    "seekTo",
    `${shippedSource("skipMarker")}
return skipMarker;`,
  )(
    { durMs: 100_000, fileId: 7, markers: [] },
    { getElementById: () => null },
    () => {},
    (id, done) => finished.push({ id, done }),
    () => finished.push("finish"),
    (sec) => sought.push(sec),
  );

  const tail = (kind) => ({ kind, start_ms: 97_000, end_ms: 100_000 });

  skipMarker(tail("credits"));
  assert.deepEqual(finished, [{ id: 7, done: true }, "finish"]);

  finished.length = 0;
  skipMarker(tail("preview"));
  assert.deepEqual(finished, [{ id: 7, done: true }, "finish"]);

  // A recap at the end of a file is a mislabelled chapter, not an ending.
  finished.length = 0;
  sought.length = 0;
  skipMarker(tail("recap"));
  assert.deepEqual(finished, []);
  assert.deepEqual(sought, [100]);

  // A kind this build does not know must not silently mark an episode watched.
  finished.length = 0;
  sought.length = 0;
  skipMarker(tail("sponsor"));
  assert.deepEqual(finished, []);
  assert.deepEqual(sought, [100]);

  finished.length = 0;
  sought.length = 0;
  skipMarker({ start_ms: 97_000, end_ms: 100_000 });
  assert.deepEqual(finished, []);
  assert.deepEqual(sought, [100]);
});

test("every server verdict reaches exactly one initial web transport", () => {
  const rows = [
    [{ method: "direct_play" }, "direct"],
    [
      { method: "direct_play", selectedAudioIndex: 2 },
      "progressive_remux",
    ],
    [{ method: "remux" }, "progressive_remux"],
    [{ method: "remux", nativeHls: true }, "copy_hls"],
    [{ method: "remux", segmentedRemux: true }, "copy_hls"],
    [
      { method: "remux", nativeHls: true, segmentedRemux: true },
      "copy_hls",
    ],
    [{ method: "transcode" }, "transcode_hls"],
  ];
  for (const [input, expected] of rows) {
    assert.equal(policy.initialRoute(input), expected, JSON.stringify(input));
  }
});

test("a missing node-local VOD index falls back only where progressive remux works", () => {
  assert.equal(
    policy.indexPendingFallback({
      code: "vod_index_pending",
      method: "remux",
      nativeHls: false,
    }),
    "progressive_remux",
  );
  for (const input of [
    { code: "vod_index_pending", method: "remux", nativeHls: true },
    { code: "vod_index_pending", method: "transcode", nativeHls: false },
    { code: "producer_failed", method: "remux", nativeHls: false },
  ]) {
    assert.equal(policy.indexPendingFallback(input), "fail", JSON.stringify(input));
  }
  assert.match(
    shippedSource("startCopyHls"),
    /indexPendingFallback[\s\S]*?stream\.mp4[\s\S]*?copyHls=false/,
    "the shipped session-open path must attach the progressive response",
  );
});

test("native HLS is Safari-only unless MSE is unavailable", () => {
  assert.equal(
    policy.nativeHlsAvailable({
      canPlayNativeHls: false,
      hasWebKitPlaybackTarget: true,
      hlsJsSupported: false,
    }),
    false,
  );
  assert.equal(
    policy.nativeHlsAvailable({
      canPlayNativeHls: true,
      hasWebKitPlaybackTarget: true,
      hlsJsSupported: true,
    }),
    true,
  );
  assert.equal(
    policy.nativeHlsAvailable({
      canPlayNativeHls: true,
      hasWebKitPlaybackTarget: false,
      hlsJsSupported: true,
    }),
    false,
  );
  assert.equal(
    policy.nativeHlsAvailable({
      canPlayNativeHls: true,
      hasWebKitPlaybackTarget: false,
      hlsJsSupported: false,
    }),
    true,
  );
  assert.equal(policy.hlsTransport({ nativeHls: true, hevcCopy: true }), "native");
  assert.equal(policy.hlsTransport({ nativeHls: true, hevcCopy: false }), "mse");
  assert.equal(
    policy.hlsTransport({
      nativeHls: true,
      hevcCopy: false,
      hlsJsSupported: false,
    }),
    "native",
  );
});

test("copy-HLS audio compatibility follows the newly selected track", () => {
  assert.equal(
    policy.copyAudioNeedsTranscode({ codec: "aac", msePairSupported: true }),
    false,
  );
  assert.equal(
    policy.copyAudioNeedsTranscode({ codec: "ac3", msePairSupported: false }),
    true,
  );
  assert.equal(
    policy.copyAudioNeedsTranscode({
      codec: "ac3",
      clientAudioCodecs: ["aac", "ac3"],
      nativeHls: true,
    }),
    false,
  );
  assert.equal(
    policy.copyAudioNeedsTranscode({
      codec: "truehd",
      clientAudioCodecs: ["aac", "ac3"],
      nativeHls: true,
    }),
    true,
  );
  assert.equal(policy.copyAudioNeedsTranscode({ codec: null }), true);
});

test("manual quality and rescue height preserve the viewer's promise", () => {
  const forces = {
    auto: "auto",
    original: "original",
    nomse: "original",
    "1080": "transcode",
    "720": "transcode",
    "480": "transcode",
    "360": "transcode",
  };
  for (const [quality, force] of Object.entries(forces)) {
    assert.equal(policy.qualityForce(quality), force);
  }
  for (const height of [1080, 720, 480, 360]) {
    assert.equal(policy.transcodeHeight(String(height)), height);
  }
  for (const quality of ["auto", "original", "nomse", "future-value"]) {
    assert.equal(policy.transcodeHeight(quality), null);
  }
  assert.equal(
    policy.sessionHeight({
      quality: "original",
      sourceHeight: 2160,
      decidedMethod: "remux",
    }),
    2160,
  );
  assert.equal(
    policy.sessionHeight({
      quality: "auto",
      sourceHeight: 2160,
      decidedMethod: "remux",
      burnedSubtitle: true,
    }),
    2160,
  );
  assert.equal(
    policy.sessionHeight({
      quality: "auto",
      sourceHeight: 2160,
      decidedMethod: "remux",
      refusedOriginal: true,
    }),
    2160,
  );
  assert.equal(
    policy.sessionHeight({
      quality: "auto",
      sourceHeight: 2160,
      decidedMethod: "transcode",
      burnedSubtitle: true,
    }),
    null,
  );
});

const serverLadder = [
  { height: 1080, total_kbps: 8160, peak_kbps: 12160 },
  { height: 720, total_kbps: 4160, peak_kbps: 6160 },
  { height: 480, total_kbps: 2160, peak_kbps: 3160 },
  { height: 360, total_kbps: 1360, peak_kbps: 1960 },
];

const demandingDolbyVision = {
  container: "mkv",
  video_codec: "hevc",
  video_profile: "Main 10",
  width: 3840,
  height: 2160,
  bit_depth: 10,
  hdr: "dolby_vision",
  hdr_format: "Dolby Vision \u00b7 Profile 7 (HDR10-compatible)",
  bitrate: 90_892_368,
};

const playableHdr10 = {
  container: "mkv",
  video_codec: "hevc",
  video_profile: "Main 10",
  width: 3840,
  height: 2160,
  bit_depth: 10,
  hdr: "hdr10",
  hdr_format: "HDR10",
  bitrate: 42_000_000,
};

test("Auto starts from the server ladder, prior, and persisted last-good rung", () => {
  assert.deepEqual(
    policy.normalizedLadder(serverLadder, 700).map((rung) => rung.height),
    [360, 480],
  );
  assert.equal(
    policy.initialAutoRung({ ladder: serverLadder, persistedHeight: 480 }),
    480,
  );
  assert.equal(policy.initialAutoRung({ ladder: serverLadder }), null);
  assert.equal(
    policy.initialAutoRung({
      ladder: serverLadder,
      persistedHeight: 720,
      priorKbps: 1500,
    }),
    360,
  );
  assert.equal(
    policy.initialAutoRung({
      ladder: serverLadder,
      persistedHeight: 1080,
      playerHeight: 700,
    }),
    480,
  );
  assert.equal(
    policy.initialAutoRung({
      ladder: serverLadder,
      persistedHeight: 1080,
      playerHeight: 300,
    }),
    360,
    "a player below the ladder floor still gets the lowest rung",
  );
});

test("the player-height ceiling uses backing pixels rather than CSS pixels", () => {
  assert.equal(
    policy.playerPixelHeight({ layoutHeight: 469, devicePixelRatio: 2 }),
    938,
  );
  assert.equal(
    policy.playerPixelHeight({ layoutHeight: 469, devicePixelRatio: 1 }),
    469,
  );
  assert.equal(
    policy.playerPixelHeight({ intrinsicHeight: 720 }),
    720,
  );
});

test("an outgoing hls.js estimate survives a restart and outranks the cold prior", () => {
  assert.equal(
    policy.bandwidthSeedBps({
      outgoingEstimateBps: 3_200_000,
      priorKbps: 1500,
    }),
    3_200_000,
  );
  assert.equal(policy.bandwidthSeedBps({ priorKbps: 1500 }), 1_500_000);
  assert.equal(policy.bandwidthSeedBps({}), null);
});

test("a completed fragment provides a fresh severe-pressure estimate", () => {
  assert.equal(
    policy.transferSampleKbps({
      loadedBytes: 250_000,
      loadingStartMs: 1_000,
      loadingEndMs: 2_000,
    }),
    2_000,
  );
  assert.equal(policy.transferSampleKbps({ loadedBytes: 250_000 }), null);
});

test("learned decode identity is versioned, deterministic, and bitrate-bucketed", () => {
  const reordered = {
    bitrate: demandingDolbyVision.bitrate,
    hdr_format: demandingDolbyVision.hdr_format,
    height: demandingDolbyVision.height,
    width: demandingDolbyVision.width,
    video_profile: "  MAIN   10 ",
    video_codec: " HEVC ",
    bit_depth: demandingDolbyVision.bit_depth,
    hdr: " DOLBY_VISION ",
    ignored_future_field: "does not change v2",
  };
  const dolbyKey = policy.decodeLimitIdentity(demandingDolbyVision);
  assert.match(dolbyKey, /^decode-v2:/);
  assert.equal(policy.decodeLimitIdentity(reordered), dolbyKey);
  assert.notEqual(policy.decodeLimitIdentity(playableHdr10), dolbyKey);
  assert.equal(policy.decodeLimitIdentity({}), policy.decodeLimitIdentity({}));

  assert.equal(policy.bitrateBucket(9_999_999).index, 0);
  assert.equal(policy.bitrateBucket(10_000_000).index, 1);
  assert.equal(policy.bitrateBucket(19_999_999).index, 1);
  assert.equal(policy.bitrateBucket(20_000_000).index, 2);
  assert.equal(policy.bitrateBucket(null), null);
});

test("a learned limit applies only to the exact media load and Auto", () => {
  const nowMs = 2_000_000_000_000;
  const dolbyKey = policy.decodeLimitIdentity(demandingDolbyVision);
  const limits = {
    "hevc@2160": { lost: 20, rate: 8, secs: 150, at: nowMs - 1_000 },
    "decode-v2:not-json": { lost: 20, rate: 8, secs: 150, at: nowMs - 1_000 },
    [dolbyKey]: { lost: 20, rate: 8, secs: 150, at: nowMs - 1_000 },
  };
  const pruned = policy.pruneDecodeLimits(limits, { nowMs });
  assert.equal(pruned.changed, true);
  assert.deepEqual(Object.keys(pruned.limits), [dolbyKey]);

  assert.equal(
    policy.learnedDecodeLimitAction({
      quality: "auto",
      source: demandingDolbyVision,
      limits,
      nowMs,
    }).action,
    "apply",
  );
  assert.equal(
    policy.learnedDecodeLimitAction({
      quality: "auto",
      source: playableHdr10,
      limits,
      nowMs,
    }).action,
    "none",
  );
  for (const quality of ["original", "nomse", "1080"]) {
    assert.equal(
      policy.learnedDecodeLimitAction({
        quality,
        source: demandingDolbyVision,
        limits,
        nowMs,
      }).action,
      "bypass",
      quality,
    );
  }
});

test("clearing an exact learned limit restores the ordinary HDR remux", () => {
  const nowMs = 2_000_000_000_000;
  const dolbyKey = policy.decodeLimitIdentity(demandingDolbyVision);
  const hdrKey = policy.decodeLimitIdentity(playableHdr10);
  const limits = {
    [dolbyKey]: { lost: 20, rate: 8, secs: 150, at: nowMs - 8 * 86_400_000 },
    [hdrKey]: { lost: 16, rate: 6.4, secs: 150, at: nowMs - 1_000 },
  };
  assert.equal(
    policy.learnedDecodeLimitAction({
      quality: "auto",
      source: demandingDolbyVision,
      limits,
      nowMs,
    }).action,
    "retest",
  );

  const cleared = policy.withoutLearnedDecodeLimit(
    limits,
    demandingDolbyVision,
  );
  assert.equal(cleared.removed, true);
  assert.equal(cleared.limits[dolbyKey], undefined);
  assert.deepEqual(cleared.limits[hdrKey], limits[hdrKey]);
  assert.equal(
    policy.learnedDecodeLimitAction({
      quality: "auto",
      source: demandingDolbyVision,
      limits: cleared.limits,
      nowMs,
    }).action,
    "none",
  );
  assert.equal(
    policy.initialRoute({ method: "remux", nativeHls: true }),
    "copy_hls",
    "without the exact learned verdict, the ordinary HDR10 remux route wins",
  );
});

test("learned HDR fallback names client performance and the range loss", () => {
  const view = policy.learnedDecodeLimitView({
    source: demandingDolbyVision,
    limit: { lost: 20, rate: 8, secs: 150 },
    ordinaryRange: "hdr10",
    deliveredRange: "sdr",
  });
  assert.match(view.loadLabel, /Dolby Vision .* Profile 7/);
  assert.equal(view.rangeConsequence, "HDR10 \u2192 SDR");
  assert.match(view.reason, /learned client-performance limit/);
  assert.match(view.reason, /HDR10 \u2192 SDR/);
});

test("a bandwidth cliff drops from 1080p to the sustainable rung in one move", () => {
  const decision = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 1080,
    estimateKbps: 1500,
    runwaySeconds: 8,
    nowMs: 10_000,
    lastSwitchAtMs: 9_000,
  });
  assert.deepEqual(decision, {
    height: 360,
    reason: "bandwidth cliff",
    emergency: true,
    mildSamples: 0,
    upgradeSinceMs: null,
  });
});

test("an active supply stall spends its one restart on the floor", () => {
  const decision = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 5000,
    activeSupplyStall: true,
    nowMs: 20_000,
    lastSwitchAtMs: 19_999,
  });
  assert.equal(decision.height, 360);
  assert.equal(decision.reason, "supply stalls");
  assert.equal(decision.emergency, true);
});

test("an empty-buffer supply stall spends its one restart on the floor", () => {
  const decision = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    // The completed-fragment and stable EWMAs may both still describe the
    // pre-cliff link when the player runs out of buffered media.
    estimateKbps: 5_000,
    recentEstimateKbps: 5_000,
    recentEstimateAtMs: 9_000,
    runwaySeconds: 0.1,
    activeSupplyStall: true,
    nowMs: 10_000,
  });
  assert.equal(decision.height, 360);
  assert.equal(decision.reason, "supply stalls");
  assert.equal(decision.emergency, true);
});

test("near-empty runway spends its one restart on the floor", () => {
  const decision = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    // The stall callback can arrive after the controller already observed the
    // empty runway, so this signal must be sufficient on its own.
    estimateKbps: 5_000,
    runwaySeconds: 0.1,
  });
  assert.equal(decision.height, 360);
  assert.equal(decision.reason, "buffer ran dry");
  assert.equal(decision.emergency, true);
});

test("an emergency downgrade cannot be stranded by the player-height ceiling", () => {
  const input = {
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 1541,
    runwaySeconds: 0.1,
    activeSupplyStall: true,
    supplyStalls: 3,
  };
  for (const playerHeight of [469, 300]) {
    const decision = policy.decideRung({ ...input, playerHeight });
    assert.equal(decision.height, 360, `player height ${playerHeight}`);
    assert.equal(decision.reason, "supply stalls", `player height ${playerHeight}`);
    assert.equal(decision.emergency, true, `player height ${playerHeight}`);
  }
});

test("one cliff episode uses fresh throughput and cannot restart twice", () => {
  const first = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    // The pre-cliff EWMA still admits 480p; the completed fragment measures
    // the shaped link closely enough that only 360p is sustainable.
    estimateKbps: 2_800,
    recentEstimateKbps: 2_000,
    recentEstimateAtMs: 5_000,
    runwaySeconds: 8,
    nowMs: 10_000,
  });
  assert.equal(first.height, 360);
  assert.equal(first.reason, "bandwidth cliff");
  assert.equal(first.emergency, true);

  const duplicateSupplySignal = policy.decideRung({
    ladder: serverLadder,
    currentHeight: first.height,
    estimateKbps: 2_800,
    recentEstimateKbps: 2_000,
    recentEstimateAtMs: 5_000,
    runwaySeconds: 0.1,
    activeSupplyStall: true,
    supplyStalls: 3,
    nowMs: 10_000,
  });
  assert.equal(duplicateSupplySignal.height, 360);
  assert.equal(duplicateSupplySignal.reason, null);
  assert.equal(duplicateSupplySignal.emergency, false);

  const staleSample = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 2_800,
    recentEstimateKbps: 2_000,
    recentEstimateAtMs: 5_000,
    runwaySeconds: 8,
    nowMs: 20_001,
  });
  assert.equal(staleSample.height, 480, "a stale transfer cannot steer a switch");
});

test("three supply stalls act while decode stalls never choose a rung", () => {
  const supply = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 7000,
    supplyStalls: 3,
  });
  assert.equal(supply.height, 360);
  assert.equal(supply.emergency, true);

  const decode = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 7000,
    decodeStalls: 9,
    runwaySeconds: 20,
  });
  assert.equal(decode.height, 720);
  assert.equal(decode.reason, null);
});

test("two long supply-stall episodes do not satisfy the three-stall rescue", () => {
  let events = [];
  for (const stall of [
    { start: 5_000, duration: 3_000 },
    { start: 25_000, duration: 3_000 },
  ]) {
    events = policy.recordStallEpisode({
      events,
      episodeAtMs: stall.start,
      nowMs: stall.start + 100,
    });
    events = policy.recordStallEpisode({
      events,
      episodeAtMs: stall.start,
      nowMs: stall.start + stall.duration,
    });
  }
  assert.deepEqual(events, [5_000, 25_000]);
  assert.equal(events.length >= 3, false);

  events = policy.recordStallEpisode({
    events,
    episodeAtMs: 45_000,
    nowMs: 48_000,
  });
  assert.equal(events.length, 3, "a third real episode reaches the threshold");
});

test("the shipped counter records one event per pause reported at both edges", () => {
  let nowMs = 0;
  const noteAutoStall = new Function(
    "PlaybackPolicy",
    "performance",
    `${shippedSource("noteAutoStall")}\nreturn noteAutoStall;`,
  )(policy, { now: () => nowMs });

  const player = { abr: { stallEvents: { supply: [], decode: [] } } };
  // Two real supply pauses. hls.js raises bufferStalledError just after each
  // one begins; the video element reports the same pause at its end, three
  // seconds later. Keying on the report instant instead of the wait's start
  // turns these two pauses into four events and fires the three-stall rescue.
  for (const startedAt of [5_000, 25_000]) {
    player.waitAt = startedAt;
    nowMs = startedAt + 100;
    noteAutoStall(player, "supply", player.waitAt);
    nowMs = startedAt + 3_000;
    noteAutoStall(player, "supply", startedAt);
    player.waitAt = null;
  }

  const supplyStalls = player.abr.stallEvents.supply.length;
  assert.equal(supplyStalls, 2, "two pauses must not become four stall events");
  assert.equal(
    policy.decideRung({
      ladder: serverLadder,
      currentHeight: 720,
      estimateKbps: 7000,
      supplyStalls,
    }).emergency,
    false,
    "two supply pauses must not reach the three-episode rescue",
  );

  // A third distinct pause still reaches it, so the correction narrows the
  // counter without disarming the rescue.
  player.waitAt = 45_000;
  nowMs = 45_100;
  noteAutoStall(player, "supply", player.waitAt);
  assert.equal(player.abr.stallEvents.supply.length, 3);
  assert.equal(
    policy.decideRung({
      ladder: serverLadder,
      currentHeight: 720,
      estimateKbps: 7000,
      supplyStalls: player.abr.stallEvents.supply.length,
    }).emergency,
    true,
  );
});

test("every shipped stall report carries the wait's start as its identity", () => {
  // The counter above is only correct if its callers hand it the episode
  // instant. These are the three reporting paths review finding 1 named.
  assert.match(
    SHIPPED_UI,
    /noteAutoStall\(p,[^;]*p\.waitAt\)/,
    "the hls.js bufferStalledError report must key on the open wait",
  );
  assert.match(
    shippedSource("endWait"),
    /recordWaitStall\(p,kind,ms,runway,[^;]*,began\)/,
    "endWait must report the stall against the instant the wait began",
  );
  assert.match(
    shippedSource("persistentWait"),
    /recordWaitStall\(p,kind,ms,runway,[^;]*,began,controlTrigger\)/,
    "persistentWait must preserve the wait instant and its exact control trigger",
  );
});

// Both automatic rescues open a replacement session and both yield at that
// request before anything marks the player, so only the shipped wiring can
// answer whether two of them can be in flight at once. This runs the real
// `maybeDecodeRescue`, `rescueAutoSupply` and `autoControllerTick` against a
// session-open that stays pending until the test releases it — the ordering the
// browser actually produces, where the cheap health GET returns before the
// session-create POST.
function autoRescueHarness(player, autoAbr = true) {
  const opened = [];
  let releaseOpen = null;
  let openFails = false;
  const video = { currentTime: 12, paused: false, videoHeight: 720 };

  async function startTranscodeFallback(reason) {
    opened.push(reason);
    await new Promise((resolve) => {
      releaseOpen = resolve;
    });
    if (openFails) return; // a 5xx leaves the outgoing stream untouched
    player.method = "transcode";
  }

  const noop = () => {};
  const build = new Function(
    "PLAYER",
    "SERVER",
    "document",
    "performance",
    "PlaybackPolicy",
    "qualityForce",
    "playedSecs",
    "lostFrameRate",
    "MARGIN_CLEAR_SECS",
    "MARGIN_LOST_PER_MIN",
    "SUPPLY_RUNWAY_SECS",
    "clearDecodeLimit",
    "decodeLimitLabel",
    "decodeMarginVerdict",
    "rememberDecodeLimit",
    "clientLog",
    "toast",
    "playbackContext",
    "setLoading",
    "recordAutoSwitch",
    "startTranscodeFallback",
    "pollSessionHealth",
    "bufferRunway",
    "playerPixelHeight",
    "switchAutoRung",
    "rememberAutoRung",
    [
      shippedSourceIfPresent("claimAutoFallback"),
      shippedSourceIfPresent("releaseAutoFallback"),
      shippedSource("maybeDecodeRescue"),
      shippedSource("rescueAutoSupply"),
      shippedSource("autoControllerTick"),
      "return {maybeDecodeRescue, rescueAutoSupply, autoControllerTick};",
    ].join("\n"),
  );

  const shipped = build(
    player,
    { playback_auto_abr: autoAbr },
    { getElementById: () => video },
    { now: () => 90_000 },
    policy,
    () => "auto",
    () => 150,
    () => ({ lost: 0, rate: 0 }),
    60,
    2,
    6,
    () => false,
    () => "HEVC 2160p",
    () => ({ lost: 15, rate: 6, secs: 150, decodeMs: 91, budgetMs: 41.7 }),
    noop,
    noop,
    noop,
    () => ({}),
    noop,
    noop,
    startTranscodeFallback,
    async () => {}, // the health poll resolves before the session-create request
    () => 30,
    () => 720,
    async () => {},
    noop,
  );

  return {
    ...shipped,
    opened,
    video,
    failNextOpen() {
      openFails = true;
    },
    // Let every pending continuation run without completing the session-open.
    async settle() {
      for (let i = 0; i < 8; i += 1) await Promise.resolve();
    },
    async finishOpen() {
      assert.notEqual(releaseOpen, null, "no session-open was in flight");
      const resolve = releaseOpen;
      releaseOpen = null;
      resolve();
      await this.settle();
    },
  };
}

async function autoRungTick(autoAbr) {
  const switches = [];
  let polls = 0;
  const player = {
    method: "transcode",
    started: true,
    offset: 0,
    ladder: serverLadder,
    autoHeight: 720,
    health: { target_height: 720, recent_speed: 2 },
    hls: { bandwidthEstimate: 1_000_000 },
    abr: {
      switching: false,
      stallEvents: { supply: [], decode: [] },
      recentEstimateKbps: null,
      recentEstimateAtMs: null,
      lastStallAtMs: null,
      lastSwitchAtMs: 0,
      mildSamples: 0,
      upgradeSinceMs: null,
      previousRunway: null,
      stableSinceMs: 0,
      failedHeights: new Set(),
    },
  };
  const tick = new Function(
    "PLAYER",
    "SERVER",
    "document",
    "performance",
    "PlaybackPolicy",
    "qualityForce",
    "SUPPLY_RUNWAY_SECS",
    "pollSessionHealth",
    "rescueAutoSupply",
    "bufferRunway",
    "playerPixelHeight",
    "switchAutoRung",
    "rememberAutoRung",
    `${shippedSource("autoControllerTick")}\nreturn autoControllerTick;`,
  )(
    player,
    { playback_auto_abr: autoAbr },
    { getElementById: () => ({ currentTime: 12, paused: false, videoHeight: 720 }) },
    { now: () => 90_000 },
    policy,
    () => "auto",
    6,
    async () => { polls += 1; },
    async () => {},
    () => 1,
    () => 720,
    async (from, decision) => { switches.push([from, decision.height]); },
    () => {},
  );

  await tick();
  return { polls, switches };
}

asyncTest("the Auto switch gates an automatic rung change", async () => {
  const enabled = await autoRungTick(true);
  assert.deepEqual(enabled.switches, [[720, 360]], "the enabled controller still acts");

  const disabled = await autoRungTick(false);
  assert.equal(disabled.polls, 0, "off must return before sampling controller health");
  assert.deepEqual(disabled.switches, [], "off must not change the playback rung");
});

test("the disabled Auto controller leaves every manual ladder rung available", () => {
  const build = new Function(
    "PLAYER",
    "SERVER",
    "PlaybackPolicy",
    [
      shippedBinding("const", "QUALITY_MODES"),
      shippedSource("qualityOptions"),
      "return qualityOptions();",
    ].join("\n"),
  );
  const options = build(
    { ladder: serverLadder },
    { playback_auto_abr: false },
    policy,
  );

  assert.deepEqual(
    options.map(([value]) => value),
    ["auto", "original", "nomse", "1080", "720", "480", "360"],
  );
});

function pressuredRemuxPlayer() {
  return {
    method: "remux",
    started: true,
    offset: 0,
    decodeRescued: false,
    decodeRetest: false,
    hitches: { back: 5, drop: 5, gap: 5, fps: 24, decodeMs: 91 },
    source: { codec: "hevc", height: 2160 },
    bufferLimits: null,
    autoFallbackInFlight: false,
    // Three supply episodes inside the 60 s window: the rescue is armed.
    abr: {
      switching: false,
      supplyRescued: false,
      stallEvents: { supply: [40_000, 60_000, 80_000], decode: [] },
      lastStallAtMs: 80_000,
      lastSwitchAtMs: 0,
      stableSinceMs: 0,
      switches: [],
    },
  };
}

asyncTest(
  "a decode verdict and the third supply stall in one interval open one session",
  async () => {
    const player = pressuredRemuxPlayer();
    const h = autoRescueHarness(player);

    // Exactly the shipped 5-second interval: maybeDecodeRescue() is not awaited,
    // and autoControllerTick() runs straight after it.
    h.maybeDecodeRescue();
    const tick = h.autoControllerTick();
    await h.settle();

    assert.deepEqual(
      h.opened,
      ["decode-rescue"],
      "a supply rescue must not open a second session behind an in-flight decode rescue",
    );
    assert.equal(
      player.abr.supplyRescued,
      false,
      "the refused supply rescue must not consume its one-shot latch",
    );

    await h.finishOpen();
    await tick;
    assert.deepEqual(h.opened, ["decode-rescue"]);
    assert.equal(
      player.autoFallbackInFlight,
      false,
      "the claim must be released once the session-open settles",
    );
  },
);

asyncTest(
  "a decode verdict during an in-flight supply rescue opens no second session",
  async () => {
    const player = pressuredRemuxPlayer();
    const h = autoRescueHarness(player);

    // The other order: the supply rescue wins the interval, and the decode
    // verdict lands on the next sample while its session-open is still pending.
    const rescue = h.rescueAutoSupply();
    await h.settle();
    h.maybeDecodeRescue();
    await h.settle();

    assert.deepEqual(
      h.opened,
      ["auto-supply"],
      "the decode rescue must not open a session behind an in-flight supply rescue",
    );
    assert.equal(
      player.decodeRescued,
      false,
      "the refused decode rescue must not consume its one-shot latch",
    );

    await h.finishOpen();
    await rescue;
    assert.deepEqual(h.opened, ["auto-supply"]);
    assert.equal(player.autoFallbackInFlight, false);
  },
);

asyncTest("a failed automatic session-open releases the claim", async () => {
  const player = pressuredRemuxPlayer();
  const h = autoRescueHarness(player);
  h.failNextOpen();

  h.maybeDecodeRescue();
  await h.settle();
  assert.deepEqual(h.opened, ["decode-rescue"]);
  await h.finishOpen();

  assert.equal(player.method, "remux", "the failed open must leave the stream");
  assert.equal(
    player.autoFallbackInFlight,
    false,
    "a 5xx must not wedge every later automatic rescue",
  );

  // The supply rescue that was refused while the decode rescue was in flight
  // can now run, so the guard costs nothing once the failure is known.
  const rescue = h.rescueAutoSupply();
  await h.settle();
  assert.deepEqual(h.opened, ["decode-rescue", "auto-supply"]);
  await h.finishOpen();
  await rescue;
});

test("mild pressure needs two samples plus cooldown, dwell, and switch gain", () => {
  const first = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 4500,
    nowMs: 60_000,
    lastSwitchAtMs: 0,
  });
  assert.equal(first.height, 720);
  assert.equal(first.mildSamples, 1);

  const cooling = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 4500,
    mildSamples: 1,
    nowMs: 59_999,
    lastSwitchAtMs: 0,
  });
  assert.equal(cooling.height, 720);

  const second = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 4500,
    mildSamples: 1,
    nowMs: 60_000,
    lastSwitchAtMs: 0,
  });
  assert.equal(second.height, 480);
  assert.equal(second.reason, "bandwidth pressure");

  const marginal = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 5350,
    mildSamples: 1,
    nowMs: 60_000,
    lastSwitchAtMs: 0,
  });
  assert.equal(marginal.height, 720, "the restart costs more than the gain");
});

test("a slow server with draining runway is actionable before a stall", () => {
  const decision = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 9000,
    recentSpeed: 0.8,
    runwaySeconds: 7,
    previousRunwaySeconds: 10,
    mildSamples: 1,
    nowMs: 60_000,
    lastSwitchAtMs: 0,
  });
  assert.equal(decision.height, 480);
  assert.equal(decision.reason, "server supply");
});

test("recovery holds for 45 seconds, moves up once, and respects pixel height", () => {
  const first = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 16_000,
    runwaySeconds: 20,
    nowMs: 60_000,
    lastSwitchAtMs: 0,
  });
  assert.equal(first.height, 720);
  assert.equal(first.upgradeSinceMs, 60_000);

  const upgrade = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 16_000,
    runwaySeconds: 20,
    nowMs: 105_000,
    lastSwitchAtMs: 0,
    upgradeSinceMs: first.upgradeSinceMs,
  });
  assert.equal(upgrade.height, 1080);
  assert.equal(upgrade.reason, "bandwidth recovered");

  const dwell = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 720,
    estimateKbps: 16_000,
    runwaySeconds: 20,
    nowMs: 115_000,
    lastSwitchAtMs: 105_000,
    upgradeSinceMs: 70_000,
  });
  assert.equal(dwell.height, 720, "recovery cannot upgrade twice in 60 seconds");

  const capped = policy.decideRung({
    ladder: serverLadder,
    currentHeight: 480,
    estimateKbps: 16_000,
    runwaySeconds: 20,
    playerHeight: 700,
    nowMs: 120_000,
    lastSwitchAtMs: 0,
    upgradeSinceMs: 70_000,
  });
  assert.equal(capped.height, 480, "the 720p rung exceeds the player");
});

test("a rejected cheap stream gets one compatibility transcode", () => {
  assert.equal(policy.fallbackAction({ method: "direct_play" }), "transcode");
  assert.equal(policy.fallbackAction({ method: "remux" }), "transcode");
  assert.equal(
    policy.fallbackAction({ method: "remux", alreadyTried: true }),
    "fail",
  );
  assert.equal(policy.fallbackAction({ method: "transcode" }), "fail");
  assert.equal(
    policy.fallbackAction({ method: "remux", playbackIsReal: true }),
    "fail",
  );
  assert.equal(
    policy.fallbackAction({ method: "remux", mediaFailure: false }),
    "fail",
  );
});

test("a persistent stall gets one bounded method-aware recovery", () => {
  assert.equal(
    policy.stallRecoveryAction({ method: "remux", quality: "auto" }),
    "transcode",
  );
  for (const quality of ["original", "nomse", "1080"]) {
    assert.equal(
      policy.stallRecoveryAction({ method: "remux", quality }),
      "restart",
      quality,
    );
  }
  for (const method of ["direct_play", "transcode"]) {
    assert.equal(
      policy.stallRecoveryAction({ method, quality: "auto" }),
      "restart",
      method,
    );
  }
  assert.equal(
    policy.stallRecoveryAction({
      method: "remux",
      quality: "auto",
      alreadyRecovered: true,
    }),
    "prompt",
  );
  assert.equal(policy.stallRecoveryAction({ method: "unknown" }), "prompt");
});

test("a persistent supply stall spends its one restart on the sustainable rung", () => {
  assert.equal(
    policy.stallRecoveryTargetHeight({
      method: "transcode",
      quality: "auto",
      kind: "supply",
      ladder: serverLadder,
      currentHeight: 720,
      // hls.js still carries pre-cliff history, while the last completed
      // transfer measures the shaped link closely enough to choose 360p.
      estimateKbps: 2_800,
      recentEstimateKbps: 1_530,
      recentEstimateAtMs: 10_000,
      nowMs: 12_000,
    }),
    360,
  );
  for (const input of [
    { method: "transcode", quality: "1080", kind: "supply" },
    { method: "transcode", quality: "auto", kind: "decode" },
    { method: "remux", quality: "auto", kind: "supply" },
  ]) {
    assert.equal(
      policy.stallRecoveryTargetHeight({
        ...input,
        ladder: serverLadder,
        currentHeight: 720,
        estimateKbps: 1_530,
        nowMs: 12_000,
      }),
      null,
    );
  }
  assert.match(
    shippedSource("persistentWait"),
    /stallRecoveryTargetHeight\([\s\S]*?seekTo\(position,true,recoveryHeight\)/,
    "the shipped persistent-stall path must pass the measured target into its bound reopen",
  );
});

test("a transcode stall reopen is bound to the exact predecessor", () => {
  const ordinary = { start: 42, height: 720 };
  assert.deepEqual(
    policy.stallReopenSessionOptions({
      options: ordinary,
      forceReopen: true,
      method: "transcode",
      sessionId: "session-before-stall",
    }),
    {
      start: 42,
      height: 720,
      previous_session_id: "session-before-stall",
      reopen_reason: "stall",
    },
  );
  assert.deepEqual(ordinary, { start: 42, height: 720 }, "input stays immutable");
  assert.deepEqual(
    policy.stallReopenSessionOptions({
      options: ordinary,
      forceReopen: false,
      method: "transcode",
      sessionId: "ordinary-seek",
    }),
    ordinary,
    "viewer-directed seeks are ordinary creates",
  );
  assert.match(
    shippedSource("seekTo"),
    /stallReopenSessionOptions\([\s\S]*?sessionId:PLAYER\.sessionId/,
    "the shipped restart path must carry the typed predecessor binding",
  );
});

test("fallback swaps preserve healthy playback until the replacement exists", () => {
  assert.equal(
    policy.fallbackResetBeforeOpen({ reason: "stall-recovery" }),
    true,
  );
  assert.equal(
    policy.fallbackResetBeforeOpen({ reason: "stall-manual" }),
    true,
  );
  assert.equal(
    policy.fallbackResetBeforeOpen({ reason: "auto-supply" }),
    true,
  );
  for (const reason of ["decode-rescue", "stream-rejected", null]) {
    assert.equal(
      policy.fallbackResetBeforeOpen({ reason }),
      false,
      String(reason),
    );
  }
});

test("a persistent-stall prompt survives later waiting events", () => {
  assert.equal(
    policy.waitingOverlayAction({ started: false, stallPrompt: false }),
    "ignore",
  );
  assert.equal(
    policy.waitingOverlayAction({ started: true, stallPrompt: false }),
    "buffer",
  );
  assert.equal(
    policy.waitingOverlayAction({ started: true, stallPrompt: true }),
    "preserve_prompt",
  );
});

test("HDR subtitle burns keep the current delivery instead", () => {
  for (const deliveredRange of ["dolby_vision", "hdr10", "hlg"]) {
    assert.equal(
      policy.subtitleBurnAction({ requiresBurn: true, deliveredRange }),
      "keep_hdr",
    );
  }
  assert.equal(
    policy.subtitleBurnAction({ requiresBurn: true, deliveredRange: "sdr" }),
    "burn",
  );
  assert.equal(
    policy.subtitleBurnAction({ requiresBurn: true, deliveredRange: null }),
    "burn",
  );
  assert.equal(
    policy.subtitleBurnAction({ requiresBurn: false, deliveredRange: "hdr10" }),
    "native",
  );
});

test("directional seeks use horizontal steps only", () => {
  assert.equal(policy.seekDeltaSeconds("ArrowLeft"), -10);
  assert.equal(policy.seekDeltaSeconds("ArrowRight"), 10);
  assert.equal(policy.seekDeltaSeconds("ArrowDown"), null);
  assert.equal(policy.seekDeltaSeconds("ArrowUp"), null);
  assert.equal(policy.seekDeltaSeconds("Enter"), null);
});

test("desktop input routing matches every shared contract row", () => {
  for (const [state, row] of Object.entries(inputContract.routing.desktop)) {
    for (const [input, expected] of Object.entries(row)) {
      assert.equal(policy.routeInput("desktop", state, input), expected);
    }
  }
  assert.throws(
    () => policy.routeInput("desktop", "transport", "unknown"),
    /no route for desktop\/transport\/unknown/,
  );
});

test("preview acceleration matches the shared contract ladder", () => {
  const ladder = inputContract.steps.preview_acceleration;
  const nextThreshold = (index) => ladder[index + 1]?.from_repeat ?? 12;
  ladder.forEach((step, index) => {
    for (let repeat = step.from_repeat; repeat < nextThreshold(index); repeat += 1) {
      assert.equal(policy.previewStepSeconds(repeat), step.step_seconds);
    }
  });
});

test("the shipped player adapter applies state precedence and preview-then-commit", () => {
  const makeClasses = (...initial) => {
    const values = new Set(initial);
    return {
      contains: (name) => values.has(name),
      add: (...names) => names.forEach((name) => values.add(name)),
      remove: (...names) => names.forEach((name) => values.delete(name)),
      toggle: (name, on) => on ? values.add(name) : values.delete(name),
    };
  };
  const body = { tagName: "DIV" };
  const calls = { seek: [], play: 0, menu: 0, close: 0, fullscreen: 0, ticks: 0, blurs: [], stats: 0 };
  const control = (id) => ({ id, focus() { document.activeElement = elements[id]; }, blur() { calls.blurs.push(id); document.activeElement = body; } });
  const elements = {
    modal: { classList: makeClasses("open") },
    player: { classList: makeClasses(), contains: () => true },
    ploading: { classList: makeClasses() },
    statsov: { classList: makeClasses(), dataset: { mode: "standard" }, querySelectorAll: () => [] },
    pmenu: { classList: makeClasses("on"), querySelectorAll: () => [] },
    pseek: control("pseek"),
    pbplay: control("pbplay"),
  };
  const document = {
    body,
    activeElement: body,
    getElementById: (id) => elements[id] || null,
    querySelector: () => null,
  };
  const player = { _seekPending: null, _seekPreview: null, _seekDragging: false, _lastFocusedControl: "pbplay" };
  // Timers are parameters so the 350 ms self-commit can be fired by hand: the
  // defect this pins is a skip's debounce outliving the commit that replaced it.
  let pending = null;
  const setTimeoutStub = (fn) => { pending = fn; return 1; };
  const clearTimeoutStub = () => { pending = null; };
  let surface = "desktop";
  const build = new Function(
    "document", "PLAYER", "PlaybackPolicy", "isFullscreenAnywhere", "toggleFullscreen",
    "toggleStats", "cycleSub", "playerActivity", "pbTotalSec", "pbPosSec",
    "pbTick", "seekTo", "togglePlay", "closeMenu", "closePlayer", "coarsePointer",
    "setTimeout", "clearTimeout",
    [
      "let PLAYER_REPEAT_KEY=null; let PLAYER_REPEAT_COUNT=0; let NUDGE_T=null;",
      shippedSource("playerInputSurface"),
      shippedSource("playerSeekPending"),
      shippedSource("playerInputState"),
      shippedSource("clearPendingSeekTimer"),
      shippedSource("commitPendingSeek"),
      shippedSource("cancelPendingSeek"),
      shippedSource("nudge"),
      shippedSource("playerContractInput"),
      shippedSource("playerHotkey"),
      shippedSource("applyPlayerOutcome"),
      shippedSource("handlePlayerKeydown"),
      "return {playerInputState,playerInputSurface,playerContractInput,applyPlayerOutcome,handlePlayerKeydown};",
    ].join("\n"),
  );
  const adapter = build(
    document, player, policy, () => false, () => { calls.fullscreen += 1; },
    () => { calls.stats += 1; }, () => {}, () => {}, () => 100, () => 20,
    () => { calls.ticks += 1; }, (target) => calls.seek.push(target),
    () => { calls.play += 1; }, () => { calls.menu += 1; }, () => { calls.close += 1; },
    () => surface === "touch",
    setTimeoutStub, clearTimeoutStub,
  );
  const key = (value, extra = {}) => ({
    key: value, target: body, repeat: false, ctrlKey: false, metaKey: false, altKey: false,
    preventDefault() {}, ...extra,
  });

  adapter.handlePlayerKeydown(key("Escape"));
  assert.equal(calls.menu, 1);
  assert.equal(calls.close, 0);

  elements.pmenu.classList.remove("on");
  document.activeElement = elements.pseek;
  adapter.handlePlayerKeydown(key("ArrowRight"));
  adapter.handlePlayerKeydown(key("ArrowRight", { repeat: true }));
  adapter.handlePlayerKeydown(key("ArrowRight", { repeat: true }));
  assert.equal(player._seekPending, 50);
  assert.deepEqual(calls.seek, []);
  adapter.handlePlayerKeydown(key("Enter"));
  assert.deepEqual(calls.seek, [50]);
  assert.equal(player._seekPending, null);

  // A pending seek outranks an open panel: Escape cancels the scrub and
  // leaves the panel alone (contract §2.2 — scrub, then menu, then info).
  document.activeElement = elements.pseek;
  adapter.handlePlayerKeydown(key("ArrowLeft"));
  elements.statsov.classList.add("on");
  assert.equal(adapter.playerInputState(), "scrub");
  assert.equal(player._seekPending, 10);
  adapter.handlePlayerKeydown(key("Escape"));
  assert.equal(player._seekPending, null);
  assert.equal(calls.stats, 0);
  assert.equal(calls.close, 0);
  assert.equal(adapter.playerInputState(), "info");
  elements.statsov.classList.remove("on");

  // A pointer drag is a scrub even though the keyboard set nothing.
  document.activeElement = elements.pseek;
  player._seekDragging = true; player._seekPreview = 42;
  assert.equal(adapter.playerInputState(), "scrub");
  player._seekDragging = false; player._seekPreview = null;

  // A skip commits itself after a quiet — unless something commits or
  // cancels first, in which case the debounce must not fire over it.
  document.activeElement = body;
  adapter.handlePlayerKeydown(key("l"));
  assert.equal(player._seekPending, 30);
  assert.ok(pending, "the skip arms a self-commit");
  document.activeElement = elements.pseek;
  adapter.handlePlayerKeydown(key("ArrowRight"));
  adapter.handlePlayerKeydown(key("Enter"));
  assert.deepEqual(calls.seek, [50, 40]);
  assert.equal(pending, null, "committing the preview disarms the skip");

  document.activeElement = body;
  adapter.handlePlayerKeydown(key("j"));
  assert.equal(player._seekPending, 10);
  pending();
  assert.deepEqual(calls.seek, [50, 40, 10]);

  // Hidden chrome on a desktop keyboard still seeks (ruling 1 is a ten-foot
  // rule); the arrow does not fall through to a second listener.
  elements.player.classList.add("idle");
  assert.equal(adapter.playerInputState(), "hidden");
  adapter.handlePlayerKeydown(key("ArrowRight"));
  assert.equal(player._seekPending, 30);
  clearTimeoutStub();
  player._seekPending = null;
  elements.player.classList.remove("idle");

  // Hiding releases focus: hidden chrome that still holds it is a focus ring
  // the viewer cannot see.
  document.activeElement = elements.pbplay;
  adapter.applyPlayerOutcome("hide", { direction: "idle" });
  assert.deepEqual(calls.blurs, ["pbplay"]);
  assert.equal(adapter.playerInputState(), "hidden");
  elements.player.classList.remove("idle");

  document.activeElement = body;
  const before = { seek: [...calls.seek], play: calls.play };
  adapter.handlePlayerKeydown(key("ArrowUp"));
  assert.deepEqual(calls.seek, before.seek);
  assert.equal(calls.play, before.play);
  adapter.handlePlayerKeydown(key("f", { ctrlKey: true }));
  assert.equal(calls.fullscreen, 0);

  // A coarse pointer follows the touch table on the same page: arrows are
  // ignored there, where the desktop table seeks.
  surface = "touch";
  assert.equal(adapter.playerInputSurface(), "touch");
  adapter.handlePlayerKeydown(key("ArrowRight"));
  assert.equal(player._seekPending, null);
  surface = "desktop";

  elements.ploading.classList.add("failed");
  assert.equal(adapter.playerInputState(), "failed");
  elements.ploading.classList.remove("failed");
  elements.statsov.classList.add("on");
  assert.equal(adapter.playerInputState(), "info");
  elements.statsov.classList.remove("on");
  elements.player.classList.add("idle");
  assert.equal(adapter.playerInputState(), "hidden");
});

test("playback-info row builders follow the shared web field list in fixture order", () => {
  const rows = new Function(
    [
      shippedBinding("const", "PLAYBACK_INFO_FIELDS"),
      shippedBinding("const", "STATS_ROWS"),
      shippedSource("playbackInfoRows"),
      "return playbackInfoRows;",
    ].join("\n"),
  )();
  const telemetry = Object.fromEntries(inputContract.inputs.map((input) => [input, input]));
  for (const field of require("./playback-info-fields.json").fields) {
    telemetry[field.id] = `value:${field.id}`;
  }
  telemetry.dynamic_range_mini = "HDR10";
  for (const mode of ["mini", "standard", "debug"]) {
    const expected = require("./playback-info-fields.json").fields
      .filter((field) => field.modes.includes(mode) && (!field.available_on || field.available_on.includes("web")))
      .map((field) => field.label);
    assert.deepEqual(rows(mode, telemetry).map((row) => row.label), expected);
  }
});

test("decode rescue uses lost frames over a long window, not pipeline latency", () => {
  assert.equal(
    policy.decodeMarginVerdict(
      { back: 5, drop: 5, gap: 5, fps: 24, decodeMs: 1000 },
      149,
    ),
    null,
  );
  assert.equal(
    policy.decodeMarginVerdict(
      { back: 5, drop: 5, gap: 5, fps: 24, decodeMs: 1000 },
      180,
    ),
    null,
  );
  const verdict = policy.decodeMarginVerdict(
    { back: 5, drop: 5, gap: 5, fps: 24, decodeMs: 91.04 },
    150,
  );
  assert.deepEqual(verdict, {
    lost: 15,
    rate: 6,
    secs: 150,
    decodeMs: 91,
    budgetMs: 41.7,
  });
});

// Every reason the server can refuse a stream with, from the wire body it
// actually sends to the two lines the overlay shows. The generic sentence
// these replace — "the server couldn't build this stream — see Settings →
// Logs" — was shown for all of them alike, including for a session the server
// was still successfully starting.
test("each refusal the server names reaches the overlay as itself", () => {
  const rows = [
    // (status, body, expected title, expected fragment of the detail line)
    [
      404,
      { code: "session_gone", message: "this stream is no longer running" },
      "Playback failed to start.",
      "no longer running",
    ],
    [
      502,
      {
        code: "producer_failed",
        message: "the server's encoder exited before it produced any video (exit status: 1)",
      },
      "Playback failed to start.",
      "exit status: 1",
    ],
    [
      502,
      {
        code: "producer_ended",
        message:
          "the server's encoder ended after publishing part of this stream (progress deadline elapsed); media already listed remains available",
      },
      "Playback failed to start.",
      "already listed remains available",
    ],
    [
      502,
      {
        code: "session_failed",
        message: "the server could not build this stream: the encoder never produced any video",
      },
      "Playback failed to start.",
      "never produced any video",
    ],
    [
      501,
      {
        code: "unsupported_build",
        message: "this server's ffmpeg build has no subtitles filter",
      },
      "Playback failed to start.",
      "no subtitles filter",
    ],
    // The one that is NOT a failure: the server is still inside its own
    // hardware->software recovery and said so.
    [
      503,
      {
        code: "startup_timeout",
        message: "the server is still preparing this stream after 45s",
      },
      "Still preparing this stream…",
      "still preparing",
    ],
  ];
  for (const [status, body, title, fragment] of rows) {
    const failure = policy.parseStreamFailure({
      status,
      body: JSON.stringify(body),
    });
    assert.deepEqual(
      failure,
      { status, code: body.code, message: body.message },
      body.code,
    );
    const overlay = policy.streamFailureOverlay(failure);
    assert.equal(overlay.title, title, body.code);
    assert.ok(
      overlay.detail.toLowerCase().includes(fragment.toLowerCase()),
      `${body.code}: "${overlay.detail}" should contain "${fragment}"`,
    );
    assert.equal(overlay.retryable, body.code === "startup_timeout", body.code);
  }
});

// Only a startup that ran out of budget is retryable, and it is retryable on
// either signal — a server that has not yet migrated the code still says 503.
test("a still-starting stream is never reported as a permanent failure", () => {
  for (const failure of [
    { status: 503, code: "startup_timeout", message: "still preparing" },
    { status: 503, code: null, message: "still preparing" },
    { status: 500, code: "startup_timeout", message: "still preparing" },
  ]) {
    const overlay = policy.streamFailureOverlay(failure);
    assert.equal(overlay.retryable, true, JSON.stringify(failure));
    assert.equal(overlay.title, "Still preparing this stream…");
  }
});

// A guess is worse than the generic sentence: it explains the wrong failure
// with complete confidence. Anything unreadable falls back rather than invents.
test("an illegible refusal explains nothing rather than guessing", () => {
  const nothing = [
    { status: 502, body: "<html>502 Bad Gateway</html>" },
    { status: 502, body: "" },
    { status: 502, body: "null" },
    { status: 502, body: JSON.stringify({ code: "producer_failed" }) },
    { status: 502, body: JSON.stringify({ message: "   " }) },
    // A 200 is not a refusal at all.
    { status: 200, body: JSON.stringify({ message: "fine" }) },
  ];
  for (const input of nothing) {
    assert.equal(policy.parseStreamFailure(input), null, JSON.stringify(input));
  }
  assert.equal(policy.streamFailureOverlay(null), null);
  assert.equal(policy.streamFailureOverlay({ status: 502 }), null);
});

// The legacy `{error}` body several routes still use carries a readable
// sentence too, and dropping it would silently un-explain those routes.
test("the legacy error body is still read", () => {
  assert.deepEqual(
    policy.parseStreamFailure({
      status: 404,
      body: JSON.stringify({ error: "transcode session not found" }),
    }),
    { status: 404, code: null, message: "transcode session not found" },
  );
});

// The stale-reason trap, in its new clothes. A segment refused early in a film
// must not be produced as the confident explanation for a fatal error forty
// minutes later; the generic sentence is the honest answer there.
test("a refusal only explains a failure it is contemporary with", () => {
  const failure = {
    status: 502,
    code: "producer_failed",
    message: "the server's encoder exited before it produced any video",
    at: 1_000_000,
  };
  assert.ok(policy.streamFailureOverlay(failure, 1_000_000 + 60_000));
  assert.equal(policy.streamFailureOverlay(failure, 1_000_000 + 600_000), null);
  // An unstamped failure, or a caller that does not stamp, is still read —
  // the check may not silently discard the only reason there is.
  assert.ok(policy.streamFailureOverlay(failure));
  assert.ok(
    policy.streamFailureOverlay({ ...failure, at: undefined }, 9_999_999),
  );
});

// A 503 belongs to the playlist request that received it. hls.js may retry
// inside the same instance; once that retry loads a level, the refusal is
// stale immediately. This runs the shipped state helpers and also pins the
// LEVEL_LOADED wiring that calls them, so a later fatal cannot inherit the
// earlier "Still preparing" explanation.
test("a successful playlist retry immediately clears its 503 explanation", () => {
  const player = { hls: {} };
  let now = 1_000_000;
  const build = new Function(
    "PlaybackPolicy",
    "PLAYER",
    "Date",
    [
      "let STREAM_FAILURE=null;",
      shippedSource("clearStreamFailure"),
      shippedSource("noteStreamFailure"),
      shippedSource("clearStreamFailureFor"),
      shippedSource("currentStreamFailureOverlay"),
      "return {noteStreamFailure, clearStreamFailureFor, currentStreamFailureOverlay};",
    ].join("\n"),
  );
  const shipped = build(policy, player, { now: () => now });
  shipped.noteStreamFailure(
    503,
    JSON.stringify({
      code: "startup_timeout",
      message: "the server is still preparing this stream after 55s",
    }),
  );
  assert.equal(
    shipped.currentStreamFailureOverlay().title,
    "Still preparing this stream…",
  );

  // A destroyed predecessor's late success is not authority over this stream.
  shipped.clearStreamFailureFor({});
  assert.ok(shipped.currentStreamFailureOverlay());

  // The matching retry succeeded. A later fatal now has no stale refusal to
  // display, even though the old 90-second freshness window is still open.
  now += 1_000;
  shipped.clearStreamFailureFor(player.hls);
  assert.equal(shipped.currentStreamFailureOverlay(), null);

  // LEVEL_LOADED proves only that a retryable playlist request recovered. It
  // must not erase a terminal refusal captured from another HLS request.
  shipped.noteStreamFailure(
    502,
    JSON.stringify({
      code: "producer_failed",
      message: "the encoder exited before it produced video",
    }),
  );
  shipped.clearStreamFailureFor(player.hls);
  assert.equal(
    shipped.currentStreamFailureOverlay().detail,
    "the encoder exited before it produced video",
  );
  assert.match(
    shippedSource("attachHls"),
    /Hls\.Events\.LEVEL_LOADED[\s\S]*?clearStreamFailureFor\(hls\)/,
    "the shipped successful-level event must invalidate its own refusal",
  );
});

// `unsupported_build` is returned by the session-creation POST, before hls.js
// exists. Exercise the shipped api -> openSession -> burnSub catch path rather
// than composing policy helpers: the typed body must survive the rejected
// promise and become the persistent player overlay, not a 2.2-second toast.
asyncTest("a burn session-open refusal reaches the persistent overlay", async () => {
  const loading = [];
  const toasts = [];
  let request = null;
  const player = {
    fileId: 7,
    offset: 0,
    audio: [{ index: 0 }],
    curAudio: 0,
    subs: [{ index: 2, language: "eng" }],
    curSub: -1,
    burnedSub: null,
  };
  const video = { currentTime: 12 };
  const build = new Function(
    "PlaybackPolicy",
    "fetch",
    "document",
    "PLAYER",
    "endWait",
    "streamGeneration",
    "newAttempt",
    "teardownHls",
    "clearSubs",
    "pbSyncSubIcon",
    "setLoading",
    "subLabelFor",
    "transcodeOpts",
    "toast",
    "clientLog",
    "playbackContext",
    "armStall",
    "attachSession",
    "qualityForce",
    "sessionHeight",
    "newRequestId",
    "vodClientContract",
    "logout",
    [
      'const API="/api/v1"; let TOKEN="token", AUTH_GENERATION=0;',
      'const PLAYBACK_ID="playback-1"; let STREAM_FAILURE=null;',
      shippedSource("api"),
      // `openSession` attaches this browser's capabilities document; the burn
      // refusal under test does not care what is in it, only that building one
      // does not throw.
      CAPS_DOCUMENT_PRELUDE,
      shippedSource("openSession"),
      shippedSource("currentStreamFailureOverlay"),
      shippedSource("showSessionOpenFailure"),
      shippedSource("burnSub"),
      "return {burnSub};",
    ].join("\n"),
  );
  const shipped = build(
    policy,
    async (url, init) => {
      request = { url, init };
      return {
        status: 501,
        ok: false,
        text: async () =>
          JSON.stringify({
            code: "unsupported_build",
            message: "this server's ffmpeg build has no overlay filter",
          }),
      };
    },
    { getElementById: (id) => (id === "video" ? video : null) },
    player,
    () => {},
    () => ({ me: player, live: () => true }),
    () => {},
    () => {},
    () => {},
    () => {},
    (...args) => loading.push(args),
    () => "English bitmap",
    (start, audio) => ({
      start,
      audio,
      subtitle_burn: player.burnedSub,
    }),
    (message) => toasts.push(message),
    () => {},
    () => ({}),
    () => {},
    () => {},
    () => "auto",
    () => null,
    () => "request-1",
    () => ({session:{presentation:"vod",block_budget_secs:8}}),
    () => {},
  );

  await shipped.burnSub(2);
  assert.equal(request.url, "/api/v1/files/7/hls/sessions");
  assert.equal(request.init.method, "POST");
  assert.equal(JSON.parse(request.init.body).subtitle_burn, 2);
  assert.deepEqual(loading.at(-1), [
    true,
    "Playback failed to start.",
    "this server's ffmpeg build has no overlay filter",
  ]);
  assert.equal(
    loading.some(([on]) => on === false),
    false,
    "the persistent refusal must not be hidden after the rejected session open",
  );
  assert.equal(toasts.at(-1), "Playback failed");
});

// ---- detail-screen track facts + pre-play selection (issue #266) ----------
//
// Everything below runs the SHIPPED index.html functions. The detail screen is
// generated JavaScript, so a Rust suite can be green while the page has stopped
// naming a subtitle track or has started sending `audio=` on a request nobody
// touched — and that last one silently downgrades a direct play.

// A top-level data declaration, sliced the way shippedSource() slices a
// function. The tests then exercise the shipped table rather than a copy that
// drifts away from it.
function shippedBinding(keyword, name) {
  const at = SHIPPED_UI.indexOf(`\n${keyword} ${name}=`);
  assert.notEqual(at, -1, `index.html no longer declares ${keyword} ${name}`);
  return sliceDeclaration(at);
}

// The detail screen with no browser: format helpers that are not under test are
// stubbed, everything that decides what a viewer READS is shipped code.
function detailHarness({ decisions = {}, admin = false } = {}) {
  const requested = [];
  const build = new Function(
    "document",
    "PlaybackPolicy",
    "qualityForce",
    "CAPS_Q",
    "api",
    "fmtSize",
    "fmtDur",
    "fmtMbps",
    "ME",
    "exactWireId",
    [
      shippedSource("esc"),
      shippedSource("fmtChannels"),
      shippedSource("audioLabel"),
      shippedBinding("const", "LANGS"),
      shippedSource("langName"),
      shippedSource("subNeedsBurn"),
      shippedBinding("const", "SUB_FORMATS"),
      shippedSource("subFormat"),
      shippedSource("subFactLabel"),
      shippedSource("audioFactLabel"),
      shippedSource("trackChip"),
      shippedSource("trackFactRow"),
      shippedSource("preferredLanguageNote"),
      shippedSource("analysisFileControl"),
      shippedSource("specBlock"),
      shippedBinding("let", "PREPLAY"),
      shippedSource("prePlaySelection"),
      shippedSource("clearPrePlay"),
      shippedSource("prePlaySelectionQuery"),
      shippedSource("decisionUrl"),
      shippedSource("setPrePlay"),
      shippedSource("prePlayPickers"),
      shippedBinding("const", "PREPLAY_SCOPE_NOTE"),
      shippedSource("prePlayBurnNeeded"),
      shippedSource("prePlayApplication"),
      shippedSource("prePlayPreview"),
      "return {specBlock, analysisFileControl, prePlayPickers, setPrePlay, clearPrePlay," +
        " prePlaySelection, decisionUrl, prePlayApplication, prePlayBurnNeeded," +
        " preferredLanguageNote};",
    ].join("\n"),
  );
  const shipped = build(
    // No note element: setPrePlay's preview early-outs, which is what a click
    // on a page this test never rendered would do.
    { getElementById: () => null },
    policy,
    () => "auto",
    "vcodec=h264&acodec=aac",
    async (url) => {
      requested.push(url);
      const answer = decisions[url];
      if (!answer) throw new Error(`no stubbed decision for ${url}`);
      return answer;
    },
    () => "3.4 GB",
    () => "1h 52m",
    () => "8.1 Mb/s",
    { is_admin: admin },
    (file) => String(file.id),
  );
  return { ...shipped, requested };
}

function bookMetadataHarness() {
  const build = new Function(
    "grid",
    [
      shippedSource("esc"),
      shippedSource("bookByline"),
      shippedSource("bookEditionSection"),
      "return {bookByline, bookEditionSection};",
    ].join("\n"),
  );
  return build((items) => `<div data-editions="${items.length}"></div>`);
}

test("book bylines escape provider text and edition rows use only server relations", () => {
  const shipped = bookMetadataHarness();
  const byline = shipped.bookByline({
    kind: "book",
    author: 'A. Reader <script src="https://example.com/x.js"></script>',
  });
  assert.match(byline, /^<div class="muted"[^>]*>By A\. Reader /);
  assert.doesNotMatch(byline, /<script/);
  assert.match(byline, /&lt;script/);
  assert.equal(shipped.bookByline({ kind: "movie", author: "Wrong" }), "");
  assert.equal(shipped.bookEditionSection({ editions: [] }), "");
  assert.match(
    shipped.bookEditionSection({ editions: [{ id: 2, kind: "audiobook" }] }),
    /Other editions[\s\S]+data-editions="1"/,
  );
});

const MOVIE_FILE = {
  id: 42,
  filename: "Arrival.2016.mkv",
  available: true,
  container: "mkv",
  video_codec: "hevc",
  width: 3840,
  height: 2160,
  audio_streams: [
    { index: 0, codec: "truehd", channels: 8, language: "eng", default: true },
    { index: 1, codec: "ac3", channels: 6, language: "fra" },
  ],
  subtitle_streams: [
    {
      index: 0,
      codec: "subrip",
      language: "eng",
      forced: true,
      title: "Heptapod",
    },
    { index: 1, codec: "subrip", language: "eng", hearing_impaired: true },
    { index: 2, codec: "hdmv_pgs_subtitle", language: "fra" },
  ],
  playback_defaults: {
    audio: {
      selected_index: 0,
      preferred_language: "eng",
      preferred_language_status: "selected",
    },
    subtitle: {
      selected_index: 1,
      preferred_language: "eng",
      preferred_language_status: "selected",
    },
  },
};

test("the detail screen names every subtitle track, its format and its markers", () => {
  const html = detailHarness().specBlock(MOVIE_FILE);
  assert.match(html, /<dt>Subtitles<\/dt>/);
  // Language · format · forced/SDH — the three facts criterion 1 asks for, for
  // tracks that until now were invisible until playback started.
  assert.match(html, /English · SRT · forced · Heptapod/);
  assert.match(html, /English · SRT · SDH/);
  assert.match(html, /French · PGS/);
  // The server's chosen track, marked as such, from `playback_defaults` — not
  // re-derived here from `default` flags or admin settings.
  assert.match(
    html,
    /English · SRT · SDH · <span class="tdef">plays by default<\/span>/,
  );
  assert.equal(
    (html.match(/plays by default/g) || []).length,
    2,
    "exactly one audio and one subtitle track carry the marker",
  );
});

test("the detail screen names the supported HLS mode before playback", () => {
  const indexed = detailHarness().specBlock({ ...MOVIE_FILE, vod_index_status: "indexed" });
  assert.match(indexed, /<dt>HLS capability<\/dt><dd><span class="mode-chip vod">VOD HLS<\/span>/);
  assert.match(indexed, /Fixed, seekable timeline/);
  const pending = detailHarness().specBlock({ ...MOVIE_FILE, vod_index_status: "pending" });
  assert.match(pending, /<span class="mode-chip live">Live HLS fallback<\/span>/);
  assert.match(pending, /while VOD analysis is pending/);
  assert.match(pending, /when live recovery is enabled in Playback settings/);
  const unsupported = detailHarness().specBlock({ ...MOVIE_FILE, vod_index_status: "unsupported" });
  assert.match(unsupported, /cannot use the VOD indexer/);
  assert.match(unsupported, /Live HLS requires live recovery to be enabled/);
  const adminControl=detailHarness({admin:true}).analysisFileControl({
    ...MOVIE_FILE,available:true,vod_index_status:"unsupported",
  });
  assert.match(adminControl,/VOD analysis unsupported/);
  assert.doesNotMatch(adminControl,/Analyze now/);
});

test("a partly analysed file says so rather than reading as ready", () => {
  // A Dolby Vision title is delivered as two different byte streams, and an
  // index for one of them is not an index for the other. Before the tri-state
  // this file reported "indexed" off the stripped pipeline alone, so the
  // screen promised VOD HLS to a device that would land on Live HLS.
  const partial = detailHarness().specBlock({ ...MOVIE_FILE, vod_index_status: "partial" });
  assert.match(
    partial,
    /<dt>HLS capability<\/dt><dd><span class="mode-chip vod">VOD HLS on some devices<\/span>/,
  );
  assert.match(partial, /some of this file's delivery routes but not all/);
  assert.doesNotMatch(partial, /Fixed, seekable timeline/);

  // And it stays actionable: the control offers the build that fills the gap,
  // not the "Rebuild analysis" wording reserved for a file already complete.
  const partialControl=detailHarness({admin:true}).analysisFileControl({
    ...MOVIE_FILE,available:true,vod_index_status:"partial",
  });
  assert.match(partialControl,/VOD analysis partly built/);
  assert.match(partialControl,/Analyze now/);
  assert.doesNotMatch(partialControl,/Rebuild analysis/);
});

test("the detail screen keeps only the selected subtitle visible until expanded", () => {
  const html = detailHarness().specBlock(MOVIE_FILE);
  const disclosure = html.match(
    /<dt>Subtitles<\/dt><dd><details class="trkfold">([\s\S]+?)<\/details>/,
  )?.[1];
  assert.ok(disclosure, "multiple subtitle tracks use a native disclosure");
  assert.doesNotMatch(html, /<details class="trkfold" open>/);
  assert.match(
    disclosure,
    /^<summary><span class="trk on">English · SRT · SDH · <span class="tdef">plays by default<\/span><\/span><span class="trkmore">2 more<\/span><\/summary>/,
  );
  assert.match(disclosure, /English · SRT · forced · Heptapod/);
  assert.match(disclosure, /French · PGS/);
});

test("one subtitle track needs no expand control", () => {
  const html = detailHarness().specBlock({
    ...MOVIE_FILE,
    subtitle_streams: [MOVIE_FILE.subtitle_streams[0]],
    playback_defaults: {
      ...MOVIE_FILE.playback_defaults,
      subtitle: {
        ...MOVIE_FILE.playback_defaults.subtitle,
        selected_index: 0,
      },
    },
  });
  const subtitleRow = html.match(/<dt>Subtitles<\/dt><dd>([\s\S]+?)<\/dd>/)?.[1];
  assert.ok(subtitleRow);
  assert.equal(subtitleRow.includes('class="trkfold"'), false);
  assert.match(subtitleRow, /English · SRT · forced · Heptapod/);
});

test("the detail screen keeps only the selected audio track visible until expanded", () => {
  const html = detailHarness().specBlock(MOVIE_FILE);
  const disclosure = html.match(
    /<dt>Audio<\/dt><dd><details class="trkfold">([\s\S]+?)<\/details>/,
  )?.[1];
  assert.ok(disclosure, "multiple audio tracks use a native disclosure");
  assert.match(
    disclosure,
    /^<summary><span class="trk on">English · TRUEHD · 7\.1 · <span class="tdef">plays by default<\/span><\/span><span class="trkmore">1 more<\/span><\/summary>/,
  );
  assert.match(disclosure, /French · AC3 · 5\.1/);
});

test("one audio track needs no expand control", () => {
  const html = detailHarness().specBlock({
    ...MOVIE_FILE,
    audio_streams: [MOVIE_FILE.audio_streams[0]],
  });
  const audioRow = html.match(/<dt>Audio<\/dt><dd>([\s\S]+?)<\/dd>/)?.[1];
  assert.ok(audioRow);
  assert.equal(audioRow.includes('class="trkfold"'), false);
  assert.match(audioRow, /English · TRUEHD · 7\.1/);
});

test("a file with no subtitle tracks says so instead of showing an empty row", () => {
  const bare = {
    ...MOVIE_FILE,
    subtitle_streams: [],
    playback_defaults: {
      ...MOVIE_FILE.playback_defaults,
      subtitle: {
        selected_index: null,
        preferred_language: "eng",
        preferred_language_status: "no_tracks",
      },
    },
  };
  const html = detailHarness().specBlock(bare);
  assert.match(html, /<dt>Subtitles<\/dt><dd><div class="trks">None<\/div>/);
  // `no_tracks` must not also produce "no English subtitles": the row already
  // said it, and saying it twice reads as two different problems.
  assert.equal(html.includes("langnote"), false);
});

test("an audiobook part is not asked about subtitles it could never have", () => {
  const part = {
    id: 9,
    filename: "book-01.m4b",
    available: true,
    container: "m4b",
    audio_streams: [{ index: 0, codec: "aac", channels: 2, default: true }],
    subtitle_streams: [],
    playback_defaults: {
      audio: {
        selected_index: 0,
        preferred_language: "eng",
        preferred_language_status: "unknown",
      },
      subtitle: {
        selected_index: null,
        preferred_language: "eng",
        preferred_language_status: "no_tracks",
      },
    },
  };
  const html = detailHarness().specBlock(part);
  assert.equal(html.includes("<dt>Subtitles</dt>"), false);
  assert.equal(html.includes("preplay"), false, "nothing to choose between");
});

test("the preferred-language sentence keeps `unknown` distinct from `missing`", () => {
  const { preferredLanguageNote } = detailHarness();
  const note = (status) =>
    preferredLanguageNote("subtitles", {
      preferred_language: "eng",
      preferred_language_status: status,
    });
  assert.equal(note("missing"), "no English subtitles");
  // The whole point of the fifth state: an untagged track means the absence of
  // English cannot be claimed, so the page must not claim it.
  assert.notEqual(note("unknown"), note("missing"));
  assert.match(note("unknown"), /no language tag/);
  assert.match(note("available"), /English subtitles available/);
  assert.equal(note("selected"), "", "the marked chip already says this");
  assert.equal(note("no_tracks"), "", "the row already says this");
});

test("a scanning viewer learns 'English audio, no English subtitles'", () => {
  const html = detailHarness().specBlock({
    ...MOVIE_FILE,
    subtitle_streams: [{ index: 0, codec: "subrip", language: "fra" }],
    playback_defaults: {
      audio: {
        selected_index: 0,
        preferred_language: "eng",
        preferred_language_status: "selected",
      },
      subtitle: {
        selected_index: null,
        preferred_language: "eng",
        preferred_language_status: "missing",
      },
    },
  });
  assert.match(html, /English · TRUEHD/);
  assert.match(html, /no English subtitles/);
});

test("both pickers offer the server default, every track, and Off", () => {
  const html = detailHarness().prePlayPickers(MOVIE_FILE);
  assert.match(html, /<select id="pp-a-42"/);
  assert.match(html, /<select id="pp-s-42"/);
  // "Default" is its own option and is what an untouched picker sends: see the
  // request test below for why that distinction is load-bearing.
  assert.match(html, /<option value="" selected>Default · English · TRUEHD · 7\.1</);
  assert.match(html, /<option value="-1">Off<\/option>/);
  assert.match(html, /<option value="2">French · PGS<\/option>/);
  assert.match(html, /it doesn’t change your Playback defaults/);
});

test("an untouched picker changes the decision request by nothing at all", () => {
  const h = detailHarness();
  const legacy = "/files/42/decision?vcodec=h264&acodec=aac&force=auto";
  assert.equal(h.decisionUrl(42, "auto", h.prePlaySelection(42)), legacy);
  // Choosing, then choosing "Default" again, must return to the byte-for-byte
  // legacy request — otherwise reverting a choice leaves a remux behind.
  h.setPrePlay(42, "audio", "1");
  assert.equal(
    h.decisionUrl(42, "auto", h.prePlaySelection(42)),
    `${legacy}&audio=1`,
  );
  h.setPrePlay(42, "audio", "");
  assert.equal(h.decisionUrl(42, "auto", h.prePlaySelection(42)), legacy);
  // Off is an explicit choice, and -1 is how it is spelled.
  h.setPrePlay(42, "subtitle", "-1");
  assert.equal(
    h.decisionUrl(42, "auto", h.prePlaySelection(42)),
    `${legacy}&subtitle=-1`,
  );
});

test("a pre-play choice is per file and does not survive leaving the item", () => {
  const h = detailHarness();
  h.setPrePlay(42, "audio", "1");
  assert.deepEqual(h.prePlaySelection(42), { audio: 1, subtitle: null });
  // A multi-version item's other file is a different set of streams; one
  // item-wide index would point into the wrong file.
  assert.equal(h.prePlaySelection(43), null);
  h.clearPrePlay();
  assert.equal(
    h.prePlaySelection(42),
    null,
    "a choice made on one item must not apply to the next",
  );
});

// The cold-start half: what a selection does to the FIRST session open.
const PGS_DECISION = {
  method: "transcode",
  delivered_dynamic_range: "sdr",
  subtitles: [
    { index: 0, codec: "subrip", text: true },
    { index: 2, codec: "hdmv_pgs_subtitle", text: false },
  ],
  selection: {
    audio_index: 0,
    subtitle_index: 2,
    subtitle_requires_burn_in: true,
    subtitle_burn_in_blocked_by_hdr: false,
  },
};

test("a pre-play burn rides the first session open rather than a restart", () => {
  const applied = detailHarness().prePlayApplication(PGS_DECISION, {
    audio: null,
    subtitle: 2,
  });
  assert.deepEqual(applied, {
    subtitle: 2,
    blockedByHdr: false,
    burnedSub: 2,
    textSub: null,
  });
});

test("a pre-play text subtitle is a <track>, not a second session", () => {
  const applied = detailHarness().prePlayApplication(
    {
      method: "direct_play",
      delivered_dynamic_range: "sdr",
      subtitles: [{ index: 0, codec: "subrip", text: true }],
      selection: {
        audio_index: 0,
        subtitle_index: 0,
        subtitle_requires_burn_in: false,
        subtitle_burn_in_blocked_by_hdr: false,
      },
    },
    { audio: null, subtitle: 0 },
  );
  assert.equal(applied.burnedSub, null);
  assert.equal(applied.textSub, 0, "applied locally, with no stream restart");
});

test("the HDR guard refuses a pre-play burn before the stream exists", () => {
  const applied = detailHarness().prePlayApplication(
    {
      ...PGS_DECISION,
      method: "remux",
      delivered_dynamic_range: "dolby_vision",
      selection: { ...PGS_DECISION.selection, subtitle_burn_in_blocked_by_hdr: true },
    },
    { audio: null, subtitle: 2 },
  );
  assert.equal(applied.blockedByHdr, true);
  assert.equal(applied.burnedSub, null, "HDR is kept, exactly as in-player");
  assert.equal(applied.textSub, null);
});

test("an audio-only choice never burns the subtitle the server merely echoed", () => {
  // `selection.subtitle_index` is the POLICY default here — the request carried
  // no `subtitle=`. Reading that echo as a choice would burn a bitmap track
  // into a film the viewer only asked to hear in French.
  const applied = detailHarness().prePlayApplication(PGS_DECISION, {
    audio: 1,
    subtitle: null,
  });
  assert.deepEqual(applied, {
    subtitle: null,
    blockedByHdr: false,
    burnedSub: null,
    textSub: null,
  });
});

test("a burn this browser needs is honoured even when the plan omits it", () => {
  // PGS with the application overlay enabled: the server plans a remux, because
  // a native client would draw the overlay itself. This player has no overlay
  // route, so following the plan would start a stream that cannot show the
  // chosen track and replace it a moment later.
  const overlayPlan = {
    method: "remux",
    delivered_dynamic_range: "sdr",
    subtitles: [{ index: 2, codec: "hdmv_pgs_subtitle", text: false }],
    selection: {
      audio_index: 0,
      subtitle_index: 2,
      subtitle_requires_burn_in: false,
      subtitle_burn_in_blocked_by_hdr: false,
    },
  };
  const h = detailHarness();
  assert.equal(h.prePlayBurnNeeded(overlayPlan, 2), true);
  assert.equal(h.prePlayApplication(overlayPlan, { subtitle: 2 }).burnedSub, 2);
});

// ---- the carry ends with the playback (review finding 1 on PR #293) --------
//
// play() asks playbackSelection() which tracks to run with, and the answer
// turns on one thing: is this still the playback that is already open? A
// quality change is; the same file played again after the viewer closed the
// player is not. closePlayer() does not replace PLAYER, so that distinction
// exists only because closePlayer() drops `preplay` — without it the pickers on
// the detail screen become decoration after a file's first playback.
//
// The whole scenario runs the SHIPPED closePlayer(), not a description of it.
function carryHarness(player) {
  const stubEl = () => ({
    classList: { remove() {}, add() {} },
    style: {},
    dataset: {},
    innerHTML: "",
    querySelectorAll: () => [],
    pause() {},
    removeAttribute() {},
    load() {},
  });
  const build = new Function(
    "document",
    "PLAYER",
    "exitPresentationModes",
    "reportProgress",
    "releaseSession",
    "stopPlayerTimers",
    "clearInterval",
    "STATS_TIMER",
    "teardownHls",
    "setLoading",
    "location",
    "setTimeout",
    "prePlayPreview",
    "PLAY_OPEN_GATE",
    "cancelPendingSeek",
    [
      shippedBinding("let", "PREPLAY"),
      shippedSource("prePlaySelection"),
      shippedSource("clearPrePlay"),
      shippedSource("playbackSelection"),
      shippedSource("setPrePlay"),
      shippedSource("rememberPlaybackSelection"),
      shippedSource("closePlayer"),
      "return {prePlaySelection, clearPrePlay, playbackSelection, setPrePlay," +
        " rememberPlaybackSelection, closePlayer};",
    ].join("\n"),
  );
  return build(
    { getElementById: stubEl },
    player,
    () => {},
    () => {},
    () => {},
    () => {},
    () => {},
    null,
    () => {},
    () => {},
    // Not an item route: the deferred re-render is a different concern, and the
    // test performs loadItem()'s picker reset explicitly where it happens.
    { hash: "#/" },
    () => {},
    () => {},
    { invalidate() {} },
    () => { player._seekPending = null; player._seekPreview = null; },
  );
}

test("closing the player ends its track choice instead of arming the next play", () => {
  const player = { fileId: 42, preplay: { audio: 1, subtitle: null } };
  const h = carryHarness(player);
  // While it is open, this playback's own tracks are the answer — that is the
  // carry a quality change depends on.
  assert.deepEqual(h.playbackSelection(player, 42), { audio: 1, subtitle: null });
  h.closePlayer();
  // loadItem() empties the pickers on the way back to the detail screen, so
  // "Default" is what the viewer now sees on both of them.
  h.clearPrePlay();
  assert.equal(
    h.playbackSelection(player, 42),
    null,
    "a closed playback's tracks must not be reused by the next cold start, " +
      "which the screen is showing as Default",
  );
});

test("a picker changed after a playback is not overruled by that playback", () => {
  const player = { fileId: 42, preplay: null };
  const h = carryHarness(player);
  // An in-player switch records itself the same way a pre-play choice does.
  h.rememberPlaybackSelection("audio", 0);
  assert.deepEqual(h.playbackSelection(player, 42), { audio: 0, subtitle: null });
  h.closePlayer();
  h.clearPrePlay();
  // Back on the detail screen the viewer picks the French track instead.
  h.setPrePlay(42, "audio", "1");
  assert.deepEqual(
    h.playbackSelection(player, 42),
    { audio: 1, subtitle: null },
    "the explicit choice on screen wins, not the previous playback's",
  );
});

test("a quality change still reproduces the tracks that are playing", () => {
  // The other half of the contract: ending the carry at close must not end it
  // mid-playback, or changing quality would silently revert the viewer's tracks
  // to the cold-start policy default.
  const player = { fileId: 42, preplay: null };
  const h = carryHarness(player);
  h.rememberPlaybackSelection("audio", 1);
  h.rememberPlaybackSelection("subtitle", 2);
  assert.deepEqual(h.playbackSelection(player, 42), { audio: 1, subtitle: 2 });
  // A different file is a cold start even while this one is open.
  h.setPrePlay(43, "subtitle", "-1");
  assert.deepEqual(h.playbackSelection(player, 43), { audio: null, subtitle: -1 });
});

// ---- HEVC capability probe -------------------------------------------------
// The probe is this client's only statement about what its decoder can do, and
// the failure it guards against is silent: a 4K Main10 stream direct-played to
// an 8-bit decoder is accepted by every layer above the decoder and rendered as
// a black picture with working sound. No `error` event fires, so no fallback
// runs. The shipped ladder, fold and query builder are exercised here, not a
// copy of them, because a copy is exactly what stops telling the truth.
function hevcHarness({
  supported = [],
  pqSupported = [],
  mediaCapabilities = "auto",
  hdrDisplay = false,
} = {}) {
  const hit = (list) => (type) => list.some((c) => String(type).includes(c));
  const decodes = hit(supported);
  const MediaSource = { isTypeSupported: decodes };
  const win = { MediaSource };
  const doc = {
    createElement: () => ({ canPlayType: (t) => (decodes(t) ? "probably" : "") }),
  };
  const calls = [];
  const nav = {};
  if (mediaCapabilities === "auto") {
    nav.mediaCapabilities = {
      decodingInfo(config) {
        calls.push({
          type: config.type,
          contentType: config.video.contentType,
          width: config.video.width,
          height: config.video.height,
          transferFunction: config.video.transferFunction || null,
        });
        const answers =
          config.video.transferFunction === "pq" ? pqSupported : supported;
        const ok = hit(answers)(config.video.contentType);
        return Promise.resolve({ supported: ok, smooth: ok, powerEfficient: ok });
      },
    };
  } else if (mediaCapabilities !== null) {
    nav.mediaCapabilities = mediaCapabilities;
  }
  const build = new Function(
    "window",
    "document",
    "MediaSource",
    "navigator",
    "displayIsHdr",
    [
      shippedBinding("const", "HEVC_TIERS"),
      shippedSource("hevcTierSummary"),
      shippedSource("hevcTiersSync"),
      shippedSource("hevcTiersMediaCapabilities"),
      shippedSource("buildPlayCaps"),
      shippedSource("capsQuery"),
      "return {HEVC_TIERS, hevcTierSummary, hevcTiersSync," +
        " hevcTiersMediaCapabilities, buildPlayCaps, capsQuery};",
    ].join("\n"),
  );
  const shipped = build(win, doc, MediaSource, nav, () => hdrDisplay);
  return {
    ...shipped,
    calls,
    codecs: shipped.HEVC_TIERS.map((t) => t.codec),
    // Everything the client would put on the wire, from the synchronous ladder.
    syncCaps() {
      const summary = shipped.hevcTierSummary(
        shipped.hevcTiersSync(
          (t) => doc.createElement().canPlayType(t) !== "",
          (t) => MediaSource.isTypeSupported(t),
        ),
        null,
      );
      return shipped.buildPlayCaps(summary);
    },
    // …and from the MediaCapabilities refinement, exactly as PLAY_CAPS_READY
    // does it: the synchronous answer stands when the API says nothing.
    async refinedCaps() {
      const mc = await shipped.hevcTiersMediaCapabilities();
      if (!mc) return this.syncCaps();
      return shipped.buildPlayCaps(
        shipped.hevcTierSummary(mc.passed, mc.pqPassed),
      );
    },
  };
}
const MAIN8 = ["hvc1.1.6.L93.B0", "hvc1.1.6.L120.B0", "hvc1.1.6.L153.B0"];
const MAIN10 = ["hvc1.2.4.L120.B0", "hvc1.2.4.L153.B0"];

test("the reported height is one every claimed HEVC profile actually decodes", () => {
  const h = hevcHarness();
  const rows = [
    [[], [], { depth8: 0, depth10: 0, maxheight: 0, pq10: false }],
    [
      ["hvc1.1.6.L93.B0"],
      [],
      { depth8: 720, depth10: 0, maxheight: 720, pq10: false },
    ],
    [MAIN8, [], { depth8: 2160, depth10: 0, maxheight: 2160, pq10: false }],
    [
      MAIN8.concat(MAIN10),
      MAIN10,
      { depth8: 2160, depth10: 2160, maxheight: 2160, pq10: true },
    ],
    // The whole point. Main10 decodes only to 1080p while 8-bit reaches 4K: the
    // client may not say "2160" and "hevc10" in one breath, because the server's
    // max_height is codec-agnostic and would then direct-play a 4K Main10.
    [
      MAIN8.concat(["hvc1.2.4.L120.B0"]),
      ["hvc1.2.4.L120.B0"],
      { depth8: 2160, depth10: 1080, maxheight: 1080, pq10: true },
    ],
    // PQ proven at 1080 only while the reported height is 2160 — hdr10t is a
    // claim about the reported height, so it must not be made here.
    [
      MAIN8.concat(MAIN10),
      ["hvc1.2.4.L120.B0"],
      { depth8: 2160, depth10: 2160, maxheight: 2160, pq10: false },
    ],
  ];
  for (const [passing, pq, expected] of rows) {
    assert.deepEqual(
      h.hevcTierSummary(
        h.codecs.map((c) => passing.includes(c)),
        h.codecs.map((c) => pq.includes(c)),
      ),
      expected,
      JSON.stringify(passing),
    );
  }
});

test("an 8-bit-only decoder is never reported to the server as Main10", () => {
  const caps = hevcHarness({
    supported: ["hvc1.1.6.L93.B0", "hvc1.1.6.L120.B0"],
  }).syncCaps();
  assert.equal(caps.vcodec.split(",").includes("hevc"), true);
  assert.equal(caps.vcodec.split(",").includes("hevc10"), false);
  assert.equal(caps.maxheight, 1080);
  assert.equal(caps.hdr10t, 0);
});

test("no HEVC decoder means no maxheight, so an HEVC cap never caps H.264", () => {
  const h = hevcHarness({ supported: ["avc1.640033"] });
  const caps = h.syncCaps();
  assert.equal(caps.vcodec.includes("hevc"), false);
  assert.equal(caps.maxheight, null);
  assert.equal(h.capsQuery(caps).includes("maxheight"), false);
});

test("the synchronous fallback cannot prove PQ, so it never claims hdr10t", () => {
  // isTypeSupported/canPlayType have no transfer-function axis. A Main10 yes
  // plus an HDR panel is the guess this probe exists to refuse to make.
  const caps = hevcHarness({
    supported: MAIN8.concat(MAIN10),
    hdrDisplay: true,
  }).syncCaps();
  assert.equal(caps.vcodec.split(",").includes("hevc10"), true);
  assert.equal(caps.maxheight, 2160);
  assert.equal(caps.hdr, 1); // the loose, long-standing claim is unchanged
  assert.equal(caps.hdr10t, 0); // the strict new one is not made
});

asyncTest(
  "hdr10t needs a PQ answer AND an HDR display, and says so on the wire",
  async () => {
    const hdr = await hevcHarness({
      supported: MAIN8.concat(MAIN10),
      pqSupported: MAIN10,
      hdrDisplay: true,
    }).refinedCaps();
    assert.equal(hdr.maxheight, 2160);
    assert.equal(hdr.hdr10t, 1);

    const sdrPanel = await hevcHarness({
      supported: MAIN8.concat(MAIN10),
      pqSupported: MAIN10,
      hdrDisplay: false,
    }).refinedCaps();
    assert.equal(sdrPanel.vcodec.split(",").includes("hevc10"), true);
    assert.equal(sdrPanel.maxheight, 2160, "the Main10 claim stays honest on SDR");
    assert.equal(sdrPanel.hdr10t, 0);

    // Decodes 10-bit but cannot put PQ on the wire — a 10-bit SDR rip must
    // still direct-play, so `hevc10` stays and only `hdr10t` goes.
    const noPq = await hevcHarness({
      supported: MAIN8.concat(MAIN10),
      pqSupported: [],
      hdrDisplay: true,
    }).refinedCaps();
    assert.equal(noPq.vcodec.split(",").includes("hevc10"), true);
    assert.equal(noPq.hdr10t, 0);

    const h = hevcHarness({
      supported: MAIN8.concat(MAIN10),
      pqSupported: MAIN10,
      hdrDisplay: true,
    });
    const query = h.capsQuery(await h.refinedCaps());
    assert.match(query, /&maxheight=2160&/);
    assert.match(query, /&hdr10t=1$/);
    assert.equal(
      h.calls.some((c) => c.transferFunction === "pq"),
      true,
      "the HDR rungs are asked with transferFunction:'pq'",
    );
  },
);

asyncTest(
  "a browser with no MediaCapabilities degrades without crashing or over-claiming",
  async () => {
    const absent = hevcHarness({
      supported: MAIN8.concat(MAIN10),
      mediaCapabilities: null,
      hdrDisplay: true,
    });
    assert.equal(await absent.hevcTiersMediaCapabilities(), null);
    const caps = await absent.refinedCaps();
    assert.equal(caps.vcodec.split(",").includes("hevc10"), true);
    assert.equal(caps.maxheight, 2160);
    assert.equal(caps.hdr10t, 0);

    // Present but rejecting every configuration: the same fallback, not an
    // exception escaping into boot() and a page that never renders.
    const throws = hevcHarness({
      supported: MAIN8,
      mediaCapabilities: {
        decodingInfo() {
          throw new TypeError("unsupported configuration");
        },
      },
      hdrDisplay: true,
    });
    assert.equal(await throws.hevcTiersMediaCapabilities(), null);
    assert.equal((await throws.refinedCaps()).maxheight, 2160);
  },
);

asyncTest("the Dolby Vision probe is unchanged by the tiering", async () => {
  // Chrome answers no to dvh1.05.06 and must keep doing so; Safari answers yes
  // to both profiles. Neither answer may move because HEVC learned about depth.
  const chrome = await hevcHarness({
    supported: MAIN8.concat(MAIN10),
    pqSupported: MAIN10,
    hdrDisplay: true,
  }).refinedCaps();
  assert.equal(chrome.dv, 0);
  assert.equal(chrome.dvprofile, "");

  const safari = hevcHarness({
    supported: MAIN8.concat(MAIN10, ["dvh1.05.06", "dvhe.08.07"]),
    mediaCapabilities: null,
    hdrDisplay: true,
  });
  const caps = await safari.refinedCaps();
  assert.equal(caps.dv, 1);
  assert.equal(caps.dvprofile, "5,8");
  assert.match(safari.capsQuery(caps), /&dv=1&dvprofile=5,8&/);
});

test("a converted Dolby Vision stream names the profile it is playing as", () => {
  // The badge state PLAYBACK-CAPS-V2-PLAN §4.8 adds and MEDIA-BADGES-PLAN
  // §2.3 spells out. A Profile 7 disc remux converted to 8.1 for a browser
  // that takes 8 and not 7 is delivered `dolby_vision` — the same value a
  // preserved Profile 7 answers — so the range alone cannot tell the two
  // apart, and a chip reading plain `DV P7` for both is describing the file
  // rather than the picture.
  //
  // Neither half dims. The base layer is copied byte for byte and nothing is
  // re-encoded, so the source capability is active; dimming it would say the
  // opposite of what happened.
  const build = new Function(
    "RANGE_SHORT",
    "RANGE_LONG",
    [
      shippedSource("hdrChip"),
      shippedSource("sourceDynamicRange"),
      shippedSource("sourceDolbyVisionProfile"),
      shippedSource("dynamicRangeReason"),
      shippedSource("dynamicRangeBadge"),
      "return {dynamicRangeBadge};",
    ].join("\n"),
  );
  const { dynamicRangeBadge } = build(
    { dolby_vision: "DV", hdr10: "HDR10", hlg: "HLG", sdr: "SDR" },
    { dolby_vision: "Dolby Vision", hdr10: "HDR10", hlg: "HLG", sdr: "SDR" },
  );

  const p7 = {
    hdr: "dolby_vision",
    hdr_format: "Dolby Vision · Profile 7 (HDR10-compatible)",
  };

  const converted = dynamicRangeBadge(p7, "dolby_vision", true, 8);
  assert.equal(converted.text, "DV P7 → DV P8");
  assert.equal(converted.base, "DV P7", "the source half still names the source");
  assert.equal(converted.arrow, "DV P8");
  assert.equal(converted.off, false, "nothing about the grade was lost");
  assert.equal(converted.rendered, "dolby_vision");
  assert.match(converted.aria, /Profile 7, playing as Dolby Vision Profile 8/);
  assert.match(converted.panel, /Profile 8/, "the stats overlay reads this one");
  assert.match(converted.full, /converted for this browser/);

  // The profile is read, not assumed: a future rung that delivered some other
  // profile must name that one.
  assert.equal(dynamicRangeBadge(p7, "dolby_vision", true, 5).arrow, "DV P5");

  // A client that decodes Profile 7 gets it untouched, and the arrow would be
  // a lie: the profile on screen is the profile on disk.
  const preserved = dynamicRangeBadge(p7, "dolby_vision", true, 7);
  assert.equal(preserved.text, "DV P7");
  assert.equal(preserved.arrow, null);

  // A server too old to send the field, or a session that carries no Dolby
  // Vision, degrades to exactly the chip that shipped before this.
  assert.equal(dynamicRangeBadge(p7, "dolby_vision", true).text, "DV P7");
  assert.equal(dynamicRangeBadge(p7, "dolby_vision", true, null).text, "DV P7");

  // A stripped stream is a different grade and keeps the dimmed state it had:
  // there the source capability really is unavailable.
  const stripped = dynamicRangeBadge(p7, "hdr10", true, null);
  assert.equal(stripped.text, "DV P7 → HDR10");
  assert.equal(stripped.off, true);

  // A row scanned before the profile columns existed has no number to compare
  // against, so it stays as it was rather than inventing an arrow.
  const unlabelled = { hdr: "dolby_vision", hdr_format: "Dolby Vision" };
  assert.equal(dynamicRangeBadge(unlabelled, "dolby_vision", true, 8).text, "DV");

  // The source profile comes from the column when there is one, because the
  // delivered profile beside it does. A row whose column and prose disagree
  // would otherwise invent `DV P7 → DV P8` over a preserved Profile 8 stream
  // that nothing converted.
  const columned = {
    hdr: "dolby_vision",
    hdr_format: "Dolby Vision · Profile 7 (HDR10-compatible)",
    dolby_vision: { profile: 8 },
  };
  const agreeing = dynamicRangeBadge(columned, "dolby_vision", true, 8);
  assert.equal(agreeing.text, "DV P8", "the column wins, and 8 → 8 is no arrow");
});

test("every surface paints the badge from the same four answers", () => {
  // The four surfaces that show this chip — the fact badges, the play overlay,
  // the stats panel and the debug ledger — used to call dynamicRangeBadge()
  // themselves, and a call site that dropped one argument would degrade
  // silently to the badge that shipped before that argument existed: a
  // correct-looking chip for the wrong delivery, on one surface out of four.
  // `playerRangeBadge` is the single reader, so there is one place to get it
  // wrong and this is the test of that place.
  const built = new Function(
    "PLAYER",
    "RANGE_SHORT",
    "RANGE_LONG",
    // node has no matchMedia, so the shipped `displayIsHdr` would answer no
    // and every case below would collapse to the display-loss branch. The
    // display answer is not what this test is about.
    "displayIsHdr",
    [
      shippedSource("hdrChip"),
      shippedSource("sourceDynamicRange"),
      shippedSource("sourceDolbyVisionProfile"),
      shippedSource("dynamicRangeReason"),
      shippedSource("dynamicRangeBadge"),
      shippedSource("playerRangeBadge"),
      "return {playerRangeBadge};",
    ].join("\n"),
  );
  const player = { deliveredRange: "dolby_vision", deliveredDvProfile: 8 };
  const { playerRangeBadge } = built(
    player,
    { dolby_vision: "DV", hdr10: "HDR10", hlg: "HLG", sdr: "SDR" },
    { dolby_vision: "Dolby Vision", hdr10: "HDR10", hlg: "HLG", sdr: "SDR" },
    () => true,
  );

  const p7 = {
    hdr: "dolby_vision",
    hdr_format: "Dolby Vision · Profile 7 (HDR10-compatible)",
  };
  assert.equal(playerRangeBadge(p7).text, "DV P7 → DV P8",
    "the profile the session reported has to reach the chip");

  player.deliveredDvProfile = null;
  assert.equal(playerRangeBadge(p7).text, "DV P7",
    "and clearing it has to reach the chip too");

  player.deliveredRange = "hdr10";
  assert.equal(playerRangeBadge(p7).text, "DV P7 → HDR10");
});

test("a session that lands on a different range repaints the badge", () => {
  // The field bug: on a tone-mapped Dexter episode the chip read "DV P7 →
  // HDR10" while the stats panel one line below read "Dynamic range: SDR".
  // Both surfaces call dynamicRangeBadge() with the same arguments — the
  // panel just repaints every second, and the chip was painted once at
  // session open, before the route was chosen. attachSession is the one site
  // every session passes through, so it owns the repaint.
  let repaints = 0;
  const build = new Function(
    "PLAYER",
    "renderPlayerInfo",
    "attachHls",
    "stopPlaybackControl",
    "startPlaybackControl",
    [shippedSource("attachSession"), "return {attachSession};"].join("\n"),
  );
  const player = { deliveredRange: "hdr10" };
  const { attachSession } = build(
    player, () => { repaints += 1; }, () => {}, () => {}, () => {},
  );

  attachSession({}, player, {
    start_seconds: 0,
    playlist_url: "/x.m3u8",
    delivered_dynamic_range: "sdr",
  }, 0);
  assert.equal(player.deliveredRange, "sdr", "the session is the source of truth");
  assert.equal(repaints, 1, "the chip must be repainted, not left at the decision's guess");

  // The Dolby Vision profile follows the same rule and needs the stricter
  // half of it: absence is an answer. A decision that promised a conversion
  // followed by a session that stripped must CLEAR the profile, or the chip
  // reads `DV P7 → DV P8` over the HDR10 base — which is the shape the
  // legacy single-ffmpeg copy actually produces on a converting title's
  // first watch, before its third fragment index exists.
  const converting = { deliveredRange: "dolby_vision", deliveredDvProfile: 8 };
  attachSession({}, converting, {
    start_seconds: 0,
    playlist_url: "/x.m3u8",
    delivered_dynamic_range: "hdr10",
  }, 0);
  assert.equal(converting.deliveredRange, "hdr10");
  assert.equal(converting.deliveredDvProfile, null,
    "a session that carries no Dolby Vision must not leave a profile behind");

  // …and the other direction: a session that does convert reports the
  // profile the decision could not know.
  const landed = { deliveredRange: "dolby_vision", deliveredDvProfile: null };
  attachSession({}, landed, {
    start_seconds: 0,
    playlist_url: "/x.m3u8",
    delivered_dynamic_range: "dolby_vision",
    delivered_dolby_vision_profile: 8,
  }, 0);
  assert.equal(landed.deliveredDvProfile, 8);

  // A response with no range keeps the decision's answer — and still must not
  // leave a stale chip behind, because other fields it paints moved too.
  const before = repaints;
  attachSession({}, player, { start_seconds: 0, playlist_url: "/x.m3u8" }, 0);
  assert.equal(player.deliveredRange, "sdr");
  assert.ok(repaints > before, "every attach repaints");

  // A superseded playback must not repaint over the live one.
  const stale = { deliveredRange: "hdr10" };
  const settled = repaints;
  attachSession({}, stale, {
    start_seconds: 0,
    playlist_url: "/y.m3u8",
    delivered_dynamic_range: "sdr",
  }, 0);
  assert.equal(repaints, settled, "a stale generation never paints the live player");
});

test("every web transcode reopen preserves the decision's HDR10 request", () => {
  const build = new Function(
    "PLAYER",
    "transcodeHeight",
    "qualityForce",
    "sessionHeight",
    [shippedSource("transcodeOpts"), "return {transcodeOpts};"].join("\n"),
  );
  const player = {
    requestHdr10: true,
    deliveredRange: "hdr10",
    autoHeight: 2160,
    burnedSub: null,
    aoffset: 0,
  };
  const { transcodeOpts } = build(player, () => null, () => "auto", () => null);

  assert.deepEqual(transcodeOpts(12, 3), {
    height: 2160,
    start: 12,
    audio: 3,
    hdr10: true,
  });

  // attachSession replaces deliveredRange with what the session actually
  // produced. That mutable display truth must never erase the immutable
  // decision request when a seek/audio switch opens the next session.
  player.deliveredRange = "sdr";
  assert.equal(transcodeOpts(30, 3).hdr10, true);

  player.requestHdr10 = false;
  assert.equal("hdr10" in transcodeOpts(30, 3), false);
});

test("HDR10 Auto leaves the cold-start height to the grade-aware server", () => {
  assert.match(
    SHIPPED_UI,
    /const autoStartHeight=[\s\S]{0,520}decision\.delivered_dynamic_range!==['"]hdr10['"]/,
    "a persisted 720p SDR rung must not override the server's proved HDR10 ceiling",
  );
});

test("control lease labels distinguish legacy, explicit, and VOD delivery", () => {
  assert.equal(policy.controlLeaseMode(null, false, 0), "legacy");
  assert.equal(policy.controlLeaseMode(null, false, 7), "explicit");
  assert.equal(policy.controlLeaseMode(null, true, 0), "vod");
  assert.equal(
    policy.controlLeaseMode("explicit", true, 0),
    "explicit",
    "a server-reported mode wins once health arrives",
  );
  assert.deepEqual(policy.controlLeasePresentation("legacy"), {
    ownership: "passive",
    label: "legacy",
  });
  assert.deepEqual(policy.controlLeasePresentation("explicit"), {
    ownership: "demand-owned",
    label: "explicit",
  });
  assert.deepEqual(policy.controlLeasePresentation("vod"), {
    ownership: "immutable VOD",
    label: "VOD",
  });
});

test("an upgrade needs encode headroom, not just a bandwidth estimate", () => {
  const ladder = [
    { height: 1080, total_kbps: 8160, peak_kbps: 12160 },
    { height: 720, total_kbps: 4160, peak_kbps: 6160 },
    { height: 480, total_kbps: 2160, peak_kbps: 3160 },
  ];
  // The production shape: on a JIT server the estimate measures
  // min(link, encode) of the CURRENT rung, so a fast 720p encode reads as
  // ~200 Mb/s and clears any bar the 1080p rung can set. The server's own
  // pace is what says whether one rung up is sustainable.
  const base = {
    ladder,
    currentHeight: 720,
    estimateKbps: 200_000,
    runwaySeconds: 40,
    previousRunwaySeconds: 40,
    nowMs: 500_000,
    lastSwitchAtMs: 0,
    lastStallAtMs: null,
    upgradeSinceMs: 1,
  };

  // 720 -> 1080 is 2.25x the pixels. A server holding 2.0x realtime at 720p
  // predicts 0.89x at 1080p — under realtime, so no upgrade.
  assert.equal(
    policy.decideRung({ ...base, recentSpeed: 2.0 }).height,
    720,
    "an encoder that cannot hold realtime one rung up must not be sent there",
  );
  // 3.0x predicts 1.33x — enough margin, so the upgrade proceeds.
  assert.equal(
    policy.decideRung({ ...base, recentSpeed: 3.0 }).height,
    1080,
    "real headroom still upgrades",
  );
  // No measurement is not evidence against: absence must not freeze the rung.
  assert.equal(
    policy.decideRung({ ...base, recentSpeed: null }).height,
    1080,
    "an unmeasured session can still rise",
  );
  // A rung this playback already failed to open is never re-entered, however
  // good the numbers look — the loop that oscillated 720<->1080 every ~2
  // minutes had healthy-looking numbers on every cycle.
  assert.equal(
    policy.decideRung({
      ...base,
      recentSpeed: 3.0,
      blockedHeights: new Set([1080]),
    }).height,
    720,
    "a failed rung is not a candidate",
  );
  // Blocking the rung above must not block the way DOWN.
  const pressured = policy.decideRung({
    ...base,
    recentSpeed: 3.0,
    blockedHeights: new Set([1080]),
    estimateKbps: 1_000,
    runwaySeconds: 0.5,
  });
  assert.equal(pressured.height, 480, "starvation reaches the available ladder floor");
  assert.equal(pressured.emergency, true);
});

test("the caps document says the two things the flat query could not", () => {
  // The two-entry HEVC ladder replacing the min-of-rungs hack
  // (PLAYBACK-CAPS-V2-PLAN §2, edge E5, and the M3 acceptance check).
  //
  // The flat query has one `maxheight` slot, so a browser whose 8-bit ceiling
  // is 2160 and whose Main10 ceiling is 1080 had to send 1080 — transcoding
  // every 4K 8-bit title it would have direct-played. The document says both.
  const build = new Function(
    "SERVER",
    "navigator",
    // The newline matters: `shippedSource` can end on a `//` comment, and
    // appending the return to that line comments it out — the harness then
    // hands back `undefined` and every assertion below fails on the wrong
    // thing.
    `${shippedSource("capsDocument")}\n; return capsDocument;`,
  )({ build: "v0.2.8-20-gabc1234" }, { userAgent: "Mozilla/5.0 (Test)" });

  const both = build(
    {
      vcodec: "h264,hevc,hevc10",
      acodec: "aac,ac3",
      container: "mp4,mov",
      hdr: 1,
      hdrDisplay: true,
      dv: 1,
      dvprofile: "5,8",
      maxheight: 2160,
      hdr10t: 1,
    },
    {},
  );

  assert.equal(both.v, 2);
  assert.equal(both.client.kind, "web");
  assert.equal(both.client.build, "v0.2.8-20-gabc1234");

  const hevc = both.video.filter((entry) => entry.codec === "hevc");
  assert.equal(hevc.length, 2, "one entry per profile ladder rung");
  assert.deepEqual(hevc[0].profiles, ["main"]);
  assert.deepEqual(hevc[1].profiles, ["main10"]);
  assert.ok(
    !both.video.some((entry) => entry.codec === "hevc10"),
    "`hevc10` is this app's name for a PROFILE; it is not a codec on the wire",
  );

  // `hdr10t` is a per-codec presentation claim and always was — the flat
  // query just had nowhere to say so.
  assert.deepEqual(hevc[0].present, ["sdr", "pq"]);
  assert.deepEqual(
    both.video.find((entry) => entry.codec === "h264").present,
    ["sdr"],
    "the PQ claim was only ever about HEVC",
  );

  // `hdr` is a DISPLAY fact, kept separate from what any codec can emit.
  assert.equal(both.display.hdr, true);
  assert.deepEqual(hevc[0].dv_profiles, [5, 8]);

  // A browser with no HEVC decoder claims no HEVC, and one with a single
  // HEVC ceiling sends a single entry rather than a fabricated ladder.
  const plain = build(
    { vcodec: "h264", acodec: "aac", container: "mp4", hdr: 0, hdrDisplay: false,
      dv: 0, dvprofile: "", maxheight: null, hdr10t: 0 },
    {},
  );
  assert.deepEqual(plain.video.map((entry) => entry.codec), ["h264"]);
  assert.equal(plain.display.hdr, false);
  assert.equal(plain.max_height, undefined, "absent is not a claim");

  // There is no way to spell the blanket Dolby Vision claim in this shape.
  assert.deepEqual(plain.video[0].dv_profiles, undefined);
  assert.equal(plain.display.dolby_vision, false);
});

test("a learned limit reaches the server with the identity it was keyed by", () => {
  // The trap this exists for: localStorage keys these entries BY the
  // identity, so the obvious `Object.values(...)` sends a document whose
  // every limit is anonymous — and an anonymous limit matches nothing on the
  // server, silently. The whole feature would look like it simply did not
  // work.
  const build = new Function(
    "SERVER",
    "navigator",
    `${shippedSource("capsDocument")}\n; return capsDocument;`,
  )({ build: "test" }, { userAgent: "Mozilla/5.0 (Test)" });

  const identity =
    'decode-v2:["hevc","main 10",3840,2160,10,"dolby_vision","dolby vision · profile 7 (hdr10-compatible)",5]';
  const document = build(
    { vcodec: "hevc", acodec: "aac", container: "mp4", hdr: 1, hdrDisplay: true,
      dv: 0, dvprofile: "", maxheight: 2160, hdr10t: 1 },
    {
      [identity]: {
        lost: 41, rate: 41, secs: 60, label: "4K HEVC Main 10", at: 1756400000000,
      },
    },
  );

  assert.equal(document.learned_limits.length, 1);
  assert.equal(document.learned_limits[0].identity, identity);
  assert.equal(document.learned_limits[0].label, "4K HEVC Main 10");
  assert.equal(
    document.learned_limits[0].at_ms,
    1756400000000,
    "the server reads `at_ms`; localStorage spells it `at`",
  );
});

test("the caps document POST falls back to the query a mixed fleet still answers", () => {
  // Mid-deploy, some nodes predate the POST. It has to degrade to the GET
  // rather than fail a play — and it can, because the server puts both wire
  // shapes through one translation and returns the same verdict.
  const source = shippedSource("askDecision");
  assert.match(source, /method:\s*"POST"/);
  assert.match(source, /caps:\s*currentCapsDocument\(\)/);
  assert.match(
    shippedSource("currentCapsDocument"),
    /capsDocument\(PLAY_CAPS,\s*decodeLimits\(\)\)/,
    "the document the decision is asked with is still built from this browser's own probe",
  );
  for (const status of [404, 405, 400]) {
    assert.ok(
      new RegExp(`e\\.status===${status}`).test(source),
      `a ${status} from an older or stricter node must fall back, not fail the play`,
    );
  }
  assert.match(source, /return api\(decisionUrl\(fileId, force, sel\)\)/);
});

test("the browser and the server key a learned limit identically", () => {
  // `tests/playback/decode-limit-identity.json` is the contract between this
  // function and `plurx_core::playback::caps::decode_limit_identity`. The
  // browser keys a learned limit by the source it is looking at; the server
  // has to recompute the same string from the file row it is deciding about.
  //
  // A divergence has no symptom. Nothing errors — the server's key simply
  // never matches, no limit is ever applied, and the viewer keeps stuttering
  // through the exact title they already taught their browser to avoid. So
  // both implementations run against these rows, and a change to either
  // spelling fails in both languages at once.
  const fixture = JSON.parse(
    fs.readFileSync(
      path.join(__dirname, "decode-limit-identity.json"),
      "utf8",
    ),
  );
  assert.ok(
    fixture.cases.length >= 10,
    "a fixture this small stops being a contract",
  );
  for (const row of fixture.cases) {
    assert.equal(
      policy.decodeLimitIdentity(row.source),
      row.identity,
      row.name,
    );
  }
});

// Drained last, in registration order, after every synchronous case has run.
(async () => {
  for (const [name, run] of ASYNC_TESTS) {
    try {
      await run();
    } catch (error) {
      error.message = `${name}: ${error.message}`;
      throw error;
    }
    process.stdout.write(`PASS ${name}\n`);
  }
})();
