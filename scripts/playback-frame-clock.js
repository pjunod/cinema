"use strict";

// An optical clock encoded into the generated video, never a DOM overlay.
// Read only cell centers; an uncertain cell, guard or checksum is unknown.
const FRAME_CLOCK = Object.freeze({version: 1, fps: 24, x: 624, y: 508,
  width: 672, height: 64, readWidth: 336, readHeight: 4, guard: 211});

function frameClockChecksum(frame) {
  return ((frame & 255) + ((frame >>> 8) & 255) * 31 + ((frame >>> 16) & 255) * 17 + 17) & 255;
}

function frameClockFilter(seconds) {
  if (!Number.isInteger(seconds) || seconds < 1 || seconds > 3600) throw new Error("Invalid optical-clock duration");
  const index = "X-1";
  const checksum = "mod(mod(N,256)+mod(floor(N/256),256)*31+floor(N/65536)*17+17,256)";
  const value = `if(lt(ld(0),8),211,if(lt(ld(0),32),N,${checksum}))`;
  const shift = "if(lt(ld(0),8),ld(0),if(lt(ld(0),32),ld(0)-8,ld(0)-32))";
  const lum = `st(0,${index});if(lt(X,1)+gte(X,41),16,if(bitand(${value},pow(2,${shift})),235,16))`;
  return `color=c=black:s=42x2:r=24:d=${seconds},geq=lum='${lum}':cb=128:cr=128,scale=672:64:flags=neighbor[optical_clock];`
    + "[0:v][optical_clock]overlay=x=624:y=508:shortest=1[clocked_video]";
}

function decodeFrameClockRgb(bytes) {
  const {readWidth: width, readHeight: height} = FRAME_CLOCK;
  if (!(bytes instanceof Uint8Array) || bytes.length !== width * height * 3) return null;
  function bit(cell) {
    // The 16px quiet border and cells become 8px after the 2:1 reduction.
    const x = 8 + cell * 8 + 4;
    let total = 0;
    for (let y = 0; y < height; y++) for (let channel = 0; channel < 3; channel++) {
      total += bytes[(y * width + x) * 3 + channel];
    }
    const average = total / (height * 3);
    return average <= 60 ? 0 : average >= 195 ? 1 : null;
  }
  function word(offset, length) {
    let value = 0;
    for (let i = 0; i < length; i++) {
      const valueBit = bit(offset + i);
      if (valueBit === null) return null;
      value += valueBit * 2 ** i;
    }
    return value;
  }
  const guard = word(0, 8), frame = word(8, 24), checksum = word(32, 8);
  if (guard !== FRAME_CLOCK.guard || frame === null || checksum !== frameClockChecksum(frame)) return null;
  return frame;
}

// The explicit capture window is mandatory. Missing pixels/timestamps and
// capture holes remain unknown; no averaging over skipped callbacks or frames.
function analyzeFrameClock(samples, {startMs, endMs, maximumSamplingGapMs = 12.5} = {}) {
  if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || startMs >= endMs
      || !Number.isFinite(maximumSamplingGapMs) || maximumSamplingGapMs <= 0) {
    throw new Error("Optical capture requires an explicit valid measurement window");
  }
  const result = {scope: "encoded optical clock in captured pixels; physical output depends on capture surface",
    complete: true, samples: 0, unknown_samples: 0, capture_gaps: 0, backward_frames: 0,
    skipped_counter_values: 0, maximum_hold_lower_ms: 0, maximum_hold_upper_ms: 0};
  let previous = null, runFirst = null, runBefore = null, lastAt = null;
  for (const sample of samples) {
    if (!Number.isFinite(sample?.at_ms) || (lastAt !== null && sample.at_ms <= lastAt)) {
      result.complete = false; result.capture_gaps++; continue;
    }
    lastAt = sample.at_ms;
    if (sample.at_ms < startMs || sample.at_ms > endMs) continue;
    result.samples++;
    const frame = sample.frame;
    if (!Number.isSafeInteger(frame) || frame < 0 || frame > 0xffffff) {
      result.unknown_samples++; result.complete = false;
      previous = null; runFirst = null; runBefore = null; continue;
    }
    if (!previous) {
      if (sample.at_ms - startMs > maximumSamplingGapMs) result.complete = false;
      runFirst = sample; previous = sample; continue;
    }
    if (sample.at_ms - previous.at_ms > maximumSamplingGapMs) {
      result.capture_gaps++; result.complete = false;
    }
    if (frame === previous.frame) {
      result.maximum_hold_lower_ms = Math.max(result.maximum_hold_lower_ms, sample.at_ms - runFirst.at_ms);
    } else {
      if (frame < previous.frame) result.backward_frames++;
      else if (frame > previous.frame + 1) result.skipped_counter_values += frame - previous.frame - 1;
      // The previous different sample bounds when this image could have begun.
      result.maximum_hold_upper_ms = Math.max(result.maximum_hold_upper_ms, sample.at_ms - (runBefore?.at_ms ?? startMs));
      runBefore = previous; runFirst = sample;
    }
    previous = sample;
  }
  if (!previous || endMs - previous.at_ms > maximumSamplingGapMs) result.complete = false;
  if (runFirst && previous) {
    result.maximum_hold_lower_ms = Math.max(result.maximum_hold_lower_ms, previous.at_ms - runFirst.at_ms);
    result.maximum_hold_upper_ms = Math.max(result.maximum_hold_upper_ms, endMs - (runBefore?.at_ms ?? startMs));
  }
  return result;
}

module.exports = {FRAME_CLOCK, frameClockChecksum, frameClockFilter, decodeFrameClockRgb, analyzeFrameClock};
