"use strict";

const assert = require("node:assert/strict");
const { test } = require("node:test");
const vm = require("node:vm");
const { shellSource } = require("./shell-source.js");

function source(name) {
  const html = shellSource().bodyScript;
  const start = html.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert.ok(start >= 0, `missing shipped ${name}`);
  return html.slice(start, html.indexOf("\n}", start) + 2);
}

test("AI completion selects the stable track after subtitle indices move", async () => {
  const selected = [];
  const tracks = [
    { index: 4, downloaded_subtitle_id: "different" },
    { index: 6, downloaded_subtitle_id: "completed" },
  ];
  const session = { file: { id: 3 }, player: {}, controller: new AbortController() };
  const context = vm.createContext({
    askDecision: async () => ({ subtitles: tracks }),
    setSub: async index => selected.push(index),
  });
  vm.runInContext(source("aiCaptionUseStored"), context);
  assert.equal(await context.aiCaptionUseStored(session, {
    state: "complete", track_id: "completed", track_index: 4,
  }, () => true), true);
  assert.deepEqual(selected, [6]);
  assert.equal(session.player.subs, tracks);
});

test("AI completion drops a decision that returns after playback changes", async () => {
  let finish;
  let current = true;
  const session = { file: { id: 3 }, player: {}, controller: new AbortController() };
  const context = vm.createContext({
    askDecision: () => new Promise(resolve => { finish = resolve; }),
    setSub: () => assert.fail("the retired playback must not attach a subtitle"),
  });
  vm.runInContext(source("aiCaptionUseStored"), context);
  const pending = context.aiCaptionUseStored(session, {
    state: "complete", track_id: "completed",
  }, () => current);
  current = false;
  finish({ subtitles: [{ index: 4, downloaded_subtitle_id: "completed" }] });
  assert.equal(await pending, false);
  assert.equal(session.player.subs, undefined);
});


test("item subtitle actions follow the account's chosen language", () => {
  const context = vm.createContext({
    aiCaptionLocal: () => true,
    langName: language => ({ en: "English", fr: "French", fra: "French" })[language],
    esc: String,
  });
  vm.runInContext(source("aiCaptionItemMarkup"), context);
  const file = {
    id: 3,
    playback_defaults: { subtitle: { preferred_language: "fr" } },
    subtitle_streams: [{ language: "en", forced: false }],
  };
  assert.match(context.aiCaptionItemMarkup(file), /\+ French/);
  assert.doesNotMatch(context.aiCaptionItemMarkup(file), /\+ English/);
  file.subtitle_streams.push({ language: "fra", forced: true });
  assert.match(context.aiCaptionItemMarkup(file), /\+ French/);
  file.subtitle_streams.push({ language: "fra", forced: false });
  assert.doesNotMatch(context.aiCaptionItemMarkup(file), /data-ai-caption-open/);
});
