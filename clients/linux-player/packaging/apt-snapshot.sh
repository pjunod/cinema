#!/usr/bin/env bash
# Point apt at snapshot.ubuntu.com at the timestamp pinned in upstream.lock,
# so a build or a verification months later resolves the same packages.
#
#   packaging/apt-snapshot.sh <packaging/upstream.lock>
#
# With /apt-cache mounted (build-deb.sh does), apt keeps its indexes and the
# packages it downloads there. A snapshot never changes, so once its indexes
# have been fetched and verified against the archive signature, later
# containers reuse them without contacting the mirror, and install every
# package they have seen before from the cache: a rebuild needs no apt mirror.
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
# snapshot.ubuntu.com answers 503 for minutes at a time: retry each fetch
# with apt's growing delay rather than fail the whole build on the first one.
cat > /etc/apt/apt.conf.d/90plurx-retries <<EOC
Acquire::Retries "10";
Acquire::Retries::Delay "true";
Acquire::Retries::Delay::Maximum "60";
EOC
marker=
if [ -d /apt-cache ]; then
  mkdir -p /apt-cache/archives/partial /apt-cache/lists/partial
  cat > /etc/apt/apt.conf.d/90plurx-apt-cache <<EOC
Dir::Cache::archives "/apt-cache/archives";
Dir::State::lists "/apt-cache/lists";
APT::Keep-Downloaded-Packages "true";
APT::Sandbox::User "root";
EOC
  marker=/apt-cache/lists/.plurx-snapshot-$ts
  if [ -f "$marker" ]; then
    # Indexes signed and verified when they were fetched; apt checks every
    # package against them, cached or not.
    # The stock image has no CA bundle yet; the same trust argument as below.
    apt-get -q -o Acquire::https::Verify-Peer=false install -y --no-install-recommends ca-certificates
    echo "apt-snapshot: snapshot.ubuntu.com $ts (indexes from /apt-cache)"
    exit 0
  fi
fi
# The stock image has no CA certificates, so ca-certificates itself comes over
# unverified TLS. Integrity does not rest on TLS: apt checks every index
# against the archive's InRelease signature (Signed-By above) and every
# package against its index. Everything after this uses verified TLS.
# Error-Mode=any: an index that still fails after the retries fails the run,
# instead of apt's default warning, so a partial index is never marked.
apt-get -q -o APT::Update::Error-Mode=any -o Acquire::https::Verify-Peer=false update
apt-get -q -o Acquire::https::Verify-Peer=false install -y --no-install-recommends ca-certificates
apt-get -q -o APT::Update::Error-Mode=any update
[ -z "$marker" ] || touch "$marker"
echo "apt-snapshot: snapshot.ubuntu.com $ts"
