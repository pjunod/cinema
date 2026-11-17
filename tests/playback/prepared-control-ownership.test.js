"use strict";
const assert = require("node:assert/strict");
const test = require("node:test");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const acorn = require("../vendor/acorn.js");
const control = require("../../crates/plurxd/src/web/playback-control.js");
const root = path.resolve(__dirname, "../../crates/plurxd/src/web");
const ownerGeneration = "11111111-1111-4111-8111-111111111111";
const oldSession = "22222222-2222-4222-8222-222222222222";
const nextSession = "33333333-3333-4333-8333-333333333333";
const actionId = "44444444-4444-4444-8444-444444444444";
function source(file, name) {
  const text = fs.readFileSync(path.join(root, file), "utf8");
  const declaration = acorn.parse(text, { ecmaVersion: "latest" }).body.find(node =>
    node.type === "FunctionDeclaration" ? node.id.name === name :
      node.type === "VariableDeclaration" && node.declarations.some(item => item.id.name === name));
  assert.ok(declaration, name);
  return text.slice(declaration.start, declaration.end);
}
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function answer(request, extra = {}) {
  return { protocol: control.PROTOCOL, generation: request.generation,
    control_epoch: request.control_epoch, accepted_sequence: request.sequence,
    action: { type: "none" }, ...extra };
}
async function flush() { for (let i = 0; i < 12; i++) await Promise.resolve(); }

