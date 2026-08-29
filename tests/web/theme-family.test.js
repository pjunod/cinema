#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const SHIPPED_UI = fs.readFileSync(INDEX, "utf8");

assert.doesNotMatch(
  SHIPPED_UI,
  /:is\(\[data-theme=panoptic\],\[data-theme=redline\],\[data-theme=panovic\]\)(?!\[data-theme=copper\])/,
  "Burnt Pumpkin and Copper must both receive the cockpit design",
);

assert.match(SHIPPED_UI, /panovic:\{name:"Burnt Pumpkin",\s*darkOnly:true/);
assert.match(
  SHIPPED_UI,
  /panovic:\{[^]+?"--bg":"#000000"[^]+?"--accent":"#e8871e"[^]+?"--accent2":"#81420b"[^]+?"--accent-dim":"rgba\(232,135,30,.12\)"[^]+?"--hairline":"rgba\(255,255,255,.06\)"/,
);
assert.match(SHIPPED_UI, /copper:\{name:"Copper",\s*darkOnly:true/);
assert.match(
  SHIPPED_UI,
  /copper:\{[^]+?"--bg":"#000000"[^]+?"--accent":"#cf7643"[^]+?"--accent2":"#70402b"[^]+?"--accent-dim":"rgba\(207,118,67,.12\)"[^]+?"--hairline":"rgba\(255,255,255,.06\)"/,
);

process.stdout.write("PASS Burnt Pumpkin and Copper share the cockpit family and contract palettes\n");
