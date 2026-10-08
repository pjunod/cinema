"use strict";

/*
 * The playback lab's shaping layer, tested without a browser.
 *
 * Everything here is the part of the stall-recovery harness that decides
 * whether a run means anything: the profile grammar, the rate limiter, the
 * cliff, and the scoring that separates "the link was shaped and the session
 * failed to adapt" from "the harness never shaped anything." A green
 * stall-recovery run whose shaper silently did nothing would be worse than no
 * harness at all, so those failure modes are asserted directly.
 */

const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const fs = require("node:fs");
const fsp = fs.promises;
const http = require("node:http");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { spawn, spawnSync } = require("node:child_process");

const lab = require("../../scripts/playback-lab");

const ROOT = path.resolve(__dirname, "..", "..");
const LAB = path.join(ROOT, "scripts", "playback-lab");

const failures = [];
const pending = [];

function test(name, run) {
  pending.push([name, run]);
}

async function runAll() {
  let executed = 0;
  for (const [name, run] of pending) {
    if (process.env.PLAYBACK_LAB_TEST_FILTER && !new RegExp(process.env.PLAYBACK_LAB_TEST_FILTER).test(name)) continue;
    executed++;
    try {
      await run();
      process.stdout.write(`PASS ${name}\n`);
    } catch (error) {
      failures.push(`${name}: ${error.message}`);
      process.stdout.write(`FAIL ${name}: ${error.message}\n`);
    }
  }
  if (!executed) failures.push("test filter matched no contracts");
  return executed;
}

test("D3 intent samples are independent of transport pause and retain lifecycle, replacement and stop", () => {
  const vm = require("node:vm");
  let now = 0, tick;
  const listeners = new Map();
  let element = { currentTime: 1, paused: true, seeking: false, ended: false, videoHeight: 720 };
  const context = { performance: { now: () => now }, WeakMap, AUTO_SWITCH_SEQ: 0,
    PLAYER: { wantsPlayback: true, sessionId: "a", attemptId: "one", offset: 0 },
    document: { visibilityState: "visible", getElementById: () => element,
      addEventListener: (name, fn) => listeners.set(name, fn), removeEventListener: name => listeners.delete(name) },
    setInterval: fn => { tick = fn; return 1; }, clearInterval: () => { tick = null; } };
  vm.createContext(context);
  vm.runInContext(`(${lab.installD3Acquisition.toString()})(32,100)`, context);
  now = 100; tick();
  context.AUTO_SWITCH_SEQ = 1;
  context.PLAYER.abr = { switches: [{ seq: 1, at_ms: 101, from_height: 720,
    to_height: 360, target_session_id: "b", target_attempt_id: "two" }] };
  context.document.visibilityState = "hidden"; now = 120; listeners.get("visibilitychange")();
  element = { ...element, currentTime: 0 }; context.PLAYER.sessionId = "b";
  now = 200; tick();
  now = 300; context.__plurxLabD3.stop("terminal-unrecovered");
  const capture = JSON.parse(JSON.stringify(context.__plurxLabD3));
  assert.equal(capture.records.find(r => r.kind === "start").wants_playback, true,
    "paused transport must not erase independent viewer intent");
  assert.ok(capture.records.some(r => r.kind === "lifecycle" && r.visible === false));
  assert.equal(capture.records.filter(r => r.kind === "attachment").length, 2);
  assert.equal(capture.records.at(-1).reason, "terminal-unrecovered");
  assert.equal(capture.auto_switch_end - capture.auto_switch_baseline, 1);
  assert.equal(capture.records.find(r => r.kind === "automatic_switch").target_session_id, "b");
  assert.equal(tick, null);
  assert.equal(listeners.size, 0);
  assert.equal(lab.d3PresentationEvidence(capture).stalled_seconds, null);
});

test("D3 overflow preserves the head and makes loss and censored absence explicit", () => {
  const vm = require("node:vm");
  let now = 0;
  const context = { performance: { now: () => ++now }, WeakMap,
    document: { visibilityState: "visible", getElementById: () => null,
      addEventListener() {}, removeEventListener() {} }, setInterval: () => 1, clearInterval() {} };
  vm.createContext(context);
  vm.runInContext(`(${lab.installD3Acquisition.toString()})(1,100)`, context);
  context.__plurxLabD3.stop("observation-end");
  const capture = JSON.parse(JSON.stringify(context.__plurxLabD3));
  assert.equal(capture.records.length, 1);
  assert.ok(capture.dropped > 0);
  const evidence = lab.d3PresentationEvidence(capture);
  assert.equal(evidence.status, "incomplete");
  assert.equal(evidence.stationary_integral_bounds_seconds, null);
  assert.equal(evidence.automatic_switches, null);
  assert.ok(evidence.missing.includes("record limit exceeded"));
});

test("D3 skipped and backward composition frames never become an exact zero stall", () => {
  const capture = { stopped: true, records: [
    { kind: "start", at_ms: 0, wants_playback: true, visible: true, seeking: false, ended: false, element_id: 1, current_time: 1 },
    { kind: "composition_submission", at_ms: 10, visible: true, element_id: 1, media_time: 1, presented_frames: 1 },
    { kind: "composition_submission", at_ms: 50, visible: true, element_id: 1, media_time: .9, presented_frames: 4 },
    { kind: "stop", at_ms: 100, wants_playback: true, visible: true, seeking: false, ended: false, element_id: 1, current_time: 1 },
    { kind: "censor", at_ms: 100, reason: "terminal" },
  ] };
  const result = lab.d3PresentationEvidence(capture);
  assert.equal(result.skipped_submissions, 2);
  assert.equal(result.backward_frames, 1);
  assert.equal(result.sampled_equal_clock_seconds, .1);
  assert.equal(result.stalled_seconds, null);
  assert.equal(result.stationary_integral_bounds_seconds, null);
  assert.equal(result.terminal_censor[0].reason, "terminal");
});

test("D3 clock brackets bound drift and frame times without interpolating an offset", () => {
  const anchors = [{ sent_ms: 100, received_ms: 110, browser_ms: 20 },
    { sent_ms: 200, received_ms: 230, browser_ms: 110 }];
  const alignment = lab.d3ClockAlignment(anchors);
  assert.equal(alignment.status, "bracketed");
  assert.equal(alignment.drift_lower_ms, 0);
  assert.equal(alignment.drift_upper_ms, 40);
  assert.deepEqual(lab.d3ControllerTimeBounds(50, anchors), [100, 230]);
  assert.equal(lab.d3ControllerTimeBounds(5, anchors), null);
  assert.equal(lab.d3ClockAlignment(anchors.slice(0, 1)).status, "missing");
  assert.equal(lab.d3ClockAlignment([anchors[0], { ...anchors[1], browser_ms: 10 }]).status, "missing");
});

test("D3 two cliffs use exact final sixty-second completion boundaries and keep advertisements separate", () => {
  const stage = { index: 1, entered_at_ms: 12_000, left_at_ms: 87_000 };
  const samples = [{ at_ms: 26_999, bytes: 900, media: true, session_id: "a" },
    { at_ms: 27_000, bytes: 500, media: true, session_id: "a" },
    { at_ms: 27_001, bytes: 60_000, media: true, session_id: "a" },
    { at_ms: 87_000, bytes: 60_000, media: true, session_id: "a" },
    { at_ms: 87_001, bytes: 999, media: true, session_id: "a" },
    { at_ms: 50_000, bytes: 999, media: false, session_id: "a" }];
  const capture = { stopped: true, records: [
    { kind: "composition_submission", visible: true, session_id: "a", height: 240 },
    { session_id: "a", ladder: [{ height: 240, total_kbps: 900 }] },
  ] };
  const first = lab.d3FinalDeliveryWindow(stage, samples, 0, capture);
  assert.equal(first.media_bytes, 120_000);
  assert.equal(first.delivered_kbps, 16);
  assert.equal(first.advertised_total_kbps, 900);
  assert.equal(first.samples.length, 2);
  assert.match(first.interval, /socket-completion.*not client consumption/);
  const second = lab.d3FinalDeliveryWindow({ index: 2, entered_at_ms: 87_000, left_at_ms: 162_000 },
    [{ at_ms: 102_001, bytes: 30_000, media: true, session_id: "b" }]);
  assert.equal(second.socket_completion_media_bytes, 30_000);
  assert.equal(second.media_bytes, null);
  assert.equal(second.delivered_kbps, null);
  assert.equal(second.advertised_total_kbps, null);
  assert.ok(second.missing.length);
  assert.equal(lab.d3FinalDeliveryWindow(stage, samples, 1).media_bytes, null);
  assert.equal(lab.d3FinalDeliveryWindow({ ...stage, left_at_ms: 50_000 }, samples).media_bytes, null);
});

test("D3 ambiguous or missing rung evidence stays missing instead of using the cap", () => {
  const stage = { index: 1, kbps: 1500, entered_at_ms: 0, left_at_ms: 75_000 };
  const result = lab.d3FinalDeliveryWindow(stage, [{ at_ms: 20_000, media: true, bytes: 123, session_id: null }]);
  assert.equal(result.advertised_total_kbps, null);
  assert.ok(result.missing.some(reason => reason.includes("cannot be attributed")));
});

test("D3 write acquisition timestamps callback completion without changing limiter settlement", async () => {
  const { Writable } = require("node:stream");
  const shaper = new lab.ShapingProxy(lab.parseNetworkProfile("8mbps-to-1.5mbps"), "http://127.0.0.1:1");
  let now = 27_001, complete;
  shaper.startedAt = 0;
  shaper.now = () => now;
  shaper.reserveSlice = async () => null;
  const sink = new Writable({ highWaterMark: 64 * 1024,
    write(_chunk, _encoding, callback) { complete = callback; } });
  await shaper.writeShaped(Buffer.alloc(1024), sink, true, () => false, "session");
  assert.equal(sink.writableLength, 1024);
  assert.equal(shaper.deliverySamples[0][0].at_ms, 27_001, "legacy accepted-write accounting stays unchanged");
  assert.equal(shaper.d3DeliverySamples[0].length, 0, "buffer admission is not completion");
  assert.equal(shaper.d3WriteRecords[0].status, "pending");
  const stage = { index: 0, entered_at_ms: 12_000, left_at_ms: 87_000 };
  let evidence = { records: shaper.d3WriteRecords, dropped: 0 };
  assert.equal(lab.d3FinalDeliveryWindow(stage, [], 0, null, evidence).socket_completion_media_bytes, null);
  now = 87_001; shaper.stageIndex = 1; complete();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(shaper.d3WriteRecords[0].completed_at_ms, 87_001);
  assert.equal(shaper.d3DeliverySamples[1][0].at_ms, 87_001);
  assert.equal(lab.d3FinalDeliveryWindow(stage, shaper.d3DeliverySamples.flat(), 0, null, evidence)
    .socket_completion_media_bytes, 0, "completion past the window is excluded");
  await shaper.writeShaped(Buffer.alloc(100), { writable: true, destroyed: false,
    write(_slice, callback) { callback(Object.assign(new Error("fixture failure"), { code: "EPIPE" })); return true; } },
  true, () => false, "session");
  assert.equal(shaper.d3WriteRecords.at(-1).status, "failed");
  assert.equal(shaper.d3WriteRecords.at(-1).callback_at_ms, 87_001);
  const later = { index: 1, entered_at_ms: 87_000, left_at_ms: 162_000 };
  assert.equal(lab.d3FinalDeliveryWindow(later, shaper.d3DeliverySamples[1], 0, null, evidence)
    .socket_completion_media_bytes, null, "failed completion cannot mean zero lost bytes");
  assert.equal(lab.d3FinalDeliveryWindow(later, [], 0, null, { records: [], dropped: 1 })
    .socket_completion_media_bytes, null);
  sink.destroy(); shaper.agent.destroy();
});

test("D3 automatic event provenance separates method-only reopens from proven rung changes", () => {
  const vm = require("node:vm");
  let now = 0, tick;
  const element = { currentTime: 1, seeking: false, ended: false, videoHeight: 720 };
  const context = { performance: { now: () => now }, WeakMap, AUTO_SWITCH_SEQ: 0,
    PLAYER: { wantsPlayback: true, sessionId: "session", attemptId: "attempt", abr: { switches: [] } },
    document: { visibilityState: "visible", getElementById: () => element,
      addEventListener() {}, removeEventListener() {} },
    setInterval: fn => { tick = fn; return 1; }, clearInterval() {} };
  vm.createContext(context);
  vm.runInContext(`(${lab.installD3Acquisition.toString()})(32,100)`, context);
  context.__plurxLabD3.appendFrame(element, 0, { mediaTime: 1, presentedFrames: 1, height: 720 }, context.PLAYER);
  now = 100; context.AUTO_SWITCH_SEQ = 2;
  context.PLAYER.abr.switches = [
    { seq: 1, at_ms: 90, from: "copy_hls", to: "transcode", reason: "auto supply",
      target_method: "transcode", position: 10, from_height: null, to_height: null },
    { seq: 2, at_ms: 99, from: "720p", to: "360p", reason: "bandwidth cliff", from_height: 720, to_height: 360 },
  ];
  tick(); now = 200; context.__plurxLabD3.stop("observation-end");
  const capture = JSON.parse(JSON.stringify(context.__plurxLabD3));
  let measured = lab.d3PresentationEvidence(capture);
  assert.equal(measured.automatic_event_sequence_complete, true);
  assert.equal(measured.automatic_events, 2);
  assert.equal(measured.proven_automatic_rung_changes, 1);
  assert.equal(measured.automatic_switches, null, "unknown Auto heights must not be counted as rung changes");
  const method = capture.records.find(r => r.kind === "automatic_switch");
  assert.equal(method.from, "copy_hls"); assert.equal(method.to, "transcode");
  assert.equal(method.reason, "auto supply"); assert.equal(method.position, 10);
  assert.equal(method.target_method, "transcode");
  const knownOnly = { ...capture, auto_switch_baseline: 1,
    records: capture.records.filter(r => r.kind !== "automatic_switch" || r.switch_seq === 2) };
  assert.equal(lab.d3PresentationEvidence(knownOnly).automatic_switches, 1);
  assert.equal(lab.d3PresentationEvidence({ ...knownOnly, stopped: false }).automatic_switches, null,
    "a last sample is not a completed observation boundary");
  for (const record of capture.records) if (Object.hasOwn(record, "current_time")) record.current_time = null;
  measured = lab.d3PresentationEvidence(capture);
  assert.equal(measured.sampled_equal_clock_seconds, null, "missing sampled clocks are not zero stationary seconds");
  assert.ok(measured.missing.includes("sampled media clock unavailable"));
  capture.records.find(r => r.kind === "start").wants_playback = null;
  assert.equal(lab.d3PresentationEvidence(capture).sampled_intent_eligible_seconds, null);
});

test("D3 normalization preserves failed raw acquisition, null metrics and provenance verbatim", () => {
  const acquisition = { acceptance: "incomplete", clock_alignment: { anchors: [{ sent_ms: 12 }] },
    presentation: { stalled_seconds: null, backward_frames: 1, raw: { records: [{ at_ms: 4 }] } },
    missing: ["physical grade unavailable"] };
  const provenance = { runtime_build: "unqualified", missing: ["binary digest unavailable"] };
  const report = { schema_version: 1, summary: { failed: 1 }, d3_provenance: provenance,
    results: [{ name: "failed", status: "failed", d3_acquisition: acquisition }] };
  const normalized = lab.normalizeTrace(report);
  assert.deepEqual(normalized.results[0].d3_acquisition, acquisition);
  assert.deepEqual(normalized.d3_provenance, provenance);
  assert.equal(normalized.results[0].status, "failed");
  assert.equal(Object.hasOwn(lab.normalizeTrace({ results: [{}] }).results[0], "d3_acquisition"), false);
});

function cli(args) {
  return spawnSync(process.execPath, [LAB, ...args], { encoding: "utf8", cwd: ROOT });
}

test("quality baseline requires an advancing outgoing frame rather than preload", () => {
  const snapshot = {frame_probe: {supported: true, sequence: 1,
    last_frame_at_ms: 2400, last_frame: {media_time: 0}},
    video: {paused: false, seeking: false}};
  assert.equal(lab.advancingOutgoingFrame(snapshot, 0), false);
  snapshot.frame_probe.last_frame.media_time = 0.041667;
  assert.equal(lab.advancingOutgoingFrame(snapshot, 1), false);
  snapshot.frame_probe.sequence = 2;
  assert.equal(lab.advancingOutgoingFrame(snapshot, 1), true);
  snapshot.video.paused = true;
  assert.equal(lab.advancingOutgoingFrame(snapshot, 1), false);
  snapshot.video.paused = false;
  snapshot.video.seeking = true;
  assert.equal(lab.advancingOutgoingFrame(snapshot, 1), false);
  assert.ok(Math.abs(lab.frameGapSince(2900, 3416.6) - 516.6) < 1e-6,
    "a real outgoing-frame blackout is still measured in full");
});

test("presentation gaps retain late callback diagnostics and refuse invalid display evidence", () => {
  // Exact Firefox receipt: the first callback precedes display by one refresh;
  // the next arrives at display. Dispatch spacing is not presentation spacing.
  const before={at_ms:71831.76,expected_display_time_ms:71848.72,
    media_time:69.5,presented_frames:1669,element_id:1};
  const after={at_ms:71933.34,expected_display_time_ms:71933.34,
    media_time:69.583333,presented_frames:1671,element_id:1};
  const gap=lab.framePresentationGap(before,after);
  assert.equal(gap.clock,"expected_display");
  assert.ok(Math.abs(gap.gap_ms-84.62)<1e-6);
  assert.ok(Math.abs(gap.callback_gap_ms-101.58)<1e-6);
  const stalled={...after,at_ms:72000,expected_display_time_ms:72000};
  assert.ok(lab.framePresentationGap(before,stalled).gap_ms>100,
    "a real compositor gap still exceeds the unchanged presentation bound");
  for(const invalid of [
    {...after,expected_display_time_ms:null},
    {...after,expected_display_time_ms:71840},
    {...after,expected_display_time_ms:71933.34+10000},
    {...after,presented_frames:1669},
    {...after,media_time:69.4},
  ]){
    const refused=lab.framePresentationGap(before,invalid);
    assert.equal(refused.clock,"callback");
    assert.ok(refused.gap_ms>100,"unknown evidence cannot erase the raw failure");
  }
  const scored=lab.transitionMetrics({media_event_seq:0},{
    media_event_seq:0,media_events:[],sampled_at_ms:71940,
    frame_probe:{supported:true,last_frame_at_ms:after.at_ms,last_frame:after,
      maximum_gap_ms:gap.gap_ms,maximum_callback_gap_ms:gap.callback_gap_ms},
    video:{paused:false,ended:false},
  });
  assert.ok(Math.abs(scored.maximum_video_gap_ms-84.62)<1e-6);
  assert.ok(Math.abs(scored.maximum_callback_gap_ms-101.58)<1e-6);
  const silent=lab.transitionMetrics({media_event_seq:0},{
    media_event_seq:0,media_events:[],sampled_at_ms:72100,
    frame_probe:{supported:true,last_frame_at_ms:after.at_ms,last_frame:after,maximum_gap_ms:0},
    video:{paused:false,ended:false},
  });
  assert.ok(silent.maximum_video_gap_ms>100,"an open presentation gap must still fail");
});

test("steady frame window excludes preload time but preserves later and switch gaps", () => {
  assert.ok(Math.abs(lab.frameGapSince(4921.2, 5421.2, 5300) - 121.2) < 1e-6);
  assert.equal(lab.frameGapSince(5421.2, 5921.2, 5300), 500,
    "a blackout wholly inside the observed window still fails the 250 ms bound");
  assert.equal(lab.frameGapSince(4921.2, 5421.2), 500,
    "a quality switch retains the complete outgoing-to-target frame gap");
  assert.equal(lab.frameGapSince(null, 5421.2, 5300), 0,
    "missing first-frame evidence is not invented by a window origin");
  const measured = lab.transitionMetrics({media_event_seq:0}, {
    media_event_seq:0,media_events:[],sampled_at_ms:5921.2,
    frame_probe:{supported:true,last_frame_at_ms:4921.2,
      maximum_gap_ms:0,measurement_start_at_ms:5300},
    video:{paused:false,ended:false},
  });
  assert.ok(Math.abs(measured.maximum_video_gap_ms - 621.2) < 1e-6,
    "no new callback after the origin remains an open blackout");
});

test("VOD readiness requires a pass that stored an index", () => {
  const empty = { message: "fragment indexing pass finished attempted=1 built=0" };
  const ready = { message: "fragment indexing pass finished attempted=2 built=1" };
  assert.equal(lab.completedFragmentIndexPass([empty]), null,
    "an attempted but empty pass cannot make a VOD player ready");
  assert.equal(lab.completedFragmentIndexPass([empty, ready]), ready);
  assert.equal(lab.completedFragmentIndexPass(null), null);
});

// ---------------------------------------------------------------- profiles

test("a malformed network profile is refused, with the reason named", () => {
  const rejected = [
    ["8mbps", "no descent"],
    ["8mbps-to-8mbps", "flat is not a cliff"],
    ["1mbps-to-8mbps", "rising is not a cliff"],
    ["8mbps-to-0mbps", "zero rate"],
    ["-8mbps-to-1mbps", "negative rate"],
    ["8gbps-to-1gbps", "unknown unit"],
    ["eightmbps-to-1mbps", "not a number"],
    ["8mbps-to-1.5mbps@0", "cliff delay must be positive"],
    ["8mbps-to-1.5mbps@later", "cliff delay must be a number"],
    ["8mbps-to-1.5mbps@12@20", "multiple cliff delays"],
    ["fast-link", "unknown named profile"],
    [true, "flag with no value"],
  ];
  for (const [spec, why] of rejected) {
    assert.throws(() => lab.parseNetworkProfile(spec), Error, `${JSON.stringify(spec)} (${why}) should be refused`);
  }
});

