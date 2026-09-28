"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const SOURCE = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/core/errors.js"),
  "utf8",
);

function fixture({readyState = "complete", fetchImpl = null} = {}) {
  let now = 0;
  const requests = [];
  const listeners = new Map();
  const timers = [];
  const nodes = new Map();
  const body = {
    appendChild(node) {
      nodes.set(node.id, node);
      node.remove = () => nodes.delete(node.id);
    },
  };
  const document = {
    readyState,
    documentElement: {dataset: {}},
    body,
    getElementById: (id) => nodes.get(id) || null,
    createElement: (tag) => ({
      tagName: tag.toUpperCase(),
      children: [],
      textContent: "",
      setAttribute(name, value) { this[name] = value; },
      addEventListener(name, callback) { this[`on${name}`] = callback; },
      appendChild(child) { this.children.push(child); },
    }),
  };
  const context = {
    document,
    navigator: {userAgent: "Mozilla/5.0 Chrome/140 Safari/537.36"},
    location: {reload() {}},
    performance: {now: () => now},
    setTimeout: (callback, delay) => { timers.push({callback, delay}); return timers.length; },
    clearTimeout: (id) => { if (timers[id - 1]) timers[id - 1].cancelled = true; },
    AbortController,
    fetch: (...args) => {
      requests.push(args);
      return fetchImpl ? fetchImpl(...args) : Promise.resolve({ok: true});
    },
    console,
  };
  context.window = context;
  context.addEventListener = (name, callback, options) =>
    listeners.set(name, {callback, options});
  vm.runInNewContext(SOURCE, context, {filename: "core/errors.js"});
  return {
    context,
    document,
    requests,
    timers,
    reporter: context.PlurxErrorReporter,
    dispatch(name, event) { listeners.get(name).callback(event); },
    listener(name) { return listeners.get(name); },
    advance(ms) { now += ms; },
  };
}

async function drainMicrotasks() {
  // VM/host promise adoption needs multiple jobs for each serialized report.
  // Yield a complete event-loop turn instead of assuming a fixed job count.
  await new Promise((resolve) => setImmediate(resolve));
}

test("early errors queue, redact, cap, deduplicate and drain on authentication", async () => {
  const f = fixture();
  const error = new Error(`Bearer secret https://host/app.js?token=secret ${"x".repeat(3_000)}`);
  error.stack = `https://host/app.js?token=secret\n${"s".repeat(3_000)}`;
  const event = {
    message: error.message,
    filename: "/assets/core/cards.js?v=secret",
    lineno: 17,
    colno: 9,
    error,
  };
  f.dispatch("error", event);
  assert.equal(f.requests.length, 0, "reports before TOKEN must remain queued");
  f.reporter.setAuth("opaque-token", "Chrome");
  assert.equal(f.requests.length, 1);
  const body = JSON.parse(f.requests[0][1].body);
  assert.equal(body.ua, "Chrome");
  assert.equal(body.src, "/assets/core/cards.js");
  assert.equal(body.line, 17);
  assert.equal(body.col, 9);
  assert.ok(body.message.length <= 512);
  assert.ok(body.stack.length <= 2_048);
  assert.doesNotMatch(JSON.stringify(body), /secret|\?token=/);
  assert.equal(body.title, undefined);
  assert.equal(body.file_id, undefined);

  f.dispatch("error", event);
  assert.equal(f.requests.length, 1, "the same message/source/line is sent once");

  for (let index = 0; index < 19; index += 1) {
    f.dispatch("error", {message: `unique-${index}`, filename: `/u${index}.js`, lineno: index + 1});
  }
  await drainMicrotasks();
  assert.equal(f.requests.length, 20);
  f.dispatch("error", {message: "over-cap", filename: "/over.js", lineno: 99});
  assert.equal(f.requests.length, 20);
  f.advance(10_000);
  f.dispatch("error", {message: "paced", filename: "/paced.js", lineno: 100});
  assert.equal(f.requests.length, 21, "after the burst, only the 10 s paced slot opens");
});

test("a logger failure cannot recursively report itself", () => {
  let f;
  f = fixture({fetchImpl: () => {
    f.dispatch("error", {message: "logger failed", filename: "/core/errors.js", lineno: 1});
    throw new Error("fetch failed");
  }});
  f.reporter.setAuth("opaque-token", "Chrome");
  f.dispatch("unhandledrejection", {reason: new Error("outer")});
  assert.equal(f.requests.length, 1, "the nested logger error is dropped by the guard");
});

