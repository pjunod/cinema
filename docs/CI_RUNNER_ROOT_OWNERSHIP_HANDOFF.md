# CI runner root-ownership recovery — stop container jobs poisoning checkout

**Status:** ready for runner repair · **Incident:** PR #634 CI checkout failures
· **Written:** 2026-08-28

Companion to [VALIDATION.md](VALIDATION.md) (how CI is selected) and
[OPERATIONS.md](OPERATIONS.md) (project operations) — this handoff is for a
separate infrastructure session working in
`/Users/pjunod/code/ansible/github-runners`. Read §2 before changing the
runner. Work through §4 in order. If the affected runner is busy, or the path
does not resolve exactly to the checkout named below, stop rather than cleaning
a broader directory.

## 1. Objective

Restore `gha-nuc1-general-01` so `actions/checkout@v4` can clean the Plurx
workspace, then prevent the root-running FFmpeg 8 nightly container from
leaving files that the `runner` service account cannot remove.

The immediate repair and the permanent workflow fix are separate:

1. Quiesce the exact runner, prove the ownership problem, restore ownership of
   only the Plurx checkout, restart the runner, and rerun the failed jobs.
2. Change the containerized nightly lane or its cleanup contract so the next
   scheduled run cannot poison the persistent workspace again.

## 2. Evidence — three checkouts failed after one root container job

All timestamps are UTC on 2026-08-28.

