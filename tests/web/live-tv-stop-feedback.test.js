"use strict";

const assert = require("node:assert/strict");
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

function harness() {
  const release = deferred(), exit = deferred();
  const message = {textContent: "Playing live"};
  const video = {paused: false, pause() { this.paused = true; }};
  const host = {hidden: false, dataset: {mode: "slot"}};
  const state = {serial: 0, playbackState: "playing"};
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
  `)(state, {stop: () => release.promise}, document, exit);
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
    h.release.resolve({outcome: "stopped"});
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
  h.exit.resolve(); h.release.resolve({outcome: "stopped"});
  await stopped;
  assert.equal(h.message.textContent, "Tuning 2.1…");
  assert.equal(h.state.playbackState, "starting");
  assert.equal(h.host.hidden, false);
}

async function main() {
  await test("stop_feedback_waits_for_release_confirmation", stop_feedback_waits_for_release_confirmation);
  await test("refused_release_never_reports_stopped", refused_release_never_reports_stopped);
  await test("late_stop_confirmation_does_not_overwrite_a_new_channel", late_stop_confirmation_does_not_overwrite_a_new_channel);
}

async function test(name, run) {
  await run();
  process.stdout.write(`PASS ${name}\n`);
}

main().catch(error => { console.error(error); process.exitCode = 1; });
