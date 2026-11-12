# Media runtime — publish dependencies before building Cinema

Companion to [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) (how changes
reach main) and [RUNNER-DISK.md](RUNNER-DISK.md) (builder storage limits).

**Status:** built; initial publication and validation evidence are recorded in
this change. The runtime pin is the `runtime-assets` base in `Dockerfile`.

## Application builds consume the committed digest

`make docker`, `make docker-up`, package smoke and current-source release
publication reuse the immutable media image. The application Dockerfile compiles
Rust binaries and copies notices; its runtime dependency closure contains no
APT installs, FFmpeg compilation, Dolby helper compilation or subtitle engine
compilation. A cold application build downloads image layers and Rust crates;
it does not contact the Debian snapshot service.

`Dockerfile.media-runtime` owns the Debian snapshot, FFmpeg and driver packages,
static probe, VMAF scorer, DV helper bundle, subtitle recognizer and the service
account. Changing application source never rebuilds these dependencies. Changes
to those inputs take effect only after publishing and committing a new pin.

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
and arm64, publishes both platform images, assembles their index and
uploads a private-candidate publication receipt. It does not prepare or modify
an application Dockerfile. Copy that exact index to the public repository with
the manual public-copy workflow, then run `scripts/media-runtime pin` against
the public digest. The pin command obtains an anonymous GHCR token, verifies
the raw index digest and both platform manifests without Docker credentials,
and only then updates source. A private candidate alone cannot support external
builders. Review and commit the pin with its media input changes and evidence.

For an operator-managed builder, the equivalent maintenance commands are:

```bash
# Build and publish both platforms with a suitably configured native builder.
scripts/media-runtime build \
  --image forge.lan:3000/noirr/plurx-media-runtime:reviewed-update \
  --metadata "$TMPDIR/media-runtime-metadata.json"
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

## Renderer sources travel with the pin

The DV helper bundle is built from `tools/dv_processing` when the runtime is
published, and plurxd refuses a bundle built from any other renderer sources
(`DV backend unavailable: unsupported DV build identity`). Processed Dolby
Vision is then off on every node that deploys the build. That happened on
2026-11-11, when a renderer change merged without a republish.

The pin therefore records the bundle's `source_sha256` beside the digest
(`ARG PLURX_MEDIA_RUNTIME_DV_SOURCE_SHA256` in `Dockerfile`). Three checks hold
it in place:

- `tests/operations/test_media_runtime_dv_identity.py` (recomputing plurxd's
  own input list) and the plurxd unit test
  `this_daemon_accepts_the_pinned_media_runtimes_dv_helpers` fail any change
  whose `tools/dv_processing` inputs differ from the recorded identity, so a
  renderer change must arrive with a republished runtime and a new pin.
- The application build fails if the pinned image's bundle differs from the
  record.
- The publish workflow reads the identity from both platform images, requires
  them to agree, and prints it in the receipt. `scripts/media-runtime pin`
  requires it as `--dv-source-sha256`.

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
On 2026-11-11 the pin moved to `sha256:b171d3de4e9dc30fb544b3ca18945b5e14624e7d2a7fa005deaded460c9d3e67`,
rebuilt from main `c0a234817`: its DV helper bundle carries the source identity
that main's plurxd expects (the previous pin carried a Nov 7 `dv_dm.h`, so plurxd
refused it and declined processed upgrades). Building the bundle from the
application tree instead of the runtime follows separately.
Both native platform builds succeeded. Media executable version checks passed;
the amd64 helper loaders returned their expected usage errors. A cold
`runtime-assets` application build with an empty source context resolved the
published index and executed no `RUN` instructions.

Validation: 97 focused publication, maintenance, snapshot, readiness and Dolby
package tests completed with one platform-specific skip; nine affected Docker
contracts passed; paperwork passed all 15 checks. This evidence covers base
publication and consumption, without asserting a full daemon rebuild, hardware
playback qualification or audited amd64 release readiness.

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

