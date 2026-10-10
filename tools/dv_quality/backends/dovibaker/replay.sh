#!/bin/bash
# Requirements: Docker Desktop/Linux daemon and Python with tarfile data filtering.
# Only this evidence directory is mounted. No checkout, history or credentials.
# Public downloads and live Debian/PyPI repositories require network access.
set -euo pipefail
TASK_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
TASK_IMAGE="codex/plurx-dv-m0-baker:replay"
if test -e "$TASK_ROOT/src"; then
    echo 'Replay requires a fresh evidence directory without src/.' >&2
    exit 64
fi
python3 "$TASK_ROOT/fetch_sources.py"
docker build --platform linux/amd64 \
    --file "$TASK_ROOT/Dockerfile" --tag "$TASK_IMAGE" "$TASK_ROOT" \
    > "$TASK_ROOT/docker-build.log" 2>&1
docker image inspect "$TASK_IMAGE" \
    --format '{{.Id}}' > "$TASK_ROOT/replay-image-id.txt"
for task_stage in build controls; do
    docker run --rm --platform linux/amd64 --cpus 2 --memory 2g \
        --security-opt no-new-privileges \
        --mount "type=bind,source=$TASK_ROOT,target=/work" \
        "$TASK_IMAGE" bash "/work/$task_stage.sh"
done
python3 "$TASK_ROOT/write_replay_receipt.py"
