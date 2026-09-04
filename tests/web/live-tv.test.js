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
    const run = new Function("LIVE_TV", "LIVE_TV_LEASE", "document", "performance", "setInterval", "PlurxLiveTv",
      `const location={hash:'#/live-tv'}, PAGE_RENDER_GENERATION=1, PLAYER=null, API='/api', window={};
       function detachLiveTvMedia(){} function liveTvMessage(){} function liveTvFailure(){}
       function exitLiveTvPresentation(){}
       ${shipped("liveTvNow")}${shipped("stopLiveTv")}${shipped("watchLiveTv")} return watchLiveTv;`)(
      state, lease, { getElementById: id => nodes[id], visibilityState: "visible" },
      { now: () => clock }, fn => { poll = fn; return null; }, liveTv);
    await run(0);
    for (let i = 0; i < 6; i++) { clock += 10000; await poll(); }
    assert.equal(statuses, 0, "paused status requests also renew server leases");
    assert.equal(releases, 1);
    assert.equal(lease.current, null);
    assert.equal(nodes["live-tv-player"].hidden, true);
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
