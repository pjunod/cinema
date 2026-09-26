"use strict";

// The type checker reads exactly the rows the browser loads.
//
// `scripts/web-types` checks what `crates/plurxd/src/web/jsconfig.json`
// lists, so a stale list is a gate that passes because it never read the new
// file (docs/clients/WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md §3.2).
// This regenerates the list from index.html and refuses any difference, and
// then checks the properties the generator exists to keep, so a generator
// edit that drops them fails here and not in review.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const {render, OUT, NOT_CHECKED} = require("../../scripts/web-jsconfig");
const {shellSource, SIDECARS} = require("./shell-source.js");

const committed = fs.readFileSync(OUT, "utf8");
assert.equal(committed, render(),
  "crates/plurxd/src/web/jsconfig.json is stale: run scripts/web-jsconfig and commit it");

const {files, compilerOptions} = JSON.parse(committed);
const {rows} = shellSource();

// Every served script row the checker is meant to read, in served order.
const expected = [...rows.head, ...rows.body].filter((row) => !NOT_CHECKED.includes(row));
assert.deepEqual(files.filter((f) => f.endsWith(".js")), expected,
  "jsconfig.json does not list the served rows in served order");
assert.ok(expected.length >= 60, `only ${expected.length} rows — the shell reader found too few`);

// The declarations for what the rows name but do not declare come first.
assert.equal(files[0], "types/globals.d.ts");

// No UMD sidecar and no vendored bundle: TypeScript would read each as a
// CommonJS module and hide the global it assigns.
for (const excluded of [...SIDECARS, ...NOT_CHECKED]) {
  assert.ok(!files.includes(excluded), `${excluded} must not be in jsconfig.json`);
}

// The script model: without `legacy` detection a row stops being a script
// whose top-level names are global, and every cross-row name goes missing.
assert.equal(compilerOptions.moduleDetection, "legacy");
assert.equal(compilerOptions.checkJs, true);
assert.equal(compilerOptions.noEmit, true);

console.log(`PASS jsconfig.json lists all ${expected.length} served rows the checker reads`);