| Time | Run / job | Runner | Observable result |
|---|---|---|---|
| 15:28:28–15:33:42 | [nightly `pacing capability contract on ffmpeg 8`](https://github.com/pjunod/plurx/actions/runs/33185304765/job/98896646943) | `gha-nuc1-general-01` | The `ubuntu:26.04` job container ran as effective UID root, then ran `cargo test` and wrote `target/debug/...`. Its test failed for an unrelated FFmpeg assertion. |
| 15:46:45–15:46:50 | [RustSec workspace scan](https://github.com/pjunod/plurx/actions/runs/33186800247/job/98901777207) | `gha-nuc1-general-01` | `actions/checkout@v4` failed before RustSec ran. |
| 15:49:39–15:49:45 | [RustSec workspace scan retry on the updated PR head](https://github.com/pjunod/plurx/actions/runs/33187028314/job/98902572218) | `gha-nuc1-general-01` | The same checkout failure reproduced. |
| 15:50:08–15:50:15 | [Docker runtime smoke](https://github.com/pjunod/plurx/actions/runs/33187028546/job/98902703838) | `gha-nuc1-general-01` | Checkout failed before Buildx, image build, or container smoke ran. |

The exact checkout error in all affected logs is:

```text
File was unable to be removed Error: EACCES: permission denied, unlink
'/opt/actions-runner/_work/plurx/plurx/target/.rustc_info.json'
```

The nightly container log separately records:

```text
error: $HOME differs from euid-obtained home directory: you may be using sudo
error: $HOME directory: /github/home
error: euid-obtained home directory: /root
Run cargo test --locked -p plurxd ...
Running unittests src/main.rs (target/debug/deps/plurxd-...)
```

This is not a RustSec finding or a Docker image failure. Both jobs stopped in
checkout; no branch command ran. PR #634's corrected policy preflight passed,
and its local Rust, web, operations, WAL, and multi-process cluster gates were
green before this handoff.

### The causal chain is narrow

```text
`validation-nightly.yml` selects the persistent self-hosted runner
                              │
                              ▼
job-level `container: ubuntu:26.04` runs commands as root
                              │
                              ▼
Cargo writes `$GITHUB_WORKSPACE/target/.rustc_info.json`
                              │
                              ▼
job exits without restoring the persistent workspace owner
                              │
                              ▼
later `runner`-owned checkout cannot unlink the root-created file
```

**Confirmed:** all four jobs named above and their runner assignments; the
container's effective UID was root; Cargo wrote under the checkout's `target/`;
three later jobs failed on the same unlink before their own commands.

**Re-verify before mutation:** `stat` the file inside the guest. The current
owner being `root:root` is a high-confidence diagnosis, not a substitute for
that check. An earlier incident recorded in
[PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md](PLAYBACK-CONTROL-IMPLEMENTATION-HANDOFF.md)
also required three attempts after the same stale root-owned
`target/.rustc_info.json`, so this is a recurring runner/workflow boundary.

## 3. Runner contract — the exact guest, account, and checkout

The authoritative infrastructure source is
`/Users/pjunod/code/ansible/github-runners`.

| Fact | Current value | Source to re-check |
|---|---|---|
| Incus project | `github-runners` | `inventory/group_vars/all.yml` |
| Guest / GitHub runner | `gha-nuc1-general-01` | `github_runner_instances` in `inventory/group_vars/all.yml` |
| Incus cluster target | `nuc1` | the same inventory row |
| Runner OS / labels | Linux X64 · `lab` · `incus` · `ubuntu-24.04` · `general` · `high-cpu` · `ffmpeg-6` | GitHub runner API and inventory |
| Service account | `runner` | `roles/github_runner_registration/tasks/instance.yml` |
| Runner directory | `/opt/actions-runner` | the same role |
| Work root | `/opt/actions-runner/_work` | registration argument `--work _work` |
| Poisoned checkout | `/opt/actions-runner/_work/plurx/plurx` | checkout error |

At 15:56 UTC the GitHub runner API reported runner ID `21` online and idle.
That observation expires immediately; query it again before stopping anything.

## 4. Immediate recovery — quiesce, inspect, repair, retry

Run these from a host with the Ansible repository's Incus cluster access.
They intentionally avoid deleting the workspace. Ownership repair lets the
normal checkout clean exactly what it owns.

### 4.1 Prove GitHub has no job on the runner

```bash
cd /Users/pjunod/code/ansible/github-runners

gh api repos/pjunod/plurx/actions/runners \
  --jq '.runners[] | select(.name == "gha-nuc1-general-01") |
    {id,name,status,busy,labels:[.labels[].name]}'
```

Proceed only with `status: "online"` and `busy: false`. If it is busy, wait
for the named job or cancel that job deliberately in GitHub; stopping a runner
under an active job creates a second ambiguous failure.

### 4.2 Resolve the service and inspect ownership before changing it

```bash
incus exec gha-nuc1-general-01 --project github-runners -- \
  find /etc/systemd/system -maxdepth 1 \
    -name 'actions.runner.*.gha-nuc1-general-01.service' -print

incus exec gha-nuc1-general-01 --project github-runners -- \
  stat -c '%U:%G %u:%g %a %n' \
    /opt/actions-runner/_work/plurx/plurx/target/.rustc_info.json

incus exec gha-nuc1-general-01 --project github-runners -- \
  find /opt/actions-runner/_work/plurx/plurx -xdev \
    ! -user runner -printf '%u:%g %m %p\n'
```

The service query must return exactly one unit. Save its basename as
`<runner-unit>` for the commands below. The ownership inventory should explain
the checkout failure; preserve its output in the repair session.

If `stat` says the file is already `runner:runner`, stop and inspect directory
execute permissions, ACLs, immutable attributes, and the checkout log instead:

```bash
incus exec gha-nuc1-general-01 --project github-runners -- \
  namei -l /opt/actions-runner/_work/plurx/plurx/target/.rustc_info.json

incus exec gha-nuc1-general-01 --project github-runners -- \
  lsattr /opt/actions-runner/_work/plurx/plurx/target/.rustc_info.json
```

### 4.3 Stop only this runner and restore only this checkout

```bash
incus exec gha-nuc1-general-01 --project github-runners -- \
  systemctl stop <runner-unit>

incus exec gha-nuc1-general-01 --project github-runners -- \
  systemctl is-active <runner-unit>

incus exec gha-nuc1-general-01 --project github-runners -- \
  chown -R runner:runner /opt/actions-runner/_work/plurx/plurx

incus exec gha-nuc1-general-01 --project github-runners -- \
  find /opt/actions-runner/_work/plurx/plurx -xdev \
    ! -user runner -print -quit
```

`systemctl is-active` must report `inactive` before `chown`. The final `find`
must print nothing. Do not widen the target to `/opt`, `/opt/actions-runner`,
or `_work`: the guest can host multiple repositories, and broad ownership
rewrites obscure which job created the defect.

Restart and re-check the API:

```bash
incus exec gha-nuc1-general-01 --project github-runners -- \
  systemctl start <runner-unit>

incus exec gha-nuc1-general-01 --project github-runners -- \
  systemctl is-active <runner-unit>

gh api repos/pjunod/plurx/actions/runners \
  --jq '.runners[] | select(.name == "gha-nuc1-general-01") |
    {id,name,status,busy}'
```

Healthy means systemd `active`, GitHub `online`, and no new non-`runner` file
in the exact Plurx checkout.

### 4.4 Rerun the failed checks only after their parent runs finish

```bash
gh run rerun 33187028314 --repo pjunod/plurx --failed
gh run rerun 33187028546 --repo pjunod/plurx --failed

gh run watch 33187028314 --repo pjunod/plurx --exit-status
gh run watch 33187028546 --repo pjunod/plurx --exit-status
```

If GitHub says a run cannot be rerun, first confirm it is no longer
`in_progress`. Do not use a no-op commit to hide whether the exact failed
checks recovered.

## 5. Permanent correction — remove root writes from persistent workspaces

The source defect is in
[`.github/workflows/validation-nightly.yml`](../.github/workflows/validation-nightly.yml):
`ffmpeg8-pacing` combines the self-hosted `general` pool with a job-level
`container: ubuntu:26.04`. The container is the FFmpeg 8 pin, but it also makes
the job's checkout and Cargo commands root inside a persistent host workspace.

### 5.1 Recommended first fix: host this one root-container lane disposably

Change only `ffmpeg8-pacing` to an unconditional GitHub-hosted runner:

```yaml
ffmpeg8-pacing:
  name: pacing capability contract on ffmpeg 8
  runs-on: ubuntu-24.04
  container: ubuntu:26.04
```

The `ubuntu:26.04` container remains the tested FFmpeg/toolchain environment;
the outer hosted VM is disposable plumbing. This costs roughly one short
hosted job per day and removes persistent ownership as a failure mode without
inventing a privileged cleanup helper.

Update `tests/operations/test_contracts.py` in the same change so its runner
trust-boundary test explicitly permits this one reviewed hosted container
lane. Add a general assertion that any future job combining `container:` with
a self-hosted selector must declare an ownership-safe execution or cleanup
contract.

### 5.2 If the lane must stay self-hosted, prove cleanup in a disposable guest

An always-run final step can restore the checkout owner from a trusted
runner-owned parent, but do not ship an untested recursive `chown`. First prove
inside a disposable Incus runner that:

- the parent used as the ownership reference is owned by `runner:runner`;
- `if: always()` runs after the assertion fails, as it did in this incident;
- the cleanup handles files and directories but does not cross filesystems;
- a second checkout on the same guest succeeds; and
- cancellation and timeout do not bypass the repair.

If cancellation can bypass the workflow step, add a runner-side completed-job
hook or a narrowly scoped root service in the Ansible repository. The hook
should quarantine the runner when `/opt/actions-runner/_work/*/*` contains a
non-`runner` owner; it must never silently sweep or chown while another runner
service is active.

### 5.3 Add infrastructure detection even after the workflow fix

Extend `/Users/pjunod/code/ansible/github-runners` with a read-only health
check for non-`runner` ownership below each registered runner's `_work` tree.
Fail or quarantine the affected runner with the exact path in the message.
Detection matters because a future Docker, Android, or custom container lane
could recreate the same class under a different filename.

## 6. Guardrails — what this repair must not do

- Do not disable `actions/checkout` cleaning. It exposed poisoned persistent
  state; suppressing it would make builds depend on leftovers.
- Do not mark RustSec or Docker smoke `continue-on-error`. Neither job reached
  its security or runtime assertion.
- Do not delete or recursively chown `/opt`, `/opt/actions-runner`, or the full
  `_work` tree. Multiple runner/repository workspaces can live in the guest.
- Do not repair while GitHub reports the runner busy or systemd still has the
  service active.
- Do not treat a successful reroute to another runner as the permanent fix.
  PR #619 already showed that retrying can land on a healthy checkout while
  leaving the poisoned runner ready to fail the next job.

## 7. Acceptance — prove repair and prevention separately

Immediate recovery is complete only when:

- the exact checkout has no non-`runner` owner;
- `gha-nuc1-general-01` is `online` and its service is `active`;
- the RustSec rerun passes checkout and executes `cargo audit`; and
- the Docker rerun passes checkout and reaches Buildx plus
  `scripts/container-smoke`.

The permanent correction is complete only when:

- the workflow and its operations contract test land together;
- the FFmpeg 8 assertion is run once through the corrected lane;
- the next job on the same persistent runner can clean checkout, if any
  self-hosted container path remains; and
- the Ansible runner health check names any future wrong-owner path before the
  runner accepts unrelated work.

Useful final checks:

```bash
cd /Users/pjunod/code/plurx
python3 -m unittest discover -s tests/operations -p 'test_*.py'
make validation-lint

cd /Users/pjunod/code/ansible/github-runners
ansible-playbook --syntax-check <affected-runner-playbook>.yml
```

Re-resolve the actual Ansible playbook name rather than inventing one from this
handoff; the inventory and roles above are authoritative, and this incident did
not require a registration token or runner replacement.
