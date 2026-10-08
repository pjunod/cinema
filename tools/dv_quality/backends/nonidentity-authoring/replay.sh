#!/bin/bash
# Requirements: approved CPU builder replay and parsed GPU replay, not local tags.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
if test "$#" -ne 3; then
    echo 'usage: replay.sh NEW_SCRATCH CPU_REPLAY_ROOT PARSED_GPU_REPLAY_ROOT' >&2
    exit 64
fi
BUNDLE_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
python3 "$BUNDLE_ROOT/prepare_replay.py" "$BUNDLE_ROOT" "$1" "$2" "$3"
TASK_ROOT=$(cd -- "$1" && pwd)
CPU_IMAGE=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["cpu_image"])' "$TASK_ROOT/runtime-inputs.json")
GPU_IMAGE=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["gpu_image"])' "$TASK_ROOT/runtime-inputs.json")
python3 -c 'import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$TASK_ROOT/preregistered-controls.json" > "$TASK_ROOT/registered-limits.sha256"
for stage in generate cpu_controls; do
    docker run --rm --platform linux/amd64 --cpus 2 --memory 2g --security-opt no-new-privileges \
        --mount "type=bind,source=$TASK_ROOT,target=/work" "$CPU_IMAGE" bash "/work/$stage.sh"
done
python3 "$TASK_ROOT/scalar_cpu.py"
docker run --rm --platform linux/arm64 --network none --cpus 2 --memory 2g --security-opt no-new-privileges \
    --mount "type=bind,source=$TASK_ROOT,target=/work" "$GPU_IMAGE" bash /work/encode.sh
python3 "$TASK_ROOT/inject_negative.py"
python3 "$TASK_ROOT/check_hdr10.py"
docker run --rm --platform linux/arm64 --network none --cpus 2 --memory 2g --security-opt no-new-privileges \
    --mount "type=bind,source=$TASK_ROOT,target=/work" "$GPU_IMAGE" bash /work/render.sh
python3 "$TASK_ROOT/scalar_gpu.py"
python3 "$TASK_ROOT/check_fixture_association.py"
python3 "$TASK_ROOT/check_injector.py"
python3 "$TASK_ROOT/test_nonidentity_checks.py"
python3 "$TASK_ROOT/test_replay_contracts.py"
python3 "$TASK_ROOT/write_receipt.py"
