import fs from 'node:fs/promises';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';
import crypto from 'node:crypto';
import { pathToFileURL } from 'node:url';

const root = path.resolve('target/qualification');
const served = path.join(root, 'served');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

export function bounded(operation, milliseconds, label) {
  let timer;
  return Promise.race([
    Promise.resolve(operation),
    new Promise((_, reject) => { timer = setTimeout(() => reject(Error(`${label} deadline exceeded`)), milliseconds); }),
  ]).finally(() => clearTimeout(timer));
}

async function wait(fn, budget = 15000) {
  const end = Date.now() + budget;
  while (Date.now() < end) {
    const value = await bounded(Promise.resolve().then(fn), Math.max(1, end - Date.now()), 'wait');
    if (value) return value;
    await sleep(Math.min(50, Math.max(0, end - Date.now())));
  }
  throw Error('wait deadline exceeded');
}

export function pageHtml(startupMs = 15000) {
  return `<!doctype html><meta charset=utf-8><button onclick="start()">Start</button><video id=v muted playsinline width=640 height=360></video><script src=/hls.js></script><script>
const v=document.querySelector('video'),q=new URLSearchParams(location.search);let h,frames=[],events=[],stage='idle';
v.addEventListener('error',()=>events.push({event:'error',code:v.error?.code,message:v.error?.message}));
for(const e of ['waiting','stalled','seeking','seeked','playing'])v.addEventListener(e,()=>events.push({event:e,wall:performance.now(),time:v.currentTime,stage}));
const canvas=document.createElement('canvas');canvas.width=32;canvas.height=18;const ctx=canvas.getContext('2d',{willReadFrequently:true});
function frame(w,m){if(!v.seeking&&!v.paused){ctx.drawImage(v,0,0,32,18);const p=ctx.getImageData(0,0,32,18).data;let sum=0;for(let i=0;i<p.length;i++)sum=(sum*31+p[i])>>>0;frames.push({wall:w,time:m.mediaTime,presented:m.presentedFrames,stage,pixels:sum,width:v.videoWidth,height:v.videoHeight})}v.requestVideoFrameCallback(frame)}v.requestVideoFrameCallback(frame);
window.start=()=>new Promise((resolve,reject)=>{
 let settled=false;const cleanups=[];
 const finish=(error)=>{if(settled)return;settled=true;clearTimeout(timer);for(const cleanup of cleanups)cleanup();error?reject(error):resolve(true)};
 const timer=setTimeout(()=>finish(Error('media startup deadline exceeded')),${startupMs});
 const mediaError=()=>finish(Error(v.error?.message||'media startup error'));v.addEventListener('error',mediaError);cleanups.push(()=>v.removeEventListener('error',mediaError));
 (async()=>{stage='cold-mid';
  if(q.get('native')){
   await new Promise((r,j)=>{const ready=()=>r();const failed=()=>j(Error(v.error?.message||'native metadata error'));v.addEventListener('loadedmetadata',ready);v.addEventListener('error',failed);cleanups.push(()=>{v.removeEventListener('loadedmetadata',ready);v.removeEventListener('error',failed)});v.src='/'+q.get('asset')+'/index.m3u8'});v.currentTime=18;
  }else{
   h=new Hls({startPosition:18,maxBufferLength:6,maxMaxBufferLength:8,backBufferLength:4});
   await new Promise((r,j)=>{const ready=()=>r();h.on(Hls.Events.MANIFEST_PARSED,ready);cleanups.push(()=>h.off(Hls.Events.MANIFEST_PARSED,ready));h.on(Hls.Events.ERROR,(_,e)=>{if(e.fatal){events.push({event:'fatal',details:e.details});const error=Error('fatal manifest/media error: '+e.details);j(error);finish(error)}});h.attachMedia(v);h.loadSource('/'+q.get('asset')+'/index.m3u8')});
  }
  if(!settled){await v.play();finish()}
 })().catch(finish);
});
window.jump=t=>{stage=t===40?'forward':'backward';v.currentTime=t;return true};window.snapshot=()=>({frames,events,stage,time:v.currentTime,ready:v.readyState,error:v.error?.message||null,userAgent:navigator.userAgent});
</script>`;
}

