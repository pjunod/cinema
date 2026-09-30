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

const {shellSource} = require("./shell-source.js");
// The shipped app is a tree now, so the string these assertions slice is
// the app's body rows, joined in served order. See tests/web/shell-source.js.
const SHIPPED_UI = shellSource().bodyScript;

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
  "activityWatchingHtml",
  // Live TV rows are drawn by the same painter and name their owner node
  // through the same roster map, so they are borrowed rather than stubbed.
  "liveTvActivityRows",
  // DVR cards/details have a full-browser regression in dvr-ui.browser.cjs.
  "liveTvNowSeconds",
  "durableDuration",
  "durableLeaseExpired",
  "durableStateLabel",
  "durableTypeBreakdownHtml",
  "activityTabsHtml",
  "activityInspectorHtml",
  "activityJobProgress",
  "fmtDur",
  "durableQueueHtml",
  "clusterWorkersHtml",
  "paintActivityBody",
];

// The painter's neighbours on the page. None of them decide anything this file
// asserts, so they are stubs — but `main` is real enough to read back.
const PRELUDE = `
  let PAINTED = null;
  let ACTIVITY_VIEWING_ID = null;
  let ACTIVITY_SNAPSHOT = {};
  const ACTIVITY_VIEW={tab:"status",inspector:null,detailTab:"summary"};
  const DVR_PAGE={selectedId:null};
  function paintActivityInspector(){}
  // The painter reads the open disclosures back out of #main before it
  // repaints; a string-backed stand-in has none unless a test plants some.
  const main = { innerHTML: "", openStreams: [], querySelectorAll(){ return main.openStreams.map((key) => ({ dataset: { stream: key } })); } };
  const document = { getElementById: (id) => (id === "main" ? main : null) };
  const ME = { is_admin: true };
  const DURABLE_ACTIVITY = {state:"queued",rows:[],counts:[],observed:0,detail:null};
  function paintActivity(acts){ PAINTED = acts; }
  function dvrRememberUi(){ return {}; }
  function dvrRestoreUi(){}
  function dvrActivityRows(){ return ""; }
  function analysisSummaryCard(){ return "<div class=\\"analysis\\"></div>"; }
  function statusText(){ return "idle"; }
  // The Processes table has its own suite (activity-processes.test.js).
  function activityProcessesHtml(){ return ""; }
`;

const painter = new Function(
  "PlurxLiveTv",
  "return (function(){" +
    PRELUDE +
    BORROWED.map(shippedSource).join("\n") +
    "\nreturn {paintActivityBody, main, nodeLabel, durable:DURABLE_ACTIVITY, view:ACTIVITY_VIEW, inspect:activityInspectorHtml, snapshot:(d)=>{ACTIVITY_SNAPSHOT=d;}, select:(key)=>{ACTIVITY_VIEWING_ID=key;}};})()",
)(require("../../crates/plurxd/src/web/live-tv.js"));

const NODE_A = "5deeeebc-8f39-4cb5-8e4a-aa5f912f327f";
const NODE_B = "9a1c77e2-0000-4000-8000-aa5f912f327f";