test("the acceptance profile parses into a recorded, descending cliff", () => {
  const profile = lab.parseNetworkProfile("8mbps-to-1.5mbps");
  assert.equal(profile.id, "8mbps-to-1.5mbps");
  assert.equal(profile.stages[0].kbps, 8000);
  assert.equal(profile.stages[1].kbps, 1500);
  assert.ok(profile.cliff_after_seconds > 0, "the cliff point must be recorded");
  assert.ok(profile.description.includes("8 Mb/s"), "the named profile keeps its description");
  assert.equal(lab.parseNetworkProfile("8mbps-to-1.5mbps@20").cliff_after_seconds, 20);
  assert.equal(lab.parseNetworkProfile("1500kbps-to-500kbps").stages[0].kbps, 1500);
});

test("a physical-device profile can descend through three exact link stages", () => {
  const profile = lab.parseNetworkProfile("8mbps-to-1.1mbps-to-350kbps@15");
  assert.deepEqual(profile.stages.map((stage) => stage.kbps), [8000, 1100, 350]);
  assert.deepEqual(profile.stages.map((stage) => stage.label), [
    "before-cliff", "after-cliff", "after-cliff-2",
  ]);
  assert.equal(profile.cliff_after_seconds, 15);
  assert.throws(
    () => lab.parseNetworkProfile("8mbps-to-1mbps-to-2mbps"),
    /must descend at every stage/,
  );
});

test("browser run accepts two scored cliffs and rejects an unscored third", () => {
  assert.doesNotThrow(() => lab.requireSupportedRunProfile(
    lab.parseNetworkProfile("8mbps-to-1.5mbps"),
  ));
  assert.doesNotThrow(() => lab.requireSupportedRunProfile(
    lab.parseNetworkProfile("8mbps-to-1.1mbps-to-350kbps"),
  ));
  assert.throws(
    () => lab.requireSupportedRunProfile(
      lab.parseNetworkProfile("8mbps-to-1.1mbps-to-350kbps-to-100kbps"),
    ),
    /supports at most two independently scored cliffs/,
  );
});

test("no profile means no shaping, so existing suites keep today's behavior", () => {
  for (const spec of [undefined, null, "", false, "none"]) {
    assert.equal(lab.parseNetworkProfile(spec), null, JSON.stringify(spec));
  }
});

// ------------------------------------------------------------ token bucket

test("the bucket enforces its rate over time and repays debt", () => {
  const bucket = new lab.TokenBucket(1000, 0); // 125 bytes/ms
  const first = bucket.reserve(125_000, 0); // 1000 kb of payload
  assert.ok(first > 900 && first < 1010, `a 1000 kb claim on a 1000 kb/s link waits ~1s, got ${first}ms`);
  const second = bucket.reserve(125_000, 1000);
  assert.ok(second > 900, "the next claim waits again rather than riding free");
});

test("a rate drop cannot be paid for with credit earned before the cliff", () => {
  const bucket = new lab.TokenBucket(8000, 0);
  bucket.refill(10_000); // sit idle and fill to capacity at the high rate
  const bankedHigh = bucket.tokens;
  bucket.setRate(1500, 10_000);
  assert.ok(bucket.tokens <= bucket.capacity, "credit is clamped to the new, smaller bucket");
  assert.ok(bucket.tokens < bankedHigh, "pre-cliff credit does not survive the cliff intact");
});

test("a non-positive rate is refused rather than dividing by zero", () => {
  assert.throws(() => new lab.TokenBucket(0, 0), /positive/);
  assert.throws(() => new lab.TokenBucket(Number.NaN, 0), /positive/);
});

// ------------------------------------------------------------- media paths

test("media supply is told apart from the control plane", () => {
  for (const route of [
    "/hls/abc/index.m3u8", "/hls/abc/000001.ts", "/api/v1/files/12/direct",
    "/api/v1/files/12/stream.mp4", "/api/v1/offline/media/tok/master.m3u8",
  ]) assert.equal(lab.isMediaPath(route), true, route);
  for (const route of ["/", "/api/v1/system", "/api/v1/files/12/decision?caps=x", "/assets/hls.min.js"]) {
    assert.equal(lab.isMediaPath(route), false, route);
  }
});

test("proxy diagnostics strip bearer-token query strings", () => {
  const route = lab.diagnosticPath("/api/v1/files/12/direct?token=secret-lab-token&part=1");
  assert.equal(route, "/api/v1/files/12/direct");
  assert.doesNotMatch(route, /secret-lab-token|token=/);
});

// ------------------------------------------------------------- the shaper

async function withOrigin(bytes, run, { firstResponseDelayMs = 0 } = {}) {
  let requests = 0;
  const origin = http.createServer((request, response) => {
    const reply = () => {
      if (request.url === "/api/v1/server") {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({ instance_id: "test-server", build: "test" }));
        return;
      }
      response.writeHead(200, { "content-type": "application/octet-stream" });
      response.end(Buffer.alloc(request.url === "/healthz" ? 1 : bytes, 0x61));
    };
    if (requests++ === 0 && firstResponseDelayMs > 0) {
      setTimeout(reply, firstResponseDelayMs);
    } else {
      reply();
    }
  });
  await new Promise((resolve) => origin.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${origin.address().port}`;
  try {
    return await run(url);
  } finally {
    await new Promise((resolve) => origin.close(resolve));
  }
}

test("the shaper holds the link, applies the cliff, and records both stages", async () => {
  // Keep each stage well beyond the bucket's 250 ms starting credit and the
  // first recorded slice. A one-second transfer puts that deliberate credit
  // on the 1.25 telemetry boundary and turns scheduler jitter into a false
  // leak; this payload holds the same bound over a multi-second window.
  await withOrigin(256 * 1024, async (origin) => {
    const profile = lab.parseNetworkProfile("1mbps-to-0.5mbps");
    const shaper = new lab.ShapingProxy(profile, origin);
    const base = await shaper.start();
    try {
      // Make both timed transfers pay equivalent connection/JIT costs. The
      // injected first-response delay makes this fail deterministically if the
      // warmup is removed, modeling the compounded cold-fetch and scheduling
      // skew seen under CPU load.
      const warmup = await fetch(`${base}/healthz`);
      await warmup.arrayBuffer();
      shaper.beginEvidence();

      const before = Date.now();
      const first = await fetch(`${base}/api/v1/files/1/direct`);
      await first.arrayBuffer();
      const firstMs = Date.now() - before;
      assert.ok(firstMs > 1_500, `256 KB over a 1 Mb/s link cannot arrive in ${firstMs}ms`);

      const cliffAt = shaper.applyCliff();
      assert.ok(cliffAt > 0, "the cliff point is recorded");

      const afterStart = Date.now();
      const second = await fetch(`${base}/api/v1/files/1/direct`);
      await second.arrayBuffer();
      const secondMs = Date.now() - afterStart;
      // The configured rate halves. Wall-clock fetches also contain fixed HTTP
      // overhead, so require a clearly slower transfer without asserting an
      // impossible strictly-greater-than 2x boundary around timer rounding.
      assert.ok(secondMs > firstMs * 1.75, `the post-cliff fetch must be far slower (${firstMs}ms then ${secondMs}ms)`);

      const telemetry = shaper.telemetry();
      assert.equal(telemetry.stages.length, 2);
      assert.ok(telemetry.cliff_applied_at_ms > 0);
      for (const stage of telemetry.stages) {
        // The bound the harness scores, not the delivered rate: this runs against
        // real sockets, and a delivered rate has no ceiling the design can state.
        assert.ok(
          stage.admitted_bytes <= lab.shaperClaimBoundBytes(stage.kbps, stage.admitted_span_ms),
          `${stage.label} released ${stage.admitted_bytes} B in ${stage.admitted_span_ms} ms`
          + ` over a ${stage.kbps} kb/s cap`,
        );
        assert.ok(stage.media_bytes > 0, `${stage.label} attributed its bytes to media`);
      }
      assert.deepEqual(telemetry.transport_errors, []);
    } finally {
      await shaper.close();
    }
    await assert.rejects(
      () => fetch(`${base}/api/v1/files/1/direct`, { signal: AbortSignal.timeout(1000) }),
      /fetch failed|aborted/i,
      "closing the shaper releases its listening socket",
    );
  }, { firstResponseDelayMs: 750 });
});

test("the physical-device control API is authenticated and advances exact stages", async () => {
  await withOrigin(1024, async (origin) => {
    const shaper = new lab.ShapingProxy(
      lab.parseNetworkProfile("8mbps-to-1mbps-to-350kbps"),
      origin,
      { controlToken: "test-control-token" },
    );
    const base = await shaper.start();
    const authorized = { "x-playback-lab-control": "test-control-token" };
    try {
      const refused = await fetch(`${base}/__playback_lab/status`);
      assert.equal(refused.status, 403);

      const status = await fetch(`${base}/__playback_lab/status`, { headers: authorized });
      assert.equal(status.status, 200);
      assert.equal((await status.json()).current_kbps, 8000);

      const first = await fetch(`${base}/__playback_lab/cliff`, {
        method: "POST", headers: authorized,
      });
      assert.equal(first.status, 200);
      assert.equal((await first.json()).current_kbps, 1000);
      const second = await fetch(`${base}/__playback_lab/cliff`, {
        method: "POST", headers: authorized,
      });
      assert.equal(second.status, 200);
      assert.equal((await second.json()).current_kbps, 350);
      const exhausted = await fetch(`${base}/__playback_lab/cliff`, {
        method: "POST", headers: authorized,
      });
      assert.equal(exhausted.status, 409);
    } finally {
      await shaper.close();
    }
  });
});

test("client probes are captured without credentials and TTFF drives scheduled cliffs", async () => {
  await withOrigin(1, async (origin) => {
    const shaper = new lab.ShapingProxy(
      lab.parseNetworkProfile("8mbps-to-1mbps-to-350kbps@0.01"),
      origin,
      {
        controlToken: "test-control-token",
        autoAdvance: true,
        recoveryCliffAfterSeconds: 0.01,
      },
    );
    await shaper.start();
    try {
      shaper.captureClientLog({
        event: "playback_probe",
        file_id: 42,
        snapshot: { runway: 3.5 },
        token: "must-not-survive",
        url: "http://example.invalid/?token=must-not-survive",
      });
      shaper.captureClientLog({ event: "ttff", reason: "cold-start", attempt: "one" });
      assert.equal(shaper.controlSnapshot().scheduled_advance.stage_index, 1);
      await new Promise((resolve) => setTimeout(resolve, 30));
      assert.equal(shaper.stageIndex, 1);

      shaper.captureClientLog({ event: "ttff", reason: "stall-buffering", attempt: "two" });
      assert.equal(shaper.controlSnapshot().scheduled_advance.stage_index, 2);
      await new Promise((resolve) => setTimeout(resolve, 30));
      assert.equal(shaper.stageIndex, 2);

      const evidence = JSON.stringify(shaper.controlSnapshot());
      assert.match(evidence, /playback_probe/);
      assert.match(evidence, /"runway":3.5/);
      assert.doesNotMatch(evidence, /must-not-survive/);
      assert.deepEqual(shaper.transitions.map((entry) => entry.reason), [
        "initial-ttff", "recovery-ttff",
      ]);
    } finally {
      await shaper.close();
    }
  });
});

function deviceRunFixture(directory) {
  const canonical = "F7CEB1BB-0000-0000-0000-000000000001";
  const diagnosticScope = "file:///private/containers/DIAG/Diagnostic.app/";
  const productionScope = "file:///private/containers/PROD/plurx.app/";
  const rows = new Map([[10, { processIdentifier: 10, executable: `${productionScope}plurx` }]]);
  const calls = [];
  const state = { rows, calls, canonical, diagnosticScope, productionScope };
  const reply = (fields) => ({ result: { deviceIdentifier: canonical, ...fields } });
  const command = async (args) => {
    calls.push([...args]);
    if (args[1] === "info" && args[2] === "apps") return reply({ apps: [
      { bundleIdentifier: "tv.plurx.diagnostic", url: diagnosticScope },
      { bundleIdentifier: "tv.plurx.app", url: productionScope },
    ] });
    if (args[1] === "info" && args[2] === "processes") return reply({ runningProcesses: [...rows.values()] });
    if (args[2] === "terminate") {
      if (state.terminateError) throw new Error("termination unavailable");
      if (!state.noopTerminate) rows.delete(Number(args[args.indexOf("--pid") + 1]));
      return reply({});
    }
    if (args[2] === "launch") {
      const bundle = args[args.indexOf("--") + 1];
      if (bundle === "tv.plurx.app") return reply({ process: rows.get(10) });
      const process = { processIdentifier: 77, executable: `${diagnosticScope}Diagnostic` };
      rows.set(77, process);
      if (state.launchError) throw new Error("malformed launch JSON after spawn");
      if (state.launchTimeout) return await new Promise(() => {});
      return reply({ process });
    }
    throw new Error(`unexpected fake command ${args.join(" ")}`);
  };
  const options = { device: "apple-tv-alias", target: "http://127.0.0.1:32400", public_host: "127.0.0.1",
    file_id: "42", item_id: "17", bundle_id: "tv.plurx.diagnostic", network_profile: "8mbps-to-1mbps-to-350kbps",
    json: path.join(directory, "device-evidence.json"), observe: "1" };
  const dependencies = {
    leaseDirectory: path.join(directory, "leases"), commandTimeoutMs: 100, cleanupTimeoutMs: 20,
    deviceCommand: command,
    createShaper: async () => ({
      clientEvents: [], start: async () => "http://127.0.0.1:9999",
      controlSnapshot: () => ({ device_calls: calls }),
      close: async () => { state.closed = true; if (state.closeError) throw new Error("proxy close failed"); },
    }),
    preflightProxy: async () => {},
    waitForAcceptance: async () => ({ reason: "test-complete" }),
  };
  return { options, dependencies, state, reply, command };
}

async function failedDeviceRun(fixture) {
  try { await lab.deviceRunCommand(fixture.options, fixture.dependencies); }
  catch (error) { assert.ok(error.evidence, error.stack); return error.evidence; }
  assert.fail("device-run should fail");
}

async function deviceLeases(fixture) {
  return await fsp.readdir(fixture.dependencies.leaseDirectory).catch((error) => {
    if (error.code === "ENOENT") return []; throw error;
  });
}

test("device-run terminates only its owned PID before clean production restore and proxy close", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    const evidence = await lab.deviceRunCommand(fixture.options, fixture.dependencies);
    const { state } = fixture;
    const launches = state.calls.filter((args) => args[2] === "launch");
    assert.equal(state.rows.has(10), true, "another production PID must survive");
    assert.equal(state.rows.has(77), false);
    assert.equal(state.closed, true);
    assert.equal(launches[0].includes("-plurx.acceptance.height"), false, "default quality remains Auto");
    assert.equal(launches[0].includes("--terminate-existing"), false);
    assert.deepEqual(launches[1], ["device", "process", "launch", "--device", state.canonical, "--activate", "--", "tv.plurx.app"]);
    assert.ok(state.calls.findIndex((args) => args[2] === "terminate") < state.calls.indexOf(launches[1]));
    assert.equal(evidence.cleanup.status, "verified-absent");
    assert.equal(evidence.verdict, "passed");
    assert.deepEqual(await deviceLeases(fixture), []);
    assert.equal(JSON.parse(await fsp.readFile(fixture.options.json)).verdict, "passed");
  });
});

test("device-run isolated lab skips absent production app and retains owned cleanup and lease release", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.options.restore_policy = "isolated-lab";
    fixture.state.rows.delete(10);
    fixture.dependencies.deviceCommand = async (args) => {
      const reply = await fixture.command(args);
      if (args[1] === "info" && args[2] === "apps") {
        reply.result.apps = reply.result.apps.filter((app) => app.bundleIdentifier !== "tv.plurx.app");
      }
      return reply;
    };
    fixture.dependencies.restoreDevice = async () => { throw new Error("production restore must not be requested"); };
    fixture.dependencies.waitForAcceptance = async () => {
      const receipt = JSON.parse(await fsp.readFile(fixture.options.json));
      assert.equal(receipt.restore_policy, "isolated-lab", "chosen policy is durable before observation");
      assert.equal(receipt.owned_process.processIdentifier, 77);
      assert.equal((await deviceLeases(fixture)).length, 1, "non-restoring run still owns a durable lease");
      return { reason: "test-complete" };
    };
    const result = await lab.deviceRunCommand(fixture.options, fixture.dependencies);
    const launches = fixture.state.calls.filter((args) => args[2] === "launch");
    assert.equal(launches.length, 1);
    assert.equal(launches[0][launches[0].indexOf("--") + 1], "tv.plurx.diagnostic");
    assert.deepEqual(fixture.state.calls.filter((args) => args[2] === "terminate"), [
      ["device", "process", "terminate", "--device", fixture.state.canonical, "--pid", "77"],
    ]);
    assert.equal(fixture.state.rows.has(77), false);
    assert.equal(fixture.state.closed, true);
    assert.equal(result.cleanup.owned_process_absent, true);
    assert.equal(result.cleanup.status, "verified-absent");
    assert.equal(result.cleanup.production_restored, false);
    assert.equal(result.cleanup.production_restore_skipped, "explicit-isolated-lab-policy");
    assert.equal(result.production_executable_scope, undefined);
    assert.equal(result.verdict, "passed");
    assert.deepEqual(await deviceLeases(fixture), []);
    const durable = JSON.parse(await fsp.readFile(fixture.options.json));
    assert.equal(durable.restore_policy, "isolated-lab");
    assert.equal(durable.cleanup.owned_process_absent, true);
    assert.equal(durable.cleanup.production_restored, false);
  });
});

test("device-run isolated lab requires separate QA bundle and explicit known restore policy", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    for (const options of [
      { ...fixture.options, restore_policy: "unknown" },
      { ...fixture.options, restore_policy: true },
      { ...fixture.options, restore_policy: "isolated-lab", bundle_id: undefined },
      { ...fixture.options, restore_policy: "isolated-lab", bundle_id: "tv.plurx.app" },
    ]) {
      await assert.rejects(lab.deviceRunCommand(options, fixture.dependencies), /restore-policy/);
    }
    assert.equal(fixture.state.calls.length, 0, "invalid policy fails before device operations");
    assert.equal(fixture.state.closed, undefined);
  });
});

test("device-run awaits injected launch restore and shaper operations with explicit manual height", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.options.height = "480";
    fixture.dependencies.launchDevice = async (args) => {
      await new Promise((resolve) => setTimeout(resolve, 5));
      const receipt = JSON.parse(await fsp.readFile(fixture.options.json));
      assert.equal(receipt.launch_attempted, true, "intent must precede launch");
      assert.equal(receipt.bundle_executable_scope, fixture.state.diagnosticScope);
      assert.equal((await deviceLeases(fixture)).length, 1);
      assert.equal(args[args.indexOf("-plurx.acceptance.height") + 1], "480");
      return await fixture.command(["device", "process", "launch", ...args]);
    };
    fixture.dependencies.restoreDevice = async (args) => {
      await new Promise((resolve) => setTimeout(resolve, 5));
      assert.equal(fixture.state.rows.has(77), false);
      assert.equal(fixture.state.closed, undefined, "proxy closes only after device cleanup/restore");
      return await fixture.command(["device", "process", "launch", ...args]);
    };
    await lab.deviceRunCommand(fixture.options, fixture.dependencies);
  });
});

test("device-run refuses a dead proxy API before launching the physical app", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.dependencies.preflightProxy = async () => { throw new Error("device proxy API preflight failed"); };
    const result = await failedDeviceRun(fixture);
    assert.match(result.errors[0].message, /device proxy API preflight/);
    assert.equal(fixture.state.calls.length, 0);
    assert.equal(fixture.state.closed, true);
  });
});

test("device-run refuses leave-running and unwritable evidence before device mutation", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    await assert.rejects(lab.deviceRunCommand({ ...fixture.options, leave_running: true }, fixture.dependencies), /leave-running/);
    await fsp.writeFile(fixture.options.json, "existing evidence");
    const result = await failedDeviceRun(fixture);
    assert.equal(result.launch_attempted, false);
    assert.equal(fixture.state.calls.length, 0);
    assert.equal(await fsp.readFile(fixture.options.json, "utf8"), "existing evidence");
  });
});

test("device-run records observation and cleanup errors and retains unresolved lease", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.dependencies.waitForAcceptance = async () => { throw new Error("observation failed"); };
    fixture.state.terminateError = true;
    const result = await failedDeviceRun(fixture);
    assert.match(result.errors.map((entry) => entry.message).join(" "), /observation failed.*termination unavailable.*remains/);
    assert.equal(result.cleanup.status, "unresolved");
    assert.equal(fixture.state.rows.has(77), true);
    assert.equal(fixture.state.calls.filter((args) => args[2] === "launch").length, 1, "unresolved cleanup must not restore");
    assert.equal((await deviceLeases(fixture)).length, 1);
    assert.equal(fixture.state.closed, true);
  });
});

for (const mode of ["launchError", "launchTimeout"]) {
  test(`device-run ${mode} after spawn retains intent without guessing or killing a PID`, async () => {
    await withTempDir(async (directory) => {
      const fixture = deviceRunFixture(directory);
      fixture.state[mode] = true;
      const result = await failedDeviceRun(fixture);
      assert.equal(result.owned_process, null);
      assert.equal(result.cleanup.status, "unresolved");
      assert.equal(fixture.state.rows.has(77), true);
      assert.equal(fixture.state.calls.some((args) => args[2] === "terminate"), false);
      assert.equal((await deviceLeases(fixture)).length, 1);
      const receipt = JSON.parse(await fsp.readFile(fixture.options.json));
      assert.equal(receipt.launch_attempted, true);
      assert.equal(receipt.verdict, "failed");
    });
  });
}

test("device-run no-op termination fails and retains lease", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.state.noopTerminate = true;
    const result = await failedDeviceRun(fixture);
    assert.equal(result.cleanup.status, "unresolved");
    assert.equal((await deviceLeases(fixture)).length, 1);
  });
});

test("device-run already absent PID skips termination but restores production", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.dependencies.waitForAcceptance = async () => {
      fixture.state.rows.delete(77); return { reason: "app-exited" };
    };
    const result = await lab.deviceRunCommand(fixture.options, fixture.dependencies);
    assert.equal(result.cleanup.production_restored, true);
    assert.equal(fixture.state.calls.some((args) => args[2] === "terminate"), false);
  });
});

test("device-run PID reuse with another executable refuses kill and retains lease", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.dependencies.waitForAcceptance = async () => {
      fixture.state.rows.set(77, { processIdentifier: 77, executable: `${fixture.state.productionScope}plurx` });
      return { reason: "pid-reused" };
    };
    const result = await failedDeviceRun(fixture);
    assert.match(result.errors[0].message, /different executable/);
    assert.equal(fixture.state.calls.some((args) => args[2] === "terminate"), false);
    assert.equal((await deviceLeases(fixture)).length, 1);
  });
});

test("device-run canonical lease excludes aliases and refuses stale ownership without mutation", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    let entered;
    const ready = new Promise((resolve) => { entered = resolve; });
    let finish;
    fixture.dependencies.waitForAcceptance = async () => { entered(); return await new Promise((resolve) => { finish = resolve; }); };
    const first = lab.deviceRunCommand(fixture.options, fixture.dependencies);
    await ready;
    const other = deviceRunFixture(directory);
    other.options.device = "another-alias-for-same-device";
    other.options.json = path.join(directory, "other.json");
    const result = await failedDeviceRun(other);
    assert.match(result.errors[0].message, /lease already exists/);
    assert.equal(other.state.calls.some((args) => args[2] === "launch"), false);
    finish({ reason: "test-complete" });
    await first;
    await fsp.writeFile(path.join(fixture.dependencies.leaseDirectory,
      require("node:crypto").createHash("sha256").update(fixture.state.canonical).digest("hex") + ".json"), "stale or truncated");
    other.options.json = path.join(directory, "stale.json");
    const stale = await failedDeviceRun(other);
    assert.match(stale.errors[0].message, /manual reconciliation/);
    assert.equal((await deviceLeases(fixture)).length, 1);
  });
});

test("device-run never takes ownership of a preexisting target app", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.state.rows.set(99, { processIdentifier: 99, executable: `${fixture.state.diagnosticScope}Diagnostic` });
    const result = await failedDeviceRun(fixture);
    assert.match(result.errors[0].message, /already running/);
    assert.equal(fixture.state.rows.has(99), true);
    assert.equal(fixture.state.calls.some((args) => args[2] === "launch" || args[2] === "terminate"), false);
    assert.deepEqual(await deviceLeases(fixture), []);
  });
});

test("device-run artifact and proxy failures cannot bypass owned process cleanup", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.state.closeError = true;
    fixture.dependencies.writeReceipt = async (filename, evidence) => {
      if (evidence.owned_process) throw new Error("artifact unavailable");
      await lab.durableDeviceReceipt(filename, evidence);
    };
    const result = await failedDeviceRun(fixture);
    assert.equal(fixture.state.rows.has(77), false);
    assert.equal(result.cleanup.owned_process_absent, true);
    assert.match(result.errors.map((entry) => entry.message).join(" "), /artifact unavailable.*proxy close failed.*artifact unavailable/);
    assert.equal((await deviceLeases(fixture)).length, 1, "retain durable audit when final artifact cannot be written");
    const lease = JSON.parse(await fsp.readFile(result.lease_path));
    assert.equal(lease.cleanup.status, "verified-absent");
    assert.equal(lease.verdict, "failed");
  });
});

async function assertDeviceRunChildSignal(signal) {
  await withTempDir(async (directory) => {
    const fixtureSource = `const fs = require('node:fs'); const fsp = fs.promises; const path = require('node:path');
const lab = require(${JSON.stringify(require.resolve("../../scripts/playback-lab"))});
${deviceRunFixture.toString()}
const fixture = deviceRunFixture(${JSON.stringify(directory)});
fixture.dependencies.waitForAcceptance = async () => { process.stdout.write('READY\\n'); return await new Promise(() => {}); };
const command = fixture.command;
fixture.dependencies.deviceCommand = async (args) => {
 if (args[2] === 'terminate') { process.stdout.write('CLEANUP\\n'); await new Promise(r => setTimeout(r, 80)); }
 return await command(args);
};
lab.deviceRunCommand(fixture.options, fixture.dependencies).then(() => {process.exitCode=2;}).catch(async error => {
 await fsp.writeFile(path.join(${JSON.stringify(directory)}, 'child-state.json'), JSON.stringify({rows:[...fixture.state.rows.keys()],closed:fixture.state.closed,evidence:error.evidence}));
 process.exitCode=1;
});`;
    const child = spawn(process.execPath, ["-e", fixtureSource], { stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    let stderr = "";
    let sent = false;
    let repeated = false;
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.stdout.on("data", (chunk) => {
      output += chunk;
      if (!sent && output.includes("READY")) { sent = true; child.kill(signal); }
      if (!repeated && output.includes("CLEANUP")) { repeated = true; child.kill(signal); }
    });
    const status = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => { child.kill("SIGKILL"); reject(new Error(`signal fixture hung: ${output} ${stderr}`)); }, 5000);
      child.on("error", (error) => { clearTimeout(timer); reject(error); });
      child.on("close", (code, exitSignal) => { clearTimeout(timer); resolve({ code, exitSignal }); });
    });
    assert.deepEqual(status, { code: 1, exitSignal: null }, stderr);
    const state = JSON.parse(await fsp.readFile(path.join(directory, "child-state.json")));
    assert.deepEqual(state.rows, [10]);
    assert.equal(state.closed, true);
    assert.equal(state.evidence.cleanup.production_restored, true);
    assert.deepEqual(state.evidence.signals, [signal, signal]);
    assert.equal(JSON.parse(await fsp.readFile(path.join(directory, "device-evidence.json"))).verdict, "failed");
    assert.deepEqual(await fsp.readdir(path.join(directory, "leases")), []);
  });
}

test("device-run real child SIGINT and repeated signals await cleanup",
  async () => await assertDeviceRunChildSignal("SIGINT"));

test("device-run real child SIGTERM and repeated signals await cleanup",
  async () => await assertDeviceRunChildSignal("SIGTERM"));

test("device-run normalizes devicectl four-slash launch URLs and UUID casing", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.dependencies.deviceCommand = async (args) => {
      const reply = await fixture.command(args);
      reply.result.deviceIdentifier = reply.result.deviceIdentifier.toLowerCase();
      if (reply.result.process) reply.result.process = { ...reply.result.process,
        executable: reply.result.process.executable.replace("file:///", "file:////") };
      return reply;
    };
    const result = await lab.deviceRunCommand(fixture.options, fixture.dependencies);
    assert.equal(result.owned_process.executable, `${fixture.state.diagnosticScope}Diagnostic`);
  });
});

test("device-run failed production restore fails run after confirmed diagnostic absence", async () => {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    fixture.dependencies.restoreDevice = async () => undefined;
    const result = await failedDeviceRun(fixture);
    assert.equal(result.cleanup.owned_process_absent, true);
    assert.equal(result.cleanup.production_restored, undefined);
    assert.equal(result.errors[0].phase, "restore");
    assert.equal(fixture.state.rows.has(77), false);
    assert.equal(fixture.state.closed, true);
    assert.deepEqual(await deviceLeases(fixture), []);
  });
});

async function assertDeviceRunFinalizationSignal(phase, signal) {
  await withTempDir(async (directory) => {
    const fixture = deviceRunFixture(directory);
    const originalUnlink = fsp.unlink;
    const originalListeners = process.listenerCount(signal);
    let emitted = false;
    if (phase === "final receipt") {
      fixture.dependencies.writeReceipt = async (filename, evidence) => {
        // Reproduce a signal after the successful verdict was serialized.
        await lab.durableDeviceReceipt(filename, evidence);
        if (!emitted && evidence.verdict === "passed") {
          emitted = true;
          process.emit(signal);
        }
      };
    } else {
      fsp.unlink = async (filename, ...args) => {
        await originalUnlink.call(fsp, filename, ...args);
        if (!emitted && path.dirname(filename) === fixture.dependencies.leaseDirectory
            && filename.endsWith(".json")) {
          emitted = true;
          process.emit(signal);
        }
      };
    }
    try {
      const result = await failedDeviceRun(fixture);
      assert.equal(emitted, true, "must exercise the requested finalization await");
      assert.equal(result.verdict, "failed");
      assert.deepEqual(result.signals, [signal]);
      assert.ok(result.errors.some((entry) => entry.phase === "signal" && entry.message.includes(signal)));
      const artifact = JSON.parse(await fsp.readFile(fixture.options.json));
      assert.equal(artifact.verdict, "failed", "durable receipt must agree with returned failure");
      assert.deepEqual(artifact.signals, [signal]);
      assert.deepEqual(artifact.errors, result.errors);
      assert.equal(artifact.cleanup.owned_process_absent, true);
      assert.equal(fixture.state.rows.has(77), false);
      assert.equal(fixture.state.closed, true);
      assert.deepEqual(await deviceLeases(fixture), [], "verified-absent lease is not recreated");
      assert.equal(process.listenerCount(signal), originalListeners);
    } finally { fsp.unlink = originalUnlink; }
  });
}

test("device-run SIGINT during final receipt fails and durably reconciles the final verdict",
  async () => await assertDeviceRunFinalizationSignal("final receipt", "SIGINT"));

test("device-run SIGTERM during final receipt fails and durably reconciles the final verdict",
  async () => await assertDeviceRunFinalizationSignal("final receipt", "SIGTERM"));

test("device-run SIGINT during lease release fails and durably reconciles the final verdict",
  async () => await assertDeviceRunFinalizationSignal("lease release", "SIGINT"));

test("device-run SIGTERM during lease release fails and durably reconciles the final verdict",
  async () => await assertDeviceRunFinalizationSignal("lease release", "SIGTERM"));

test("device command bounds and reaps a stalled host child without device calls", async () => {
  let child;
  await assert.rejects(lab.physicalAppleDeviceCommand(["device", "info", "processes"], {
    timeoutMs: 30,
    spawnCommand: () => {
      child = spawn(process.execPath, ["-e", "setInterval(()=>{},1000)"], { stdio: ["ignore", "pipe", "pipe"] });
      return child;
    },
  }), /timed out/);
  assert.ok(child.exitCode !== null || child.signalCode !== null);
});

test("concurrent reservations are rescheduled at the cliff instead of bursting", async () => {
  await withOrigin(16 * 1024, async (origin) => {
    const profile = lab.parseNetworkProfile("8mbps-to-1mbps");
    const shaper = new lab.ShapingProxy(profile, origin);
    const base = await shaper.start();
    try {
      const requests = Array.from({ length: 8 }, () =>
        fetch(`${base}/api/v1/files/1/direct`).then((response) => response.arrayBuffer()));
      await new Promise((resolve) => setTimeout(resolve, 5));
      shaper.applyCliff();
      await Promise.all(requests);
      const after = shaper.telemetry().stages[1];
      assert.ok(after.media_bytes > 0, "concurrent media crossed the post-cliff stage");
      // Eight connections is exactly the shape that groups drains, so this
      // asserts what the bucket released rather than when the bytes landed.
      assert.ok(
        after.admitted_bytes <= lab.shaperClaimBoundBytes(after.kbps, after.admitted_span_ms),
        `concurrent requests released ${after.admitted_bytes} B in ${after.admitted_span_ms} ms`
        + ` over a ${after.kbps} kb/s cap`,
      );
    } finally {
      await shaper.close();
    }
  });
});

test("one non-reading response cannot block another shaped connection", async () => {
  const origin = http.createServer((request, response) => {
    response.writeHead(200, { "content-type": "application/octet-stream" });
    response.end(Buffer.alloc(request.url.includes("/files/1/") ? 64 * 1024 * 1024 : 16 * 1024, 0x61));
  });
  await new Promise((resolve) => origin.listen(0, "127.0.0.1", resolve));
  const target = `http://127.0.0.1:${origin.address().port}`;
  const shaper = new lab.ShapingProxy(lab.parseNetworkProfile("400mbps-to-200mbps"), target);
  let blockedDrainResolve;
  let blockedDrainSeen = false;
  const blockedDrain = new Promise((resolve) => { blockedDrainResolve = resolve; });
  const waitForDrain = shaper.waitForDrain.bind(shaper);
  shaper.waitForDrain = (sink, cancelled) => {
    const waiting = waitForDrain(sink, cancelled);
    const timer = setTimeout(() => {
      if (blockedDrainSeen) return;
      blockedDrainSeen = true;
      blockedDrainResolve();
    }, 100);
    return waiting.finally(() => clearTimeout(timer));
  };
  const base = await shaper.start();
  const stuck = net.connect(shaper.port, "127.0.0.1");
  try {
    await new Promise((resolve, reject) => {
      stuck.once("connect", resolve);
      stuck.once("error", reject);
    });
    stuck.write(
      "GET /api/v1/files/1/direct HTTP/1.1\r\n" +
      "Host: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n",
    );
    stuck.pause();
    await Promise.race([
      blockedDrain,
      new Promise((_, reject) => setTimeout(() => reject(new Error("non-reading client never backpressured")), 3000)),
    ]);

    const started = Date.now();
    const healthy = await fetch(`${base}/hls/healthy/seg0.ts`, { signal: AbortSignal.timeout(1000) });
    assert.equal((await healthy.arrayBuffer()).byteLength, 16 * 1024);
    assert.ok(Date.now() - started < 1000, "a healthy connection must not wait for another socket to drain");
    assert.deepEqual(shaper.telemetry().transport_errors, []);
  } finally {
    stuck.destroy();
    await shaper.close();
    origin.closeAllConnections?.();
    await new Promise((resolve) => origin.close(resolve));
  }
});