// Only browser rendering/first-frame delivery and HTTP are simulated. Exposure,
// capture, snapshot, ACK queue, settlement, retirement and rollback are shipped
// functions, connected through the real Reporter and a deterministic clock.
function harness({ native = true, send = request => Promise.resolve(answer(request)), shared = false,
  successorBootstrap = null } = {}) {
  let now = 100000, timerId = 0, frameProof, frameId = 0;
  const frameCallbacks = new Map();
  const timers = new Map(), sent = [], handled = [], recovered = [], logs = [];
  function setTimer(fn, ms) { timers.set(++timerId, { fn, at: now + ms }); return timerId; }
  function clearTimer(id) { timers.delete(id); }
  class Reporter extends control.Reporter {
    constructor(options) { super({ ...options, now: () => now, setTimer, clearTimer }); }
  }
  const nodes = [];
  function element(id, currentTime, ranges, readyState) {
    const node = { id, currentTime, ranges, readyState, paused: false, seeking: false,
      ended: false, error: null, muted: false, volume: 1, playbackRate: 1, defaultPlaybackRate: 1,
      style: {}, attributes: {}, src: "fixture", parentNode: null,
      setAttribute(key, value) { this.attributes[key] = value; },
      removeAttribute(key) { delete this.attributes[key]; if (key === "src") this.src = null; },
      pause() { this.paused = true; }, play() { this.paused = false; return Promise.resolve(); },
      load() { this.currentTime = 0; this.readyState = 0; },
      removeEventListener() {}, requestVideoFrameCallback() {},
      buffered: { get length() { return ranges.length; }, start: i => ranges[i][0], end: i => ranges[i][1] },
    };
    node.parentNode = { removeChild(retired) { nodes.splice(nodes.indexOf(retired), 1); retired.parentNode = null; } };
    nodes.push(node); return node;
  }
  const incumbent = element("video", 57, [[50, 90]], 4);
  const successor = element("video-prepared", 2317, [], 2);
  const oldHls = native ? null : { bandwidthEstimate: 9000000 };
  const p = { fileId: 7757, sessionId: oldSession, offset: 2200, vod: false, rangePresentation: true,
    hls: oldHls, method: "remux", copyHls: true, started: true, wantsPlayback: true,
    durMs: 7200000, mediaAttachment: { nativePositionPending: false }, controlIntentGeneration: 0,
    fileContext: shared ? { source_ref: { kind: "shared" }, session_id: oldSession } : null };
  const state = { actionId, sessionId: nextSession, playlistUrl: `/api/v1/hls/${nextSession}/index.m3u8`,
    mediaOriginMs: 0, offeredOriginMs: 0, vod: false, selection: { dynamic_range: "dolby_vision" },
    controlBootstrap: successorBootstrap, hls: native ? null : { bandwidthEstimate: 1000000 },
    sharedContext: shared ? { source_ref: { kind: "shared" }, session_id: nextSession } : null,
    stageAtMs: now, state: "buffer_ready" };
  const capabilities = { platform: "web", max_height: 2160, codecs: ["hevc"],
    dynamic_ranges: ["sdr", "dolby_vision"], dual_player_preparation: true };
  const context = vm.createContext({ console, PLAYER: p, window: { PlurxPlaybackControl: { ...control, Reporter } },
    PlurxPlaybackControl: { ...control, Reporter }, CONTROL_CLIENT_ID: "55555555-5555-4555-8555-555555555555",
    document: { getElementById: id => nodes.find(node => node.id === id) || null },
    setTimeout: setTimer, clearTimeout: clearTimer, performance: { now: () => now }, Date,
    ENDED_SLACK_SEC: 5, PERSISTENT_STALL_MS: 8000, SERVER: {}, PlaybackPolicy: { isPauseGraceExpiry: () => false },
    raisePlaybackSurface() {}, playbackSurfaceGeneration: () => null,
    clearEffectiveProcessing(player) { player.effectiveProcessing = null; },
    hasPendingPlaybackOpen: () => false, playbackControlCapabilities: () => capabilities,
    playbackControlSelection: () => ({ quality: { mode: "auto" }, audio_track: 0,
      subtitle: { mode: "off", track: null }, audio_offset_ms: 0, codec: "auto", dynamic_range: "auto" }),
    qualityControlSupported: () => false, settlePlaybackControlWaiters() {}, clearPlaybackControlWaiters() {},
    effectiveHdrProcessing: value => value || null, processingSkipFromWire: value => value || null,
    renderPlayerInfo() {}, candidateLinkReceipt: () => null,
    async sendPlaybackControl(url, request) { sent.push({ url, request }); return send(request, sent.length); },
    handlePreparedReplacementAction(player, action) { handled.push(action); },
    recoverSharedCommittedReplacement(player, pending) { recovered.push(pending); },
    detachPreparedOverlapListeners() {}, restorePreparedOverlap() {},
    adoptPlaybackMediaElement() {}, resetPlaybackTransportEvents() {}, preferNativeHls: () => native,
    notePreparedSwitchCommit() {}, notePreparedSwitchFirstFrame() {}, adoptPreparedHls() {},
    streamHasVideo: () => true,
    requestPlaybackVideoFrame(video, fn) { frameCallbacks.set(++frameId, fn); return frameId; },
    settleDirectedChange() {}, fallBackDirectedChange() {}, reportPreparedSeamAfterWindow() {},
    playbackContext: () => ({}), clientLog: event => logs.push(event),
    resumePreparedIncumbentLoad() {}, destroyHlsInstance(player, hls) { if (hls) hls.destroyed = true; },
    freePreparedReplacement() {}, markPreparedSettlement() {},
    cancelPlaybackVideoFrame(video, id) { frameCallbacks.delete(id); },
  });
  for (const name of ["PREPARED_TERMINAL_STATES", "PREPARED_ACK_QUEUE_MAX", "PREPARED_FINALIZATION_MS", "PREPARED_FIRST_FRAME_MS"])
    vm.runInContext(source("player/prepared-replacement.js", name), context);
  for (const name of ["PREPARED_RETIRE_MAX_MS", "PREPARED_RETIRE_IDLE_TIMEOUT_MS", "PREPARED_RETIRE_ADVANCING_FRAMES"])
    vm.runInContext(source("player/prepared-replacement.js", name), context);
  for (const [file, names] of [
    ["player/stall-diagnosis.js", ["playbackOwnsAttachedMedia"]],
    ["player/session.js", ["playbackControlBufferedRange", "playbackControlObservationOverride", "playbackControlSnapshot"]],
    ["player/prepared-replacement.js", ["preparedState", "pendingPlaybackControlAcknowledgement",
      "queuePlaybackControlAcknowledgement", "settlePlaybackControlAcknowledgement", "exposePreparedReplacement"]],
    ["player/prepared-switch-measurement.js", ["disposeRetiredMediaElement", "deferPreparedPredecessorRetirement",
      "retirePreparedPredecessor", "rollbackPreparedReplacement", "abandonPreparedReplacement", "preparedFirstFrame", "cancelPreparedFirstFrame",
      "flushPreparedSettlement", "endingPlaybackControlSnapshot", "finishStoppingPlaybackControl",
      "continueStoppingPlaybackControl", "stopPlaybackControl", "notifyPlaybackControl"]],
    ["player/directed-change.js", ["startPlaybackControl"]],
  ]) for (const name of names) vm.runInContext(source(file, name), context);
  const reporter = context.startPlaybackControl(incumbent, p, { protocol: control.PROTOCOL,
    url: `/api/v1/hls/${oldSession}/control`, generation: ownerGeneration,
    control_epoch: 1, next_exchange_ms: 1000, lease_timeout_ms: 300000 });
  assert.ok(reporter);
  return { p, state, reporter, incumbent, successor, sent, handled, recovered, context, logs,
    expose() {
      assert.equal(context.exposePreparedReplacement(p, state, incumbent, successor, 2317000), true);
      frameProof = frameCallbacks.get(state.frameCallbackId);
      assert.ok(frameProof, "actual first-frame owner registered its rendering callback");
    },
    prove() { frameProof(now, { mediaTime: successor.currentTime }); },
    rollback() {
      const deadline = timers.get(state.frameTimer);
      assert.ok(deadline); timers.delete(state.frameTimer); now = deadline.at; deadline.fn();
    },
    retire() { context.retirePreparedPredecessor(p, state); },
    async advance(ms) {
      now += ms;
      for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.fn(); }
      await flush();
    },
  };
}