function snapshot(overrides) {
  return Object.assign(
    {
      deliveries: [
        {
          method: "hls-copy",
          presentation: "live-recovery",
          user: "operator",
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
  painter.snapshot(d);
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
  const html = paint(snapshot({ node_hostnames: { [NODE_A]: "lab3" } }));
  assert.match(
    html,
    /<dd><div class="nodename">lab3<\/div><div class="clid">5deeeebc-8f39-4cb5-8e4a-aa5f912f327f<\/div><\/dd>/,
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
  const d = snapshot({ node_hostnames: { [NODE_A]: "lab3" } });
  d.deliveries.push(
    Object.assign({}, d.deliveries[0], { node_id: NODE_B, file_id: 2, user: "guest" }),
  );
  painter.select(`delivery:${NODE_B}:7:${d.deliveries[1].started_unix}`);
  const html = paint(d);
  assert.match(html, /<span class="clid">9a1c77e2-0000-4000-8000-aa5f912f327f<\/span><\/dd>/);
  assert.match(html, /data-activity-view="delivery:5deeeebc/,
    "the named neighbour remains a selectable viewer card");
});

test("the serving-node detail stays explicit with and without an id", () => {
  const withIds = paint(snapshot());
  assert.match(withIds, /<dt>Serving node<\/dt><dd><span class="clid">5deeeebc/);
  const d = snapshot();
  delete d.deliveries[0].node_id;
  painter.select(`delivery::7:${d.deliveries[0].started_unix}`);
  assert.match(paint(d), /<dt>Serving node<\/dt><dd><span class="clid">Unknown<\/span><\/dd>/);
});

test("a row with no node id at all is Unknown, not blank and not undefined", () => {
  const d = snapshot({ node_hostnames: { [NODE_A]: "lab3" } });
  d.deliveries = [Object.assign({}, d.deliveries[0], { node_id: "", file_id: 3, user: "guest" })];
  painter.select(`delivery::7:${d.deliveries[0].started_unix}`);
  const html = paint(d);
  assert.match(html, /<span class="clid">Unknown<\/span>/);
  assert.doesNotMatch(html, /undefined/);
});

test("the incomplete-activity banner names the node it is complaining about", () => {
  const html = paint(
    snapshot({
      node_hostnames: { [NODE_B]: "lab6" },
      activity_nodes: [
        { node_id: NODE_A, status: "answered" },
        { node_id: NODE_B, status: "unreachable" },
      ],
    }),
  );
  assert.match(html, /Node lab6 · unreachable/);
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
      node_hostnames: { [NODE_A]: "lab3", [NODE_B]: "lab6" },
      activity_nodes: [
        { node_id: NODE_A, status: "refused" },
        { node_id: NODE_B, status: "http_error" },
        { node_id: "node-c", status: "unsupported" },
      ],
    }),
  );
  assert.match(html, /Node lab3 · refused the activity request/);
  assert.match(html, /Node node-c · does not publish Activity HTTP/);
  assert.match(html, /Node lab6 · returned an HTTP error/);
  assert.doesNotMatch(html, /Node (lab3|lab6) · unreachable/);
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
    node_hostnames: { [hostile]: "lab3" },
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
  const html = paint(snapshot({ node_hostnames: { [NODE_A]: { name: "lab3" } } }));
  assert.match(html, /<span class="clid">5deeeebc-8f39-4cb5-8e4a-aa5f912f327f<\/span>/);
  assert.doesNotMatch(html, /\[object Object\]/);
});

test("names never survive into a snapshot that did not carry them", () => {
  paint(snapshot({ node_hostnames: { [NODE_A]: "lab3" } }));
  const html = paint(snapshot());
  assert.doesNotMatch(html, /lab3/);
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
      user_name: "operator",
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
          user: "operator",
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
  assert.match(html, /<div class="stream-head"><span class="mode-chip live">Live HLS<\/span><span class="stream-state active">Active<\/span><span class="stream-method">Transcode <span class="sub">· 1080p · vaapi<\/span><\/span><\/div>/);
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
    deliveries: [{ method: "direct", user: "operator", file_id: 1, item_id: 7, title: "Remux Me", started_unix: Math.floor(Date.now() / 1000) - 60, idle_seconds: 0, delivered_bytes: null, delivered_bps: null }],
  }));
  assert.match(html, /<div class="stream-head"><span class="stream-method">Direct play<\/span><\/div>/);
  assert.doesNotMatch(html, /stream-state|stream-meters|stream-diag/);
  const remux = paint(snapshot({
    deliveries: [{ method: "remux", user: "operator", file_id: 1, item_id: 7, title: "Remux Me", started_unix: Math.floor(Date.now() / 1000) - 60, idle_seconds: 44, delivered_bytes: 1_048_576, delivered_bps: 38_200_000 }],
  }));
  assert.match(remux, /<span class="stream-method">Remux<\/span><\/div><div class="stream-meters"><div class="stream-meter "><span class="k">Delivery rate<\/span><span class="v">38 Mb\/s<span class="of">1.0 MB<\/span><\/span><\/div><\/div>/);
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

