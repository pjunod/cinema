#!/usr/bin/env python3
"""Finite physical-device probe. Pass an explicitly selected idle transport."""
import argparse, hashlib, json, pathlib, re, subprocess, time

p = argparse.ArgumentParser()
p.add_argument('--adb', required=True)
p.add_argument('--transport', required=True)
p.add_argument('--apk', required=True)
p.add_argument('--port', type=int, required=True)
p.add_argument('--case', required=True)
p.add_argument('--asset', required=True)
p.add_argument('--out', required=True)
p.add_argument('--static', action='store_true')
a = p.parse_args()
package = 'org.plurx.qualification.video.oct12'
base = [a.adb, '-t', a.transport]
receipt = {'schema': 1, 'case': a.case, 'apk_sha256': hashlib.sha256(pathlib.Path(a.apk).read_bytes()).hexdigest(), 'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'cleanup': {}}
installed = reversed_port = install_attempted = False

def call(*args, check=True):
    r = subprocess.run(base + list(args), capture_output=True, text=True, timeout=60 if args[0] == "install" else 20)
    if check and r.returncode:
        raise RuntimeError('adb command failed: ' + str(args[:3]) + ': ' + r.stderr[-500:])
    return r.stdout

def focus():
    return [x.strip() for x in call('shell', 'dumpsys', 'window').splitlines() if 'mCurrentFocus=' in x]

try:
    sessions = call('shell', 'dumpsys', 'media_session')
    states = re.findall(r'state=PlaybackState \{state=([^,]+)', sessions)
    receipt['preflight_media_states'] = states
    if any(x.startswith(('PLAYING', 'BUFFERING', 'CONNECTING', 'FAST_FORWARDING', 'REWINDING')) for x in states):
        raise RuntimeError('Device has active foreign media; no modification made')
    power = call('shell', 'dumpsys', 'power')
    if 'mWakefulness=Awake' not in power:
        raise RuntimeError('Device is not already awake; no modification made')
    policy = call('shell', 'dumpsys', 'window', 'policy')
    unlocked = ('showing=false' in policy or 'mIsShowing=false' in policy) and 'mInputRestricted=true' not in policy
    receipt['keyguard_not_showing'] = unlocked
    if not unlocked:
        raise RuntimeError('Device keyguard is not verifiably absent; no modification made')
    receipt['focus_before'] = focus()
    if call('shell', 'pm', 'path', package, check=False).strip():
        raise RuntimeError('Probe package already exists; ownership cannot be inferred')
    reverse = call('reverse', '--list')
    if f'tcp:{a.port}' in reverse:
        raise RuntimeError('Requested reverse port already has an owner')
    install_attempted = True
    call('install', '--no-streaming', a.apk); installed = True
    call('reverse', f'tcp:{a.port}', f'tcp:{a.port}'); reversed_port = True
    if 'No such file' not in call('shell', 'run-as', package, 'ls', 'files/qualification.json', check=False):
        # shell exit semantics vary; absence is proved separately below.
        absent = call('shell', 'run-as', package, 'sh', '-c', "'test ! -e files/qualification.json && echo ABSENT'", check=False)
        if 'ABSENT' not in absent:
            raise RuntimeError('Fresh receipt absence was not established')
    url = f'http://127.0.0.1:{a.port}/' + a.asset.lstrip('/')
    call('shell', 'am', 'start', '-W', '-n', package + '/org.plurx.qualification.video.ProbeActivity', '--es', 'url', url, '--es', 'case', a.case, '--ez', 'expect_motion', 'false' if a.static else 'true')
    deadline = time.monotonic() + 105
    while time.monotonic() < deadline:
        raw = call('exec-out', 'run-as', package, 'cat', 'files/qualification.json', check=False)
        try:
            value = json.loads(raw)
            if value.get('case') != a.case:
                raise RuntimeError('Mismatched receipt identity')
            receipt['probe'] = value
            break
        except json.JSONDecodeError:
            time.sleep(1)
    else:
        raise RuntimeError('105-second receipt deadline exceeded')
except Exception as error:
    receipt['launcher_error'] = str(error)
finally:
    if installed or install_attempted:
        call('shell', 'am', 'force-stop', package, check=False)
        receipt['cleanup']['uninstall_result'] = call('uninstall', package, check=False).strip()
        receipt['cleanup']['package_absent'] = not call('shell', 'pm', 'path', package, check=False).strip()
    if reversed_port:
        call('reverse', '--remove', f'tcp:{a.port}', check=False)
        receipt['cleanup']['owned_reverse_absent'] = f'tcp:{a.port}' not in call('reverse', '--list', check=False)
    receipt['focus_after'] = focus()
    receipt['finished_utc'] = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    pathlib.Path(a.out).write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps({'case': a.case, 'result': receipt.get('probe', {}).get('result'), 'launcher_error': receipt.get('launcher_error'), 'cleanup': receipt['cleanup']}))
raise SystemExit(0 if receipt.get('probe', {}).get('result') == 'pass' and not receipt.get('launcher_error') else 1)
