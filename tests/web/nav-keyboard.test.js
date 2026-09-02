"use strict";

// The non-player half of the input contract (plan M5): a library you can
// reach from a keyboard or a D-pad, dialogs that say they are dialogs, and a
// search field that survives the route change its own keystrokes cause.
//
// The functions are sliced out of the shipped index.html and executed here,
// the same pattern tests/playback/web-policy.test.js uses — a test that reads
// the source without running it proves the text, not the behaviour.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const source = fs.readFileSync(
  path.join(__dirname, "../../crates/plurxd/src/web/index.html"),
  "utf8",
);

function shippedSource(name) {
  const start = source.indexOf(`function ${name}(`);
  assert.ok(start >= 0, `index.html no longer declares ${name}()`);
  let depth = 0;
  let seen = false;
  for (let at = start; at < source.length; at += 1) {
    const ch = source[at];
    if (ch === "{") {
      depth += 1;
      seen = true;
    } else if (ch === "}") {
      depth -= 1;
      if (seen && depth === 0) return source.slice(start, at + 1);
    }
  }
  throw new Error(`${name}() is unbalanced`);
}

// ---- cards and episode rows are reachable ---------------------------------

function element(cls, onclick) {
  const attributes = new Map();
  return {
    classList: { contains: (name) => cls.split(" ").includes(name) },
    getAttribute: (name) => (name === "onclick" ? onclick : attributes.get(name) ?? null),
    setAttribute: (name, value) => attributes.set(name, value),
    attributes,
  };
}

const poster = element("poster k-movie", "location.hash='#/item/12'");
const photo = element("poster k-photo", "openLightbox(4)");
const episode = element("eprow", "location.hash='#/item/88'");

const enhance = new Function(
  "document",
  [
    shippedSource("navEnhanceClickables"),
    "return navEnhanceClickables;",
  ].join("\n"),
)({
  // The selector asks for :not([tabindex]); the stub answers with the
  // elements that would match, which is what the shipped querySelectorAll
  // does against the real DOM.
  querySelectorAll: (selector) => {
    assert.match(selector, /\.poster\[onclick\]:not\(\[tabindex\]\)/);
    assert.match(selector, /\.eprow\[onclick\]:not\(\[tabindex\]\)/);
    return [poster, photo, episode];
  },
});
enhance();

assert.equal(poster.attributes.get("tabindex"), "0");
assert.equal(poster.attributes.get("role"), "link", "a card that navigates is a link");
assert.equal(photo.attributes.get("role"), "button", "a card that opens the lightbox is a button");
assert.equal(episode.attributes.get("tabindex"), "0", "episode rows were mouse-only");
assert.equal(episode.attributes.get("role"), "link");

// One handler, every layout: the near-copies that guarded on layoutId() are
// gone, and Classic — which had none at all — is covered by the same one.
assert.doesNotMatch(source, /function catalogEnhanceCards\(/);
assert.doesNotMatch(source, /function theaterEnhanceCards\(/);
assert.equal((source.match(/navKeyboardWireOnce\(\);/g) || []).length, 3,
  "every layout's chrome wires the shared card reach");

// ---- the search caret survives the rebuild its own typing causes ----------

const searchState = { focused: 0, range: null };
const searchInput = {
  selectionStart: 3,
  selectionEnd: 3,
  focus: () => { searchState.focused += 1; },
  setSelectionRange: (start, end) => { searchState.range = [start, end]; },
};
const searchDocument = { activeElement: searchInput, getElementById: (id) => (id === "q" ? searchInput : null) };
const search = new Function(
  "document",
  [
    shippedSource("captureSearchFocus"),
    shippedSource("restoreSearchFocus"),
    "return {captureSearchFocus, restoreSearchFocus};",
  ].join("\n"),
)(searchDocument);

const carried = search.captureSearchFocus();
assert.deepEqual(carried, { start: 3, end: 3 });
search.restoreSearchFocus(carried);
assert.equal(searchState.focused, 1, "the rebuilt field takes focus back");
assert.deepEqual(searchState.range, [3, 3], "and the caret with it");

searchDocument.activeElement = { tagName: "BODY" };
assert.equal(search.captureSearchFocus(), null, "a field nobody was typing in is not restored");
search.restoreSearchFocus(null);
assert.equal(searchState.focused, 1, "restoring nothing focuses nothing");

// ---- the two dialogs say they are dialogs and give focus back ------------

for (const [name, label] of [["openLightbox", "Photo"], ["openEdit", "Edit details"]]) {
  const body = shippedSource(name);
  assert.match(body, /setAttribute\("role","dialog"\)/, `${name} does not announce a dialog`);
  assert.match(body, /setAttribute\("aria-modal","true"\)/, `${name} is not modal`);
  assert.ok(body.includes(label), `${name} has no accessible name`);
}
for (const name of ["closeLightbox", "closeEdit"]) {
  assert.match(
    shippedSource(name),
    /opener&&opener\.focus&&document\.contains\(opener\)/,
    `${name} does not return focus to what opened it`,
  );
}
// One Escape handler decides which modal closes, so a single press can never
// close two of them.
const escape = source.slice(source.indexOf("// Capture Escape before layout-specific sheet handlers"));
assert.match(escape.slice(0, 900), /connectQrOpen\(\)[\s\S]*closeConnectQr\(\); return;/);
assert.match(escape.slice(0, 900), /getElementById\("editwrap"\)[\s\S]*closeEdit\(\);/);

process.stdout.write("PASS non-player navigation is keyboard-reachable and announces its dialogs\n");
