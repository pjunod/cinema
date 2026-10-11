#!/bin/sh
set -eu
# Tests may substitute a process that fails after the actual decoder writes.
decoder=${TIMELINE_TEST_DECODER:-./decode_timeline}
run_case() {
    case_name=$1
    shift
    mkdir -p "$case_name/frames"
    status=0
    "$decoder" "$1" "$case_name/frames" "$2" ${3:+"$3"} > "$case_name/decode.jsonl" 2> "$case_name/decode.stderr" || status=$?
    printf '%s\n' "$status" > "$case_name/decode-status.txt"
    # Every fixture here is expected to decode successfully. Association
    # negatives are handled later; a helper failure stops this driver now.
    [ "$status" -eq 0 ] || return "$status"
}
for b in 0 2; do run_case normal-b$b compound-vfr-b$b.mkv normal; done
for mode in seek-reset seek-no-reset; do run_case "$mode" compound-vfr-b2.mkv "$mode"; done
for mode in epochs-reset epochs-no-reset; do run_case "$mode" compound-segment0.mkv "$mode" compound-segment1.mkv; done
run_case cross-epoch-el compound-segment0.mkv epochs-reset compound-cross-epoch-el.mkv
run_case implicit-seek compound-implicit.mkv seek-reset
run_case swapped-rpu compound-swapped-rpu.mkv normal
printf 'passed\n' > decoder-driver-passed.txt
