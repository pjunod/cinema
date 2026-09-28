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
  method: "Transcode · cached", player_state: "Playing", decode_resolution: "Unavailable",
  source_resolution: "3840×2160", stream_frame: "Unavailable", client_loaded: "12.0 s", stalls: "0",
});
assert.match(missingPicture, /pi-picture[^]*?Source frame[^]*?<strong>3840×2160<\/strong>/);
assert.match(missingPicture, /Stream frame[^]*?<strong>Unavailable<\/strong>/);
assert.match(missingPicture, /Player display size[^]*?<strong>Unavailable<\/strong>/);
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
//
// The handlers are composed with the shipped routing (`watchRouteInput` over
// the real contract table) and the shipped intent reader. `togglePlay` is a
// seam here that flips `PLAYER.wantsPlayback`, which is what the real one does
// on an attached player; `web-control.test.js` drives the real `togglePlay`
// through the reattach window, where the element and the intent disagree.
const PlaybackPolicy = require("../../crates/plurxd/src/web/playback-policy.js");
function mediaSessionHarness(navigatorStub, { state = "transport", autoNext = true, meta = null, wantsPlayback = false } = {}) {
  const log = [];
  const world = { state, autoNext };
  const video = { paused: true, ended: false, playbackRate: 1 };
  const PLAYER = { wantsPlayback, meta };
  const run = new Function(
    "navigator", "document", "PlaybackPolicy", "PLAYER", "world", "log",
    [
      "const WATCH=null, play={}, PLAY_OPEN_GATE={current:()=>false};",
      "function playerInputState(){return world.state;}",
      "function togglePlay(){PLAYER.wantsPlayback=!PLAYER.wantsPlayback;log.push(PLAYER.wantsPlayback?'toggle:play':'toggle:pause');}",
      "function commitPendingSeek(){log.push('commit');}",
      "function nudge(d){log.push(d);}",
      "function autoNextOn(){return world.autoNext;}",
      "function playNextEpisode(){log.push('next');}",
      shipped("playerInputSurface"), shipped("watchRouteInput"), shipped("playerWantsPlayback"),
      shipped("setPlayerMediaAction"), shipped("playerMediaPlayPause"), shipped("playerMediaSkip"),
      shipped("playerNextTrackOffered"), shipped("syncPlayerNextTrack"),
      shipped("installPlayerMediaSession"), shipped("clearPlayerMediaSession"),
      "return {installPlayerMediaSession, clearPlayerMediaSession, syncPlayerNextTrack};",
    ].join("\n"),
  )(navigatorStub, { getElementById: () => video }, PlaybackPolicy, PLAYER, world, log);
  return { run, log, world, video, PLAYER };
}
function recordingNavigator({ refuse = [] } = {}) {
  const handlers = {};
  return {
    handlers,
    mediaSession: {
      playbackState: "none",
      setActionHandler(action, handler) {
        // A browser throws NotSupportedError for an action it does not know.
        if (refuse.includes(action)) throw new Error("NotSupportedError");
        handlers[action] = handler;
      },
      setPositionState() {},
    },
  };
}

{
  // A browser without the API is asked for nothing at all.
  const { run } = mediaSessionHarness({});
  assert.equal(run.installPlayerMediaSession(), false, "a browser with no mediaSession was still asked for handlers");
}

{
  const nav = recordingNavigator({ refuse: ["nexttrack"] });
  const { run, log, world, PLAYER, video } = mediaSessionHarness(nav);
  const { handlers } = nav;
  assert.equal(run.installPlayerMediaSession(), true);
  // The unknown action did not take the ones after it down with it.
  assert.deepEqual(Object.keys(handlers).sort(), ["pause", "play", "seekbackward", "seekforward"]);

  // play and pause are separate and each is idempotent ON THE VIEWER'S
  // INTENT. The element here stays paused throughout — the reattach window —
  // and that must not decide anything.
  handlers.play();
  assert.deepEqual(log, ["toggle:play"], "play did not start a film the viewer had paused");
  handlers.play();
  assert.deepEqual(log, ["toggle:play"], "play on a film the viewer wants playing toggled it off (the element was merely paused)");
  handlers.pause();
  assert.deepEqual(log, ["toggle:play", "toggle:pause"]);
  handlers.pause();
  assert.deepEqual(log, ["toggle:play", "toggle:pause"], "pause on a paused film toggled it back on");
  // An ended element reads as not playing: `play` is the replay, `pause` nothing.
  PLAYER.wantsPlayback = true; video.ended = true; log.length = 0;
  handlers.pause();
  assert.deepEqual(log, [], "pause on an ended film asked togglePlay, which replays it");
  handlers.play();
  assert.deepEqual(log, ["toggle:pause"], "play on an ended film did not reach togglePlay's replay");
  video.ended = false; PLAYER.wantsPlayback = true; log.length = 0;

  // Seek uses the browser's own offset when it sends one, and the player's
  // ten seconds when it does not — and never in the wrong direction.
  handlers.seekbackward({});
  handlers.seekforward({});
  handlers.seekbackward({ seekOffset: 30 });
  handlers.seekforward({ seekOffset: -30 });
  assert.deepEqual(log, [-10, 10, -30, 30]);

  // The contract table decides, as it does for the keys (§5).
  // failed × play_pause = ignore: a headset press on a blocking prompt.
  world.state = "failed"; log.length = 0; PLAYER.wantsPlayback = false;
  handlers.play(); handlers.seekforward({});
  assert.deepEqual(log, [], "a blocking prompt did not ignore the OS transport");
  assert.equal(PLAYER.wantsPlayback, false);
  // scrub × play_pause = commit_then_toggle_play; scrub × skip = ignore.
  world.state = "scrub"; PLAYER.wantsPlayback = true;
  handlers.play();
  assert.deepEqual(log, ["commit"], "play during a pending seek must commit it and keep playing");
  handlers.pause();
  assert.deepEqual(log, ["commit", "commit", "toggle:pause"], "pause during a pending seek must commit it, then pause");
  handlers.seekbackward({}); handlers.seekforward({ seekOffset: 5 });
  assert.equal(log.length, 3, "a skip during a pending seek replaced its target");
  // menu and info × skip = ignore; play_pause still toggles.
  for (const state of ["menu", "info"]) {
    world.state = state; log.length = 0; PLAYER.wantsPlayback = true;
    handlers.seekforward({}); handlers.seekbackward({});
    handlers.pause();
    assert.deepEqual(log, ["toggle:pause"], `${state}: skips must be ignored and pause must still pause`);
  }
  // hidden and timeline skip, as the keys do.
  for (const state of ["hidden", "timeline"]) {
    world.state = state; log.length = 0;
    handlers.seekforward({ seekOffset: 15 });
    assert.deepEqual(log, [15], `${state}: the OS skip must reach nudge`);
  }

  run.clearPlayerMediaSession();
  assert.deepEqual(Object.values(handlers).filter(Boolean), [], "closing left a handler installed");
  assert.equal(nav.mediaSession.playbackState, "none");
  process.stdout.write("PASS the OS transport installs feature-detected, routes through the contract table, keeps play and pause apart on intent, and is handed back on close\n");
}

