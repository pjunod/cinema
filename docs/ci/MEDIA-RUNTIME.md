# Media runtime — publish dependencies before building Cinema

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how changes
reach main) and [RUNNER-DISK.md](RUNNER-DISK.md) (builder storage limits).

**Status:** built; the runtime pin is the `runtime-assets` base in
`Dockerfile`. The DV helper bundle moved back to the application build on
2026-11-11; its dependency image is the `dv-processing-dependencies` pin
beside it, published and pinned on 2026-11-12.

## Application builds consume the committed digest

`make docker`, `make docker-up`, package smoke and current-source release
publication reuse two immutable images from one repository: the media runtime
and the DV helper build dependencies. The application Dockerfile compiles the
Rust binaries and the DV helper bundle and copies notices; its runtime
dependency closure contains no APT installs, FFmpeg compilation or subtitle
engine compilation. A cold application build downloads image layers and Rust
crates; it does not contact the Debian snapshot service.

`Dockerfile.media-runtime` owns the Debian snapshot, FFmpeg and driver packages,
static probe, VMAF scorer, subtitle recognizer, the service account, and the
DV helper *dependencies*: FFmpeg, libplacebo and libdovi built from pinned
sources, published with the compiler and headers the helpers need. Changing
application source never rebuilds these dependencies. Changes to those inputs
take effect only after publishing and committing a new pin.

## The application owns the DV helper bundle

The bundle at `/usr/lib/plurx/dv-processing` is application source.
`plurxd` compiles the identity of `tools/dv_processing` into itself
(`renderer_source_identity` in `crates/plurxd/src/dv_runtime.rs`) and refuses a
bundle whose `build-identity.json` names any other sources, so a bundle and the
daemon that loads it must come from one tree. The `dv-processing-helpers` stage
of `Dockerfile` therefore compiles the bundle from the build context against
the pinned dependency image, and the `runtime` stage removes whatever a media
base left in that directory, installs the bundle and requires every helper to
load. No published image carries a bundle.

**Why it moved:** from 2026-11-09 the bundle shipped inside the published
runtime. Helper changes merged afterwards never reached an image, and an image
built from them would have carried a daemon expecting sources its bundle did
not have: DV processing disabled, with "unsupported DV build identity".

The dependency image records the files it was built from
(`/opt/dv-dependencies/build-inputs.sha256`, printed by
`tools/dv_processing/dependency-inputs.sh`): the dependency recipe, the decoder
patch, the Python build requirements, the renderer source fetcher (libplacebo
7.387 and its Vulkan headers) and the two parser pin files under
`tools/dv_quality/backends/parsed-rpu`. `Dockerfile` records the same identity beside the
pin. A change to any of them fails
`tests/operations/test_dv_helper_ownership.py` in the fast lane and fails the
image build, until new dependencies are published and pinned. The helper
sources themselves need no publication: edit, build, ship.

The default repository is public: `ghcr.io/pjunod/cinema-media-runtime`. Docker,
Make and Compose use it without a login or host configuration, regardless of
whether the source was cloned from GitHub, Forgejo, SSH or a local archive. The
Git remote is never used to choose a registry.

The digest is fixed in source. To use an explicit mirror, set the full repository
in `PLURX_MEDIA_RUNTIME_REPOSITORY`, for example
`forge.lan:3000/noirr/plurx-media-runtime`. Direct Compose users can set the same
variable in `deploy/.env`. A mirror must serve exactly the same manifest bytes.
The registry must retain the manifest and all referenced layers; deleting them
breaks cold builds.

## Publish a dependency update explicitly

Run the manual **publish media runtime** workflow (`media-runtime.yml`) from
the source revision whose media inputs you intend to ship. Its default audited amd64 option builds and verifies the Dolby package; disable
that option only for an ordinary local-build base. It builds natively on amd64
and arm64 and publishes two images per platform: the runtime and the DV helper
build dependencies. Before recording either digest it builds the application
`Dockerfile`'s `runtime` target against both candidates, with placeholder
daemon binaries, so the bundle is proven to compile from that revision's
`tools/dv_processing` and load in the new runtime. It then assembles one index
for each image and uploads a private-candidate publication receipt naming
both. It does not prepare or modify an application Dockerfile.

Then, in order:

1. Run the manual public-copy workflow (`media-runtime-public.yml`) once for
   each index in the receipt: `Media-Runtime` and `DV-Build-Dependencies`.
2. Pin the runtime: `scripts/media-runtime pin
   ghcr.io/pjunod/cinema-media-runtime@sha256:<runtime index>`.
