"use strict";

// The Now playing table's Node column, tested against the shipped index.html.
//
// What this file is protecting: a node id is stable and names nothing. An
// operator looking at "5deeeebc-8f39-4cb5-8e4a-aa5f912f327f" cannot tell which
// machine is serving the stream, which is the only question the column exists
// to answer. The server sends the roster's id -> short-hostname map; these
// assertions read the rendered cell, because "the response contains a hostname"
// is true of a page that never prints it.
//
// The assertions run the shipped painter, not the helper underneath it: a test
// that only calls activityNodeCell() leaves the table free to keep printing
// de.node_id, which is exactly the defect.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const SHIPPED_UI = fs.readFileSync(INDEX, "utf8");

// Same extraction contract as tests/web/cluster-membership.test.js: every
// function borrowed here is declared at column zero in one inline <script>, so
// the next top-level declaration terminates it. A rename fails loudly rather
// than silently testing nothing.
const DECLARATIONS = ["\nfunction ", "\nasync function "];
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) =>
    SHIPPED_UI.indexOf(`${kind}${name}(`),
  ).find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter(
    (at) => at !== -1,
  );
  const end = ends.length ? Math.min(...ends) : -1;
  return (end === -1 ? rest : rest.slice(0, end)).trimEnd();
}

// Borrowed rather than stubbed, so the real escaping and the real "5m ago"
// formatting stay under test.
const BORROWED = [
  "esc",
  "fmtAgo",
  "activityNodeFailures",
  "activityNodeStatusText",
  "nodeLabel",
  "activityNodeCell",
  "activityNodeFailureText",
  "detailActivitySummary",
  "clockFromSec",
  "fmtBytes",
  "fmtMbps",
  "activityMethodLabel",
  "activityStreamState",
  "activityStreamMeters",
  "activityStreamDetails",
  "activityStreamCell",
  "paintActivityBody",
];

// The painter's neighbours on the page. None of them decide anything this file
// asserts, so they are stubs — but `main` is real enough to read back.
const PRELUDE = `
  let PAINTED = null;
  // The painter reads the open disclosures back out of #main before it
  // repaints; a string-backed stand-in has none unless a test plants some.
  const main = { innerHTML: "", openStreams: [], querySelectorAll(){ return main.openStreams.map((key) => ({ dataset: { stream: key } })); } };
  const document = { getElementById: (id) => (id === "main" ? main : null) };
  const ME = { is_admin: true };
  function paintActivity(acts){ PAINTED = acts; }
  function analysisSummaryCard(){ return "<div class=\\"analysis\\"></div>"; }
  function statusText(){ return "idle"; }
`;

const painter = new Function(
  "return (function(){" +
    PRELUDE +
    BORROWED.map(shippedSource).join("\n") +
    "\nreturn {paintActivityBody, main, nodeLabel};})()",
)();

const NODE_A = "5deeeebc-8f39-4cb5-8e4a-aa5f912f327f";
const NODE_B = "9a1c77e2-0000-4000-8000-aa5f912f327f";

function snapshot(overrides) {
  return Object.assign(
    {
      deliveries: [
        {
          method: "hls-copy",
          presentation: "live-recovery",
          user: "pjunod",
          file_id: 1,
          item_id: 7,
          title: "Tom Segura: Disgraceful",
          started_unix: Math.floor(Date.now() / 1000) - 300,
          idle_seconds: 0,
          node_id: NODE_A,
        },
      ],
      scans: [],
      offline: [],
      trakt: {},
      producing: null,
    },
    overrides,
  );
}

function paint(d) {
  painter.paintActivityBody(d);
  return painter.main.innerHTML;
}

let started = 0;
let finished = 0;
let failures = 0;
async function test(name, run) {
  started += 1;
  try {
    await run();
    process.stdout.write(`PASS ${name}\n`);
  } catch (error) {
    failures += 1;
    process.stdout.write(`FAIL ${name}\n${error && error.stack}\n`);
  }
  finished += 1;
}

