#!/bin/sh
set -eu
[ "$#" -eq 3 ] || { echo 'usage: replay_cross.sh NEW-scratch parsed-replay authoring-evidence' >&2; exit 2; }
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
image_id=$(python3 "$bundle_dir/image_identity.py" read "$2/image-id.txt")
case "$1" in /*) ;; *) exit 2 ;; esac
[ ! -e "$1" ] && [ ! -L "$1" ] || exit 2
scratch_dir=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
case "$scratch_dir" in "$bundle_dir"|"$bundle_dir"/*) exit 2 ;; esac
mkdir "$scratch_dir"
printf '%s\n' "$image_id" > "$scratch_dir/image-id.txt"
for directory in prefix include lib; do cp -R "$2/$directory" "$scratch_dir/$directory"; done
for source in p81_export_probe.c hdr10_baseline_probe.c; do cp "$bundle_dir/$source" "$scratch_dir/$source"; done
cp "$3/decoded.yuv420p10le" "$scratch_dir/decoded.yuv420p10le"
for frame in 0 1 2 3; do cp "$3/adapted-rpu-frame$frame.nal" "$scratch_dir/"; done
python3 - "$scratch_dir" <<'PY'
import hashlib,json,sys
from pathlib import Path
r=Path(sys.argv[1]);files=[r/'decoded.yuv420p10le',*sorted(r.glob('adapted-rpu-frame*.nal'))]
(r/'input-hashes.json').write_text(json.dumps({p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in files},indent=2,allow_nan=False)+'\n')
PY
docker run --rm --network none --cpus 2 --memory 2g --mount "type=bind,source=$scratch_dir,target=/work" "$image_id" sh -ec '
    export PKG_CONFIG_PATH=/work/prefix/lib/aarch64-linux-gnu/pkgconfig LD_LIBRARY_PATH=/work/prefix/lib/aarch64-linux-gnu
    export XDG_RUNTIME_DIR=/tmp/runtime-m0 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json
    mkdir "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
    for probe in p81_export_probe hdr10_baseline_probe; do
        cc -Wall -Wextra -Werror -std=c11 -I/work/include /work/$probe.c /work/lib/libdovi.a -o /work/$probe-bin $(pkg-config --cflags --libs libplacebo) -lm -lpthread -ldl
        for i in 0 1 2 3; do
            mkdir -p /work/$probe/frame-$i/outputs
            cd /work/$probe/frame-$i
            cp /work/decoded.yuv420p10le .
            /work/$probe-bin /work/adapted-rpu-frame$i.nal $i > probe.jsonl 2> probe.stderr
        done
    done
' > "$scratch_dir/cross-run.log" 2>&1
python3 "$bundle_dir/check_cross.py" "$scratch_dir" > "$scratch_dir/four-frame-results.json"
python3 "$bundle_dir/image_identity.py" record "$scratch_dir" cross "$image_id"
