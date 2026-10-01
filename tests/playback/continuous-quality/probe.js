/* Investigative CQ0 only: integrate measurements into playback-lab later. */
'use strict';
const video = document.querySelector('video');
const scenario = new URLSearchParams(location.search).get('case') || 'switch';
const native = new URLSearchParams(location.search).has('native');
const receipt = window.receipt = {
  scenario, native, userAgent: navigator.userAgent, events: [], appends: [], frames: [],
  identity: { elements: 1, hls: 0, mediaSources: 0, audioBuffers: 0, removals: 0 },
  limits: { displayCapture: 'not measured', audioCapture: 'not measured',
    pixels: 'canvas diagnostic only; not proof of display output',
    admission: 'local refusal only; no production resource admission exercised' },
};
const event = (type, detail = {}) => receipt.events.push({ type, wall: performance.now(), time: video.currentTime, ...detail });
const ranges = (r) => Array.from({ length: r.length }, (_, i) => [r.start(i), r.end(i)]);
let videoBuffer, hls, intent = 0, targetAppended = false, switched = false;
const originalSource = MediaSource.prototype.addSourceBuffer;
MediaSource.prototype.addSourceBuffer = function(mime) {
  const sb = originalSource.call(this, mime);
  if (mime.startsWith('video/')) videoBuffer = sb;
  if (mime.startsWith('audio/')) receipt.identity.audioBuffers++;
  const append = sb.appendBuffer.bind(sb), remove = sb.remove.bind(sb);
  let pendingAppend;
  // Install before returning the SourceBuffer: hls.js completion listeners run later.
  sb.addEventListener('updateend', () => {
    if (!pendingAppend) return;
    receipt.appends.push({ ...pendingAppend, after: ranges(sb.buffered), wall: performance.now() });
    pendingAppend = null;
  });
  sb.appendBuffer = (data) => {
    pendingAppend = { mime, before: ranges(sb.buffered), intent };
    try { return append(data); } catch (e) { pendingAppend = null; throw e; }
  };
  sb.remove = (...args) => { receipt.identity.removals++; event('buffer-remove', { mime, args }); return remove(...args); };
  return sb;
};
for (const name of ['playing', 'waiting', 'stalled', 'seeking', 'seeked', 'pause', 'ended', 'error', 'resize']) video.addEventListener(name, () => event(name, { width: video.videoWidth, height: video.videoHeight }));
const canvas = document.createElement('canvas'); canvas.width = 32; canvas.height = 18;
const ctx = canvas.getContext('2d', { willReadFrequently: true });
if (video.requestVideoFrameCallback) {
  const frame = (wall, metadata) => {
    ctx.drawImage(video, 0, 0, 32, 18);
    const pixels = ctx.getImageData(0, 0, 32, 18).data;
    let hash = 2166136261, luma = 0;
    for (let i = 0; i < pixels.length; i += 4) { hash = Math.imul(hash ^ pixels[i], 16777619); luma += pixels[i] + pixels[i + 1] + pixels[i + 2]; }
    receipt.frames.push({ wall, pts: metadata.mediaTime, width: video.videoWidth, height: video.videoHeight, hash: hash >>> 0, mean: luma / (32 * 18 * 3), presentedFrames: metadata.presentedFrames });
    video.requestVideoFrameCallback(frame);
  };
  video.requestVideoFrameCallback(frame);
}
function request(level, reason) {
  intent++;
  const committed = videoBuffer ? ranges(videoBuffer.buffered) : ranges(video.buffered);
  event('intent', { intent, level, reason, committed, frontier: committed.at(-1)?.[1] ?? 0 });
  if (reason === 'denied') { event('retained-current', { intent, appended: false }); return; }
  // loadLevel sets the manual owner and future loading without currentLevel flushing.
  // nextLoadLevel alone leaves manualLevelIndex at its old value in 1.6.16.
  hls.loadLevel = level;
  event('scheduled', { intent, level, abrEnabled: hls.autoLevelEnabled });
}
window.start = async () => {
  if (native) {
    receipt.nativeExactControl = { videoTracks: typeof video.videoTracks, exactHeightAPI: false,
      constraint: 'HTMLMediaElement exposes no exact manual rendition selector; autonomous master only. No equivalence claimed for bitrate preference.' };
    video.src = '/media/master.m3u8'; await video.play(); event('native-attached'); return;
  }
  hls = new Hls({ autoStartLoad: false, startLevel: 0, maxBufferLength: scenario === 'long-buffer' ? 30 : 6, maxMaxBufferLength: scenario === 'long-buffer' ? 30 : 6, backBufferLength: 90 });
  receipt.hlsVersion = Hls.version; receipt.identity.hls++;
  hls.on(Hls.Events.MEDIA_ATTACHED, () => { receipt.identity.mediaSources++; hls.loadSource('/media/master.m3u8'); });
  hls.on(Hls.Events.MANIFEST_PARSED, () => { hls.loadLevel = 0; hls.startLoad(); video.play().catch(e => event('play-error', { message: e.message })); });
  hls.on(Hls.Events.ERROR, (_, data) => event('hls-error', { details: data.details, fatal: data.fatal }));
  hls.on(Hls.Events.FRAG_LOADING, (_, { frag }) => event('loading', { track: frag.type, level: frag.level, sn: frag.sn, start: frag.start, end: frag.end }));
  hls.on(Hls.Events.FRAG_BUFFERED, (_, { frag }) => {
    event('buffered', { track: frag.type, level: frag.level, sn: frag.sn, start: frag.start, end: frag.end, startPTS: frag.startPTS, endPTS: frag.endPTS, intent });
    if (frag.type === 'main' && frag.level === 1 && !targetAppended) {
      targetAppended = true;
      const actualAppend = receipt.appends.findLast(a => a.mime.startsWith('video/') && a.after.at(-1)?.[1] > (a.before.at(-1)?.[1] || 0));
      receipt.committedTarget = { start: actualAppend?.before.at(-1)?.[1] ?? actualAppend?.after[0]?.[0], end: actualAppend?.after.at(-1)?.[1], playlistStart: frag.start, fragmentPTS: frag.startPTS, intent, attachment: 1 };
      event('target-append-provenance', receipt.committedTarget);
      if (scenario === 'cancel-after-append') {
        event('cancel-after-append', { state: 'appended', settlement: 'observation_pending', ...receipt.committedTarget });
        request(0, 'superseding-intent');
      }
    }
  });
  hls.on(Hls.Events.LEVEL_SWITCHED, (_, data) => event('engine-level-switched', { level: data.level }));
  hls.attachMedia(video);
};
video.addEventListener('timeupdate', () => {
  if (!native && !switched && video.currentTime >= 3) {
    switched = true;
    if (scenario === 'baseline') { event('baseline-no-switch'); return; }
    if (scenario === 'cancel-before-append') {
      hls.stopLoad(); request(1, 'request'); request(0, 'cancel-before-append'); hls.startLoad(-1); event('retained-current', { appended: false });
    } else request(1, scenario === 'denied' ? 'denied' : 'request');
  }
});
window.snapshot = () => ({ ...receipt, currentTime: video.currentTime, width: video.videoWidth, height: video.videoHeight, buffered: ranges(video.buffered), paused: video.paused, error: video.error?.message || null, quality: video.getVideoPlaybackQuality?.() });
document.querySelector('button').onclick = () => start().catch(e => event('start-error', { message: e.message }));
