#!/usr/bin/env python3
"""Native reach experiment over real copyseg output, not daemon qualification.

The media exporter runs the actual Rust segmenter under explicit lab limits.
This server models a bounded 48s publication allowance; it cannot prove actor,
scratch admission, source identity, or production throughput. No credentials.
"""
import argparse
import json
import re
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--media', type=Path, required=True)
parser.add_argument('--receipt', type=Path, required=True)
parser.add_argument('--port', type=int, default=8767)
args = parser.parse_args()
variants = {}
for name in ('baseline', 'short'):
    text = (args.media / name / 'index.m3u8').read_text()
    target = int(re.search(r'#EXT-X-TARGETDURATION:(\d+)', text)[1])
    rows = re.findall(r'#EXTINF:([\d.]+),\n([^\n]+)', text)
    ends, edge = [], 0.0
    for duration, uri in rows:
        duration = float(duration)
        if not 0 < duration <= target or not re.fullmatch(r'seg\d+\.m4s', uri):
            raise ValueError('invalid segment contract')
        edge += duration
        ends.append((edge, duration, uri))
    variants[name] = {'target': target, 'segments': ends, 'duration': edge}
runs = {}
lock = threading.Lock()

PAGE = r'''<!doctype html><meta charset="utf-8"><title>Native rolling seek comparison</title>
<style>body{background:#111b28;color:#eef;font:16px system-ui;max-width:1100px;margin:30px auto}button{padding:12px;margin:5px}video{width:480px}pre{white-space:pre-wrap}</style>
<h1>Native rolling seek comparison</h1>
<p>Actual GOP-aware copy output. Baseline target 16 s / actual 6 s; lab short target 3 s / actual 2 s.
Both retain a 48 s allowance. Generated AVC/AAC; modeled pacing and publication. This is not daemon or incident-source qualification.</p>
<button id="baseline">Start baseline</button><button id="short">Start short-GOP candidate</button>
<button id="sweep">20 local reach attempts</button><button id="chain">Five spaced +30 presses</button>
<button id="pause">Pause</button><button id="resume">Resume</button><button id="double">2×</button>
<video id="v" muted controls playsinline></video><pre id="result">Ready</pre>
<script>
const v=document.querySelector('#v'), result=document.querySelector('#result');
let run=null, count=0, pending=null, frameCount=0, frameId=null, samples=[];
const ranges=r=>Array.from({length:r.length},(_,i)=>[r.start(i),r.end(i)]);
function snapshot(){return {at_ms:performance.now(),position_s:v.currentTime,rate:v.playbackRate,paused:v.paused,seeking:v.seeking,buffered:ranges(v.buffered),seekable:ranges(v.seekable),ready_state:v.readyState,frames:frameCount};}
function report(event,details={},own=run){if(!own||own!==run)return;const row={event,...snapshot(),...details};
 samples.push(row);result.textContent=JSON.stringify(samples.slice(-5),null,2);
 navigator.sendBeacon('/event',JSON.stringify({run,event:row}));}
async function paired(event,own=run){if(!own||own!==run)return;const m=await(await fetch('/state/'+own)).json();if(own!==run)return;report(event,{served:m},own);}
async function start(name){
 if(pending){report('cancelled',{target_s:pending.target,reason:'attachment-change'});pending.resolve();pending=null;}
 if(frameId!==null){v.cancelVideoFrameCallback(frameId);frameId=null;}
 const own=name+'-'+(++count);run=own;samples=[];
 await fetch('/begin',{method:'POST',body:JSON.stringify({run:own,variant:name,user_agent:navigator.userAgent})});
 if(own!==run)return;
 v.pause();v.removeAttribute('src');v.load();v.playbackRate=1;v.src='/media/'+own+'/index.m3u8';
 armFrame(own);report('input-start',{variant:name},own);await v.play();if(own!==run)return;await paired('attachment',own);}
function seek(delta,own=run){if(!own||own!==run)return Promise.resolve();if(pending){report('cancelled',{target_s:pending.target});pending.resolve();pending=null;}
 const before=snapshot(),target=Math.max(0,v.currentTime+delta),reachable=before.seekable.some(r=>target>=r[0]&&target<=r[1]);
 report('committed-input',{delta,target_s:target,reachable,before});
 if(!reachable){report('outside-reach',{delta,target_s:target});return Promise.resolve();}
 return new Promise(resolve=>{const at=performance.now(),floor=frameCount;pending={target,at,floor,resolve,own,frame:null};
 v.currentTime=target;setTimeout(()=>{if(pending&&pending.at===at&&pending.own===own){report('target-timeout',{target_s:target});pending=null;resolve();}},3000);});}
function settleFrame(){if(!pending||pending.own!==run||!pending.frame||v.seeking)return;
 const p=pending;pending=null;report('target-frame',{target_s:p.target,media_time_s:p.frame.mediaTime,
 click_latency_ms:p.frame.at_ms-p.at,settlement_latency_ms:performance.now()-p.at,
 landing_error_s:p.frame.mediaTime-p.target},p.own);p.resolve();}
function armFrame(own){frameId=v.requestVideoFrameCallback((_,meta)=>{if(own!==run)return;frameId=null;frameCount++;
 if(pending&&pending.own===own&&frameCount>pending.floor&&Math.abs(meta.mediaTime-pending.target)<1){
 pending.frame={mediaTime:meta.mediaTime,at_ms:performance.now()};settleFrame();}
 armFrame(own);});}
v.addEventListener('playing',()=>report('playing'));v.addEventListener('waiting',()=>report('waiting'));
v.addEventListener('error',()=>report('error',{code:v.error?.code}));
v.addEventListener('seeked',()=>{report('seeked');settleFrame();});setInterval(()=>paired('paired-sample').catch(()=>{}),500);
document.querySelector('#baseline').onclick=()=>start('baseline');document.querySelector('#short').onclick=()=>start('short');
document.querySelector('#sweep').onclick=async()=>{const own=run;for(let i=0;i<20&&own===run;i++){await paired('pre-input',own);await seek([10,30,-10,-30][i%4],own);await new Promise(r=>setTimeout(r,1000));}report('sweep-complete',{},own);};
document.querySelector('#chain').onclick=async()=>{const own=run;for(let i=0;i<5&&own===run;i++){await paired('pre-chain-input',own);await seek(30,own);await new Promise(r=>setTimeout(r,5000));}report('chain-complete',{},own);};
document.querySelector('#pause').onclick=()=>{v.pause();report('pause');};document.querySelector('#resume').onclick=()=>{v.play();report('resume');};
document.querySelector('#double').onclick=()=>{v.playbackRate=2;report('rate-change');};
</script>'''


