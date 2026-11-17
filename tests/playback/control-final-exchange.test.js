"use strict";
const assert = require("node:assert/strict");
const test = require("node:test");
const control = require("../../crates/plurxd/src/web/playback-control.js");

const owner = { lifecycleId: "final-exchange", attachmentGeneration: 1 };
const generation = "11111111-1111-4111-8111-111111111111";
const actionId = "33333333-3333-4333-8333-333333333333";
function capture(position = 1000, acknowledgement = null, demand = "active") {
  return control.capture({ demand, position_ms: position, buffered_from_ms: position,
    buffered_through_ms: position + 10000, playback_rate: 1, render_state: "rendering",
    seek_target_ms: null, observed_download_bps: null,
    selection: { quality: { mode: "auto" }, audio_track: 0,
      subtitle: { mode: "off", track: null }, audio_offset_ms: 0,
      codec: "auto", dynamic_range: "auto" },
    capabilities: { platform: "web", max_height: 2160, codecs: ["hevc"],
      dynamic_ranges: ["sdr"], dual_player_preparation: true },
    observation: null, acknowledgement }, 0, owner);
}
function committed(position = 1000) {
  return capture(position, { action_id: actionId, state: "committed",
    first_frame_unix_ms: 1800000000000, committed_media_origin_ms: 0 });
}
function answer(request) {
  return { protocol: control.PROTOCOL, generation: request.generation,
    control_epoch: request.control_epoch, accepted_sequence: request.sequence,
    action: { type: "none" } };
}
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function flush() { for (let i = 0; i < 8; i++) await Promise.resolve(); }
function harness(send, onExchange = () => {}) {
  let now = 0, id = 0, live = capture();
  const timers = new Map(), sent = [];
  const reporter = new control.Reporter({
    bootstrap: { protocol: control.PROTOCOL, url: "/api/v1/hls/predecessor/control",
      generation, control_epoch: 7, next_exchange_ms: 5000, lease_timeout_ms: 300000 },
    clientInstanceId: "22222222-2222-4222-8222-222222222222",
    capture: () => live,
    send: (url, request) => { sent.push({ url, request }); return send(request, sent.length); },
    onExchange, now: () => now,
    setTimer: (fn, ms) => { timers.set(++id, { fn, at: now + ms }); return id; },
    clearTimer: key => timers.delete(key),
  });
  return { reporter, sent, timers, live(value) { live = value; },
    async advance(ms) {
      now += ms;
      for (const [key, timer] of [...timers]) if (timer.at <= now) {
        timers.delete(key); timer.fn();
      }
      await flush();
    } };
}

test("final exchange retries exact bytes after a lost response and stops before ordinary work can escape", async () => {
  let h, notifyDuringAcceptance;
  const events = [];
  h = harness((request, attempt) => attempt === 1
    ? Promise.reject(new Error("response lost")) : Promise.resolve(answer(request)), event => {
      events.push(event);
      if (event.response) notifyDuringAcceptance = h.reporter.notify(capture(99000));
    });
  h.reporter.finish(committed(4000));
  const final = h.reporter.terminalCapture;
  await flush();
  h.live(null);
  h.reporter.notify(capture(90000));
  h.reporter.finish(committed(80000));
  assert.strictEqual(h.reporter.capture(), final, "teardown can still read the immutable owner capture");
  await h.advance(5000);
  assert.equal(h.sent.length, 2);
  assert.strictEqual(h.sent[1].request, h.sent[0].request);
  assert.equal(h.sent[1].request.position_ms, 4000);
  assert.strictEqual(events[1].capture, final);
  assert.equal(notifyDuringAcceptance, null, "the accepted callback cannot queue ordinary work");
  assert.equal(h.reporter.stopped, true);
  assert.equal(h.reporter.notify(capture()), null);
  assert.equal(h.reporter.finish(committed()), null);
  await h.advance(60000);
  assert.equal(h.sent.length, 2);
  assert.equal(h.timers.size, 0);
});