for (const native of [true, false]) test(`${native ? "native" : "MSE"} successor waiting does not replace healthy incumbent observations or its origin`, async () => {
  const h = harness({ native }); await flush(); h.expose();
  assert.equal(h.context.document.getElementById("video"), h.successor);
  const captured = h.reporter.capture().snapshot;
  assert.equal(captured.render_state, "rendering");
  assert.equal(captured.position_ms, 2257000);
  assert.equal(captured.buffered_through_ms, 2290000);
  assert.equal(captured.observed_download_bps, native ? null : 9000000);
  h.context.notifyPlaybackControl("stalled", { decoder_state: "starved" });
  await h.advance(250);
  assert.equal(h.sent[1].request.render_state, "rendering");
  assert.equal(h.sent[1].request.position_ms, 2257000);
  assert.equal(h.sent[1].request.generation, ownerGeneration);
  h.reporter.stop();
});

test("actual incumbent waiting remains visible and advances to stalled during successor exposure", async () => {
  const h = harness(); await flush();
  h.incumbent.readyState = 2; h.p.waitAt = 93000;
  h.expose();
  assert.equal(h.reporter.capture().snapshot.render_state, "waiting");
  await h.advance(1100);
  assert.equal(h.reporter.capture().snapshot.render_state, "stalled");
  h.incumbent.readyState = 4;
  assert.equal(h.reporter.capture().snapshot.render_state, "rendering");
  h.reporter.stop();
});

test("first-frame commit freezes the incumbent capture before its actual physical retirement", async () => {
  const held = deferred();
  const h = harness({ send: (request, count) => count === 1 ? Promise.resolve(answer(request)) : held.promise });
  await flush(); h.expose(); h.prove();
  const final = h.reporter.terminalCapture;
  assert.ok(final); assert.equal(final.snapshot.acknowledgement.state, "committed");
  assert.equal(final.snapshot.position_ms, 2257000);
  assert.equal(final.snapshot.acknowledgement.committed_media_origin_ms, 0);
  h.retire();
  assert.equal(h.incumbent.readyState, 0); assert.equal(h.incumbent.currentTime, 0);
  assert.equal(h.state.predecessor, null);
  assert.strictEqual(h.reporter.capture(), final);
  h.context.notifyPlaybackControl("stalled");
  await h.advance(250);
  assert.equal(h.sent[1].request.position_ms, 2257000);
  assert.equal(h.sent[1].request.render_state, "rendering");
  held.resolve(answer(h.sent[1].request)); await flush();
  assert.equal(h.reporter.stopped, true);
  assert.equal(h.p.controlReporter, null, "accepted commit closes the old reporter synchronously");
});

for (const proven of [false, true]) test(`old progress responses cannot publish predecessor processing or prepare actions after ${proven ? "first-frame proof and retirement" : "exposure"}`, async () => {
  const held = deferred(); const h = harness({ send: () => held.promise }); h.expose();
  if (proven) { h.prove(); h.retire(); }
  const visibleReport = { successor: true }; h.p.effectiveProcessing = visibleReport;
  const request = h.sent[0].request;
  held.resolve(answer(request, { effective_processing: { predecessor: true }, action: {
    type: "prepare", action_id: actionId, session_id: nextSession,
    playlist_url: `/api/v1/hls/${nextSession}/index.m3u8`, media_origin_ms: 0,
    effective_selection: { quality_auto: true, height: 2160, audio_track: null, subtitle_burn: null,
      audio_offset_ms: 0, codec: "server_selected", dynamic_range: "dolby_vision" },
  } })); await flush();
  assert.strictEqual(h.p.effectiveProcessing, visibleReport);
  assert.equal(h.handled.length, 0);
  assert.equal(h.reporter.stopped, false);
  h.reporter.stop();
});