3. Pin the dependencies from a checkout of the published source revision:
   `scripts/media-runtime pin-dv-dependencies
   ghcr.io/pjunod/cinema-media-runtime@sha256:<dependency index>`. It writes
   the digest and that tree's dependency input identity together.
4. Review and commit both pins with the media input changes and evidence.

Each pin command obtains an anonymous GHCR token, verifies the raw index digest
and both platform manifests without Docker credentials, and only then updates
source. A private candidate alone cannot support external builders. The two
pins are independent: a decoder-patch change needs only step 3's image, and a
runtime package change only step 2's.

For an operator-managed builder, the equivalent maintenance commands are:

```bash
# Build and publish both platforms with a suitably configured native builder.
scripts/media-runtime build \
  --image forge.lan:3000/noirr/plurx-media-runtime:reviewed-update \
  --metadata "$TMPDIR/media-runtime-metadata.json"
scripts/media-runtime build --target dv-processing-dependencies \
  --image forge.lan:3000/noirr/plurx-media-runtime:reviewed-dv-dependencies \
  --metadata "$TMPDIR/dv-dependencies-metadata.json"
# Copy that exact index to the public repository using the public-copy workflow.
# Pin the public index digest after anonymous verification, never a moving tag.
scripts/media-runtime pin \
  ghcr.io/pjunod/cinema-media-runtime@sha256:<published-index-digest>
```

For the fleet's plain HTTP registry, `--insecure` is explicit; omit it for a
registry with trusted HTTPS. `pin` may inspect an explicit private mirror, but
always requires the same digest to be available anonymously from the canonical
public repository before updating source. Publish publicly first, then commit
the source update. A missing base fails at image resolution; application builds
never fall back to rebuilding it.

## Republication — 2026-11-12

Republished from `c0a234817` after #1481 changed the renderer sources without a
re-pin, which left processed Dolby Vision off on every node built from that
tree. The variant is the same as the initial base: an ordinary base, not the
audited amd64 release base. Forgejo run 7267 built amd64 on the nonvoter heavy
worker and arm64 on the release arm64 runner.

| | Digest |
|---|---|
| Index (pinned) | `sha256:b171d3de4e9dc30fb544b3ca18945b5e14624e7d2a7fa005deaded460c9d3e67` |
| linux/amd64 | `sha256:7242b24e931f887deb1136a7172cc0e8d163765d7d2702bd61b22b3b7aedb1a9` |
| linux/arm64 | `sha256:44bda5af679331e52fc699e3b3ba9fb371f30d87f25b8c31a8122768da814c9d` |

Both platform bundles report DV `source_sha256`
`0ae1fc25613537752ae7a9e0e632b82bb9269603208f1994694377c499486e50`, which is the
identity of `tools/dv_processing` at `c0a234817`. The public copy (GitHub run
43136340769) is anonymously readable with identical index bytes. The temporary
copy credential was removed afterwards. `scripts/media-runtime pin` verified
both platforms and the public index before rewriting the pin.

That republication was the immediate repair. Its guard recorded the bundle's
identity beside the pin (`PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256`) and checked the
pinned image against it; both went when the bundle returned to the application
build, below, because the runtime no longer supplies the bundle.

## Helper bundle returned to the application build — 2026-11-12

The runtime pinned on 2026-11-09 carried a bundle whose `source_sha256` was
`cfb13f96…` while main's daemon expected `0ae1fc25…`. The dependency image is
pinned to index digest
`sha256:9130f0ce818833dea3361c93b50eaa4d25ceef3f0275593a2ab57877a389a050`,
published natively for amd64 and arm64 by the maintenance workflow, which also
built the application recipe against both candidates on each platform.
`pin-dv-dependencies` read the input record out of both platform images and
found this tree's identity in each.

On a lab host, with a capped builder, the helper stage built against that
published image: its inputs matched the tree, the bundle compiled against
libplacebo 7.387 and the six active-area C controls passed. The `runtime`
target, given a daemon built from the same tree, installed the bundle, loaded
all four helpers and had `plurxd dv-helper-identity` accept it as the service
account. With one line appended to `dv_dm.h` after the daemon was built, the
image build stopped at that step with "unsupported DV build identity"; with
one byte appended to the decoder patch, the helper stage refused the
dependency image and printed the differing input. The bundle's `scan_title`
hashes the same as the one in the pinned runtime, so the title-scanner key
does not move.

Not yet done: the daemon check on an arm64 image (the workflow's qualification
uses placeholder daemon binaries), and playback served against the new bundle.
