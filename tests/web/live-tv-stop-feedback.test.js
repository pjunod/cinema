"use strict";

const assert = require("node:assert/strict");
const liveTv = require("../../crates/plurxd/src/web/live-tv.js");
const policy = require("../../crates/plurxd/src/web/playback-policy.js");
const {shellSource} = require("./shell-source.js");
const source = shellSource().bodyScript;

function shipped(name) {
  const match = new RegExp(`\\n(?:async )?function ${name}\\(`).exec(source);
  assert.ok(match, `missing shipped function ${name}`);
  const end = source.indexOf("\n}", match.index + 1);
  assert.ok(end > match.index, `missing closing brace for ${name}`);
  return source.slice(match.index + 1, end + 2);
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return {promise, resolve, reject};
}

function harness(options = {}) {
  const release = deferred(), exit = deferred();
  const message = {textContent: "Playing live"};
  const video = {paused: false, pause() { this.paused = true; }};
  const host = {hidden: false, dataset: {mode: "slot"}};
  const state = options.state || {serial: 0, playbackState: "playing"};
  const nodes = {"live-tv-message": message, "live-tv-video": video, "live-tv-host": host};
  const document = {getElementById: id => nodes[id] || null};
  const stop = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "exit", `
    const location = {hash: "#/live-tv"};
    function liveTvSyncTransportButtons() {}
    function exitLiveTvPresentation() { return exit.promise; }
    function detachLiveTvMedia() {}
    ${shipped("liveTvMessage")}
    ${shipped("liveTvPlaybackState")}
    ${shipped("stopLiveTv")}
    return stopLiveTv;
  `)(state, options.lease || {stop: () => release.promise}, document, exit);
  return {stop, release, exit, message, state, video, host};
}

async function stop_feedback_waits_for_release_confirmation() {
  const h = harness(), stopped = h.stop();
  try {
    assert.equal(h.video.paused, true);
    assert.equal(h.message.textContent, "Stopping live TV…",
      "Stop must replace Playing live before remote release completes");
    h.exit.resolve();
    await Promise.resolve();
    assert.equal(h.message.textContent, "Stopping live TV…",
      "presentation teardown alone does not confirm remote release");
    h.release.resolve(true);
    await stopped;
    assert.equal(h.message.textContent, "Live TV stopped. Select a channel to watch.");
  } finally {
    h.exit.resolve(); h.release.resolve(); await stopped;
  }
}

async function refused_release_never_reports_stopped() {
  const h = harness(), stopped = h.stop();
  h.release.reject(new Error("release refused"));
  h.exit.resolve();
  await assert.rejects(stopped, /release refused/);
  assert.equal(h.message.textContent, "Stopping live TV…");
  assert.doesNotMatch(h.message.textContent, /TV stopped/);
}

async function late_stop_confirmation_does_not_overwrite_a_new_channel() {
  const h = harness(), stopped = h.stop();
  h.state.serial++;
  h.state.playbackState = "starting";
  h.message.textContent = "Tuning 2.1…";
  h.exit.resolve(); h.release.resolve(true);
  await stopped;
  assert.equal(h.message.textContent, "Tuning 2.1…");
  assert.equal(h.state.playbackState, "starting");
  assert.equal(h.host.hidden, false);
}

async function unanswered_start_retains_hint_without_confirming_release() {
  const values = new Map(), posts = [], unanswered = deferred(), entered = deferred();
  let releases = 0;
  const storage = {get length() { return values.size; }, key: i => [...values.keys()][i],
    getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value),
    removeItem: key => values.delete(key)};
  const leaseBlock = source.slice(source.indexOf("const LIVE_TV="),
    source.indexOf("async function liveTvRequest("));
  assert.match(leaseBlock, /const LIVE_TV_LEASE=new PlurxLiveTv.Lease/);
  const scope = new Function("PlurxLiveTv", "PlaybackPolicy", "performance", "Date",
    "localStorage", "crypto", "liveTvRequest", "BroadcastChannel", "currentCapsDocument", `
    ${leaseBlock}
    return {state: LIVE_TV, lease: LIVE_TV_LEASE, hints: LIVE_TV_HINTS};
  `)(liveTv, policy, {now: () => 0}, {now: () => 1000}, storage,
    {getRandomValues: bytes => { bytes.fill(42); return bytes; }},
    async (path, method, _timeout, _auth, _keepalive, body) => {
      if (method === "POST" && path.endsWith("/sessions")) {
        posts.push(body.request_id); entered.resolve(); return unanswered.promise;
      }
      if (method === "DELETE") releases++;
      return {outcome: "retired"};
    }, function () { throw new Error("no BroadcastChannel"); },
    () => ({video: [{codec: "h264", profiles: ["high"], max_height: 1080}], audio: ["aac"]}));
  scope.state.protocols = [1, 2, 3];
  scope.state.starting = 1;
  const starting = scope.lease.start("7.1");
  const refused = assert.rejects(starting, error => error.code === "no_answer");
  await entered.promise;
  const held = scope.state.hint;
  assert.ok(held);
  const h = harness(scope), stopped = h.stop();
  h.exit.resolve(); unanswered.reject(new Error("start did not answer"));
  await refused;
  assert.equal(await stopped, false, "real Lease has no known capability to release");
  assert.deepEqual(posts, [held, held], "existing replay keeps the same request id");
  assert.equal(releases, 0, "no release acknowledgement was received");
  assert.equal(scope.state.hint, held);
  assert.equal(scope.hints.list()[0].id, held, "the real start path retains recovery");
  assert.equal(h.message.textContent, "Playback stopped locally. Tuner release is unconfirmed.");
}

async function idle_stop_does_not_fabricate_owner_release() {
  const lease = new liveTv.Lease({start: async () => { throw new Error("must not start"); },
    release: async () => { throw new Error("must not release"); }});
  const h = harness({lease}), stopped = h.stop();
  h.exit.resolve();
  assert.equal(await stopped, false);
  assert.equal(h.message.textContent, "Playback stopped locally. Select a channel to watch.");
}

async function main() {
  await test("stop_feedback_waits_for_release_confirmation", stop_feedback_waits_for_release_confirmation);
  await test("refused_release_never_reports_stopped", refused_release_never_reports_stopped);
  await test("late_stop_confirmation_does_not_overwrite_a_new_channel", late_stop_confirmation_does_not_overwrite_a_new_channel);
  await test("unanswered_start_retains_hint_without_confirming_release", unanswered_start_retains_hint_without_confirming_release);
  await test("idle_stop_does_not_fabricate_owner_release", idle_stop_does_not_fabricate_owner_release);
}

async function test(name, run) {
  if (process.argv[2] && process.argv[2] !== name) return;
  await run();
  process.stdout.write(`PASS ${name}\n`);
}

module.exports = main;
if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
