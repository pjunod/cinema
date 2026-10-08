#!/bin/sh
set -eu
export PKG_CONFIG_PATH=/work/ffmpeg-prefix/lib/pkgconfig
for helper in mux_layers decode_layers; do
    cc -std=c11 -Wall -Wextra -Werror /work/$helper.c -o /work/$helper $(pkg-config --static --cflags --libs libavformat libavcodec libavutil)
done
for b in 0 2; do
    for mode in normal missing-el swapped-el swapped-rpu missing-rpu; do
        mkdir -p /work/b$b-$mode/frames
        /work/mux_layers /work/bl-b$b.mkv /work/el-b$b.mkv /work/rpu-tags /work/b$b-$mode/compound.mkv $mode > /work/b$b-$mode/mux.jsonl 2> /work/b$b-$mode/mux.stderr
        set +e
        /work/decode_layers /work/b$b-$mode/compound.mkv /work/b$b-$mode/frames > /work/b$b-$mode/decode.jsonl 2> /work/b$b-$mode/decode.stderr
        status=$?
        set -e
        printf "%s\n" "$status" > /work/b$b-$mode/decode-status.txt
        [ "$status" -eq 0 ] || [ "$status" -eq 1 ] || exit 65
        if [ "$mode" = normal ]; then [ "$status" -eq 0 ] || exit 65; fi
    done
done
