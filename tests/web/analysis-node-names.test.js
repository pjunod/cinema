"use strict";

// The Content analysis workspace's Node column, tested against the shipped
// index.html.
//
// Same defect as the Now playing table's, one page over: every row here is
// attributed to a node by id, and `298849e0-92fa-4c36-9186-da8b0c1dc01d` names
// no machine. The shape of the answer differs, and deliberately. This row is
// already four lines deep and carries a "Technical details" disclosure, so the
// column gets the name alone and the id keeps a labelled line inside the
// disclosure — where it stays selectable and reaches the Copy details text an
// operator actually pastes into a support question.
//
// The assertions run the shipped painter, not the helpers underneath it.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const SHIPPED_UI = fs.readFileSync(INDEX, "utf8");

// Same extraction contract as tests/web/activity-node-names.test.js.
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

const BORROWED = [
  "esc",
  "fmtAgo",
  "fmtBytes",
  "nodeLabel",
  "analysisNodeCell",
  "analysisNodeDetail",
  "analysisStateLabel",
  "analysisPhase",
  "analysisErrorInfo",
  "analysisErrorHtml",
  "analysisRows",
  "analysisCounts",
  "analysisRowKey",
  "analysisDisposition",
  "analysisErrors",
  "analysisAttemptHistory",
  "analysisAttemptHistoryHtml",
  "analysisAction",
  "analysisCanRetry",
  "analysisDiagnosticText",
  "copyAnalysisDetails",
  "paintAnalysis",
];

// `copied` is filled by the shipped `copyAnalysisDetails`, not by a
// reimplementation of it: the map has to reach the clipboard through the call
// site an operator actually presses, or that call site is free to drop it.
const PRELUDE = `
  const main = { innerHTML: "", querySelectorAll: () => [] };
  const document = { activeElement: null, body: { contains: () => true },
    getElementById: (id) => (id === "main" ? main : null) };
  const copied = [];
  const navigator = { clipboard: { writeText: (text) => { copied.push(text); } } };
  function toast(){}
  let ANALYSIS_SNAPSHOT = null, ANALYSIS_ROW_LOOKUP = new Map();
  let ANALYSIS_VIEW = {filter:"all",query:"",page:1,pageSize:25,auto:true,cursors:[""]};
`;

const workspace = new Function(
  "return (function(){" +
    PRELUDE +
    BORROWED.map(shippedSource).join("\n") +
    "\nreturn {main, copied, paint:(s)=>{ANALYSIS_SNAPSHOT=s;paintAnalysis(s);}," +
    "\n  copy:(key)=>copyAnalysisDetails(key,{textContent:\"Copy details\"})};})()",
)();

async function copyText(key) {
  workspace.copied.length = 0;
  await workspace.copy(key);
  assert.equal(workspace.copied.length, 1, "the shipped copy path wrote once");
  return workspace.copied[0];
}

const OWNER = "298849e0-92fa-4c36-9186-da8b0c1dc01d";
const TARGET = "75813686-9927-41d0-97e0-18d152f9efa2";

function row(overrides) {
  return Object.assign(
    {
      row_key: "job:job-1",
      request_id: "",
      job_id: "job-1",
      file_id: "1",
      item_id: "100",
      title: "Movie",
      state: "ready",
      request_state: "",
      job_state: "ready",
      disposition: "ready",
      updated_at_ms: 100,
      attempts: 1,
      owner_node_id: OWNER,
      target_node_id: TARGET,
      pipeline_version: "0123456789ab",
      source_size: 123,
      request_error_code: "",
      job_error_code: "",
      action: "rebuild",
    },
    overrides,
  );
}

function snapshot(overrides) {
  return Object.assign(
    {
      enabled: true,
      now_ms: 200,
      filtered_total: 1,
      next_cursor: null,
      rows: [row()],
      summary: { available: true, enabled: true, total: 1, ready: 1 },
    },
    overrides,
  );
}

function paint(d) {
  workspace.paint(d);
  return workspace.main.innerHTML;
}