def snapshot(run):
    now = time.monotonic()
    v = variants[run['variant']]
    elapsed = now - run['began']
    # Deliberate lab source model: burst at 8x through 48s, then 2x.
    produced = min(v['duration'], min(48, elapsed * 8) + max(0, elapsed - 6) * 2)
    allowance = run['position'] + 48 * max(1, run['rate']) + v['target']
    eligible = [row for row in v['segments'] if row[0] <= min(produced, allowance)]
    if not run['published']:
        eligible = eligible if eligible and eligible[-1][0] >= 48 else []
    elif now - run['publish_at'] < v['target']:
        return run['published']
    if eligible and len(eligible) > len(run['published']):
        run['published'] = eligible
        run['publish_at'] = now
        run['revision'] += 1
    return run['published']


def state(run, rows=None):
    if rows is None:
        rows = snapshot(run)
    return {'revision': run['revision'], 'served_edge_s': rows[-1][0] if rows else 0,
            'target_s': variants[run['variant']]['target'], 'actual_durations_s': [r[1] for r in rows],
            'sample_at_s': round(time.monotonic()-run['began'], 3)}


def save():
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    data = {'schema_version': 1, 'qualification': 'generated copyseg media; modeled publication, not daemon',
            'variants': {name: {'target_s': v['target'], 'duration_s': v['duration'],
                                'durations_s': [r[1] for r in v['segments']]} for name, v in variants.items()},
            'runs': {key: {k: val for k, val in run.items() if k not in ('began', 'publish_at', 'published')}
                     for key, run in runs.items()}}
    temporary = args.receipt.with_suffix('.tmp')
    temporary.write_text(json.dumps(data, indent=2)+'\n')
    temporary.replace(args.receipt)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, body, mime='application/json', status=200):
        if isinstance(body, str):
            body = body.encode()
        self.send_response(status)
        self.send_header('Content-Type', mime)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_POST(self):
        data = json.loads(self.rfile.read(min(65536, int(self.headers.get('Content-Length', 0)))))
        with lock:
            if self.path == '/begin':
                if data['variant'] not in variants:
                    return self.send('{}', status=400)
                runs[data['run']] = {'variant': data['variant'], 'user_agent': data['user_agent'],
                                    'began': time.monotonic(), 'publish_at': 0, 'published': [],
                                    'revision': 0, 'position': 0, 'rate': 1, 'events': [], 'requests': []}
            elif self.path == '/event' and data['run'] in runs:
                run = runs[data['run']]
                event = data['event']
                run['position'] = max(0, float(event['position_s']))
                run['rate'] = min(4, max(.25, float(event['rate'])))
                run['events'].append(event)
                save()
        self.send('{}')

    def do_GET(self):
        path = urlsplit(self.path).path
        if path == '/':
            return self.send(PAGE, 'text/html')
        if path.startswith('/state/'):
            with lock:
                run = runs.get(path[7:])
                return self.send(json.dumps(state(run)) if run else '{}', status=200 if run else 404)
        parts = path.split('/')
        if len(parts) != 4 or parts[1] != 'media' or parts[2] not in runs:
            return self.send('{}', status=404)
        run_id, resource = parts[2:]
        run = runs[run_id]
        if resource == 'index.m3u8':
            deadline = time.monotonic()+12
            rows = []
            while time.monotonic() < deadline:
                with lock:
                    rows = snapshot(run)
                    transmitted = state(run, rows)
                if rows:
                    break
                time.sleep(.05)
            with lock:
                run['requests'].append({'resource': resource, **transmitted})
                save()
                text = '#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:'+str(variants[run['variant']]['target'])+'\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-START:TIME-OFFSET=0\n#EXT-X-MAP:URI="init.mp4"\n'
                text += ''.join(f'#EXTINF:{duration:.6f},\n{uri}\n' for _, duration, uri in rows)
            return self.send(text, 'application/vnd.apple.mpegurl', 200 if rows else 503)
        if resource != 'init.mp4' and not re.fullmatch(r'seg\d+\.m4s', resource):
            return self.send('{}', status=404)
        file = args.media / run['variant'] / resource
        return self.send(file.read_bytes(), 'video/mp4') if file.is_file() else self.send('{}', status=404)


print(f'Native reach fixture http://127.0.0.1:{args.port}', flush=True)
ThreadingHTTPServer(('127.0.0.1', args.port), Handler).serve_forever()
