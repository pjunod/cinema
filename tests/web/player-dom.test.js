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

const transport = playerLine.slice(transportAt, playerLine.indexOf('id="pinfo"'));
const optionOrder = ["pbaudio", "pbsubs", "pbquality", "pbsettings", "pbinfo", "pbpip"];
let previous = -1;
for (const id of optionOrder) {
  const current = transport.indexOf(`id="${id}"`);
  assert.ok(current > previous, `${id} is outside the shared transport option order`);
  previous = current;
}
for (const retired of ["skipbtn", "autonextbtn", "syncbtn", "qualbtn", "audiobtn", "subsbtn"]) {
  assert.doesNotMatch(playerLine, new RegExp(`id="${retired}"`), `${retired} must be folded into the transport menus`);
}
assert.match(source, /function settingsMenuHtml\(\)/);
assert.match(source, /Auto-skip intro and credits/);
assert.match(source, /Autoplay next episode/);
assert.match(source, /<div class="amsec">Audio sync<\/div>/);

process.stdout.write("PASS player DOM follows the shared row and dialog contract\n");
