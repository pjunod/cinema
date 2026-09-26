"use strict";

// The Player typedef makes the checker see the playback object's shape.
//
// Before it, `PLAYER` was typed from its idle initialiser, which in a
// JavaScript file TypeScript treats as an open ("expando") object: a
// misspelled field read nothing and reported nothing. With `@typedef Player`
// in player/player.js and `@returns {Player}` on buildPlayer(), a field the
// typedef does not name is a diagnostic, and a new field in buildPlayer()'s
// literal must be named there first
// (docs/clients/WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md §3.4).
//
// This compiles a probe against the typedef with the pinned TypeScript that
// scripts/web-types installs, so it runs after that script in make web-check.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const ROOT = path.join(__dirname, "../..");
const WEB = path.join(ROOT, "crates/plurxd/src/web");
const TS = path.join(ROOT, "tools/web-types/node_modules/typescript");

if (!fs.existsSync(path.join(TS, "package.json"))) {
  console.error("FAIL the pinned TypeScript is not installed — run scripts/web-types first");
  process.exit(1);
}
const ts = require(TS);

const player = fs.readFileSync(path.join(WEB, "player/player.js"), "utf8");
const properties = (player.match(/^ \* @property /gm) || []).length;
assert.ok(properties >= 94, `the Player typedef names ${properties} fields; the plan requires every one (≥ 94)`);
assert.match(player, /^\/\*\* @type \{Player\} \*\/\nlet PLAYER=\{/m, "let PLAYER is not typed Player");
assert.match(fs.readFileSync(path.join(WEB, "player/decode-tiers.js"), "utf8"),
  /^\/\*\* @returns \{Player\}[^\n]*\nfunction buildPlayer\(/m, "buildPlayer() is not typed Player");

const {compilerOptions} = JSON.parse(fs.readFileSync(path.join(WEB, "jsconfig.json"), "utf8"));
const options = ts.convertCompilerOptionsFromJson(compilerOptions, WEB).options;

function probe(body) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "player-typedef-"));
  const file = path.join(dir, "probe.js");
  fs.writeFileSync(file, `"use strict";\nfunction probe(){\n${body}\n}\n`);
  try {
    const program = ts.createProgram(
      [path.join(WEB, "types/globals.d.ts"), path.join(WEB, "player/player.js"),
        path.join(WEB, "player/decode-tiers.js"), file],
      options);
    return ts.getPreEmitDiagnostics(program, program.getSourceFile(file))
      .map((d) => `TS${d.code} ${ts.flattenDiagnosticMessageText(d.messageText, " ")}`);
  } finally {
    fs.rmSync(dir, {recursive: true, force: true});
  }
}

// A misspelled field, read straight off PLAYER and off an alias of it.
assert.deepEqual(probe("return PLAYER.sesionId;"),
  ["TS2551 Property 'sesionId' does not exist on type 'Player'. Did you mean 'sessionId'?"]);
assert.deepEqual(probe("const p=PLAYER; p.wantsPlaybak=true;"),
  ["TS2551 Property 'wantsPlaybak' does not exist on type 'Player'. Did you mean 'wantsPlayback'?"]);
// A named field is fine, including one only a later row assigns.
assert.deepEqual(probe("PLAYER.hlsRetryUsed=1; return PLAYER.sessionId;"), []);
// A field typed as a number refuses a string.
assert.equal(probe("PLAYER.offset='12';").length, 1);

console.log(`PASS the Player typedef names ${properties} fields and the checker holds reads to them`);
