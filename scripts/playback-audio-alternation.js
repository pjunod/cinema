"use strict";

// Audio-language alternation acquisition for the playback lab.
// Pictures: the encoded optical frame clock read back from the decoded
// video element on every animation frame. Sound: decoded PCM from a
// test-only AudioWorklet, classified by the fixture's per-language tone.
// Both are captured independently of player telemetry; neither is a
// physical display or speaker measurement.

const {FRAME_CLOCK, decodeFrameClockRgb, analyzeFrameClock} = require("./playback-frame-clock");

const TONES = Object.freeze({eng: 440, fre: 660});
const BLOCK = 480; // 10 ms at 48 kHz
const DISPLAY_SAMPLE_GAP_MS = 25; // rAF on a 60 Hz surface plus scheduling slack

// Runs in the page. Installs one capture for the current video element.
function installAlternationCapture(clock, tones, block, decodeSource) {
  const decode = new Function(`const FRAME_CLOCK=${JSON.stringify(clock)};
    function frameClockChecksum(frame){return ((frame&255)+((frame>>>8)&255)*31+((frame>>>16)&255)*17+17)&255;}
    return (${decodeSource});`)();
  const v = document.getElementById("video");
  const capture = {schema_version: 1, element: v, frames: [], audio: [], marks: [], errors: [],
    sessions: [], hls_objects: 0, stopped: false};
  const hlsIds = new WeakMap();
  const canvas = document.createElement("canvas");
  canvas.width = clock.readWidth; canvas.height = clock.readHeight;
  const ctx = canvas.getContext("2d", {willReadFrequently: true});
  const rgb = new Uint8Array(clock.readWidth * clock.readHeight * 3);
  const tick = (now) => {
    if (capture.stopped) return;
    const current = document.getElementById("video");
    const p = typeof PLAYER !== "undefined" ? PLAYER : null;
    let frame = null;
    try {
      ctx.drawImage(current, clock.x, clock.y, clock.width, clock.height, 0, 0, clock.readWidth, clock.readHeight);
      const rgba = ctx.getImageData(0, 0, clock.readWidth, clock.readHeight).data;
      for (let i = 0, j = 0; i < rgba.length; i += 4) { rgb[j++] = rgba[i]; rgb[j++] = rgba[i + 1]; rgb[j++] = rgba[i + 2]; }
      frame = decode(rgb);
    } catch (e) { if (capture.errors.length < 16) capture.errors.push(String(e && e.message || e)); }
    if (p && p.hls && !hlsIds.has(p.hls)) hlsIds.set(p.hls, ++capture.hls_objects);
    capture.frames.push({at_ms: now, frame, same_element: current === v,
      session_id: p ? p.sessionId || null : null, hls_id: p && p.hls ? hlsIds.get(p.hls) : null,
      media_time: Number.isFinite(current.currentTime) ? current.currentTime : null,
      seeking: !!current.seeking});
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  const worklet = `class AltTone extends AudioWorkletProcessor{
    constructor(){super();this.n=0;this.acc=[0,0,0,0,0];this.at=0;}
    process(inputs,outputs){for(const c of outputs[0]||[])c.fill(0);
      const input=inputs[0]&&inputs[0][0];if(!input)return true;
      for(let i=0;i<input.length;i++){const t=(currentFrame+i)/sampleRate,x=input[i];
        if(this.n===0)this.at=currentFrame+i;
        this.acc[0]+=x*Math.cos(2*Math.PI*${tones.eng}*t);this.acc[1]+=x*Math.sin(2*Math.PI*${tones.eng}*t);
        this.acc[2]+=x*Math.cos(2*Math.PI*${tones.fre}*t);this.acc[3]+=x*Math.sin(2*Math.PI*${tones.fre}*t);
        this.acc[4]+=x*x;
        if(++this.n===${block}){const a=this.acc,n=this.n;
          this.port.postMessage({frame:this.at,eng:Math.hypot(a[0],a[1])*2/n,fre:Math.hypot(a[2],a[3])*2/n,rms:Math.sqrt(a[4]/n)});
          this.n=0;this.acc=[0,0,0,0,0];}}
      return true;}}
    registerProcessor('alt-tone',AltTone);`;
  capture.ready = (async () => {
    const audio = new AudioContext({sampleRate: 48000});
    await audio.audioWorklet.addModule(URL.createObjectURL(new Blob([worklet], {type: "text/javascript"})));
    const node = new AudioWorkletNode(audio, "alt-tone");
    audio.createMediaElementSource(v).connect(node).connect(audio.destination);
    node.port.onmessage = (e) => {
      if (capture.stopped) return;
      const stamp = audio.getOutputTimestamp();
      // Map the block's context frame to the page clock through the latest output timestamp.
      const at = stamp.performanceTime + (e.data.frame / audio.sampleRate - stamp.contextTime) * 1000;
      const {eng, fre, rms} = e.data;
      const label = rms < 0.02 ? "silence" : eng > 4 * fre ? "eng" : fre > 4 * eng ? "fre" : "mixed";
      capture.audio.push({at_ms: at, label, eng, fre, rms});
    };
    await audio.resume();
    capture.audio_context = audio;
    return true;
  })();
  capture.mark = (kind, detail) => capture.marks.push({kind, at_ms: performance.now(), ...detail});
  capture.stop = () => { capture.stopped = true; try { capture.audio_context && capture.audio_context.close(); } catch (e) {} };
  capture.drain = () => {
    const out = {frames: capture.frames, audio: capture.audio, marks: capture.marks, errors: capture.errors,
      hls_objects: capture.hls_objects};
    capture.frames = []; capture.audio = []; capture.marks = [];
    return out;
  };
  globalThis.__plurxLabAlternation = capture;
  return capture.ready;
}

function pageInstaller() {
  return `return (${installAlternationCapture.toString()})(${JSON.stringify(FRAME_CLOCK)},${JSON.stringify(TONES)},${BLOCK},${JSON.stringify(decodeFrameClockRgb.toString())});`;
}

function percentile(values, p) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(p / 100 * sorted.length) - 1)];
}

