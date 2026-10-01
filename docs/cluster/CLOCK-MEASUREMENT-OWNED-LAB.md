# Owned clock lab — prepare, observe and retire one identified artifact

**Status:** open — prepared for review; no lab build, launch or observation executed
· **Written:** 2026-10-01 · **Source preparation:** actual effort `e0f91c741`.

Companion to [the measurement plan](CLOCK-SKEW-MEASUREMENT-IMPLEMENTATION.md)
and [accepted design §5.5](CLOCK-SKEW-GUARD-DESIGN.md#55-m4--record-the-evidence-gate).
This is the operator procedure for [the owned controller](../../scripts/k06-owned-lab.py),
not a measurement receipt or permission to deploy. Obtain coordinator review,
a separate operational window and a selected immutable measurement artifact
before any build or remote mutation. The source-only contracts do not validate
the physical hosts or waive the original acceptance.

## 1. Scope — four fresh voters, never the production cluster

| Host | Private address | Daemon publications |
|---|---|---|
| nynuc | 192.168.5.236 | TCP 55420 / 55421 / 55422 |
| m6 | 192.168.4.14 | TCP 55420 / 55421 / 55422 |
| nuc4 | 192.168.4.8 | TCP 55420 / 55421 / 55422 |
| nuc3 | 192.168.4.7 | TCP 55420 / 55421 / 55422 |

The controller binds published sockets only to those host addresses. The
daemon binds `0.0.0.0` inside a new owner-labelled bridge on each host; a host
address is not a valid bridge-container bind address. There is no host network,
privileged mode, device, Docker socket, production network or data mount.
Existing `PLURX_MDNS_ADVERTISE=false` disables advertisement. No discovery
companion or UDP GDM publication is created; the responder stays inside the
isolated bridge. There is no invented GDM setting or new product gate.

Each daemon has 2 CPU · 2 GiB RAM with no extra swap · 256 PIDs · read-only
root · 128 MiB `/tmp` · 16 MiB compatibility tmpfs · dropped capabilities ·
no-new-privileges · restart disabled. Docker logs are 10 MiB × 3. A 5,400-second
in-container timeout bounds a daemon left behind by controller failure.
Retained root size is checked against 1 GiB on resource samples. This is an
abort threshold, not a filesystem quota. The empty lab has no media/library.
Raw controller JSONL is capped at 64 MiB; exported daemon logs are capped at
32 MiB/node, and iperf output at 2 MiB on each of its two hosts. Total retained
raw/log/load output is therefore bounded at 196 MiB, apart from separately
bounded private state exports (at most 1 GiB/node).

At launch, all hosts must show available RAM ≥8 GiB, disk ≥10 GiB, load1 below
half their CPU count, no swap traffic and bounded CPU/memory/I/O pressure.
Active named action-job containers refuse admission. These are fresh checks,
not a capacity reservation: the earlier nynuc observation had active builds
and was unsuitable for idle measurement. Never stop a build or production
service to manufacture an idle window.

## 2. Freeze source before building the actual daemon

Use a clean owned clone on the selected current measurement source:

```bash
python3 scripts/k06-owned-lab.py source /private/tmp/k06-source-<nonce> \
  --repo /private/tmp/<owned-clean-clone>    # git archive only; no remote build
```

This records full commit/tree/archive SHA-256 and refuses archive traversal,
links, credential names and `.git`. Transfer the archive, not the checkout or
a repository/SSH credential, to the authorized existing Linux compiler.
The source receipt is explicitly **not built** and cannot be used as an
artifact receipt for launch.

Read-only inventory on 2026-10-01 found native AMD64 tooling on nynuc:
`sha256:2b11dbc8dc7f2b59ce42e743b838e96711a133bf67a6f50880dbdf5973b7e961`.
Its retained build history installs clang, cmake, nasm, ninja, pkg-config and
time on the pinned Rust 1.97.1 image. The older P02 tooling image `858e143b…`
is absent there. Reinspect the selected image before an authorized build;
no new toolchain, package installation or duplicate tooling image is needed.
The observed production runtime image on nynuc was
`sha256:15dde06b072c142a9f50b9168be6f0d6a83b843142ebcccd39bb90189eea6aae`.
It is a reusable runtime layer, **not** the selected measurement binary.
Never commit a production container or copy its config/environment/data.

The later authorized builder must retain its exact owner-labelled compiler
container ID, caps, image ID, a source-only read-only mount, and separately
leased owned target/registry cache paths. Do not borrow another live compiler
target or mount registry credentials. Verify this exact compiler before the
default-feature build, using the repository's [source-only loop](../ci/AGENT-COMPILE-LOOP.md):

```bash
rustc +1.97.1 --version                    # must be 1.97.1 / 8bab26f4f
PLURX_BUILD_REF=<full-source-sha> \
PLURX_BUILD_SHA=<full-source-sha> \
cargo build --offline --locked --release -p plurxd --bin plurxd
sha256sum <owned-target>/release/plurxd    # retain executable identity
```

No topology fake, validation feature, `--no-default-features` or debug daemon
can supply this receipt. Package that binary over the existing reviewed runtime
layer using a source-free Docker context containing only the binary and a
small Dockerfile; preserve `org.opencontainers.image.revision=<full SHA>`.
Record the canonical OCI manifest digest, its referenced config digest, complete
config and ordered uncompressed layer diff IDs, binary hash, compiler, full
build stamp and build command. Independently verify the archive index → manifest
→ config → compressed layers → uncompressed diff IDs before selecting it.
Docker's local `Id` is not universally the config digest: containerd stores can
expose the manifest digest while classic stores expose the config digest.
The former global-ID requirement was incorrect for this genuine mixed-store
case and is superseded by the proof-bound per-node identity below.
Load the identical approved archive on all four hosts. Image build/load is a separately
authorized later action, not performed by this controller preparation.

The private artifact JSON supplied to `plan` has these fields:

```json
{
  "source": "<40 lowercase hexadecimal characters>",
  "tree": "<40 lowercase hexadecimal characters>",
  "archive_sha256": "<64 lowercase hexadecimal characters>",
  "binary_sha256": "<64 lowercase hexadecimal characters>",
  "image": "sha256:<64 lowercase hexadecimal characters>",
  "config_digest": "<64 lowercase hexadecimal characters>",
  "rootfs_diff_ids": ["sha256:<ordered uncompressed layer digest>"],
  "image_config": {"Labels": {"org.opencontainers.image.revision": "<source>", "tv.plurx.k06-source-tree": "<tree>"}},
  "build": "<same full source SHA>",
  "compiler": "rustc 1.97.1 (8bab26f4f 2026-07-14)",
  "command": "cargo build --offline --locked --release -p plurxd --bin plurxd"
}
```

Placeholders are documentation only and fail validation. The executing binary
hash is checked after owned-container start, and the HTTP build stamp during
collection. A mismatch leaves the recovery manifest and fails the run.
`image` is the canonical manifest digest; `image_config` is the **entire**
independently verified runtime Config, not only the abbreviated Labels example.
Preflight inspects only the two pinned immutable manifest/config identifiers,
requires Linux/amd64 plus exact Config and ordered RootFS equality, and refuses
missing, foreign or ambiguous objects. It persists each node's actual
`docker_image_id` before any claim/create. Create, validation and recovery bind
to that retained node ID and recheck the canonical proof; source/config labels
and the existing binary/build checks remain additional checks, not substitutes.
No tag fallback, store reconfiguration or weaker source-only acceptance exists.

The only accepted image volume declaration is `/var/lib/plurx` (or none).
The original argv already covered that known path with a 16 MiB tmpfs; it did
not allocate an anonymous volume for the selected artifact. Same-review
hardening additionally refuses other declarations, adds `noexec` to both
temporary mounts, and checks exact `HostConfig.Tmpfs` bounds: `/tmp` 128 MiB
and `/var/lib/plurx` 16 MiB, both `rw,nosuid,nodev,noexec`. Docker's
[`--tmpfs` representation](https://docs.docker.com/engine/storage/tmpfs/)
is retained in HostConfig; if an engine also reports tmpfs Mounts entries,
the complete two-path inventory must match. The actual Mounts inventory
permits only one RW bind from the exact owned root to `/data`, plus those
optional tmpfs entries. Anonymous/named volumes, extra binds, partial tmpfs
inventories or changed bounds refuse start, recovery and cleanup validation.

## 3. Claim controller ownership, then enter the authorized window

```bash
python3 scripts/k06-owned-lab.py plan /private/tmp/k06-observation-<nonce> \
  --artifact /private/tmp/<approved-artifact-receipt>.json
python3 scripts/k06-owned-lab.py validate \
  /private/tmp/k06-observation-<nonce>/.active-cleanup.json
```

These two commands have no remote side effects. `plan` generates a 256-bit
nonce, fresh private output, `.owner` and durable `.active-cleanup.json` before
any host object. Remote roots are exactly
`/var/tmp/plurx-k06-measure.<owner>-<host>`. The container/network names and
labels bind to that nonce; cleanup refuses cross-campaign roots and foreign
labels. Paths with traversal, symlink controller ancestors/owner files,
nonprivate ownership and unbound hosts are refused.

After **separate** coordinator authorization and fresh capacity checks:

```bash
python3 scripts/k06-owned-lab.py launch <absolute-active-manifest> \
  --ssh-key <absolute-existing-local-key> \
  --authorized-window <coordinator-window-receipt>
```

The acknowledgement is an operator boundary, not an enforcement feature gate
or a self-issued qualification. The key is used by local SSH only; its contents
are never read, archived or sent to a compiler/container. No host operation
in these instructions is authorized merely because this document exists.

Launch obtains all production/discovery IDs, images, start times and restart
counts before creating any lab object. It persists returned exact network and
container IDs before starting each daemon. Current source
[`select_daemon_store`](../../crates/plurx-core/src/cluster/migration.rs)
activates the first fresh default-feature voter; no activation HTTP route is
invented. Ordinary `POST /api/v1/setup` with a random fresh password creates
the lab administrator. The returned token remains in a private local file.
`POST /api/v1/cluster/join-tokens` with `expires_in_seconds=600` admits each
fresh joining voter via its owner-only `join_token_file`. Tokens travel through
HTTP bodies/headers and SSH stdin, never argv or printed output. HTTP bypasses
proxies and refuses redirects. Raft/API TLS remains automatic and unchanged.

Every partial failure keeps the recovery manifest. Names are consulted only
to recover exact owner-matching IDs after a lost response; deletion itself
uses those IDs. An incomplete owner marker, foreign object, incomplete log
export or ambiguous process identity requires reviewed recovery, not guessing.

## 4. Collect actual signed observations, then bounded network traffic

```bash
python3 scripts/k06-owned-lab.py collect <absolute-active-manifest> \
  --ssh-key <absolute-existing-local-key> --authorized-window <receipt>
python3 scripts/k06-owned-lab.py load <absolute-active-manifest> \
  --ssh-key <absolute-existing-local-key> --authorized-window <receipt>
python3 scripts/k06-owned-lab.py report <absolute-active-manifest>
```

Idle collection is 3,600 seconds at 10-second cadence, 361 snapshots/node
including endpoints. It requires four reachable committed voters and exactly
three bounded signed peers per node. Each row retains metric completion's
monotonic timestamp, roster, build/uptime, container start/restart identity,
resource facts, host boot/uptime/type and actual timesync discipline output.
The host must remain synchronized with absolute offset below 250 ms. Numeric
clock upper bounds must remain below 2,000 ms; uncertainty, Unknown, missing
series, resets, discontinuities, swap/pressure or collection gaps fail closed.
The actual failing metric row is written before its clock assessment.

Load is one generated TCP stream from m6 to nuc4, private TCP 55423, using
the installed iperf3: 60 seconds at 20 Mb/s, one-off receiver capped at
22 Mb/s, wrapper deadlines 70/75 seconds and five-second termination grace.
Each process has address-space 256 MiB · CPU-time 15 seconds · output 2 MiB ·
256-process ceiling · 64 file descriptors. The exact timeout PID, executable,
start tick and argv digest are recorded privately; cleanup verifies identity
before signaling that wrapper, which forwards termination to its owned child.
No existing receiver is reused and no package, firewall, qdisc, service or clock
is changed. This carries roughly 150 MB of generated bytes; it is neither
link saturation nor production playback nor an executed ordinary-media encode.
The original loaded acceptance stays open until the coordinator accepts the
actual identified traffic receipt; this preparation does not waive its wording.

Loaded clock snapshots are 61/node at one-second cadence. Resource/discipline
facts refresh asynchronously every ten seconds with explicit timestamps and a
15-second freshness bound; they are not relabelled one-second CPU measurements.
Iperf JSON must prove the exact peer addresses, 59–62-second duration,
100–180 MB and 18–22 Mb/s. Unknown/discontinuity counters are compared across
the idle→load boundary, not reset to hide a failure.

`report` summarizes actual absolute offset, uncertainty and upper-bound
min/p50/p95/max, sample counts, raw hashes and clock-route authorization counter
deltas over measured monotonic duration. Three inbound peers at ten seconds
predict approximately 0.3 attributable authority reads/second/node; evaluate
the actual counter, never manufacture the rate. Observer roster discovery has
two consistent reads per round in current source; no dedicated measured roster
counter exists, and the summary says so. The collector's own two authenticated
admin requests per snapshot/node are separately labelled, not claimed as probe
authorization cost. An incomplete file remains incomplete; NTP output alone
never qualifies signed coverage. Every summary has `qualified=false`.

## 5. Stop exact owned objects and preserve private recovery evidence

```bash
python3 scripts/k06-owned-lab.py cleanup <absolute-active-manifest> \
  --ssh-key <absolute-existing-local-key> --authorized-window <receipt>
python3 scripts/k06-owned-lab.py export <absolute-active-manifest> \
  --ssh-key <absolute-existing-local-key> --authorized-window <receipt>
python3 scripts/k06-owned-lab.py purge <absolute-active-manifest> \
  --ssh-key <absolute-existing-local-key> --authorized-window <receipt>
```

Cleanup stops only verified owned process/container IDs, retains bounded daemon
logs before removing containers, and removes only empty owner-matching network
IDs. It verifies released ports and unchanged production/discovery identity.
There is no name-pattern kill, prune, production stop or broad directory deletion.
A failure keeps the manifest; do not call the run clean.

Export runs only after owned containers are absent. It writes a private bounded
tar on that same host, refuses links/special files, and records exact path/hash/
size. These disposable state archives contain **lab credentials**: keep them
private, and never attach their contents to a public report. Purge rechecks the
archive identity/hash and owner marker before fd-safe removal of the one
explicit lab root. The private tar, controller raw evidence and recovery
manifest remain for controlled retention. Their later removal needs explicit
exact-target authorization; this tool does not delete them or call them public.

## 6. Development proof is not a host receipt

Two new [source-only contracts](../../tests/operations/test_k06_owned_lab.py)
each failed with a bounded real guard mutation, then passed once after restore:
cross-campaign root/cap scope (`4c1d7a`→`308771`) and symlink-owner/unauthorized
launch (`0bd10f`→`e7bdb8`). No SSH, Docker or HTTP endpoint was opened by those
tests. The original passing test-file SHA-256 is
`bdac3455119860667a2d45c2dbac4da2cae3aca4b4143a833324a9219c4763a1`.
The launcher passing-worktree hash was `71fb16f2d365af42600745b183faa3b3fae683d2441844aeb518d669552ca12a`;
later collector/recovery refinements are source-inspected/syntax-checked, not
relabeled as a repeated final-tree test execution. No successful test is rerun.

Sole review35 found the threaded post-fork limiter and omitted daemon stderr.
Two separate [local process regressions](../../tests/operations/test_k06_owned_lab_review.py)
failed on the reviewed source (`3ef392`, `9e8758`), then each passed once on
the correction (`de3044`, `2333ee`). The launcher now execs a fresh limiter
interpreter before the command; no post-fork Python callback is used. Cleanup
combines both daemon channels within the unchanged 32 MiB export cap before
removing the exact owned container. Fake SSH/Docker executables used only
private local fixtures; no host endpoint was contacted. Original passes remain
historical evidence, not repeated executions or physical acceptance.

No compiler artifact was built, lab voter started, load generated, production
service changed or clock stepped during development. Operator review, selected
artifact build/load, fresh host capacity, actual idle/load observations and
independent receipt evaluation are still required. Active enforcement,
24-hour healthy refusal observation and the approved 15-second single/common-
mode clock drills remain separate open acceptance.
