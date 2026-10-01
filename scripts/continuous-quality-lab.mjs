#!/usr/bin/env node
// Isolated CQ0 prototype; production acceptance remains playback-lab.
import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const OUT = path.join(ROOT, 'target/continuous-quality');
const MEDIA = path.join(OUT, 'media');
const sleep = ms => new Promise(r => setTimeout(r, ms));
function command(bin, args) {
  const r = spawnSync(bin, args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  if (r.error || r.status !== 0) throw new Error(`${bin}: ${r.error?.message || r.stderr}`);
  return r.stdout;
}
async function files(dir) {
  const result = [];
  for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
    const name = path.join(dir, entry.name);
    if (entry.isDirectory()) result.push(...await files(name)); else result.push(name);
  }
  return result.sort();
}
async function fixture() {
  await fs.mkdir(MEDIA, { recursive: true });
  // A deterministic frame number lane survives scaling and exposes held frames.
  const glyphs = ['111101101101111','010110010010111','111001111100111','111001111001111','101101111001001','111100111001111','111100111101111','111001001001001','111101111101111','111101111001111'];
  const count = Math.ceil(48 * 24000 / 1001), width = 160, height = 32;
  const lane = Buffer.alloc(count * width * height * 3);
  for (let frame = 0; frame < count; frame++) {
    const digits = String(frame).padStart(6, '0');
    for (let d = 0; d < digits.length; d++) for (let y = 0; y < 5; y++) for (let x = 0; x < 3; x++) {
      if (glyphs[Number(digits[d])][y * 3 + x] !== '1') continue;
      for (let dy = 0; dy < 4; dy++) for (let dx = 0; dx < 4; dx++) {
        const offset = ((frame * height + y * 4 + dy + 4) * width + d * 20 + x * 4 + dx + 8) * 3;
        lane.fill(255, offset, offset + 3);
      }
    }
  }
  const lanePath = path.join(OUT, 'frame-number.rgb'); await fs.writeFile(lanePath, lane);
  const common = ['-hide_banner', '-loglevel', 'error', '-y'];
  for (const [rung, size] of [['720', '1280x720'], ['1080', '1920x1080']]) {
    const dir = path.join(MEDIA, rung); await fs.mkdir(dir, { recursive: true });
    command('ffmpeg', [...common, '-f', 'lavfi', '-i', `testsrc2=size=${size}:rate=24000/1001:duration=48`, '-f', 'rawvideo', '-pixel_format', 'rgb24', '-video_size', '160x32', '-framerate', '24000/1001', '-i', lanePath, '-filter_complex', '[0:v][1:v]overlay=32:32:shortest=1[v]', '-map', '[v]', '-an', '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '25', '-pix_fmt', 'yuv420p', '-profile:v', 'high', '-level:v', '4.0', '-g', '48', '-keyint_min', '48', '-sc_threshold', '0', '-bf', '2', '-flags', '+cgop', '-f', 'hls', '-hls_time', '2.002', '-hls_playlist_type', 'vod', '-hls_segment_type', 'fmp4', '-hls_flags', 'independent_segments', '-hls_fmp4_init_filename', `init-${rung}.mp4`, '-hls_segment_filename', path.join(dir, 'seg%d.m4s'), path.join(dir, 'index.m3u8')]);
  }
  const audio = path.join(MEDIA, 'audio'); await fs.mkdir(audio, { recursive: true });
  command('ffmpeg', [...common, '-f', 'lavfi', '-i', 'sine=frequency=997:sample_rate=48000:duration=48', '-vn', '-c:a', 'aac', '-b:a', '128k', '-f', 'hls', '-hls_time', '2.002', '-hls_playlist_type', 'vod', '-hls_segment_type', 'fmp4', '-hls_fmp4_init_filename', 'init-audio.mp4', '-hls_segment_filename', path.join(audio, 'seg%d.m4s'), path.join(audio, 'index.m3u8')]);
  await fs.writeFile(path.join(MEDIA, 'master.m3u8'), '#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="shared",NAME="Reference",DEFAULT=YES,AUTOSELECT=YES,URI="audio/index.m3u8"\n#EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1280x720,CODECS="avc1.640028,mp4a.40.2",AUDIO="shared"\n720/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=8000000,RESOLUTION=1920x1080,CODECS="avc1.640028,mp4a.40.2",AUDIO="shared"\n1080/index.m3u8\n');
  const hash = createHash('sha256');
  for (const file of await files(MEDIA)) hash.update(path.relative(MEDIA, file)).update(await fs.readFile(file));
  const identity = { sha256: hash.digest('hex'), ffmpeg: command('ffmpeg', ['-version']).split('\n')[0], frameRate: '24000/1001', segmentFrames: 48, segmentSeconds: 2.002, audio: 'one 48kHz AAC encode, shared by both video rungs', limitations: ['synthetic CFR only', 'nonzero origin/VFR/native devices not qualified', 'no real admission or JIT producers'] };
  await fs.writeFile(path.join(OUT, 'fixture.json'), JSON.stringify(identity, null, 2));
  console.log(JSON.stringify(identity, null, 2));
}
async function verifyMedia() {
  const rows = [];
  for (const rung of ['720', '1080']) {
    const dir = path.join(MEDIA, rung);
    const segments = (await files(dir)).filter(f => /seg\d+\.m4s$/.test(f)).sort((a,b) => Number(a.match(/seg(\d+)/)[1]) - Number(b.match(/seg(\d+)/)[1]));
    let previousPTS = null;
    for (const seg of segments) {
      const temp = path.join(OUT, 'decode.mp4');
      await fs.writeFile(temp, Buffer.concat([await fs.readFile(path.join(dir, `init-${rung}.mp4`)), await fs.readFile(seg)]));
      const data = JSON.parse(command('ffprobe', ['-v', 'error', '-select_streams', 'v', '-show_frames', '-show_entries', 'frame=pts_time,pkt_dts_time,key_frame,pict_type,width,height', '-of', 'json', temp]));
      // ffprobe decoding alone can hide corruption; also perform a strict decode.
      command('ffmpeg', ['-v', 'error', '-xerror', '-i', temp, '-f', 'null', '-']);
      const frames = data.frames;
      for (const frame of frames) {
        const pts = Number(frame.pts_time);
        if (previousPTS !== null && Math.abs(pts - previousPTS - 1001 / 24000) > 0.000002) throw new Error(`Noncontinuous decoded PTS in ${rung}: ${seg}`);
        previousPTS = pts;
      }
      rows.push({ rung, segment: Number(seg.match(/seg(\d+)/)[1]), count: frames.length, first: frames[0], last: frames.at(-1) });
    }
  }
  const aligned = rows.filter(r => r.rung === '720').every(a => {
    const b = rows.find(r => r.rung === '1080' && r.segment === a.segment);
    return a.first.key_frame === 1 && b.first.key_frame === 1 && a.first.pts_time === b.first.pts_time && a.last.pts_time === b.last.pts_time && a.count === b.count;
  });
  const audioDir = path.join(MEDIA, 'audio');
  const audioSegments = (await files(audioDir)).filter(f => /seg\d+\.m4s$/.test(f)).sort((a,b) => Number(a.match(/seg(\d+)/)[1]) - Number(b.match(/seg(\d+)/)[1]));
  const audioFile = path.join(OUT, 'audio-complete.mp4');
  await fs.writeFile(audioFile, Buffer.concat([await fs.readFile(path.join(audioDir, 'init-audio.mp4')), ...await Promise.all(audioSegments.map(f => fs.readFile(f)))]));
  const packets = JSON.parse(command('ffprobe', ['-v', 'error', '-show_packets', '-show_entries', 'packet=pts,duration', '-of', 'json', audioFile])).packets;
  const audioGaps = packets.slice(1).filter((p,i) => Number(p.pts) !== Number(packets[i].pts) + Number(packets[i].duration)).length;
  command('ffmpeg', ['-v', 'error', '-xerror', '-i', audioFile, '-f', 'null', '-']);
  await fs.rm(path.join(OUT, 'decode.mp4'));
  const report = { aligned, rows, audio: { packets: packets.length, timestampGaps: audioGaps, capture: 'encoded fixture only; browser/device output not measured' } };
  await fs.writeFile(path.join(OUT, 'media-verification.json'), JSON.stringify(report, null, 2));
  console.log(`Independent segment decode: ${rows.length}; rational-grid alignment=${aligned}; encoded audio gaps=${audioGaps}`);
  if (!aligned || audioGaps) throw new Error('fixture alignment failed');
}
async function verifyJoins() {
  const dir = path.join(OUT, 'joins'); await fs.mkdir(dir, { recursive: true });
  const verification = JSON.parse(await fs.readFile(path.join(OUT, 'media-verification.json'), 'utf8'));
  const rows = verification.rows.filter(r => r.rung === '720').sort((a,b) => a.segment-b.segment);
  const lines = ['#EXTM3U', '#EXT-X-VERSION:7', '#EXT-X-TARGETDURATION:3', '#EXT-X-MEDIA-SEQUENCE:0', '#EXT-X-PLAYLIST-TYPE:VOD', '#EXT-X-INDEPENDENT-SEGMENTS'];
  const elementary = [];
  for (const row of rows) {
    const rung = row.segment % 2 ? '1080' : '720';
    lines.push(`#EXT-X-MAP:URI="../media/${rung}/init-${rung}.mp4"`, `#EXTINF:${(row.count*1001/24000).toFixed(6)},`, `../media/${rung}/seg${row.segment}.m4s`);
    const piece = path.join(dir, 'piece.mp4'), annex = path.join(dir, 'piece.h264');
    await fs.writeFile(piece, Buffer.concat([await fs.readFile(path.join(MEDIA,rung,`init-${rung}.mp4`)),await fs.readFile(path.join(MEDIA,rung,`seg${row.segment}.m4s`))]));
    command('ffmpeg',['-v','error','-y','-i',piece,'-map','0:v','-c:v','copy','-bsf:v','h264_mp4toannexb','-f','h264',annex]);
    elementary.push(await fs.readFile(annex));
  }
  lines.push('#EXT-X-ENDLIST');
  const playlist=path.join(dir,'alternating.m3u8'); await fs.writeFile(playlist,lines.join('\n')+'\n');
  const fmp4 = spawnSync('ffmpeg',['-v','error','-xerror','-allowed_extensions','ALL','-i',playlist,'-vf','scale=1280:720','-fps_mode','passthrough','-f','null','-'],{encoding:'utf8',maxBuffer:1024*1024});
  const joined=path.join(dir,'alternating.h264'); await fs.writeFile(joined,Buffer.concat(elementary));
  const frameFile=path.join(dir,'annexb.framemd5');
  command('ffmpeg',['-v','error','-xerror','-y','-f','h264','-i',joined,'-vf','scale=1280:720','-fps_mode','passthrough','-f','framemd5',frameFile]);
  const frameCount=(await fs.readFile(frameFile,'utf8')).split('\n').filter(line=>line && !line.startsWith('#')).length;
  const expectedFrames=rows.reduce((n,row)=>n+row.count,0);
  const decoded=JSON.parse(command('ffprobe',['-v','error','-f','h264','-i',joined,'-show_frames','-select_streams','v','-show_entries','frame=width,height,key_frame','-of','json'])).frames;
  const expectedHeights=rows.flatMap(row=>Array(row.count).fill(row.segment%2?1080:720));
  const shapesMatch=decoded.length===expectedHeights.length && decoded.every((frame,i)=>frame.height===expectedHeights[i]);
  const resolutionTransitions=decoded.slice(1).filter((frame,i)=>frame.height!==decoded[i].height).length;
  const report={ fixture:JSON.parse(await fs.readFile(path.join(OUT,'fixture.json'),'utf8')), ffmpeg:command('ffmpeg',['-version']).split('\n')[0], boundaries:rows.length-1, expectedFrames,
    fmp4MapChange:{status:fmp4.status===0?'decoded':'failed',exitCode:fmp4.status,diagnostic:(fmp4.stderr||'').split('\n').slice(0,12),meaning:'FFmpeg HLS demuxer/map-change experiment only; not a browser incompatibility verdict'},
    annexB:{status:frameCount===expectedFrames?'decoded':'frame-count-mismatch',frames:frameCount,shapesMatch,resolutionTransitions,meaning:'24 alternating independently initialized segments decode through one elementary H.264 pipeline with SPS/PPS. Elementary extraction does not preserve MP4 sample timestamps.'},
    limitations:['browser/display/audio capture not measured','burned frame identities not recognized','fMP4 map-change result must be separated from Annex-B decoder reconfiguration','not production-media acceptance']};
  await fs.writeFile(path.join(OUT,'join-verification.json'),JSON.stringify(report,null,2));
  for(const name of ['piece.mp4','piece.h264']) await fs.rm(path.join(dir,name),{force:true});
  console.log(JSON.stringify({boundaries:report.boundaries,expectedFrames,fmp4MapChange:report.fmp4MapChange.status,annexB:report.annexB.status,decodedFrames:frameCount,shapesMatch,resolutionTransitions}));
}
async function server() {
  const requests = [];
  const srv = http.createServer(async (req, res) => {
    const pathname = new URL(req.url, 'http://localhost').pathname;
    const map = { '/': 'tests/playback/continuous-quality/probe.html', '/probe.js': 'tests/playback/continuous-quality/probe.js', '/hls.min.js': 'crates/plurxd/src/web/hls.min.js' };
    const file = map[pathname] ? path.join(ROOT, map[pathname]) : pathname.startsWith('/media/') ? path.resolve(MEDIA, '.' + pathname.slice(6)) : null;
    if (!file || (!map[pathname] && !file.startsWith(MEDIA + path.sep))) { res.writeHead(404); res.end(); return; }
    try {
      const bytes = await fs.readFile(file);
      requests.push({ path: pathname, wall: Date.now(), bytes: bytes.length });
      const mime = file.endsWith('.m3u8') ? 'application/vnd.apple.mpegurl' : file.endsWith('.js') ? 'text/javascript' : file.endsWith('.html') ? 'text/html' : 'video/mp4';
      res.writeHead(200, { 'content-type': mime, 'cache-control': 'no-store', 'access-control-allow-origin': '*' }); res.end(bytes);
    } catch { res.writeHead(404); res.end(); }
  });
  await new Promise(r => srv.listen(0, '127.0.0.1', r));
  return { srv, requests, url: `http://127.0.0.1:${srv.address().port}/` };
}
async function waitFor(fn, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { const result = await fn(); if (result) return result; await sleep(100); }
  throw new Error('probe timed out');
}
async function chrome(url) {
  const executable = process.env.CQ_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
  const child = spawn(executable, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${path.join(OUT, 'chrome-profile')}`, '--autoplay-policy=no-user-gesture-required', '--no-first-run', '--no-default-browser-check', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
  let stderr = '', failed; child.stderr.on('data', b => stderr += b); child.on('error', e => failed = e);
  try {
    const endpoint = await waitFor(() => { if (failed) throw failed; return stderr.match(/DevTools listening on (ws:\/\/\S+)/)?.[1]; });
    const base = `http://${new URL(endpoint).host}`;
    const targets = await (await fetch(base + '/json/list')).json();
    const ws = new WebSocket(targets.find(t => t.type === 'page').webSocketDebuggerUrl);
    await new Promise((r,j) => { ws.addEventListener('open',r,{once:true}); ws.addEventListener('error',j,{once:true}); });
    let next = 0; const pending = new Map();
    ws.addEventListener('message', e => { const m = JSON.parse(e.data); const p = pending.get(m.id); if (p) { pending.delete(m.id); m.error ? p.reject(new Error(m.error.message)) : p.resolve(m.result); } });
    const send = (method, params = {}) => new Promise((resolve,reject) => { const id = ++next; const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 15000); pending.set(id,{resolve:r=>{clearTimeout(timer);resolve(r);},reject:e=>{clearTimeout(timer);reject(e);}}); ws.send(JSON.stringify({id,method,params})); });
    const evaluate = async expression => { const r = await send('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true}); if(r.exceptionDetails) throw new Error(r.exceptionDetails.text); return r.result.value; };
    await send('Page.navigate',{url}); await waitFor(() => evaluate('typeof window.start === "function"'));
    return { evaluate, screenshot: async () => Buffer.from((await send('Page.captureScreenshot')).data,'base64'), close: () => { ws.close(); child.kill(); } };
  } catch (e) { child.kill(); throw e; }
}
async function safari(url) {
  const temp = http.createServer(); await new Promise(r=>temp.listen(0,'127.0.0.1',r)); const port=temp.address().port; await new Promise(r=>temp.close(r));
  const child = spawn('/usr/bin/safaridriver',['--port',String(port)],{stdio:['ignore','ignore','ignore']});
  let id;
  const wd = async (method, route, body) => { const response = await fetch(`http://127.0.0.1:${port}${route}`,{method,headers:{'content-type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(15000)}).catch(e=>{throw new Error(`Safari ${method} ${route}: ${e.message}`);}); const data=await response.json(); if(!response.ok || data.value?.error) throw new Error(data.value?.message || `Safari HTTP ${response.status}`); return data.value; };
  try {
    await waitFor(async()=>{try{await wd('GET','/status');return true;}catch{return false;}});
    const session=await wd('POST','/session',{capabilities:{alwaysMatch:{browserName:'safari'}}}); id=session.sessionId;
    await wd('POST',`/session/${id}/url`,{url});
    const evaluate = expression=>wd('POST',`/session/${id}/execute/sync`,{script:`return (${expression});`,args:[]});
    const element=await wd('POST',`/session/${id}/element`,{using:'css selector',value:'button'});
    await wd('POST',`/session/${id}/element/${element['element-6066-11e4-a52e-4f735466cecf']}/click`,{});
    return { evaluate, screenshot:async()=>Buffer.from(await wd('GET',`/session/${id}/screenshot`),'base64'), close:async()=>{try{await wd('DELETE',`/session/${id}`);}finally{child.kill();}} };
  } catch(e) { if(id) await wd('DELETE',`/session/${id}`).catch(()=>{}); child.kill(); throw e; }
}
export function summarize(r) {
  const frames = r.frames || [], intents = r.events.filter(e=>e.type==='intent');
  const requested = intents.find(e=>e.level===1);
  const target = frames.find(f=>f.height===1080 && requested && f.wall>=requested.wall);
  const gaps = frames.slice(1).map((f,i)=>f.wall-frames[i].wall);
  const stableGaps = frames.slice(1).flatMap((f,i) => frames[i].pts > 1 ? [f.wall - frames[i].wall] : []);
  const switchGaps = target ? frames.slice(1).flatMap((f,i) => Math.abs(f.pts-target.pts) < 1 ? [f.wall - frames[i].wall] : []) : [];
  const sorted = [...stableGaps].sort((a,b)=>a-b);
  return { scenario:r.scenario, native:r.native, identity:r.identity, firstTarget:target||null, request:requested||null,
    targetDelayMs: target && requested ? target.wall-requested.wall : null,
    maximumCallbackGapMs:gaps.length?Math.max(...gaps):null,
    stableCallbackGapP95Ms: sorted.length ? sorted[Math.floor((sorted.length-1)*0.95)] : null,
    switchWindowMaximumCallbackGapMs: switchGaps.length ? Math.max(...switchGaps) : null,
    targetAppended:r.committedTarget||null,
    appendFrontierRespected: r.committedTarget && requested ? r.committedTarget.start + 0.002 >= requested.frontier : null,
    movingTargetFrames: frames.filter(f=>f.height===1080).length,
    errors:r.events.filter(e=>e.type==='hls-error'||e.type==='start-error'||e.type==='error'),
    capture:r.limits, nativeExactControl:r.nativeExactControl||null,
    verdict:'investigative; no production or device acceptance' };
}
async function run(browser, cases) {
  const s=await server();
  try {
    for(const scenario of cases) {
      let driver; const begin=s.requests.length;
      try {
        driver=await (browser==='safari'?safari:chrome)(s.url+`?case=${scenario}${browser==='safari'?'&native=1':''}`);
        if(browser!=='safari') await driver.evaluate('window.start().then(()=>true)');
        await waitFor(async()=> (await driver.evaluate('video.currentTime')) > (scenario==='long-buffer'?38:18),55000);
        const r=await driver.evaluate('window.snapshot()'); r.requests=s.requests.slice(begin);
        const name=`${browser}-${scenario}`; r.sourceTree = command('git',['-C',ROOT,'rev-parse','HEAD']).trim(); r.fixture = JSON.parse(await fs.readFile(path.join(OUT,'fixture.json'),'utf8')); r.probeSha256 = createHash('sha256').update(await fs.readFile(path.join(ROOT,'tests/playback/continuous-quality/probe.js'))).digest('hex');
        await fs.writeFile(path.join(OUT,`${name}.json`),JSON.stringify(r,null,2));
        await fs.writeFile(path.join(OUT,`${name}.png`),await driver.screenshot());
        const summary=summarize(r); await fs.writeFile(path.join(OUT,`${name}-summary.json`),JSON.stringify(summary,null,2)); console.log(JSON.stringify(summary));
      } catch(e) {
        await fs.writeFile(path.join(OUT,`${browser}-${scenario}-blocked.json`),JSON.stringify({browser,scenario,error:e.message,acceptance:'not measured'},null,2)); throw e;
      } finally { if(driver) await driver.close(); }
    }
  } finally { await new Promise(r=>s.srv.close(r)); }
}
async function main() {
  await fs.mkdir(OUT,{recursive:true}); const [cmd='help',browser='chrome',...cases]=process.argv.slice(2);
  if(cmd==='fixture') await fixture();
  else if(cmd==='verify-media') await verifyMedia();
  else if(cmd==='verify-joins') await verifyJoins();
  else if(cmd==='run') await run(browser,cases.length?cases:['baseline','switch','cancel-before-append','cancel-after-append','denied','long-buffer']);
  else if(cmd==='serve') { const s=await server(); console.log(s.url); }
  else console.log('node scripts/continuous-quality-lab.mjs fixture | verify-media | verify-joins | serve | run chrome|safari [case ...]');
}
if(process.argv[1] && path.resolve(process.argv[1])===fileURLToPath(import.meta.url)) main().catch(e=>{console.error(e.message);process.exitCode=1;});
