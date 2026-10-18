#!/bin/sh
# Register the pinned media package's Intel provider in ordinary loader discovery.
set -eu
[ "$#" = 2 ] || { echo 'usage: register-vulkan-driver.sh PACKAGE_ROOT ICD_DIRECTORY' >&2; exit 2; }
package_root=$1
icd_directory=$2
manifest="$package_root/share/vulkan/icd.d/intel_icd.x86_64.json"
library="$package_root/lib/libvulkan_intel.so"
[ -f "$manifest" ] && [ -f "$library" ] || { echo 'pinned Intel Vulkan provider unavailable' >&2; exit 1; }
# Relocating a relative library path would change its meaning. The pinned
# package uses this absolute path; refuse drift rather than guessing a rewrite.
grep -F "\"library_path\": \"$library\"" "$manifest" >/dev/null || {
    echo 'Intel Vulkan provider has an unexpected library path' >&2; exit 1;
}
mkdir -p "$icd_directory"
# Replace the obsolete provider for this vendor, rather than allowing two
# drivers for one adapter and relying on directory enumeration order. Retain
# the separate legacy HASVK, other vendors and software fallback providers.
cp "$manifest" "$icd_directory/intel_icd.x86_64.json"
