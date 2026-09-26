"use strict";

// The Activity page's Processes table (plan P-02 §3.2), tested against the
// shipped script: every child the server runs is listed with what it is,
// why it runs at its priority, what the kernel reports, and a stop.

const assert = require("node:assert/strict");

const {shellSource} = require("./shell-source.js");
const SHIPPED_UI = shellSource().bodyScript;

const DECLARATIONS = ["\nfunction ", "\nasync function "];
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) =>
    SHIPPED_UI.indexOf(`${kind}${name}(`),
  ).find((at) => at !== -1);
  assert.notEqual(start, undefined, `the shell no longer declares ${name}`);
  const rest = SHIPPED_UI.slice(start + 1);
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter(
    (at) => at !== -1,
  );
  const end = ends.length ? Math.min(...ends) : -1;
  return (end === -1 ? rest : rest.slice(0, end)).trimEnd();
}

const render = new Function(
  "return (function(){" +
    ["esc", "fmtAgo", "activityProcessesHtml"].map(shippedSource).join("\n") +
    "\nreturn activityProcessesHtml;})()",
)();

const now = Date.now();
const rows = [
  {
    pid: 4242,
    class: "background",
    purpose: "library scan thumbnail",
    reason: "nobody is waiting on it, so it yields to playback",
    program: "ffmpeg",
    started_at_ms: now - 90_000,
    requested: {nice: 15, io_level: 7, oom_score_adj: 800},
    observed: {nice: 15, io_class: "best_effort", io_level: 7, oom_score_adj: 800},
    applied: true,
    stoppable: true,
  },
  {
    pid: 4343,
    class: "realtime",
    purpose: "playback transcode <b>",
    reason: "a viewer or a recording is waiting on it",
    program: "ffmpeg",
    started_at_ms: now - 5_000,
    requested: {nice: 5, io_level: 4, oom_score_adj: 500},
    observed: {nice: 0, io_class: "best_effort", io_level: 4, oom_score_adj: 500},
    applied: false,
    stoppable: false,
  },
];

const html = render(rows);
assert.match(html, /<h2 class="section">Processes<\/h2>/);
assert.match(html, /library scan thumbnail/, "what it is");
assert.match(html, /nobody is waiting on it, so it yields to playback/, "why");
assert.match(html, /Background <span class="muted">· nice 15 · I\/O 7 · OOM \+800<\/span>/, "the class and what the kernel reports");
assert.match(html, /Playback <span class="muted">· nice 0 · I\/O 4 · OOM \+500<\/span> <span class="pill warn">priority not applied<\/span>/, "a refused priority is said out loud");
assert.match(html, /ffmpeg <span class="muted">· pid 4242<\/span>/);
assert.match(html, /onclick="stopProcess\(4242,&quot;library scan thumbnail&quot;\)"/, "a way to stop it");
assert.equal((html.match(/stopProcess\(/g) || []).length, 1, "no stop where the server cannot stop safely");
assert.doesNotMatch(html, /transcode <b>/, "purposes are escaped");

assert.equal(render(undefined), "", "non-admins get no field and no section");
assert.match(render([]), /No child processes running on this server/);

const painter = shippedSource("paintActivityBody");
assert.match(painter, /activityProcessesHtml\(d\.processes\)/, "the Activity page draws the table");
const stop = shippedSource("stopProcess");
assert.match(stop, /api\(`\/activity\/processes\/\$\{pid\}`,\{method:"DELETE"\}\)/);

console.log("activity processes: ok");
