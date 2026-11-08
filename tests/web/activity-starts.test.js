"use strict";

// Activity → Starts, against the shipped scripts.
//
// What this file protects: a start is one row per media-session create, and
// two devices starting one title through two different ingresses are two
// rows. The page is also the only place a missing observation can show up —
// a node that did not answer, a client that never reported — so those are
// asserted as rendered text, not as fields in a response.
//
// The painter under test is the shipped pages/activity-starts.js, evaluated
// whole; its neighbours (esc, nodeLabel, api, the tab bar) are the shipped
// functions too, so a rename fails here instead of testing nothing.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {shellSource, WEB} = require("./shell-source.js");
const SHIPPED_UI = shellSource().bodyScript;

const DECLARATIONS = ["\nfunction ", "\nasync function "];
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) => SHIPPED_UI.indexOf(`${kind}${name}(`)).find((at) => at !== -1);
  assert.notEqual(start, undefined, `the shell no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  return (ends.length ? rest.slice(0, Math.min(...ends)) : rest).trimEnd();
}

const STARTS_SOURCE = fs.readFileSync(path.join(WEB, "pages/activity-starts.js"), "utf8");
// The tab names live beside the tab functions in activity.js; read them from
// there rather than restating them, so a fourth tab added without its pane
// fails here.
const TAB_CONSTANTS = ["ACTIVITY_TABS", "ACTIVITY_TAB_LABELS"].map((name) => {
  const line = fs.readFileSync(path.join(WEB, "pages/activity.js"), "utf8").split("\n").find((text) => text.startsWith(`const ${name}=`));
  assert.ok(line, `activity.js no longer declares ${name}`);
  return line;
}).join("\n");
const TAB_SOURCE = ["activityTabNeighbor", "activityTabsHtml", "setActivityTab"].map(shippedSource).join("\n");

function harness(overrides = {}) {
  const calls = [];
  const elements = new Map();
  const document = {
    getElementById: (id) => {
      if (!elements.has(id)) elements.set(id, {innerHTML: "", hidden: false, attrs: {}, setAttribute(k, v) { this.attrs[k] = v; }});
      return elements.get(id);
    },
  };
  const context = {
    ME: {is_admin: true},
    refreshDurableActivity() {},
    location: {hash: "#/activity"},
    document,
    calls,
    api: async (route, options) => {
      calls.push({route, options});
      return context.nextPage || {rows: [], nodes: []};
    },
    nextPage: null,
    ...overrides,
  };
  const names = Object.keys(context);
  const body = "\"use strict\";" +
    "const ACTIVITY_VIEW={tab:'status'};" +
    ["esc", "nodeLabel"].map(shippedSource).join("\n") + "\n" +
    TAB_CONSTANTS + "\n" + TAB_SOURCE + "\n" + STARTS_SOURCE +
    "\nreturn {ACTIVITY_STARTS, ACTIVITY_VIEW, activityStartsHtml, activityStartsPaneHtml, refreshActivityStarts, runActivityStartsAction, pageActivityStarts, previousActivityStarts, activityTabsHtml, setActivityTab, activityTabNeighbor};";
  return {api: new Function(...names, body)(...names.map((name) => context[name])), context, document};
}

const NODE_A = "5deeeebc-8f39-4cb5-8e4a-aa5f912f327f";
const NODE_B = "9a1c77e2-0000-4000-8000-aa5f912f327f";
const NODE_C = "c0ffee00-0000-4000-8000-aa5f912f327f";

function row(overrides) {
  return {
    attempt: "claim-a", at_unix_ms: 1_700_000_000_000, node_id: NODE_A, user_id: 1, user: "paul",
    file_id: 9, title: "Avatar", client_build: "web/Chrome", route: "rolling", refusal: null,
    refusal_message: null, retryable: false, phases: [{name: "plan", ms: 12, authority_reads: 1, local_reads: 2, mutations: 0}],
    owner_node: NODE_B, elapsed_ms: 40, authority_reads: 1, local_reads: 2, mutations: 0, store_ms: 3,
    ttff_ms: 1500, outcome: "presented", failure: null,
    index: {status: "indexed", refusal: null, identities: 2, present: 2, action: {kind: "none"}},
    ...overrides,
  };
}

let failures = 0;
async function test(name, run) {
  try {
    await run();
    process.stdout.write(`PASS ${name}\n`);
  } catch (error) {
    failures += 1;
    process.stdout.write(`FAIL ${name}\n${error && error.stack}\n`);
  }
}

(async () => {
  await test("two devices on one title through different ingresses are two rows", () => {
    const {api} = harness();
    api.ACTIVITY_STARTS.rows = [
      row({attempt: "claim-b", node_id: NODE_B, client_build: "apple/86", ttff_ms: 4200, at_unix_ms: 1_700_000_005_000}),
      row({attempt: "claim-a", node_id: NODE_A}),
    ];
    api.ACTIVITY_STARTS.observed = 1;
    const html = api.activityStartsHtml(api.ACTIVITY_STARTS, {[NODE_A]: "lab1", [NODE_B]: "lab2"});
    assert.equal((html.match(/data-starts-attempt=/g) || []).length, 2);
    assert.match(html, /data-starts-attempt="claim-a"/);
    assert.match(html, /data-starts-attempt="claim-b"/);
    assert.match(html, /web\/Chrome/);
    assert.match(html, /apple\/86/);
    assert.match(html, /via lab1/, "the ingress that handled the create is named");
    assert.match(html, /via lab2/);
    assert.match(html, /1\.5 s/);
    assert.match(html, /4\.2 s/);
    assert.doesNotMatch(html, /undefined|\[object/);
  });

  await test("a slow, a failed and a cross-node start are each explained on the page", () => {
    const {api} = harness();
    api.ACTIVITY_STARTS.rows = [
      row({attempt: "slow", ttff_ms: 31_000, elapsed_ms: 9200, phases: [{name: "plan", ms: 12, authority_reads: 1, local_reads: 2, mutations: 0}, {name: "worker", ms: 9100, authority_reads: 4, local_reads: 0, mutations: 3}], index: {status: "pending", refusal: null, identities: 2, present: 0, action: {kind: "enqueue", method: "POST", path: "/api/v1/files/9/analysis", body: {components: ["fragment_index"]}}}}),
      row({attempt: "failed", outcome: "failed", failure: "hls_fatal", ttff_ms: null, index: {status: "refused", refusal: "unsupported_codec: no keyframes", identities: 2, present: 0, action: {kind: "retry", request_id: "req-7", state: "failed", method: "POST", path: "/api/v1/analysis/jobs/req-7/retry"}}}),
      row({attempt: "cross", node_id: NODE_B, owner_node: NODE_C, outcome: "refused", refusal: "transcode_capacity_pending", refusal_message: "all encoders busy", retryable: true, ttff_ms: null, index: {status: "pending", action: {kind: "joining", request_id: "req-8", state: "running"}}}),
    ];
    api.ACTIVITY_STARTS.observed = 1;
    const html = api.activityStartsHtml(api.ACTIVITY_STARTS, {[NODE_A]: "lab1", [NODE_B]: "lab2", [NODE_C]: "lab3"});
    assert.match(html, /31 s/, "time to first frame");
    assert.match(html, /worker<\/b> 9\.1 s/, "the phase the time went in");
    assert.match(html, /Where the time went · 9\.2 s/);
    assert.match(html, /hls_fatal/, "the failure the client reported");
    assert.match(html, /transcode_capacity_pending[\s\S]*retried by the client[\s\S]*all encoders busy/);
    assert.match(html, /lab3/, "the owner node of the cross-node start");
    assert.match(html, /via lab2/, "and the node that took the request");
    // The three index states, each with the one action that fits.
    assert.match(html, /data-starts-action="0"[^>]*>Index now</);
    assert.match(html, /data-starts-action="1"[^>]*>Index now \(retry the failed job\)</);
    assert.match(html, /Indexing already in progress/);
    assert.doesNotMatch(html, /data-starts-action="2"/, "joining an active job offers no second one");
    assert.match(html, /unsupported_codec: no keyframes/);
  });

  await test("a node that did not answer, and a start with no client report, are said out loud", () => {
    const {api} = harness();
    api.ACTIVITY_STARTS.rows = [row({outcome: "unobserved", ttff_ms: null}), row({attempt: "w", outcome: "pending", ttff_ms: null})];
    api.ACTIVITY_STARTS.nodes = [
      {node_id: NODE_A, status: "answered", rows: 2},
      {node_id: NODE_C, status: "unavailable", reason: "timed_out"},
    ];
    api.ACTIVITY_STARTS.observation = {attempts: 2, presented: 0, failed: 0, refused: 0, pending: 1, unobserved: 1, unavailable_nodes: 1};
    api.ACTIVITY_STARTS.observed = 1;
    const html = api.activityStartsHtml(api.ACTIVITY_STARTS, {[NODE_C]: "lab3"});
    assert.match(html, /Starts are incomplete[\s\S]*lab3 timed out/);
    assert.match(html, /1<\/b> nodes did not answer/);
    assert.match(html, /1<\/b> with no client report/);
    assert.match(html, /missing observation/);
    api.ACTIVITY_STARTS.directoryUnavailable = true;
    assert.match(api.activityStartsHtml(api.ACTIVITY_STARTS, {}), /cluster directory could not be read/);
  });

  await test("everything from the wire is escaped", () => {
    const {api} = harness();
    api.ACTIVITY_STARTS.rows = [row({title: "<img src=x onerror=alert(1)>", client_build: "<b>web</b>", user: "\"><script>", refusal: "<x>", outcome: "refused", refusal_message: "<y>"})];
    api.ACTIVITY_STARTS.observed = 1;
    const html = api.activityStartsHtml(api.ACTIVITY_STARTS, {});
    assert.doesNotMatch(html, /<img |<script>|<b>web|<x>|<y>/);
    assert.match(html, /&lt;img src=x/);
  });

  await test("the Starts tab is in the bar, admin-only, and reachable by arrow keys", () => {
    const {api} = harness();
    const html = api.activityTabsHtml();
    assert.match(html, /id="activity-tab-starts"[^>]*aria-controls="activity-starts"[^>]*>Starts</);
    assert.equal(api.activityTabNeighbor("starts", 1), "status");
    assert.equal(api.activityTabNeighbor("status", -1), "starts");
    // A non-admin gets no bar and no pane.
    const guest = harness({ME: {is_admin: false}});
    assert.equal(guest.api.activityTabsHtml(), "");
    assert.equal(guest.api.activityStartsPaneHtml({}), "");
  });

  await test("selecting the tab shows its pane and reads the page", async () => {
    const h = harness();
    h.context.nextPage = {rows: [row({})], nodes: [{node_id: NODE_A, status: "answered", rows: 1}], next_before: null, node_hostnames: {[NODE_A]: "lab1"}, observation: {attempts: 1}};
    h.api.setActivityTab("starts");
    assert.equal(h.api.ACTIVITY_VIEW.tab, "starts");
    assert.equal(h.document.getElementById("activity-starts").hidden, false);
    assert.equal(h.document.getElementById("activity-status").hidden, true);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(h.context.calls.length, 1);
    assert.equal(h.context.calls[0].route, "/activity/starts?limit=25");
    assert.match(h.document.getElementById("activity-starts-body").innerHTML, /data-starts-attempt="claim-a"/);
    assert.match(h.document.getElementById("activity-starts-body").innerHTML, /via lab1/);
  });

  await test("paging asks for strictly older starts and Newer returns", async () => {
    const h = harness();
    h.context.nextPage = {rows: [row({})], nodes: [], next_before: 1_699_999_000_000};
    await h.api.refreshActivityStarts(true);
    assert.equal(h.api.ACTIVITY_STARTS.next, 1_699_999_000_000);
    await h.api.pageActivityStarts(h.api.ACTIVITY_STARTS.next);
    assert.equal(h.context.calls[1].route, "/activity/starts?limit=25&before=1699999000000");
    assert.deepEqual(h.api.ACTIVITY_STARTS.history, [null]);
    await h.api.previousActivityStarts();
    assert.equal(h.context.calls[2].route, "/activity/starts?limit=25");
  });

  await test("Index now sends the server's own offered request, and only that", async () => {
    const h = harness();
    const offered = [
      row({index: {status: "pending", action: {kind: "enqueue", method: "POST", path: "/api/v1/files/9/analysis", body: {components: ["fragment_index"]}}}}),
      row({attempt: "b", index: {status: "refused", action: {kind: "retry", method: "POST", path: "/api/v1/analysis/jobs/req-7/retry"}}}),
      row({attempt: "c", index: {status: "pending", action: {kind: "enqueue", method: "POST", path: "https://elsewhere.example/steal", body: {}}}}),
    ];
    // Each action refreshes the page afterwards; the page is offered afresh.
    h.context.nextPage = {rows: [], nodes: []};
    for (let i = 0; i < offered.length; i += 1) {
      h.api.ACTIVITY_STARTS.rows = offered;
      await h.api.runActivityStartsAction(i);
    }
    const posts = h.context.calls.filter((call) => call.options && call.options.method === "POST");
    assert.deepEqual(posts.map((call) => call.route), ["/files/9/analysis", "/analysis/jobs/req-7/retry"]);
    assert.deepEqual(posts[0].options.body, {components: ["fragment_index"]});
  });

  await test("a refused request is shown, not swallowed", async () => {
    const h = harness({api: async () => { throw new Error("analysis queue is disabled"); }});
    h.api.ACTIVITY_STARTS.rows = [row({index: {status: "pending", action: {kind: "enqueue", method: "POST", path: "/api/v1/files/9/analysis", body: {}}}})];
    await h.api.runActivityStartsAction(0);
    assert.match(h.api.activityStartsHtml(h.api.ACTIVITY_STARTS, {}), /Could not request indexing: analysis queue is disabled/);
  });

  if (failures) {
    process.stdout.write(`${failures} failed\n`);
    process.exit(1);
  }
  process.stdout.write("PASS activity-starts\n");
})();
