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

// Derived from the fixture, not from the DOM it is checking: a reorder there
// fails here. The web is the surface with the most controls the row grammar
// has to name — `title_info` and `fullscreen` exist nowhere else — and the one
// that renders `info` in the bar rather than the transport row, which the
// fixture says in `surface_placement` rather than leaving to a comment.
const contract = require("../playback/player-input-contract.json");
const rowFor = (id) => {
  const row = contract.controls.rows.find((entry) => entry.id === id);
  assert.ok(row, `the contract has no ${id} row`);
  return row;
};
const WEB_ELEMENT_FOR_ITEM = {
  skip_back: "pbback",
  play_pause: "pbplay",
  skip_forward: "pbforward",
  audio: "pbaudio",
  subtitles: "pbsubs",
  quality: "pbquality",
  settings: "pbsettings",
  info: "statsbtn",
  pip: "pbpip",
  title_info: "pbinfo",
  fullscreen: "pbfs",
  close: "pbclose",
  airplay: "apbtn",
  spacer: null,
};

const barAt = playerLine.indexOf('id="pbar"');
assert.ok(barAt >= 0 && barAt < timelineAt, "the player lost its top bar");
const bar = playerLine.slice(barAt, timelineAt);
const transport = playerLine.slice(transportAt, playerLine.indexOf('id="pinfo"'));
const placement = rowFor("transport").surface_placement || {};
const movedOnDesktop = Object.entries(placement)
  .filter(([, where]) => where.desktop)
  .map(([item, where]) => ({ item, ...where.desktop }));

const rowItems = (rowId) => {
  const items = rowFor(rowId).items.filter(
    (item) => !movedOnDesktop.some((move) => move.item === item),
  );
  for (const move of movedOnDesktop) {
    if (move.row === rowId) items.splice(move.position, 0, move.item);
  }
  return items;
};

for (const [rowId, markup] of [["transport", transport], ["bar", bar]]) {
  const items = rowItems(rowId);
  let previous = -1;
  for (const item of items) {
    assert.ok(item in WEB_ELEMENT_FOR_ITEM, `the fixture's ${rowId} row gained ${item}`);
    const id = WEB_ELEMENT_FOR_ITEM[item];
    if (!id) continue;
    const current = markup.indexOf(`id="${id}"`);
    assert.ok(current >= 0, `${id} is missing from the ${rowId} row`);
    assert.ok(current > previous, `${id} is outside the shared ${rowId} row order`);
    previous = current;
  }
}

assert.match(source, /function settingsMenuHtml\(\)/);
assert.match(source, /Auto-skip intro and credits/);
assert.match(source, /Autoplay next episode/);
assert.match(source, /<div class="amsec">Audio sync<\/div>/);

process.stdout.write("PASS player DOM follows the shared row and dialog contract\n");
