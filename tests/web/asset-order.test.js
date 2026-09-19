"use strict";

// The served order of the web assets is a load order, and this refuses one that
// cannot load. See tests/web/asset-graph.js for what it reads and why a runtime
// load is not enough on its own.

const assert = require("node:assert/strict");
const {analyze, servedSources} = require("./asset-graph.js");

const shipped = analyze();

// ---- what is served today ---------------------------------------------------

{
  assert.deepEqual(
    shipped.missingPrologue, [],
    "every web asset needs its own \"use strict\"; — a directive applies per "
    + "script, and in sloppy mode a write to a frozen table like STATS_ROWS is "
    + "a silent no-op instead of a TypeError",
  );
}

{
  assert.deepEqual(
    shipped.duplicates, [],
    "two rows declare the same top-level name; one of them silently wins",
  );
}

{
  const named = shipped.forwardRefs.map(
    (f) => `${f.row} names ${f.name} at load, but it is declared in ${f.declaredIn}`
      + (f.via.length ? ` (via ${f.via.join(" > ")})` : ""),
  );
  assert.deepEqual(named, [],
    "a row runs something at load that names a binding no earlier row has "
    + "declared yet — the app would die on a blank page");
}

console.log(`PASS ${shipped.rows.length} web assets load in an order that resolves`);

// ---- and the proof that this can fail --------------------------------------

const served = servedSources();
const at = (path) => {
  const index = served.findIndex((row) => row.path === path);
  assert.notEqual(index, -1, `${path} is not served`);
  return index;
};

{
  // The one relocation the split had to make. Put it back where its banner
  // was — before the functions it names are declared — and this must say so.
  const reordered = served.slice();
  const [block] = reordered.splice(at("layouts/register-classic.js"), 1);
  reordered.splice(reordered.findIndex((r) => r.path === "layouts/renderers.js"), 0, block);
  const broken = analyze(reordered);
  const names = broken.forwardRefs
    .filter((f) => f.row === "layouts/register-classic.js")
    .map((f) => f.name);
  assert.ok(names.includes("classicItemBody"),
    `moving register-classic before renderers must name classicItemBody, got ${names}`);
  console.log(`PASS register-classic before renderers fails, naming ${names.join(", ")}`);
}

{
  // …and a swap that genuinely does not matter must stay quiet, or the gate is
  // just "do not touch anything" wearing a parser.
  const swapped = served.slice();
  const a = at("core/cards.js");
  const b = at("core/lightbox.js");
  [swapped[a], swapped[b]] = [swapped[b], swapped[a]];
  assert.deepEqual(analyze(swapped).forwardRefs, [],
    "swapping cards and lightbox changes nothing and must not fail");
  console.log("PASS swapping two independent rows stays green");
}

{
  const duped = served.map((row) => row.path === "core/lightbox.js"
    ? {...row, source: `${row.source}\nfunction fmtDate(s){ return s; }\n`}
    : row);
  const found = analyze(duped).duplicates.map((d) => d.name);
  assert.deepEqual(found, ["fmtDate"], `a second fmtDate must be refused, got ${found}`);
  console.log("PASS a second top-level fmtDate is refused");
}

{
  const sloppy = served.map((row) => row.path === "pages/settings.js"
    ? {...row, source: row.source.replace('"use strict";\n', "")}
    : row);
  assert.deepEqual(analyze(sloppy).missingPrologue, ["pages/settings.js"],
    "a row that lost its prologue must be refused");
  console.log("PASS a row without its \"use strict\"; prologue is refused");
}

// ---- the shapes a forward reference can take --------------------------------

// A gate is only as good as the constructions it sees through, and this one
// reads code rather than running it, so every shape it does NOT model is a hole
// it will never report. Each case below plants a reference to `setPageTimer`
// (declared in `router.js`, the last row) in `core/app.js`, the first. The
// negatives matter as much as the positives: a gate that fires on a click
// handler is a gate people route around.
//
// Five of these were holes when this file was first written — an immediately
// invoked `.map`/`.forEach`/`.sort` callback, a `class extends` clause, a
// default parameter, a destructuring default — and one of them, an immediate
// callback inside a branch the load smoke does not take, passed BOTH gates
// while shipping a blank page to every touch device.
{
  const POSITIVE = {
    "a direct reference": "const probe = setPageTimer;",
    "a direct call": 'setPageTimer("x");',
    "an alias, then a call": "const f = setPageTimer; f();",
    "an IIFE": "(function(){ setPageTimer(); })();",
    "a .map callback": '["a"].map(function(x){ return setPageTimer(x); });',
    "a .forEach callback": '["a"].forEach(x => setPageTimer(x));',
    "a .sort comparator": '["a","b"].sort((x,y) => setPageTimer(x));',
    "Array.from's callback": "Array.from([1], x => setPageTimer(x));",
    "an immediate callback inside a branch":
      'if (navigator.maxTouchPoints > 0) { ["a"].forEach(function(i){ setPageTimer(i); }); }',
    "a class extends clause": "class Probe extends setPageTimer {}",
    "a class computed key": "class Probe { [setPageTimer()](){} }",
    "a class static block": "class Probe { static { setPageTimer(); } }",
    "a default parameter": "function g(a = setPageTimer){ return a; } g();",
    "a destructuring default": "const {q = setPageTimer} = {};",
    "a call two hops away": "function a1(){ return a2(); } function a2(){ return setPageTimer(); } a1();",
    "a spread": "const s = [...[setPageTimer]];",
    "a tagged template": "String.raw`${setPageTimer}`;",
    "a top-level loop": "for (let i = 0; i < 0; i++) { setPageTimer(); }",
    "a try block": "try { setPageTimer(); } catch (e) {}",
  };
  const NEGATIVE = {
    "an event handler": 'window.addEventListener("x", function(){ setPageTimer(); });',
    "a setTimeout callback": "setTimeout(function(){ setPageTimer(); }, 0);",
    "a .then continuation": "Promise.resolve().then(function(){ setPageTimer(); });",
    "a function merely named": "window.zz = function(){ return setPageTimer(); };",
    "a function nobody calls": "function never(){ return setPageTimer(); }",
  };
  const plant = (code) => analyze(served.map((row) => row.path === "core/app.js"
    ? {...row, source: `${row.source}\n${code}\n`} : row))
    .forwardRefs.filter((f) => f.row === "core/app.js");

  for (const [shape, code] of Object.entries(POSITIVE)) {
    const names = plant(code).map((f) => f.name);
    assert.ok(names.includes("setPageTimer"),
      `${shape} is a load-time forward reference this gate does not see: ${code}`);
  }
  for (const [shape, code] of Object.entries(NEGATIVE)) {
    assert.deepEqual(plant(code), [],
      `${shape} runs after every row is in; failing on it makes this gate a nuisance`);
  }
  console.log(`PASS ${Object.keys(POSITIVE).length} shapes of forward reference are caught, `
    + `${Object.keys(NEGATIVE).length} deferred shapes are not`);
}