test("durable work renders escaped observations beside current activity", () => {
  painter.durable.observed = Date.now();
  painter.durable.rows = [{id:'job-"<id>',kind:"fragment_index_build",state:"queued",
    title:"<hostile-title>",library:"<hostile-library>",owner_node_id:"<hostile-owner>",
    priority:1,age_ms:120000,not_before_ms:0,supported:true,error_code:"<hostile-error>"}];
  const html = paint(snapshot());
  assert.match(html, /<h2 class="section">Jobs/);
  assert.match(html, /fragment index build/);
  for(const field of ["title","library","owner","error"]){
    assert.ok(html.includes(`&lt;hostile-${field}&gt;`));
    assert.ok(!html.includes(`<hostile-${field}>`));
  }
  assert.match(html, /cancelDurableJob/);
  assert.match(html, /2 min/);
  painter.durable.rows = [];
});

test("durable owners, attempts and repair destinations use roster names", () => {
  const now=Date.now();
  const job={id:"job",kind:"subtitle_extract",state:"running",owner_node_id:"owner-id",
    priority:0,age_ms:1427*60000,not_before_ms:0,supported:true,
    lease_expires_ms:now+30000,observed_at_ms:now};
  Object.assign(painter.durable,{observed:now,rows:[job],repairs:[{kind:"subtitle",target_node_id:"owner-id",phase:"copying",age_ms:1000}],
    detail:{job,waiters:[],attempts:[{node_id:"owner-id",started_at_ms:now-120000,outcome:null}]}});
  const html=paint(snapshot({node_hostnames:{"owner-id":"m6"}}));
  assert.match(html, /title="owner-id">m6<\/span>/);
  assert.match(html, /<td>m6<\/td><td>copying/);
  painter.view.inspector={kind:"job",id:"job"};painter.view.detailTab="history";
  assert.match(painter.inspect(), /m6<\/b> · running[\s\S]*2 min elapsed/);painter.view.inspector=null;
  assert.match(html, /Since requested/);
  assert.match(html, /23h 47m/);
  assert.doesNotMatch(html, /1427 min/);
  const missing=paint(snapshot());
  assert.match(missing, /title="owner-id">owner-id<\/span>/);
  Object.assign(painter.durable,{rows:[],repairs:[],detail:null});
});

test("expired durable leases do not masquerade as live executions", () => {
  const now=Date.now();
  const job={id:"expired",kind:"subtitle_extract",state:"running",owner_node_id:"owner-id",
    priority:0,age_ms:1427*60000,not_before_ms:0,supported:true,
    lease_expires_ms:now-3600000,observed_at_ms:now};
  Object.assign(painter.durable,{observed:now,rows:[job],detail:{job,waiters:[],
    attempts:[{node_id:"owner-id",started_at_ms:job.lease_expires_ms-120000,outcome:null}]}});
  const html=paint(snapshot({node_hostnames:{"owner-id":"<m6>"}}));
  assert.match(html, /Lease expired · awaiting recovery/);
  assert.match(html, /previous owner/);
  painter.view.inspector={kind:"job",id:"expired"};painter.view.detailTab="history";
  assert.match(painter.inspect(), /&lt;m6&gt;<\/b> · lease expired[\s\S]*2 min elapsed/);painter.view.inspector=null;
  assert.doesNotMatch(html, /<m6>/);
  Object.assign(painter.durable,{rows:[],detail:null});
});