test("Shared lost commit response retries the exact final instead of reopening", async () => {
  let attempts = 0;
  const h = harness({ shared: true, send: request => {
    if (request.acknowledgement?.state === "committed" && ++attempts === 1)
      return Promise.reject(Object.assign(new Error("lost response"), { retryAfterMs: 250 }));
    return Promise.resolve(answer(request));
  } });
  await flush(); h.expose(); h.prove(); h.retire(); await h.advance(250);
  assert.equal(h.recovered.length, 0);
  assert.equal(h.reporter.stopped, false);
  await h.advance(250);
  assert.strictEqual(h.sent[1].request, h.sent[2].request);
  assert.equal(h.recovered.length, 0);
  assert.equal(h.reporter.stopped, true);
  assert.equal(h.p.fileContext.session_id, nextSession);
});

test("teardown settles frozen commit then end despite a lost commit response and disposed incumbent", async () => {
  let attempts = 0;
  const h = harness({ send: request => {
    if (request.acknowledgement?.state === "committed" && ++attempts === 1)
      return Promise.reject(Object.assign(new Error("lost response"), { retryAfterMs: 250 }));
    return Promise.resolve(answer(request));
  } });
  await flush(); h.expose(); h.prove(); h.retire();
  h.context.stopPlaybackControl(h.p);
  h.p.mediaAttachment = null; h.p.sessionId = null;
  await h.advance(250);
  assert.equal(h.sent.length, 2);
  assert.equal(h.sent[1].request.demand, "active");
  await h.advance(250);
  assert.strictEqual(h.sent[1].request, h.sent[2].request);
  await h.advance(250);
  const ending = h.sent[3].request;
  assert.equal(ending.demand, "end"); assert.equal(ending.acknowledgement, null);
  assert.equal(ending.position_ms, 2257000); assert.equal(ending.render_state, "rendering");
  assert.equal(ending.generation, ownerGeneration);
  assert.equal(h.reporter.stopped, true);
  const sent = h.sent.length; await h.advance(5000); assert.equal(h.sent.length, sent);
});

test("failed first-frame proof rolls observation ownership back to the incumbent", async () => {
  const h = harness(); await flush(); h.expose(); h.rollback();
  assert.equal(h.context.document.getElementById("video"), h.incumbent);
  assert.equal(h.p.sessionId, oldSession); assert.equal(h.p.offset, 2200);
  assert.equal(h.p.preparedCommitting, null);
  const view = h.reporter.capture().snapshot;
  assert.equal(view.render_state, "rendering"); assert.equal(view.position_ms, 2257000);
  assert.equal(view.acknowledgement.state, "failed");
  assert.equal(h.reporter.terminalCapture, null);
  h.reporter.stop();
});

test("a queued earlier action ACK settles before the frozen visible commit", async () => {
  const h = harness(); await flush();
  h.context.queuePlaybackControlAcknowledgement(h.p,
    "66666666-6666-4666-8666-666666666666", "aborted");
  h.expose(); h.prove(); h.retire();
  await h.advance(250); await h.advance(250);
  assert.deepEqual(h.sent.slice(1).map(item => item.request.acknowledgement?.state),
    ["aborted", "committed"]);
  assert.equal(h.p.controlAcknowledgements.length, 0);
  assert.equal(h.reporter.stopped, true);
});

for (const failure of ["410", "terminal", "accepted_end"]) test(`${failure} during exposure rolls back and fences a late first frame`, async () => {
  const held = deferred();
  const h = harness({ send: () => held.promise });
  if (failure === "accepted_end") {
    // The held request must be a real END request, authored before exposure.
    h.reporter.stop();
    h.incumbent.ended = true; h.incumbent.currentTime = 5000; h.p.durMs = 7200000;
    h.context.startPlaybackControl(h.incumbent, h.p, { protocol: control.PROTOCOL,
      url: `/api/v1/hls/${oldSession}/control`, generation: ownerGeneration,
      control_epoch: 1, next_exchange_ms: 1000, lease_timeout_ms: 300000 });
    h.reporter = h.p.controlReporter;
    assert.equal(h.sent[1].request.demand, "end");
  }
  h.expose();
  if (failure === "410") held.reject(Object.assign(new Error("ended"), {
    status: 410, code: "session_ended", terminalReason: "replaced",
  }));
  else held.resolve(answer(h.sent.at(-1).request, failure === "terminal"
    ? { action: { type: "terminal", code: "producer_failed", message: "source ended" } } : {}));
  await flush();
  assert.equal(h.p.preparedCommitting, null);
  assert.equal(h.state.frameCancelled, true);
  assert.equal(h.context.document.getElementById("video"), h.incumbent);
  assert.equal(h.p.sessionId, oldSession);
  assert.equal(h.reporter.stopped, true);
  h.prove();
  assert.equal(h.p.preparedControlPending, undefined);
  assert.equal(h.p.controlAcknowledgements.some(ack => ack.state === "committed"), false);
  assert.equal(h.state.state, "failed");
});