// Queued and run in order, not fired off as they are declared. These tests
// share one painted snapshot through module state, exactly as the page does,
// so an async body that yields lets the next test repaint underneath it — which
// is how the first draft of this file reported a bare id for a named node and
// blamed the code. A test suite that interleaves is measuring itself.
const QUEUE = [];
let started = 0;
let finished = 0;
let failures = 0;
function test(name, run) {
  QUEUE.push({ name, run });
}

async function main() {
  for (const { name, run } of QUEUE) {
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
}

test("the Node column names the machine that owns the row", () => {
  const html = paint(snapshot({ node_hostnames: { [OWNER]: "nuc3" } }));
  assert.match(html, /<span class="nodename">nuc3<\/span>/);
  // The defect: the id must not be what the column shows.
  assert.doesNotMatch(html, /<span class="clid">298849e0/);
});

test("the id keeps a labelled line in Technical details, name beside it", () => {
  const html = paint(
    snapshot({ node_hostnames: { [OWNER]: "nuc3", [TARGET]: "m6" } }),
  );
  assert.match(
    html,
    /<b>Owner<\/b><span>nuc3 \(298849e0-92fa-4c36-9186-da8b0c1dc01d\)<\/span>/,
  );
  assert.match(
    html,
    /<b>Target node<\/b><span>m6 \(75813686-9927-41d0-97e0-18d152f9efa2\)<\/span>/,
  );
});

test("an unnamed node keeps the id it always showed, in both places", () => {
  const html = paint(snapshot());
  assert.match(html, /<span class="clid">298849e0-92fa-4c36-9186-da8b0c1dc01d<\/span>/);
  assert.match(html, /<b>Owner<\/b><span>298849e0-92fa-4c36-9186-da8b0c1dc01d<\/span>/);
  assert.doesNotMatch(html, /\(298849e0/);
  assert.doesNotMatch(html, /undefined/);
  assert.doesNotMatch(html, /null/);
});

test("a row attributed to nobody is an em dash, not a name and not undefined", () => {
  const html = paint(
    snapshot({
      rows: [row({ owner_node_id: "", target_node_id: "" })],
      node_hostnames: { [OWNER]: "nuc3" },
    }),
  );
  assert.match(html, /<span class="clid">—<\/span>/);
  assert.doesNotMatch(html, /nuc3/);
  assert.doesNotMatch(html, /undefined/);
  // An unattributed row has no Owner or Target line at all, as before.
  assert.doesNotMatch(html, /<b>Owner<\/b>/);
  assert.doesNotMatch(html, /<b>Target node<\/b>/);
});

test("the column falls back to the target when no owner has claimed the row", () => {
  const html = paint(
    snapshot({
      rows: [row({ owner_node_id: "" })],
      node_hostnames: { [TARGET]: "m6" },
    }),
  );
  assert.match(html, /<span class="nodename">m6<\/span>/);
});

test("analysis rows render their stored priority and trigger", () => {
  const background = row({
    row_key: "request:background",
    request_id: "background",
    job_id: "",
    priority: "normal",
    trigger: "background",
  });
  const foreground = row({
    row_key: "job:foreground",
    request_id: "",
    job_id: "foreground",
    priority: "foreground",
    trigger: "foreground",
  });
  const html = paint(
    snapshot({ filtered_total: 2, rows: [background, foreground] }),
  );
  assert.match(html, /Priority Normal · Trigger Background/);
  assert.match(html, /Priority Foreground · Trigger Foreground/);
  assert.match(html, /<b>Priority<\/b><span>Foreground<\/span>/);
  assert.match(html, /<b>Trigger<\/b><span>Foreground<\/span>/);
  assert.doesNotMatch(html, /Operator request/);
  assert.doesNotMatch(html, /Background build/);
});

test("Copy details carries the name and the id an operator will quote", async () => {
  paint(snapshot({ node_hostnames: { [OWNER]: "nuc3", [TARGET]: "m6" } }));
  const text = await copyText("job:job-1");
  assert.match(text, /Owner: nuc3 \(298849e0-92fa-4c36-9186-da8b0c1dc01d\)/);
  assert.match(text, /Target: m6 \(75813686-9927-41d0-97e0-18d152f9efa2\)/);
});

test("Copy details still carries a bare id when the roster named nobody", async () => {
  paint(snapshot());
  const text = await copyText("job:job-1");
  assert.match(text, /Owner: 298849e0-92fa-4c36-9186-da8b0c1dc01d/);
  assert.doesNotMatch(text, /Owner: undefined/);
  assert.doesNotMatch(text, /Owner: \S+ \(/);
});

test("a hostile hostname and a hostile id are both escaped", () => {
  const hostile = '"><img src=x onerror=alert(1)>';
  const html = paint(
    snapshot({
      rows: [row({ owner_node_id: hostile, target_node_id: "" })],
      node_hostnames: { [hostile]: '<script>alert(2)</script>' },
    }),
  );
  assert.doesNotMatch(html, /<img src=x/);
  assert.doesNotMatch(html, /<script>alert\(2\)/);
  assert.match(html, /<span class="nodename">&lt;script&gt;alert\(2\)&lt;\/script&gt;<\/span>/);
  assert.match(html, /&quot;&gt;&lt;img src=x/);
});

test("a hostile id is escaped in the column too, where no name covers it", () => {
  // The named branch hides this one: with a hostname in the map the column
  // renders the name, and the id only appears in the details line. An
  // unnamed node is the branch that prints the id into the column itself.
  const hostile = '"><img src=x onerror=alert(3)>';
  const html = paint(
    snapshot({ rows: [row({ owner_node_id: hostile, target_node_id: "" })] }),
  );
  assert.doesNotMatch(html, /<img src=x/);
  assert.match(html, /<span class="clid">&quot;&gt;&lt;img src=x onerror=alert\(3\)&gt;<\/span>/);
});

test("a non-string name is refused rather than printed", () => {
  const html = paint(snapshot({ node_hostnames: { [OWNER]: ["nuc3"] } }));
  assert.match(html, /<span class="clid">298849e0-92fa-4c36-9186-da8b0c1dc01d<\/span>/);
  assert.doesNotMatch(html, /\[object Object\]/);
});

test("names never survive into a snapshot that did not carry them", () => {
  paint(snapshot({ node_hostnames: { [OWNER]: "nuc3" } }));
  const html = paint(snapshot());
  assert.doesNotMatch(html, /nuc3/);
  assert.match(html, /<span class="clid">298849e0-92fa-4c36-9186-da8b0c1dc01d<\/span>/);
});

test("the node label wears a class nothing else in the sheet styles", () => {
  // The first draft called it `.clnode`, which is also the cluster page's node
  // *card* — border, radius, panel background — defined later in the same
  // stylesheet at equal specificity. The later rule wins, so the label
  // rendered as a bordered box inside a table cell, on two pages.
  const rule = /\.([a-z][\w-]*)(?![\w-])\s*(?=[,{])/g;
  const defined = new Map();
  const sheet = SHIPPED_UI.slice(0, SHIPPED_UI.indexOf("</style>"));
  for (const match of sheet.matchAll(rule)) {
    defined.set(match[1], (defined.get(match[1]) || 0) + 1);
  }
  for (const used of ["nodename"]) {
    assert.equal(
      defined.get(used),
      1,
      `.${used} is defined ${defined.get(used)} times; a label class must be its own`,
    );
  }
  // …and the cell must not reach for the multiply-defined one again.
  for (const cell of ["analysisNodeCell", "activityNodeCell"]) {
    assert.doesNotMatch(shippedSource(cell), /class="clnode"/);
    assert.match(shippedSource(cell), /class="nodename"/);
  }
});

test("the workspace and the Now playing table share one naming helper", () => {
  // Two spellings of "hostname or else the id" drift apart; the Now playing
  // cell and this one must agree about what a node is called.
  assert.match(shippedSource("analysisNodeCell"), /nodeLabel\(names,nodeId\)/);
  assert.match(shippedSource("analysisNodeDetail"), /nodeLabel\(names,nodeId\)/);
  assert.match(shippedSource("activityNodeCell"), /nodeLabel\(names,nodeId\)/);
});

main().catch((error) => {
  failures += 1;
  process.stdout.write(`FAIL the suite itself threw\n${error && error.stack}\n`);
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
