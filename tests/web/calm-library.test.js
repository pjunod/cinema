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
const english = new Function(`${declaration('englishTrackNote')}; return englishTrackNote;`)();
assert.match(english([{language: 'en-US'}], 'audio'), /available/);
assert.match(english([{language: 'fra'}, {language: null}], 'audio'), /not confirmed/);
assert.equal(english([{language: 'fra'}], 'subtitles'), 'No English subtitles');
assert.match(english([], 'audio', false), /not analyzed/);
const summary = new Function('prePlaySelection', 'audioFactLabel', 'subFactLabel',
  `${declaration('itemTrackSummary')}; return itemTrackSummary;`
)(id => id === 'file-b' ? {audio: 7, subtitle: -1} : null,
  track => track.language, track => track.language);
const file = {id: 'file-b', audio_streams: [{index: 0, language: 'French'}, {index: 7, language: 'English'}],
  subtitle_streams: [{index: 9, language: 'English'}],
  playback_defaults: {audio: {selected_index: 0}, subtitle: {selected_index: 9}}};
assert.equal(summary(file, 'audio'), 'English');
assert.equal(summary(file, 'subtitle'), 'Off');
assert.equal(summary({...file, id: 'file-a'}, 'audio'), 'French · default');
assert.equal(summary({...file, id: 'file-a'}, 'subtitle'), 'English · default');
console.log('Item regressions: unknown languages and per-file track choices passed.');

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
  assert.match(body, /viewingHubs\(p\)/);
  assert.match(body, /homeSavedHtml\(p\)/);
  assert.doesNotMatch(body, /calmHomeBody|<details/);
}
assert.match(declaration('viewingHubs'), /nextRows=next.filter/);
assert.match(declaration('homeSavedHtml'), /dvr-saved-card/);
assert.doesNotMatch(source, /calm-home|resume-strip|home-explore/);
console.log('All three Home layouts retain their original shelves and Theater feature.');
