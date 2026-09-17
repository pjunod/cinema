#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const UI = fs.readFileSync(INDEX, "utf8");

function shippedFunction(name) {
  const marker = `\nfunction ${name}(`;
  const start = UI.indexOf(marker);
  assert.notEqual(start, -1, `index.html declares ${name}`);
  const rest = UI.slice(start + 1);
  const next = rest.indexOf("\nfunction ", 1);
  return (next === -1 ? rest : rest.slice(0, next)).trimEnd();
}

const esc = (value) => String(value)
  .replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
  .replaceAll('"', "&quot;").replaceAll("'", "&#39;");

test("content analysis failures retain distinct truthful operator guidance", () => {
  const info = new Function(`${shippedFunction("analysisErrorInfo")} return analysisErrorInfo;`)();
  const timeout = info("index_budget_exceeded");
  const unverified = info("index_completion_unverified");
  const shortfall = info("index_video_shortfall");
  const legacy = info("truncated");

  assert.match(timeout.title, /timed out/i);
  assert.match(timeout.next, /retry cycle/i);
  assert.match(unverified.detail, /could not prove/i);
  assert.doesNotMatch(unverified.detail, /damaged|corrupt/i);
  assert.match(shortfall.detail, /process exited successfully/i);
  assert.match(legacy.detail, /did not preserve enough detail/i);
  assert.equal(new Set([timeout.title, unverified.title, shortfall.title, legacy.title]).size, 4);
});

test("copied diagnostics expose retry and selected-video evidence without interpreting hostile text", () => {
  const diagnosticText = new Function(
    `${shippedFunction("analysisStateLabel")}
     ${shippedFunction("analysisDisposition")}
     ${shippedFunction("analysisErrors")}
     ${shippedFunction("analysisAttemptHistory")}
     ${shippedFunction("analysisNodeDetail")}
     ${shippedFunction("analysisDiagnosticText")}
     return analysisDiagnosticText;`,
  )();
  const hostile = "<img src=x onerror=alert(1)>";
  const text = diagnosticText({
    file_id: "42",
    title: hostile,
    state: "failed",
    disposition: "attention",
    attempts: 2,
    job_error_code: "index_completion_unverified",
    job_attempt_errors: ["index_probe_timeout", "index_completion_unverified"],
    index_retry_deadline_ms: Date.UTC(2026, 8, 20),
    index_diagnostic: {
      selected_stream: 3,
      expectation_provenance: "stream_ticks",
      expected_ms: 120000,
      covered_ms: 117000,
      stderr_tail: [hostile],
    },
  }, {});

  assert.match(text, /Selected stream: 3/);
  assert.match(text, /Timing provenance: stream_ticks/);
  assert.match(text, /Attempt history: index_probe_timeout, index_completion_unverified/);
  assert.match(text, /Retry deadline:/);
  assert.ok(text.includes(hostile), "plain-text copy preserves evidence without executing markup");
});

test("Developer enablement is authoritative and readiness remains advisory", () => {
  const card = new Function(
    "esc", "setCard", "cardHead", "togRow", "devReq", "setCardFoot",
    `${shippedFunction("contentAnalysisEnableCard")} return contentAnalysisEnableCard;`,
  )(
    esc,
    (html) => html,
    (title, detail, badge) => `<h2>${title}</h2><p>${detail}</p>${badge}`,
    (id, title, detail, checked) => `<input id="${id}" type="checkbox"${checked ? " checked" : ""}><b>${title}</b><p>${detail}</p>`,
    (_readiness, _feature, key, title, detail) => `<div data-key="${key}"><b>${title}</b><p>${detail}</p></div>`,
    (save) => `<button data-save="${save}">Save</button>`,
  );

  const html = card({ vod_index_cluster_cache: true }, {});
  assert.match(html, /id="ca-enabled"[^>]*checked/);
  assert.match(html, /advisory/i);
  assert.match(html, /never disable or override/i);
  assert.match(html, /compatibility_inventory/);
  assert.doesNotMatch(shippedFunction("contentAnalysisEnableCard"), /disabled\s*=|\.disabled/);

  const save = shippedFunction("saveContentAnalysisDeveloper");
  assert.match(save, /vod_index_cluster_cache:document\.getElementById\("ca-enabled"\)\.checked/);
  assert.doesNotMatch(save, /readiness|requirements|compatibility_inventory/);
});
