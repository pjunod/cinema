import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { summarize } from '../../../scripts/continuous-quality-lab.mjs';
const directory = process.env.CQ_RECEIPTS;
function receipt(name) {
  const current = path.join(directory, name + '.json');
  const historical = path.join(directory, 'range-growth-series', name + '.json');
  return JSON.parse(fs.readFileSync(fs.existsSync(current) ? current : historical, 'utf8'));
}
const base = { events: [], frames: [], identity: {}, limits: { audioCapture: 'not measured' } };
test('missing target append remains unknown rather than passing a frontier check', () => {
  const result = summarize(base);
  assert.equal(result.appendFrontierRespected, null);
  assert.equal(result.firstTarget, null);
  assert.equal(result.capture.audioCapture, 'not measured');
});
test('a buffered target is not reported as presented without a target frame', () => {
  const result = summarize({ ...base, committedTarget: { start: 12, end: 14 }, events: [{ type: 'intent', level: 1, frontier: 12, wall: 50 }] });
  assert.equal(result.appendFrontierRespected, true);
  assert.equal(result.firstTarget, null);
});
test('an overlapping append is rejected even if the target later presents', () => {
  const result = summarize({ ...base, committedTarget: { start: 10, end: 12 }, events: [{ type: 'intent', level: 1, frontier: 11, wall: 50 }], frames: [{ wall: 100, height: 1080 }] });
  assert.equal(result.appendFrontierRespected, false);
});
function invariant(r) {
  assert.equal(r.hlsVersion, '1.6.16');
  assert.deepEqual(r.identity, { elements: 1, hls: 1, mediaSources: 1, audioBuffers: 1, removals: 0 });
  assert.equal(r.requests.filter(q => q.path === '/media/audio/init-audio.mp4').length, 1);
  assert.equal(r.limits.audioCapture, 'not measured');
  assert.equal(r.limits.displayCapture, 'not measured');
  assert.ok(r.frames.length > 200, 'actual moving-frame observations required');
  assert.ok(r.frames.some((f, i) => i && f.hash !== r.frames[i - 1].hash));
}
test('real same-player switch appends beyond the measured video frontier', { skip: !directory }, () => {
  const r = receipt('chrome-switch'); invariant(r);
  const summary = summarize(r);
  assert.equal(summary.appendFrontierRespected, true);
  assert.ok(summary.firstTarget);
  assert.equal(r.committedTarget.sampleBoundEvidence, true);
  assert.equal(r.events.some(e => e.type === 'sample-inspection-error'), false);
  assert.ok(r.committedIntervals.some(i => i.height === 1080 && i.samples === 48));
  assert.ok(summary.movingTargetFrames > 50);
  assert.equal(r.events.filter(e => e.type === 'scheduled').every(e => !e.abrEnabled), true);
});
test('real cancellation before append leaves the incumbent moving', { skip: !directory }, () => {
  const r = receipt('chrome-cancel-before-append'); invariant(r);
  assert.equal(r.committedTarget, undefined);
  assert.equal(summarize(r).firstTarget, null);
  assert.equal(r.height, 720);
});
test('real cancellation after append still presents committed target then latest intent', { skip: !directory }, () => {
  const r = receipt('chrome-cancel-after-append'); invariant(r);
  assert.equal(summarize(r).appendFrontierRespected, true);
  assert.ok(summarize(r).firstTarget);
  assert.equal(r.height, 720);
  const cancel = r.events.find(e => e.type === 'cancel-after-append');
  assert.equal(cancel.settlement, 'observation_pending');
  assert.ok(r.frames.some(f => f.wall > cancel.wall && f.height === 1080));
});
test('real local denied selection keeps incumbent and performs no target request', { skip: !directory }, () => {
  const r = receipt('chrome-denied'); invariant(r);
  assert.equal(r.height, 720);
  assert.equal(r.requests.some(q => q.path.startsWith('/media/1080/')), false);
});
test('real long prebuffer preserves committed media beyond the thirty-second request budget', { skip: !directory }, () => {
  const r = receipt('chrome-long-buffer'); invariant(r);
  const summary = summarize(r);
  assert.equal(summary.appendFrontierRespected, true);
  assert.ok(summary.targetDelayMs > 30000);
  assert.ok(summary.firstTarget.pts >= summary.request.frontier - 0.002);
});
test('real fixture decodes independent segments on one rational grid with no encoded audio timestamp gap', { skip: !directory }, () => {
  const r = receipt('media-verification');
  assert.equal(r.aligned, true);
  assert.equal(r.rows.length, 48);
  assert.equal(r.audio.timestampGaps, 0);
});

test('real decoded PCM switch matches its no-switch baseline without a silent run', { skip: !directory }, () => {
  const baseline = receipt('chrome-audio-baseline').audioCapture;
  const switched = receipt('chrome-audio-switch').audioCapture;
  assert.equal(switched.contiguousClock, true);
  assert.equal(switched.sampleRate, 48000);
  assert.ok(switched.analysis.measuredSamples > 700000);
  assert.ok(switched.analysis.maximumNearZeroRunSamples <= baseline.analysis.maximumNearZeroRunSamples);
  assert.ok(switched.analysis.maximumAdjacentSampleDelta <= baseline.analysis.maximumAdjacentSampleDelta + 0.0001);
  assert.equal(switched.physicalOutput, 'not measured');
});