// The first time after `fromMs` that `label` holds for `holdMs` of contiguous blocks.
function confirmedAudio(audio, label, fromMs, untilMs = Infinity, holdMs = 100) {
  let runStart = null;
  for (const row of audio) {
    if (row.at_ms < fromMs) continue;
    if (row.at_ms > untilMs) break;
    if (row.label === label) {
      if (runStart === null) runStart = row.at_ms;
      if (row.at_ms - runStart >= holdMs) return runStart;
    } else runStart = null;
  }
  return null;
}

function silenceBetween(audio, startMs, endMs) {
  let total = 0, previous = null;
  for (const row of audio) {
    if (row.at_ms < startMs || row.at_ms > endMs) continue;
    if (row.label === "silence") total += previous === null ? 10 : Math.min(10, row.at_ms - previous);
    previous = row.at_ms;
  }
  return total;
}

// Scores every switch in one window around it. A switch is healthy only when
// the picture kept advancing on one element, attachment and session, and the
// requested language became audible.
function scoreAlternation(capture, switches, {baselineHoldMs = null, windowAfterMs = 1500} = {}) {
  const frames = capture.frames, audio = capture.audio;
  const results = switches.map((change, index) => {
    const next = switches[index + 1];
    const confirmed = confirmedAudio(audio, change.target_label, change.tap_ms, next ? next.tap_ms : Infinity);
    const end = Math.min(next ? next.tap_ms : Infinity, (confirmed ?? change.tap_ms) + windowAfterMs);
    const start = change.tap_ms - 250;
    const window = frames.filter(f => f.at_ms >= start && f.at_ms <= end);
    const picture = window.length ? analyzeFrameClock(window, {startMs: start, endMs: end, maximumSamplingGapMs: DISPLAY_SAMPLE_GAP_MS}) : null;
    const sessions = [...new Set(window.map(f => f.session_id))];
    const hls = [...new Set(window.map(f => f.hls_id))];
    const elementChanged = window.some(f => !f.same_element);
    const seeking = window.some(f => f.seeking);
    const silence = silenceBetween(audio, change.tap_ms, confirmed ?? end);
    const heard = {};
    for (const row of audio) if (row.at_ms >= change.tap_ms && row.at_ms <= end) heard[row.label] = (heard[row.label] || 0) + 1;
    const holdLimit = baselineHoldMs === null ? null : baselineHoldMs + 1000 / FRAME_CLOCK.fps;
    const failures = [];
    if (confirmed === null) failures.push("requested language never confirmed audible");
    if (!picture || !picture.complete) failures.push("picture capture incomplete (inconclusive)");
    if (picture && picture.backward_frames) failures.push("backward pictures");
    if (picture && picture.skipped_counter_values) failures.push("skipped pictures");
    if (picture && holdLimit !== null && picture.maximum_hold_lower_ms > holdLimit) failures.push("held picture");
    if (sessions.length !== 1) failures.push("session changed");
    if (hls.length !== 1) failures.push("hls instance changed");
    if (elementChanged) failures.push("media element replaced");
    if (seeking) failures.push("seek during switch");
    if (silence > 20) failures.push("introduced silence");
    return {index, target: change.target_label, tap_ms: change.tap_ms,
      delay_ms: confirmed === null ? null : confirmed - change.tap_ms,
      silence_ms: silence, heard_blocks: heard, sessions, hls_instances: hls, element_changed: elementChanged,
      seeking, picture, healthy: failures.length === 0, failures};
  });
  const delays = results.map(r => r.delay_ms).filter(Number.isFinite);
  let consecutive = 0, best = 0;
  for (const r of results) { consecutive = r.healthy ? consecutive + 1 : 0; best = Math.max(best, consecutive); }
  return {switches: results, healthy: results.filter(r => r.healthy).length,
    longest_healthy_run: best,
    delay_ms: {p50: percentile(delays, 50), p95: percentile(delays, 95), worst: delays.length ? Math.max(...delays) : null},
    capture_errors: capture.errors, scope: "decoded-element pixels and decoded PCM; physical output not measured"};
}