test("closing interrupts an active throttled response", async () => {
  await withOrigin(1024 * 1024, async (origin) => {
    const shaper = new lab.ShapingProxy(lab.parseNetworkProfile("8kbps-to-4kbps"), origin);
    const base = await shaper.start();
    const request = fetch(`${base}/api/v1/files/1/direct`)
      .then((response) => response.arrayBuffer())
      .catch(() => null);
    await new Promise((resolve) => setTimeout(resolve, 50));
    await Promise.race([
      shaper.close(),
      new Promise((_, reject) => setTimeout(() => reject(new Error("shaper close timed out")), 1000)),
    ]);
    await request;
  });
});

test("browser restarts cancel upstream streams without phantom bytes or link debt", async () => {
  let largeClosedResolve;
  let largeClosedCount = 0;
  const largeClosed = new Promise((resolve) => { largeClosedResolve = resolve; });
  const origin = http.createServer((request, response) => {
    const large = request.url.includes("/files/1/");
    response.writeHead(200, { "content-type": "application/octet-stream" });
    if (!large) {
      response.end(Buffer.alloc(1024, 0x61));
      return;
    }
    response.flushHeaders();
    const timer = setInterval(() => response.write(Buffer.alloc(16 * 1024, 0x61)), 10);
    response.once("close", () => {
      clearInterval(timer);
      largeClosedCount += 1;
      if (largeClosedCount === 6) largeClosedResolve();
    });
  });
  await new Promise((resolve) => origin.listen(0, "127.0.0.1", resolve));
  const target = `http://127.0.0.1:${origin.address().port}`;
  const shaper = new lab.ShapingProxy(lab.parseNetworkProfile("128kbps-to-64kbps"), target);
  const base = await shaper.start();
  try {
    const controllers = Array.from({ length: 6 }, () => new AbortController());
    const requests = controllers.map((controller) =>
      fetch(`${base}/api/v1/files/1/direct`, { signal: controller.signal })
        .then((response) => response.arrayBuffer())
        .catch(() => null));
    await new Promise((resolve) => setTimeout(resolve, 30));
    for (const controller of controllers) controller.abort();
    await Promise.all(requests);
    await Promise.race([
      largeClosed,
      new Promise((_, reject) => setTimeout(() => reject(new Error("canceled upstream stayed open")), 750)),
    ]);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(Object.values(shaper.agent.sockets).flat().length, 0,
      "canceled transfers release every active upstream Agent socket");

    const smallStarted = Date.now();
    const small = await fetch(`${base}/api/v1/files/2/direct`);
    assert.equal((await small.arrayBuffer()).byteLength, 1024);
    assert.ok(Date.now() - smallStarted < 500,
      "the canceled 16 KiB reservation must not delay the replacement request");

    const telemetry = shaper.telemetry();
    assert.deepEqual(telemetry.transport_errors, [], "a deliberate player restart is not a link fault");
    assert.equal(telemetry.stages[0].media_bytes, 1024,
      "only the replacement response, not the canceled slice, is counted as delivered");
  } finally {
    await shaper.close();
    await new Promise((resolve) => origin.close(resolve));
  }
});

/**
 * A real leak both admits and delivers the bytes, so the dilution tests below
 * drive the ledger and the delivery meter together. Driving only `record` would
 * describe drain grouping, which is not a leak and is no longer scored.
 */
function leak(shaper, bytes, media) {
  shaper.meterAdmission(bytes, media);
  shaper.record(bytes, media);
}

test("browser startup dead time cannot dilute a pre-cliff shaper leak", async () => {
  const shaper = new lab.ShapingProxy(
    lab.parseNetworkProfile("8mbps-to-1.5mbps"), "http://127.0.0.1:1",
  );
  let now = 1_000;
  shaper.now = () => now;
  shaper.startedAt = 0;
  shaper.stages[0].entered_at_ms = 0;
  leak(shaper, 500_000, false); // page assets before the first frame
  now = 30_000;
  shaper.beginEvidence();
  // Twelve Mb/s admitted and delivered over the one active second after 30
  // seconds of browser setup. Measuring from proxy bind would dilute this to
  // 387 kb/s.
  leak(shaper, 750_000, true);
  now = 31_000;
  leak(shaper, 750_000, true);
  shaper.applyCliff();
  const telemetry = shaper.telemetry();
  assert.equal(telemetry.stages[0].measured_kbps, 6857.1,
    "the whole-stage estimator includes the first delivered slice's cap-time");
  assert.equal(telemetry.stages[0].peak_kbps, 12_000,
    "the rolling peak still exposes the burst without counting browser startup");
  assert.equal(telemetry.stages[0].admitted_peak_kbps, 12_000,
    "the scored ledger peak sees the same burst");
  assert.equal(lab.scoreRecovery(CRITERIA, observation({ shaping: telemetry })).outcome, "shaping");
  await shaper.close();
});

test("idle time after the last byte cannot dilute a pre-cliff shaper leak", async () => {
  const shaper = new lab.ShapingProxy(
    lab.parseNetworkProfile("8mbps-to-1.5mbps"), "http://127.0.0.1:1",
  );
  let now = 0;
  shaper.now = () => now;
  shaper.startedAt = 0;
  shaper.stages[0].entered_at_ms = 0;
  leak(shaper, 3_000_000, true);
  now = 3_500;
  leak(shaper, 3_000_000, true);
  now = 11_500; // the player's buffer is full until the cliff
  shaper.applyCliff();
  const telemetry = shaper.telemetry();
  assert.equal(telemetry.stages[0].held_seconds, 3.5);
  assert.equal(telemetry.stages[0].measured_kbps, 7384.6);
  assert.equal(telemetry.stages[0].peak_kbps, 24_000,
    "the last-byte boundary cannot dilute the delivered burst");
  assert.equal(telemetry.stages[0].admitted_peak_kbps, 24_000,
    "nor the admitted one");
  assert.equal(lab.scoreRecovery(CRITERIA, observation({ shaping: telemetry })).outcome, "shaping");
  await shaper.close();
});

test("slower delivery after a mid-stage burst cannot dilute a shaper leak", async () => {
  const shaper = new lab.ShapingProxy(
    lab.parseNetworkProfile("8mbps-to-1.5mbps"), "http://127.0.0.1:1",
  );
  let now = 0;
  shaper.now = () => now;
  shaper.startedAt = 0;
  shaper.stages[0].entered_at_ms = 0;
  // A 13.6 Mb/s one-second burst followed by slower delivery averages to only
  // 4.32 Mb/s over the whole active stage. The burst must remain observable.
  leak(shaper, 850_000, true);
  now = 999;
  leak(shaper, 850_000, true);
  now = 5_000;
  leak(shaper, 1_000_000, true);
  shaper.applyCliff();
  const telemetry = shaper.telemetry();
  assert.equal(telemetry.stages[0].measured_kbps, 3692.3);
  assert.equal(telemetry.stages[0].peak_kbps, 13_600);
  assert.equal(telemetry.stages[0].admitted_peak_kbps, 13_600);
  assert.equal(lab.scoreRecovery(CRITERIA, observation({ shaping: telemetry })).outcome, "shaping");
  await shaper.close();
});

/**
 * A downstream sink that holds its slice until the test releases it, which is
 * what a browser socket does whenever the player stops reading. Reservations
 * stay serialized through the shared bucket; only the completion is deferred.
 */
function heldSink() {
  const sink = new EventEmitter();
  sink.destroyed = false;
  sink.writable = true;
  sink.written = 0;
  sink.holding = true;
  sink.write = (slice) => {
    sink.written += slice.length;
    return !sink.holding;
  };
  sink.release = () => {
    sink.holding = false;
    sink.emit("drain");
  };
  return sink;
}

