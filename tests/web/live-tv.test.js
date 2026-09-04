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

async function main() {
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
    assert.equal(lease.released, undefined);
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
    const nodes = { "live-tv-video": video, "live-tv-player": { hidden: true }, "live-tv-title": {} };
    const state = { channels: [{ id: "one", guide_number: "7.1", guide_name: "Test" }], serial: 0 };
    const lease = new liveTv.Lease({
      start: async () => ({ session_id: "cap" }), release: async () => { releases++; },
      status: async () => { statuses++; return { state: "active" }; }, keepalive: async () => {},
    });
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "Date", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       ${shipped("stopLiveTv")}${shipped("watchLiveTv")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 6; i++) { clock += 10000; await poll(); }
    assert.equal(statuses, 0, "paused status requests also renew server leases");
    assert.equal(releases, 1);
    assert.equal(lease.current, null);
    assert.equal(nodes["live-tv-player"].hidden, true);
  });

  await test("Live TV exits its own fullscreen before hiding, with iPhone entry fallback", async () => {
    const events = [], panel = { hidden: false, contains: () => false };
    const video = { webkitEnterFullscreen: () => events.push("iphone-enter") };
    const document = { getElementById: id => id === "live-tv-player" ? panel : video,
      fullscreenElement: panel, exitFullscreen: () => { events.push("exit"); document.fullscreenElement = null; return Promise.resolve(); } };
    const state = { serial: 0 };
    const control = new Function("document", "LIVE_TV", "LIVE_TV_LEASE", "detachLiveTvMedia",
      `${shipped("exitLiveTvPresentation")}${shipped("stopLiveTv")}${shipped("fullscreenLiveTv")}
       return {stopLiveTv,fullscreenLiveTv};`)(document, state, { stop: async () => events.push("release") }, () => events.push("detach"));
    await control.stopLiveTv();
    assert.deepEqual(events, ["exit", "detach", "release"]);
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
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error}\n`);
  process.exitCode = 1;
});
