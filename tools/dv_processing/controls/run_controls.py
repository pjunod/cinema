"""Actual held-window, refusal, encode and packet-preservation controls.

Runs in the pinned Docker image with this scratch mounted at /work.
Known generated output directories are replaced on replay.
"""
from pathlib import Path
from fractions import Fraction
import os, subprocess, json, hashlib, struct
import shutil
def test_window_metadata_graph():
    R = Path('/work')
    env = os.environ | {'LD_LIBRARY_PATH': '/work/prefix/lib/aarch64-linux-gnu', 'VK_ICD_FILENAMES': '/usr/share/vulkan/icd.d/lvp_icd.aarch64.json', 'XDG_RUNTIME_DIR': '/work/runtime'}
    (R / 'runtime').mkdir(exist_ok=True)
    (R / 'runtime').chmod(448)
    results = []

    def renderer(name, source, start='42/1000', end='167/1000', want=0, reason=None, diagnostics=True):
        d = R / name / 'output'
        if d.exists():
            shutil.rmtree(d)
        d.mkdir()
        with open(source, 'rb') as src, (d / 'out.nut').open('xb') as out:
            p = subprocess.run([str(R / 'bin/segment_decode_render'), f'/proc/self/fd/{src.fileno()}', '.', '0', '64', '64', '64', '64', '64', '0', 'bt2020-pq-master-clip', f'/proc/self/fd/{out.fileno()}', start, end, '512'], cwd=d, env=env | {'PLURX_DV_FRAME_HASHES': '1' if diagnostics else '0'}, pass_fds=(src.fileno(), out.fileno()), capture_output=True)
        (d / 'stdout').write_bytes(p.stdout)
        (d / 'stderr').write_bytes(p.stderr)
        events = [json.loads(l) for l in p.stdout.decode().splitlines()]
        if want:
            prefix = 'Segment refused' if name == 'long-terminal' else 'Probe failed'
            assert p.returncode == 1 and p.stderr.decode().strip() == f'{prefix}: {reason}', (name, p.returncode, p.stderr.decode())
            assert not any((e['kind'] in ('segment_complete', 'window_complete') for e in events))
            if name in ('invalid-area', 'nonzero-eotf', 'binding-max', 'long-l8'):
                assert not any((e['kind'] == 'gpu_context_created' for e in events))
        else:
            assert p.returncode == 0, (name, p.returncode, p.stderr.decode())
            c = next((e for e in events if e['kind'] == 'window_complete'))
            assert c['preroll_pairs'] == 1 and c['frames'] == 3 and (not c['source_eof_observed'])
        if not want:
            rendered = [e for e in events if e['kind'] == 'rendered_frame']
            assert [e['pts'] for e in rendered] == ['42/1000', '83/1000', '125/1000']
            if diagnostics:
                assert len({e['rgb_sha256'] for e in rendered}) == 3
            else:
                assert all('rgb_sha256' not in e for e in rendered)
                assert all('bl_sha256' not in e and 'el_sha256' not in e for e in events if e['kind'] == 'accepted_source_pair')
            runtime = next(e for e in events if e['kind'] == 'gpu_runtime')
            assert len(runtime['device_uuid']) == len(runtime['driver_uuid']) == 32
            assert runtime['vendor_id'] > 0 and runtime['api_version'] > 0
            if name == 'valid':
                parsed = [e for e in events if e['kind'] == 'parsed_rpu']
                assert len(parsed) == 3 and all((e['creative_l2_count'] == 2 and e['creative_l8_count'] == 1 and (e['creative_trims_applied'] is False) for e in parsed))
        results.append({'case': name, 'exit': p.returncode, 'reason': reason, 'gpu_contexts': sum((e['kind'] == 'gpu_context_created' for e in events))})
        return d
    valid = renderer('valid', R / 'valid/source.mkv')
    (R / 'production').mkdir(exist_ok=True)
    production = renderer('production', R / 'valid/source.mkv', diagnostics=False)
    plain = renderer('plain', R / 'plain/source.mkv')
    for name, reason in [('invalid-area', 'invalid active area metadata'), ('nonzero-eotf', 'unsupported EOTF parameters'), ('binding-max', 'unsupported bounded vdr_in_max residual clipping'), ('long-l8', 'unsupported extended L8 trim format')]:
        renderer(name, R / name / 'source.mkv', want=1, reason=reason)
    raw = (R / 'long-terminal-unpatched.mkv').read_bytes()
    marker = b'D\x89\x88'
    pos = raw.index(marker) + 3
    actual = struct.unpack_from('>d', raw, pos)[0]
    assert actual > 1000, actual
    bad = bytearray(raw)
    struct.pack_into('>d', bad, pos, 600.0)
    (R / 'long-terminal').mkdir(exist_ok=True)
    (R / 'long-terminal/source.mkv').write_bytes(bad)
    renderer('long-terminal', R / 'long-terminal/source.mkv', '708/1000', '749/1000', want=1, reason='source EOF extent unavailable or inconsistent')

    def decode(path, pix):
        return subprocess.check_output(['ffmpeg', '-nostdin', '-v', 'error', '-threads', '1', '-i', str(path), '-f', 'rawvideo', '-pix_fmt', pix, 'pipe:1'])
    a = decode(valid / 'out.nut', 'rgb48le')
    b = decode(plain / 'out.nut', 'rgb48le')
    assert a == b == decode(production / 'out.nut', 'rgb48le')
    results.append({'case': 'retained-trims-unapplied-master', 'rgb_sha256': hashlib.sha256(a).hexdigest(), 'rgb_bytes': len(a), 'same_plain_master': True})
    encoded = valid / 'encoded.mp4'
    cmd = ['ffmpeg', '-nostdin', '-v', 'error', '-n', '-threads', '1', '-copyts', '-i', str(valid / 'out.nut'), '-filter_threads', '1', '-vf', 'zscale=matrixin=gbr:transferin=smpte2084:primariesin=2020:rangein=full:matrix=2020_ncl:transfer=smpte2084:primaries=2020:range=limited:chromal=center:filter=point,format=yuv420p10le', '-c:v', 'libx265', '-threads', '1', '-profile:v', 'main10', '-x265-params', 'pools=none:frame-threads=1:qp=0:bframes=2:repeat-headers=1:chromaloc=1:colorprim=9:transfer=16:colormatrix=9:range=limited', '-fps_mode', 'passthrough', '-enc_time_base', '1/1000', '-r', '24', '-an', str(encoded)]
    p = subprocess.run(cmd, capture_output=True)
    (valid / 'encoder.stderr').write_bytes(p.stderr)
    assert p.returncode == 0, p.stderr.decode()
    result = subprocess.run([str(R / 'bin/author_p81'), str(encoded), str(valid / 'timing.tsv'), str(valid / 'rpus'), str(valid / 'authored.mkv')], capture_output=True, env=env)
    (valid / 'author.stdout').write_bytes(result.stdout)
    (valid / 'author.stderr').write_bytes(result.stderr)
    assert result.returncode == 0, result.stderr.decode()
    receipts = [json.loads(l) for l in result.stdout.decode().splitlines()]
    assert len(receipts) == 3

    def probe(path):
        return json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_streams', '-show_packets', '-show_data', '-of', 'json', str(path)]))

    def payload(packet):
        return bytes.fromhex(''.join((l.split(':', 1)[1].split('  ', 1)[0].replace(' ', '') for l in packet['data'].strip().splitlines())))
    before = probe(encoded)
    after = probe(valid / 'authored.mkv')
    (valid / 'author-probe.json').write_text(json.dumps(after, indent=2))
    (R / 'adapted').mkdir(exist_ok=True)
    for orig, new, row in zip(before['packets'], after['packets'], receipts, strict=True):
        x = payload(orig)
        y = payload(new)
        assert y[:len(x)] == x
        size = int.from_bytes(y[len(x):len(x) + 4], 'big')
        nal = y[len(x) + 4:]
        assert len(nal) == size
        assert hashlib.sha256(nal).hexdigest() == row['adapted_rpu_sha256']
        assert hashlib.sha256((valid / 'rpus' / f"frame-{row['renderer_frame']:03d}.nal").read_bytes()).hexdigest() == row['source_rpu_sha256']
        (R / 'adapted' / f"frame-{row['renderer_frame']:03d}.nal").write_bytes(nal)
    assert decode(encoded, 'yuv420p10le') == decode(valid / 'authored.mkv', 'yuv420p10le')
    results.append({'case': 'authoring-encoded-base-unchanged', 'frames': 3, 'decoded_yuv_sha256': hashlib.sha256(decode(encoded, 'yuv420p10le')).hexdigest(), 'coded_packet_prefixes_unchanged': True, 'rpu_hash_binding': True})
    (R / 'actual-results.json').write_text(json.dumps(results, indent=2) + '\n')


def test_fmp4_output_grid():
    subprocess.run(["/work/test_fmp4_grid"], check=True, timeout=10)


if __name__ == "__main__":
    test_window_metadata_graph()
    test_fmp4_output_grid()
