#!/usr/bin/env bash
set -euo pipefail
root=/tmp/plurx-vaapi-qualification-20261003
container=plurx-vaapi-qualification-20261003
cd "$root"
idle() {
  local metric
  metric=$(curl -fsS --max-time 5 http://127.0.0.1:32400/metrics | sed -n 's/^plurx_transcode_sessions_active //p')
  [ "$metric" = 0 ]
}
idle || { echo 'SKIP: active viewer or unavailable activity evidence'; exit 3; }
image=$(docker inspect plurxd --format '{{.Image}}')
revision=$(docker inspect plurxd --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
encoder_hash=$(docker exec plurxd sha256sum /usr/lib/jellyfin-ffmpeg/ffmpeg | cut -d' ' -f1)
python3 - "$image" "$revision" "$encoder_hash" <<'PY'
import json,sys,platform
json.dump(dict(image_id=sys.argv[1],deployed_revision=sys.argv[2],encoder_sha256=sys.argv[3],
 host=platform.node(),container_cpu_limit=2,container_memory_bytes=2147483648,
 container_network='none',max_scratch_media_bytes=536870912,viewer_watch_seconds=2),open('provenance.json','w'),indent=2)
PY
docker run --detach --rm --name "$container" --network none --cpus 2 --memory 2g \
 --pids-limit 128 --user "$(id -u):$(id -g)" --group-add "$(stat -c %g /dev/dri/renderD128)" --device /dev/dri/renderD128 \
 --mount "type=bind,src=$root,dst=$root" --entrypoint /bin/sleep "$image" 240 >/dev/null
watcher=''
cleanup() {
 if [ -n "$watcher" ]; then kill "$watcher" 2>/dev/null || true; wait "$watcher" 2>/dev/null || true; fi
 docker rm -f "$container" >/dev/null 2>&1 || true
 rm -f "$root/source.yuv" "$root/source.mkv" "$root/encoded.mp4" "$root/decoded.yuv"
}
trap cleanup EXIT INT TERM
(
 while sleep 2; do
  if ! idle || [ "$(du -sk "$root" | cut -f1)" -gt 524288 ]; then
   echo 'ABORT: playback activity changed or scratch limit exceeded' > "$root/aborted.txt"
   docker rm -f "$container" >/dev/null 2>&1 || true
   exit 0
  fi
 done
) &
watcher=$!
timeout --kill-after=5 200 python3 qualify-vaapi.py "$root" "$container"
test ! -e aborted.txt
idle