// Next exists only while autoplay-next is on, and only for what can be an
// episode: a browser draws a Next button for any action with a handler.
{
  const nav = recordingNavigator();
  const { run, log, world, PLAYER } = mediaSessionHarness(nav, { autoNext: false });
  run.installPlayerMediaSession();
  assert.equal(nav.handlers.nexttrack ?? null, null, "Next was registered with autoplay-next off");
  world.autoNext = true; run.syncPlayerNextTrack();
  assert.equal(typeof nav.handlers.nexttrack, "function", "turning autoplay-next on did not offer Next");
  nav.handlers.nexttrack();
  assert.deepEqual(log, ["next"]);
  world.autoNext = false; run.syncPlayerNextTrack();
  assert.equal(nav.handlers.nexttrack, null, "turning autoplay-next off left Next on the lock screen");
  world.autoNext = true; PLAYER.meta = { kind: "movie" }; run.syncPlayerNextTrack();
  assert.equal(nav.handlers.nexttrack, null, "a movie was offered a next episode");
  PLAYER.meta = { kind: "episode" }; run.syncPlayerNextTrack();
  assert.equal(typeof nav.handlers.nexttrack, "function");
  run.clearPlayerMediaSession();
  assert.equal(nav.handlers.nexttrack, null);
  run.syncPlayerNextTrack();
  assert.equal(nav.handlers.nexttrack, null, "a settings change after close re-installed Next for a player that is gone");
  // `setAutoNext` is where the setting changes; it has to re-sync.
  assert.match(shipped("setAutoNext"), /syncPlayerNextTrack\(\)/, "setAutoNext no longer re-syncs the OS transport's Next");
  process.stdout.write("PASS the OS transport offers Next only while autoplay-next is on, and follows the setting\n");
}

// The position goes out on the tick the player already runs, not on a new one.
{
  const states = [];
  const PLAYER = { wantsPlayback: true };
  const update = new Function(
    "navigator", "pbTotalSec", "pbShownSec", "PLAYER",
    `const play={}, PLAY_OPEN_GATE={current:()=>false};
     ${shipped("playerWantsPlayback")}
     ${shipped("updatePlayerMediaSession")}\nreturn updatePlayerMediaSession;`,
  );
  const session = { playbackState: "none", setPositionState: (state) => states.push(state) };
  const run = update({ mediaSession: session }, () => 7200, () => 900, PLAYER);
  run({ paused: false, playbackRate: 1 }, PLAYER);
  assert.equal(session.playbackState, "playing");
  assert.deepEqual(states, [{ duration: 7200, position: 900, playbackRate: 1 }]);
  // The reattach window: the element is paused, the viewer is not. The OS
  // must keep showing Pause, or the next press arrives as `play`.
  run({ paused: true, playbackRate: 1 }, PLAYER);
  assert.equal(session.playbackState, "playing", "a reattach's paused element told the OS the viewer had paused");
  PLAYER.wantsPlayback = false;
  run({ paused: true, playbackRate: 1 }, PLAYER);
  assert.equal(session.playbackState, "paused");
  assert.equal(states.length, 3, "a paused player still publishes where it is");

  // A stream whose duration is still growing reports a position past the
  // duration routinely, and setPositionState throws on it. Say nothing rather
  // than throw out of the half-second tick.
  const growing = update({ mediaSession: session }, () => 0, () => 900, PLAYER);
  growing({ paused: false, playbackRate: 1 }, PLAYER);
  assert.equal(states.length, 3, "an unknown duration was published as a position state");
  process.stdout.write("PASS the OS transport takes its position from the sampling tick and its play state from the viewer's intent\n");
}
