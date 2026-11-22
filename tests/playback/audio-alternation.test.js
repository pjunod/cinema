"use strict";

// Negative controls for the audio-alternation observer: an injected skip,
// a held picture, a forced reopen and a falsely reported switch must each
// fail, and an uninterrupted in-place switch must pass.
const assert = require("node:assert/strict");
const test = require("node:test");
const {scoreAlternation} = require("../../scripts/playback-audio-alternation.js");

const FRAME_MS = 1000 / 24;

// 60 Hz samples of a 24 fps picture, on one session/element/hls unless told otherwise.
function capture({until = 4000, mutate = () => ({}), audio = () => "eng"} = {}) {
  const frames = [];
  for (let at = 0; at <= until; at += 1000 / 60) {
    frames.push({at_ms: at, frame: Math.floor(at / FRAME_MS), same_element: true,
      session_id: "s1", hls_id: 1, media_time: at / 1000, seeking: false, ...mutate(at)});
  }
  const rows = [];
  for (let at = 0; at <= until; at += 10) rows.push({at_ms: at, label: audio(at), rms: 0.3});
  return {frames, audio: rows, errors: []};
}

const SWITCH = [{target: 1, target_label: "fre", tap_ms: 1000}];
const french = at => at >= 1080 ? "fre" : "eng";

test("an in-place switch with continuous pictures is healthy", () => {
  const score = scoreAlternation(capture({audio: french}), SWITCH, {baselineHoldMs: FRAME_MS});
  assert.equal(score.healthy, 1, JSON.stringify(score.switches[0].failures));
  assert.ok(Math.abs(score.switches[0].delay_ms - 80) < 11);
});

test("an injected one-frame skip fails", () => {
  const score = scoreAlternation(capture({audio: french,
    mutate: at => at >= 1200 ? {frame: Math.floor(at / FRAME_MS) + 1} : {}}), SWITCH, {baselineHoldMs: FRAME_MS});
  assert.ok(score.switches[0].failures.includes("skipped pictures"));
});

test("a held picture fails", () => {
  const score = scoreAlternation(capture({audio: french,
    mutate: at => at >= 1100 && at < 1400 ? {frame: Math.floor(1100 / FRAME_MS)}
      : at >= 1400 ? {frame: Math.floor(at / FRAME_MS) - 7} : {}}), SWITCH, {baselineHoldMs: FRAME_MS});
  assert.ok(score.switches[0].failures.includes("held picture"));
});

test("a forced reopen fails on session and hls identity", () => {
  const score = scoreAlternation(capture({audio: french,
    mutate: at => at >= 1050 ? {session_id: "s2", hls_id: 2} : {}}), SWITCH, {baselineHoldMs: FRAME_MS});
  assert.ok(score.switches[0].failures.includes("session changed"));
  assert.ok(score.switches[0].failures.includes("hls instance changed"));
});

test("a reported switch that never becomes audible fails", () => {
  const score = scoreAlternation(capture(), SWITCH, {baselineHoldMs: FRAME_MS});
  assert.ok(score.switches[0].failures.includes("requested language never confirmed audible"));
});

test("audio confirmed only after the next tap is not credited to the earlier switch", () => {
  const switches = [{target: 1, target_label: "fre", tap_ms: 1000}, {target: 0, target_label: "eng", tap_ms: 2000}];
  const score = scoreAlternation(capture({audio: at => at >= 2500 ? "fre" : "eng"}), switches, {baselineHoldMs: FRAME_MS});
  assert.equal(score.switches[0].delay_ms, null);
});

test("introduced silence above 20 ms fails", () => {
  const score = scoreAlternation(capture({audio: at => at < 1000 ? "eng" : at < 1100 ? "silence" : "fre"}),
    SWITCH, {baselineHoldMs: FRAME_MS});
  assert.ok(score.switches[0].failures.includes("introduced silence"));
});
