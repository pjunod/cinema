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
const recent = new Function(`${declaration('homeRecentItems')}; return homeRecentItems;`)();
const page = {
  libs: [{id: '9007199254740993', kind: 'movies'}, {id: '9007199254740994', kind: 'recordings'}],
  hubs: {recently_added: [
    {id: '1', library_id: '9007199254740994', title: 'Movie-looking DVR title'},
    {id: '2', library_id: '9007199254740993', recorded_at: '2026-09-15'},
    {id: '3', library_id: 'unknown'},
  ]},
};
assert.deepEqual(recent(page).map(item => item.id), ['2']);
assert.deepEqual(recent({...page, previewsPending: true}), []);
assert.deepEqual(recent({...page, previewsError: true}), []);
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
assert.doesNotMatch(declaration('calmHomeBody'), /next_up|theaterHero|watch-cards/);
console.log('Calm library regressions: recording ownership, unknown languages, per-file choices, and compact Home passed.');

const hero = new Function('homeRecentItems',
  `const THEATER_HERO_KINDS={movie:1,episode:1,video:1}; ${declaration('theaterHeroPick')}; return theaterHeroPick;`)(recent);
const continued = {id:'continue',kind:'episode',title:'The episode I was watching'};
assert.equal(hero({...page,hubs:{...page.hubs,continue_watching:[continued]}}).item,continued);
assert.equal(hero({...page,hubs:{...page.hubs,continue_watching:[continued]}}).resuming,true);
assert.equal(hero({...page,hubs:{recently_added:[],next_up:[continued]}}),null);
assert.match(declaration('theaterHomeBody'), /theaterHeroHtml/);
assert.match(declaration('theaterHomeBody'), /data-home-slot="hero"/);
console.log('Theater retains its recently played feature without promoting Next up.');