function refreshingQueue(q,api){
  return new Function("DURABLE_ACTIVITY","api",`
    const ME={is_admin:true},location={hash:"#/activity"};
    let PAGE_RENDER_GENERATION=1;
    const document={getElementById:()=>null};
    function closeActivityInspector(){DURABLE_ACTIVITY.selectedId=null;}
    const ACTIVITY_VIEW={inspector:{kind:"job",id:"job"},detailTab:"history"};
    const ACTIVITY_SNAPSHOT={node_hostnames:{node:"m6"}};
    function paintDurableActivity(){}
    ${["esc","nodeLabel","durableDuration","durableLeaseExpired","durableStateLabel","durableTypeBreakdownHtml","durableQueueHtml","activityInspectorHtml","activityJobProgress","refreshDurableActivity","resetActivityWork"].map(shippedSource).join("\n")}
    return {refresh:refreshDurableActivity,reset:()=>{resetActivityWork();PAGE_RENDER_GENERATION++;},html:()=>activityInspectorHtml()};
  `)(q,api);
}
test("bounded queue refresh updates the open attempt when execution completes", async () => {
  const now=Date.now(),job={id:"job",kind:"subtitle_extract",state:"running",observed_at_ms:now-60000,lease_expires_ms:now-30000};
  const q={state:"running",epoch:0,observed:0,rows:[],counts:[],selectedId:"job",
    detail:{job,waiters:[],attempts:[{node_id:"node",started_at_ms:now-120000,outcome:null}]}};
  const calls=[];
  const runner=refreshingQueue(q,async url=>{
    calls.push(url);
    if(url.includes("?"))return {jobs:[],counts:[],observed_at_ms:now};
    return {job:{...job,state:"succeeded",observed_at_ms:now,lease_expires_ms:null},waiters:[],
      attempts:[{node_id:"node",started_at_ms:now-120000,finished_at_ms:now,outcome:"succeeded"}]};
  });
  await runner.refresh(true);
  assert.deepEqual(calls,["/cluster/jobs?state=running&limit=25","/cluster/jobs/job"]);
  assert.match(runner.html(), /m6<\/b> · succeeded[\s\S]*2 min elapsed/);
  assert.doesNotMatch(runner.html(), /this attempt|awaiting recovery/);
});
test("a late detail refresh cannot reopen closed or replace newly selected details", async () => {
  for(const change of [q=>{q.detail=null;q.selectedId=null;},q=>{q.detail={job:{id:"other"}};q.selectedId="other";}]){
    const q={state:"running",epoch:0,observed:0,rows:[],counts:[],selectedId:"job",detail:{job:{id:"job"}}};
    const runner=refreshingQueue(q,async url=>{
      if(url.includes("?"))return {jobs:[],counts:[],observed_at_ms:Date.now()};
      change(q);
      return {job:{id:"job",state:"succeeded"}};
    });
    await runner.refresh(true);
    assert.notEqual(q.detail?.job.id,"job");
  }
});

test("a pending old session cannot block or release a new queue request", async () => {
  const q={state:"queued",epoch:0,rows:[],counts:[],observed:0,retries:new Map()};
  const pending=[];const runner=refreshingQueue(q,()=>new Promise(resolve=>pending.push(resolve)));
  const old=runner.refresh(true);assert.equal(pending.length,1);
  runner.reset();const fresh=runner.refresh(true);assert.equal(pending.length,2);
  pending[0]({jobs:[{id:"old"}],counts:[],observed_at_ms:Date.now()});await old;
  assert.equal(q.busy,true);assert.deepEqual(q.rows,[]);
  pending[1]({jobs:[{id:"new"}],counts:[],observed_at_ms:Date.now()});await fresh;
  assert.equal(q.busy,false);assert.equal(q.rows[0].id,"new");
});
test("polling recovers an inspector whose first detail read failed", async () => {
  const q={state:"queued",epoch:0,rows:[],counts:[],observed:0,selectedId:"job",detail:null,detailError:"unavailable"};
  const calls=[];const runner=refreshingQueue(q,async url=>{calls.push(url);return url.includes("?")?{jobs:[],counts:[],observed_at_ms:Date.now()}:{job:{id:"job"},waiters:[],attempts:[]};});
  await runner.refresh(true);assert.equal(calls.length,2);assert.equal(q.detail.job.id,"job");assert.equal(q.detailError,null);
});

function workerObservation(overrides={}) {
  return {observed_at_ms:Date.now(),heavy_limit:1,heavy_in_use:0,heavy_available:1,
    accepting_work:true,hardware_used:0,hardware_limit:3,software_used:0,software_limit:8,
    children:[],child_count:0,...overrides};
}