export function cdpTransport(ws, requestMs = 15000) {
  let id = 0, failure = null;
  const pending = new Map();
  const fail = error => {
    failure = error;
    for (const request of pending.values()) { clearTimeout(request.timer); request.reject(error); }
    pending.clear();
  };
  ws.addEventListener('close', () => fail(Error('CDP socket closed')));
  ws.addEventListener('error', () => fail(Error('CDP socket error')));
  ws.addEventListener('message', event => {
    let message;
    try { message = JSON.parse(event.data); } catch { fail(Error('invalid CDP response')); return; }
    const request = pending.get(message.id);
    if (!request) return;
    clearTimeout(request.timer);
    pending.delete(message.id);
    if (message.error) request.reject(Error(`CDP command failed: ${JSON.stringify(message.error)}`));
    else request.resolve(message);
  });
  return {
    send(method, params) {
      if (failure) return Promise.reject(failure);
      return new Promise((resolve, reject) => {
        const ownId = ++id;
        const timer = setTimeout(() => {
          pending.delete(ownId);
          reject(Error(`CDP ${method} deadline exceeded`));
        }, requestMs);
        pending.set(ownId, { resolve, reject, timer });
        try { ws.send(JSON.stringify({ id: ownId, method, params })); }
        catch (error) { fail(error); }
      });
    },
    close() {
      fail(Error('CDP owner closed'));
      try { ws.close(); } catch { /* The owned child is reaped even if the socket never opened. */ }
    },
  };
}

function childLifetime(child) {
  const state = { closed: false, error: null };
  state.completion = new Promise(resolve => child.once('close', () => { state.closed = true; resolve(); }));
  child.on('error', error => { state.error = error; });
  return state;
}

async function reap(child, lifetime, terminateMs, killMs) {
  if (lifetime.closed) return;
  child.kill('SIGTERM');
  try { await bounded(lifetime.completion, terminateMs, 'Chrome terminate'); }
  catch {
    if (!lifetime.closed) child.kill('SIGKILL');
    await bounded(lifetime.completion, killMs, 'Chrome reap');
  }
}

export async function chrome(url, dependencies = {}) {
  const launch = dependencies.spawn || spawn;
  const request = dependencies.fetch || fetch;
  const Socket = dependencies.WebSocket || WebSocket;
  const remove = dependencies.remove || (profile => fs.rm(profile, { recursive: true, force: true }));
  const startupMs = dependencies.startupMs ?? 15000;
  const requestMs = dependencies.requestMs ?? 15000;
  const terminateMs = dependencies.terminateMs ?? 3000;
  const killMs = dependencies.killMs ?? 3000;
  const profile = path.join(root, 'chrome-owned-' + crypto.randomUUID());
  let child, lifetime, transport, ws, closePromise;
  const close = () => closePromise ||= (async () => {
    transport?.close();
    if (!transport && ws) { try { ws.close(); } catch { /* Reap remains authoritative. */ } }
    if (child) await reap(child, lifetime, terminateMs, killMs);
    // Never erase a profile while its owning process may still be alive.
    await remove(profile);
  })();
  try {
    child = launch('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
      '--headless=new', '--remote-debugging-port=0', '--user-data-dir=' + profile,
      '--autoplay-policy=no-user-gesture-required', '--no-first-run', '--no-default-browser-check', 'about:blank',
    ], { stdio: ['ignore', 'ignore', 'pipe'] });
    lifetime = childLifetime(child);
    let logs = '';
    child.stderr.on('data', chunk => { logs = (logs + chunk).slice(-65536); });
    const endpoint = await wait(() => {
      if (lifetime.error) throw lifetime.error;
      if (lifetime.closed) throw Error('Chrome exited before startup');
      return logs.match(/DevTools listening on (ws:\/\/\S+)/)?.[1];
    }, startupMs);
    const targets = await bounded((async () => {
      const response = await request('http://' + new URL(endpoint).host + '/json/list', { signal: AbortSignal.timeout(requestMs) });
      if (!response.ok) throw Error(`Chrome target discovery failed: ${response.status}`);
      return response.json();
    })(), requestMs, 'Chrome target discovery');
    const target = targets.find(value => value.type === 'page');
    if (!target?.webSocketDebuggerUrl) throw Error('Chrome page target missing');
    ws = new Socket(target.webSocketDebuggerUrl);
    transport = cdpTransport(ws, requestMs);
    await bounded(new Promise((resolve, reject) => {
      ws.addEventListener('open', resolve, { once: true });
      ws.addEventListener('close', () => reject(Error('CDP closed before open')), { once: true });
      ws.addEventListener('error', () => reject(Error('CDP error before open')), { once: true });
    }), startupMs, 'CDP open');
    const evaluate = async expression => {
      const message = await transport.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
      if (message.result.exceptionDetails) throw Error(JSON.stringify(message.result.exceptionDetails));
      return message.result.result.value;
    };
    await transport.send('Page.navigate', { url });
    await wait(() => evaluate('typeof start==="function"'), startupMs);
    return { evaluate, start: () => evaluate('start()'), close };
  } catch (error) {
    try { await close(); } catch (cleanupError) { throw new AggregateError([error, cleanupError], 'Chrome startup and cleanup failed'); }
    throw error;
  }
}

