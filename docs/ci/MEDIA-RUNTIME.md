# Media runtime — publish dependencies before building Cinema

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how changes
reach main) and [RUNNER-DISK.md](RUNNER-DISK.md) (builder storage limits).

**Status:** built; the runtime pin is the `runtime-assets` base in
`Dockerfile`. The DV helper bundle moved back to the application build on
2026-11-11; its dependency image **is not published yet**, so the
`dv-processing-dependencies` pin is the all-zero placeholder and
`tests/operations/test_dv_helper_ownership.py` fails on it by design until the
[publication below](#publish-a-dependency-update-explicitly) is pinned.

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
patch, the Python build requirements and the three source-pin files under
`tools/dv_quality/backends`. `Dockerfile` records the same identity beside the
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

## Qualification and rollback stay explicit

The `media-runtime` maintenance target preserves the incumbent ordinary runtime.
Pinned amd64 release publication requires the audited Dolby capsule, executable
identity and complete shipping system-library checksums. The initial ordinary
base preserves local-build behavior and does not satisfy that release contract.
Publish and promote an audited base before attempting a release.

The separate `runtime-assets-dolby-amd64` target still accepts an audited Dolby
package through the named `linux-dolby-package` build context. Building the base
alone does not establish hardware playback or audited-package qualification.
For that target, publish amd64 separately, compose it with the reviewed arm64
image using `docker manifest create --insecure` followed by `docker manifest push --insecure`, then pin the combined index.
The retired `build-linux-dolby-ffmpeg --step docker-build` route prints this
migration instead of rebuilding dependencies during an application build.

Historical release tags with inline runtime recipes retain their original
packaging path. New tags consume their committed base and continue recording the
selected runtime digest and Debian package inventory. The CI tooling target can
install compiler and browser test tools; it is not in the application runtime
closure.

Rollback restores the previous committed digest and rebuilds the application.
Retain previous runtime manifests and layers for as long as those application
revisions remain supported. Run focused packaging contracts and platform runtime
smoke checks before promoting a dependency update.

## Initial publication — 2026-11-09

The first ordinary base is pinned to index digest
`sha256:17b3bf9f0d3ed5dd167540a3c805e30a7624fbd93dad250e9e866799ecb22cae`.
Both native platform builds succeeded. Media executable version checks passed;
the amd64 helper loaders returned their expected usage errors. A cold
`runtime-assets` application build with an empty source context resolved the
published index and executed no `RUN` instructions.

Validation: 97 focused publication, maintenance, snapshot, readiness and Dolby
package tests completed with one platform-specific skip; nine affected Docker
contracts passed; paperwork passed all 15 checks. This evidence covers base
publication and consumption, without asserting a full daemon rebuild, hardware
playback qualification or audited amd64 release readiness.

## Helper bundle returned to the application build — 2026-11-11

The pinned runtime `sha256:17b3bf9f…` carries a bundle whose
`source_sha256` is `cfb13f96…`; the tree at `c0a234817` hashes to `0ae1fc25…`.
On a lab host, with a capped builder, the `dv-processing-dependencies` target
built for amd64 (910 MB compressed) and the application `runtime` target built
against it and the pinned public runtime, with placeholder daemon binaries. The
helper stage compiled and verified the bundle in three seconds; all four
helpers loaded and reported usage; the image's `build-identity.json` named
`0ae1fc25…`, equal to the daemon's identity computed from the same tree (in
Python, by the daemon's algorithm), and each installed tool hashed to its
manifest entry. With one byte appended to the decoder patch the helper stage
refused the dependency image and printed the differing input.

Not yet done: publication of the dependency image, an arm64 build, a run of the
changed maintenance workflow, and a daemon started against the new bundle.
