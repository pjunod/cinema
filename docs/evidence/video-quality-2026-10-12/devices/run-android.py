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

def package_path():
    r = subprocess.run(base + ['shell', 'pm', 'path', package], capture_output=True, text=True, timeout=20)
    # Android returns1 for a genuinely absent package. A transport error is
    # not absence, even though it also produces no stdout.
    if r.returncode not in (0, 1) or r.stderr.strip():
        raise RuntimeError('Could not inspect owned package: ' + r.stderr[-500:])
    return r.stdout.strip()

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
    if package_path():
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
    def cleanup(name, operation):
        try:
            receipt['cleanup'][name] = operation()
        except Exception as error:
            receipt['cleanup'][name + '_error'] = str(error)
    if installed or install_attempted:
        cleanup('force_stop', lambda: call('shell', 'am', 'force-stop', package, check=False).strip())
        cleanup('uninstall_result', lambda: call('uninstall', package, check=False).strip())
        cleanup('package_absent', lambda: not package_path())
    if reversed_port:
        cleanup('remove_reverse', lambda: call('reverse', '--remove', f'tcp:{a.port}', check=False).strip())
        cleanup('owned_reverse_absent', lambda: f'tcp:{a.port}' not in call('reverse', '--list'))
    try:
        receipt['focus_after'] = focus()
    except Exception as error:
        receipt['focus_after_error'] = str(error)
    receipt['finished_utc'] = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())
    pathlib.Path(a.out).write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps({'case': a.case, 'result': receipt.get('probe', {}).get('result'), 'launcher_error': receipt.get('launcher_error'), 'cleanup': receipt['cleanup']}))
cleanup_ok = receipt['cleanup'].get('package_absent') is True and receipt['cleanup'].get('owned_reverse_absent') is True and not any(k.endswith('_error') for k in receipt['cleanup'])
raise SystemExit(0 if receipt.get('probe', {}).get('result') == 'pass' and not receipt.get('launcher_error') and cleanup_ok else 1)
