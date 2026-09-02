"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const source = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/index.html"),
  "utf8",
);
const playerLine = source.split("\n").find((line) => line.includes('id="player"'));
assert.ok(playerLine, "the shipped player markup must remain on a discoverable line");

const timelineAt = playerLine.indexOf('id="ptimeline"');
const transportAt = playerLine.indexOf('id="ptransport"');
assert.ok(timelineAt >= 0, "the player has no timeline row");
assert.ok(transportAt > timelineAt, "the timeline row must precede the transport row");

const timeline = playerLine.slice(timelineAt, transportAt);
assert.equal((timeline.match(/tabindex=/g) || []).length, 1, "the timeline row has more than one tab stop");
assert.match(timeline, /id="pseek"[^>]*tabindex="0"/, "the timeline itself must be its only tab stop");

const modalTag = playerLine.match(/^<div class="modal"[^>]*>/)?.[0] || "";
assert.match(modalTag, /role="dialog"/);
assert.match(modalTag, /aria-modal="true"/);
assert.match(modalTag, /aria-label="Player"/);

const menuButtons = [...playerLine.matchAll(/<button class="[^"]+"[^>]*onclick="toggleMenu\([^>]+>/g)]
  .map((match) => match[0]);
assert.ok(menuButtons.length >= 4, "the transport row lost an option menu");
for (const button of menuButtons) {
  assert.match(button, /aria-haspopup="menu"/, `menu opener lacks aria-haspopup: ${button}`);
  assert.match(button, /aria-expanded="false"/, `menu opener lacks aria-expanded: ${button}`);
}

// Derived from the fixture, not from the DOM it is checking. Two of the
// contract's transport items are rendered in the top bar on the web
// (`#statsbtn` and `#pbclose`) and two web-only controls have no fixture entry
// at all (`#pbinfo`, the title-info toggle, and `#pbfs`, fullscreen). That is a
// live question about the row — recorded in docs/PLAYER-INPUT-CONTRACT.md §6.4
// — so it is written down here as an exception with a name, and the ORDER of
// everything else comes from the fixture. A reorder there fails this test.
const contract = require("../playback/player-input-contract.json");
const transportRow = contract.controls.rows.find((row) => row.id === "transport");
assert.ok(transportRow, "the contract has no transport row");
const WEB_ELEMENT_FOR_ITEM = {
  skip_back: "pbback",
  play_pause: "pbplay",
  skip_forward: "pbforward",
  audio: "pbaudio",
  subtitles: "pbsubs",
  quality: "pbquality",
  settings: "pbsettings",
  pip: "pbpip",
  // Rendered in `#pbar`, not `#ptransport`, on the web today.
  info: null,
  close: null,
  spacer: null,
};
const transport = playerLine.slice(transportAt, playerLine.indexOf('id="pinfo"'));
let previous = -1;
for (const item of transportRow.items) {
  assert.ok(item in WEB_ELEMENT_FOR_ITEM, `the fixture's transport row gained ${item}`);
  const id = WEB_ELEMENT_FOR_ITEM[item];
  if (!id) continue;
  const current = transport.indexOf(`id="${id}"`);
  assert.ok(current > previous, `${id} is outside the shared transport row order`);
  previous = current;
}
// The two the contract does not know about are still where this test expects
// them, so moving one is a decision rather than an accident.
for (const [id, where] of [["statsbtn", playerLine], ["pbclose", playerLine], ["pbinfo", transport], ["pbfs", transport]]) {
  assert.ok(where.includes(`id="${id}"`), `${id} left the player without a fixture row to move to`);
}
for (const retired of ["skipbtn", "autonextbtn", "syncbtn", "qualbtn", "audiobtn", "subsbtn"]) {
  assert.doesNotMatch(playerLine, new RegExp(`id="${retired}"`), `${retired} must be folded into the transport menus`);
}
assert.match(source, /function settingsMenuHtml\(\)/);
assert.match(source, /Auto-skip intro and credits/);
assert.match(source, /Autoplay next episode/);
assert.match(source, /<div class="amsec">Audio sync<\/div>/);

process.stdout.write("PASS player DOM follows the shared row and dialog contract\n");
