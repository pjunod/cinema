"use strict";

// The admin API-keys panel, tested against the shipped index.html rather than
// against a copy of it.
//
// What this file is protecting: the two things a Rust gate structurally
// cannot see. First, the SCOPES — the checkbox list is a hand-written mirror
// of `plurx_core::domain::scopes::ALL`, and a scope added on the server that
// nobody adds here is a capability no operator can ever grant, with no failing
// test anywhere to say so. Second, the WORDS AND ESCAPING around a credential
// that exists exactly once: a mint form offered against a list we failed to
// read, a secret rendered without its "shown once" warning, or a key name that
// closes the onclick attribute it sits in are all invisible to `cargo test`
// and all ship a broken or unsafe settings screen.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const ROOT = path.resolve(__dirname, "../..");
const INDEX = fs.readFileSync(
  path.join(ROOT, "crates/plurxd/src/web/index.html"),
  "utf8",
);
const DOMAIN = fs.readFileSync(
  path.join(ROOT, "crates/plurx-core/src/domain.rs"),
  "utf8",
);

// Same extraction contract as tests/web/cluster-membership.test.js: every
// function borrowed here is declared at column zero in one inline <script>, so
// the next top-level declaration terminates it and no brace parsing is needed.
// A rename fails loudly rather than silently testing nothing.
const DECLARATIONS = ["\nfunction ", "\nasync function "];
function shippedSource(name) {
  const start = DECLARATIONS.map((kind) =>
    INDEX.indexOf(`${kind}${name}(`),
  ).find((at) => at !== -1);
  assert.notEqual(start, undefined, `index.html no longer declares ${name}`);
  const rest = INDEX.slice(start + 1);
  const ends = DECLARATIONS.map((kind) => rest.indexOf(kind, 1)).filter(
    (at) => at !== -1,
  );
  const end = ends.length ? Math.min(...ends) : -1;
  return (end === -1 ? rest : rest.slice(0, end)).trimEnd();
}

// The scope table is a top-level const, not a function, so it is lifted by its
// own literal. Reading the shipped array keeps the drift assertion below
// honest — it compares what ships, not what this test hopes ships.
function shippedKeyScopes() {
  const match = /\nconst KEY_SCOPES=(\[[\s\S]*?\n\]);/.exec(INDEX);
  assert.notEqual(match, null, "index.html no longer declares KEY_SCOPES");
  return { literal: match[1], value: new Function(`return ${match[1]}`)() };
}

// The app's own helpers are borrowed rather than stubbed, so the escaping and
// the "3m ago" formatting are under test here too.
const BORROWED = [
  "esc",
  "fmtAgo",
  "apiKeyRow",
  "apiKeysCardHtml",
  "mintKeyPanel",
  "mintedKeyHtml",
  "integrationsPanel",
];

// One sandbox per test so a mutation of ME or MINTED_KEY cannot leak into the
// next assertion.
function sandbox({ isAdmin = true, minted = null } = {}) {
  const source = `
    let ME = ${JSON.stringify({ is_admin: isAdmin })};
    let MINTED_KEY = ${JSON.stringify(minted)};
    const KEY_SCOPES = ${shippedKeyScopes().literal};
    function monarrCardHtml(){ return "<!--monarr-->"; }
    ${BORROWED.map(shippedSource).join("\n")}
    return { ${BORROWED.join(", ")} };
  `;
  return new Function(source)();
}

const NOW = Math.floor(Date.now() / 1000);
function key(over = {}) {
  return {
    id: 7,
    name: "monarr",
    scopes: ["scan:trigger"],
    created_at: NOW - 3 * 86400,
    last_used_at: NOW - 120,
    disabled: false,
    ...over,
  };
}

function test(name, fn) {
  try {
    fn();
    process.stdout.write(`ok - ${name}\n`);
  } catch (error) {
    process.stderr.write(`not ok - ${name}\n${error.stack}\n`);
    process.exitCode = 1;
  }
}

// --- the drift gate -------------------------------------------------------

test("the checkbox list is exactly the server's closed scope set", () => {
  // `pub const ALL: &[&str] = &[SCAN_TRIGGER, STATUS_READ];` names constants,
  // not literals, so resolve each `NAME: &str = "value"` and follow it.
  const literals = new Map();
  for (const [, name, value] of DOMAIN.matchAll(
    /pub const ([A-Z_]+): &str = "([^"]+)";/g,
  )) {
    literals.set(name, value);
  }
  const all = /pub const ALL: &\[&str\] = &\[([^\]]*)\]/.exec(DOMAIN);
  assert.notEqual(all, null, "domain.rs no longer declares scopes::ALL");
  const server = all[1]
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map((name) => {
      assert.ok(literals.has(name), `scopes::${name} has no string literal`);
      return literals.get(name);
    });

  assert.deepEqual(
    shippedKeyScopes().value.map(([id]) => id),
    server,
    "Settings -> Integrations offers a different scope set than the server accepts",
  );
});

test("every offered scope carries a sentence saying what it lets a holder do", () => {
  for (const [id, description] of shippedKeyScopes().value) {
    assert.ok(description.length > 20, `${id} is offered with no explanation`);
    assert.ok(
      /[.!]$/.test(description),
      `${id}'s explanation is not a sentence`,
    );
  }
});

