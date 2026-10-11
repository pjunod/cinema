/* CQ0-only fMP4 sample inspection. Does not modify appended bytes. */
'use strict';
(() => {
  function view(bytes) {
    return bytes instanceof ArrayBuffer ? new DataView(bytes) : new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  function boxes(v, start = 0, end = v.byteLength) {
    const result = [];
    while (start + 8 <= end) {
      let size = v.getUint32(start), header = 8;
      const type = String.fromCharCode(...Array.from({ length: 4 }, (_, i) => v.getUint8(start + 4 + i)));
      if (size === 1) { if (start + 16 > end) throw new Error('truncated extended box'); size = Number(v.getBigUint64(start + 8)); header = 16; }
      if (size === 0) size = end - start;
      if (!Number.isSafeInteger(size) || size < header || start + size > end) throw new Error('invalid box bounds');
      result.push({ type, start: start + header, end: start + size }); start += size;
    }
    if (start !== end) throw new Error('truncated box tail');
    return result;
  }
  const children = (v, box, type) => boxes(v, box.start, box.end).filter(b => b.type === type);
  function inspector() {
    const tracks = new Map();
    return bytes => {
      const v = view(bytes), top = boxes(v), result = [];
      for (const moov of top.filter(b => b.type === 'moov')) {
        for (const trak of children(v, moov, 'trak')) {
          const tkhd = children(v, trak, 'tkhd')[0], mdia = children(v, trak, 'mdia')[0];
          if (!tkhd || !mdia) continue;
          const mdhd = children(v, mdia, 'mdhd')[0]; if (!mdhd) continue;
          const id = v.getUint32(tkhd.start + (v.getUint8(tkhd.start) === 1 ? 20 : 12));
          const scale = v.getUint32(mdhd.start + (v.getUint8(mdhd.start) === 1 ? 20 : 12));
          if (!scale) throw new Error('zero media timescale');
          tracks.set(id, { timescale: scale, width: v.getUint32(tkhd.end - 8) / 65536, height: v.getUint32(tkhd.end - 4) / 65536 });
        }
      }
      for (const moof of top.filter(b => b.type === 'moof')) for (const traf of children(v, moof, 'traf')) {
        const tfhd = children(v, traf, 'tfhd')[0], tfdt = children(v, traf, 'tfdt')[0];
        if (!tfhd || !tfdt) throw new Error('fragment lacks tfhd/tfdt');
        const flags = v.getUint32(tfhd.start) & 0xffffff, id = v.getUint32(tfhd.start + 4), track = tracks.get(id);
        if (!track) throw new Error('fragment track lacks init');
        let pos = tfhd.start + 8;
        if (flags & 1) pos += 8;
        if (flags & 2) pos += 4;
        const defaultDuration = flags & 8 ? v.getUint32(pos) : null;
        let dts = v.getUint8(tfdt.start) === 1 ? Number(v.getBigUint64(tfdt.start + 4)) : v.getUint32(tfdt.start + 4);
        if (!Number.isSafeInteger(dts)) throw new Error('unsafe decode timestamp');
        let start = Infinity, end = -Infinity, count = 0;
        for (const trun of children(v, traf, 'trun')) {
          const version = v.getUint8(trun.start), runFlags = v.getUint32(trun.start) & 0xffffff, samples = v.getUint32(trun.start + 4);
          let cursor = trun.start + 8;
          if (runFlags & 1) cursor += 4;
          if (runFlags & 4) cursor += 4;
          for (let i = 0; i < samples; i++) {
            const duration = runFlags & 0x100 ? v.getUint32(cursor) : defaultDuration;
            if (runFlags & 0x100) cursor += 4;
            if (runFlags & 0x200) cursor += 4;
            if (runFlags & 0x400) cursor += 4;
            const composition = runFlags & 0x800 ? (version === 1 ? v.getInt32(cursor) : v.getUint32(cursor)) : 0;
            if (runFlags & 0x800) cursor += 4;
            if (!duration || cursor > trun.end) throw new Error('unsupported duration or truncated sample table');
            const pts = (dts + composition) / track.timescale;
            start = Math.min(start, pts); end = Math.max(end, pts + duration / track.timescale);
            dts += duration; count++;
          }
          if (cursor !== trun.end) throw new Error('unexpected sample table bytes');
        }
        if (count) result.push({ ...track, track: id, start, end, samples: count });
      }
      return result;
    };
  }
  globalThis.CQMP4 = { inspector };
})();
