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

// Every field the rows write on the playback object is named.
//
// Most writers reach the player through a parameter (`p`, `player`,
// `openedPlayer`, …) that carries no type, so tsc never compared those writes
// with the typedef, and fields they add were missing from it while
// `PLAYER||{}` casts reported them as false TS2339s (PR #554 review, finding
// 1). Two checks, the second the stronger:
//
// 1. A write `p.x=` / `player.x=` / `PLAYER.x=` (and `+=`, `++`, `||=`, …)
//    anywhere under player/ names a field the typedef lists.
// 2. The whole shell compiled with every such parameter under player/ typed
//    `Player` (a JSDoc tag prepended on the function's own line, so line
//    numbers do not move) has no "does not exist on type 'Player'"
//    diagnostic, reads included: a field nothing writes is a dead read.
const named = new Set([...player.matchAll(/^ \* @property \{.*?\} \[?([A-Za-z_$][\w$]*)/gm)].map((m) => m[1]));
assert.equal(named.size, properties, "every @property line must parse to one field name");
const PLAYER_DIR = path.join(WEB, "player");
const WRITE = /\b(?:p|player|PLAYER|[a-z]+Player)\.([A-Za-z_$][\w$]*)\s*(?:=(?!=)|\+\+|--|(?:\+|-|\*|\/|\|\||&&|\?\?)=)/g;
const unnamedWrites = [];
for (const file of fs.readdirSync(PLAYER_DIR).filter((f) => f.endsWith(".js")).sort()) {
  fs.readFileSync(path.join(PLAYER_DIR, file), "utf8").split("\n").forEach((line, i) => {
    for (const m of line.matchAll(WRITE)) {
      if (!named.has(m[1])) unnamedWrites.push(`player/${file}:${i + 1} writes .${m[1]}`);
    }
  });
}
assert.deepEqual(unnamedWrites, [], "fields written on the player that the Player typedef does not name");

const PARAM = /^(?:p|player|[a-z]+Player)$/;
function typePlayerParameters(text) {
  return text.split("\n").map((line) => {
    const m = line.match(/^(\s*)((?:async\s+)?function\s*\*?\s*[\w$]+\s*\(([^)]*)\))/);
    if (!m) return line;
    const name = m[3].split(",").map((a) => a.split("=")[0].trim()).find((a) => PARAM.test(a));
    return name ? `${m[1]}/** @param {Player} ${name} */ ${line.slice(m[1].length)}` : line;
  }).join("\n");
}
const parsed = ts.getParsedCommandLineOfConfigFile(path.join(WEB, "jsconfig.json"), {},
  {...ts.sys, onUnRecoverableConfigFileDiagnostic: (d) => { throw new Error(ts.flattenDiagnosticMessageText(d.messageText, " ")); }});
const host = ts.createCompilerHost(parsed.options);
const readFile = host.readFile.bind(host);
let typedParameters = 0;
host.readFile = (fileName) => {
  const text = readFile(fileName);
  if (text == null || path.dirname(path.resolve(fileName)) !== PLAYER_DIR || !fileName.endsWith(".js")) return text;
  const typed = typePlayerParameters(text);
  typedParameters += (typed.match(/\/\*\* @param \{Player\} /g) || []).length - (text.match(/\/\*\* @param \{Player\} /g) || []).length;
  return typed;
};
const program = ts.createProgram(parsed.fileNames, parsed.options, host);
const missing = ts.getPreEmitDiagnostics(program)
  .filter((d) => /on type 'Player'/.test(ts.flattenDiagnosticMessageText(d.messageText, " ")))
  .map((d) => {
    const {line} = d.file.getLineAndCharacterOfPosition(d.start);
    return `${path.relative(WEB, d.file.fileName)}:${line + 1} ${ts.flattenDiagnosticMessageText(d.messageText, " ")}`;
  });
assert.ok(typedParameters >= 100, `only ${typedParameters} player parameters were typed; the rewrite stopped matching`);
assert.deepEqual(missing, [], "with every player parameter typed Player, these fields are not in the typedef");

console.log(`PASS the Player typedef names ${properties} fields, the checker holds reads to them, and ${typedParameters} typed player parameters find no field it misses`);