/** Run the microtask/immediate queues until the shaper stops making progress. */
async function settle(rounds = 50) {
  for (let index = 0; index < rounds; index += 1) {
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/** A shaper whose reservations are paid for by advancing the clock, not a timer. */
function deterministicShaper(spec) {
  const shaper = new lab.ShapingProxy(lab.parseNetworkProfile(spec), "http://127.0.0.1:1");
  const clock = { now: 0 };
  shaper.now = () => clock.now;
  shaper.startedAt = 0;
  shaper.stages[0].entered_at_ms = 0;
  // The bucket, not a real timer, decides when a reservation is granted. Paying
  // the debt by advancing the clock keeps the whole scenario deterministic.
  // Round up, because a real timer never fires early either.
  shaper.waitForLimiter = (delay) => {
    clock.now += Math.ceil(delay);
    return new Promise((resolve) => setImmediate(resolve));
  };
  return [shaper, clock];
}

/*
 * The scored whole-stage quantity is the ledger's, not the delivered stream's,
 * and this is why. Three connections each take one legally priced slice and
 * then stop reading; the stage delivers nothing else. Every byte arrives in the
 * same instant the players resume, so the delivered stage rate is 4500 kb/s on
 * a 1500 kb/s link — a rate no bound can be stated for, since the skew is one
 * undrained slice per connection and the proxy opens up to `max_sockets` of
 * them. Nothing pads this stage: there is no later slice diluting the average
 * and no burst to isolate. If the run is scored on what the bucket released, it
 * passes; if it is scored on delivery timing, it invents a shaping fault.
 */
test("a stage delivered entirely in one grouped drain is not a shaper leak", async () => {
  const [shaper, clock] = deterministicShaper("8mbps-to-1.5mbps");
  const slice = Buffer.alloc(16 * 1024, 0x61);
  const open = () => false;
  shaper.applyCliff();

  const held = [heldSink(), heldSink(), heldSink()];
  const pending = held.map((sink) => shaper.writeShaped(slice, sink, true, open));
  await settle();
  for (const sink of held) {
    assert.equal(sink.written, slice.length, "each held connection was granted exactly one slice");
  }
  assert.ok(clock.now > 250, `three 16 KiB slices cannot be granted in ${clock.now}ms at 1500 kb/s`);

  clock.now = 3_000;
  for (const sink of held) sink.release();
  await Promise.all(pending);

  const after = shaper.telemetry().stages[1];
  assert.equal(after.measured_kbps, 4500, "the delivered stage rate is the one the reviewer reproduced");
  assert.ok(after.measured_kbps > 1500 * 1.25, "and it sits past the flat delivered gate this replaced");
  assert.equal(after.admitted_bytes, 3 * slice.length, "the bucket released exactly three slices");
  assert.ok(
    after.admitted_bytes <= lab.shaperClaimBoundBytes(1500, after.admitted_span_ms),
    `and no more than the ${after.admitted_span_ms} ms window allows`,
  );

  const observed = observation();
  observed.shaping.stages[1] = after;
  const score = lab.scoreRecovery(CRITERIA, observed);
  assert.deepEqual(score.errors, [],
    "a stage whose every byte drained at once is not evidence that the shaper leaked");
  assert.equal(score.outcome, "passed");
  await shaper.close();
});

/*
 * There is one bucket, so a refund credits whichever stage is live when it
 * happens. Filing the withdrawal against the stage that made the claim would
 * hand the post-cliff bucket spendable credit its own ledger never saw, and the
 * claims that credit funds would then read as bytes the bucket could not have
 * released.
 */
test("a refund after the cliff is withdrawn from the stage whose bucket took it", async () => {
  const [shaper, clock] = deterministicShaper("8mbps-to-1.5mbps");
  const slice = Buffer.alloc(16 * 1024, 0x61);
  const open = () => false;

  // One pre-cliff slice is granted and then held by a player that stops reading.
  const held = heldSink();
  const abandoned = shaper.writeShaped(slice, held, true, open).catch(() => null);
  await settle();
  assert.equal(held.written, slice.length);
  assert.equal(shaper.admissionSamples[0].length, 1, "the claim is filed against the stage that made it");

  // The link idles until the 8 Mb/s bucket is full, then the cliff clamps it.
  clock.now = 5_000;
  shaper.applyCliff();

  // The post-cliff stage then claims continuously, so its rolling peak sits on
  // the bucket's ceiling and one unaccounted slice is enough to break it.
  const free = heldSink();
  free.holding = false;
  for (let index = 0; index < 4; index += 1) await shaper.writeShaped(slice, free, true, open);

  // The abandoned connection closes here. The bucket is in debt, so it takes
  // the whole slice back and immediately funds another claim with it.
  held.destroyed = true;
  held.emit("close");
  await abandoned;
  for (let index = 0; index < 16; index += 1) await shaper.writeShaped(slice, free, true, open);

  const refunds = shaper.admissionSamples[1].filter((entry) => entry.bytes < 0);
  assert.deepEqual(shaper.admissionSamples[0].filter((entry) => entry.bytes < 0), [],
    "the pre-cliff ledger keeps a claim its bucket really made");
  assert.deepEqual(refunds.map((entry) => entry.bytes), [-slice.length],
    "and the post-cliff ledger records the credit its own bucket received");

  const after = shaper.telemetry().stages[1];
  const bound = Number(lab.shaperBurstBoundKbps(1500).toFixed(1));
  // 1966.1 kb/s against a 2006.1 kb/s ceiling. Filed against the claiming stage
  // instead, the same run reads 2097.2 kb/s — one uncancelled slice over the
  // ceiling, and a healthy bucket reported as a leak.
  assert.equal(after.admitted_peak_kbps, 1966.1);
  assert.ok(after.admitted_peak_kbps <= bound,
    `a refunded slice cannot inflate the peak, got ${after.admitted_peak_kbps} against ${bound}`);
  assert.ok(after.admitted_peak_kbps + (lab.SHAPER_SLICE_BYTES * 8) / 1000 > bound,
    "one more unaccounted slice must break the ceiling, or the credit was never binding");

  // End to end the abandoned slice balances: it is claimed pre-cliff, refunded
  // post-cliff, and never delivered, so the shaper's own totals agree without
  // any allowance at all.
  const whole = shaper.telemetry();
  assert.equal(whole.stages.reduce((total, stage) => total + stage.bytes, 0), 327_680);
  assert.equal(whole.stages.reduce((total, stage) => total + stage.admitted_bytes, 0), 327_680);

  const observed = observation();
  observed.shaping.stages[1] = after;
  // `after` records a refund of a claim the pre-cliff bucket really made, so
  // the synthetic pre-cliff stage has to carry that claim for the two stages to
  // describe one run. Without it the pair is missing 16 KiB of admission that
  // the real telemetry above has.
  observed.shaping.stages[0].admitted_bytes += slice.length;
  const score = lab.scoreRecovery(CRITERIA, observed);
  assert.deepEqual(score.errors, [], "a canceled transfer across the cliff is not a shaper leak");
  await shaper.close();
});

/*
 * A slice the downstream has already taken is spent. The reservation used to
 * stay refundable until after the post-drain cancellation check, so a `close`
 * landing between `drain` and that continuation resuming handed the bucket back
 * credit it had spent on bytes the socket accepted, and dropped the delivery
 * from the artifact at the same time. Reproduced at 32 sinks: 524,288 B went
 * downstream against a 250,759 B one-second bound and still scored `passed`.
 */
test("a drain-then-close race can neither refund a taken slice nor erase it", async () => {
  const [shaper] = deterministicShaper("8mbps-to-1.5mbps");
  const slice = Buffer.alloc(16 * 1024, 0x61);
  let closing = false;
  const open = () => closing;

  const sinks = [];
  const writes = [];
  for (let index = 0; index < 32; index += 1) {
    const sink = heldSink();
    sinks.push(sink);
    writes.push(shaper.writeShaped(slice, sink, true, open).catch(() => null));
  }
  await settle();
  const accepted = sinks.reduce((total, sink) => total + sink.written, 0);
  assert.equal(accepted, 32 * slice.length, "every sink took its slice");

  // Each sink drains — the bytes are downstream's — and only then closes,
  // before the continuation that follows the drain can resume.
  for (const sink of sinks) sink.release();
  closing = true;
  for (const sink of sinks) {
    sink.destroyed = true;
    sink.emit("close");
  }
  await Promise.all(writes);

  const telemetry = shaper.telemetry();
  const delivered = telemetry.stages.reduce((total, stage) => total + stage.bytes, 0);
  const admitted = telemetry.stages.reduce((total, stage) => total + stage.admitted_bytes, 0);
  assert.equal(delivered, accepted, "a taken slice cannot vanish from the artifact");
  assert.equal(admitted, accepted, "and cannot be refunded back into the bucket");
  assert.deepEqual(
    telemetry.stages.flatMap((stage, index) => shaper.admissionSamples[index])
      .filter((entry) => entry.bytes < 0),
    [],
    "no refund is filed for a slice the downstream already took",
  );

  // The old behavior scored this clean while the link carried every byte.
  const observed = observation();
  observed.shaping.stages[1] = telemetry.stages[0];
  assert.equal(delivered > admitted, false, "delivery never exceeds admission after the race");
  await shaper.close();
});

/*
 * `beginEvidence` discards the ledger mid-stage, so a slice the bucket priced
 * before the first presented frame can be delivered after it with its claim in
 * the discarded series. The allowance for that seam used to be a blanket
 * `max_sockets x slice_bytes` — 512 KiB usable by delivery with no claim behind
 * it anywhere. It is now the measured bytes of the claims that actually crossed.
 */
test("the evidence seam exempts the claims that crossed it and nothing else", async () => {
  const [shaper] = deterministicShaper("8mbps-to-1.5mbps");
  const slice = Buffer.alloc(16 * 1024, 0x61);
  const open = () => false;

  // One slice is priced, then held by a sink that has not drained yet.
  const held = heldSink();
  const crossing = shaper.writeShaped(slice, held, true, open);
  await settle();
  assert.equal(shaper.pendingClaims.size, 1, "the claim is outstanding at the seam");

  // The first frame arrives and the ledger holding that claim is discarded.
  shaper.beginEvidence();
  assert.equal(shaper.telemetry().evidence_pending_claim_bytes, slice.length,
    "the artifact records what was actually in flight, not a socket-count ceiling");
  assert.equal(shaper.telemetry().carried_over_bytes, 0, "nothing has crossed yet");

  // It drains after the reset, so this completion is the genuine carry-over.
  held.release();
  await crossing;
  assert.equal(shaper.telemetry().carried_over_bytes, slice.length);

  const carried = shaper.telemetry();
  const delivered = carried.stages.reduce((total, stage) => total + stage.bytes, 0);
  const admitted = carried.stages.reduce((total, stage) => total + stage.admitted_bytes, 0);
  assert.equal(delivered - admitted, slice.length,
    "the delivery outruns the surviving ledger by exactly the slice that crossed");

  // Scored with its provenance the run is clean; one unclaimed byte past it is
  // not, where the blanket allowance would have excused 512 KiB of it.
  const observed = observation();
  observed.shaping.evidence_pending_claim_bytes = slice.length;
  observed.shaping.carried_over_bytes = slice.length;
  observed.shaping.stages[1].bytes += slice.length;
  assert.equal(lab.scoreRecovery(CRITERIA, observed).outcome, "passed");
  observed.shaping.stages[1].bytes += 1;
  assert.equal(lab.scoreRecovery(CRITERIA, observed).outcome, "shaping");
  await shaper.close();
});

test("a claim canceled before it is delivered never widens the seam allowance", async () => {
  const [shaper] = deterministicShaper("8mbps-to-1.5mbps");
  const slice = Buffer.alloc(16 * 1024, 0x61);
  let closing = false;

  const held = heldSink();
  const abandoned = shaper.writeShaped(slice, held, true, () => closing).catch(() => null);
  await settle();
  shaper.beginEvidence();
  assert.equal(shaper.telemetry().evidence_pending_claim_bytes, slice.length);

  // The player abandons the rung before the sink ever drains, so the slice
  // never reached the wire. It is refunded, and it is not a carry-over.
  closing = true;
  held.destroyed = true;
  held.emit("close");
  await abandoned;
  assert.equal(shaper.telemetry().carried_over_bytes, 0,
    "a refunded claim cannot be spent as delivery allowance");
  assert.equal(shaper.pendingClaims.size, 0, "and it is no longer outstanding");
  await shaper.close();
});

test("a pre-evidence claim refunded after the reset keeps the delivery proof balanced", async () => {
  const [shaper] = deterministicShaper("8mbps-to-1.5mbps");
  const slice = Buffer.alloc(16 * 1024, 0x61);
  let closing = false;

  // Price one slice before the first frame, then hold it until the evidence
  // reset has discarded the positive admission that belongs to the claim.
  const held = heldSink();
  const abandoned = shaper.writeShaped(slice, held, true, () => closing).catch(() => null);
  await settle();
  assert.equal(shaper.pendingClaims.size, 1, "the pre-window claim is still outstanding");
  shaper.beginEvidence();

  // The player changes rung before the sink drains. The retained ledger sees
  // the refund but not the discarded claim, which is the exact seam the final
  // delivery proof must reconcile.
  closing = true;
  held.destroyed = true;
  held.emit("close");
  await abandoned;

  // Ordinary delivery on both sides of the cliff makes this a complete scored
  // run rather than another bookkeeping-only assertion.
  const free = heldSink();
  free.holding = false;
  const open = () => false;
  await shaper.writeShaped(slice, free, true, open);
  shaper.applyCliff();
  await shaper.writeShaped(slice, free, true, open);

  const telemetry = shaper.telemetry();
  assert.equal(telemetry.evidence_pending_claim_bytes, slice.length);
  assert.equal(telemetry.carried_over_bytes, 0, "the canceled claim delivered no bytes");
  assert.equal(telemetry.evidence_refunded_claim_bytes, slice.length,
    "the artifact records the exact cross-seam credit the bucket accepted");
  assert.equal(
    telemetry.stages.reduce((total, stage) => total + stage.bytes, 0),
    2 * slice.length,
    "only the two ordinary slices reached the browser",
  );

  const observed = observation({ shaping: telemetry });
  assert.equal(lab.scoreRecovery(CRITERIA, observed).outcome, "passed",
    "a real cross-seam refund cannot turn healthy shaping into a false failure");

  // Reconciliation is exact provenance, not a new blanket allowance.
  observed.shaping.stages[1].bytes += 1;
  assert.equal(lab.scoreRecovery(CRITERIA, observed).outcome, "shaping",
    "one genuinely unclaimed byte still invalidates the run");
  await shaper.close();
});

test("a canceled post-evidence claim cannot authorize later unclaimed delivery", async () => {
  const [shaper, clock] = deterministicShaper("8mbps-to-1.5mbps");
  const slice = Buffer.alloc(16 * 1024, 0x61);
  let closing = false;
  shaper.beginEvidence();

  // Claim a slice inside the evidence window and hold it before drain. By the
  // time the player cancels, the idle bucket is full and accepts no refund, so
  // its conservation ledger legitimately retains the whole positive claim.
  const held = heldSink();
  const abandoned = shaper.writeShaped(slice, held, true, () => closing).catch(() => null);
  await settle();
  assert.equal(shaper.pendingClaims.size, 1, "the post-window claim is still outstanding");
  clock.now = 10_000;
  closing = true;
  held.destroyed = true;
  held.emit("close");
  await abandoned;
  assert.equal(shaper.bucket.refund(slice.length, clock.now), 0,
    "the full bucket has no room for any more cancellation credit");

  // One ordinary post-cliff slice still has a matching admission. The canceled
  // claim delivered nothing, so it must authorize no browser bytes even though
  // its zero-credit refund leaves that admission in the conservation ledger.
  shaper.applyCliff();
  const free = heldSink();
  free.holding = false;
  await shaper.writeShaped(slice, free, true, () => false);
  const clean = shaper.telemetry();
  assert.equal(clean.undelivered_admitted_bytes, slice.length,
    "the artifact names the stranded admission from the canceled claim");
  assert.equal(lab.scoreRecovery(CRITERIA, observation({ shaping: clean })).outcome, "passed",
    "a legitimate zero-credit cancellation remains clean");

  // Reproduce the reviewer's counterexample through the real proxy: an equal
  // unreserved delivery cannot spend the canceled claim's stranded admission.
  shaper.record(slice.length, true);
  const bypassed = shaper.telemetry();
  assert.equal(
    bypassed.stages.reduce((total, stage) => total + stage.bytes, 0),
    bypassed.stages.reduce((total, stage) => total + stage.admitted_bytes, 0),
    "the old aggregate comparison saw equal totals and returned a false pass",
  );
  const score = lab.scoreRecovery(CRITERIA, observation({ shaping: bypassed }));
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /canceled claims left 16384 B of admission without delivery/);
  await shaper.close();
});

test("a refund is metered as the credit the bucket took, not the slice that was asked back", () => {
  const bucket = new lab.TokenBucket(1500, 0);
  bucket.refill(10_000);
  assert.equal(bucket.tokens, bucket.capacity, "the idle bucket is full");
  assert.equal(bucket.refund(16 * 1024, 10_000), 0, "a full bucket cannot take a slice back");
  assert.equal(bucket.tokens, bucket.capacity, "and does not overflow trying");
  bucket.reserve(16 * 1024, 10_000);
  assert.equal(bucket.refund(16 * 1024, 10_000), 16 * 1024, "a bucket with room takes the whole slice");
});

test("slices held by separate sinks and released together are not a shaper leak", async () => {
  const shaper = new lab.ShapingProxy(
    lab.parseNetworkProfile("8mbps-to-1.5mbps"), "http://127.0.0.1:1",
  );
  let now = 0;
  shaper.now = () => now;
  shaper.startedAt = 0;
  shaper.stages[0].entered_at_ms = 0;
  // The bucket, not a real timer, decides when a reservation is granted. Paying
  // the debt by advancing the clock keeps the whole scenario deterministic.
  // Round up, because a real timer never fires early either.
  shaper.waitForLimiter = (delay) => {
    now += Math.ceil(delay);
    return new Promise((resolve) => setImmediate(resolve));
  };
  const slice = Buffer.alloc(16 * 1024, 0x61);
  const open = () => false;
  shaper.applyCliff();

  // Three connections each take one legally priced slice out of the 1.5 Mb/s
  // bucket and then stop reading. Every reservation waited its full turn.
  const held = [heldSink(), heldSink(), heldSink()];
  const pending = held.map((sink) => shaper.writeShaped(slice, sink, true, open));
  await settle();
  for (const sink of held) {
    assert.equal(sink.written, slice.length, "each held connection was granted exactly one slice");
  }
  const grantedBy = now;
  assert.ok(grantedBy > 250, `three 16 KiB slices cannot be granted in ${grantedBy}ms at 1500 kb/s`);

  // The link then idles long enough to refill the bucket, and all three players
  // resume reading at the same instant.
  now = 3_000;
  for (const sink of held) sink.release();
  await Promise.all(pending);

  // Fourteen further slices are admitted normally, each one waiting for the
  // bucket exactly as designed.
  const free = heldSink();
  free.holding = false;
  for (let index = 0; index < 14; index += 1) {
    await shaper.writeShaped(slice, free, true, open);
  }
  // One late slice, so the whole-stage average stays far below its own gate and
  // cannot be what this test is measuring.
  now = 7_000;
  await shaper.writeShaped(slice, free, true, open);

  const after = shaper.telemetry().stages[1];
  const bound = lab.shaperBurstBoundKbps(1500);
  // 17 slices land inside one window although only 14 were ever priced into it.
  assert.equal(after.peak_kbps, 2228.2,
    "the delivered rolling peak reaches the shape that used to score as a leak");
  assert.ok(after.peak_kbps > bound,
    `the delivered peak must exceed the ceiling, got ${after.peak_kbps} against ${bound}`);
  assert.equal(after.measured_kbps, 577.2);
  assert.ok(after.measured_kbps < 1500 * 1.25,
    "the whole-stage average must stay nonbinding, or it is what this test measures");

  const observed = observation();
  observed.shaping.stages[1] = after;
  const score = lab.scoreRecovery(CRITERIA, observed);
  assert.deepEqual(score.errors, [],
    "deferred drains on separate connections are not evidence that the shaper leaked");
  assert.equal(score.outcome, "passed");
  assert.ok(after.admitted_peak_kbps <= bound,
    `no window admitted past the ceiling, got ${after.admitted_peak_kbps} against ${bound}`);
  await shaper.close();
});

test("whole-stage rate includes the first slice's earning time", async () => {
  const shaper = new lab.ShapingProxy(
    lab.parseNetworkProfile("1mbps-to-0.5mbps"), "http://127.0.0.1:1",
  );
  let now = 0;
  shaper.now = () => now;
  shaper.startedAt = 0;
  shaper.stages[0].entered_at_ms = 0;
  const sliceBytes = 16 * 1024;
  const sliceMs = sliceBytes * 8 / 1000;
  for (let index = 0; index < 8; index += 1) {
    shaper.record(sliceBytes, true);
    now += sliceMs;
  }
  shaper.applyCliff();
  const before = shaper.telemetry().stages[0];
  assert.equal(before.measured_kbps, 1000,
    "eight slices at the cap must not report the old 8/7 first-slice bias");
  assert.equal(before.measured_media_kbps, 1000);
  await shaper.close();
});

test("one immutable shaping snapshot is reused after the evidence window closes", async () => {
  const shaper = new lab.ShapingProxy(
    lab.parseNetworkProfile("8mbps-to-1.5mbps"), "http://127.0.0.1:1",
  );
  let now = 1_000;
  shaper.now = () => now;
  shaper.startedAt = 0;
  shaper.stages[0].entered_at_ms = 0;
  shaper.record(1_000, true);
  const frozen = shaper.freezeTelemetry();
  now = 60_000;
  shaper.record(5_000, true);
  assert.strictEqual(shaper.telemetry(), frozen);
  assert.equal(shaper.telemetry().stages[0].bytes, 1_000,
    "later cleanup activity cannot rewrite the retained throughput account");
  await shaper.close();
});

test("a shaper that cannot bind says so, and leaves no listening socket behind", async () => {
  const profile = lab.parseNetworkProfile("8mbps-to-1.5mbps");
  const shaper = new lab.ShapingProxy(profile, "http://127.0.0.1:1");
  await assert.rejects(
    () => shaper.start(() => Promise.reject(new Error("EACCES"))),
    /network shaping is unavailable/,
  );
  await shaper.close();
  const silent = new lab.ShapingProxy(profile, "http://127.0.0.1:1");
  await assert.rejects(() => silent.start(() => Promise.resolve(0)), /bound no port/);
  await silent.close();
});

// ------------------------------------------------------------- the scoring

function sample(atMs, overrides = {}) {
  return {
    at_ms: atMs,
    absolute_time: atMs / 1000,
    height: 360,
    ttff_ms: 500,
    stalls: 0,
    attempt_id: "a1",
    attempt_reason: "cold-start",
    player_generation: 1,
    play_started_at: null,
    paused: false,
    seeking: false,
    ready_state: 4,
    media_error: null,
    ...overrides,
  };
}

function healthyTimeline(overrides = {}) {
  return Array.from({ length: 46 }, (_, index) => sample(index * 1000, overrides));
}

function autoSwitch(seq, atMs, fromHeight, toHeight, overrides = {}) {
  return {
    seq,
    at_ms: 20_000 + atMs,
    from: `${fromHeight}p`,
    to: `${toHeight}p`,
    from_height: fromHeight,
    to_height: toHeight,
    reason: "test",
    position: atMs / 1000,
    target_method: "transcode",
    target_session_id: "session-1",
    target_attempt_id: "a1",
    ...overrides,
  };
}

function autoTransition(events, overrides = {}) {
  const finalAtMs = 95_000;
  return {
    sampled_at_ms: finalAtMs,
    media_event_seq: 0,
    media_events: [],
    auto_switch_seq: events.length ? events.at(-1).seq : 0,
    auto_switches: events,
    frame_probe: {
      supported: true, sequence: events.length, frames: events.length,
      last_frame_at_ms: finalAtMs, maximum_gap_ms: 50,
      last_frame: events.length ? { sequence: events.length, at_ms: finalAtMs } : null,
      auto_presentations: events.map((event) => ({
        switch_seq: event.seq,
        frame: {
          sequence: event.seq, at_ms: event.at_ms + 50,
          absolute_time: event.position, height: event.to_height,
          method: event.target_method, session_id: event.target_session_id,
          attempt_id: event.target_attempt_id,
        },
      })),
    },
    ...overrides,
  };
}

/**
 * A healthy shaped run. Both stages carry a ledger consistent with what they
 * delivered, because that is what the scored gates read: `admitted_bytes` over
 * `admitted_span_ms` for the sustained rate, the rolling admitted peak for the
 * burst, and delivered bytes only as a total against the ledger's own total.
 */
function shapedStage(label, kbps, { spanMs, rate, mediaRate }) {
  const bytes = Math.round((rate * spanMs) / 8);
  return {
    label,
    kbps,
    bytes,
    media_bytes: Math.round((mediaRate * spanMs) / 8),
    measured_kbps: rate,
    measured_media_kbps: mediaRate,
    peak_kbps: rate,
    admitted_bytes: bytes,
    admitted_span_ms: spanMs,
    admitted_kbps: rate,
    admitted_peak_kbps: rate,
  };
}

function observation(extra = {}) {
  return {
    shaping: {
      cliff_applied_at_ms: 12_000,
      slice_bytes: 16 * 1024,
      max_sockets: 32,
      evidence_pending_claim_bytes: 0,
      carried_over_bytes: 0,
      evidence_refunded_claim_bytes: 0,
      undelivered_admitted_bytes: 0,
      stages: [
        shapedStage("before-cliff", 8000, { spanMs: 12_000, rate: 4100, mediaRate: 4000 }),
        shapedStage("after-cliff", 1500, { spanMs: 45_000, rate: 1480, mediaRate: 1400 }),
      ],
    },
    baseline_clock_rate: 1.0,
    baseline_error: null,
    pre_cliff_runway_seconds: 10,
    timeline: healthyTimeline(),
    ...extra,
  };
}

// No shaping tolerance appears here, and none is accepted: both shaping gates
// are the bucket's own conservation identity, so a multiplier would only be an
// unproved band in which a real leak scores as healthy.
const CRITERIA = {
  baseline_minimum_clock_rate: 0.9,
  maximum_automatic_restarts: 1,
  maximum_upgrades_per_60s: 1,
  recovery_deadline_seconds: 30,
  sustained_seconds: 10,
  sustained_minimum_clock_rate: 0.9,
};

test("a clean recovery passes", () => {
  const score = lab.scoreRecovery(CRITERIA, observation());
  assert.deepEqual(score.errors, []);
  assert.equal(score.outcome, "passed");
});

test("each cliff is scored against its own rate and player window", () => {
  const first = observation();
  const second = observation({
    shaping: {
      ...first.shaping,
      stages: [
        ...first.shaping.stages,
        { ...shapedStage("after-cliff-2", 350, {
          spanMs: 45_000, rate: 340, mediaRate: 320,
        }), entered_at_ms: 87_000 },
      ],
    },
    timeline: healthyTimeline().map((row, index) => ({
      ...row, absolute_time: Math.min(index, 5),
    })),
  });
  assert.equal(lab.scoreRecovery(CRITERIA, first, 1).outcome, "passed");
  const secondScore = lab.scoreRecovery(CRITERIA, second, 2);
  assert.notEqual(secondScore.outcome, "passed");
  assert.ok(secondScore.errors.some((error) => /never held/.test(error)));
  assert.equal(secondScore.metrics.shaping.stages[2].kbps, 350);
  const unapplied = lab.scoreRecovery(CRITERIA, {
    ...second,
    shaping: { ...second.shaping, stages: second.shaping.stages.map((stage, index) =>
      index === 2 ? { ...stage, entered_at_ms: null } : stage) },
  }, 2);
  assert.equal(unapplied.outcome, "shaping");
  assert.match(unapplied.errors[0], /never applied cliff 2/);
});

test("second cliff baseline uses the stable tail after early first-window impairment", () => {
  const firstWindow = Array.from({ length: 76 }, (_, second) => ({
    at_ms: second * 1000,
    absolute_time: Math.max(0, second - 20),
  }));
  const wholeWindowRate = firstWindow.at(-1).absolute_time / 75;
  assert.ok(wholeWindowRate < CRITERIA.baseline_minimum_clock_rate);
  const tailRate = lab.tailClockRate(firstWindow, 1000);
  assert.equal(tailRate, 1);
  const first = observation();
  const second = observation({
    shaping: {
      ...first.shaping,
      stages: [
        ...first.shaping.stages,
        { ...shapedStage("after-cliff-2", 350, {
          spanMs: 45_000, rate: 340, mediaRate: 320,
        }), entered_at_ms: 87_000 },
      ],
    },
    baseline_clock_rate: tailRate,
  });
  assert.equal(lab.scoreRecovery(CRITERIA, {
    ...second, baseline_clock_rate: wholeWindowRate,
  }, 2).outcome, "browser_playback");
  assert.equal(lab.scoreRecovery(CRITERIA, second, 2).outcome, "passed");
  assert.equal(lab.tailClockRate(firstWindow.filter((row) => row.at_ms !== 70_000), 1000), 1);
  assert.equal(lab.tailClockRate(firstWindow.filter((row) => row.at_ms < 65_000 || row.at_ms > 71_000), 1000), 0);
});

test("browser observer applies both cliffs after separate complete windows", async () => {
  const profile = lab.parseNetworkProfile("8mbps-to-1.1mbps-to-350kbps@0.01");
  const applied = [];
  const shaper = {
    profile,
    stageIndex: 0,
    beginEvidence() {},
    applyCliff() {
      this.stageIndex += 1;
      applied.push(this.stageIndex);
      return this.stageIndex * 10_000;
    },
    telemetrySnapshot() {
      return {
        ...observation().shaping,
        stages: [
          shapedStage("before-cliff", 8000, { spanMs: 12_000, rate: 4100, mediaRate: 4000 }),
          shapedStage("after-cliff", 1100, { spanMs: 45_000, rate: 1000, mediaRate: 980 }),
          { ...shapedStage("after-cliff-2", 350, { spanMs: 45_000, rate: 340, mediaRate: 320 }),
            entered_at_ms: this.stageIndex >= 2 ? 20_000 : null },
        ],
      };
    },
    freezeTelemetry() { return this.telemetrySnapshot(); },
  };
  let absoluteTime = 0;
  const driver = {
    exec: async () => true,
    eval: async () => ({
      started: true,
      video: { absolute_time: ++absoluteTime, current_time: absoluteTime,
        runway: 0, ready_state: 4, paused: false, seeking: false },
      frame_probe: { supported: false },
      attempt_id: "a1", player_generation: 1,
      auto_switches: [], media_events: [],
    }),
  };
  const observed = await lab.observeShapedCliff(driver, shaper, {
    recovery: { sample_interval_seconds: 0.01, recovery_observe_seconds: 0.02,
      sustained_seconds: 0.01, recovery_deadline_seconds: 0.02 },
  }, { video: { absolute_time: 0 } });
  assert.deepEqual(applied, [1, 2]);
  assert.deepEqual(observed.windows.map((window) => window.cliff_index), [1, 2]);
  assert.deepEqual(observed.windows.map((window) => window.cliff_applied_at_ms), [10_000, 20_000]);
  assert.ok(observed.windows.every((window) => window.observation.timeline[0].at_ms === 0));
  assert.equal(observed.score.metrics.cliffs.length, 2);
});

test("Auto acceptance requires one seamless downshift inside ten seconds", () => {
  const criteria = {
    ...CRITERIA,
    maximum_automatic_restarts: 0,
    minimum_downshifts: 1,
    maximum_downshifts: 1,
    maximum_downshift_ms: 10_000,
    minimum_post_switch_runway_seconds: 1,
    maximum_single_transition_video_gap_ms: 100,
    maximum_video_gap_ms: 250,
    maximum_landing_error_seconds: 0.25,
    maximum_wait_events: 0,
    stable_after_downshift_seconds: 60,
  };
  const timeline = Array.from({ length: 751 }, (_, index) => sample(index * 100, {
    height: index < 50 ? 720 : 360,
    runway_seconds: 2,
    sampled_at_ms: 20_000 + index * 100,
    media_event_seq: 0,
    media_events: [],
  }));
  const transitionStart = {
    sampled_at_ms: 20_000,
    media_event_seq: 0,
    media_events: [],
    auto_switch_seq: 0,
    auto_switches: [],
  };
  const oneDownshift = [autoSwitch(1, 5_000, 720, 360)];
  const clean = lab.scoreRecovery(criteria, observation({
    timeline,
    transition_start: transitionStart,
    transition_end: autoTransition(oneDownshift),
  }));
  assert.deepEqual(clean.errors, []);
  assert.equal(clean.metrics.downshifts, 1);
  assert.equal(clean.metrics.restarts, 0);

  const destructive = timeline.map((row, index) => ({
    ...row,
    attempt_id: index < 50 ? "a1" : "a2",
    player_generation: index < 50 ? 1 : 2,
  }));
  const reopened = lab.scoreRecovery(criteria, observation({
    timeline: destructive,
    transition_start: transitionStart,
    transition_end: autoTransition(oneDownshift),
  }));
  assert.ok(reopened.errors.some((error) => /automatic restarts/.test(error)));

  const gapped = timeline.map((row, index) => index === timeline.length - 1 ? {
    ...row,
    media_event_seq: 2,
    media_events: [
      { seq: 1, event: "waiting", at_ms: 21_000 },
      { seq: 2, event: "playing", at_ms: 21_400 },
    ],
  } : row);
  const discontinuous = lab.scoreRecovery(criteria, observation({
    timeline: gapped,
    transition_start: transitionStart,
    transition_end: {
      sampled_at_ms: 65_000,
      media_event_seq: 2,
      media_events: gapped.at(-1).media_events,
      auto_switch_seq: 1,
      auto_switches: oneDownshift,
    },
  }));
  assert.ok(discontinuous.errors.some((error) => /video gap 400 ms/.test(error)));
  assert.ok(discontinuous.errors.some((error) => /transition wait events/.test(error)));

  const visibleHitch = lab.scoreRecovery(criteria, observation({
    timeline,
    transition_start: transitionStart,
    transition_end: autoTransition(oneDownshift, {
      frame_probe: {
        ...autoTransition(oneDownshift).frame_probe,
        maximum_gap_ms: 200,
      },
    }),
  }));
  assert.ok(visibleHitch.errors.some((error) => /every nightly cliff/.test(error)),
    "one nightly cliff must itself meet the 100ms distribution budget");

  for (const jump of [-1, 1]) {
    const discontinuousLanding = autoTransition(oneDownshift);
    discontinuousLanding.frame_probe.auto_presentations[0].frame.absolute_time += jump;
    const score = lab.scoreRecovery(criteria, observation({
      timeline,
      transition_start: transitionStart,
      transition_end: discontinuousLanding,
    }));
    assert.ok(score.errors.some((error) => /landed 1\.000s from the handoff boundary/.test(error)),
      `${jump < 0 ? "backward" : "forward"} film-position jumps must fail`);
  }

  const silent = timeline.map((row, index) => ({
    ...row,
    absolute_time: index < 4 ? 0 : row.absolute_time,
  }));
  const inertFrameApi = lab.scoreRecovery(criteria, observation({
    timeline: silent,
    transition_start: transitionStart,
    transition_end: autoTransition(oneDownshift, {
      frame_probe: {
        supported: true, sequence: 0, frames: 0,
        last_frame_at_ms: null, maximum_gap_ms: 0, last_frame: null,
      },
    }),
  }));
  assert.ok(inertFrameApi.errors.some((error) => /video gap 400 ms/.test(error)),
    "API presence without a callback uses the sampled-clock fallback");
  assert.equal(inertFrameApi.metrics.transition.frame_probe_operative, false);

  const stoppedFrameApi = lab.scoreRecovery(criteria, observation({
    timeline,
    transition_start: transitionStart,
    transition_end: autoTransition(oneDownshift, {
      frame_probe: {
        supported: true, sequence: 1, frames: 1,
        last_frame_at_ms: 21_000, maximum_gap_ms: 0,
        last_frame: { sequence: 1, at_ms: 21_000 },
      },
    }),
  }));
  assert.ok(stoppedFrameApi.errors.some((error) => /video gap 74000 ms/.test(error)),
    "a callback that stops remains an open presented-frame gap");

  const oscillating = timeline.map((row, index) => ({ ...row, height: index === 0 ? 720 : 360 }));
  const collapsedMoves = [
    autoSwitch(1, 10, 720, 360),
    autoSwitch(2, 20, 360, 480),
    autoSwitch(3, 30, 480, 360),
  ];
  const movedAgain = lab.scoreRecovery(criteria, observation({
    timeline: oscillating,
    transition_start: transitionStart,
    transition_end: autoTransition(collapsedMoves),
  }));
  assert.ok(movedAgain.errors.some((error) => /additional rung move/.test(error)));
  assert.equal(movedAgain.metrics.auto_switches.event_count, 3,
    "moves that collapse between two 100ms samples remain authoritative");

  const beforeFirstPoll = timeline.map((row) => ({ ...row, height: 360 }));
  beforeFirstPoll[0] = { ...beforeFirstPoll[0], height: 720 };
  const seam = lab.scoreRecovery(criteria, observation({
    timeline: beforeFirstPoll,
    transition_start: transitionStart,
    transition_end: autoTransition([autoSwitch(1, 1, 720, 360)]),
  }));
  assert.equal(seam.metrics.downshifts, 1,
    "the pre-cliff boundary makes a fast first-sample transition visible");

  const lateLedger = lab.scoreRecovery(criteria, observation({
    timeline,
    transition_start: transitionStart,
    transition_end: autoTransition([autoSwitch(1, 10_001, 720, 360)]),
  }));
  assert.ok(lateLedger.errors.some((error) => /downshift took 10001 ms/.test(error)),
    "the deadline comes from the switch timestamp, not the next sample");

  const retained = Array.from({ length: 8 }, (_, index) =>
    autoSwitch(index + 2, index + 2, index % 2 ? 360 : 480, index % 2 ? 480 : 360));
  const truncated = lab.scoreRecovery(criteria, observation({
    timeline,
    transition_start: transitionStart,
    transition_end: {
      ...autoTransition(retained),
      auto_switch_seq: 9,
    },
  }));
  assert.ok(truncated.errors.some((error) => /switch trace retained 8\/9/.test(error)),
    "the bounded player log cannot silently discard an earlier move");

  for (const invalid of [Number.NaN, Number.POSITIVE_INFINITY]) {
    const invalidRunway = timeline.map((row) => ({ ...row }));
    invalidRunway[50].runway_seconds = invalid;
    const score = lab.scoreRecovery(criteria, observation({
      timeline: invalidRunway,
      transition_start: transitionStart,
      transition_end: autoTransition(oneDownshift),
    }));
    assert.ok(score.errors.some((error) => /runway .*not finite/.test(error)),
      `${String(invalid)} runway must fail closed`);
  }
});

test("a cliff that was never applied fails as a shaping fault, not a player fault", () => {
  const score = lab.scoreRecovery(CRITERIA, observation({
    shaping: { cliff_applied_at_ms: null, stages: observation().shaping.stages },
  }));
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /never applied cliff 1/);
});

