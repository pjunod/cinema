#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {shellSource} = require("./shell-source.js");
// The theme *tables* live in the <head> script, which runs before first paint
// — crates/plurxd/src/web/core/theme.js. The rules that dress them are in
// app.css, and the two claims below are about different halves: reading the
// selector out of the head script would be refusing a pattern that file
// structurally cannot contain, which is a guard that can never fail.
const SHIPPED_SHELL = shellSource();
const SHIPPED_UI = SHIPPED_SHELL.headScript;
const SHIPPED_CSS = SHIPPED_SHELL.css;

assert.doesNotMatch(
  SHIPPED_CSS,
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
