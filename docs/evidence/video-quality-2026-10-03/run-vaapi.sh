#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -gt 2 ]; then
 echo 'Usage: run-vaapi.sh [RECIPE] [continuous-pq-ramp|original-sharp-panels]' >&2
 exit 2
fi
signal=${2-continuous-pq-ramp}
case "$signal" in
 continuous-pq-ramp|original-sharp-panels) ;;
 *) echo 'Unsupported diagnostic signal; no qualification started' >&2; exit 2 ;;
esac
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
recipe=${1:-"$script_dir/vaapi-recipe.json"}
# Each invocation owns a private path; historical receipts are never overwritten.
umask 077
root=$(mktemp -d /var/tmp/plurx-vaapi-qualification.XXXXXXXX)
container="$(basename -- "$root")"
container_id=''
container_attempted=false
watcher=''
watcher_running() {
 local job
 # Completed jobs may remain in jobs -p until wait; stopped jobs still need KILL.
 for job in $(jobs -pr) $(jobs -ps); do [ "$job" != "$watcher" ] || return 0; done
 return 1
}
cleanup() {
 local status=$?
 trap - EXIT
 set +e  # Cleanup failures must not replace the original command/signal exit.
 # Bounded TERM (3 s), then KILL (2 s); never wait on a still-running job.
 local watcher_state=not_started watcher_wait_exit='' attempt
 if [ -n "$watcher" ]; then
  if watcher_running; then kill "$watcher" 2>/dev/null || true; fi
  for ((attempt=0; attempt<30; attempt++)); do
   watcher_running || break
   sleep 0.1
  done
  if watcher_running; then
   kill -KILL "$watcher" 2>/dev/null || true
   for ((attempt=0; attempt<20; attempt++)); do
    watcher_running || break
    sleep 0.1
   done
  fi
  if watcher_running; then
   watcher_state=stop_not_confirmed
  else
   watcher_state=stopped
   if wait "$watcher" 2>/dev/null; then watcher_wait_exit=0; else watcher_wait_exit=$?; fi
  fi
 fi
 # Docker can create the container before a failed/interrupted command assigns stdout.
 local cid='' cid_source=unavailable candidate='' cid_bytes
 if [[ "$container_id" =~ ^[0-9a-f]{64}$ ]]; then
  cid=$container_id; cid_source=stdout
 elif [ ! -L "$root/container.id" ] && [ -f "$root/container.id" ]; then
  cid_bytes=$(wc -c < "$root/container.id")
  if [ "$cid_bytes" -eq 64 ] || [ "$cid_bytes" -eq 65 ]; then
   candidate=$(< "$root/container.id")
   if [[ "$candidate" =~ ^[0-9a-f]{64}$ ]]; then cid=$candidate; cid_source=private_cidfile; fi
  fi
 fi
 local container_state=not_started remove_exit=''
 if [ -n "$cid" ]; then
  if timeout --kill-after=1 5 docker rm -f "$cid" > "$root/container-cleanup.log" 2>&1; then
   container_state=removed; remove_exit=0
  else
   remove_exit=$?; container_state=failed_or_unconfirmed
  fi
 elif [ "$container_attempted" = true ]; then
  container_state=identity_unresolved
 fi
 local retained=false
 if [ "$status" -ne 0 ] && [ "$(du -sk "$root" | cut -f1)" -le 524288 ]; then
  retained=true
 else
  rm -f -- "$root/source.yuv" "$root/source.mkv" "$root/encoded.mp4" "$root/decoded.yuv"
 fi
 if python3 - "$root" "$status" "$retained" "$cid" "$cid_source" "$container_state" "$remove_exit" "$watcher_state" "$watcher_wait_exit" <<'PY'
import datetime,json,pathlib,sys
root=pathlib.Path(sys.argv[1])
now=datetime.datetime.now(datetime.timezone.utc)
media=[dict(name=name,bytes=(root/name).stat().st_size)
       for name in ('source.yuv','source.mkv','encoded.mp4','decoded.yuv') if (root/name).is_file()]
record=dict(owned_private_path=str(root),exit_status=int(sys.argv[2]),
 media_retained=bool(media),retention_requested=sys.argv[3]=='true',media_files=media,max_bundle_bytes=536870912,
 review_or_delete_by=(now+datetime.timedelta(hours=48)).isoformat(),
 policy='Owner must archive privately or delete the four exact media files within 48 hours; '
        'no automatic broad temporary-directory collector. Successful or over-budget media are removed.',
 cleanup_files=['source.yuv','source.mkv','encoded.mp4','decoded.yuv'])
record['cleanup']=dict(container_id=sys.argv[4] or None,identity_source=sys.argv[5],
 container_result=sys.argv[6],remove_exit=int(sys.argv[7]) if sys.argv[7] else None,
 container_deadline_seconds=5,container_kill_after_seconds=1,
 watcher_result=sys.argv[8],watcher_wait_exit=int(sys.argv[9]) if sys.argv[9] else None,
 watcher_term_seconds=3,watcher_kill_seconds=2,
 confirmed=sys.argv[6] in ('not_started','removed') and sys.argv[8] in ('not_started','stopped'),
 recovery='If cleanup is unconfirmed, owner must use only the recorded exact CID or privately '
          'resolve the owned cidfile; no container-name deletion or guessed production identity.')
(root/'retention.json').write_text(json.dumps(record,indent=2)+'\n')
PY
 then
  echo "Private capture directory: $root; retention/cleanup policy: $root/retention.json"
 else
  echo "Cleanup receipt unavailable; preserve private recovery path: $root" >&2
 fi
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
container_attempted=true
container_id=$(docker run --detach --rm --cidfile "$root/container.id" --name "$container" --network none --cpus 2 --memory 2g \
 --pids-limit 128 --user "$(id -u):$(id -g)" --group-add "$(stat -c %g /dev/dri/renderD128)" --device /dev/dri/renderD128 \
 --mount "type=bind,src=$root,dst=$root" --entrypoint /bin/sleep "$image" 240)
(
 while sleep 2; do
  if ! idle || [ "$(du -sk "$root" | cut -f1)" -gt 524288 ]; then
   echo 'ABORT: playback activity changed or scratch limit exceeded' > "$root/aborted.txt"
   # This early stop is also bounded; the main exit trap records its own result.
   timeout --kill-after=1 5 docker rm -f "$container_id" > "$root/watcher-container-cleanup.log" 2>&1 || true
   exit 0
  fi
 done
) &
watcher=$!
timeout --kill-after=5 200 python3 qualify-vaapi.py "$root" "$container_id" "$signal"
test ! -e aborted.txt
idle
