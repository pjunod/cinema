"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {shellSource} = require("./shell-source.js");
// The markup, for the player's own DOM, and the body rows, for the code
// that paints into it — the two halves index.html used to hold together.
const {html, bodyScript} = shellSource();
const source = `${html}\n${bodyScript}`;
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
  larger: "pblarger",
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

// Execute the production presentation functions: a cached VOD stream must
// keep unknown player dimensions distinct from a known original picture.
const infoStart = source.indexOf("function playbackInfoOverview(");
const infoEnd = source.indexOf("function patchPlaybackInfoRows(", infoStart);
const info = new Function("esc", source.slice(infoStart, infoEnd) + "\nreturn {playbackInfoOverview, playbackInfoMarkup};")(
  value => String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll('"', "&quot;"),
);
const missingPicture = info.playbackInfoOverview({
  method: "Transcode · cached", player_state: "Playing", decode_resolution: "Not reported",
  source_resolution: "3840×2160", client_loaded: "12.0 s", stalls: "0",
});
assert.match(missingPicture, /pi-picture[^]*?Playing resolution[^]*?<strong>Not reported<\/strong>/);
assert.match(missingPicture, /Original file[^]*?<strong>3840×2160<\/strong>/);
const infoFields = require("../playback/playback-info-fields.json").fields;
const diagnosticMarkup = info.playbackInfoMarkup("debug", infoFields.map(f => ({...f, value: "observation"})), "", "");
for (const field of infoFields) assert.equal(diagnosticMarkup.split(`data-stats-id="${field.id}"`).length - 1, 1, `${field.id} remains reachable exactly once`);
assert.match(diagnosticMarkup, /<summary>Picture &amp; sound<\/summary>/);
assert.match(diagnosticMarkup, /<summary>Session &amp; history<\/summary>/);
process.stdout.write("PASS playback information keeps unknown picture size and complete diagnostics\n");

const originalAudio = info.playbackInfoOverview({method: "Transcode", source_audio: "DTS · 5.1"});
assert.match(originalAudio, /Stream audio track[^]*?<strong>Not reported<\/strong>/);
assert.match(originalAudio, /Original audio track[^]*?<strong>DTS · 5.1<\/strong>/);
assert.match(source, /client_loaded:clientLoadedSeconds==null\?"Not reported"/);

// ---------------------------------------------------------------------------
// F-web-13: the two transports that are not a keyboard.
//
// Both live inside the `player-input-adapter` fence region in player/stats.js,
// which `scripts/player-input-fence` keeps as the player's ONE key adapter.
// These execute the shipped functions rather than matching their text.

function shipped(name) {
  let at = bodyScript.indexOf(`\nfunction ${name}(`);
  if (at < 0) at = bodyScript.indexOf(`\nasync function ${name}(`);
  assert.ok(at >= 0, `the shell no longer declares ${name}`);
  const rest = bodyScript.slice(at + 1);
  const ends = ["\nfunction ", "\nasync function ", "\nconst ", "\nlet ", "\nwindow.", "\ndocument."]
    .map((kind) => rest.indexOf(kind, 1)).filter((where) => where !== -1);
  return ends.length ? rest.slice(0, Math.min(...ends)) : rest;
}

// A television's Back button. Escape is a keyboard's answer and no remote
// sends it; each TV platform has its own, and until these landed the page
// navigated away from the player — on a TV, out of the app.
{
  const contractInput = new Function(`${shipped("playerContractInput")}\nreturn playerContractInput;`)();
  for (const [event, why] of [
    [{ key: "Escape" }, "a desktop keyboard"],
    [{ key: "GoBack" }, "Fire TV Silk and Android TV name the key"],
    [{ key: "BrowserBack" }, "desktop Chromium's own back key"],
    [{ key: "Unidentified", keyCode: 10009 }, "Tizen"],
    [{ key: "Unidentified", keyCode: 461 }, "webOS"],
  ]) {
    assert.equal(contractInput(event, "hidden"), "back", `${why} must reach the contract as back`);
  }
  // Nothing else became Back on the way.
  assert.equal(contractInput({ key: "Unidentified", keyCode: 462 }, "hidden"), null);
  assert.equal(contractInput({ key: "Backspace" }, "hidden"), null);
  process.stdout.write("PASS every television's Back key reaches the contract as back\n");
}

