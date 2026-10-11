"use strict";
const {test} = require("node:test");
const assert = require("node:assert/strict");
const {decodeFrameClockRgb, analyzeFrameClock} = require("../../scripts/playback-frame-clock");

// Independent on-wire fixture: guard D3, counter 0x123456, checksum E5.
function image() {
  const words = [0xd3, 0x56, 0x34, 0x12, 0xe5];
  const bytes = Buffer.alloc(336 * 4 * 3);
  for (let cell = 0; cell < 40; cell++) {
    const value = ((words[Math.floor(cell / 8)] >>> (cell % 8)) & 1) ? 255 : 0;
    for (let y = 0; y < 4; y++) for (let x = 8 + cell * 8; x < 16 + cell * 8; x++) {
      bytes.fill(value, (y * 336 + x) * 3, (y * 336 + x + 1) * 3);
    }
  }
  return bytes;
}

test("optical frame clock rejects ambiguous pixels and corrupt counter integrity", () => {
  const bytes = image();
  assert.equal(decodeFrameClockRgb(bytes), 0x123456);
  assert.equal(decodeFrameClockRgb(bytes.subarray(0, -1)), null);
  assert.equal(decodeFrameClockRgb(Array.from(bytes)), null);
  for (const cell of [0, 8, 16, 24, 32]) {
    const damaged = Buffer.from(bytes), x = 8 + cell * 8 + 4;
    for (let y = 0; y < 4; y++) for (let c = 0; c < 3; c++) damaged[(y * 336 + x) * 3 + c] ^= 255;
    assert.equal(decodeFrameClockRgb(damaged), null);
  }
  const ambiguous = Buffer.from(bytes);
  for (let y = 0; y < 4; y++) ambiguous.fill(128, (y * 336 + 12) * 3, (y * 336 + 13) * 3);
  assert.equal(decodeFrameClockRgb(ambiguous), null);
});

test("optical capture preserves held pictures and missing sampling as independent evidence", () => {
  const samples = Array.from({length: 25}, (_, i) => ({at_ms: i * 8, frame: Math.floor(i / 5)}));
  const ordinary = analyzeFrameClock(samples, {startMs: 0, endMs: 192});
  assert.equal(ordinary.complete, true);
  assert.equal(ordinary.backward_frames, 0);
  assert.equal(ordinary.skipped_counter_values, 0);
  assert.ok(ordinary.maximum_hold_upper_ms <= 48);
  const held = analyzeFrameClock(samples.map(s => ({...s, frame: 0})), {startMs: 0, endMs: 192});
  assert.equal(held.complete, true);
  assert.equal(held.maximum_hold_lower_ms, 192);
  assert.equal(held.maximum_hold_upper_ms, 192);
  const missing = analyzeFrameClock(samples.filter((_, i) => i < 5 || i > 10), {startMs: 0, endMs: 192});
  assert.equal(missing.complete, false);
  assert.ok(missing.capture_gaps > 0);
  const unreadable = analyzeFrameClock(samples.map((s, i) => i === 9 ? {...s, frame: null} : s), {startMs: 0, endMs: 192});
  assert.equal(unreadable.complete, false);
  assert.equal(unreadable.unknown_samples, 1);
  const backward = analyzeFrameClock(samples.map((s, i) => i === 10 ? {...s, frame: 0} : s), {startMs: 0, endMs: 192});
  assert.ok(backward.backward_frames > 0);
  assert.equal(analyzeFrameClock(samples.slice(0, -4), {startMs: 0, endMs: 192}).complete, false);
  assert.throws(() => analyzeFrameClock(samples), /explicit/);
});