// --- the list -------------------------------------------------------------

test("an empty list names the scope the monarr integration needs", () => {
  const html = sandbox().apiKeysCardHtml([]);
  assert.match(html, /No keys yet/);
  assert.match(html, /<code>scan:trigger<\/code>/);
  assert.match(html, /id="nkname"/, "an empty list must still offer the form");
});

test("a listed key shows its scopes, its age, and a way to revoke it", () => {
  const html = sandbox().apiKeysCardHtml([key()]);
  assert.match(html, /<td data-label="Name">monarr/);
  assert.match(html, /<code>scan:trigger<\/code>/);
  assert.match(html, /3d ago/, "created_at is not rendered as an age");
  assert.match(html, /2m ago/, "last_used_at is not rendered as an age");
  assert.match(html, /deleteKey\(7,/, "revoke does not carry the key id");
});

test("a key that has never been used says so rather than rendering blank", () => {
  // The blank cell is the dangerous version: it reads as "no data yet" when
  // what it means is "whatever you configured is holding a different key".
  const html = sandbox().apiKeysCardHtml([key({ last_used_at: null })]);
  assert.match(html, />never</);
});

test("a hostile key name cannot escape its cell or its onclick attribute", () => {
  const html = sandbox().apiKeysCardHtml([
    key({ name: '"><img src=x onerror=alert(1)>' }),
  ]);
  assert.ok(!html.includes("<img"), "a raw tag reached the document");
  assert.match(html, /&quot;&gt;&lt;img/, "the name was not escaped");
  // The handler is single-quoted, so the JSON argument must carry no bare
  // quote of either kind.
  const handler = /onclick='([^']*)'/.exec(html);
  assert.notEqual(handler, null, "the revoke handler lost its quoting");
  assert.ok(!handler[1].includes('"'), "a bare double quote survived");
});

test("a list we could not read offers a retry and withholds the form", () => {
  // Offering "Create key" against an unreadable list invites a duplicate of a
  // key that already exists, which is the one mistake the list exists to stop.
  const html = sandbox().apiKeysCardHtml({
    unavailable: true,
    message: "quorum unavailable",
  });
  assert.match(html, /quorum unavailable/);
  assert.match(html, /retryKeys\(\)/);
  assert.ok(!html.includes('id="nkname"'), "the mint form was still offered");
});

// --- the secret -----------------------------------------------------------

test("the minted secret is shown once, loudly, beside its scopes", () => {
  const html = sandbox().mintedKeyHtml({
    id: 9,
    name: "monarr",
    scopes: ["scan:trigger"],
    key_secret: "plx_deadbeef",
  });
  assert.match(html, /plx_deadbeef/);
  assert.match(html, /<b>Shown once\.<\/b>/, "the once-only warning is missing");
  assert.match(html, /<code>scan:trigger<\/code>/, "shown without its scopes");
  assert.match(html, /copyMintedKey\(\)/);
  assert.match(html, /clearMintedKey\(\)/);
});

test("a held secret replaces the form instead of sitting beside it", () => {
  const held = sandbox({
    minted: { id: 9, name: "monarr", scopes: [], key_secret: "plx_beef" },
  });
  const html = held.mintKeyPanel();
  assert.match(html, /plx_beef/);
  assert.ok(!html.includes('id="nkname"'), "the form survived the secret");
});

test("scan:trigger is pre-checked so the common case is one click", () => {
  const html = sandbox().mintKeyPanel();
  assert.match(html, /value="scan:trigger" checked/);
  assert.ok(
    !/value="status:read" checked/.test(html),
    "status:read must be opt-in — every scope is a promise about a stolen key",
  );
});

// --- the tab --------------------------------------------------------------

test("both directions of the monarr seam land on the one tab", () => {
  // The whole reason this tab exists: the key another app holds to call plurx,
  // and the key plurx holds to call that app, are trivially confused when they
  // live on different screens.
  const html = sandbox().integrationsPanel({ keys: [] });
  assert.match(html, /API keys/);
  assert.match(html, /<!--monarr-->/, "the monarr card is not on this tab");
});

test("the tab is registered and a non-admin renders nothing at all", () => {
  assert.match(
    INDEX,
    /\["integrations","Integrations"\]/,
    "the Integrations tab is not in SET_TABS",
  );
  assert.match(
    INDEX,
    /integrations:\{required:\["settings","keys"\]/,
    "the Integrations tab declares no data requirement",
  );
  assert.equal(sandbox({ isAdmin: false }).integrationsPanel({ keys: [] }), "");
});

test("leaving Settings drops a minted key the way it drops a join token", () => {
  // Bearer material must not outlive the screen that showed it.
  assert.match(
    INDEX,
    /forgetJoinToken\(\); forgetMintedKey\(\);/,
    "setSettingsTab or viewSettings no longer drops the minted key",
  );
  assert.ok(
    !/localStorage[^\n]*MINTED_KEY|MINTED_KEY[^\n]*localStorage/.test(INDEX),
    "the minted secret must never reach localStorage",
  );
});
