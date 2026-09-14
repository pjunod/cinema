"use strict";

// Shared DVR visibility contracts exercised against the JavaScript shipped in
// index.html. The suite grows with the implementation packages; these first
// cases protect the real recordings page envelope and its stale-data behavior.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const SHIPPED_UI = fs.readFileSync(INDEX, "utf8");
const DECLARATIONS = ["\nfunction ", "\nasync function "];

function shippedSource(name) {
  const start = DECLARATIONS.map((kind) => SHIPPED_UI.indexOf(`${kind}${name}(`))
    .find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  return (ends.length ? rest.slice(0, Math.min(...ends)) : rest).trimEnd();
}

const pageDecoder = new Function(`${shippedSource("dvrRecordingsPage")}; return dvrRecordingsPage;`)();

const overviewPresentation = new Function(
  `${shippedSource("dvrOverviewFresh")}
   ${shippedSource("dvrIndicatorText")}
   return {fresh:dvrOverviewFresh,text:dvrIndicatorText};`,
)();
const overviewCases = JSON.parse(fs.readFileSync(
  path.join(__dirname, "../playback/dvr-visibility-cases.json"), "utf8",
)).overviews;

test("shared overview cases preserve freshness and mixed phase words", () => {
  for (const row of overviewCases) {
    const receivedAt = 10_000;
    const fresh = overviewPresentation.fresh(row.body, receivedAt, receivedAt + row.client_age_ms);
    assert.equal(fresh, row.fresh, row.case);
    assert.equal(overviewPresentation.text(row.body, fresh), row.indicator || "", row.case);
  }
});

test("recording lists decode their page envelope and retain its cursor", () => {
  const row = {id: "rec-1", title: "Tidewater"};
  assert.deepEqual(pageDecoder({rows: [row], next: "cursor-1"}), {
    rows: [row], next: "cursor-1",
  });
  assert.deepEqual(pageDecoder({rows: []}), {rows: [], next: null});
});

test("a bare array or malformed page is an error, never an empty recording list", () => {
  assert.throws(() => pageDecoder([]), /recording list response is invalid/i);
  assert.throws(() => pageDecoder({rows: null}), /recording list response is invalid/i);
  assert.throws(() => pageDecoder({rows: [], next: 7}), /recording list cursor is invalid/i);
});

function libraryHarness(api) {
  const state = {
    status: null, schedule: null, reminders: [], library: null,
    libraryNext: null, libraryError: null, libraryLoading: false,
    rules: null, serial: 0,
  };
  let paints = 0;
  const load = new Function(
    "api", "LIVE_TV_DVR", "renderLiveTvChannels", "AbortSignal",
    `let PAGE_RENDER_GENERATION=1; const location={hash:"#/live-tv"};
     ${shippedSource("dvrRecordingsPage")}
     ${shippedSource("liveTvLoadRecordings")}
     ${shippedSource("liveTvLoadMoreRecordings")}
     return {load:liveTvLoadRecordings,more:liveTvLoadMoreRecordings};`,
  )(api, state, () => { paints += 1; }, {timeout: () => undefined});
  return {state, load, paints: () => paints};
}

test("a failed Saved refresh keeps the last successful page and marks it stale", async () => {
  const answers = [
    () => Promise.resolve({rows: [{id: "rec-1"}], next: "page-2"}),
    () => Promise.reject(new Error("owner unavailable")),
  ];
  const harness = libraryHarness(() => answers.shift()());
  await harness.load.load("library", "/dvr/recordings?state=done,partial");
  assert.deepEqual(harness.state.library.map((row) => row.id), ["rec-1"]);
  assert.equal(harness.state.libraryNext, "page-2");

  await harness.load.load("library", "/dvr/recordings?state=done,partial");
  assert.deepEqual(harness.state.library.map((row) => row.id), ["rec-1"]);
  assert.match(harness.state.libraryError, /owner unavailable/);
  assert.equal(harness.paints(), 2);
});

test("Load more follows the cursor and de-duplicates a repeated boundary row", async () => {
  const requests = [];
  const harness = libraryHarness((url) => {
    requests.push(url);
    if (requests.length === 1) return Promise.resolve({
      rows: [{id: "rec-2"}, {id: "rec-1"}], next: "older cursor",
    });
    return Promise.resolve({rows: [{id: "rec-1"}, {id: "rec-0"}], next: null});
  });
  await harness.load.load("library", "/dvr/recordings?state=done,partial");
  await harness.load.more();
  assert.deepEqual(harness.state.library.map((row) => row.id), ["rec-2", "rec-1", "rec-0"]);
  assert.equal(harness.state.libraryNext, null);
  assert.equal(requests[1], "/dvr/recordings?state=done,partial&after=older%20cursor");
});
