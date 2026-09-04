#!/usr/bin/env node
"use strict";

// CSS Grid's `1fr` is really `minmax(auto, 1fr)`: user content can become the
// track's minimum width and push a card off screen. Every shipped fraction must
// be the direct maximum of a minmax() with an explicit, non-intrinsic minimum.
// Containment roots such as .specs use zero; intentional tile grids retain
// their declared poster/card minimums. Keep this as one cross-surface contract
// instead of rediscovering the same overflow in each layout.

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

function functionRanges(value) {
  const ranges = [];
  const stack = [];
  const identifier = /[a-z-]/i;

  for (let index = 0; index < value.length; index += 1) {
    if (!identifier.test(value[index])) continue;
    const start = index;
    while (index + 1 < value.length && identifier.test(value[index + 1])) index += 1;
    const name = value.slice(start, index + 1).toLowerCase();
    let open = index + 1;
    while (/\s/.test(value[open] || "")) open += 1;
    if (value[open] !== "(") continue;
    stack.push({ name, open, commas: [] });
    index = open;

    while (index + 1 < value.length && stack.length) {
      index += 1;
      const character = value[index];
      if (character === "\"" || character === "'") {
        const quote = character;
        while (index + 1 < value.length) {
          index += 1;
          if (value[index] === "\\") index += 1;
          else if (value[index] === quote) break;
        }
      } else if (identifier.test(character)) {
        const nestedStart = index;
        while (index + 1 < value.length && identifier.test(value[index + 1])) index += 1;
        const nestedName = value.slice(nestedStart, index + 1).toLowerCase();
        let nestedOpen = index + 1;
        while (/\s/.test(value[nestedOpen] || "")) nestedOpen += 1;
        if (value[nestedOpen] === "(") {
          stack.push({ name: nestedName, open: nestedOpen, commas: [] });
          index = nestedOpen;
        }
      } else if (character === ",") {
        stack[stack.length - 1].commas.push(index);
      } else if (character === ")") {
        const range = stack.pop();
        range.close = index;
        ranges.push(range);
      }
    }
  }
  return ranges;
}

function isExplicitMinimum(value) {
  const floor = value.trim().toLowerCase();
  return floor.length > 0
    && !/(^|[^a-z-])(auto|min-content|max-content|fit-content)([^a-z-]|$)/.test(floor)
    && !/(?:\d+(?:\.\d+)?|\.\d+)fr\b/.test(floor);
}

function unboundedFractions(value) {
  const ranges = functionRanges(value);
  const unsafe = [];
  for (const token of value.matchAll(/(?:\d+(?:\.\d+)?|\.\d+)fr\b/gi)) {
    const minmax = ranges
      .filter((range) => range.name === "minmax"
        && range.open < token.index && token.index < range.close)
      .sort((left, right) => right.open - left.open)[0];
    if (!minmax || minmax.commas.length !== 1) {
      unsafe.push(token[0]);
      continue;
    }
    const floor = value.slice(minmax.open + 1, minmax.commas[0]);
    const ceiling = value.slice(minmax.commas[0] + 1, minmax.close).trim();
    if (!isExplicitMinimum(floor) || ceiling.toLowerCase() !== token[0].toLowerCase()) {
      unsafe.push(token[0]);
    }
  }
  return unsafe;
}

const CASES = [
  ["1fr", ["1fr"]],
  ["84px minmax(0, 1fr)", []],
  ["repeat(2, minmax(0px, 1.25fr))", []],
  ["repeat(auto-fill, minmax(var(--poster, 120px), 1fr))", []],
  ["minmax(min(100%, 215px), 1fr)", []],
  ["minmax(auto, 1fr)", ["1fr"]],
  ["minmax(min-content, 1fr)", ["1fr"]],
  ["minmax(max-content, 1fr)", ["1fr"]],
  ["minmax(fit-content(20rem), 1fr)", ["1fr"]],
  ["minmax(0, calc(1fr))", ["1fr"]],
  ["minmax(0, 1fr", ["1fr"]],
];
for (const [value, expected] of CASES) {
  assert.deepEqual(unboundedFractions(value), expected, `fraction policy fixture: ${value}`);
}

