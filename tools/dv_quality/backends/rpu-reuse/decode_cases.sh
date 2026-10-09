#!/bin/sh
set -eu
[ "$#" -eq 2 ] || exit 2
root=$1
decoder=$2
for mode in normal missing-rpu seek-reset; do
    mkdir "$root/$mode"
    set +e
    python3 "$(dirname -- "$0")/execute_stage.py" "$root" decode "$mode" "$decoder"
    status=$?
    set -e
    [ "$status" -eq 0 ] || exit "$status"
done
printf 'completed cleanly\n' > "$root/decode-success.txt"
