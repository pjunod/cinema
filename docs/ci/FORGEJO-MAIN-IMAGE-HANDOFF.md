# Forgejo main-image publisher handoff

Snapshot: 2026-09-04 21:24 EDT

## Outcome

Forgejo pull request [#18](http://forge.lan:3000/noirr/plurx/pulls/18)
is merged into `main`. The merge commit is:

```text
6f1494e9a8dbb2f046605953bb7d241d8b676007
```

That merge added the post-merge publisher requested for the fleet. A successful
Forgejo `main` run now builds the Linux/x86_64 image on one runner and publishes
it to the local Forgejo container registry as:

```text
forge.lan:3000/noirr/plurxd:main
forge.lan:3000/noirr/plurxd:sha-6f1494e9a8db
```

The moving `main` tag means the newest qualified merge. The `latest` tag remains
owned by versioned releases and is not changed by ordinary merges.

## State at handoff

The first post-merge workflow is [Forgejo Actions run
#103](http://forge.lan:3000/noirr/plurx/actions/runs/103). It was still
running when work stopped.

At the last observation:

- `validation scope` passed.
- `fast policy and contract preflight` passed.
- `replicated WAL recovery contracts` passed.
- Rust, legacy Store, topology, daemon, and both package/smoke jobs were still
  running.
- `publish merged image (Forgejo registry)` was correctly blocked pending that
  validation fan-out.
- `Main promotion gate` was also still blocked pending the fan-out.

The separate lint workflow for the same merge is [run
#104](http://forge.lan:3000/noirr/plurx/actions/runs/104).

No node was restarted and no node deployment configuration was changed as part
of this work. The registry image still needs to finish publishing and be
verified before any rollout.

## What changed

- `.github/workflows/ci.yml` has a `publish_main` job that runs only for a push
  to `refs/heads/main`.
- The publisher waits for the complete post-merge validation fan-out and fails
  closed if any required job fails or is cancelled.
- `scripts/registry-push` checks the exact 40-character source SHA and a clean
  tracked tree before building.
- The immutable `sha-<12hex>` image is pushed and pulled back from Forgejo, then
  checked for architecture, OCI source and revision labels, binary version,
  cluster build identity, and container runtime behavior.
- The moving `main` tag is pushed only after the immutable image passes those
  checks. A same-SHA retry reuses and re-verifies the immutable image.
- Deployment documentation and the Unraid template now use `:main`; rollback
  continues to use an immutable `sha-<12hex>` tag.

The two source commits merged by pull request #18 are:

```text
7ef28000e6d974e6122e634319a8d04a6c7ebbad  ci: publish qualified main image to Forgejo
97fc3ef5ee35aca49a5604971f08d79172d80e7a  test: catalog Forgejo main image publishing
```

## Validation already complete

Local validation passed before the pull request was opened:

```text
make check                         passed
make operations-check             165 passed
make validation-lint              passed
make history-check                 passed
bash -n scripts/registry-push     passed
workflow YAML parse               passed
git diff --check                  passed
```

The host's default Homebrew FFmpeg has a broken `libx265.216.dylib` reference.
The successful full commit gates used the repository's already-established
wrappers:

```bash
PATH=/private/tmp/plurx-test-bin:$PATH \
PLURX_FFMPEG=/private/tmp/plurx-test-bin/ffmpeg \
PLURX_FFPROBE=/private/tmp/plurx-test-bin/ffprobe \
  make check
```

Forgejo pull-request run [#97](http://forge.lan:3000/noirr/plurx/actions/runs/97)
also passed, including the single `Main promotion gate`. Its slowest lane was
the legacy replicated Store contract job at 27 minutes 51 seconds.

## Resume checklist

1. Open [run #103](http://forge.lan:3000/noirr/plurx/actions/runs/103) and
   wait for `publish merged image (Forgejo registry)` to finish.
2. If the publisher fails, inspect that job before retrying the workflow. The
   script deliberately refuses an indeterminate registry response and will not
   move `main` after a failed verification.
3. Confirm the package has both `main` and `sha-6f1494e9a8db` in the [Forgejo
   package view](http://forge.lan:3000/noirr/-/packages/container/plurxd/main).
4. From an authenticated x86_64 node, pull without restarting the service and
   verify the published identity:

   ```bash
   docker pull forge.lan:3000/noirr/plurxd:main
   docker image inspect \
     --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' \
     forge.lan:3000/noirr/plurxd:main
   docker run --rm forge.lan:3000/noirr/plurxd:main --version
   docker run --rm \
     --entrypoint /usr/local/bin/plurx-cluster-check \
     forge.lan:3000/noirr/plurxd:main build-identity
   ```

   Both revision checks should return the full merge SHA shown above. Pulling
   is read-only with respect to the running voter; do not run `docker compose
   up -d` until a rollout is explicitly requested.
5. Verify the outbound GitHub mirror has received merge
   `6f1494e9a8dbb2f046605953bb7d241d8b676007`. That mirror check had not yet
   been performed when work stopped.

## Workspace caution

The primary checkout at `~/code/plurx` was already dirty and was
not used to implement or merge this change. At handoff it was on local `main`,
behind its configured `origin/main`, with unrelated modified and untracked
files. Do not reset, clean, or pull that checkout without first preserving the
user's work.

Implementation and validation ran in the isolated worktree:

```text
/private/tmp/plurx-forgejo-main-image
```

Its local branch `codex/forgejo-main-image` remains available in that worktree;
Forgejo deleted the remote branch after merging pull request #18.
