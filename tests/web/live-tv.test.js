"use strict";

const assert = require("node:assert/strict");
const liveTv = require("../../crates/plurxd/src/web/live-tv.js");
const fs = require("node:fs");
const path = require("node:path");
const shell = fs.readFileSync(path.join(__dirname, "../../crates/plurxd/src/web/index.html"), "utf8");
function shipped(name) {
  const match = new RegExp(`\\n(?:async )?function ${name}\\(`).exec(shell);
  assert.ok(match, `missing shipped function ${name}`);
  const start = match.index + 1, end = shell.indexOf("\n}", start);
  assert.ok(end > start, `missing closing brace for ${name}`);
  return shell.slice(start, end + 2);
}

function test(name, run) {
  return Promise.resolve()
    .then(run)
    .then(() => process.stdout.write(`PASS ${name}\n`));
}

function deferred() {
  let resolve;
  const promise = new Promise((settle) => { resolve = settle; });
  return { promise, resolve };
}

function memoryStorage() {
  const values = new Map();
  return { get length() { return values.size; }, key: i => [...values.keys()][i],
    getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value),
    removeItem: key => values.delete(key) };
}

async function main() {
  await test("both guide views put the compact player above a full-width player-height guide", () => {
    const list = shipped("liveTvListMarkup"), grid = shipped("liveTvGridMarkup");
    const stage = shipped("liveTvStageMarkup"), now = shipped("liveTvNowBar");
    assert.ok(list.indexOf("liveTvStageMarkup") < list.indexOf('class="lt-list"'));
    assert.ok(grid.indexOf("liveTvStageMarkup") < grid.indexOf('class="lt-gridwrap"'));
    assert.doesNotMatch(grid, /grid-template-columns:minmax\(0,1fr\) 400px/);
    assert.match(shell, /\.lt-stage-top\{[^}]*grid-template-columns:minmax\(0,2fr\) minmax\(240px,1fr\)/s);
    assert.match(stage, /lt-showinfo|liveTvShowInfo/);
    assert.match(stage, /lt-stage\$\{wide\?" wide":""\}/);
    assert.match(now, /Larger/);
    assert.match(now, /Smaller/);
    for(const control of ["pauseLiveTv", "muteLiveTv", "toggleLiveTvPip", "fullscreenLiveTv", "stopLiveTv"])
      assert.match(now, new RegExp(control));
    assert.match(shell, /\.lt-list\{[^}]*overflow:auto[^}]*height:var\(--live-tv-slot-height,64vh\)/s);
    assert.match(shell, /\.lt-gridwrap\{[^}]*height:var\(--live-tv-slot-height,64vh\)/s);
    assert.match(shipped("liveTvTrackSlot"), /--live-tv-slot-height/);
  });

  await test("an explicit stream failure is not rewritten as a lost start response", () => {
    assert.match(shell, /LIVE_TV_DEFINITIVE_REFUSALS=new Set\(\[[^\]]*"stream_failed"/);
    assert.match(shell, /LIVE_TV_DEFINITIVE_REFUSALS=new Set\(\[[^\]]*"codec_unsupported"/);
  });

  await test("mute stays an icon while its accessible action follows player state", () => {
    const video = { muted: false };
    const buttons = [
      { textContent: "🔇", title: "Mute", setAttribute(name, value) { this[name] = value; } },
      { textContent: "🔇", title: "Mute", setAttribute(name, value) { this[name] = value; } },
    ];
    const toggle = new Function("document",
      `${shipped("liveTvSyncMuteButtons")}${shipped("muteLiveTv")} return muteLiveTv;`)(
      { getElementById: id => id === "live-tv-video" ? video : null,
        querySelectorAll: selector => selector === "[data-live-tv-mute]" ? buttons : [] });
    toggle();
    assert.equal(video.muted, true);
    assert.deepEqual(buttons.map(button => [button.textContent, button.title, button["aria-label"]]),
      [["🔊", "Unmute", "Unmute"], ["🔊", "Unmute", "Unmute"]]);
    toggle();
    assert.equal(video.muted, false);
    assert.deepEqual(buttons.map(button => [button.textContent, button.title, button["aria-label"]]),
      [["🔇", "Mute", "Mute"], ["🔇", "Mute", "Mute"]]);
  });

  await test("protected channels are visible but never watchable", () => {
    assert.deepEqual(liveTv.channelView({ drm: true, support: "drm_unsupported" }), {
      disabled: true,
      label: "Protected · unsupported",
      tone: "warn",
    });
    assert.deepEqual(liveTv.channelView({ drm: false, support: "ready" }), {
      disabled: false,
      label: "Available",
      tone: "ready",
    });
  });

  await test("typed terminal failures produce specific recovery", () => {
    assert.deepEqual(liveTv.errorView({ code: "capability_expired" }), {
      code: "capability_expired",
      title: "Live session expired",
      detail: "The player was idle or disconnected. Start the channel again.",
      retryable: true,
    });
    assert.equal(liveTv.errorView({ code: "stream_failed" }).title, "Live stream stopped");
    assert.equal(liveTv.errorView({ code: "owner_unavailable" }).retryable, true);
    assert.equal(liveTv.errorView({ code: "drm_unsupported" }).retryable, false);
    assert.equal(liveTv.errorView({ code: "unknown-server-code" }).title, "Tuner owner unavailable");
  });

  await test("close and channel switch release each capability exactly once", async () => {
    const releases = [];
    const lease = new liveTv.Lease({
      start: async (channel) => ({ session_id: `cap-${channel}`, channel: { id: channel } }),
      release: async (id) => { releases.push(id); },
    });
    assert.equal((await lease.start("7.1")).session_id, "cap-7.1");
    assert.equal((await lease.start("9.1")).session_id, "cap-9.1");
    await Promise.all([lease.stop(), lease.stop()]);
    assert.deepEqual(releases, ["cap-7.1", "cap-9.1"]);
  });

  await test("a superseded late start is released before the next tuner opens", async () => {
    const first = deferred();
    const started = deferred();
    const events = [];
    const lease = new liveTv.Lease({
      start: async (channel) => {
        events.push(`start:${channel}`);
        if (channel === "2.1") {
          started.resolve();
          return first.promise;
        }
        return { session_id: `cap-${channel}` };
      },
      release: async (id) => { events.push(`release:${id}`); },
    });
    const opening = lease.start("2.1");
    await started.promise;
    const switched = lease.start("4.1");
    first.resolve({ session_id: "cap-2.1" });
    assert.equal(await opening, null);
    assert.equal((await switched).session_id, "cap-4.1");
    assert.deepEqual(events, ["start:2.1", "release:cap-2.1", "start:4.1"]);
  });

  await test("status and keepalive cannot commit across a switch", async () => {
    const status = deferred();
    const lease = new liveTv.Lease({
      start: async (channel) => ({ session_id: `cap-${channel}` }),
      release: async () => {},
      status: async () => status.promise,
      keepalive: async () => ({ ok: true }),
    });
    await lease.start("7.1");
    const stale = lease.status();
    const switched = lease.start("8.1");
    status.resolve({ state: "active" });
    assert.equal(await stale, null);
    await switched;
    assert.deepEqual(await lease.keepalive(), { ok: true });
  });

  await test("a status poll never renews a capability that is being released", async () => {
    const release = deferred();
    let statuses = 0;
    const lease = new liveTv.Lease({
      start: async (channel) => ({ session_id: `cap-${channel}` }),
      release: async () => release.promise,
      status: async () => { statuses++; return { state: "active" }; },
      keepalive: async () => ({ ok: true }),
    });
    await lease.start("7.1");
    const closing = lease.stop();
    // `stop()` bumps the generation synchronously but only sets `releasing` two
    // microtasks later. A poll entered in that window is the real hazard, so it
    // is checked BEFORE letting the chain run — waiting first would hide it.
    const racing = lease.status();
    assert.equal(await racing, null, "a poll entered before the DELETE begins must be refused");
    assert.equal(statuses, 0, "a superseded capability must never be renewed");
    await new Promise((settle) => setImmediate(settle));
    assert.equal(lease.releasing, true, "the DELETE must still be in flight");
    assert.equal(await lease.status(), null);
    assert.equal(statuses, 0, "a release in flight must not renew its own lease");
    assert.equal(await lease.keepalive(), null);
    release.resolve();
    await closing;
    assert.equal(lease.current, null);
  });

  await test("failed cleanup retains the capability and blocks another tuner", async () => {
    const events = [];
    let fail = true;
    const lease = new liveTv.Lease({
      start: async (id) => { events.push(`start:${id}`); return { session_id: id }; },
      release: async (id) => { events.push(`release:${id}`); if (fail) throw new Error("offline"); },
    });
    await lease.start("one");
    await assert.rejects(lease.start("two"), /offline/);
    assert.equal(lease.current.session_id, "one");
    assert.deepEqual(events, ["start:one", "release:one"]);
    fail = false;
    await lease.start("three");
    assert.deepEqual(events, ["start:one", "release:one", "release:one", "start:three"]);
    await lease.stop();
    assert.equal(lease.current, null);
  });

  await test("late-start cleanup failure is retained across close", async () => {
    const pending = deferred(), started = deferred();
    let fail = true;
    const lease = new liveTv.Lease({
      start: async () => { started.resolve(); return pending.promise; },
      release: async () => { if (fail) throw new Error("offline"); },
    });
    const opening = lease.start("one");
    await started.promise;
    const closing = lease.stop();
    pending.resolve({ session_id: "one" });
    await assert.rejects(opening, /offline/);
    await assert.rejects(closing, /offline/);
    assert.equal(lease.current.session_id, "one");
    fail = false;
    await lease.stop();
    assert.equal(lease.current, null);
  });

  await test("autoplay rejection releases the tuner within the play deadline", async () => {
    let clock = 1000, poll, releases = 0, statuses = 0;
    const video = { paused: true, canPlayType: () => "maybe", play: async () => { throw new Error("autoplay denied"); } };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    // `support` is a non-optional enum on the wire, so a fixture channel that
    // omits it is not a channel this client can ever receive.
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => { statuses++; return { state: "active" }; }, keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 6; i++) { clock += 10000; await poll(); }
    assert.equal(statuses, 0, "paused status requests also renew server leases");
    assert.equal(releases, 1);
    assert.equal(lease.current, null);
    assert.equal(nodes["live-tv-host"].hidden, true);
  });

  await test("the tuner watchdog, the keepalive and the status poll all survive docking", async () => {
    // The whole point of M3 is that leaving the route keeps the picture. The
    // lease keeps the household's only tuner, so the three things that own it
    // must keep running in exactly that state — and the poll's liveness gate
    // used to read the route and the render generation, both of which change
    // the instant you dock. Nothing here asserts source text: it drives the
    // real poll across a real route change.
    let clock = 1000, poll, keepalives = 0, statuses = 0, releases = 0, frames = 240;
    const video = { paused: false, currentTime: 5, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: frames }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true, dataset: {} }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => { statuses++; return { state: "active" }; },
      keepalive: async () => { keepalives++; },
    });
    const routing = { hash: "#/live-tv", generation: 1 };
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv", "ROUTING",
      `const location=ROUTING, PLAYER=null, API='/api', window={};
       let PAGE_RENDER_GENERATION=ROUTING.generation;
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){} function liveTvSetMode(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv, routing);

    await run(0);
    clock += 10000; frames += 60; await poll();
    assert.equal(keepalives, 1, "the poll runs on the page");

    // Dock: the viewer navigates to another route. Both liveness inputs move.
    routing.hash = "#/";
    for (let i = 0; i < 12; i++) { clock += 10000; frames += 60; await poll(); }
    assert.equal(keepalives, 13, "a docked stream still renews the lease it holds");
    assert.equal(statuses, 13, "and still asks the owner whether the session is alive");

    // And the 30-second no-progress release still fires while docked, which is
    // the difference between a stalled dock and a permanently held tuner.
    for (let i = 0; i < 5; i++) { clock += 10000; await poll(); }
    assert.equal(releases, 1, "a docked stream that stops decoding gives the tuner back");
    assert.equal(lease.current, null);
  });

  await test("navigating away during the start POST still docks and can still be stopped", async () => {
    // Before: `liveTvLeaveRoute` docked only when a lease already existed, and
    // during the ~45 s start there is none — so the session landed with no
    // media, no poll and no Stop control, holding a tuner invisibly.
    let clock = 1000, poll, releases = 0;
    const started = deferred(), grant = deferred();
    const video = { paused: false, currentTime: 5, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: 240 }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true, dataset: {} }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0, starting: null };
    const lease = new liveTv.Lease({
      start: async () => { started.resolve(); return grant.promise; },
      release: async () => { releases++; },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const routing = { hash: "#/live-tv" };
    const modes = [];
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv", "ROUTING", "MODES",
      `const location=ROUTING, PLAYER=null, API='/api', window={};
       let PAGE_RENDER_GENERATION=1;
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvSetMode(mode){ MODES.push(mode); }
       function liveTvHost(){ return document.getElementById("live-tv-host"); }
       function liveTvShowHost(){ MODES.push("slot"); } function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("liveTvLeaveRoute")}${shipped("watchLiveTv")}
       return {watchLiveTv,liveTvLeaveRoute};`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv, routing, modes);

    const watching = run.watchLiveTv(0);
    await started.promise;
    assert.equal(state.starting, 1, "a start in flight is visible to the router");

    // The viewer leaves while the POST is outstanding.
    routing.hash = "#/";
    run.liveTvLeaveRoute();
    assert.deepEqual(modes, ["dock"], "a start in flight docks rather than hiding");

    grant.resolve({ session_id: "cap" });
    await watching;
    assert.equal(lease.current.session_id, "cap");
    assert.ok(poll, "the granted session is wired to a poll it can be released by");
    assert.equal(state.starting, null, "and the flag is cleared once the start settles");

    // The watchdog can now reclaim it, which is what makes the dock safe.
    video.paused = true;
    for (let i = 0; i < 5; i++) { clock += 10000; await poll(); }
    assert.equal(releases, 1);
  });

  await test("starting a channel closes an open film and reports only that film's progress", async () => {
    // The one line on the Live TV path that can touch VOD state is
    // `closePlayer()`, and the browser acceptance never opens a film, so this
    // is where that path is pinned. The shipped `closePlayer` does report
    // progress — for the file being closed, which is the viewer's own position
    // in the film they were watching, not Live TV writing VOD state. Losing it
    // would be the defect; what must not happen is a second close, a close
    // after the tuner is open, or a report for anything else.
    let clock = 1000, poll, closes = 0;
    const progress = [];
    const video = { paused: false, currentTime: 5, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: 240 }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const events = [];
    const lease = new liveTv.Lease({
      start: async (id) => { events.push("start:" + id); return { session_id: "cap" }; },
      release: async () => { events.push("release"); },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval",
      "PlurxLiveTv", "PLAYER", "closePlayer", "reportProgress",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv,
      { fileId: 7 }, () => { closes += 1; progress.push(7); }, (id) => progress.push(id));
    await run(0);
    assert.equal(closes, 1, "an open film must be closed exactly once before a tuner opens");
    assert.deepEqual(progress, [7], "only the closing film's own progress may be reported");
    assert.deepEqual(events, ["start:one"], "and the tuner must still open afterwards");
  });

  await test("a live window that moves backwards never stops a decoding stream", async () => {
    let clock = 1000, poll, releases = 0, frames = 0;
    // Healthy live playback whose position regresses as the window slides.
    const video = { paused: false, currentTime: 40, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: frames }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 6; i++) { clock += 10000; frames += 120; video.currentTime -= 4; await poll(); }
    assert.equal(releases, 0, "decoded frames advanced, so the stream was never stuck");
    assert.notEqual(lease.current, null);
  });

  await test("a frozen picture still expires even while position climbs", async () => {
    let clock = 1000, poll, releases = 0;
    // The mirror image: the timeline advances but nothing is decoded.
    const video = { paused: false, currentTime: 10, canPlayType: () => "maybe", play: async () => {},
      getVideoPlaybackQuality: () => ({ totalVideoFrames: 500 }) };
    const nodes = { "live-tv-video": video, "live-tv-host": { hidden: true }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test", drm: false, support: "ready" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => ({ state: "active" }), keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       function liveTvShowHost(){} function liveTvInPip(){ return false; }
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 5; i++) { clock += 10000; video.currentTime += 10; await poll(); }
    assert.equal(releases, 1, "a frozen decoder must not be renewed by a moving clock");
    assert.equal(lease.current, null);
  });

  await test("a store full of stale markers heals instead of refusing forever", () => {
    let clock = 0, nonce = 0;
    const storage = memoryStorage();
    for (let i = 0; i < 40; i++) storage.setItem("plurx_live_tv_pending_v1:old" + i, "1");
    const barrier = new liveTv.StartBarrier(() => storage, () => clock, () => String(++nonce));
    // The cap still refuses while those markers are inside their safety window.
    assert.throws(() => barrier.sync(), e => e.code === "live_tv_storage_unavailable");
    clock = 90001;
    const marker = barrier.begin();
    assert.equal(typeof marker, "string");
    assert.equal(storage.length, 1, "the expired markers were swept, not kept forever");
  });

  await test("typed lost-owner starts survive clock jumps and route/profile reuse", async () => {
    let monotonic = 0, wall = 1000, posts = 0;
    const block = shell.slice(shell.indexOf("const LIVE_TV="), shell.indexOf("async function liveTvRequest("));
    let nonce = 0;
    const lease = new Function("PlurxLiveTv", "performance", "Date", "localStorage", "crypto", "liveTvRequest",
      `${block} return LIVE_TV_LEASE;`)(liveTv, { now: () => monotonic }, { now: () => wall }, memoryStorage(),
      { getRandomValues: bytes => { bytes.fill(0); bytes[0] = ++nonce; return bytes; } }, async () => {
        posts++; throw { code: "owner_unavailable", status: 503 };
      });
    await assert.rejects(lease.start("one"), e => e.code === "start_outcome_unknown");
    await lease.stop(); // navigation/logout does not discard the app-wide lease
    wall += 120000;
    await assert.rejects(lease.start("two"), e => e.code === "start_outcome_unknown");
    assert.equal(posts, 1);
    wall -= 3600000;
    monotonic = 90001;
    await assert.rejects(lease.start("three"), e => e.code === "start_outcome_unknown");
    assert.equal(posts, 2);
  });

  await test("an unrecognised start failure keeps the durable marker", async () => {
    let posts = 0, nonce = 0;
    const block = shell.slice(shell.indexOf("const LIVE_TV="), shell.indexOf("async function liveTvRequest("));
    const lease = new Function("PlurxLiveTv", "performance", "Date", "localStorage", "crypto", "liveTvRequest",
      `${block} return LIVE_TV_LEASE;`)(liveTv, { now: () => 0 }, { now: () => 0 }, memoryStorage(),
      { getRandomValues: bytes => { bytes.fill(0); bytes[0] = ++nonce; return bytes; } }, async () => {
        // A typed 5xx this client has never seen. The owner may already have
        // opened a tuner, so the marker must survive and block the retry.
        posts++; throw { code: "encoder_spawn_failed", status: 500 };
      });
    await assert.rejects(lease.start("one"), e => e.code === "start_outcome_unknown");
    await assert.rejects(lease.start("two"), e => e.code === "start_outcome_unknown");
    assert.equal(posts, 1, "an unknown failure must not admit a second tuner request");
  });

  await test("a refusal made before allocation costs no safety wait", async () => {
    let posts = 0, nonce = 0;
    const block = shell.slice(shell.indexOf("const LIVE_TV="), shell.indexOf("async function liveTvRequest("));
    const lease = new Function("PlurxLiveTv", "performance", "Date", "localStorage", "crypto", "liveTvRequest",
      `${block} return LIVE_TV_LEASE;`)(liveTv, { now: () => 0 }, { now: () => 0 }, memoryStorage(),
      { getRandomValues: bytes => { bytes.fill(0); bytes[0] = ++nonce; return bytes; } }, async () => {
        posts++; throw { code: "tuner_capacity", status: 503 };
      });
    await assert.rejects(lease.start("one"), e => e.code === "tuner_capacity");
    await assert.rejects(lease.start("two"), e => e.code === "tuner_capacity");
    assert.equal(posts, 2, "capacity is decided before a tuner is opened; do not quarantine it");
  });

  await test("an unfamiliar channel support value is refused, not offered", () => {
    assert.equal(liveTv.channelView({ drm: false, support: "ready" }).disabled, false);
    assert.equal(liveTv.channelView({ drm: true, support: "ready" }).disabled, true);
    assert.equal(liveTv.channelView({ support: "drm_unsupported" }).disabled, true);
    // A lineup that grows a new state must fail closed, not render Watch live.
    assert.equal(liveTv.channelView({ support: "encrypted" }).disabled, true);
    assert.equal(liveTv.channelView({}).disabled, true);
  });

  await test("durable starts block reloads and never clear another request's marker", () => {
    let clock = 0, nonce = 0;
    const storage = memoryStorage(), make = () => new liveTv.StartBarrier(() => storage, () => clock, () => String(++nonce));
    const first = make(), marker = first.begin(); // pending POST already protected
    const reload = make(); reload.sync();
    assert.throws(() => reload.begin(), e => e.code === "start_outcome_unknown");
    clock = 90001;
    const second = reload.begin();
    first.confirm(marker);
    assert.equal(storage.length, 1, "old completion cannot erase another request");
    reload.hold(second); // ambiguous response or failed DELETE rearms its marker
    const reopened = make(); reopened.sync();
    assert.throws(() => reopened.begin(), e => e.code === "start_outcome_unknown");
    clock += 90001;
    const third = reopened.begin(); reopened.confirm(third);
    assert.equal(storage.length, 0);
  });

  await test("blocked persistence refuses allocation before a request can escape", () => {
    const barrier = new liveTv.StartBarrier(() => { throw new Error("storage denied"); }, () => 0, () => "unused");
    assert.throws(() => barrier.begin(), e => e.code === "live_tv_storage_unavailable");
  });

  await test("expiry cannot delete another document's freshly rearmed immutable marker", () => {
    let clock = 0, nonce = 0, hook;
    const storage = memoryStorage(), remove = storage.removeItem;
    storage.removeItem = key => { if (hook) { const run = hook; hook = null; run(); } remove(key); };
    const make = () => new liveTv.StartBarrier(() => storage, () => clock, () => String(++nonce));
    const owner = make(), first = owner.begin(), observer = make(); observer.sync();
    clock = 90001;
    let replacement;
    hook = () => { replacement = owner.hold(first); };
    observer.sync();
    assert.equal(storage.getItem(replacement), "1");
    assert.throws(() => observer.begin(), e => e.code === "start_outcome_unknown");
    assert.equal(storage.getItem(replacement), "1");
  });

  await test("LAN HTTP marker IDs do not require the secure-context randomUUID API", () => {
    const marker = new Function("crypto", `${shipped("liveTvMarkerId")} return liveTvMarkerId();`)({
      getRandomValues: bytes => { bytes.fill(42); return bytes; },
    });
    assert.equal(marker, "2a".repeat(16));
  });

  await test("successful active ownership survives reload until confirmed DELETE", async () => {
    const storage = memoryStorage(); let nonce = 0;
    const block = shell.slice(shell.indexOf("const LIVE_TV="), shell.indexOf("async function liveTvRequest("));
    const create = () => new Function("PlurxLiveTv", "performance", "localStorage", "crypto", "liveTvRequest",
      `${block} return LIVE_TV_LEASE;`)(liveTv, { now: () => 0 }, storage,
      { getRandomValues: bytes => { bytes.fill(0); bytes[0] = ++nonce; return bytes; } },
      async (_path, method) => method === "POST" ? { session_id: "cap", live: true } : null);
    const first = create(); await first.start("one");
    const restarted = create();
    await assert.rejects(restarted.start("two"), e => e.code === "start_outcome_unknown");
    await first.stop();
    await restarted.start("two"); await restarted.stop();
    assert.equal(storage.length, 0);
  });

  await test("Live TV awaits fullscreen exit before hiding, with iPhone entry fallback", async () => {
    const events = [], panel = { hidden: false, contains: () => false };
    const video = { webkitEnterFullscreen: () => events.push("iphone-enter") };
    const exited = deferred(), neverSettles = deferred(), listeners = new Map();
    const document = { getElementById: id => id === "live-tv-host" ? panel : video,
      addEventListener: (name, listener) => listeners.set(name, listener),
      removeEventListener: (name, listener) => { if (listeners.get(name) === listener) listeners.delete(name); },
      fullscreenElement: panel, exitFullscreen: () => {
        events.push("exit"); exited.promise.then(() => {
          document.fullscreenElement = null; listeners.get("fullscreenchange")?.();
        });
        return neverSettles.promise;
      } };
    const state = { serial: 0 };
    const control = new Function("document", "LIVE_TV", "LIVE_TV_LEASE", "detachLiveTvMedia",
      `${shipped("exitLiveTvPresentation")}${shipped("stopLiveTv")}${shipped("fullscreenLiveTv")}
       return {stopLiveTv,fullscreenLiveTv};`)(document, state, { stop: async () => events.push("release") }, () => events.push("detach"));
    const closing = control.stopLiveTv();
    await Promise.resolve();
    assert.deepEqual(events, ["exit", "release"]);
    assert.equal(panel.hidden, false, "the fullscreen subtree must remain mounted until exit settles");
    exited.resolve();
    await closing;
    assert.deepEqual(events, ["exit", "release", "detach"]);
    assert.equal(panel.hidden, true);
    control.fullscreenLiveTv();
    assert.equal(events.at(-1), "iphone-enter");
  });

  await test("theater viewers can discover Live TV without administrator access", () => {
    const nav = new Function("ME", "location", `${shipped("theaterNavItems")} return theaterNavItems;`)(
      { is_admin: false }, { hash: "#/live-tv" });
    const items = nav("live-tv");
    assert.deepEqual(items.find(item => item.tab === "live-tv"), {
      tab: "live-tv", href: "#/live-tv", label: "Live TV", on: true,
    });
    assert.equal(items.some(item => item.tab === "settings"), false);
  });

  await test("settings separate disabled configuration, enable, and exact physical-fence recovery", async () => {
    const writes = [], settings = { live_tv_enabled: false, live_tv_config_generation: 8,
      live_tv_transition_from_owner_node_id: "owner-a", live_tv_transition_drain_before: 6 };
    const nodes = { ltip: { value: " 192.168.4.99 " }, ltowner: { value: "owner-b" },
      ltlimit: { value: "2" }, ltheight: { value: "720" }, ltfenced: { checked: false },
      "live-tv-settings": { classList: { contains: () => false } } };
    const controls = new Function("SETTINGS", "document", "liveTvSettingsWrite", "toast",
      `${shipped("saveLiveTvSettings")}${shipped("setLiveTvEnabled")}${shipped("recoverLiveTvOwner")}
       return {saveLiveTvSettings,setLiveTvEnabled,recoverLiveTvOwner};`)(
      settings, { getElementById: id => nodes[id] }, async body => writes.push(body), () => {});
    await controls.saveLiveTvSettings();
    assert.deepEqual(writes.pop(), { live_tv_config_generation: 8, live_tv_enabled: false,
      live_tv_device_ipv4: "192.168.4.99", live_tv_owner_node_id: "owner-b", live_tv_max_sessions: 2, live_tv_output_height: 720 });
    await controls.setLiveTvEnabled(true);
    assert.deepEqual(writes.pop(), { live_tv_config_generation: 8, live_tv_enabled: true });
    await controls.recoverLiveTvOwner(); assert.equal(writes.length, 0);
    nodes.ltfenced.checked = true; await controls.recoverLiveTvOwner();
    assert.deepEqual(writes.pop(), { live_tv_config_generation: 8, live_tv_fenced_owner: {
      owner_node_id: "owner-a", drain_before_generation: 6, stopped_and_restart_prevented: true } });
    settings.live_tv_enabled = true;
    await controls.saveLiveTvSettings(); await controls.recoverLiveTvOwner();
    assert.equal(writes.length, 0);
  });

  // ---- the shared guide cases, reproduced by the web renderer -------------
  // tests/playback/live-tv-guide-cases.json is the same fixture the Apple and
  // Android suites read. Three reducers, one truth.
  const CASES = JSON.parse(
    fs.readFileSync(path.join(__dirname, "../playback/live-tv-guide-cases.json"), "utf8"),
  );

  await test("programmeAt answers every shared now/next/progress case", () => {
    for (const c of CASES.programme_at) {
      const answer = liveTv.programmeAt(CASES.guide, c.channel, c.now);
      assert.equal(
        answer.now ? answer.now.title : null,
        c.expect.now_title,
        `${c.name}: what is on now`,
      );
      assert.equal(
        answer.next ? answer.next.title : null,
        c.expect.next_title,
        `${c.name}: what is next`,
      );
      if (c.expect.progress === null) {
        assert.equal(answer.progress, null, `${c.name}: progress`);
      } else {
        assert.ok(
          Math.abs(answer.progress - c.expect.progress) < 1e-9,
          `${c.name}: progress ${answer.progress} != ${c.expect.progress}`,
        );
      }
    }
  });

  await test("gridLayout places every cell where the shared cases say, and never overlaps", () => {
    const g = CASES.grid;
    const layout = liveTv.gridLayout(
      CASES.guide, CASES.lineup, g.window, g.now, g.slot_seconds, g.px_per_slot,
    );
    assert.equal(layout.nowX, g.expect.now_line_x);
    assert.equal(layout.totalWidth, g.expect.total_width);
    assert.equal(layout.rows.length, CASES.lineup.length, "a channel never loses its row");
    for (const expected of g.expect.rows) {
      const row = layout.rows.find(r => r.channel.id === expected.channel);
      assert.ok(row, `no row for ${expected.channel}`);
      assert.deepEqual(
        row.cells.map(cell => ({
          title: cell.programme.title,
          left: cell.left,
          width: cell.width,
          airing: cell.airing,
          clipped: cell.clipped,
        })),
        expected.cells,
        `cells for ${expected.channel}`,
      );
      // Geometry the fixture cannot state row by row: cells advance and never
      // overlap, so a grid can never draw two programmes over one another.
      let edge = -Infinity;
      for (const cell of row.cells) {
        assert.ok(cell.left >= edge - 1e-9, `${expected.channel}: cells overlap`);
        assert.ok(cell.width > 0, `${expected.channel}: a zero-width cell`);
        edge = cell.left + cell.width;
      }
    }
    // 68.1 is in the lineup with no guide rows. The row is present and empty:
    // a channel must not disappear because the guide is thin.
    const protectedRow = layout.rows.find(r => r.channel.id === "68.1");
    assert.deepEqual(protectedRow.cells, []);
  });

  await test("the grid clips to its window and reports where the data ends", () => {
    const g = CASES.grid;
    const layout = liveTv.gridLayout(
      CASES.guide, CASES.lineup, g.window, g.now, g.slot_seconds, g.px_per_slot,
    );
    for (const row of layout.rows) {
      for (const cell of row.cells) {
        assert.ok(cell.left >= 0, "a cell starts before the window");
        assert.ok(cell.left + cell.width <= layout.totalWidth + 1e-9, "a cell runs past the window");
      }
    }
    assert.deepEqual(liveTv.gridSlots(g.window, g.slot_seconds).length, 4);
    // 11.1's last row ends inside the window, so the grid can say so.
    assert.equal(typeof liveTv.guideEnds(CASES.guide, "11.1", g.window), "number");
    // 7.1 runs past it, which is the ordinary case and gets no marker.
    assert.equal(liveTv.guideEnds(CASES.guide, "7.1", g.window), null);
  });

  await test("filterChannels answers every shared filtering case", () => {
    for (const c of CASES.filters) {
      const visible = liveTv.filterChannels(
        CASES.lineup,
        CASES.guide,
        { query: c.opts.query, filter: c.opts.filter, hideProtected: c.opts.hide_protected },
        CASES.grid.now,
      );
      assert.deepEqual(visible.map(channel => channel.id), c.expect, c.name);
    }
  });

  await test("adjacentChannel wraps in both directions and never lands nowhere", () => {
    for (const c of CASES.adjacent) {
      assert.equal(liveTv.adjacentChannel(c.visible, c.current, c.delta), c.expect, c.name);
    }
  });

  await test("Activity attributes a tuner to a channel, a viewer, a node and what is on", () => {
    // Anything on this server that uses a tuner and an encoder has to be
    // attributable from inside the product. The rows have always been in
    // /activity/detail; the page counted them in its summary and drew none of
    // them, so "2 active Live TV sessions" was the whole answer.
    const rows = new Function("esc",
      `${shipped("liveTvActivityRows")} return liveTvActivityRows;`)(value => String(value));
    assert.equal(rows(null, {}), "", "no sessions draws no section");
    assert.equal(rows([], {}), "");
    const html = rows([
      { channel_number: "7.1", channel_name: "WABC", user: "paul", owner_node_id: "node-b",
        encoder: "software", output_height: 720, age_seconds: 372, state: "active",
        programme_title: "City Beat" },
      { channel_number: "4.1", channel_name: "WNBC", user: "guest", owner_node_id: "node-b",
        encoder: "vaapi", output_height: 1080, age_seconds: 30, state: "starting" },
    ], { "node-b": "nynuc" });
    assert.match(html, /Live television/);
    assert.match(html, /City Beat/);
    assert.match(html, /nynuc/, "the roster name, not the raw node id");
    assert.doesNotMatch(html, /node-b/, "a node with a known name never shows its id");
    assert.match(html, /6 min · active/);
    // A row the guide cannot describe is still a row: the session is real
    // whether or not anything knows what is on it.
    assert.match(html, /WNBC/);
    assert.match(html, /—/);
  });

  await test("a guide that is off, empty or erroring is a rendered state, not a failure", () => {
    const empty = { source: "off", freshness: "unavailable", channels: [] };
    assert.deepEqual(liveTv.programmeAt(empty, "7.1", CASES.grid.now), {
      now: null, next: null, progress: null,
    });
    assert.deepEqual(liveTv.programmeAt(null, "7.1", 0), { now: null, next: null, progress: null });
    // Every channel still renders, with a row and no cells.
    const layout = liveTv.gridLayout(empty, CASES.lineup, CASES.grid.window, CASES.grid.now, 1800, 240);
    assert.equal(layout.rows.length, CASES.lineup.length);
    assert.ok(layout.rows.every(row => row.cells.length === 0));
    // And filtering still works without a guide behind it.
    assert.deepEqual(
      liveTv.filterChannels(CASES.lineup, empty, { query: "wabc" }, 0).map(c => c.id),
      ["7.1"],
    );
    // The guide has its own error copy, or every guide failure would read
    // "Tuner owner unavailable".
    const view = liveTv.errorView({ code: "guide_unavailable" });
    assert.equal(view.title, "No programme guide yet");
    assert.match(view.detail, /Channels still play/);
  });
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
