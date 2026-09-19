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
