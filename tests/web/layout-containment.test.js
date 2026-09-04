#!/usr/bin/env node
"use strict";

// CSS Grid's `1fr` is really `minmax(auto, 1fr)`: user content can become the
// column's minimum width and push a card off screen. Shipped content columns
// use an explicit zero floor so their contents wrap inside the component that
// owns them. Keep this as one cross-surface contract instead of rediscovering
// the same overflow in each layout.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const ROOT = path.join(__dirname, "../..");
const ASSETS = [
  "crates/plurxd/src/web/index.html",
  "crates/plurxd/src/web/reader.css",
  "crates/plurxd/src/web/offline-reader.html",
];

function stylesFrom(relativePath) {
  const source = fs.readFileSync(path.join(ROOT, relativePath), "utf8");
  if (relativePath.endsWith(".css")) return source;
  return [...source.matchAll(/<style\b[^>]*>([\s\S]*?)<\/style>/gi)]
    .map((match) => match[1])
    .join("\n");
}

function unboundedFractions(value) {
  const calls = [];
  const unsafe = [];
  const tokens = value.matchAll(
    /([a-z-]+)\s*\(|\)|(?:\d+(?:\.\d+)?|\.\d+)fr\b/gi,
  );
  for (const token of tokens) {
    if (token[1]) {
      calls.push(token[1].toLowerCase());
    } else if (token[0] === ")") {
      calls.pop();
    } else if (!calls.includes("minmax")) {
      unsafe.push(token[0]);
    }
  }
  return unsafe;
}

const failures = [];
for (const asset of ASSETS) {
  const styles = stylesFrom(asset);
  for (const declaration of styles.matchAll(/grid-template-columns\s*:\s*([^;}]+)/gi)) {
    const unsafe = unboundedFractions(declaration[1]);
    if (unsafe.length) failures.push(`${asset}: ${declaration[1].trim()}`);
  }
}

assert.deepEqual(
  failures,
  [],
  `fractional content columns need an explicit minmax() floor:\n${failures.join("\n")}`,
);

const index = fs.readFileSync(path.join(ROOT, ASSETS[0]), "utf8");
assert.match(index, /\.specs dd\{[^}]*min-width:0/);
assert.match(index, /\.specs \.trk\{[^}]*max-width:100%[^}]*overflow-wrap:anywhere/);
assert.match(index, /\.specs \.trkfold summary\{[^}]*width:fit-content[^}]*max-width:100%/);

const baseline = fs.readFileSync(path.join(ROOT, "scripts/ui-baseline"), "utf8");
assert.match(baseline, /def stress_long_track_containment\(page, name, layout_id\):/);
assert.equal(
  (baseline.match(/stress_long_track_containment\(page, name, layout_id\)/g) || []).length,
  2,
  "the Chromium containment stress must be defined and called",
);

process.stdout.write("PASS shipped grid and long-track content stay inside their owners\n");
