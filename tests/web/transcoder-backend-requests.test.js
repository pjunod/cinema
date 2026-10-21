"use strict";

// The Transcoding backend card's two buttons must send what the server reads.
//
// `api()` serializes `body` itself. Optimize and Save handed it an already
// serialized string, so the server received a JSON *string* where
// `OptimizeTranscoderRequest` / `HardwarePreferenceRequest` expect an object,
// axum refused it with a plain-text 422, and the card could only toast
// "request failed" (reported 2026-10-11). The type on `api()`'s options makes
// scripts/web-types refuse a string body; this test runs the shipped buttons
// through the shipped `api()` and reads the bytes that reach the wire, so the
// card is held to the server's contract and not to a helper's.

const assert = require("node:assert/strict");
const test = require("node:test");

const {shellSource, apiPrelude} = require("./shell-source.js");
const SHIPPED_UI = shellSource().bodyScript;

const DECLARATIONS = ["\nfunction ", "\nasync function "];
const TERMINATORS = DECLARATIONS.concat([
  "\nconst ", "\nlet ", "\nwindow.", "\ndocument.", "\nsetInterval(", "\n// ",
]);
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) => SHIPPED_UI.indexOf(`${kind}${name}(`))
    .find((at) => at !== -1);
  assert.notEqual(start, undefined, `the shell no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = TERMINATORS.map((kind) => rest.indexOf(kind, 1)).filter((at) => at !== -1);
  return (ends.length ? rest.slice(0, Math.min(...ends)) : rest).trimEnd();
}

function reply(status, body) {
  const text = JSON.stringify(body);
  return {
    status,
    ok: status >= 200 && status < 300,
    headers: { get: () => null },
    text: () => Promise.resolve(text),
    json: () => Promise.resolve(body),
  };
}

// What serde does with the body before the handler runs: the request structs
// are objects with string fields, so anything else is a 422.
function serverReads(init, fields) {
  assert.equal(init.headers["content-type"], "application/json");
  const value = JSON.parse(init.body);
  assert.equal(typeof value, "object", `the server received ${init.body}, not an object`);
  assert.ok(value && !Array.isArray(value), `the server received ${init.body}, not an object`);
  for (const field of fields) {
    assert.equal(typeof value[field], "string", `${field} is missing from ${init.body}`);
  }
  return value;
}

function harness(answer) {
  const sent = [], toasts = [];
  const status = { textContent: "" };
  const field = { value: "qsv", dataset: { nodeId: "node-a" } };
  const card = { outerHTML: "", querySelectorAll:()=>[] };
  const elements = { "node-hwaccel": field, "node-hwaccel-status": status, "system-performance": card };
  const buttons = new Function(
    "PlaybackPolicy", "answer", "sent", "toasts", "elements",
    [
      "const API='/api/v1';let TOKEN='t';let AUTH_GENERATION=0;",
      "function logout(){}",
      "function fetch(url,init){sent.push({url,init});return Promise.resolve(answer);}",
      "function toast(message){toasts.push(message);}",
      "function esc(s){return String(s??'');}",
      shippedSource("systemPerformanceCard"),
      shippedSource("replaceSystemCard"),
      "const document={getElementById:(id)=>elements[id]||null};",
      "const SETTINGS_DATA={sys:{node_id:'node-a',hwaccel_pref:'qsv',hwaccel_requested:'qsv',encoders:{qsv:true},transcoder_optimization:{}}};",
      apiPrelude() + shippedSource("api"),
      shippedSource("optimizeNodeTranscoder"),
      shippedSource("saveNodeHwaccel"),
      "return {api, optimizeNodeTranscoder, saveNodeHwaccel, SETTINGS_DATA};",
    ].join("\n"),
  )({ parseStreamFailure: () => null }, answer, sent, toasts, elements);
  return { ...buttons, sent, toasts, status, card };
}

test("optimize posts node_id as an object", async () => {
  const h = harness(reply(200, { running: true }));
  const button = { disabled: false };
  await h.optimizeNodeTranscoder(button);
  assert.equal(h.sent.length, 1);
  assert.equal(h.sent[0].url, "/api/v1/system/transcoder");
  assert.equal(h.sent[0].init.method, "POST");
  assert.deepEqual(serverReads(h.sent[0].init, ["node_id"]), { node_id: "node-a" });
  assert.deepEqual(h.toasts, [], "a started benchmark raises no error toast");
  assert.equal(h.SETTINGS_DATA.sys.transcoder_optimization.running, true);
  assert.equal(button.disabled, true, "the button stays disabled while it runs");
  // The success path re-renders the real card in its running state; the
  // select it restores the draft into is still there, and the button says so.
  assert.match(h.card.outerHTML, /id="system-performance"/);
  assert.match(h.card.outerHTML, /disabled>Running comparison…<\/button>/);
});

test("save puts node_id and preference as an object", async () => {
  const h = harness(reply(200, { node_id: "node-a", preference: "qsv", restart_required: true }));
  const button = { disabled: false };
  await h.saveNodeHwaccel(button);
  assert.equal(h.sent.length, 1);
  assert.equal(h.sent[0].init.method, "PUT");
  assert.deepEqual(
    serverReads(h.sent[0].init, ["node_id", "preference"]),
    { node_id: "node-a", preference: "qsv" },
  );
  assert.deepEqual(h.toasts, ["Transcoding preference saved"]);
  assert.match(h.status.textContent, /^Saved\. Restart this node/);
  assert.equal(button.disabled, false);
});

test("a real optimize refusal reaches the card in the server's words", async () => {
  const h = harness(reply(409, { error: "A transcoder benchmark is already running." }));
  const button = { disabled: false };
  await h.optimizeNodeTranscoder(button);
  assert.deepEqual(h.toasts, ["A transcoder benchmark is already running."]);
  assert.equal(button.disabled, false);
});

test("api refuses a pre-serialized body before sending anything", async () => {
  const h = harness(reply(200, {}));
  for (const body of ['{"node_id":"node-a"}', new String("{}")]) {
    await assert.rejects(
      h.api("/system/transcoder", { method: "POST", body }),
      (error) => error instanceof TypeError && /not a JSON string/.test(error.message),
    );
  }
  assert.equal(h.sent.length, 0, "nothing reached the wire");
});

test("a refusal with no JSON sentence still names its status", async () => {
  const plain = {
    status: 422,
    ok: false,
    headers: { get: () => null },
    text: () => Promise.resolve("Failed to deserialize the JSON body into the target type"),
  };
  const h = harness(plain);
  const button = { disabled: false };
  await h.optimizeNodeTranscoder(button);
  assert.deepEqual(h.toasts, ["request failed (HTTP 422)"]);
  assert.equal(button.disabled, false);
});