// MediaSession: the OS media keys, a headset button, the lock screen.
{
  const build = (navigatorStub, video) => {
    const toggles = [];
    const nudges = [];
    const run = new Function(
      "navigator", "document", "togglePlay", "nudge", "autoNextOn", "playNextEpisode",
      [
        shipped("setPlayerMediaAction"),
        shipped("installPlayerMediaSession"),
        shipped("clearPlayerMediaSession"),
        "return {installPlayerMediaSession, clearPlayerMediaSession};",
      ].join("\n"),
    )(
      navigatorStub,
      { getElementById: () => video },
      () => toggles.push(video.paused ? "play" : "pause"),
      (d) => nudges.push(d),
      () => navigatorStub.autoNext !== false,
      () => nudges.push("next"),
    );
    return { run, toggles, nudges };
  };

  // A browser without the API is asked for nothing at all.
  {
    const { run } = build({}, { paused: true });
    assert.equal(run.installPlayerMediaSession(), false, "a browser with no mediaSession was still asked for handlers");
  }

  const handlers = {};
  const navigatorStub = {
    mediaSession: {
      playbackState: "none",
      setActionHandler(action, handler) {
        // A browser throws NotSupportedError for an action it does not know.
        if (action === "nexttrack") throw new Error("NotSupportedError");
        handlers[action] = handler;
      },
      setPositionState() {},
    },
  };
  const video = { paused: true, playbackRate: 1 };
  const { run, toggles, nudges } = build(navigatorStub, video);
  assert.equal(run.installPlayerMediaSession(), true);
  // The unknown action did not take the ones after it down with it.
  assert.deepEqual(Object.keys(handlers).sort(), ["pause", "play", "seekbackward", "seekforward"]);

  // play and pause are separate and each is idempotent. A remote that sends
  // `play` to a film already playing must leave it playing — togglePlay would
  // stop it, which is exactly what one shared handler would have done.
  handlers.play();
  assert.deepEqual(toggles, ["play"], "play did not start a paused film");
  video.paused = false;
  handlers.play();
  assert.deepEqual(toggles, ["play"], "play on a playing film toggled it off");
  handlers.pause();
  assert.deepEqual(toggles, ["play", "pause"]);
  video.paused = true;
  handlers.pause();
  assert.deepEqual(toggles, ["play", "pause"], "pause on a paused film toggled it back on");

  // Seek uses the browser's own offset when it sends one, and the player's
  // ten seconds when it does not — and never in the wrong direction.
  handlers.seekbackward({});
  handlers.seekforward({});
  handlers.seekbackward({ seekOffset: 30 });
  handlers.seekforward({ seekOffset: 30 });
  assert.deepEqual(nudges, [-10, 10, -30, 30]);

  run.clearPlayerMediaSession();
  assert.deepEqual(Object.values(handlers).filter(Boolean), [], "closing left a handler installed");
  assert.equal(navigatorStub.mediaSession.playbackState, "none");
  process.stdout.write("PASS the OS transport installs feature-detected, keeps play and pause apart, and is handed back on close\n");
}

// The position goes out on the tick the player already runs, not on a new one.
{
  const states = [];
  const update = new Function(
    "navigator", "pbTotalSec", "pbShownSec",
    `${shipped("updatePlayerMediaSession")}\nreturn updatePlayerMediaSession;`,
  );
  const session = { playbackState: "none", setPositionState: (state) => states.push(state) };
  const run = update({ mediaSession: session }, () => 7200, () => 900);
  run({ paused: false, playbackRate: 1 }, {});
  assert.equal(session.playbackState, "playing");
  assert.deepEqual(states, [{ duration: 7200, position: 900, playbackRate: 1 }]);
  run({ paused: true, playbackRate: 1 }, {});
  assert.equal(session.playbackState, "paused");
  assert.equal(states.length, 2, "a paused player still publishes where it is");

  // A stream whose duration is still growing reports a position past the
  // duration routinely, and setPositionState throws on it. Say nothing rather
  // than throw out of the half-second tick.
  const growing = update({ mediaSession: session }, () => 0, () => 900);
  growing({ paused: false, playbackRate: 1 }, {});
  assert.equal(states.length, 2, "an unknown duration was published as a position state");
  process.stdout.write("PASS the OS transport takes its position from the sampling tick and refuses an impossible one\n");
}
