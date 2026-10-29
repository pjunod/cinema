# Versioning and releases

## What the numbers mean

plurx carries **one version for the whole workspace**. `plurx-core`,
`plurx-compat-plex`, and `plurxd` are not published to crates.io and are never
useful apart from each other, so a single number is what "a plurx release"
means. It lives in exactly one place:

```toml
# Cargo.toml
[workspace.package]
version = "0.3.0"
```

Every crate inherits it with `version.workspace = true`. The native apps keep
their store-facing marketing versions aligned with the workspace separately;
their monotonically increasing build counters advance whenever their release
paths change, and CI enforces that contract.

plurx follows [semantic versioning](https://semver.org/) under the 0.x rules,
which are worth stating plainly because they are not what 1.x users expect:

| Change | While `0.x` | After `1.0` |
| --- | --- | --- |
| Breaking API, config, or on-disk format change | **minor** — `0.1.0` → `0.2.0` | major |
| New feature, backwards compatible | **minor** — `0.1.0` → `0.2.0` | minor |
| Bug fix, no interface change | **patch** — `0.1.0` → `0.1.1` | patch |

So under 0.x a minor bump means "something may have moved" and a patch bump
means "nothing moved." That is the promise a self-hosted user actually needs:
it tells them whether `docker pull` is safe to do unattended.

The compatibility surface this covers is the HTTP API under `/api/v1`, the Plex
façade, the config file schema, and the SQLite schema. The web app ships inside
the binary and is versioned with it.

**1.0 is not a maturity badge, it's a promise.** It happens when the API and
on-disk format are stable enough to commit to not breaking them — realistically
once the HA cluster work (Phase 4) is wired up and the schema has settled.

## The build stamp

A version number cannot distinguish a tagged release from the forty commits
after it, which is exactly the situation most bug reports come from. So the
binary carries two strings:

- `version` — bare semver, e.g. `0.1.0`. Parseable; clients compare it.
- `build` — `git describe --tags --always --dirty`, e.g. `v0.1.0-14-gc0ffee`
  or `v0.1.0-14-gc0ffee-dirty`. Identifies the exact commit.

Both appear in `plurxd --version`, in the startup log, in `GET /api/v1/server`
and `GET /api/v1/system`, on the `plurx_build_info` metric, and in Settings →
Server (the build stamp is hidden there when it says nothing the version
doesn't).

`crates/plurxd/build.rs` produces the stamp and is not allowed to fail a build.
Without a git checkout — a source tarball, or the Docker context, which excludes
`.git` — it falls back to `unknown` unless `PLURX_BUILD_REF` is set:

```sh
docker build --build-arg PLURX_BUILD_REF="$(git describe --tags --always --dirty)" -t plurx/plurxd .
```

CI passes the tag name automatically when it publishes an image.

## Cutting a release

1. **Decide the number** using the table above.

2. **Run `scripts/release-cut`** (`--part minor` for a minor bump, or
   `--version X.Y.Z`). It refuses an empty `Unreleased` section, an existing
   tag and an existing changelog section before it writes anything; then it
   moves `CHANGELOG.md`'s `Unreleased` section under a dated
   `## [X.Y.Z] — YYYY-MM-DD` heading, rewrites the two link definitions at the
   bottom, sets the workspace version in `Cargo.toml`, sets both native apps'
   marketing versions to it and advances both native build counters (the
   mobile-version contract requires all of that of a version bump), rewrites
   the status lines that quote the version or a build counter
   (`clients/apple/README.md`, `clients/android/README.md`,
   `docs/clients/APPLE-CLIENT-PARITY.md`, `docs/STATUS.html`, the Apple ones
   through `validation/apple_build.py`'s generator) so the tree it leaves
   passes `make operations-check`, and lets Cargo relock `Cargo.lock` and
   `spikes/hiqlite-m0/Cargo.lock`. It never tags.

   Review the exact-file fingerprints before pushing. Apple project version
   and build changes need a reviewed refresh of `BUILD_SPECS` in
   `validation/native_receipts.py`. Cargo manifest and lockfile changes also
   need reviewed fingerprints in the durable-jobs and operation-progress
   formal mappings, even when only workspace versions changed. The release
   script leaves these independent review records alone. Preserve their
   exact-byte checks and record which source changes were reviewed. Run
   `python3 -m unittest tests.validation.test_native_receipts` and
   `python3 scripts/check-formal check-mapping --package <package>` for
   `durable-jobs`, `operation-progress`, and `worker-refinement` before pushing.

3. **Read the changelog it dated.** Entries are for people running the server,
   not for people reading the diff; edit them in the release pull request.

4. **Run the gates.** `scripts/validate run --profile ci --all --strict` is the
   exact all-points CI contract: catalog, fmt, clippy, tests, embedded
   JavaScript, theme contrast, and Android unit/lint. Then run
   `make validate-full` for browser and container checks available on this
   machine; every unavailable check is recorded as a skip rather than disguised
   as a pass.

5. **Merge the release pull request; the weekly run tags its landing commit.**
   Open a pull request for the release commit (`release: vX.Y.Z`), wait for its
   required checks, and merge it. The commit that lands it on `main` is the
   **release commit**: the first commit on `main`'s first-parent line whose
   `CHANGELOG.md` carries the dated `## [X.Y.Z]` heading. The next scheduled
   release-readiness run (below) checks that commit out, runs
   `make release-check` there, and pushes the annotated tag onto it only if it
   is green, however many pull requests have merged on top of it since.
   Tagging by hand remains possible and is the same act the workflow performs:

   ```sh
   make release-check
   git tag -a v0.3.1 -m "v0.3.1"
   git push origin v0.3.1
   ```

   The tag is `v` + the version. CI refuses to publish a tag that disagrees
   with `Cargo.toml`, so a mismatch fails before anything reaches a registry
   rather than after.

6. **CI does the rest.** A `v*` tag runs the full gate, then calls the same
   publication workflow used for recovery. That workflow peels the annotated
   tag once, builds x86-64 and aarch64 binaries inside the pinned Bookworm
   toolchain, and stamps both with the validated tag. It packages and smoke
   tests each platform by digest before assigning the local Forgejo registry
   tags `{version}`, `{major}.{minor}`, and `latest`. A merge to `main` does
   not build or publish; the separate `sha-` fleet image requires a manual
   full-CI dispatch on `main`, so releases remain deliberate.

### The weekly release tag

Releases are cut **weekly, from a green scheduled run** — the cadence Paul
chose on 2026-09-23 ([LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
§3.5 option (a)). `.github/workflows/release-readiness.yml` runs every Monday
at 06:00 UTC and starts from `main`'s tip:

- `scripts/release-cut --pending` asks whether a release is waiting: the
  tip's workspace version is dated in `CHANGELOG.md` and `v<version>` is not
  yet a tag. That is true exactly when a release pull request has merged since
  the last tag. It then names the **release commit** (step 5), never the tip:
  every commit merged after the release still carries the same version and
  dated section, but none of them is the release. It refuses, and the run
  fails, when that commit is not one the changelog describes: it does not
  declare the version, it still has entries under `[Unreleased]`, or `main`
  has since edited the release's section.
- If a release is waiting, the run checks the release commit out and the gate
  there is `make release-check`; if not, the gate on the tip is the same
  `scripts/validate run --profile ci --all --strict` sweep, so every week
  answers "is `main` releasable" whether or not anything is waiting.
- Only when that gate is green, on a scheduled run, for a pending release,
  does the `tag` job push the annotated tag, onto the release commit the gate
  passed. The tag starts `ci.yml`'s full sweep and the image publication in
  step 6.

The push uses the `RELEASE_TAG_TOKEN` repository secret, scoped to that one
step: a tag pushed with a run's own token starts no workflow, and this tag has
to start one. Without the secret a pending release's run fails at that step
and says so; nothing else changes. A manual dispatch runs the gate and never
tags.

A week with nothing to release is a green run and no tag. Preparing the week's
release is landing one `scripts/release-cut` pull request before Monday.

**First-tag prerequisite.** The fleet's deploy identity is the
`sha-<12hex>` image. An operator may dispatch `ci.yml` on `main`; only a
green full run reaches `publish_main` and moves that image's `main` alias.
Neither a merge nor the Monday release-readiness schedule publishes it.
Before merging the first release pull request after `v0.3.0`, verify the
qualified `sha-` image exists and arrange the `RELEASE_TAG_TOKEN` repository
secret. The weekly tag job fails closed without that secret; this source
change supplies neither the secret nor a first tag. See
[LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
correction 1 for the trigger decision.

### GitHub release downloads

The public GitHub repository is a push mirror of Forgejo, so the weekly tag
reaches it when the mirror next syncs, and there it starts
`.github/workflows/github-release.yml` on GitHub-hosted runners (free for a
public repository; nothing runs on the lab fleet). From the tagged commit it
builds and publishes the GitHub release for that tag:

| Download | For | Checked before publishing |
|---|---|---|
| `plurxd_X.Y.Z-1_amd64.deb`, `_arm64.deb` | Debian 12+, Ubuntu 24.04+ | installed, started as `plurx` to `/readyz`, force-reinstalled while installed, then removed on Debian 12 and Ubuntu 24.04 |
| `plurxd-X.Y.Z-1.x86_64.rpm`, `.aarch64.rpm` | Fedora, RHEL 9+ (ffmpeg from RPM Fusion) | the same on Fedora 42; scriptlets and binary on AlmaLinux 9 |
| `plurxd-X.Y.Z-macos-universal.tar.gz` | macOS, Apple Silicon and Intel | each slice reports its version on its own hardware; complete closure, Developer ID, notarization, stapled ticket and Gatekeeper checks |
| `plurxd-X.Y.Z-windows-x86_64.zip` | Windows x64 | `scripts/package-windows`; `plurxd.exe --version` on Windows Server 2022 |
| `SHA256SUMS` | all of the above | computed over exactly the files published |

The Linux binary is built in AlmaLinux 8, so it needs only glibc 2.28; the
distribution list is set by the packages' ffmpeg dependency instead: 5.1 is
the floor plurx paces streams on (`-readrate`; 6.1 adds the initial burst, see
[OPERATIONS.md](OPERATIONS.md)), which Debian 12 and Ubuntu 24.04 ship and
Ubuntu 22.04 does not. Both packages come from one nfpm description,
[`deploy/packaging/nfpm.yaml`](../deploy/packaging/nfpm.yaml), assembled by
`scripts/package-linux`: the binary and its `.dwp` in `/usr/lib/plurx`, a
`/usr/bin/plurxd` symlink, and `deploy/plurxd.service` with only `ExecStart`
repointed at `/usr/bin`. Installing creates the `plurx` user and
`/var/lib/plurx` (a directory the package does not own, so no upgrade resets
it and no removal deletes it), then enables and starts the service; an upgrade
restarts it only if it is running; removing stops and disables it and keeps
the state. With a retained managed installation record, installing again after
removal requires restoring the trusted package guard first; preinstall refuses
to unpack a new payload when that guard is absent. The workflow tests forced
reinstallation while the guard remains installed, followed by removal. The macOS archive is the
universal runtime, desktop app and controllers beside `deploy/install`. Its
architecture-neutral WASM source parser is built once and shared unchanged by
both native media slices; independent host SDK builds can emit slightly
different floating-point constants, and universal assembly requires identical
parser bytes and ABI. The complete parser provenance travels with that shared
artifact. Thus
`./install` in the unpacked folder is the whole install. Actual startup, login,
reboot and rollback behavior remain separate native qualification evidence.

The build jobs hold a read-only token and pass their output on as
workflow artifacts retained for seven days. Only the last job can write, and
it compiles nothing: it checks the exact twelve native assets (plus the
manifest/signature pair only when in-app upgrades are qualified), computes
`SHA256SUMS`, and
creates the release in one `gh release create`, which publishes only once
every file is attached. A release that is already published is never rebuilt
over; a draft an interrupted publish left behind is replaced. The tag is
validated as `publish-release.yml` validates it: annotated `vX.Y.Z`, equal to
the workspace version, with a `## [X.Y.Z]` heading in `CHANGELOG.md`.

Two manual entry points, both **Run workflow** on GitHub's Actions tab: with
`release_tag` set it back-fills an existing tag (built from the tag, packaged
by the current workflow, marked *latest* only if it is the highest tag); with
it empty it is a dry run of the dispatched ref, which builds and checks
everything and publishes nothing. GitHub starts no workflow for a push that
carries more than three tags, so a mirror catching up on several releases at
once needs the back-fill dispatch for each.

Forgejo reads `.github/workflows` too. Every job starts with the positive
`github.server_url == 'https://github.com'` condition, with explicit mode and
status routing where needed. Forgejo evaluates that gate on the server and
records these jobs as skipped without allocating a runner. The contract
`test_github_only_workflows_never_reach_the_fleet` holds the file to that
exact gate, to GitHub-hosted runners, to local actions plus GitHub's two
pinned artifact actions, to tag-push and dispatch triggers only, and to one
writing job that builds nothing. On GitHub, the fleet workflows are disabled,
Actions may run only local actions and those two, outside contributors' runs
need approval, and the default token is read-only.

### The release pull request is the gate the cut depends on

Two `v0.3.0` failures shared a lesson: some gates only fire in regimes the
cutting machine may not reproduce. The spike workspace's own `Cargo.lock`
pin needs a toolchain to check at all (`make spike-lock-check` could not run
on the toolchain-less host that cut the release; CI caught it). The
exact-count cluster windows only failed once a loaded runner slowed them
past a background heartbeat interval — a speed regime, not a cache one, and
one a fast development machine never enters (root-caused and fixed after
that cut). Local green is therefore evidence, not proof.

What makes the cut safe is step 5, followed exactly: the tag goes on the
merged release commit only after its required checks pass, so the release
pull request's CI run — real runners, real load, full toolchain — is the
run the tag actually depends on. A red required check there stops the cut.
The one exception is the standing infrastructure rule, stated here for the
release path: if the tests ran and passed and the job then died on a
Forgejo infrastructure fault — artifact upload, storage pressure, runner
capacity —
note it in the release pull request and proceed. A `FAILED` or `panicked`
test line is never that exception; on the release path it is a gate doing
its job.

One honest limit: CI's dependency caches restore across version bumps, so
no step of this procedure produces a provably cold compile. Nothing above
relies on one — but if a cold build per cycle is ever wanted as policy, it
has to be arranged deliberately (a cache-disabled job on the release pull
request), not assumed.

### Recovering a cancelled publication without moving the tag

A cancelled image build does not justify retagging a different commit. Run the
manual workflow from current `main` and pass the existing annotated tag; the
workflow resolves source and runtime files from that immutable tag rather than
from the branch that supplied the repaired workflow.

Open Forgejo → `noirr/plurx` → Actions → `publish release image`, choose
`main`, enter the existing tag in `release_tag`, and run it. Require that exact
run to finish green before checking the registry aliases:

```bash
image=forge.lan:3000/noirr/plurxd
version_digest=$(docker buildx imagetools inspect "$image:0.3.1" \
  | sed -n 's/^Digest:[[:space:]]*//p' | head -1)
for alias in 0.3.1 0.3 latest; do
  test "$(docker buildx imagetools inspect "$image:$alias" \
    | sed -n 's/^Digest:[[:space:]]*//p' | head -1)" = "$version_digest"
  test "$(docker buildx imagetools inspect --raw "$image:$alias" \
    | jq -r '.manifests[].platform | "\(.os)/\(.architecture)"' | sort)" \
    = "$(printf 'linux/amd64\nlinux/arm64')"
done
```

The workflow refuses lightweight tags, version or changelog mismatches, and a
remote tag that moves after source resolution. It pushes architecture images
without human-facing tags, smoke tests them, then creates the immutable version
index and its moving aliases. If the version index already exists, both of its
architecture bindings must pass the same source-label, version, and container
smoke checks before the workflow reuses it. `0.3` and `latest` move only when
the recovered version is not older than their verified current target, so a
late recovery cannot roll clients backward.

The successful workflow is the acceptance record: both platform jobs must
report `plurxd 0.3.1 (v0.3.1)`, both image configs must name the tag's peeled
source commit, and container smoke must pass on both. For a first publication,
`0.3.1`, `0.3`, and `latest` must resolve to the same two-platform index. For a
recovery after a newer release, the immutable `0.3.1` index must pass while the
newer moving aliases remain unchanged. Never delete or move the release tag to
make recovery pass; a mismatch is an incident to investigate, not an alias to
overwrite.

The weekly release-readiness run is described above. A red run means the
gate failed on `main`'s tip or, for a pending release, on the release commit
(or that `--pending` refused the release commit, or that the tag push could
not happen); it is never a reason to move or overwrite an existing tag.

## Checking what a build reports

```sh
make version          # what a build from this tree would stamp
plurxd --version      # what an existing binary reports
curl -s localhost:32400/api/v1/server | jq '{version, build}'
```

From a node's `build` to the changelog entries it runs: a `build` of
`v0.3.1` is exactly the `## [0.3.1]` section of `CHANGELOG.md`; a `build` of
`v0.3.1-12-gabc1234` is that section **plus** whatever of `[Unreleased]` was
merged by commit `abc1234` (`git log v0.3.1..abc1234`). Across the fleet the
same pair is the `plurx_build_info{version,build}` metric, and
[OPERATIONS.md](OPERATIONS.md) "The fleet registry" says which image a node
runs.

## Desktop update signing — the initial trust bootstrap

Native controllers embed the public keys in
[`production-trust-roots.json`](../crates/plurx-desktop-control/production-trust-roots.json).
Only public Ed25519 keys belong in the repository. The private production key
lives in protected operator storage and the `desktop-release-signing` GitHub
environment secret `PLURX_UPDATE_ED25519_PRIVATE_KEY`; build jobs never receive
it. Keep a protected operator backup before retiring the provisioning machine.

**First installation:** controllers built before a production root was pinned
cannot authenticate an update that introduces that root. Manually reinstall a
trusted source build or complete native release once. That rebuild must include
the reviewed public root. Changing GitHub settings cannot alter a key list
already compiled into an installed controller. The initial Linux Compose tray
download uses GitHub HTTPS plus release checksums, independently of this signed
native upgrade path.

**GitHub setup:** require an operator reviewer on `desktop-release-signing`,
restrict deployment branches/tags to `main` and `v*`, and upload the private PEM
with `gh secret set PLURX_UPDATE_ED25519_PRIVATE_KEY --env desktop-release-signing
--repo pjunod/cinema < /protected/path/key.pem`. Keep private bytes out of command
arguments, logs, pull requests and build artifacts. Set the trusted packaging
commit in the protected signing environment. Set the five `PLURX_UPDATE_*`
qualification values as **repository variables** only for a reviewed, qualified
release; the resolver reads them before allocating a protected signing job:

| Variable | Required value |
|---|---|
| `PLURX_TRUSTED_PACKAGING_COMMIT` | Exact reviewed workflow commit used for this tag/dispatch; refresh it after review for every release, including version-only changes. |
| `PLURX_UPDATE_UPGRADE_FROM` | Qualified source-version requirement, such as `=0.7.0`; do not infer compatibility from the new version number. |
| `PLURX_UPDATE_DATA_COMPATIBILITY` | Qualified data-format compatibility identity. |
| `PLURX_UPDATE_CLUSTER_COMPATIBILITY` | Qualified cluster compatibility identity. |
| `PLURX_UPDATE_MINIMUM_CONTROLLER` | Lowest controller version tested with this release. |
| `PLURX_UPDATE_ROLLBACK` | `binary_compatible` after rollback qualification, otherwise `manual_recovery`. |

**Installer publication:** verified native packages/archives and `SHA256SUMS`
publish independently of in-app upgrade qualification. If any of the five
`PLURX_UPDATE_*` qualification variables is absent, the release omits update
metadata and explicitly advertises manual installation only. It does not enter
the protected update-signing environment or publish an unsigned/fixture feed.
Apple distribution verification remains mandatory for macOS.

**Authenticated update publication:** when all five qualification variables are
configured, the GitHub release workflow checks that the dispatch commit
matches `PLURX_TRUSTED_PACKAGING_COMMIT`, that candidate and approved packaging
roots agree, and that the private key matches a pinned public key. It signs the
exact manifest bytes only after checking the native artifact cohort. Such a
release also includes `plurx-update-v1.json` and its detached `.sig`, plus the native
packages/archives and `SHA256SUMS`. Once update publication is selected, missing
signing configuration or invalid qualification remains a refusal; never
substitute fixture keys or bypass authentication. Partial qualification never
advertises an update manifest, even when an Ed25519 key is already configured.

**Reading the result:** a populated public-root document and configured secret
establish signing capability; they do not mean a signed release is published.
Check that the released manifest/signature exist and verify against a pinned
root before declaring in-app updates available. For rotation, ship old and new
public roots together in an old-key-signed release before retiring the old key.

**macOS publication has a separate Apple requirement:** the Ed25519 update key
is not a code-signing certificate. An existing paid Apple Developer Program
membership can create a Developer ID Application certificate for direct macOS
distribution, but an iOS signing certificate cannot substitute for it.
The release workflow assembles an ad-hoc candidate without Apple credentials,
then signs and notarizes it in the separate `macos_signing` job. That job never
executes the candidate. It signs every loadable Mach-O image with a secure
timestamp and Hardened Runtime, keeps the daemon's source-parser entitlement,
submits the complete package to Apple, staples the accepted app ticket, and
checks the existing Developer ID/team/notarization/Gatekeeper policy. The final
closure and component hashes describe the signed, stapled bytes; the original
build manifests remain in `provenance/apple-signing.json`.

Create the `macos-release-signing` GitHub environment with required reviewers
and trusted branch/tag restrictions. Add these environment secrets (private
bytes belong in GitHub's secret UI or files passed to `gh secret set`, never
shell arguments or repository files):

| Secret | Value |
|---|---|
| `PLURX_MACOS_CERTIFICATE_P12_BASE64` | Base64 of the exported Developer ID Application certificate **and private key** in a password-protected `.p12`. |
| `PLURX_MACOS_CERTIFICATE_PASSWORD` | Password for that `.p12`. |
| `PLURX_MACOS_NOTARY_PRIVATE_KEY` | App Store Connect team API private key, including its PEM delimiters. |

Add these environment variables:

| Variable | Value |
|---|---|
| `PLURX_MACOS_SIGNING_IDENTITY` | Exact `Developer ID Application: … (TEAMID)` certificate name. |
| `PLURX_MACOS_DEVELOPER_TEAM_ID` | Ten-character Apple team ID matching the certificate. |
| `PLURX_MACOS_NOTARY_KEY_ID` | Ten-character App Store Connect team API key ID. |
| `PLURX_MACOS_NOTARY_ISSUER_ID` | API key issuer UUID. |
| `PLURX_TRUSTED_PACKAGING_COMMIT` | Exact reviewed workflow commit matching this tag/dispatch; refresh it for every release after review. Desktop metadata signing has its own pin. |

The signing job imports the certificate into a temporary keychain and removes
it and the notarization key on success, failure or cancellation. Missing
configuration, rejected notarization, or failed distribution verification
prevents the macOS upload and publication. The complete native platform cohort
is always required. The separate desktop update-signing environment and
qualified compatibility variables above are required only to advertise
authenticated in-app upgrades; they do not gate manual installer downloads.

A dry run needs neither Apple credentials nor environment approval. It uploads
an ad-hoc `release-macos` workflow artifact without publishing a GitHub release.
A tag publication uploads only the verified archive under that artifact name;
the unsigned intermediate is `macos-candidate`. Dispatch the reviewed workflow
with an existing `release_tag` to publish that tag, or push a new annotated
release tag. The artifact is `plurxd-X.Y.Z-macos-universal.tar.gz`; unpack it and
run `./install`. Existing published releases are never overwritten.

Follow [Apple's Developer ID setup](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/)
and [notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
for certificate and team API key provisioning.
