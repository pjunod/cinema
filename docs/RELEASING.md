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
   tags `{version}`, `{major}.{minor}`, and `latest`. Pushes to `main` build but
   do not publish, so releases are always deliberate.

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

**Not yet in use.** The fleet's deploy identity is the `sha-<12hex>` image,
and at the time of writing nothing produces one: `ci.yml` runs on `v*` tags
and manual dispatch only, while its `publish_main` job requires a push to
`main` ([LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md](ci/LEDGER-TEXT-CONTRACTS-AND-RELEASE-TAGS.md)
correction 1; [RUST-TEST-EXECUTION-POLICY.md](ci/RUST-TEST-EXECUTION-POLICY.md)
§3.1(b) owns the fix). The first release after `v0.3.0` waits for that, so
no release pull request should merge before it; the schedule tags nothing
until one does.

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
