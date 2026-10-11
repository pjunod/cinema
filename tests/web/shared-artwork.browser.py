#!/usr/bin/env python3
"""Actual headless Chromium PNG decode/canvas retirement; no physical GPU claim."""
import binascii
import html
import http.server
import json
import pathlib
import os
import signal
import time
import re
import shutil
import struct
import subprocess
import tempfile
import threading
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
CHROME = pathlib.Path('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome')
REFERENCE = dict(import_id='11111111-1111-4111-8111-111111111111', server_id='22222222-2222-4222-8222-222222222222', catalogue_epoch='33333333-3333-4333-8333-333333333333', library_id='9007199254740993', item_id='9223372036854775807')
ART = '/api/v1/shared/imports/' + REFERENCE['import_id'] + '/art/' + 'A' * 272
DETAIL = '/api/v1/shared/imports/' + REFERENCE['import_id'] + '/items/' + REFERENCE['item_id']

def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', binascii.crc32(kind + data) & 0xffffffff)

PNG = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 600, 400, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress((b'\0' + b'\xff\0\0\xff' * 600) * 400)) + chunk(b'IEND', b'')
ITEM = dict(source='shared', reference=REFERENCE, title='Actual PNG', kind='movie', art=[dict(kind='poster', variant='w300', url=ART)], poster_url=ART)
requests = []
PAGE = '''<!doctype html><body><main id="shared-catalogue"></main><pre id="result">pending</pre>
<script>
let AUTH_GENERATION=1,PAGE_RENDER_GENERATION=1,TOKEN='fixture-bearer',API='/api/v1';location.hash='#/shared';
const PLAYBACK_FILE_UUID=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
function esc(v){return String(v).replaceAll('&','&amp;').replaceAll('"','&quot;').replaceAll('<','&lt;');}function fmtDur(v){return String(v);}
async function api(path,options){return fetch(API+path,{...options,headers:{authorization:'Bearer '+TOKEN},redirect:'error'});}
function clearLocalSession(g){if(g===AUTH_GENERATION){sharedArtworkRetire();TOKEN=null;AUTH_GENERATION++;}}
</script><script src="/pages/shared-artwork.js"></script><script src="/pages/shared-libraries.js"></script><script>
(async()=>{try{
const capture={generation:1,auth:1,token:TOKEN,origin:API,route:location.hash};
const data=await SHARED_ARTWORK.metadata(DETAIL,capture),root=document.getElementById('shared-catalogue');
root.innerHTML=SHARED_ARTWORK.markup(data.item,data.item.reference,capture);SHARED_ARTWORK.hydrate(root);
const canvas=root.querySelector('canvas');for(let n=0;n<1000&&!canvas.dataset.sharedArtReady;n++)await new Promise(r=>setTimeout(r,25));
if(canvas.dataset.sharedArtReady!=='true'||canvas.width!==300||canvas.height!==200)throw Error('actual image decode did not publish '+JSON.stringify({width:canvas.width,height:canvas.height,dataset:canvas.dataset,snapshot:SHARED_ARTWORK.snapshot()}));
const pixel=[...canvas.getContext('2d').getImageData(0,0,1,1).data];if(pixel.join(',')!=='255,0,0,255')throw Error('actual decoded pixel differs');
const before=SHARED_ARTWORK.snapshot();SHARED_ARTWORK.retire();const after=SHARED_ARTWORK.snapshot();
if(canvas.width!==0||canvas.height!==0||after.pixels.bytes!==0||after.compressed.bytes!==0)throw Error('actual canvas not retired');
document.getElementById('result').textContent=JSON.stringify({pass:true,dimensions:[300,200],pixel,before,after});
}catch(error){document.getElementById('result').textContent=JSON.stringify({pass:false,error:String(error)});}})();
</script>'''.replace('SHARED_ARTWORK.metadata(DETAIL,', 'SHARED_ARTWORK.metadata(' + json.dumps(DETAIL.removeprefix('/api/v1')) + ',')

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if self.path in (DETAIL, ART):
            requests.append(dict(path=self.path, authorization=self.headers.get('Authorization')))
            if self.headers.get('Authorization') != 'Bearer fixture-bearer':
                self.send_error(401)
                return
        if self.path == '/':
            body, mime = PAGE.encode(), 'text/html'
        elif self.path.startswith('/pages/') and self.path in ('/pages/shared-artwork.js', '/pages/shared-libraries.js'):
            body, mime = (ROOT / 'crates/plurxd/src/web' / self.path[1:]).read_bytes(), 'text/javascript'
        elif self.path == DETAIL:
            body, mime = json.dumps(dict(item=ITEM, files=[], delivery_status='unavailable')).encode(), 'application/json'
        elif self.path == ART:
            body, mime = PNG, 'image/png'
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', mime)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

if not CHROME.exists():
    candidate = shutil.which('chromium') or shutil.which('chromium-browser') or shutil.which('google-chrome')
    if not candidate:
        raise SystemExit('Actual Chromium unavailable; browser decode not qualified')
    CHROME = pathlib.Path(candidate)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix='sharing-art-browser-') as profile:
        with tempfile.TemporaryFile(mode='w+t') as output, tempfile.TemporaryFile(mode='w+t') as errors:
            process = subprocess.Popen([str(CHROME), '--headless=new', '--disable-gpu', '--no-sandbox', '--disable-background-networking', '--disable-default-apps', '--no-first-run', '--disable-sync', '--disable-extensions', '--metrics-recording-only', '--user-data-dir=' + profile, '--dump-dom', '--virtual-time-budget=30000', 'http://127.0.0.1:' + str(server.server_port) + '/'], stdout=output, stderr=errors, start_new_session=True)
            try:
                match = None
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    output.seek(0)
                    match = re.search(r'<pre id="result">(.*?)</pre>', output.read(), re.S)
                    if match and match.group(1) != 'pending':
                        break
                    time.sleep(0.1)
                if not match or match.group(1) == 'pending':
                    errors.seek(0)
                    raise SystemExit('Browser did not finish actual decode: ' + errors.read()[-1000:])
            finally:
                # Own isolated process group only. Descendant updater pipes can
                # outlive --dump-dom; no user browser/profile is touched.
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
        receipt = json.loads(html.unescape(match.group(1)))
        if not receipt.get('pass'):
            raise SystemExit(json.dumps(receipt))
        if requests != [dict(path=DETAIL, authorization='Bearer fixture-bearer'), dict(path=ART, authorization='Bearer fixture-bearer')]:
            raise SystemExit('Unexpected browser requests: ' + repr(requests))
        receipt['browser'] = subprocess.check_output([str(CHROME), '--version'], text=True, timeout=10).strip()
        receipt['scope'] = 'Actual headless software PNG/ImageBitmap/Canvas decode and retirement; physical GPU/storage unqualified'
        print(json.dumps(receipt, sort_keys=True))
finally:
    server.shutdown()
    server.server_close()