test("a shaper that leaked more than its cap fails as a shaping fault", () => {
  const leaked = observation();
  const after = leaked.shaping.stages[1];
  // Four times the cap, admitted and delivered alike, over the whole stage.
  after.admitted_bytes = Math.round((6000 * after.admitted_span_ms) / 8);
  after.bytes = after.admitted_bytes;
  const score = lab.scoreRecovery(CRITERIA, leaked);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /leaked/);
  assert.match(score.errors[0], /6000 kb\/s/);
});

// The sustained gate is the same identity as the burst gate, read over the
// stage's own ledger span. The fixed burst and slice terms are amortized across
// that span, so a long stage is held far closer to its cap than the flat 1.25x
// this gate used to carry: 1537.9 kb/s rather than 1875 kb/s on a 45 s stage.
test("the sustained bound tightens as the stage it scores gets longer", () => {
  const rate = (spanMs) => (lab.shaperClaimBoundBytes(1500, spanMs) * 8) / spanMs;
  assert.ok(rate(1_000) > rate(10_000), "a one-second window may claim more than a ten-second one");
  assert.ok(rate(45_000) < 1500 * 1.05, `a 45s stage is held within 5% of its cap, got ${rate(45_000)}`);
  assert.ok(rate(45_000) < 1500 * 1.25, "and well inside the flat tolerance this replaced");

  const leaked = observation();
  const after = leaked.shaping.stages[1];
  // 1900 kb/s sustained: inside the old flat 1875 kb/s gate's rounding, inside
  // the one-second burst bound, and past what the bucket can release over 45 s.
  after.admitted_bytes = Math.round((1900 * after.admitted_span_ms) / 8);
  after.bytes = after.admitted_bytes;
  after.admitted_peak_kbps = 1870;
  assert.ok(1870 < lab.shaperBurstBoundKbps(1500),
    "the peak must clear the burst gate, or this does not isolate the sustained gate");
  const score = lab.scoreRecovery(CRITERIA, leaked);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /sustained|admitted \d+ B in/);
  assert.match(score.errors[0], /1900 kb\/s/);
});

// The bucket stores 250 ms of credit and serialization allows exactly one
// outstanding claim, so the most a one-second window can take out is 1.25x the
// cap plus one 16 KiB slice. That ceiling is a consequence of token
// conservation, not an estimate, so a peak sitting exactly on it is healthy and
// the tolerance above it covers only reporting quantization.
test("an admitted peak at the bucket's own conservation ceiling is not a leak", () => {
  const designed = observation();
  const bound = lab.shaperBurstBoundKbps(8000);
  assert.equal(bound, 8000 * (1 + 250 / 1000) + (16 * 1024 * 8) / 1000,
    "the ceiling is stored burst, one window of earned rate, and one in-flight slice");
  designed.shaping.stages[0].admitted_peak_kbps = Number(bound.toFixed(1));
  const score = lab.scoreRecovery(CRITERIA, designed);
  assert.deepEqual(score.errors, []);
  assert.equal(score.outcome, "passed");
});

test("an admitted peak past the conservation ceiling is a leak", () => {
  const leaked = observation();
  leaked.shaping.stages[0].admitted_peak_kbps = 12_000;
  const score = lab.scoreRecovery(CRITERIA, leaked);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /1s burst rate of 12000 kb\/s over a 8000 kb\/s cap/);
});

// The ceiling already carries the one in-flight slice serialization permits; a
// second one — the skew a per-connection drain could add — must still fail.
test("one slice past the conservation ceiling is already a leak", () => {
  const leaked = observation();
  const bound = lab.shaperBurstBoundKbps(1500);
  leaked.shaping.stages[1].admitted_peak_kbps = Number((bound + (16 * 1024 * 8) / 1000).toFixed(1));
  const score = lab.scoreRecovery(CRITERIA, leaked);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /1s burst rate/);
});