async function safari(url) {
  const socket = http.createServer();
  await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
  const port = socket.address().port;
  await new Promise(resolve => socket.close(resolve));
  const child = spawn('/usr/bin/safaridriver', ['--port', String(port)], { stdio: 'ignore' });
  const lifetime = childLifetime(child);
  let id;
  const req = async (method, route, body) => {
    const response = await fetch('http://127.0.0.1:' + port + route, { method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(15000) });
    const value = (await response.json()).value;
    if (!response.ok || value?.error) throw Error(value?.message || response.status);
    return value;
  };
  const close = async () => {
    try { if (id) await req('DELETE', '/session/' + id); } finally { await reap(child, lifetime, 3000, 3000); }
  };
  try {
    await wait(async () => { if (lifetime.error) throw lifetime.error; try { return await req('GET', '/status'); } catch { return false; } });
    id = (await req('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'safari' } } })).sessionId;
    await req('POST', '/session/' + id + '/url', { url });
    const evaluate = expression => req('POST', '/session/' + id + '/execute/sync', { script: 'return (' + expression + ');', args: [] });
    return { evaluate, start: async () => {
      const element = await req('POST', '/session/' + id + '/element', { using: 'css selector', value: 'button' });
      return req('POST', '/session/' + id + '/element/' + element['element-6066-11e4-a52e-4f735466cecf'] + '/click', {});
    }, close };
  } catch (error) { await close(); throw error; }
}

async function run(browser = process.argv[2] || 'chrome') {
  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://localhost');
      if (url.pathname === '/') { res.setHeader('content-type', 'text/html'); res.end(pageHtml()); return; }
      const file = url.pathname === '/hls.js' ? path.resolve('crates/plurxd/src/web/hls.min.js') : path.resolve(served, '.' + url.pathname);
      if (url.pathname !== '/hls.js' && !file.startsWith(served + '/')) throw Error('path');
      const bytes = await fs.readFile(file);
      res.setHeader('content-type', file.endsWith('js') ? 'text/javascript' : file.endsWith('m3u8') ? 'application/vnd.apple.mpegurl' : 'video/mp4');
      res.setHeader('cache-control', 'no-store'); res.end(bytes);
    } catch { res.writeHead(404); res.end(); }
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const base = 'http://127.0.0.1:' + server.address().port;
  const rows = [];
  try {
    for (const asset of ['animation-bf0', 'animation-bf2', 'live-action-bf0', 'live-action-bf2']) {
      let driver;
      const row = { asset, browser };
      try {
        driver = await (browser === 'safari' ? safari : chrome)(base + '/?asset=' + asset + (browser === 'safari' ? '&native=1' : ''));
        await bounded(driver.start(), 20000, 'browser media startup');
        for (const target of [18, 40, 8]) {
          if (target !== 18) await driver.evaluate('jump(' + target + ')');
          await wait(async () => { const value = await driver.evaluate('snapshot()'); if (value.error) throw Error(value.error); return value.time > target + 2 && value.time < target + 8; }, 20000);
        }
        row.capture = await driver.evaluate('snapshot()');
        row.stages = ['cold-mid', 'forward', 'backward'].map(stage => {
          const frames = row.capture.frames.filter(value => value.stage === stage);
          return { stage, frames: frames.length, first: frames[0]?.time, last: frames.at(-1)?.time, backwardSteps: frames.slice(1).filter((value, index) => value.time < frames[index].time).length, distinctPixelHashes: new Set(frames.map(value => value.pixels)).size };
        });
        row.status = row.stages.every(stage => stage.frames >= 20 && stage.backwardSteps === 0 && stage.distinctPixelHashes >= 10) && !row.capture.events.some(event => ['error', 'fatal'].includes(event.event)) ? 'passed' : 'failed';
      } catch (error) { row.status = 'failed'; row.error = error.message; }
      finally { if (driver) await driver.close(); }
      rows.push(row);
      await fs.writeFile(path.join(root, browser + '-capture.json'), JSON.stringify(rows, null, 2));
      console.log(asset, row.status, row.error || JSON.stringify(row.stages));
    }
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  await run();
}