test("the Node cell leads with the machine name and keeps the id under it", () => {
  const html = paint(snapshot({ node_hostnames: { [NODE_A]: "nuc3" } }));
  assert.match(
    html,
    /<td><div class="nodename">nuc3<\/div><div class="clid">5deeeebc-8f39-4cb5-8e4a-aa5f912f327f<\/div><\/td>/,
  );
  // The defect this file exists for: the id must not be the whole answer.
  assert.doesNotMatch(html, /<td><span class="clid">5deeeebc/);
  // …and it must not be the *only* answer either. A tooltip would take the id
  // away from touch, from selection, and from assistive technology.
  assert.doesNotMatch(html, /title="Node ID/);
});

test("without a roster map the cell still shows the id it always showed", () => {
  const html = paint(snapshot());
  assert.match(html, /<span class="clid">5deeeebc-8f39-4cb5-8e4a-aa5f912f327f<\/span>/);
  assert.doesNotMatch(html, /undefined/);
});

test("a node the roster could not name keeps its id while its neighbours are named", () => {
  const d = snapshot({ node_hostnames: { [NODE_A]: "nuc3" } });
  d.deliveries.push(
    Object.assign({}, d.deliveries[0], { node_id: NODE_B, file_id: 2, user: "guest" }),
  );
  const html = paint(d);
  assert.match(html, /<div class="nodename">nuc3<\/div>/);
  assert.match(html, /<td><span class="clid">9a1c77e2-0000-4000-8000-aa5f912f327f<\/span><\/td>/);
});

test("the Node column still appears on ids alone, and disappears without them", () => {
  const withIds = paint(snapshot());
  assert.match(withIds, /<th>Node<\/th>/);
  const d = snapshot();
  delete d.deliveries[0].node_id;
  assert.doesNotMatch(paint(d), /<th>Node<\/th>/);
});

test("a row with no node id at all is Unknown, not blank and not undefined", () => {
  const d = snapshot({ node_hostnames: { [NODE_A]: "nuc3" } });
  d.deliveries.push(
    Object.assign({}, d.deliveries[0], { node_id: "", file_id: 3, user: "guest" }),
  );
  const html = paint(d);
  assert.match(html, /<span class="clid">Unknown<\/span>/);
  assert.doesNotMatch(html, /undefined/);
});

test("the incomplete-activity banner names the node it is complaining about", () => {
  const html = paint(
    snapshot({
      node_hostnames: { [NODE_B]: "m6" },
      activity_nodes: [
        { node_id: NODE_A, status: "answered" },
        { node_id: NODE_B, status: "unreachable" },
      ],
    }),
  );
  assert.match(html, /Node m6 · unreachable/);
  assert.doesNotMatch(html, /Node 9a1c77e2/);
});

test("an unnamed peer is still named by its id in the banner", () => {
  const html = paint(
    snapshot({
      activity_nodes: [{ node_id: NODE_B, status: "timed_out" }],
    }),
  );
  assert.match(html, /Node 9a1c77e2-0000-4000-8000-aa5f912f327f · timed out/);
});

test("authentication refusals and HTTP failures stay distinct in the banner", () => {
  const html = paint(
    snapshot({
      node_hostnames: { [NODE_A]: "nuc3", [NODE_B]: "m6" },
      activity_nodes: [
        { node_id: NODE_A, status: "refused" },
        { node_id: NODE_B, status: "http_error" },
        { node_id: "node-c", status: "unsupported" },
      ],
    }),
  );
  assert.match(html, /Node nuc3 · refused the activity request/);
  assert.match(html, /Node node-c · does not publish Activity HTTP/);
  assert.match(html, /Node m6 · returned an HTTP error/);
  assert.doesNotMatch(html, /Node (nuc3|m6) · unreachable/);
});

test("the peer directory keeps its own sentence when no node can be named", () => {
  const html = paint(
    snapshot({ activity_nodes: [{ status: "unavailable" }] }),
  );
  assert.match(html, /The cluster peer directory · directory unavailable/);
});

test("a hostile hostname is escaped", () => {
  const html = paint(
    snapshot({ node_hostnames: { [NODE_A]: '"><img src=x onerror=alert(1)>' } }),
  );
  assert.doesNotMatch(html, /<img src=x/);
  assert.match(html, /&quot;&gt;&lt;img src=x onerror=alert\(1\)&gt;/);
});

test("a hostile node id is escaped, named or not", () => {
  // The id is server-generated, but it reaches this cell down the same path as
  // the peer-reported hostname and is printed in both branches of the cell.
  const hostile = '"><img src=x onerror=alert(1)>';
  const named = paint(snapshot({
    deliveries: [Object.assign({}, snapshot().deliveries[0], { node_id: hostile })],
    node_hostnames: { [hostile]: "nuc3" },
  }));
  assert.doesNotMatch(named, /<img src=x/);
  assert.match(named, /<div class="clid">&quot;&gt;&lt;img src=x/);

  const unnamed = paint(snapshot({
    deliveries: [Object.assign({}, snapshot().deliveries[0], { node_id: hostile })],
  }));
  assert.doesNotMatch(unnamed, /<img src=x/);
  assert.match(unnamed, /<span class="clid">&quot;&gt;&lt;img src=x/);
});

test("a non-string name is refused rather than printed", () => {
  const html = paint(snapshot({ node_hostnames: { [NODE_A]: { name: "nuc3" } } }));
  assert.match(html, /<span class="clid">5deeeebc-8f39-4cb5-8e4a-aa5f912f327f<\/span>/);
  assert.doesNotMatch(html, /\[object Object\]/);
});

test("names never survive into a snapshot that did not carry them", () => {
  paint(snapshot({ node_hostnames: { [NODE_A]: "nuc3" } }));
  const html = paint(snapshot());
  assert.doesNotMatch(html, /nuc3/);
  assert.match(html, /<span class="clid">5deeeebc-8f39-4cb5-8e4a-aa5f912f327f<\/span>/);
});

// ---- the Stream cell -------------------------------------------------------
//
// What these protect: the cell used to be one " · "-joined sentence of every
// fact the session had, so "is it playing / is the server keeping up / is it
// held and why" were answered somewhere in the middle of the sequence
// numbers. The assertions read the painted row, not the helpers, so the table
// cannot quietly go back to printing the sentence.

const SESSION = "75813686-9927-41d0-97e0-18d152f9efa2";
function liveSession(overrides) {
  return Object.assign(
    {
      id: SESSION,
      presentation: "live-recovery",
      item_id: 7,
      item_title: "Tom Segura: Disgraceful",
      user_name: "pjunod",
      target_height: 1080,
      encoder: "vaapi",
      lease_mode: "explicit",
      lease_state: "active",
      lease_timeout_ms: 30_000,
      control_demand: "active",
      reported_position_ms: 1_693_000,
      client_runway_ms: 43_000,
      render_state: "rendering",
      production_policy: "explicit_demand",
      ahead_seconds: 61,
      production_ahead_seconds: 59,
      production_target_seconds: 73,
      producer_attempt: 1,
      playlist_ready: true,
      published_segment: 114,
      next_media_sequence: 115,
      suspended: false,
      suspend_count: 0,
      delivered_bytes: 734_003_200,
      delivered_bps: 12_400_000,
      delivered_idle_ms: 0,
    },
    overrides,
  );
}
function streaming(sessionOverrides, deliveryOverrides) {
  const session = liveSession(sessionOverrides);
  return snapshot({
    sessions: [session],
    deliveries: [
      Object.assign(
        {
          method: "transcode",
          presentation: session.presentation,
          user: "pjunod",
          file_id: 1,
          item_id: 7,
          title: "Tom Segura: Disgraceful",
          started_unix: Math.floor(Date.now() / 1000) - 120,
          idle_seconds: 0,
          session_id: SESSION,
          target_height: session.target_height,
          encoder: session.encoder,
          delivered_bytes: session.delivered_bytes,
          delivered_bps: session.delivered_bps,
        },
        deliveryOverrides,
      ),
    ],
  });
}

test("an active transcode leads with a state pill, a method line and named meters", () => {
  const html = paint(streaming());
  assert.match(html, /<td class="stream-cell"><div class="stream-head"><span class="mode-chip live">Live HLS<\/span><span class="stream-state active">Active<\/span><span class="stream-method">Transcode <span class="sub">· 1080p · vaapi<\/span><\/span><\/div>/);
  assert.match(html, /<span class="k">Position<\/span><span class="v">28:13<\/span>/);
  assert.match(html, /<div class="stream-meter "><span class="k">Server ahead<\/span><span class="v">61 s<\/span><\/div>/);
  assert.match(html, /<div class="stream-meter good"><span class="k">Demand window<\/span><span class="v">59 s<span class="of">of 73 s<\/span><\/span><span class="stream-bar" aria-hidden="true"><i style="width:81%"><\/i><\/span><\/div>/);
  assert.match(html, /<span class="k">Client runway<\/span><span class="v">43 s<\/span>/);
  assert.match(html, /<span class="k">Delivery rate<\/span><span class="v">12 Mb\/s<span class="of">734 MB<\/span><\/span>/);
  assert.doesNotMatch(html, /Suspends/);
  assert.doesNotMatch(html, /position 1693s|produced ahead 59s|next sequence 115 ·/);
  assert.doesNotMatch(html, /undefined|NaN|null/);
});

test("the sequence numbers survive behind the disclosure, none of them lost", () => {
  const html = paint(streaming({ pending_fetched_segment: 116, fetched_segment: 113, last_request: "segment", recent_speed: 1.42 }));
  assert.match(html, /<details class="stream-diag" data-stream="75813686-9927-41d0-97e0-18d152f9efa2"><summary>Technical details<\/summary>/);
  for (const [label, value] of [
    ["Lease", "explicit · 30 s · active"], ["Demand", "active"], ["Policy", "explicit demand"],
    ["Target", "73 s"], ["Render state", "rendering"], ["Encode speed", "1.42×"],
    ["Producer attempt", "1"], ["Playlist", "ready"], ["Published segment", "114"],
    ["Next sequence", "115"], ["Fetched segment", "113"], ["Fetched timing pending", "116"],
    ["Last request", "segment"],
  ]) {
    assert.match(html, new RegExp(`<div><b>${label}</b><span>${value.replace(/[.*+?^${}()|[\\]\\\\]/g, "\\\\$&")}</span></div>`), `${label} row`);
  }
});

test("a held session says so on the pill, with the reason, and counts its suspends", () => {
  const html = paint(streaming({ suspended: true, hold_reason: "time", suspend_count: 6, resume_below_seconds: 45 }));
  assert.match(html, /<span class="stream-state hold">Holding <span class="why">· reserve full<\/span><\/span>/);
  assert.match(html, /<div class="stream-meter warn"><span class="k">Suspends<\/span><span class="v">6<\/span><\/div>/);
  // A full reserve is not a warning colour on the ahead meter: held means the
  // server has produced enough, not too little.
  assert.match(html, /<div class="stream-meter "><span class="k">Demand window<\/span><span class="v">59 s/);
  assert.match(html, /<div><b>Hold reason<\/b><span>time · resumes below 45 s<\/span><\/div>/);
  const bytes = paint(streaming({ suspended: true, hold_reason: "global", suspend_count: 1, resume_below_bytes: 536_870_912 }));
  assert.match(bytes, /· global scratch limit<\/span>/);
  assert.match(bytes, /<b>Hold reason<\/b><span>global · resumes below 537 MB<\/span>/);
});

test("a production deficit is a red Behind pill and a red negative meter", () => {
  const html = paint(streaming({ production_ahead_seconds: -3 }));
  assert.match(html, /<span class="stream-state bad">Behind <span class="why">· 3 s deficit<\/span><\/span>/);
  assert.match(html, /<div class="stream-meter bad"><span class="k">Demand window<\/span><span class="v">−3 s<span class="of">of 73 s<\/span><\/span><span class="stream-bar" aria-hidden="true"><i style="width:0%">/);
});

test("a stalled client outranks the hold it sits under, and a dead lease outranks both", () => {
  const stalled = paint(streaming({ render_state: "stalled", suspended: true, hold_reason: "time", suspend_count: 2 }));
  assert.match(stalled, /<span class="stream-state bad">Client stalled <span class="why">· held · reserve full<\/span><\/span>/);
  const expired = paint(streaming({ lease_state: "expired", render_state: "stalled", suspended: true }));
  assert.match(expired, /<span class="stream-state bad">Lease expired<\/span>/);
  const legacy = paint(streaming({ lease_mode: "legacy", lease_state: "unavailable", production_policy: "legacy_frontier", production_ahead_seconds: null, ahead_seconds: 31, production_target_seconds: null }));
  assert.match(legacy, /<span class="stream-state active">Active<\/span>/);
  assert.match(legacy, /<span class="k">Server ahead<\/span><span class="v">31 s<\/span><\/div>/);
  assert.doesNotMatch(legacy, /Demand window/);
  assert.match(legacy, /<b>Lease<\/b><span>legacy · 30 s · unavailable<\/span>/);
});

test("a viewer waiting on its first playlist is Starting, and a short runway is amber", () => {
  const html = paint(streaming({ playlist_ready: false, published_segment: null, production_ahead_seconds: null, client_runway_ms: 4_000 }));
  assert.match(html, /<span class="stream-state wait">Starting <span class="why">· playlist not published yet<\/span><\/span>/);
  assert.match(html, /<div class="stream-meter warn"><span class="k">Demand window<\/span><span class="v">—<span class="of">waiting for publication<\/span>/);
  assert.match(html, /<div class="stream-meter bad"><span class="k">Client runway<\/span><span class="v">4 s<\/span>/);
  assert.match(html, /<div><b>Playlist<\/b><span>waiting<\/span><\/div>/);
});

test("a failed producer or client is never painted Active", () => {
  const producer = paint(streaming({ producer_state: "failed" }));
  assert.match(producer, /<span class="stream-state bad">Producer failed<\/span>/);
  assert.match(producer, /<div><b>Producer<\/b><span>failed<\/span><\/div>/);
  const client = paint(streaming({ render_state: "failed", suspended: true, hold_reason: "time" }));
  assert.match(client, /<span class="stream-state bad">Client failed <span class="why">· held · reserve full<\/span><\/span>/);
  assert.match(paint(streaming({ render_state: "ended" })), /<span class="stream-state idle">Ending<\/span>/);
});

test("a viewer's own hold reads as Paused, not as a server hold, and hides the zero target", () => {
  const html = paint(streaming({ control_demand: "hold", suspended: true, hold_reason: "demand", suspend_count: 1, production_target_seconds: 0, production_ahead_seconds: 40 }));
  assert.match(html, /<span class="stream-state idle">Paused <span class="why">· viewer asked to hold<\/span><\/span>/);
  assert.doesNotMatch(html, /Holding|<b>Target<\/b>|of 0 s/);
  assert.match(html, /<div class="stream-meter "><span class="k">Demand window<\/span><span class="v">40 s<\/span><\/div>/);
  assert.match(html, /<div><b>Hold reason<\/b><span>demand<\/span><\/div>/);
});

test("a byte hold at zero release still prints a number", () => {
  const html = paint(streaming({ suspended: true, hold_reason: "bytes", resume_below_bytes: 0 }));
  assert.match(html, /<b>Hold reason<\/b><span>bytes · resumes below 0 B<\/span>/);
});

test("the rung and encoder come from the session row, which is where the server puts them", () => {
  const html = paint(streaming({ target_height: 720, encoder: "nvenc" }, { target_height: undefined, encoder: undefined }));
  assert.match(html, /<span class="stream-method">Transcode <span class="sub">· 720p · nvenc<\/span><\/span>/);
  assert.doesNotMatch(html, /undefined/);
});

test("a direct play has a headline and a delivery meter and nothing invented", () => {
  const html = paint(snapshot({
    deliveries: [{ method: "direct", user: "pjunod", file_id: 1, item_id: 7, title: "Remux Me", started_unix: Math.floor(Date.now() / 1000) - 60, idle_seconds: 0, delivered_bytes: null, delivered_bps: null }],
  }));
  assert.match(html, /<td class="stream-cell"><div class="stream-head"><span class="stream-method">Direct play<\/span><\/div><\/td>/);
  assert.doesNotMatch(html, /stream-state|stream-meters|stream-diag/);
  const remux = paint(snapshot({
    deliveries: [{ method: "remux", user: "pjunod", file_id: 1, item_id: 7, title: "Remux Me", started_unix: Math.floor(Date.now() / 1000) - 60, idle_seconds: 44, delivered_bytes: 1_048_576, delivered_bps: 38_200_000 }],
  }));
  assert.match(remux, /<span class="stream-method">Remux<\/span><\/div><div class="stream-meters"><div class="stream-meter "><span class="k">Delivery rate<\/span><span class="v">38 Mb\/s<span class="of">1.0 MB<\/span><\/span><\/div><\/div><\/td>/);
  assert.match(remux, /<div>idle 44s<\/div>/);
  assert.doesNotMatch(remux, /stream-state|stream-diag/);
});

test("a cached VOD session says so once, on the method line", () => {
  const html = paint(streaming({ presentation: "vod", encoder: "cached" }, { presentation: "vod", encoder: "cached" }));
  assert.match(html, /<span class="mode-chip vod">VOD HLS<\/span>/);
  assert.match(html, /<span class="stream-method">Transcode · cached <span class="sub">· 1080p<\/span><\/span>/);
});

test("a disclosure the reader opened stays open across the repaint", () => {
  painter.main.openStreams = [SESSION];
  const html = paint(streaming());
  painter.main.openStreams = [];
  assert.match(html, /<details class="stream-diag" data-stream="75813686-9927-41d0-97e0-18d152f9efa2" open>/);
  assert.match(paint(streaming()), /<details class="stream-diag" data-stream="75813686-9927-41d0-97e0-18d152f9efa2"><summary>/);
});

test("session facts are escaped on the way into the cell", () => {
  const html = paint(streaming({ hold_reason: "\"><img src=x onerror=alert(1)>", suspended: true, last_request: "<b>x</b>" }));
  assert.doesNotMatch(html, /<img src=x/);
  assert.match(html, /· &quot;&gt;&lt;img src=x onerror=alert\(1\)&gt;<\/span>/);
  assert.match(html, /<b>Last request<\/b><span>&lt;b&gt;x&lt;\/b&gt;<\/span>/);
});

let reported = false;
process.on("beforeExit", () => {
  if (reported) return;
  reported = true;
  if (started !== finished) {
    failures += started - finished;
    process.stdout.write(
      `FAIL ${started - finished} test(s) never finished — an async body is waiting on ` +
        `something the test never resolves, so it printed no result at all\n`,
    );
  }
  if (failures) process.exitCode = 1;
});
