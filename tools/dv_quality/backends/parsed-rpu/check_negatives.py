"""Require controlled parser refusals before renderer setup or frame writes."""
import json
from pathlib import Path
import sys

EXPECTED = {
    'missing': 'Probe failed: RPU input exists\n',
    'truncated': 'RPU parser rejected input: Failed parsing RPU: Invalid RPU length: 16\n',
    'bounded': 'Probe failed: unsupported bounded vdr_in_max residual clipping\n',
    'missing-el': 'Probe failed: missing EL rejected for requested residual reconstruction\n',
}


def check(root):
    results = []
    for control, diagnostic in EXPECTED.items():
        folder = Path(root) / 'negatives' / control
        if (folder / 'status.txt').read_text() != '1\n':
            raise ValueError(f'{control}: expected controlled exit1, not crash or success')
        if (folder / 'stderr.txt').read_text() != diagnostic:
            raise ValueError(f'{control}: refusal reason differs')
        stdout = (folder / 'stdout.txt').read_text()
        events = [json.loads(line) for line in stdout.splitlines()]
        if control == 'missing-el':
            if (len(events) != 1 or events[0].get('kind') != 'parsed_rpu' or
                    events[0].get('parser_error') is not False):
                raise ValueError('missing-el: only successful metadata parse may precede refusal')
        elif events:
            raise ValueError(f'{control}: unexpected output or renderer event')
        output = folder / 'outputs'
        if not output.is_dir() or any(output.iterdir()):
            raise ValueError(f'{control}: frame artifact written during refusal')
        if {path.name for path in folder.iterdir()} != {'status.txt','stderr.txt','stdout.txt','outputs'}:
            raise ValueError(f'{control}: unexpected artifact during refusal')
        results.append({'control':control,'exit_status':1,'expected_reason':diagnostic.rstrip(),
                        'renderer_started':False,'frame_writes':False})
    return results


if __name__ == '__main__':
    print(json.dumps({'scope':'controlled-refusal','results':check(sys.argv[1])},indent=2,allow_nan=False))
