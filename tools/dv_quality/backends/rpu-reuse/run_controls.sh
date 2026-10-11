#!/bin/sh
set -eu
export PYTHONDONTWRITEBYTECODE=1
export CARGO_HOME=/work/cargo-home
export TMPDIR=/work/tmp
mkdir "$TMPDIR"
export PKG_CONFIG_PATH=/work/ffmpeg-prefix/lib/pkgconfig
cc -std=c11 -Wall -Wextra -Werror /work/mux_reuse.c -o /work/mux_reuse $(pkg-config --static --cflags --libs libavformat libavcodec libavutil)
cc -std=c11 -Wall -Wextra -Werror /work/decode_reuse.c -o /work/decode_reuse $(pkg-config --static --cflags --libs libavformat libavcodec libavutil)
cc -std=c11 -Wall -Wextra -Werror -I/work/FFmpeg-bf1b838f2ab88b4f8fd83443325c782ea0e0f7fa /work/cache_probe.c -o /work/cache_probe $(pkg-config --static --cflags --libs libavformat libavcodec libavutil)
cargo build --offline --locked --release --manifest-path /work/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/Cargo.toml -p dolby_vision --features serde --example m0_inspect_reuse
inspector=/work/dovi_tool-83e1fdad6dcd5995556235946e7c5c0f9010d5a1/target/release/examples/m0_inspect_reuse
for mode in normal missing-rpu; do
    /work/mux_reuse /work/bl-vfr-b2.mkv /work/fresh-rpus "/work/$mode.mkv" "$mode" > "/work/$mode-mux.jsonl" 2> "/work/$mode-mux.stderr"
done
sh /work/decode_cases.sh /work /work/decode_reuse
for mode in normal missing-rpu seek-reset; do
    "$inspector" "/work/$mode" > "/work/$mode/inspect.log" 2>&1
done
python3 /work/record_provenance.py /work
for mode in same cold flush wrong-compression no-reset; do
    set +e
    python3 /work/execute_stage.py /work cache "$mode" /work/cache_probe
    status=$?
    set -e
    [ "$status" -eq 0 ] || exit "$status"
done
set +e
/work/mux_reuse /work/bl-vfr-b2.mkv /work/fresh-rpus /work/p7-refusal.mkv p7-refusal > /work/p7-refusal.jsonl 2> /work/p7-refusal.stderr
status=$?
set -e
printf '%s\n' "$status" > /work/p7-refusal.status
[ "$status" -eq 1 ] || exit 1
