#!/bin/sh
# Print the identity of every file that defines the published DV build
# dependencies. The dependency image records this at publication; an
# application build compares its own tree and refuses a dependency prefix
# that was built from different inputs.
set -eu
[ "$#" -eq 0 ] || { echo 'usage: dependency-inputs.sh' >&2; exit 2; }
LC_ALL=C
export LC_ALL
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
inputs=${PLURX_DV_BUILD_INPUTS:-"$source_dir/../dv_quality/backends"}
cd "$source_dir"
sha256sum build-dependencies.sh build-requirements.txt fetch_renderer_sources.py *.patch
cd "$inputs"
sha256sum parsed-rpu/fetch_parser.py parsed-rpu/resolved-Cargo.lock
