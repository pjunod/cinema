"use strict";

// Reads the split web shell the way a browser reads it.
//
// Why this exists: until 2026-09 `crates/plurxd/src/web/index.html` was one
// 23,901-line file and twenty-two tests sliced functions straight out of it by
// name. `docs/clients/WEB-SHELL-SPLIT-PLAN.md` cut it into sixty-two files, so
// the string those tests want is no longer a file — it is the concatenation of
// the shell's body rows in served order. This is the one place that knows how
// to build it, so a new file joins every slicing test the moment it is served.
//
// The shell's own tag order is the source of truth, because that is what a
// browser obeys; `WEB_ASSETS` in `crates/plurxd/src/http/web.rs` is pinned to
// it by a unit test there, not the other way round.
//
//   const {shellSource} = require("./shell-source.js");
//   const {bodyScript} = shellSource();

const fs = require("node:fs");
const path = require("node:path");

const WEB = path.join(__dirname, "../../crates/plurxd/src/web");

// The seven pre-existing sidecars and reader.css keep their own routes and are
// not part of the split table (plan §2): they are UMD modules `require()`d by
// path from tests, and three of them are bundled into the native clients by
// path. A `<script src>` row naming one of these is not a body row.
const SIDECARS = Object.freeze([
  "cluster-panel.js", "playback-policy.js", "playback-control.js", "reader.js",
  "hls.min.js", "live-tv.js", "library-channels.js", "reader.css",
]);

const SCRIPT_ROW = /^<script src="\/assets\/([^"?]+)"><\/script>$/;
const STYLE_ROW = /^<link rel="stylesheet" href="\/assets\/([^"?]+)">$/;

function read(rel) {
  return fs.readFileSync(path.join(WEB, rel), "utf8");
}

function shellSource() {
  const html = read("index.html");
  const lines = html.split("\n");
  const headEnd = lines.findIndex((l) => l === "</head>");
  if (headEnd < 0) throw new Error("shell-source: index.html has no </head>");

  const styles = [];
  const headScripts = [];
  const bodyScripts = [];
  const sidecars = [];
  lines.forEach((line, i) => {
    const style = STYLE_ROW.exec(line);
    if (style) {
      (SIDECARS.includes(style[1]) ? sidecars : styles).push(style[1]);
      return;
    }
    const script = SCRIPT_ROW.exec(line);
    if (!script) return;
    if (SIDECARS.includes(script[1])) sidecars.push(script[1]);
    else (i < headEnd ? headScripts : bodyScripts).push(script[1]);
  });

  if (styles.length !== 1) {
    throw new Error(`shell-source: expected one stylesheet row, got ${styles.join(", ")}`);
  }
  if (headScripts.length !== 1) {
    throw new Error(`shell-source: expected one <head> script row, got ${headScripts.join(", ")}`);
  }

  const files = [...styles, ...headScripts, ...bodyScripts]
    .map((rel) => ({path: rel, source: read(rel)}));
  const sourceOf = (rel) => files.find((f) => f.path === rel).source;

  return {
    html,
    css: sourceOf(styles[0]),
    headScript: sourceOf(headScripts[0]),
    // The exact string the pre-split tests used to read out of index.html.
    // Rows carry their own `"use strict";` prologue; concatenated, each one is
    // a no-op expression statement between two files, which is harmless inside
    // any slice that happens to span it.
    bodyScript: bodyScripts.map(sourceOf).join(""),
    // Everything the browser ends up with, in document order: the markup and
    // its tags, the stylesheet they name, the head script, then the body rows.
    // This is the whole of what `index.html` used to be, for assertions that
    // read across all of it — an absence checked against one half is not an
    // absence. It is NOT the string to use for an ordering claim: everything
    // here precedes everything after it whatever the shell says, so compare
    // `rows` positions instead.
    get everything() {
      return [html, sourceOf(styles[0]), sourceOf(headScripts[0]),
        ...bodyScripts.map(sourceOf)].join("\n");
    },
    rows: {style: styles, head: headScripts, body: bodyScripts, sidecars},
    files,
  };
}

module.exports = {shellSource, WEB, SIDECARS};