test("cluster capacity counts fresh nodes and never counts missing or stale peers as idle", () => {
  const html=paint(snapshot({node_hostnames:{a:"alpha",b:"beta",c:"gamma",d:"delta"},
    activity_nodes:[{node_id:"a",status:"answered"},{node_id:"b",status:"timed_out"}],
    workers:{a:workerObservation(),b:workerObservation(),c:workerObservation({observed_at_ms:Date.now()-60000})}}));
  assert.match(html, /<b>1 \/ 4<\/b><span>nodes reporting capacity/);
  assert.match(html, /<b>1<\/b><span>heavy slots available now/);
  assert.equal((html.match(/Capacity unknown/g)||[]).length,3);
  assert.match(html, /Last capacity report is stale/);
  assert.match(html, /partial total/);
});

test("cluster workers distinguish heavy occupancy, foreground blocking and maintenance", () => {
  const html=paint(snapshot({workers:{busy:workerObservation({heavy_in_use:1,heavy_available:0,software_used:8}),
    blocked:workerObservation({heavy_available:0}),maintenance:workerObservation({heavy_available:0,accepting_work:false})}}));
  assert.match(html, /Heavy worker busy/);
  assert.match(html, /Waiting for media capacity/);
  assert.match(html, /Not accepting work/);
  assert.match(html, /<td>8 \/ 8<\/td>/);
  assert.match(html, /<b>1 \/ 3<\/b><span>heavy slots occupied/);
  assert.match(html, /<b>0<\/b><span>heavy slots available now/);
});

test("node assignments are independent of queue filters and expired ownership stays explicit", () => {
  const now=Date.now();
  Object.assign(painter.durable,{state:"failed",rows:[],observed:now,activeTruncated:true,
    activeJobs:[{id:"j",kind:"subtitle_extract",title:"<unsafe>",owner_node_id:"worker",state:"running",lease_expires_ms:now-1,observed_at_ms:now}]});
  const html=paint(snapshot({workers:{worker:workerObservation({children:["<child>"],child_count:1})}}));
  assert.match(html, /&lt;unsafe&gt;/);
  assert.match(html, /Lease expired · awaiting recovery · previous owner/);
  assert.match(html, /Assignment list is partial/);
  painter.view.inspector={kind:"node",id:"worker"};
  assert.match(painter.inspect(), /&lt;child&gt;/);painter.view.inspector=null;
  assert.doesNotMatch(html, /<unsafe>|<child>/);
  Object.assign(painter.durable,{activeJobs:[],activeTruncated:false});
});


test("watching stays above both tabs and the selected workspace survives polling", () => {
  for(const tab of ["status","jobs"]){
    painter.view.tab=tab;
    const html=paint(snapshot());
    assert.ok(html.indexOf("Happening now · Watching") < html.indexOf('aria-label="Activity views"'));
    const hidden=tab==="jobs"?"status":"jobs";
    assert.match(html,new RegExp(`id="activity-${hidden}"[^>]* hidden`));
    assert.match(html,new RegExp(`id="activity-tab-${tab}"[^>]*aria-selected="true"`));
    assert.match(html, /data-activity-section="recordings"><summary>/);
  }
  painter.view.tab="status";
});
test("job details reject stale and unrelated execution progress", () => {
  const now=Date.now(),job={id:"j",fence:2,kind:"fragment_index_build",state:"running",owner_node_id:"n",observed_at_ms:now-5000,lease_expires_ms:now+30000};
  const row={job_id:"legacy-artifact",durable_job_id:"j",durable_fence:2,node_id:"n",updated_at_ms:now,stage:"fragment_index",bytes_read:100,total_bytes:200};
  painter.durable.detail={job,waiters:[],attempts:[]};painter.view.inspector={kind:"job",id:"j"};painter.view.detailTab="stages";
  painter.snapshot({analysis:{progress:[row]}});assert.match(painter.inspect(), /fragment index/);
  for(const change of [{durable_job_id:"other"},{durable_fence:1},{node_id:"other"},{updated_at_ms:now-60000},{updated_at_ms:now+60000},{durable_fence:undefined}]){
    painter.snapshot({analysis:{progress:[{...row,...change}]}});assert.match(painter.inspect(), /Not reported/);
  }
  painter.snapshot({analysis:{progress:[row]}});
  for(const change of [{state:"succeeded"},{lease_expires_ms:now-1}]){
    painter.durable.detail.job={...job,...change};assert.match(painter.inspect(), /Not reported/);
  }
  painter.view.inspector=null;painter.durable.detail=null;
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
