#!/usr/bin/env bash
# Point apt at snapshot.ubuntu.com at the timestamp pinned in upstream.lock,
# so a build or a verification months later resolves the same packages.
#
#   packaging/apt-snapshot.sh <packaging/upstream.lock>
set -euo pipefail
ts=$(awk '$1 == "apt-snapshot" {print $2}' "$1")
[[ $ts =~ ^[0-9]{8}T[0-9]{6}Z$ ]] || { echo "apt-snapshot: no apt-snapshot row in $1" >&2; exit 1; }
. /etc/os-release
rm -f /etc/apt/sources.list
cat > /etc/apt/sources.list.d/ubuntu.sources <<EOS
Types: deb
URIs: https://snapshot.ubuntu.com/ubuntu/$ts/
Suites: $VERSION_CODENAME $VERSION_CODENAME-updates $VERSION_CODENAME-security
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOS
export DEBIAN_FRONTEND=noninteractive
# The stock image has no CA certificates, so ca-certificates itself comes over
# unverified TLS. Integrity does not rest on TLS: apt checks every index
# against the archive's InRelease signature (Signed-By above) and every
# package against its index. Everything after this uses verified TLS.
apt-get -q -o Acquire::https::Verify-Peer=false update
apt-get -q -o Acquire::https::Verify-Peer=false install -y --no-install-recommends ca-certificates
apt-get -q update
echo "apt-snapshot: snapshot.ubuntu.com $ts"