test("older queued ACK and ended incumbent remain active until the visible commit settles", async () => {
  const h = harness(); await flush();
  h.context.queuePlaybackControlAcknowledgement(h.p,
    "66666666-6666-4666-8666-666666666666", "aborted");
  h.expose();
  h.incumbent.ended = true; h.incumbent.currentTime = 5000;
  h.prove(); h.retire();
  await h.advance(250); await h.advance(250);
  const final = h.sent.slice(1).map(item => item.request);
  assert.deepEqual(final.map(request => request.acknowledgement?.state), ["aborted", "committed"]);
  assert.deepEqual(final.map(request => request.demand), ["active", "active"]);
  assert.equal(h.p.controlAcknowledgements.length, 0);
  assert.equal(h.reporter.stopped, true);
});

test("owner_changed on the frozen visible commit is surfaced without retargeting", async () => {
  const h = harness({ shared: true, send: request => request.acknowledgement?.state === "committed"
    ? Promise.reject(Object.assign(new Error("owner moved"), { status: 409, code: "owner_changed",
      generation: "77777777-7777-4777-8777-777777777777", controlEpoch: 2 }))
    : Promise.resolve(answer(request)) });
  await flush(); h.expose(); h.prove(); h.retire(); await h.advance(250);
  assert.equal(h.recovered.length, 1);
  assert.equal(h.reporter.bootstrap.generation, ownerGeneration);
  assert.equal(h.reporter.stopped, true);
  assert.equal(h.sent[1].request.generation, ownerGeneration);
});

test("terminal response to the final commit cannot settle or start the successor", async () => {
  const bootstrap = { protocol: control.PROTOCOL, generation: "77777777-7777-4777-8777-777777777777",
    url: `/api/v1/hls/${nextSession}/control`, control_epoch: 1,
    next_exchange_ms: 1000, lease_timeout_ms: 300000 };
  const h = harness({ shared: true, successorBootstrap: bootstrap, send: request =>
    Promise.resolve(answer(request, request.acknowledgement?.state === "committed"
      ? { action: { type: "terminal", code: "producer_failed", message: "source ended" } } : {})) });
  await flush(); h.expose(); h.prove(); h.retire(); await h.advance(250);
  assert.equal(h.recovered.length, 1);
  assert.equal(h.p.fileContext.session_id, oldSession);
  assert.ok(h.p.preparedControlPending, "the terminal answer did not consume the successful-commit settlement");
  assert.equal(h.reporter.stopped, true);
  await h.advance(0);
  assert.equal(h.sent.some(item => item.url === bootstrap.url), false);
});

for (const lost of ["aborted", "committed"]) test(`close drains older ACK then commit then END when ${lost} response is lost`, async () => {
  let lostOnce = false;
  const h = harness({ send: request => {
    if (request.acknowledgement?.state === lost && !lostOnce) {
      lostOnce = true;
      return Promise.reject(Object.assign(new Error("lost response"), { retryAfterMs: 250 }));
    }
    return Promise.resolve(answer(request));
  } });
  await flush();
  h.context.queuePlaybackControlAcknowledgement(h.p,
    "66666666-6666-4666-8666-666666666666", "aborted");
  h.expose(); h.prove(); h.retire(); h.context.stopPlaybackControl(h.p);
  h.p.mediaAttachment = null; h.p.sessionId = null;
  for (let turn = 0; turn < 5; turn++) await h.advance(250);
  const requests = h.sent.slice(1).map(item => item.request);
  const states = requests.map(request => request.acknowledgement?.state || "end");
  assert.deepEqual(states, lost === "aborted"
    ? ["aborted", "aborted", "committed", "end"]
    : ["aborted", "committed", "committed", "end"]);
  const retry = lost === "aborted" ? 0 : 1;
  assert.strictEqual(requests[retry], requests[retry + 1]);
  assert.deepEqual(requests.map(request => request.demand), ["active", "active", "active", "end"]);
  assert.equal(requests.every(request => request.position_ms === 2257000), true);
  assert.equal(requests.every(request => request.generation === ownerGeneration), true);
  assert.equal(h.reporter.stopped, true);
});

