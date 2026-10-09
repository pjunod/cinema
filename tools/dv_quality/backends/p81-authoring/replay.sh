#!/bin/bash
# Requires Docker Linux arm64 support and Python3 with tarfile data filtering.
# Only scratch evidence is mounted. No repository credentials or .git history.
set -euo pipefail
TASK_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
TASK_IMAGE=codex/plurx-dv-m0-authoring:replay
python3 "$TASK_ROOT/fetch_dovi.py"
docker build --platform linux/arm64 --file "$TASK_ROOT/Dockerfile" \
    --tag "$TASK_IMAGE" "$TASK_ROOT" > "$TASK_ROOT/docker-build.log" 2>&1
docker image inspect "$TASK_IMAGE" --format '{{.Id}}' > "$TASK_ROOT/replay-image-id.txt"
for stage in generate_rpus encode checks; do
    docker run --rm --platform linux/arm64 --cpus 2 --memory 2g \
        --security-opt no-new-privileges \
        --mount "type=bind,source=$TASK_ROOT,target=/work" \
        "$TASK_IMAGE" bash "/work/$stage.sh"
done
python3 "$TASK_ROOT/write_receipt.py"
