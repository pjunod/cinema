"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

const SOURCE = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/pages/optical.js"),
  "utf8",
);

function harness() {
  const calls = [];
  const played = [];
  const toasts = [];
  const context = vm.createContext({
    console,
    encodeURIComponent,
    location: { hash: "#/discs", origin: "https://plurx.test" },
    document: {
      hidden: false,
      getElementById: () => null,
      querySelector: () => null,
      querySelectorAll: () => [],
    },
    ME: { id: 1, is_admin: true },
    PLAYER: null,
    PAGE_RENDER_GENERATION: 1,
    api: async (url, options) => { calls.push({ url, options }); return []; },
    play: async (...args) => { played.push(args); },
    toast: message => { toasts.push(message); },
    confirm: () => true,
    render: async () => {},
    esc: value => String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;")
      .replaceAll('"', "&quot;"),
    fmtDur: seconds => `${seconds}s`,
    fmtChannels: channels => `${channels} ch`,
    langName: language => language,
    codecLabel: codec => codec,
    clockFromSec: seconds => `${seconds}s`,
    exactWireId: item => String(item.id_string || item.item_id || item.id),
    catalogIcon: () => "icon",
    playbackInputForPlayer: player => player.source,
    playbackInputIsOptical: value => value?.source === "optical",
    opticalRouteDrive: value => value.route_drive_id || `${value.owner_node_id}:${value.drive_id}`,
    layoutChrome: () => {},
    pageHead: () => "",
    setPagePhase: () => {},
    setPageTimer: () => {},
  });
  vm.runInContext(SOURCE, context, { filename: "pages/optical.js" });
  return { context, calls, played, toasts };
}

function insertedDrive(overrides = {}) {
  return {
    id: "node-a:drive one/side?",
    name: "Media <room>",
    owner_node_id: "node-a",
    requirements: [],
    state: { state: "ready", media_generation: "generation-a", disc_id: "disc/a?" },
    disc: {
      id: "disc/a?",
      media_generation: "generation-a",
      format: "bluray",
      display_title: "Example & Disc",
    },
    ...overrides,
  };
}

test("every opaque optical route component is encoded independently", async () => {
  const { context: c, calls } = harness();
  const drive = insertedDrive();
  assert.equal(c.opticalDriveHref(drive), "#/discs/node-a%3Adrive%20one%2Fside%3F");

  c.api = async (url, options) => {
    calls.push({ url, options });
    return { drive, titles: [] };
  };
  await c.opticalLoadDrive(drive.id);
  await c.opticalUnmatch("disc/a?", "title #1/alt");

  assert.equal(calls[0].url, "/optical/drives/node-a%3Adrive%20one%2Fside%3F/disc");
  assert.equal(
    calls[1].url,
    "/optical/discs/disc%2Fa%3F/titles/title%20%231%2Falt/match",
  );
  assert.deepEqual(calls[1].options.body, { matched_item_id: null, match_kind: null });
});

test("failed insertions stay visible without rendering host text as markup", () => {
  const { context: c } = harness();
  const drive = insertedDrive({
    disc: null,
    state: {
      state: "failed",
      media_generation: "generation-failed",
      reason: "bad <script>alert(1)</script>",
    },
  });
  const html = c.opticalHomeHtml({ optical: [drive] });
  assert.match(html, /class="optical-card reading"/);
  assert.match(html, /bad &lt;script>alert\(1\)&lt;\/script>/);
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /node-a%3Adrive%20one%2Fside%3F/);
});

test("busy eject is enabled only for the exact locally owned optical session", () => {
  const { context: c } = harness();
  const drive = insertedDrive({
    state: {
      state: "busy",
      media_generation: "generation-a",
      disc_id: "disc/a?",
      title_id: "title-a",
    },
  });
  c.PLAYER = {
    sessionId: "session-a",
    source: {
      source: "optical",
      owner_node_id: "node-a",
      drive_id: "drive one/side?",
      route_drive_id: drive.id,
      media_generation: "generation-a",
      disc_id: "disc/a?",
    },
  };
  assert.equal(c.opticalOwnedBusySession(drive), "session-a");
  assert.match(c.opticalEjectControl(drive), /Stop &amp; eject/);

  c.PLAYER.source = { ...c.PLAYER.source, media_generation: "generation-old" };
  assert.equal(c.opticalOwnedBusySession(drive), null);
  assert.match(c.opticalEjectControl(drive), /disabled title="Another playback owns this drive"/);
});

test("play adapts a title into the shared player without a fake file id", async () => {
  const { context: c, played } = harness();
  const drive = insertedDrive();
  c.api = async () => ({
    drive,
    titles: [{ id: "title #1/alt", duration_ms: 7_200_000, angles: 1 }],
  });

  await c.opticalPlayAt(drive.id, "title #1/alt", 12_345);
  assert.equal(played.length, 1);
  const [source, title, startMs, durationMs] = played[0];
  assert.deepEqual(JSON.parse(JSON.stringify(source)), {
    source: "optical",
    owner_node_id: "node-a",
    drive_id: "drive one/side?",
    route_drive_id: "node-a:drive one/side?",
    media_generation: "generation-a",
    disc_id: "disc/a?",
    title_id: "title #1/alt",
    angle: 1,
  });
  assert.equal(title, "Example & Disc");
  assert.equal(startMs, 12_345);
  assert.equal(durationMs, 7_200_000);
  assert.equal(Object.hasOwn(source, "file_id"), false);
});

test("eject repeats insertion identity and never takes over another session", async () => {
  const { context: c, calls, toasts } = harness();
  const drive = insertedDrive();
  c.api = async (url, options) => {
    calls.push({ url, options });
    if (url.endsWith("/disc")) return { drive, titles: [] };
    return null;
  };
  await c.opticalEject(drive.id);
  assert.equal(calls[1].url, "/optical/drives/node-a%3Adrive%20one%2Fside%3F/eject");
  assert.deepEqual(calls[1].options.body, {
    expected_disc_id: "disc/a?",
    media_generation: "generation-a",
    stop_active: false,
    session_id: null,
  });
  assert.deepEqual(toasts, ["Disc ejected"]);

  const other = insertedDrive({ state: { ...drive.state, state: "busy" } });
  calls.length = 0;
  c.api = async (url, options) => {
    calls.push({ url, options });
    return { drive: other, titles: [] };
  };
  await c.opticalEject(other.id);
  assert.equal(calls.length, 1, "a viewer cannot send eject for another playback's reader");
  assert.equal(toasts.at(-1), "The drive is in use by another playback");
});