test("ready incumbent retains its authored failed HLS observation during successor exposure", async () => {
  const h = harness(); await flush();
  h.p.controlRenderOverride = "failed";
  h.p.controlObservationOverride = { decoder_state: "failed", error_code: "network",
    error_detail: "fragment load fatal" };
  assert.equal(h.incumbent.readyState, 4);
  h.expose();
  const observation = h.reporter.capture().snapshot;
  assert.equal(observation.render_state, "failed");
  assert.equal(observation.observation.decoder_state, "failed");
  assert.equal(observation.observation.error_code, "network");
  assert.equal(observation.observation.error_detail, "fragment load fatal");
  h.reporter.stop();
});

test("ready incumbent retains its authored stalled verdict during successor exposure", async () => {
  const h = harness(); await flush();
  h.p.controlRenderOverride = "stalled";
  assert.equal(h.incumbent.readyState, 4);
  h.expose();
  assert.equal(h.reporter.capture().snapshot.render_state, "stalled");
  h.reporter.stop();
});

test("paused source keeps hold and zero rate through an older ACK and final commit", async () => {
  const h = harness(); await flush();
  h.context.queuePlaybackControlAcknowledgement(h.p,
    "66666666-6666-4666-8666-666666666666", "aborted");
  h.p.wantsPlayback = false; h.incumbent.paused = true; h.incumbent.playbackRate = 0;
  h.expose(); h.prove(); h.retire();
  await h.advance(250); await h.advance(250);
  const requests = h.sent.slice(1).map(item => item.request);
  assert.deepEqual(requests.map(request => request.acknowledgement?.state), ["aborted", "committed"]);
  assert.deepEqual(requests.map(request => request.demand), ["hold", "hold"]);
  assert.deepEqual(requests.map(request => request.playback_rate), [0, 0]);
  assert.equal(h.p.controlAcknowledgements.length, 0);
  assert.equal(h.reporter.stopped, true);
});

for (const lost of [false, true]) for (const closing of [false, true])
  test(`Shared in-flight ABORT settles once before commit${closing ? " then close" : ""}${lost ? " with exact retry" : ""}`, async () => {
    const held = deferred();
    let abortedSequence = null, abortedSends = 0;
    const h = harness({ shared: true, send: request => {
      if (request.acknowledgement?.state === "aborted") {
        if (abortedSequence !== null && request.sequence !== abortedSequence)
          return Promise.reject(Object.assign(new Error("duplicate ABORT at new sequence"), { status: 409, code: "stale_control" }));
        abortedSequence = request.sequence;
        if (++abortedSends === 1) return held.promise;
      }
      return Promise.resolve(answer(request));
    } });
    await flush();
    h.context.queuePlaybackControlAcknowledgement(h.p,
      "66666666-6666-4666-8666-666666666666", "aborted");
    await h.advance(250);
    const earlier = h.sent.at(-1).request;
    assert.equal(earlier.acknowledgement.state, "aborted");
    h.expose(); h.prove(); h.retire();
    if (closing) {
      h.context.stopPlaybackControl(h.p);
      h.p.mediaAttachment = null; h.p.sessionId = null;
    }
    if (lost) held.reject(Object.assign(new Error("ABORT answer lost"), {
      status: 429, code: "control_rate_limited", retryAfterMs: 250,
    }));
    else held.resolve(answer(earlier));
    await flush();
    for (let turn = 0; turn < 4; turn++) await h.advance(250);
    const requests = h.sent.slice(1).map(item => item.request);
    const expected = lost ? ["aborted", "aborted", "committed"] : ["aborted", "committed"];
    if (closing) expected.push("end");
    assert.deepEqual(requests.map(request => request.acknowledgement?.state || "end"), expected);
    assert.equal(new Set(requests.filter(request => request.acknowledgement?.state === "aborted")
      .map(request => request.sequence)).size, 1, "Shared never receives a second ABORT sequence");
    if (lost) assert.strictEqual(requests[0], requests[1]);
    assert.equal(h.recovered.length, 0);
    assert.equal(h.reporter.stopped, true);
  });
