"use strict";
const assert = require('node:assert/strict');
const fs = require('node:fs');
const source = fs.readFileSync('crates/plurxd/src/web/index.html', 'utf8');
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
