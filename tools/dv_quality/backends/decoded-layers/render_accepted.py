"""Admit known paired decoder evidence before invoking the isolated renderer."""
import json
from pathlib import Path
import subprocess
import sys
import check_association
import image_identity
root = Path(sys.argv[1]).resolve()
image_id = image_identity.inspect_identity(root / 'image-id.txt')
accepted = [check_association.inspect(root, b, 'normal') for b in (0, 2)]
base = ['docker', 'run', '--rm', '--network', 'none', '--cpus', '2', '--memory', '2g', '--mount', f'type=bind,source={root},target=/work', image_id]
compile_script = 'export PKG_CONFIG_PATH=/work/prefix/lib/aarch64-linux-gnu/pkgconfig LD_LIBRARY_PATH=/work/prefix/lib/aarch64-linux-gnu; cc -Wall -Wextra -Werror -std=c11 -I/work/include /work/decoded_export_probe.c /work/lib/libdovi.a -o /work/decoded_export_probe $(pkg-config --cflags --libs libplacebo) -lm -lpthread -ldl'
with (root / 'renderer-compile.log').open('w') as log:
    subprocess.run([*base, 'sh', '-ec', compile_script], stdout=log, stderr=subprocess.STDOUT, check=True)
for receipt in accepted:
    b = receipt['bframes']
    for pair in receipt['pairs']:
        source = pair['source_frame']
        out = root / f'b{b}-normal/render/frame-{source}'
        out.mkdir(parents=True, exist_ok=False)
        (out / 'outputs').mkdir()
        command = 'export LD_LIBRARY_PATH=/work/prefix/lib/aarch64-linux-gnu XDG_RUNTIME_DIR=/tmp/runtime-m0 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json; mkdir "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"; cd "$1"; /work/decoded_export_probe "$2" "$3" "$4" "$5" "$6" > probe.jsonl 2> probe.stderr'
        target = '/work/' + str(out.relative_to(root))
        inputs = ['/work/' + pair[key] for key in ['rpu_path', 'bl_path', 'el_path']]
        subprocess.run([*base, 'sh', '-ec', command, 'bridge-render', target, *inputs, pair['pts'], pair['duration']], check=True)
        (out / 'association.json').write_text(json.dumps(pair, indent=2, allow_nan=False) + '\n')
