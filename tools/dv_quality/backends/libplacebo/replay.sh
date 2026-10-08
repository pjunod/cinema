#!/bin/sh
# Run the source-only mechanics probe in an isolated local Linux ARM64 container.
set -eu
if [ "$#" -ne 1 ]; then
    echo "usage: replay.sh /absolute/scratch-directory" >&2
    exit 2
fi
case "$1" in /*) ;; *) echo "scratch path must be absolute" >&2; exit 2 ;; esac
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -e "$1" ] || [ -L "$1" ]; then
    echo "scratch directory must not already exist" >&2
    exit 2
fi
scratch_dir=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")
case "$scratch_dir" in
    "$bundle_dir"|"$bundle_dir"/*)
        echo "scratch directory must be outside the source bundle" >&2
        exit 2 ;;
esac
# Atomic creation refuses a concurrent creation; its parent must exist.
mkdir "$scratch_dir"
python3 "$bundle_dir/fetch_sources.py" "$scratch_dir"
for filename in Dockerfile build.sh run-probe.sh fel_export_probe.c; do
    cp "$bundle_dir/$filename" "$scratch_dir/$filename"
done
# Only the Dockerfile is a build input: source is bound into the build container.
printf '*\n!Dockerfile\n' > "$scratch_dir/.dockerignore"
docker build --platform linux/arm64 -t codex/plurx-dv-m0:mechanics "$scratch_dir" > "$scratch_dir/image-build.log" 2>&1
docker image inspect codex/plurx-dv-m0:mechanics --format '{{.Id}}' > "$scratch_dir/image-id.txt"
docker run --rm --cpus 3 --memory 4g --network none --mount "type=bind,source=$scratch_dir,target=/work" codex/plurx-dv-m0:mechanics sh /work/build.sh > "$scratch_dir/libplacebo-build.log" 2>&1
docker run --rm --cpus 3 --memory 4g --network none --mount "type=bind,source=$scratch_dir,target=/work" codex/plurx-dv-m0:mechanics sh /work/run-probe.sh > "$scratch_dir/probe-run.log" 2>&1
python3 "$bundle_dir/identity_reference.py" "$scratch_dir/identity-control"
for filename in source-definition.json reference-method.json; do
    cp "$scratch_dir/identity-control/$filename" "$scratch_dir/$filename"
done
for filename in nonzero-reference.rgb48le base-reference.rgb48le shifted-reference.rgb48le; do
    cp "$scratch_dir/identity-control/$filename" "$scratch_dir/outputs/$filename"
done
mkdir -p "$scratch_dir/bundle"
cp "$bundle_dir/identity_reference.py" "$scratch_dir/bundle/identity_reference.py"
python3 "$bundle_dir/check_probe.py" "$scratch_dir"
