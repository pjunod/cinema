#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
recipe=${1:-"$script_dir/vaapi-recipe.json"}
# Each invocation owns a private path; historical receipts are never overwritten.
umask 077
root=$(mktemp -d /var/tmp/plurx-vaapi-qualification.XXXXXXXX)
container="$(basename -- "$root")"
container_id=''
watcher=''
cleanup() {
 local status=$?
 trap - EXIT
 if [ -n "$watcher" ]; then kill "$watcher" 2>/dev/null || true; wait "$watcher" 2>/dev/null || true; fi
 # Docker can create the container before a failed/interrupted command assigns stdout.
 if [ -z "$container_id" ] && [ -f "$root/container.id" ]; then
  read -r container_id < "$root/container.id" || true
 fi
 if [[ "$container_id" =~ ^[0-9a-f]{64}$ ]]; then docker rm -f "$container_id" >/dev/null 2>&1 || true; fi
 local retained=false
 if [ "$status" -ne 0 ] && [ "$(du -sk "$root" | cut -f1)" -le 524288 ]; then
  retained=true
 else
  rm -f -- "$root/source.yuv" "$root/source.mkv" "$root/encoded.mp4" "$root/decoded.yuv"
 fi
 python3 - "$root" "$status" "$retained" <<'PY'
import datetime,json,pathlib,sys
root=pathlib.Path(sys.argv[1])
now=datetime.datetime.now(datetime.timezone.utc)
media=[dict(name=name,bytes=(root/name).stat().st_size)
       for name in ('source.yuv','source.mkv','encoded.mp4','decoded.yuv') if (root/name).is_file()]
record=dict(owned_private_path=str(root),exit_status=int(sys.argv[2]),
 media_retained=sys.argv[3]=='true' and bool(media),media_files=media,max_bundle_bytes=536870912,
 review_or_delete_by=(now+datetime.timedelta(hours=48)).isoformat(),
 policy='Owner must archive privately or delete the four exact media files within 48 hours; '
        'no automatic broad temporary-directory collector. Successful or over-budget media are removed.',
 cleanup_files=['source.yuv','source.mkv','encoded.mp4','decoded.yuv'])
(root/'retention.json').write_text(json.dumps(record,indent=2)+'\n')
PY
 echo "Private capture receipt: $root/qualification.json; retention policy: $root/retention.json"
 exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cp -- "$recipe" "$root/vaapi-recipe.json"
cp -- "$script_dir/qualify-vaapi.py" "$root/qualify-vaapi.py"
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
container_id=$(docker run --detach --rm --cidfile "$root/container.id" --name "$container" --network none --cpus 2 --memory 2g \
 --pids-limit 128 --user "$(id -u):$(id -g)" --group-add "$(stat -c %g /dev/dri/renderD128)" --device /dev/dri/renderD128 \
 --mount "type=bind,src=$root,dst=$root" --entrypoint /bin/sleep "$image" 240)
(
 while sleep 2; do
  if ! idle || [ "$(du -sk "$root" | cut -f1)" -gt 524288 ]; then
   echo 'ABORT: playback activity changed or scratch limit exceeded' > "$root/aborted.txt"
   docker rm -f "$container_id" >/dev/null 2>&1 || true
   exit 0
  fi
 done
) &
watcher=$!
timeout --kill-after=5 200 python3 qualify-vaapi.py "$root" "$container_id"
test ! -e aborted.txt
idle