// Drives `count` alternating switches through the menu's own handler.
async function runAlternation(driver, testCase, start, {waitFor}) {
  if (start.audio_count < 2) throw new Error("fixture has fewer than two audio tracks");
  const count = Number(testCase.switches || 20);
  const spacing = Number(testCase.switch_spacing_ms || 3000);
  await driver.exec(pageInstaller());
  await waitFor(async () => (await driver.eval("(()=>{const c=globalThis.__plurxLabAlternation;return c&&c.audio.length>50})()")) || null,
    10_000, "audio capture warm-up");
  // Calibrate the uninterrupted picture before the first switch.
  const calibrationMs = Number(testCase.calibration_ms || 4000);
  await new Promise(r => setTimeout(r, calibrationMs));
  const capture = {frames: [], audio: [], errors: []};
  const absorb = (part) => { capture.frames.push(...part.frames); capture.audio.push(...part.audio); capture.errors.push(...part.errors); };
  absorb(await driver.eval("globalThis.__plurxLabAlternation.drain()"));
  const calibrationEnd = capture.frames.at(-1)?.at_ms;
  const baseline = capture.frames.length ? analyzeFrameClock(capture.frames,
    {startMs: capture.frames[0].at_ms, endMs: calibrationEnd, maximumSamplingGapMs: DISPLAY_SAMPLE_GAP_MS}) : null;
  const switches = [];
  for (let i = 0; i < count; i++) {
    const target = (Number(start.current_audio || 0) + i + 1) % 2;
    const tap = await driver.eval(`(()=>{const at=performance.now();switchAudio(${target});return at})()`);
    switches.push({target, target_label: target === 0 ? "eng" : "fre", tap_ms: tap});
    await new Promise(r => setTimeout(r, spacing));
    absorb(await driver.eval("globalThis.__plurxLabAlternation.drain()"));
  }
  await driver.exec("globalThis.__plurxLabAlternation.stop();return true;");
  const score = scoreAlternation(capture, switches, {baselineHoldMs: baseline ? baseline.maximum_hold_lower_ms : null});
  return {calibration: baseline, ...score};
}

module.exports = {TONES, BLOCK, installAlternationCapture, pageInstaller, confirmedAudio, silenceBetween,
  scoreAlternation, runAlternation};
