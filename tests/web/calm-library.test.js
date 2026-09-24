"use strict";
const assert = require('node:assert/strict');
const fs = require('node:fs');
const {shellSource} = require("./shell-source.js");
// The shipped app twice over: its script, for the painters these features
// would need, and its CSS, for the class names they would style. Neither
// half may come back.
const shell = shellSource();
const source = shell.bodyScript + shell.css;
function declaration(name) {
  const start = source.indexOf(`function ${name}(`);
  assert.notEqual(start, -1);
  const tail = source.slice(start);
  return tail.slice(0, tail.indexOf('\n}') + 2);
}
// The item page is the original one on every layout: no per-layout dispatch to a
// shared "viewing" body, no calm-item hero, no collapsible track lists. Each
// layout's item body starts at its own model line, so a dispatch inserted above
// it would move that line and fail here.
for (const [name, first] of [['classicItemBody', 'const d={files:p.files, ancestors:p.ancestors, children:p.children};'],
                             ['catalogItemBody', 'const d={files:p.files, ancestors:p.ancestors, children:p.children};'],
                             ['theaterItemBody', 'const it=p.item;']]) {
  const body = declaration(name);
  assert.equal(body.split('\n')[1].trim(), first, `${name} opens with its own model, not a dispatch`);
}
assert.doesNotMatch(source, /function viewingItemBody\(|function viewingKind\(|function loadSeriesContinuation\(|function itemTrackSummary\(|function englishTrackNote\(/);
assert.doesNotMatch(source, /calm-item|watch-hero|watch-resume|item-quality|media-tracks|series-continue/);
assert.match(declaration('specBlock'), /\$\{audRow\}\n\s*\$\{subRow\}\n\s*\$\{hlsRow\}/);
console.log('Item pages use the original per-layout bodies with open track facts.');

const hero = new Function(
  `const THEATER_HERO_KINDS={movie:1,episode:1,video:1}; ${declaration('theaterHeroPick')}; return theaterHeroPick;`)();
const continued = {id:'continue',kind:'episode',title:'The episode I was watching'};
assert.equal(hero({hubs:{continue_watching:[continued]}}).item,continued);
assert.equal(hero({hubs:{continue_watching:[continued]}}).resuming,true);
assert.equal(hero({hubs:{recently_added:[],next_up:[continued]}}),null);
assert.match(declaration('theaterHomeBody'), /theaterHeroHtml/);
assert.match(declaration('theaterHomeBody'), /data-home-slot="hero"/);
for (const name of ['classicHomeBody', 'catalogHomeBody', 'theaterHomeBody']) {
  const body = declaration(name);
  assert.match(body, /rail\("Continue watching"/);
  assert.match(body, /rail\("Next up"/);
  assert.match(body, /card\(i,true\)/);
  assert.doesNotMatch(body, /calmHomeBody|<details/);
}
assert.doesNotMatch(source, /function viewingHubs\(|function viewingCard\(|function homeSavedHtml\(/);
assert.doesNotMatch(source, /calm-home|resume-strip|home-explore|watch-cards|watch-next/);
console.log('All three Home layouts retain their original shelves and Theater feature.');

// ---------------------------------------------------------------------------
// The library view's incremental draw (F-web-11), and the shared reduced-motion
// rule (F-web-15). Both live here because this file already holds the shipped
// body rows and the shipped stylesheet, which is what each of them needs.

// A grid that can be extended is a grid whose nodes survive, so this harness
// has to be able to tell one node from another. The mini-DOM below models only
// what `libraryView`'s draw touches: innerHTML replaces a node's children,
// insertAdjacentHTML("beforeend") adds to them, and every card parsed out of
// either is a distinct object whose identity the test can compare. Cards are
// stubbed to `<i:ID>` so the parse is exact rather than a guess at markup.
function libraryHarness(pages, state) {
  const nodes = {};
  const parse = (html) => [...html.matchAll(/<i:([^>]+)>/g)].map((m) => ({ id: m[1] }));
  const makeNode = (id) => {
    const node = {
      id, children: [], grid: null,
      set innerHTML(html) {
        this.children = [];
        this.grid = html.includes('<div class="grid">')
          ? { children: parse(html),
              insertAdjacentHTML(where, more) {
                assert.equal(where, "beforeend");
                this.children.push(...parse(more));
              } }
          : null;
      },
      get innerHTML() { return ""; },
      querySelector(selector) {
        assert.equal(selector, ".grid");
        return this.grid;
      },
      // The browse tools are mounted before #libbody and are not this test's
      // subject; only their landing place has to exist.
      insertAdjacentHTML(where) { assert.equal(where, "beforebegin"); },
    };
    nodes[id] = node;
    return node;
  };
  ["main", "libbody", "libpager", "librail", "libcount"].forEach(makeNode);
  const api = new Function(
    "assert", "document", "state", "pages", "nodes",
    [
      "let LIB_LOAD=0,LIB_PAGE_AT=0,LIB_VIEW=null,PHOTO_SET=null;",
      "const LIB_PER_PAGE=200;",
      // The three filter globals and the page size are the state a case sets.
      "let LIB_SCOPE=state.scope||'',LIB_FIND=state.find||'',LIB_FILTER=state.filter||'all',LIB_PER=state.per||'all';",
      "function layoutChrome(){} function setOrigin(){} function restoreScroll(){}",
      "function libraryBrowseTools(){return '';}",
      "async function libsCached(){return [{id:1}];}",
      // Only "unwatched" is modelled: enough to hide some cards and show them again.
      "function matchWatch(it,f){return f==='unwatched'?!it.watched:true;}",
      "function libPageCount(n){return LIB_PER==='all'?1:Math.max(1,Math.ceil(n/LIB_PER));}",
      "function pagerHtml(){return '';} function alphaRailHtml(){return '';}",
      "function tagAlpha(){} function exactWireId(i){return i.id;}",
      // The shipped painters, with only the card stubbed.
      "function card(i){return `<i:${i.id}>`;}",
      declaration("rememberGridPhotos"),
      declaration("grid"),
      declaration("classicLibraryItems"),
      "function resSections(items){return `<div class=\"res\">${items.map(card).join('')}</div>`;}",
      "function layoutRegion(name,region,m){assert.equal(name,'library');"
        + "return region==='items'?classicLibraryItems(m):'';}",
      // Two batches, delivered the way eachItemPage delivers them.
      "async function eachItemPage(ids,sort,gen,onBatch){for(const batch of pages){onBatch(batch,0);await Promise.resolve();}return true;}",
      // `declaration` slices from `function <name>(`, which drops the `async`
      // keyword in front of it; put it back or the body's `await` is an
      // identifier.
      `async ${declaration("libraryView")}`,
      // The shipped pager, and the three narrowing globals, so a case can do
      // what the viewer does after the load: page, filter, scope, find.
      declaration("libGoPage"),
      "return {libraryView, libGoPage, redraw(){LIB_VIEW.draw(LIB_VIEW.done);},"
        + "narrow(n){if('find' in n)LIB_FIND=n.find;if('filter' in n)LIB_FILTER=n.filter;if('scope' in n)LIB_SCOPE=n.scope;}};",
    ].join("\n"),
  )(assert, { getElementById: (id) => nodes[id] || null, querySelector: () => null }, state, pages, nodes);
  return { run: api.libraryView, api, nodes };
}
// Titles, libraries and watched state exist so the find, scope and filter
// cases below can hide batch A and show it again.
const batchA = [{ id: "a1", title: "a1", library_id: 1, watched: true }, { id: "a2", title: "a2", library_id: 1, watched: true }];
const batchB = [{ id: "b1", title: "b1", library_id: 2 }, { id: "b2", title: "b2", library_id: 2 }, { id: "b3", title: "b3", library_id: 2 }];

(async () => {
  // A single library, unfiltered, one page: the second batch extends the grid
  // the first batch painted. The first card is the SAME object afterwards —
  // an innerHTML rebuild would have replaced it — and every card is present.
  {
    const { run, nodes } = libraryHarness([batchA, batchB], {});
    let first = null;
    const view = { title: "Films", href: "#/lib/1", libIds: [1], sort: "title", resort: null,
      onBatch: null };
    const painting = run(view);
    // Capture the first card's identity as soon as batch one has painted.
    await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
    first = nodes.libbody.grid && nodes.libbody.grid.children[0];
    await painting;
    const cards = nodes.libbody.grid.children;
    assert.equal(cards.length, batchA.length + batchB.length, "the grid did not grow to hold both batches");
    assert.deepEqual(cards.map((c) => c.id), ["a1", "a2", "b1", "b2", "b3"], "the batches are out of order");
    assert.ok(first, "the first batch painted no card");
    assert.equal(cards[0], first, "the second batch rebuilt the grid instead of extending it");
  }
  // A category re-sorts the merged set, so a later batch may belong BEFORE the
  // cards already on screen: it must rebuild. `resort` reverses here, which is
  // enough to prove the rebuild happened — an append could not produce it.
  {
    const { run, nodes } = libraryHarness([batchA, batchB], {});
    const view = { title: "Movies", href: "#/cat/movie", libIds: [1], sort: "title",
      resort: (items) => items.slice().reverse() };
    const painting = run(view);
    await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
    const first = nodes.libbody.grid && nodes.libbody.grid.children[0];
    await painting;
    const cards = nodes.libbody.grid.children;
    assert.deepEqual(cards.map((c) => c.id), ["b3", "b2", "b1", "a2", "a1"], "the category did not re-sort the merged set");
    assert.notEqual(cards[0], first, "a category view appended instead of rebuilding");
  }
  // A paged view's window moves as the list grows and as the viewer pages, so
  // it rebuilds too. Two cards a page over five items: page 2 is as long as
  // page 1, which is exactly the draw an append would claim (`page.length >=
  // painted`) and then leave page 1's cards on screen under page 2's pager.
  {
    const { run, api, nodes } = libraryHarness([batchA, batchB], { per: 2 });
    const view = { title: "Films", href: "#/lib/1", libIds: [1], sort: "title", resort: null };
    await run(view);
    assert.deepEqual(nodes.libbody.grid.children.map((c) => c.id), ["a1", "a2"], "the page slice was not applied");
    api.libGoPage(1);
    assert.deepEqual(nodes.libbody.grid.children.map((c) => c.id), ["b1", "b2"],
      "paging after the load kept page 1's cards: a paged view appended instead of rebuilding");
    api.libGoPage(2);
    assert.deepEqual(nodes.libbody.grid.children.map((c) => c.id), ["b3"]);
  }
  // Filter, scope and find change WHICH cards are visible, not how many
  // arrived. Narrow after the load, then clear: the grid must be the whole
  // list again, in order and once each. An append would have kept the narrowed
  // cards and added the tail of the full list after them.
  for (const [what, narrow, clear] of [
    ["find", { find: "b" }, { find: "" }],
    ["filter", { filter: "unwatched" }, { filter: "all" }],
    ["scope", { scope: "2" }, { scope: "" }],
  ]) {
    const { run, api, nodes } = libraryHarness([batchA, batchB], {});
    const view = { title: "Films", href: "#/lib/1", libIds: [1, 2], sort: "title", resort: null };
    await run(view);
    assert.deepEqual(nodes.libbody.grid.children.map((c) => c.id), ["a1", "a2", "b1", "b2", "b3"]);
    api.narrow(narrow); api.redraw();
    assert.deepEqual(nodes.libbody.grid.children.map((c) => c.id), ["b1", "b2", "b3"], `${what} did not narrow the grid`);
    api.narrow(clear); api.redraw();
    assert.deepEqual(nodes.libbody.grid.children.map((c) => c.id), ["a1", "a2", "b1", "b2", "b3"],
      `clearing the ${what} appended to the narrowed grid instead of rebuilding it`);
  }
  console.log("Library batches extend the grid they own and rebuild the one they do not.");

  // F-web-15. One rule, in the base stylesheet, covering both properties the
  // hover animates — and no per-layout copy of it left to drift.
  const css = shell.css;
  const reduce = [...css.matchAll(/@media \(prefers-reduced-motion: reduce\)\{([\s\S]*?)\n  \}/g)];
  assert.equal(reduce.length, 1, "there is more than one poster reduced-motion rule");
  assert.match(reduce[0][1], /\n\s*\.poster\{transition:none\}/, "the shared rule does not stop the transition");
  assert.match(reduce[0][1], /\n\s*\.poster:hover\{transform:none\}/, "the shared rule does not stop the transform");
  assert.doesNotMatch(css, /\[data-layout=(?:catalog|theater)\] \.poster\{transition:none\}/,
    "a per-layout reduced-motion override survived the shared rule");
  // The rule it opts out of must still be the unconditional one, or the opt-out
  // is guarding something that no longer moves.
  assert.match(css, /\.poster\{[^}]*transition:transform \.12s,border-color \.12s/);
  assert.match(css, /\.poster:hover\{transform:translateY\(-3px\)/);
  console.log("One shared reduced-motion rule covers the poster hover on every layout.");

  // F-web-11's sibling: a poster image decodes off the paint path.
  assert.match(declaration("artHtml"), /<img class="art \$\{cls\|\|''\}" loading="lazy" decoding="async"/,
    "the poster image lost decoding=\"async\"");
  console.log("Poster images decode asynchronously.");
})().catch((error) => { console.error(error); process.exit(1); });
