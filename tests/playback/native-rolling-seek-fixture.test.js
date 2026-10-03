'use strict';
// Execute the fixture's actual script to reject false measurement evidence.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, 'native-rolling-seek-fixture.py'), 'utf8');
const html = source.match(/PAGE = r'''([\s\S]*?)'''/)[1];
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const handlers = {}, callbacks = new Map(), reports = [], controls = {};
let callbackId = 0, position = 0, now = 100, pendingState = null;
const range = { length: 1, start: () => 0, end: () => 100 };
const video = {
  buffered: range, seekable: range, seeking: false, paused: false, playbackRate: 1, readyState: 4,
  pause() { this.paused = true; }, play() { this.paused = false; return Promise.resolve(); },
  removeAttribute() {}, load() {}, addEventListener(name, fn) { handlers[name] = fn; },
  requestVideoFrameCallback(fn) { const id = ++callbackId; callbacks.set(id, fn); return id; },
  cancelVideoFrameCallback(id) { callbacks.delete(id); },
};
Object.defineProperty(video, 'currentTime', {
  get() { return position; }, set(value) { position = value; video.seeking = true; },
});
const context = vm.createContext({
  document: { querySelector(selector) { return selector === '#v' ? video : (controls[selector] ||= {}); } },
  performance: { now: () => now }, navigator: { userAgent: 'fixture-unit', sendBeacon(_, raw) { reports.push(JSON.parse(raw)); } },
  setTimeout() {}, setInterval() {},
  fetch(url) {
    if (url.startsWith('/state/') && pendingState) return pendingState;
    return Promise.resolve({ json: async () => ({ revision: 1, served_edge_s: 48 }) });
  },
});
vm.runInContext(script, context);
const api = vm.runInContext('({start,paired,seek,state:()=>({run,pending,frameCount,frameId})})', context);
(async () => {
  await api.start('baseline');
  video.pause();
  const seeking = api.seek(10);
  const id = api.state().frameId;
  now = 140;
  callbacks.get(id)(now, { mediaTime: 10 });
  assert.equal(api.state().pending.target, 10, 'one destination frame waits for seeked');
  video.seeking = false;
  handlers.seeked();
  await seeking;
  const frame = reports.find(row => row.event.event === 'target-frame');
  assert.equal(frame.event.media_time_s, 10);
  assert.equal(frame.event.click_latency_ms, 40);
  assert.equal(api.state().pending, null, 'paused sole frame settles without Play or a second frame');

  let resolveState;
  pendingState = new Promise(resolve => { resolveState = resolve; });
  const stale = api.paired('stale-observer');
  pendingState = null;
  const staleFrame = callbacks.get(api.state().frameId);
  await api.start('short');
  const count = api.state().frameCount;
  staleFrame(now, { mediaTime: 10 });
  assert.equal(api.state().frameCount, count, 'retired attachment callback cannot advance current evidence');
  resolveState({ json: async () => ({ revision: 99, served_edge_s: 999 }) });
  await stale;
  assert.equal(reports.some(row => row.event.event === 'stale-observer'), false,
    'old served state cannot be tagged with the new attachment');
  console.log('native rolling fixture: sole-frame and attachment/sample fences passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