// The burst gate carries no tolerance multiplier at all, so the slack above the
// proven ceiling is exactly what rounding the report to 0.1 kb/s can add — one
// reporting quantum, not the ~100 kb/s a 1% multiplier granted on this stage.
test("the burst gate allows one reporting quantum above the ceiling and no more", () => {
  const bound = lab.shaperBurstBoundKbps(8000);
  const reported = Number(bound.toFixed(1));
  assert.ok(Math.abs(reported - bound) <= 0.05, "the ceiling is compared at the quantum it is reported in");

  const designed = observation();
  designed.shaping.stages[0].admitted_peak_kbps = reported;
  assert.equal(lab.scoreRecovery(CRITERIA, designed).outcome, "passed");

  const over = observation();
  over.shaping.stages[0].admitted_peak_kbps = Number((reported + 0.1).toFixed(1));
  const score = lab.scoreRecovery(CRITERIA, over);
  assert.equal(score.outcome, "shaping",
    "0.1 kb/s past the ceiling already fails, so no unproved band survives above it");
  assert.ok(reported + 0.1 < bound * 1.01,
    "and that failing rate sits inside the 1% band this replaced, which would have passed it");
});

test("a stage with no admission ledger invalidates shaping evidence", () => {
  const missing = observation();
  missing.shaping.stages[1].admitted_bytes = null;
  missing.shaping.stages[1].admitted_span_ms = null;
  missing.shaping.stages[1].admitted_peak_kbps = null;
  const score = lab.scoreRecovery(CRITERIA, missing);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /no admission ledger for after-cliff/);

  // A half-populated ledger is a broken artifact, not a healthy stage.
  const partial = observation();
  partial.shaping.stages[1].admitted_peak_kbps = null;
  assert.equal(lab.scoreRecovery(CRITERIA, partial).outcome, "shaping");
});

// The ledger is the shaper's own bookkeeping, so one thing it cannot establish
// is that the browser was fed from it. Delivered bytes are gated as a total —
// the one delivered quantity per-connection drain grouping cannot distort.
test("bytes the browser received but the bucket never released are a leak", () => {
  const bypassed = observation();
  bypassed.shaping.stages[1].bytes += (32 * 16 * 1024) + 1;
  const score = lab.scoreRecovery(CRITERIA, bypassed);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /the browser received \d+ B while the bucket released \d+ B/);
});

// The exemption is provenance, not a ceiling. A blanket `max_sockets x
// slice_bytes` allowance excused 512 KiB of delivery that no claim anywhere
// accounted for — at 1.5 Mb/s, 2.8 seconds of link capacity scoring `passed`.
test("unclaimed delivery is a leak at any size, down to a single slice", () => {
  // No claim crossed the seam, so nothing is exempt and one slice is enough.
  const oneSlice = observation();
  oneSlice.shaping.carried_over_bytes = 0;
  oneSlice.shaping.stages[1].bytes += 16 * 1024;
  const score = lab.scoreRecovery(CRITERIA, oneSlice);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /reconciling 0 B delivered and 0 B refunded/);

  // The old blanket allowance is not available to unclaimed bytes any more.
  const blanket = observation();
  blanket.shaping.carried_over_bytes = 0;
  blanket.shaping.stages[1].bytes += 32 * 16 * 1024;
  assert.equal(lab.scoreRecovery(CRITERIA, blanket).outcome, "shaping");

  // An artifact with no seam field at all exempts nothing rather than 512 KiB.
  const absent = observation();
  delete absent.shaping.carried_over_bytes;
  absent.shaping.stages[1].bytes += 16 * 1024;
  assert.equal(lab.scoreRecovery(CRITERIA, absent).outcome, "shaping");
});

test("a genuine pre-evidence in-flight completion is exempt for exactly its bytes", () => {
  // A slice the bucket priced before `beginEvidence` and delivered after it is
  // real traffic with a real claim; the run still scores.
  const seam = observation();
  seam.shaping.evidence_pending_claim_bytes = 3 * 16 * 1024;
  seam.shaping.carried_over_bytes = 3 * 16 * 1024;
  seam.shaping.stages[1].bytes += 3 * 16 * 1024;
  assert.equal(lab.scoreRecovery(CRITERIA, seam).outcome, "passed");

  // One byte past what actually crossed the seam is still a leak.
  const overrun = observation();
  overrun.shaping.evidence_pending_claim_bytes = 3 * 16 * 1024;
  overrun.shaping.carried_over_bytes = 3 * 16 * 1024;
  overrun.shaping.stages[1].bytes += (3 * 16 * 1024) + 1;
  const score = lab.scoreRecovery(CRITERIA, overrun);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /reconciling 49152 B delivered and 0 B refunded/);
});

test("evidence-seam adjustments cannot exceed the claims measured at the reset", () => {
  const forged = observation();
  forged.shaping.evidence_pending_claim_bytes = 16 * 1024;
  forged.shaping.carried_over_bytes = 16 * 1024;
  forged.shaping.evidence_refunded_claim_bytes = 1;
  const score = lab.scoreRecovery(CRITERIA, forged);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /invalid evidence-seam provenance/);
});

test("a shaping transport error invalidates the recovery verdict", () => {
  const broken = observation();
  broken.shaping.transport_errors = ["GET /hls/session/000001.ts: connection reset"];
  const score = lab.scoreRecovery(CRITERIA, broken);
  assert.equal(score.outcome, "shaping");
  assert.match(score.errors[0], /transport error/);
});

test("a baseline that was never sustainable cannot be read as a recovery verdict", () => {
  const score = lab.scoreRecovery(CRITERIA, observation({ baseline_clock_rate: 0.2 }));
  assert.equal(score.outcome, "browser_playback");
  assert.match(score.errors[0], /not sustainable/);

  const errored = lab.scoreRecovery(CRITERIA, observation({ baseline_error: "media error 3" }));
  assert.equal(errored.outcome, "browser_playback");
  assert.match(errored.errors[0], /before the cliff/);
});

test("a session that never recovers fails", () => {
  const frozen = healthyTimeline().map((row, index) =>
    index < 5 ? row : { ...row, absolute_time: 5 });
  const score = lab.scoreRecovery(CRITERIA, observation({ timeline: frozen }));
  assert.ok(score.errors.some((error) => /never held/.test(error)), score.errors.join("; "));
  assert.notEqual(score.outcome, "passed");
});

test("a fatal post-cliff media error cannot score as recovery", () => {
  const failed = healthyTimeline();
  failed[failed.length - 1] = {
    ...failed[failed.length - 1], media_error: 3, paused: true, ready_state: 0,
  };
  const score = lab.scoreRecovery(CRITERIA, observation({ timeline: failed }));
  assert.equal(score.outcome, "browser_playback");
  assert.match(score.errors[0], /media error 3/);
});

test("a link with headroom that still starved is reported as a supply fault", () => {
  const frozen = healthyTimeline().map((row, index) => (index < 5 ? row : { ...row, absolute_time: 5 }));
  const starved = observation({ timeline: frozen });
  starved.shaping.stages[1].measured_media_kbps = 200; // the link was barely used
  const score = lab.scoreRecovery(CRITERIA, starved);
  assert.equal(score.outcome, "server_supply");
});

test("recovery later than the deadline fails", () => {
  const late = healthyTimeline().map((row, index) =>
    index < 34 ? { ...row, absolute_time: 0 } : { ...row, absolute_time: (index - 34) });
  const score = lab.scoreRecovery(CRITERIA, observation({ timeline: late }));
  assert.ok(score.errors.some((error) => /recovery took|never held/.test(error)), score.errors.join("; "));
  assert.equal(score.outcome, "recovery");
});

test("too many restarts and too many upgrades each fail on their own", () => {
  const churn = healthyTimeline().map((row, index) => ({
    ...row,
    height: [360, 480, 720][index % 3],
    ttff_ms: 500 + (index % 3),
    attempt_id: `a${index + 1}`,
  }));
  const switches = [
    autoSwitch(1, 1_000, 720, 480),
    autoSwitch(2, 2_000, 480, 720),
    autoSwitch(3, 3_000, 720, 480),
    autoSwitch(4, 4_000, 480, 720),
  ];
  const score = lab.scoreRecovery(CRITERIA, observation({
    timeline: churn,
    transition_start: {
      sampled_at_ms: 20_000, media_event_seq: 0, media_events: [],
      auto_switch_seq: 0, auto_switches: [],
    },
    transition_end: autoTransition(switches),
  }));
  assert.ok(score.errors.some((error) => /automatic restarts/.test(error)), score.errors.join("; "));
  assert.ok(score.errors.some((error) => /upgrades inside one 60s window/.test(error)), score.errors.join("; "));
  assert.equal(score.outcome, "recovery");
});

test("a seamless downgrade is not mislabeled as a player restart", () => {
  const stepped = healthyTimeline().map((row, index) =>
    index < 5 ? { ...row, height: 720, ttff_ms: 400 } : { ...row, height: 360, ttff_ms: 900 });
  const events = lab.rungHistory(stepped);
  assert.equal(events.length, 1, "the rung transition remains observable");
  assert.equal(events[0].direction, "down");
  assert.equal(events[0].from_height, 720);
  assert.equal(events[0].to_height, 360);
  assert.equal(events[0].restart_count, 0,
    "an unchanged attempt identity proves the player did not reopen");
});

test("a new player attempt is a restart even when its rung and TTFF match", () => {
  const restarted = healthyTimeline().map((row, index) => ({
    ...row,
    height: 720,
    ttff_ms: 1570,
    attempt_id: index < 20 ? "a1" : "a2",
    attempt_reason: "stall-restart",
  }));
  const events = lab.rungHistory(restarted);
  assert.equal(events.length, 1,
    "the attempt identity is authoritative when presentation symptoms are unchanged");
  assert.equal(events[0].direction, "restart");
  assert.equal(events[0].attempt_reason, "stall-restart");
});

test("consecutive same-reason attempts cannot collapse below the restart budget", () => {
  const restarted = healthyTimeline().map((row, index) => ({
    ...row,
    height: 720,
    ttff_ms: 1570,
    attempt_id: index < 10 ? "a1" : index < 20 ? "a2" : "a3",
    attempt_reason: "stall-restart",
  }));
  const events = lab.rungHistory(restarted);
  assert.equal(events.length, 2);
  assert.deepEqual(events.map((event) => event.attempt_id), ["a2", "a3"]);
  const score = lab.scoreRecovery(CRITERIA, observation({ timeline: restarted }));
  assert.equal(score.metrics.restarts, 2);
  assert.equal(score.outcome, "recovery");
  assert.ok(score.errors.some((error) => /automatic restarts/.test(error)), score.errors.join("; "));
});

test("attempt sequence gaps retain restarts hidden inside one sample interval", () => {
  const restarted = healthyTimeline().map((row, index) => ({
    ...row,
    attempt_id: index < 20 ? "a1" : "a3",
    attempt_reason: "stall-restart",
  }));
  const events = lab.rungHistory(restarted);
  assert.equal(events.length, 1);
  assert.equal(events[0].restart_count, 2);
  assert.equal(lab.scoreRecovery(CRITERIA, observation({ timeline: restarted })).metrics.restarts, 2);
});

// Both of the following were real defects, found by running the harness for
// real against a shaped link rather than by reading the code: the first scored
// a session that plainly never recovered as a pass, and the second reported the
// recovery time as a negative 56-year interval.
test("draining the pre-cliff buffer is not recovery", () => {
  // Ten seconds of banked runway keep the clock at 1.0x, then it collapses —
  // the exact shape a real 8 -> 1.5 Mb/s run produced with no controller.
  const drained = Array.from({ length: 46 }, (_, index) => sample(index * 1000, {
    absolute_time: index <= 10 ? index : 10 + (index - 10) * 0.15,
    stalls: index <= 10 ? 0 : Math.floor((index - 10) / 8),
  }));
  const score = lab.scoreRecovery(CRITERIA, observation({ timeline: drained }));
  assert.ok(score.errors.some((error) => /never held/.test(error)), score.errors.join("; "));
  assert.notEqual(score.outcome, "passed");
  assert.equal(lab.timeToSustained(drained, 10, 0.9), null,
    "a sustained window must reach the end of the observation, not stop where the buffer ran out");
});

test("an observation too short to outlast the banked runway is refused as harness error", () => {
  const score = lab.scoreRecovery(
    { ...CRITERIA, recovery_observe_seconds: 12, sustained_seconds: 10 },
    observation({ pre_cliff_runway_seconds: 10 }),
  );
  assert.equal(score.outcome, "harness");
  assert.match(score.errors[0], /cannot outlast/);
});

test("timeline timestamps are relative to the cliff, so they read forward from zero", () => {
  const score = lab.scoreRecovery(CRITERIA, observation());
  assert.ok(score.metrics.recovered_after_ms >= 0,
    `recovery time must not be negative, got ${score.metrics.recovered_after_ms}`);
  assert.ok(score.metrics.recovered_after_ms < 60_000, "and must be within the observation window");
});

test("the upgrade rate is measured over a sliding 60s window", () => {
  const events = [
    { at_ms: 0, direction: "up" },
    { at_ms: 30_000, direction: "up" },
    { at_ms: 59_000, direction: "up" },
    { at_ms: 120_000, direction: "up" },
    { at_ms: 10_000, direction: "down" },
  ];
  assert.equal(lab.peakUpgradeRate(events), 3);
  assert.equal(lab.peakUpgradeRate([]), 0);
});

test("sustained playback is only credited when the clock really advanced", () => {
  const good = Array.from({ length: 20 }, (_, index) => sample(index * 1000));
  assert.equal(lab.timeToSustained(good, 10, 0.9), 0);
  const stalled = good.map((row) => ({ ...row, absolute_time: 0 }));
  assert.equal(lab.timeToSustained(stalled, 10, 0.9), null);
  const restalled = good.map((row, index) => ({ ...row, stalls: index > 3 ? 1 : 0 }));
  assert.equal(lab.timeToSustained(restalled, 10, 0.9), 4000);
});

test("a restart cannot erase later stalls by resetting the player counter", () => {
  const reset = healthyTimeline().map((row, index) => ({
    ...row,
    height: index < 20 ? 720 : 480,
    attempt_id: index < 20 ? "a1" : "a2",
    attempt_reason: index < 20 ? "cold-start" : "quality",
    player_generation: index < 20 ? 1 : 2,
    stalls: index < 20 ? 5 : index < 30 ? 0 : index < 38 ? 1 : 2,
  }));
  assert.equal(lab.rungHistory(reset).length, 1, "the counter reset occurs at one permitted restart");
  assert.equal(lab.timeToSustained(reset, 10, 0.9), null,
    "two post-restart stalls leave too little clean runway to prove recovery");
  const score = lab.scoreRecovery(CRITERIA, observation({ timeline: reset }));
  assert.equal(score.outcome, "recovery");
  assert.match(score.errors[0], /never held/);
});

test("an in-place restart does not recount stalls carried by the same player", () => {
  const carried = healthyTimeline().map((row, index) => ({
    ...row,
    height: 720,
    attempt_id: index < 31 ? "a1" : "a2",
    attempt_reason: index < 31 ? "cold-start" : "stall-restart",
    stalls: index === 0 ? 0 : index === 1 ? 1 : 2,
  }));
  const events = lab.rungHistory(carried);
  assert.equal(events.length, 1);
  assert.equal(events[0].at_ms, 31_000);
  assert.equal(events[0].counter_rebase, false);
  assert.equal(lab.timeToSustained(carried, 10, 0.9), 2000,
    "the restart must not add the same two pre-restart stalls a second time");
  const score = lab.scoreRecovery(CRITERIA, observation({ timeline: carried }));
  assert.equal(score.outcome, "passed");
  assert.equal(score.metrics.recovered_after_ms, 2000);
  assert.equal(score.metrics.restarts, 1);
});

test("missing attempt identity or an unexplained counter reset invalidates the run", () => {
  const missing = healthyTimeline();
  missing[5] = { ...missing[5], attempt_id: null };
  const missingScore = lab.scoreRecovery(CRITERIA, observation({ timeline: missing }));
  assert.equal(missingScore.outcome, "harness");
  assert.match(missingScore.errors[0], /missing player attempt/);

  const reset = healthyTimeline().map((row, index) => ({
    ...row,
    stalls: index < 10 ? 3 : 0,
  }));
  const resetScore = lab.scoreRecovery(CRITERIA, observation({ timeline: reset }));
  assert.equal(resetScore.outcome, "harness");
  assert.match(resetScore.errors[0], /decreased without an object rebase/);
});

// ------------------------------------------------------------ the artifact

const ARTIFACT = {
  schema_version: 1,
  generated_at: "2026-08-13T19:00:00.000Z",
  suite: "smoke",
  browser: { browser: "Chrome" },
  network_profile: null,
  summary: { total: 1, passed: 1, failed: 0 },
  results: [{
    name: "direct-h264-aac-1080 :: auto :: steady",
    status: "passed",
    operation: "steady",
    decision: { method: "direct_play", delivery: { mode: "file" } },
    metrics: { actual_method: "direct", copy_hls: false, encoder: null, decoded_dimensions: "1920x1080" },
    end: { tried_fallback: false },
    errors: [],
    warnings: [],
  }],
};

async function withTempDir(run) {
  const directory = await fsp.mkdtemp(path.join(os.tmpdir(), "plurx-shaping-test-"));
  try {
    return await run(directory);
  } finally {
    await fsp.rm(directory, { recursive: true, force: true });
  }
}

test("a missing, unparseable, or foreign artifact is refused rather than read as empty", async () => {
  await withTempDir(async (directory) => {
    await assert.rejects(() => lab.loadArtifact(path.join(directory, "absent.json")), /could not be read/);
    await assert.rejects(() => lab.loadArtifact(undefined), /--json PATH is required/);

    const broken = path.join(directory, "broken.json");
    await fsp.writeFile(broken, "{ not json", "utf8");
    await assert.rejects(() => lab.loadArtifact(broken), /not valid JSON/);

    const foreign = path.join(directory, "foreign.json");
    await fsp.writeFile(foreign, JSON.stringify({ hello: "world" }), "utf8");
    await assert.rejects(() => lab.loadArtifact(foreign), /not a playback-lab report/);

    const truncated = path.join(directory, "truncated.json");
    await fsp.writeFile(truncated, JSON.stringify({ schema_version: 1 }), "utf8");
    await assert.rejects(() => lab.loadArtifact(truncated), /not a playback-lab report/);

    const good = path.join(directory, "good.json");
    await fsp.writeFile(good, JSON.stringify(ARTIFACT), "utf8");
    assert.equal((await lab.loadArtifact(good)).suite, "smoke");
  });
});

test("fatal artifacts retain the failure without retaining credentials", async () => {
  await withTempDir(async (directory) => {
    const json = path.join(directory, "fatal.json");
    const junit = path.join(directory, "fatal.xml");
    const error = new Error("fetch /hls/1.m3u8?token=secret-lab-token with Bearer secret-bearer failed");
    await lab.writeFatalRunReport({ suite: "stall-recovery", json, junit }, error);
    const retained = `${await fsp.readFile(json, "utf8")}\n${await fsp.readFile(junit, "utf8")}`;
    assert.doesNotMatch(retained, /secret-lab-token|secret-bearer/);
    assert.match(retained, /<redacted>/);
  });
});

test("normalization removes every run-local value and keeps the behavioral shape", () => {
  const noisy = JSON.parse(JSON.stringify(ARTIFACT));
  noisy.results[0].errors = [
    "session 6f1c9d02-4b7a-4a1e-9f2b-1c3d4e5f6a7b failed at 127.0.0.1:52341",
    "runtime /var/folders/xy/T/plurx-playback-lab-Ab3d9/data went away after 1234 ms",
    "started at 2026-08-13T19:00:00.000Z",
  ];
  const normalized = lab.normalizeTrace(noisy);
  const text = JSON.stringify(normalized);
  assert.doesNotMatch(text, /6f1c9d02/, "UUIDs are scrubbed");
  assert.doesNotMatch(text, /52341/, "ports are scrubbed");
  assert.doesNotMatch(text, /plurx-playback-lab-Ab3d9/, "temp paths are scrubbed");
  assert.doesNotMatch(text, /2026-08-13T19/, "wall-clock is scrubbed");
  assert.doesNotMatch(text, /1234 ms/, "durations are scrubbed");
  assert.equal(normalized.results[0].decision_method, "direct_play");
  assert.equal(normalized.results[0].decoded_dimensions, "1920x1080");
  assert.equal(normalized.results[0].status, "passed");
});

test("two runs that differ only in run-local values normalize identically", () => {
  const first = JSON.parse(JSON.stringify(ARTIFACT));
  const second = JSON.parse(JSON.stringify(ARTIFACT));
  second.generated_at = "2027-01-01T05:06:07.000Z";
  second.results[0].duration_ms = 99_999;
  second.results[0].metrics.ttff_ms = 4242;
  assert.deepEqual(lab.normalizeTrace(first), lab.normalizeTrace(second));
});

// ------------------------------------------------------------------- CLI

test("API parsing preserves opaque 64-bit ids for later routes", () => {
  const parsed = lab.parseApiJson(
    '{"id":8622887431169855001,"file_id":7,"nested":{"item_id":-9007199254740993}}',
  );
  assert.equal(parsed.id, "8622887431169855001");
  assert.equal(parsed.file_id, 7, "safe ids keep the API's ordinary numeric shape");
  assert.equal(parsed.nested.item_id, "-9007199254740993");
});

