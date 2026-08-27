#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const INDEX = path.join(__dirname, "../../crates/plurxd/src/web/index.html");
const SHIPPED_UI = fs.readFileSync(INDEX, "utf8");

assert.doesNotMatch(
  SHIPPED_UI,
  /:is\(\[data-theme=panoptic\],\[data-theme=redline\]\)(?!\[data-theme=panovic\])/,
  "Panovic must receive the panoptic/redline cockpit design",
);

assert.match(SHIPPED_UI, /panovic:\{name:"Panovic",\s*darkOnly:true/);
assert.match(
  SHIPPED_UI,
  /panovic:\{[^]+?"--bg":"#000000"[^]+?"--accent2":"#8e4a1e"/,
);

process.stdout.write("PASS Panovic shares the cockpit family and contract palette\n");
