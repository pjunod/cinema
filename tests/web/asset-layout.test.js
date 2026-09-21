"use strict";

// docs/clients/WEB-SHELL-LAYOUT.md is the map of the split web shell, and a map
// that has quietly stopped matching the ground is worse than no map: it makes
// the reader confident about a file that moved. So the document is checked
// against what is actually served, in both directions.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const {shellSource, WEB, SIDECARS} = require("./shell-source.js");

const DOC = path.join(__dirname, "../../docs/clients/WEB-SHELL-LAYOUT.md");
const doc = fs.readFileSync(DOC, "utf8");

// `| 7 | [`core/chrome.js`](…) | what it holds | 4526–4560 |`
const ROW = /^\| (\d+) \| \[`([^`]+)`\]\(([^)]+)\) \| (.+?) \| (.+?) \|$/gm;
const documented = [...doc.matchAll(ROW)].map((m) => ({
  number: Number(m[1]), file: m[2], link: m[3], holds: m[4], old: m[5],
}));

const shell = shellSource();
const served = [...shell.rows.style, ...shell.rows.head, ...shell.rows.body];

assert.deepEqual(
  documented.map((row) => row.file), served,
  "the layout table and the shell disagree about what is served, or in what order",
);

documented.forEach((row, index) => {
  assert.equal(row.number, index + 1, `${row.file} is numbered ${row.number}`);
  assert.ok(fs.existsSync(path.join(WEB, row.file)), `${row.file} is documented but is not a file`);
  assert.equal(
    row.link, `../../crates/plurxd/src/web/${row.file}`,
    `${row.file}'s link does not point at it`,
  );
  assert.ok(row.holds.trim().length > 20, `${row.file} has no description worth reading`);
  if (row.file === "hls.min.js") {
    assert.equal(row.old, "Vendored dependency.");
  } else {
    assert.match(row.old, /^(\d+–\d+|\*\*Relocated\.\*\*.*|\d+–\d+ \(less \d+–\d+\))$/,
      `${row.file} has no usable old line range: ${row.old}`);
  }
});

// The sidecars are the other half of the claim: §4 says which files are
// deliberately NOT in the table, and a sidecar quietly joining it would make
// that section a lie.
for (const sidecar of SIDECARS) {
  assert.ok(doc.includes(`\`${sidecar}\``) || doc.includes(sidecar),
    `${sidecar} is not named in the layout doc's "what is not in the table"`);
  assert.ok(!documented.some((row) => row.file === sidecar),
    `${sidecar} is a sidecar but appears in the served table`);
}

// Nothing under crates/plurxd/src/web/ that looks like a shell row is missing
// from the table — a file that exists, is not a sidecar, and is not served is
// either dead or a bug, and either way nobody should have to find out by
// accident.
const folders = ["core", "layouts", "detail", "player", "pages"];
const onDisk = folders
  .flatMap((folder) => fs.readdirSync(path.join(WEB, folder)).map((name) => `${folder}/${name}`))
  .concat(fs.readdirSync(WEB).filter((name) => name === "router.js" || name === "app.css"))
  .filter((name) => name.endsWith(".js") || name.endsWith(".css"));
assert.deepEqual(
  onDisk.filter((name) => !served.includes(name)), [],
  "a file in the split shell's folders is not served and not documented",
);

console.log(`PASS the layout doc, the shell and the tree agree on all ${served.length} files`);
