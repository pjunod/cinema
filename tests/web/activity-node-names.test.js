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
  "activityNodeName",
  "activityNodeCell",
  "activityNodeFailureText",
  "detailActivitySummary",
  "activitySessionControlText",
  "paintActivityBody",
];

// The painter's neighbours on the page. None of them decide anything this file
// asserts, so they are stubs — but `main` is real enough to read back.
const PRELUDE = `
  let PAINTED = null;
  const main = { innerHTML: "" };
  const document = { getElementById: (id) => (id === "main" ? main : null) };
  const ME = { is_admin: true };
  function paintActivity(acts){ PAINTED = acts; }
  function analysisSummaryCard(){ return "<div class=\\"analysis\\"></div>"; }
  function statusText(){ return "idle"; }
  function fmtBytes(){ return ""; }
`;

const painter = new Function(
  "return (function(){" +
    PRELUDE +
    BORROWED.map(shippedSource).join("\n") +
    "\nreturn {paintActivityBody, main, activityNodeName};})()",
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
    /<td><div class="clnode">nuc3<\/div><div class="clid">5deeeebc-8f39-4cb5-8e4a-aa5f912f327f<\/div><\/td>/,
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
  assert.match(html, /<div class="clnode">nuc3<\/div>/);
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
