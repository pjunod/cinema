#!/bin/sh
# F+ acceptance (FEL-RPU-REFIT-BUILD.md milestone 3): two real
# `--refit-stats` runs over one source window print the same bytes, and the
# job's parse and fit of both give the same curve digest as the recorded run
# (the ignored `two_real_helper_runs_give_the_same_digest` test).
#
# usage, from the repository root:
#   tools/dv_processing/controls/refit_digest_twice.sh OUTDIR HELPER-COMMAND...
# where HELPER-COMMAND prints one run on stdout, for example
#   segment_decode_render --refit-stats michael5400-source.mkv 0 --threads 2
set -eu
out=$1
shift
"$@" > "$out/run-a.jsonl"
"$@" > "$out/run-b.jsonl"
cmp "$out/run-a.jsonl" "$out/run-b.jsonl"
PLURX_REFIT_RUN_A="$out/run-a.jsonl" PLURX_REFIT_RUN_B="$out/run-b.jsonl" \
  cargo test -p plurx-core --lib two_real_helper_runs_give_the_same_digest -- --ignored