test("the boot sentinel distinguishes slow, crashed and timed-out bootstrap", async () => {
  const slow = fixture({readyState: "loading"});
  assert.equal(slow.reporter.checkBoot(5_000), "slow");
  assert.equal(slow.document.getElementById("boot-sentinel"), null);
  assert.equal(slow.reporter.checkBoot(20_000), "slow");
  assert.match(slow.document.getElementById("boot-sentinel").textContent, /Still loading \(20s\)/);

  const timedOut = fixture({readyState: "complete"});
  assert.equal(timedOut.reporter.checkBoot(5_000), "waiting");
  assert.equal(timedOut.document.getElementById("boot-sentinel"), null);
  assert.equal(timedOut.reporter.checkBoot(20_000), "timed_out");
  const timeoutBanner = timedOut.document.getElementById("boot-sentinel");
  assert.match(timeoutBanner.textContent, /did not finish starting after 20s/);
  assert.equal(timeoutBanner.children[0].textContent, "Reload");

  const crashed = fixture();
  assert.equal(crashed.listener("error").options, true,
    "resource failures are observable only from the capture phase");
  crashed.dispatch("error", {
    target: {src: "/assets/core/cards.js?v=one"},
  });
  crashed.reporter.setAuth("opaque-token", "Chrome");
  assert.equal(crashed.reporter.checkBoot(5_000), "crashed");
  assert.match(crashed.document.getElementById("boot-sentinel").textContent, /core\/cards\.js/);
  await drainMicrotasks();
  const reports = crashed.requests.map(([, options]) => JSON.parse(options.body));
  assert.equal(reports[0].src, "/assets/core/cards.js");
  assert.equal(reports[0].message, "resource failed to load");
  const events = reports.map((body) => body.event);
  assert.deepEqual(events, ["client_error", "boot_sentinel"]);

  crashed.reporter.markReady();
  assert.equal(crashed.document.documentElement.dataset.boot, "ready");
  assert.equal(crashed.document.getElementById("boot-sentinel"), null);
});

test("report delivery waits for completion so an early error precedes its boot sentinel", async () => {
  const pending = [];
  const f = fixture({fetchImpl: () => new Promise((resolve, reject) => pending.push({resolve, reject}))});
  f.dispatch("error", {target: {src: "/assets/core/cards.js"}});
  f.reporter.setAuth("opaque-token", "Chrome");
  assert.equal(f.reporter.checkBoot(5_000), "crashed", "the banner does not wait for transport");
  assert.equal(f.requests.length, 1, "the sentinel must not race the held first response");
  const completedDeadline = f.timers.at(-1);
  pending[0].resolve({ok: true});
  await drainMicrotasks();
  assert.equal(completedDeadline.cancelled, true, "completion removes the owned deadline");
  completedDeadline.callback();
  assert.equal(f.requests.length, 2, "a stale deadline cannot release the active sentinel request");
  assert.deepEqual(f.requests.map(([, options]) => JSON.parse(options.body).event),
    ["client_error", "boot_sentinel"]);
  f.dispatch("error", {message: "later", filename: "/later.js"});
  pending[1].reject(new Error("network unavailable"));
  await drainMicrotasks();
  assert.equal(f.requests.length, 3, "a rejected request must release the next report");
});

test("report deadlines release a stuck request and fence late completions and account changes", async () => {
  const pending = [];
  const f = fixture({fetchImpl: () => new Promise((resolve) => pending.push({resolve}))});
  f.reporter.setAuth("account-one", "Chrome");
  f.dispatch("error", {message: "first", filename: "/first.js"});
  f.dispatch("error", {message: "second", filename: "/second.js"});
  const firstSignal = f.requests[0][1].signal;
  const firstDeadline = f.timers.at(-1);
  assert.equal(firstDeadline.delay, 5_000);
  firstDeadline.callback();
  assert.equal(firstSignal.aborted, true);
  assert.equal(f.requests.length, 2, "one deadline dispatches the next report");
  f.dispatch("error", {message: "third", filename: "/third.js"});
  pending[0].resolve({ok: true});
  await drainMicrotasks();
  assert.equal(f.requests.length, 2, "the timed-out completion cannot release the active second request");
  const secondSignal = f.requests[1][1].signal;
  const signedOutDeadline = f.timers.at(-1);
  f.reporter.setAuth(null);
  assert.equal(signedOutDeadline.cancelled, true, "sign-out removes the active deadline");
  assert.equal(secondSignal.aborted, true, "sign-out cancels the active authenticated request");
  f.reporter.setAuth("account-two", "Chrome");
  pending[1].resolve({ok: true});
  await drainMicrotasks();
  assert.equal(f.requests.length, 2, "old account's queued report is discarded");
  f.dispatch("error", {message: "new-account", filename: "/new.js"});
  assert.equal(f.requests[2][1].headers.authorization, "Bearer account-two");
  assert.equal(JSON.parse(f.requests[2][1].body).message, "new-account");
});