function columnSide(value) {
  let depth = 0;
  let quote = null;
  for (let index = 0; index < value.length; index += 1) {
    const character = value[index];
    if (quote) {
      if (character === "\\") index += 1;
      else if (character === quote) quote = null;
    } else if (character === "\"" || character === "'") {
      quote = character;
    } else if (character === "(") {
      depth += 1;
    } else if (character === ")") {
      depth -= 1;
    } else if (character === "/" && depth === 0) {
      return value.slice(index + 1);
    }
  }
  return "";
}

function gridDeclarations(styles) {
  return [...styles.matchAll(
    /\b(grid-template-columns|grid-auto-columns|grid-template|grid)\s*:\s*([^;}]+)/gi,
  )].map((declaration) => ({
    property: declaration[1].toLowerCase(),
    value: declaration[2],
    columns: /^(grid|grid-template)$/.test(declaration[1].toLowerCase())
      ? columnSide(declaration[2])
      : declaration[2],
  }));
}

assert.equal(
  gridDeclarations('.fixture{grid-template:"label value" / 84px 1fr}').length,
  1,
  "the grid-template shorthand must stay in the policy scan",
);
assert.deepEqual(
  unboundedFractions(gridDeclarations('.fixture{grid-template:"label value" / 84px 1fr}')[0].columns),
  ["1fr"],
  "a bare fraction in grid-template shorthand must fail",
);
assert.deepEqual(
  unboundedFractions(gridDeclarations(".fixture{grid:auto / 1fr}")[0].columns),
  ["1fr"],
  "a bare fraction in grid shorthand must fail",
);
assert.deepEqual(
  unboundedFractions(gridDeclarations(".fixture{grid:1fr / 84px}")[0].columns),
  [],
  "a fractional row must not be mistaken for a content column",
);
assert.deepEqual(
  unboundedFractions(gridDeclarations(".fixture{grid-auto-columns:1fr}")[0].columns),
  ["1fr"],
  "implicit columns must remain inside the policy",
);

const failures = [];
for (const asset of ASSETS) {
  const styles = stylesFrom(asset);
  for (const declaration of gridDeclarations(styles)) {
    const unsafe = unboundedFractions(declaration.columns);
    if (unsafe.length) failures.push(`${asset}: ${declaration.property}: ${declaration.value.trim()}`);
  }
}

assert.deepEqual(
  failures,
  [],
  `fractional content columns need an explicit minmax() floor:\n${failures.join("\n")}`,
);

const index = fs.readFileSync(path.join(ROOT, ASSETS[0]), "utf8");
assert.match(index, /\.specs\{[^}]*grid-template-columns:84px minmax\(0,1fr\)/);
assert.match(index, /\.specs dd\{[^}]*min-width:0[^}]*overflow-wrap:anywhere/);
assert.match(index, /\.specs \.trk\{[^}]*max-width:100%[^}]*overflow-wrap:anywhere/);
assert.match(index, /\.specs \.trkfold summary\{[^}]*width:fit-content[^}]*max-width:100%/);
assert.match(index, /\.clmetric strong\{[^}]*min-width:0[^}]*overflow-wrap:anywhere/);

const baseline = fs.readFileSync(path.join(ROOT, "scripts/ui-baseline"), "utf8");
for (const stress of ["stress_long_track_containment", "stress_cluster_metric_containment"]) {
  assert.equal(
    (baseline.match(new RegExp(`${stress}\\(page, name, layout_id\\)`, "g")) || []).length,
    2,
    `the Chromium containment stress ${stress} must be defined and called`,
  );
}

process.stdout.write("PASS shipped grid and long-track content stay inside their owners\n");