test("VOD readiness waits for the exact file built by an indexing pass", () => {
  const pass = {
    message: "fragment indexing pass finished attempted=2 built=2 built_files=[1, 42]",
  };
  assert.equal(lab.fragmentIndexPassBuiltFile(pass, 42), true);
  assert.equal(lab.fragmentIndexPassBuiltFile(pass, 4), false);
  assert.equal(lab.fragmentIndexPassBuiltFile(pass, 420), false);
  assert.equal(lab.fragmentIndexPassBuiltFile({
    message: "fragment indexing pass finished attempted=1 built=1",
  }, 42), false);
});

test("actual Auto pressure uses measured peaks and refuses an unsafe two-rung interval", () => {
  const qualification = require("../../scripts/continuous-quality-qualification");
  const catalog = [{route: "encode", height: 720, peak_bps: 6160000},
    {route: "encode", height: 1080, peak_bps: 12160000},
    {route: "copy", height: 1080, peak_bps: 999999999}];
  const profile = qualification.autoLinkProfile(catalog);
  assert.equal(profile.stages.length, 5);
  assert.equal(profile.stages[0], profile.stages[2]);
  assert.equal(profile.stages[2], profile.stages[4]);
  assert.equal(profile.stages[1], profile.stages[3]);
  assert.ok(profile.stages[1] * 1000 >= profile.low_floor_bps);
  assert.ok(profile.stages[1] * 1000 < profile.low_ceiling_bps);
  assert.throws(() => qualification.autoLinkProfile([]), /safe pressure interval/);
  assert.throws(() => qualification.autoLinkProfile([
    {route: "encode", height: 720, peak_bps: 1000000},
    {route: "encode", height: 1080, peak_bps: 1548000}]), /shaper rate resolution/);
  assert.throws(() => qualification.autoLinkProfile([
    {route: "encode", height: 720, peak_bps: 9000000},
    {route: "encode", height: 1080, peak_bps: 10000000}]), /safe pressure interval/);
});

test("encoded-only qualification does not wait for an impossible copy fragment index", () => {
  const files = new Map([
    ["encoded.mp4", {id: 1, video_codec: "mpeg4"}],
    ["avc.mp4", {id: 2, video_codec: "h264"}],
    ["hevc.mp4", {id: 3, video_codec: "hevc"}],
    ["unknown.mp4", {id: 4}],
  ]);
  assert.deepEqual(lab.fragmentIndexTargets(files, ["encoded.mp4"]), []);
  assert.deepEqual(lab.fragmentIndexTargets(files, ["encoded.mp4", "avc.mp4", "hevc.mp4"])
    .map(file => file.id), [2, 3]);
  assert.throws(() => lab.fragmentIndexTargets(files, ["unknown.mp4"]), /video codec/);
  assert.throws(() => lab.fragmentIndexTargets(files, ["missing.mp4"]), /scan missed/);
});

test("VOD acceptance pauses startup indexing until its fixture scan is complete", () => {
  const source = fs.readFileSync(LAB, "utf8");
  const start = source.indexOf("async function startServer");
  const end = source.indexOf("\nfunction cdpBrowserArgs", start);
  const server = source.slice(start, end);
  const pause = server.indexOf("body: { vod_index_mins: 0 }");
  const library = server.indexOf('const library = await api(baseUrl, "/libraries"');
  const scan = server.indexOf('"fixture scan"', library);
  const enable = server.indexOf("vod_presentation: true");

  assert.ok(start >= 0 && end > start, "the server harness remains inspectable");
  assert.ok(pause >= 0, "the startup indexer is explicitly paused");
  assert.ok(pause < library, "indexing is paused before the fixture library can scan");
  assert.ok(library < scan, "the fixture library reaches its explicit scan wait");
  assert.ok(scan < enable, "indexing is re-enabled only after the scan wait");
});

test("the manifest keeps the stall-recovery suite reviewable and opt-in", () => {
  const manifest = lab.loadManifest();
  const suite = manifest.suites["stall-recovery"];
  assert.ok(suite, "the stall-recovery suite exists");
  assert.equal(suite.requires_network_profile, true);
  const cases = lab.expandCases(manifest, "stall-recovery");
  assert.equal(cases.length, 1);
  assert.equal(cases[0].operation, "shaped-cliff");
  assert.ok(cases[0].recovery.recovery_deadline_seconds > 0, "the criteria are in the manifest, not the code");
  assert.equal(cases[0].recovery.maximum_automatic_restarts, 0);
  assert.equal(cases[0].recovery.minimum_downshifts, 1);
  assert.equal(cases[0].recovery.maximum_downshifts, 1);
  assert.equal(cases[0].recovery.maximum_downshift_ms, 10_000);
  assert.equal(cases[0].recovery.maximum_single_transition_video_gap_ms, 100);
  assert.equal(cases[0].recovery.maximum_video_gap_ms, 250);
  assert.equal(cases[0].recovery.maximum_landing_error_seconds, 0.25);
  assert.equal(cases[0].recovery.stable_after_downshift_seconds, 60);
  assert.ok(
    cases[0].recovery.recovery_observe_seconds * 1000
      >= cases[0].recovery.maximum_downshift_ms
        + cases[0].recovery.stable_after_downshift_seconds * 1000,
    "the observation covers the latest allowed move plus its full stability dwell",
  );
  assert.ok(
    cases[0].recovery.recovery_observe_seconds
      >= cases[0].recovery.recovery_deadline_seconds
        + cases[0].recovery.sustained_seconds + 15,
    "the evidence window retains headroom for runner-dependent pre-cliff runway",
  );

  // The shaping fixture must not widen the general matrix.
  const full = lab.expandCases(manifest, "full");
  assert.equal(full.some((testCase) => testCase.fixture === "shaping-mpeg4-mp3-720"), false);
  assert.equal(lab.expandCases(manifest, "smoke").length, 11, "the smoke suite is unchanged");
  assert.equal(full.length, 45, "the full suite includes one explicit quality transition");
  const quality = full.find((testCase) => testCase.operation === "quality-cycle");
  assert.deepEqual(quality.switches, [
    { quality: "720", expected_method: "transcode", expected_height: 720 },
    { quality: "original", expected_method: "remux", expected_height: 1080 },
  ]);
  assert.equal(quality.repetitions, 20);
  assert.equal(quality.require_same_session, true);
  assert.equal(quality.require_same_player_generation, true);
  assert.equal(quality.evidence_scope, "browser_video_partial");
  assert.equal(quality.minimum_runway_seconds, 2);
  assert.equal(quality.transition.p95_video_gap_ms, 100);
  assert.equal(quality.transition.maximum_video_gap_ms, 250);
  assert.equal(quality.transition.maximum_reopen_events, 0);
  assert.equal(quality.transition.maximum_stalls, 0);
  assert.equal(quality.transition.maximum_hitches, 0);

  const ordinaryCorpus = lab.fixturesForBuild(manifest);
  assert.equal(ordinaryCorpus.some((fixture) => fixture.id === "shaping-mpeg4-mp3-720"), false,
    "the general fixtures command does not pay for the 120-second opt-in source");
  const shapedCorpus = lab.fixturesForBuild(manifest, new Set(["shaping-mpeg4-mp3-720"]));
  assert.deepEqual(shapedCorpus.map((fixture) => fixture.id), ["shaping-mpeg4-mp3-720"]);
});

test("the CI-provisioned Chromium path is a first-class browser candidate", async () => {
  await withTempDir(async (directory) => {
    const chromium = path.join(directory, "headless-shell");
    await fsp.writeFile(chromium, "fixture", "utf8");
    const previous = process.env.PLURX_PLAYBACK_CHROME;
    process.env.PLURX_PLAYBACK_CHROME = chromium;
    try {
      assert.equal(lab.findChrome(), chromium);
    } finally {
      if (previous === undefined) delete process.env.PLURX_PLAYBACK_CHROME;
      else process.env.PLURX_PLAYBACK_CHROME = previous;
    }
  });
});

test("transition scoring retains gaps and reopens that precede the steady window", () => {
  const event = (seq, name, at) => ({ seq, event: name, at_ms: at });
  const snapshot = (overrides = {}) => ({
    sampled_at_ms: 0,
    media_event_seq: 0,
    media_events: [],
    started: true,
    decided_method: "remux",
    method: "remux",
    tried_fallback: false,
    copy_hls: false,
    vod: false,
    stalls: 0,
    hitches: {},
    ttff_ms: 100,
    session_id: "session-1",
    player_generation: 1,
    video: {
      error: null, current_time: 10, absolute_time: 10, height: 1080,
      width: 1920, runway: 3, dropped: 0, total: 300,
    },
    ...overrides,
  });
  const continuityStart = snapshot({ lifetime_stalls: 2, lifetime_hitches: 4 });
  const operationEnd = snapshot({
    sampled_at_ms: 500,
    media_event_seq: 3,
    media_events: [event(1, "pause", 100), event(2, "emptied", 110), event(3, "playing", 500)],
  });
  const end = snapshot({
    sampled_at_ms: 8_500,
    media_event_seq: 3,
    media_events: operationEnd.media_events,
    video: { ...operationEnd.video, current_time: 18, absolute_time: 18 },
  });
  const score = lab.scoreCase(
    { thresholds: { minimum_clock_rate: 0.9, maximum_hitches: 2, maximum_stalls: 0 } },
    {
      quality: "original",
      operation: "quality-cycle",
      switches: ["720", "original"],
      repetitions: 1,
      require_same_player_generation: true,
      transition: { maximum_video_gap_ms: 250, maximum_wait_events: 0, maximum_reopen_events: 0 },
    },
    { method: "remux", delivery: { mode: "progressive" } },
    operationEnd,
    end,
    8,
    { changes: [{
      quality: "720", ready_ms: 100, landing_error_seconds: 0,
      from_player_generation: 1, to_player_generation: 2,
    }] },
    continuityStart,
  );
  assert.match(score.errors.join("; "), /transition video gap 400 ms/);
  assert.match(score.errors.join("; "), /destructive reopen events/);
  assert.match(score.errors.join("; "), /1\/2 quality switches completed/);
  assert.match(score.errors.join("; "), /replaced the player/);

  assert.deepEqual(lab.sampledVideoGap([
    sample(0, { absolute_time: 10, ready_state: 4 }),
    sample(100, { absolute_time: 10.1, ready_state: 4 }),
    sample(200, { absolute_time: 10.1, ready_state: 1 }),
    sample(300, { absolute_time: 10.1, ready_state: 1 }),
    sample(500, { absolute_time: 10.3, ready_state: 4 }),
  ]), { maximum_clock_gap_ms: 400, samples: 5 });

  const frameMeasured = lab.transitionMetrics(continuityStart, snapshot({
    sampled_at_ms: 800,
    lifetime_stalls: 3,
    lifetime_hitches: 6,
    frame_probe: {
      supported: true, last_frame_at_ms: 750, maximum_gap_ms: 310, frames: 8,
    },
  }));
  assert.equal(frameMeasured.maximum_video_gap_ms, 310,
    "Chromium's presented-frame callback is the precise gap oracle");
  assert.equal(frameMeasured.stalls, 1);
  assert.equal(frameMeasured.hitches, 2);
});

test("player snapshots retain stall and hitch counts across object replacement", () => {
  let now = 10_000;
  const observedVideos = [];
  const page = { __plurxLabInstallVideoProbe: (video) => observedVideos.push(video) };
  const video = {
    currentTime: 10, duration: 100, paused: false, seeking: false, ended: false,
    readyState: 4, videoWidth: 1920, videoHeight: 1080, playbackRate: 1, error: null,
    getVideoPlaybackQuality: () => ({ droppedVideoFrames: 0, totalVideoFrames: 100 }),
  };
  let currentVideo = video;
  const document = { getElementById: () => currentVideo };
  const performance = { now: () => ++now };
  const take = (player, lifetimeStalls, lifetimeHitches) => new Function(
    "PLAYER", "document", "performance", "bufferRunway", "globalThis",
    "PLAYBACK_LIFETIME_STALLS", "PLAYBACK_LIFETIME_HITCHES",
    `return ${lab.playerSnapshotExpression()};`,
  )(player, document, performance, () => 3, page, lifetimeStalls, lifetimeHitches);

  const first = { stalls: 2, hitches: { back: 1 } };
  assert.deepEqual(
    [take(first, 2, 1).lifetime_stalls, take(first, 2, 1).lifetime_hitches],
    [2, 1],
  );
  // These faults land after the last sample of the outgoing object. The
  // page-lifetime counters are incremented at the fault sites, so replacement
  // cannot erase them even though the harness never sees `first` again.
  first.stalls = 3;
  first.hitches.back = 2;
  const second = { stalls: 0, hitches: {} };
  assert.deepEqual(
    [take(second, 3, 2).lifetime_stalls, take(second, 3, 2).lifetime_hitches],
    [3, 2],
    "the outgoing object's final faults survive an unsampled replacement",
  );

  second.stalls = 1;
  second.hitches.held = 4;
  assert.deepEqual(
    [take(second, 4, 6).lifetime_stalls, take(second, 4, 6).lifetime_hitches],
    [4, 6],
  );
  assert.deepEqual(
    [take(null, 4, 6).lifetime_stalls, take(null, 4, 6).lifetime_hitches],
    [4, 6],
  );
  second.recoveringStall = { player: second, kind: "supply", action: "restart",
    position: 42, targetHeight: 240 };
  const duringRecovery = take(second, 4, 6);
  assert.deepEqual(duringRecovery.recovering_stall,
    { kind: "supply", action: "restart", position: 42, target_height: 240 });
  assert.doesNotThrow(() => JSON.stringify(duringRecovery),
    "a persistent-stall snapshot must not return its cyclic runtime player");
  const successor = { ...video, videoHeight: 240 };
  currentVideo = successor;
  take(second, 4, 6);
  assert.equal(observedVideos.at(-1), successor,
    "a prepared switch installs the trace probes on the new authoritative video");
  assert.deepEqual(
    [take(null, 4, 6).lifetime_stalls, take(null, 4, 6).lifetime_hitches],
    [4, 6],
    "a null interval cannot reset the page lifetime",
  );

  const web = require("../web/shell-source.js").shellSource().bodyScript;
  assert.match(web, /p\.stalls=\(p\.stalls\|\|0\)\+1;\s*PLAYBACK_LIFETIME_STALLS\+\+;/);
  assert.match(web, /h\.n\+\+;\s*PLAYBACK_LIFETIME_HITCHES\+\+;/);
});

test("a requested case can never disappear behind a skipped status", () => {
  const testCase = { name: "fixture :: auto :: steady", quality: "auto", operation: "steady" };
  const result = lab.enforceCaseResult(testCase, { id: "fixture" }, { status: "skipped" });
  assert.equal(result.status, "failed");
  assert.equal(result.outcome, "harness");
  assert.match(result.errors[0], /non-terminal status "skipped"/);
});

test("quality cycles wait for the requested rendition and a presented frame", async () => {
  let selected = "original";
  let committed = "original";
  let renditionPolls = 0;
  let sampledAt = 1_000;
  let position = 10;
  let frames = 0;
  let frameSequence = 0;
  let lastFrame = null;
  let maximumGapMs = 0;
  let switchPolls = null;
  const targetFrames = [];
  const presentFrame = () => {
    frames += 1;
    frameSequence += 1;
    const down = committed === "720";
    lastFrame = {
      sequence: frameSequence,
      at_ms: sampledAt,
      media_time: position,
      absolute_time: position,
      width: down ? 1280 : 1920,
      height: down ? 720 : 1080,
      method: down ? "transcode" : "remux",
      session_id: "stable-session",
      attempt_id: "a1",
    };
  };
  const state = () => {
    sampledAt += 50;
    position += 0.05;
    // Playback advances before each request: the lab waits for a fresh
    // outgoing frame before it issues one.
    if (switchPolls === null) presentFrame();
    if (switchPolls !== null) {
      switchPolls += 1;
      renditionPolls = switchPolls;
      if (switchPolls === 1) presentFrame(); // one late frame from the outgoing rendition
      if (switchPolls === 2) committed = selected;
      if (switchPolls === 4) { // first frame after target state was observed
        presentFrame();
        targetFrames.push(frameSequence);
      }
      if (switchPolls >= 5) {
        maximumGapMs = Math.max(maximumGapMs, 150);
        presentFrame(); // the target rendition keeps playing
      }
    }
    const down = committed === "720";
    return {
      sampled_at_ms: sampledAt,
      media_event_seq: 0,
      media_events: [],
      lifetime_stalls: 0,
      lifetime_hitches: 0,
      started: true,
      decided_method: down ? "transcode" : "remux",
      method: down ? "transcode" : "remux",
      session_id: "stable-session",
      attempt_id: "a1",
      player_generation: 7,
      keeper_fires: 0,
      frame_probe: {
        supported: true, sequence: frameSequence, last_frame_at_ms: lastFrame?.at_ms ?? null,
        maximum_gap_ms: maximumGapMs, frames, last_frame: lastFrame,
      },
      video: {
        error: null, paused: false, ended: false, seeking: false, ready_state: 4,
        current_time: position, absolute_time: position, height: down ? 720 : 1080,
        runway: 3,
      },
      hitches: {},
      stalls: 0,
    };
  };
  let current = state();
  const driver = {
    exec: async (script) => {
      const requested = /setQuality\("([^\"]+)"\)/.exec(script);
      if (requested) {
        selected = requested[1];
        renditionPolls = 0;
        switchPolls = 0;
      }
      if (script.includes("probe.maximum_gap_ms=0")) {
        maximumGapMs = 0;
        frames = 0;
        switchPolls = null;
      }
      return true;
    },
    eval: async (expression) => {
      if (expression === "playQuality()") return selected;
      current = state();
      return current;
    },
  };
  const operation = await lab.performOperation(driver, {
    operation: "quality-cycle",
    switches: [
      { quality: "720", expected_method: "transcode", expected_height: 720 },
      { quality: "original", expected_method: "remux", expected_height: 1080 },
    ],
    repetitions: 1,
    switch_hold_seconds: 0.01,
    switch_wait_timeout_seconds: 1,
  }, current);
  assert.equal(operation.changes.length, 2);
  assert.deepEqual(operation.changes.map((change) => change.actual_method), ["transcode", "remux"]);
  assert.deepEqual(operation.changes.map((change) => change.decoded_height), [720, 1080]);
  // Frames keep arriving before and after each switch, so the committed
  // frame is named by the poll that produced it, not by a count.
  assert.equal(targetFrames.length, 2);
  assert.deepEqual(operation.changes.map((change) => change.committed_frame_sequence), targetFrames);
  assert.ok(renditionPolls >= 4,
    "the old frame and the target-state poll were not accepted without a later target frame");
  assert.ok(
    operation.changes[1].from_position_seconds > operation.changes[0].to_position_seconds,
    "the second handoff boundary is sampled after the inter-switch hold",
  );
  assert.deepEqual(operation.changes.map((change) => change.video_gap_ms), [150, 150],
    "the post-commit hold remains inside each switch's gap measurement");

  const inertState = {
    ...current,
    frame_probe: {
      supported: true, sequence: 0, last_frame_at_ms: null,
      maximum_gap_ms: 0, frames: 0, last_frame: null,
    },
  };
  const inertDriver = {
    exec: async () => true,
    eval: async (expression) => expression === "playQuality()" ? "original" : inertState,
  };
  await assert.rejects(
    () => lab.performOperation(inertDriver, {
      operation: "quality-cycle",
      switches: [
        { quality: "720", expected_method: "transcode", expected_height: 720 },
        { quality: "original", expected_method: "remux", expected_height: 1080 },
      ],
      frame_baseline_timeout_seconds: 0.01,
    }, inertState),
    /outgoing frame before quality switch/,
    "an API-present but inert callback cannot omit the first handoff seam",
  );
});

test("quality-cycle scoring rejects missing runway and a single excessive gap", () => {
  const snapshot = {
    sampled_at_ms: 0,
    media_event_seq: 0,
    media_events: [],
    lifetime_stalls: 0,
    lifetime_hitches: 0,
    started: true,
    decided_method: "remux",
    method: "remux",
    tried_fallback: false,
    copy_hls: false,
    vod: false,
    stalls: 0,
    hitches: {},
    ttff_ms: 100,
    session_id: "session-1",
    player_generation: 1,
    frame_probe: { supported: true, last_frame_at_ms: 0, maximum_gap_ms: 0, frames: 1 },
    video: {
      error: null, current_time: 10, absolute_time: 10, height: 1080,
      width: 1920, runway: 3, dropped: 0, total: 300,
    },
  };
  const changes = Array.from({ length: 40 }, (_, index) => ({
    quality: index % 2 ? "original" : "720",
    ready_ms: 100,
    landing_error_seconds: 0,
    from_session_id: "session-1",
    to_session_id: "session-1",
    from_player_generation: 1,
    to_player_generation: 1,
    runway_seconds: index === 0 ? undefined : 3,
    video_gap_ms: index === 1 ? 300 : index < 4 ? 150 : 50,
  }));
  const score = lab.scoreCase(
    { thresholds: { minimum_clock_rate: 0.9, maximum_hitches: 0, maximum_stalls: 0 } },
    {
      quality: "original", operation: "quality-cycle", switches: ["720", "original"],
      repetitions: 20, minimum_runway_seconds: 2,
      transition: { p95_video_gap_ms: 100, maximum_video_gap_ms: 250 },
    },
    { method: "remux", delivery: { mode: "progressive" } },
    snapshot,
    { ...snapshot, sampled_at_ms: 8_000, video: { ...snapshot.video, current_time: 18 } },
    8,
    { changes, timeline: [] },
    snapshot,
  );
  assert.match(score.errors.join("; "), /no measured runway/);
  assert.match(score.errors.join("; "), /video-gap p95 150 ms/);
  assert.match(score.errors.join("; "), /video-gap max 300 ms/);
});

