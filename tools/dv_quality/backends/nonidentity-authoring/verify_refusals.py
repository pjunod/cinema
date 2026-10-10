"""A clean guarded refusal is exact exit1, diagnostic and zero render output."""
import json
from registered_limits import read_json


def verify(root):
    admission = read_json(root/'negative-admission.json')
    cases = [('unsupported_matrix','matrix-rejected','supplied standard matrix domain only'),
             ('creative_trims','trim-rejected','no creative trims in this bounded control')]
    for key,directory,reason in cases:
        path = root/'gpu'/directory
        if type(admission[key]) is not int or admission[key] != 1:
            raise ValueError('clean refusal exit must be1')
        if (path/'probe.stderr').read_text() != 'Probe failed: '+reason+'\n':
            raise ValueError('clean refusal diagnostic differs')
        outputs = path/'outputs'
        if outputs.exists() and (not outputs.is_dir() or any(outputs.iterdir())):
            raise ValueError('clean refusal unexpectedly produced output')
        for line in (path/'probe.jsonl').read_text().splitlines():
            event = json.loads(line)
            if event.get('kind') != 'parsed_luma_curve':
                raise ValueError('clean refusal unexpectedly emitted render/frame event')
    return admission