test("an earlier rate-limited progress request settles before the queued immutable final", async () => {
  const first = deferred();
  const h = harness((request, attempt) => attempt === 1 ? first.promise : Promise.resolve(answer(request)));
  h.reporter.start();
  h.reporter.finish(committed(5000));
  const final = h.reporter.terminalCapture;
  h.live(capture(90000)); h.reporter.notify(capture(80000));
  first.reject(Object.assign(new Error("busy"), { status: 429, code: "control_rate_limited", retryAfterMs: 500 }));
  await flush(); await h.advance(500);
  assert.equal(h.sent.length, 2);
  assert.strictEqual(h.sent[0].request, h.sent[1].request, "the earlier retry keeps its request and sequence");
  assert.equal(h.sent[1].request.acknowledgement, null);
  await h.advance(250);
  assert.equal(h.sent.length, 3);
  assert.equal(h.sent[2].request.sequence, 2);
  assert.equal(h.sent[2].request.acknowledgement.state, "committed");
  assert.equal(h.sent[2].request.position_ms, 5000);
  assert.strictEqual(h.reporter.terminalCapture, final);
  assert.equal(h.reporter.stopped, true);
});

test("final exchange freezes a mutable supplied capture and explicit notifications cannot replace it", async () => {
  const held = deferred();
  const h = harness(() => held.promise);
  const value = JSON.parse(JSON.stringify(committed(7000)));
  h.reporter.finish(value);
  value.snapshot.position_ms = 99000; value.owner.attachmentGeneration = 99;
  h.live(capture(80000)); h.reporter.notify(capture(90000));
  assert.equal(h.reporter.capture().snapshot.position_ms, 7000);
  assert.equal(h.reporter.capture().owner.attachmentGeneration, 1);
  assert.equal(h.reporter.pending.snapshot.position_ms, 7000);
  held.resolve(answer(h.sent[0].request)); await flush();
  assert.equal(h.reporter.stopped, true);
  assert.equal(h.sent.length, 1);
});

test("owner changes surface to the final exchange caller without retargeting its commit", async () => {
  const errors = [];
  const h = harness(() => Promise.reject(Object.assign(new Error("owner moved"), {
    status: 409, code: "owner_changed", generation: "44444444-4444-4444-8444-444444444444", controlEpoch: 8,
  })), event => { if (event.error) errors.push(event.error); });
  h.reporter.finish(committed()); await flush();
  assert.equal(errors.length, 1);
  assert.equal(errors[0].code, "owner_changed");
  assert.equal(h.reporter.bootstrap.generation, generation);
  assert.equal(h.reporter.bootstrap.control_epoch, 7);
  assert.equal(h.reporter.stopped, true);
  assert.equal(h.reporter.resetForOwner(errors[0]), false);
  await h.advance(60000);
  assert.equal(h.sent.length, 1);
});

test("accepted commit may synchronously chain an explicit final end and no ordinary sequence", async () => {
  let h;
  h = harness(request => Promise.resolve(answer(request)), event => {
    if (event.response && event.request.acknowledgement?.state === "committed") {
      assert.equal(h.reporter.notify(capture(90000)), null);
      assert.ok(h.reporter.finish(capture(5000, null, "end")));
      h.reporter.notify(capture(99000));
    }
  });
  h.reporter.finish(committed(5000)); await flush();
  assert.equal(h.reporter.stopped, false);
  assert.equal(h.sent.length, 1);
  await h.advance(250);
  assert.deepEqual(h.sent.map(entry => entry.request.demand), ["active", "end"]);
  assert.equal(h.sent[1].request.position_ms, 5000);
  assert.equal(h.sent[1].request.sequence, 2);
  assert.equal(h.reporter.stopped, true);
  await h.advance(60000);
  assert.equal(h.sent.length, 2);
});

test("an accepted final stays stopped if the consumer callback throws", async () => {
  let exchanges = 0;
  const h = harness(request => Promise.resolve(answer(request)), () => {
    exchanges++; throw new Error("observer failed after acceptance");
  });
  h.reporter.finish(committed()); await flush();
  assert.equal(h.reporter.acceptedSequence, 1);
  assert.equal(h.reporter.stopped, true);
  assert.equal(exchanges, 1);
  await h.advance(60000);
  assert.equal(h.sent.length, 1);
});