test("continuous switch evidence refuses replacement, future removal and stale presentation", () => {
  const before = { family_id: "family", closed: false, element: 1, hls: 2, media_source: 3,
    buffers: ["video", "audio"].map((type, index) => ({ type, identity: index + 4, removal_sequence: 0, completed_removals: [] })) };
  const after = { ...before, wanted_candidate: "target", presented: { candidate_id: "target", height: 720 },
    transaction: { first_presented_tick: 50, first_presented_at_ms: 1100,
      appended: [{ from_tick: 40, through_tick: 60, timescale: 24 }] } };
  assert.deepEqual(lab.continuousSwitchErrors(before, after, 1000, 720), []);
  assert.match(lab.continuousSwitchErrors(before, { ...after, hls: 99 }, 1000, 720).join(";"), /hls.*replaced/);
  assert.match(lab.continuousSwitchErrors(before, { ...after, transaction: { ...after.transaction,
    first_presented_at_ms: 900 } }, 1000, 720).join(";"), /fresh presented receipt/);
  assert.match(lab.continuousSwitchErrors(before, { ...after, transaction: { ...after.transaction,
    first_presented_tick: 60 } }, 1000, 720).join(";"), /actual appended interval/);
  const removed = { ...after, buffers: after.buffers.map((row) => row.type === "audio" ? { ...row,
    removal_sequence: 1, completed_removals: [{ sequence: 1, from: 20, through: 22, playhead: 10 }] } : row) };
  assert.match(lab.continuousSwitchErrors(before, removed, 1000, 720).join(";"), /audio removed media ahead/);
  removed.buffers[1].completed_removals[0].through = 9;
  assert.deepEqual(lab.continuousSwitchErrors(before, removed, 1000, 720), [], "ordinary back-buffer eviction is allowed");
  removed.buffers[1].completed_removals = [];
  assert.match(lab.continuousSwitchErrors(before, removed, 1000, 720).join(";"), /evidence was truncated/);
  assert.match(lab.continuousSwitchErrors(before, after, undefined, 720).join(";"), /fresh presented receipt/);
});

test("continuous snapshots count only completed removals and keep weak transport identities", () => {
  const listeners = {};
  const buffer = { updating: false, remove() {}, addEventListener(name, fn) { listeners[name] = fn; } };
  const video = { currentTime: 20 };
  const player = { hls: { bufferController: { mediaSource: {}, tracks: { audio: { buffer } } } } };
  const objects = { next: 0, ids: new WeakMap() };
  const before = lab.continuousTransportSnapshot(player, video, objects);
  buffer.remove(0, 10);
  assert.equal(lab.continuousTransportSnapshot(player, video, objects).buffers[0].removal_sequence, 0);
  listeners.updateend();
  const completed = lab.continuousTransportSnapshot(player, video, objects);
  assert.equal(completed.element, before.element);
  assert.equal(completed.hls, before.hls);
  assert.equal(completed.media_source, before.media_source);
  assert.equal(completed.buffers[0].removal_sequence, 1);
  buffer.remove(10, 12); listeners.error(); listeners.updateend();
  assert.equal(lab.continuousTransportSnapshot(player, video, objects).buffers[0].removal_sequence, 1);
  player.hls.bufferController.tracks.audio.buffer = { ...buffer, addEventListener() {} };
  assert.notEqual(lab.continuousTransportSnapshot(player, video, objects).buffers[0].identity, before.buffers[0].identity);
});

test("the continuous suite requires actual production proof for twenty future-load switches", () => {
  const manifest = lab.loadManifest();
  const [testCase] = lab.expandCases(manifest, "continuous");
  assert.equal(testCase.require_continuous, true);
  assert.equal(testCase.require_vod, true);
  assert.equal(testCase.repetitions * testCase.switches.length, 20);
  assert.equal(manifest.suites.continuous.requires_vod, true);
  const fixture = manifest.fixtures.find((row) => row.id === testCase.fixture);
  assert.equal(fixture.opt_in, true);
  assert.ok(fixture.duration_seconds > 20 * 60, "the normal sixty-second frontier must fit without shortening playback buffers");
});

test("the VOD suite makes native seeking and resume invariants executable", () => {
  const manifest = lab.loadManifest();
  const suite = manifest.suites.vod;
  assert.ok(suite, "the named VOD suite exists");
  assert.equal(suite.requires_vod, true);
  const cases = lab.expandCases(manifest, "vod");
  assert.equal(cases.length, 3);
  assert.ok(cases.every((testCase) => testCase.require_vod === true));
  const storm = cases.find((testCase) => testCase.operation === "seek-storm");
  assert.equal(storm.seeks, 20);
  assert.equal(storm.maximum_session_creates, 1, "only the initial create is allowed");
  assert.ok(cases.some((testCase) => testCase.operation === "suspend-resume"));

  const retainedCiCases = lab.expandCases(manifest, "vod", undefined, "seek-storm");
  assert.deepEqual(
    retainedCiCases.map((testCase) => testCase.operation),
    ["steady", "suspend-resume"],
    "a named exclusion keeps the two healthy VOD gates without deleting the storm contract",
  );
  assert.throws(
    () => lab.expandCases(manifest, "vod", undefined, "remux-h264-multitrack-1080"),
    /no cases remain after excluding remux-h264-multitrack-1080/,
  );
});

test("the run command retains JSON and JUnit when a bad profile exits nonzero", async () => {
  await withTempDir(async (directory) => {
    const json = path.join(directory, "bad-profile.json");
    const junit = path.join(directory, "bad-profile.xml");
    const result = cli([
      "run", "--suite", "stall-recovery", "--network-profile", "8mbps-to-9mbps",
      "--json", json, "--junit", junit,
    ]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /must descend/);
    assert.doesNotMatch(result.stdout, /BUILD|START/, "no corpus or server work happens first");
    const artifact = JSON.parse(await fsp.readFile(json, "utf8"));
    assert.equal(artifact.outcome, "harness");
    assert.deepEqual(artifact.summary, { total: 1, passed: 0, failed: 1 });
    assert.match(await fsp.readFile(junit, "utf8"), /failures="1"/);
  });
});

test("a shaped suite refuses to run unshaped and retains the harness failure", async () => {
  await withTempDir(async (directory) => {
    const artifact = path.join(directory, "unshaped.json");
    const result = cli(["run", "--suite", "stall-recovery", "--json", artifact]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /--network-profile/);
    assert.equal(JSON.parse(await fsp.readFile(artifact, "utf8")).outcome, "harness");
  });
});

function lifecycleDependencies(manifest, result, state, startError = null) {
  const fixture = manifest.fixtures.find((entry) => entry.id === "shaping-mpeg4-mp3-720");
  const server = {
    baseUrl: "http://127.0.0.1:41001",
    runtime: "/tmp/playback-lab-contract-runtime",
    token: "contract-token",
    files: new Map([[fixture.filename, { id: 1, filename: fixture.filename }]]),
    close: async () => { state.server_closed += 1; },
  };
  const shaper = {
    start: async () => {
      if (startError) throw startError;
      return "http://127.0.0.1:41002";
    },
    close: async () => { state.shaper_closed += 1; },
    telemetry: () => ({
      profile: "8mbps-to-1.5mbps",
      spec: "8mbps-to-1.5mbps",
      cliff_after_seconds: 12,
      cliff_applied_at_ms: result?.shaping?.cliff_applied_at_ms ?? null,
      transport_errors: [],
      stages: result?.shaping?.stages || [],
    }),
  };
  const driver = {
    start: async () => {},
    close: async () => { state.driver_closed += 1; },
    exec: async () => true,
  };
  return {
    buildFixtures: async () => ({
      directory: "/tmp/playback-lab-contract-fixtures",
      metadata: { [fixture.id]: { duration_seconds: 210 } },
    }),
    startServer: async () => server,
    createShaper: () => shaper,
    createDriver: () => driver,
    prepareBrowser: async () => ({
      browser: "Contract Browser",
      caps: { vcodec: "h264", acodec: "aac", hdr: 0 },
      native_hls: false,
    }),
    api: async (_base, route) => route.startsWith("/system/logs") ? [] : { version: "contract" },
    runOneCase: async (_driver, _server, _manifest, _fixture, testCase) => ({
      name: testCase.name,
      fixture: testCase.fixture,
      quality: testCase.quality,
      operation: testCase.operation,
      status: result.status,
      outcome: result.outcome,
      duration_ms: 0,
      errors: result.errors,
      warnings: [],
      metrics: result.metrics,
      shaping: result.shaping,
    }),
  };
}

test("a shaped suite enables the in-play Auto controller it measures", async () => {
  await withTempDir(async (directory) => {
    const manifest = lab.loadManifest();
    const json = path.join(directory, "auto-enabled.json");
    const junit = path.join(directory, "auto-enabled.xml");
    const state = { server_closed: 0, shaper_closed: 0, driver_closed: 0 };
    const score = lab.scoreRecovery(CRITERIA, observation());
    const result = { ...score, status: "passed", shaping: observation().shaping };
    const dependencies = lifecycleDependencies(manifest, result, state);
    const startServer = dependencies.startServer;
    let serverOptions = null;
    dependencies.startServer = async (options, fixtureDirectory) => {
      serverOptions = options;
      return startServer(options, fixtureDirectory);
    };

    const outcome = await lab.executeRun(manifest, {
      suite: "stall-recovery", network_profile: "8mbps-to-1.5mbps", json, junit,
    }, dependencies);

    assert.deepEqual(outcome, { code: 0, error: null });
    assert.equal(serverOptions.enable_auto_abr, true,
      "the real server must not silently run the Auto acceptance suite with Auto disabled");
    assert.deepEqual(state, { server_closed: 1, shaper_closed: 1, driver_closed: 1 });
  });
});

test("two-cliff run refuses a source that ends before both recovery windows", async () => {
  await withTempDir(async (directory) => {
    const manifest = lab.loadManifest();
    const json = path.join(directory, "short-fixture.json");
    const state = { server_closed: 0, shaper_closed: 0, driver_closed: 0 };
    const result = { ...lab.scoreRecovery(CRITERIA, observation()), status: "passed" };
    const dependencies = lifecycleDependencies(manifest, result, state);
    const buildFixtures = dependencies.buildFixtures;
    dependencies.buildFixtures = async (...args) => {
      const corpus = await buildFixtures(...args);
      corpus.metadata["shaping-mpeg4-mp3-720"].duration_seconds = 120;
      return corpus;
    };
    const outcome = await lab.executeRun(manifest, {
      suite: "stall-recovery",
      network_profile: "8mbps-to-1.1mbps-to-350kbps",
      json,
    }, dependencies);
    assert.equal(outcome.code, 1);
    assert.match(outcome.error.message, /need at least 177s of source/);
    assert.deepEqual(state, { server_closed: 0, shaper_closed: 0, driver_closed: 0 });
    assert.equal(JSON.parse(await fsp.readFile(json, "utf8")).outcome, "harness");
  });
});

test("an unavailable shaper fails the full run, retains artifacts, and cleans owned state", async () => {
  await withTempDir(async (directory) => {
    const manifest = lab.loadManifest();
    const json = path.join(directory, "unavailable.json");
    const junit = path.join(directory, "unavailable.xml");
    const state = { server_closed: 0, shaper_closed: 0, driver_closed: 0 };
    const dependencies = lifecycleDependencies(
      manifest, null, state, new Error("network shaping is unavailable: EACCES"),
    );
    const outcome = await lab.executeRun(manifest, {
      suite: "stall-recovery", network_profile: "8mbps-to-1.5mbps", json, junit,
    }, dependencies);
    assert.equal(outcome.code, 1);
    assert.match(outcome.error.message, /shaping is unavailable/);
    assert.deepEqual(state, { server_closed: 1, shaper_closed: 1, driver_closed: 0 });
    assert.equal(JSON.parse(await fsp.readFile(json, "utf8")).outcome, "harness");
    assert.match(await fsp.readFile(junit, "utf8"), /failures="1"/);
  });
});

test("missed-cliff and failed-recovery verdicts fail the full run and clean every owner", async () => {
  const missed = lab.scoreRecovery(CRITERIA, observation({
    shaping: { cliff_applied_at_ms: null, stages: observation().shaping.stages },
  }));
  const frozen = healthyTimeline().map((row, index) =>
    index < 5 ? row : { ...row, absolute_time: 5 });
  const recovery = lab.scoreRecovery(CRITERIA, observation({ timeline: frozen }));
  for (const [name, score] of [["missed-cliff", missed], ["failed-recovery", recovery]]) {
    await withTempDir(async (directory) => {
      const manifest = lab.loadManifest();
      const json = path.join(directory, `${name}.json`);
      const junit = path.join(directory, `${name}.xml`);
      const state = { server_closed: 0, shaper_closed: 0, driver_closed: 0 };
      const result = { ...score, status: "failed", shaping: observation().shaping };
      if (name === "missed-cliff") result.shaping = { ...result.shaping, cliff_applied_at_ms: null };
      const outcome = await lab.executeRun(manifest, {
        suite: "stall-recovery", network_profile: "8mbps-to-1.5mbps", json, junit,
      }, lifecycleDependencies(manifest, result, state));
      assert.deepEqual(outcome, { code: 1, error: null });
      assert.deepEqual(state, { server_closed: 1, shaper_closed: 1, driver_closed: 1 });
      const artifact = JSON.parse(await fsp.readFile(json, "utf8"));
      assert.equal(artifact.outcome, score.outcome);
      assert.deepEqual(artifact.summary, { total: 1, passed: 0, failed: 1 });
      assert.match(await fsp.readFile(junit, "utf8"), /failures="1"/);
    });
  }
});

test("the normalize command exits nonzero on missing and invalid artifacts", async () => {
  await withTempDir(async (directory) => {
    const missing = cli(["normalize", "--json", path.join(directory, "absent.json")]);
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /could not be read/);

    const malformedPath = path.join(directory, "malformed.json");
    await fsp.writeFile(malformedPath, "{ not json", "utf8");
    const malformed = cli(["normalize", "--json", malformedPath]);
    assert.equal(malformed.status, 1);
    assert.match(malformed.stderr, /not valid JSON/);

    const foreignPath = path.join(directory, "foreign.json");
    await fsp.writeFile(foreignPath, JSON.stringify({ hello: "world" }), "utf8");
    const foreign = cli(["normalize", "--json", foreignPath]);
    assert.equal(foreign.status, 1);
    assert.match(foreign.stderr, /not a playback-lab report/);
  });
});

test("the normalize command emits a stable trace for a real artifact", async () => {
  await withTempDir(async (directory) => {
    const artifact = path.join(directory, "report.json");
    await fsp.writeFile(artifact, JSON.stringify(ARTIFACT), "utf8");
    const result = cli(["normalize", "--json", artifact]);
    assert.equal(result.status, 0, result.stderr);
    const parsed = JSON.parse(result.stdout);
    assert.equal(parsed.results[0].decision_method, "direct_play");
    assert.equal(parsed.network_profile, null);
  });
});

test("the documented acceptance command is the one the harness accepts", () => {
  const help = cli([]);
  assert.equal(help.status, 0, help.stderr);
  assert.match(help.stdout, /--network-profile/);
  assert.match(help.stdout, /8mbps-to-1\.5mbps/);
  assert.match(help.stdout, /stall-recovery/);
});

test("the raw Chromium driver is safe to launch in an unprivileged runner container", () => {
  const args = lab.cdpBrowserArgs(
    { headed: false },
    "http://127.0.0.1:41001",
    "/tmp/playback-lab-chrome-profile",
  );
  assert.ok(args.includes("--headless=new"));
  assert.ok(args.includes("--no-sandbox"), "the Incus runner cannot create Chromium's namespace sandbox");
  assert.ok(args.includes("--disable-dev-shm-usage"), "media playback must not depend on container /dev/shm size");
  assert.ok(args.includes("--remote-debugging-port=0"));
  assert.ok(args.includes("--user-data-dir=/tmp/playback-lab-chrome-profile"));
  assert.equal(args.at(-1), "http://127.0.0.1:41001");
  assert.equal(lab.CDP_DEVTOOLS_TIMEOUT_MS, 90_000, "cold shared hosts need bounded startup headroom");
  assert.equal(lab.CDP_JSON_TIMEOUT_MS, 5_000, "local DevTools discovery must tolerate a loaded host");
  assert.equal(lab.CDP_PAGE_TARGET_TIMEOUT_MS, 60_000, "the first page target gets bounded cold-start headroom");
});

test("the raw Chromium driver creates a page when headless shell exposes none", async () => {
  const calls = [];
  const page = {
    type: "page",
    webSocketDebuggerUrl: "ws://127.0.0.1:41002/devtools/page/created",
  };
  const request = async (url, options = {}) => {
    calls.push({ url, method: options.method || "GET" });
    if (url.endsWith("/json/list")) {
      return { json: async () => [{ type: "browser" }] };
    }
    return { ok: true, json: async () => page };
  };

  const target = await lab.findOrCreateCdpPageTarget(
    "http://127.0.0.1:41002",
    "http://127.0.0.1:41001/#/player/7",
    request,
  );

  assert.deepEqual(target, page);
  assert.deepEqual(calls, [
    { url: "http://127.0.0.1:41002/json/list", method: "GET" },
    {
      url: "http://127.0.0.1:41002/json/new?http%3A%2F%2F127.0.0.1%3A41001%2F%23%2Fplayer%2F7",
      method: "PUT",
    },
  ]);
});

test("the isolated playback server reserves distinct HTTP, Raft, and API ports", async () => {
  const ports = await lab.freePorts(3);
  assert.equal(ports.length, 3);
  assert.equal(new Set(ports).size, 3);
  assert.ok(ports.every((port) => Number.isInteger(port) && port > 0));

  const config = lab.playbackServerConfig("/tmp/playback-data", ...ports);
  assert.match(config, new RegExp(`bind = "127\\.0\\.0\\.1:${ports[0]}"`));
  assert.match(config, new RegExp(`raft_bind = "127\\.0\\.0\\.1:${ports[1]}"`));
  assert.match(config, new RegExp(`api_bind = "127\\.0\\.0\\.1:${ports[2]}"`));
  assert.doesNotMatch(config, /3240[12]/);
});

runAll().then(executed => {
  if (failures.length) {
    process.stderr.write(`\n${failures.length} shaping contract failure(s)\n`);
    process.exitCode = 1;
    return;
  }
  process.stdout.write(`\n${executed} shaping contracts hold\n`);
});


test("VOD attachment census survives console eviction and counts same-session replacement",()=>{
 const event={event:"session_start",encoder:"vod",file_id:"9007199254740999",
  at_unix_ms:2000,session_id:"redacted",extra:'{"presentation":"vod"}'};
 assert.equal(lab.vodAttachmentCensus([event],event.file_id,1000),1);
 assert.equal(lab.vodAttachmentCensus([event,{...event,at_unix_ms:3000}],event.file_id,1000),2);
 assert.equal(lab.vodAttachmentCensus([event,{...event,file_id:"9007199254740998"},
  {...event,at_unix_ms:999},{...event,encoder:"legacy"}],event.file_id,1000),1);
 assert.throws(()=>lab.vodAttachmentCensus(Array(2000).fill(event),event.file_id,1000),/truncated/);
 assert.throws(()=>lab.vodAttachmentCensus(null,event.file_id,1000),/unavailable/);
});


test("sampled removal evidence survives a long Auto window and refuses observation gaps", () => {
  const listeners = new Map();
  const makeBuffer = () => {
    const handlers = {};
    const buffer = { updating: false, remove() {}, addEventListener(name, fn) { handlers[name] = fn; } };
    listeners.set(buffer, handlers); return buffer;
  };
  const video = { currentTime: 1000 };
  const tracks = { video: { buffer: makeBuffer() }, audio: { buffer: makeBuffer() } };
  const player = { hls: { bufferController: { mediaSource: {}, tracks } } };
  const objects = { next: 0, ids: new WeakMap() };
  const snapshot = () => ({ ...lab.continuousTransportSnapshot(player, video, objects),
    family_id: "family", closed: false, wanted_candidate: "target", presented: { candidate_id: "target", height: 720 },
    transaction: { first_presented_tick: 50, first_presented_at_ms: 1100,
      appended: [{ from_tick: 40, through_tick: 60, timescale: 24 }] } });
  const before = snapshot(), evidence = lab.continuousRemovalEvidence(before);
  for (let index = 0; index < 300; index++) {
    for (const { buffer } of Object.values(tracks)) { buffer.remove(index, index + 1); listeners.get(buffer).updateend(); }
    if (index % 16 === 15) lab.observeContinuousRemovals(evidence, snapshot());
  }
  const after = snapshot();lab.observeContinuousRemovals(evidence, after);
  assert.equal(after.buffers[0].completed_removals.length, 64, "the browser journal stays bounded");
  assert.match(lab.continuousSwitchErrors(before, after, 1000, 720).join(";"), /truncated/);
  assert.deepEqual(lab.continuousSwitchErrors(before, after, 1000, 720, evidence), []);
  assert.equal(evidence.buffers[0].completed_count, 300);
  const missed = lab.continuousRemovalEvidence(before);lab.observeContinuousRemovals(missed, after);
  assert.match(lab.continuousSwitchErrors(before, after, 1000, 720, missed).join(";"), /truncated/);
  const next = lab.continuousRemovalEvidence(after);
  tracks.audio.buffer.remove(1001, 1002);listeners.get(tracks.audio.buffer).updateend();
  const futureRemoval = snapshot();lab.observeContinuousRemovals(next, futureRemoval);
  assert.match(lab.continuousSwitchErrors(after, futureRemoval, 1000, 720, next).join(";"), /audio removed media ahead/);
  tracks.video.buffer = makeBuffer();const replaced = snapshot();lab.observeContinuousRemovals(next, replaced);
  assert.match(lab.continuousSwitchErrors(after, replaced, 1000, 720, next).join(";"), /buffer.*replaced/);
});
