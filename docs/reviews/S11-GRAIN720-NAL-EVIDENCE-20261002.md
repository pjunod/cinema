# Grain720 NAL evidence — one retained header census, not S11 closure

**Status:** done for this immutable scoped evidence ledger; original S11
qualification remains open · **Written:** 2026-10-02

Companion to the [codec qualification plan](../streaming/CODEC-AND-GPU-QUALIFICATION.md)
and [sixteen-cell internal ledger](../streaming/S11-INTERNAL-ROLLING-CELLS-2026-10-02.md).
This record makes one historical Grain720 header census independently
inspectable without treating packet key flags as NAL identity. It adds no
production behavior, maintained tool, registered test or qualification waiver.
Nine new synthetic controls and one new retained-byte census succeeded once;
no producer, probe, decoder, old test or passing cell was replayed.

## The result — 35 first IDRs and two genuine internal extras

| Retained Grain720 fact | Actual result |
|---|---|
| Contiguous complete TS objects | 35 |
| AUD-delimited, picture-identity and PTS-matched access units | 1,680 |
| First access unit with type-5 IDR header | 35/35 segments |
| Total type-5 IDR headers | 37 |
| Extra in segment 6 | PTS 1,256,250/90,000 = 13.958333333… seconds |
| Extra in segment 18 | PTS 3,420,000/90,000 = 38 seconds |
| Old packet key flags | 37, including the same two extras |
| Referenced SPS geometry | 1280×720 |
| Parsed pictures versus old packet counts and unique PTS | Equal; each timestamp matched within 1/90,000 second |

**How to read it:** these are actual Annex-B boundaries, parsed type-5
headers, referenced SPS/PPS and first-VCL picture identity, not a `0x65`
byte scan, a PES-equals-frame assumption or renamed old key flags. The first
example has PPS 0/SPS 0, one slice, PTS 7,500/DTS 0 and TS offset 564.
The old raw probes were read as comparison inputs, never executed again.
The two extras remain extras; they do not imply packet-grid drift or justify
changing production GOP flags.

This proves header identity on **one** synthetic internal context. Fifteen
other internal NAL contexts remain unmeasured. The campaign still records
**16 internal cells / 4 synthetic inputs / 0 original fully-qualified public
cells**. No entropy or pixel decode, closed-GOP reference independence,
decoder/device random-access acceptance, real-film, public Create, calibrated
HDR, tone-map fidelity, current-effort runtime, native or GPU qualification
follows. The original media1 matrix, real sources and physical acceptance
remain open.

## Exact historical source — not the current publication tree

| Identity | SHA-256 or immutable source |
|---|---|
| Measured source | `fb4360792a3ae077aa73a40ee30cfcdd024ff445` |
| Measured tree | `cc94375018a2fb569833d783dbdc9da93336fdf0` |
| ARM64 lab binary | `656e75891579d9526de544874f92651649ddf8f85071413b0bb207b4aabc524a` |
| Runtime image | `b7bc6f794e9d6aebae8fd54c32c513c3507173729e40c1b5048097e09e23d303` |
| Synthetic grain source, 1,222,841,401 bytes / 70.023 seconds | `df310256db516c37559f76f0e5f3945609b5c2824b1c548912731de1f49e65e1` |
| Original private acquisition receipt | `6039c85c357b7a6d2129b69677e8f5881a8489d214cef71b455f17bb2c88a18f` |
| Exact served playlist, 1,194 bytes | `2dd773c87ced6fefa63fd8370e79eec901855cb6ec3c06487ae0a71a829d2e0a` |
| Original private NAL result, 343,739 bytes | `9a286a6cbd781fa3bcf4f5a1180522f23f9321eb44afdb9c46f06b4db0ec995a` |

The [ordered input manifest](evidence/s11-grain720-nal-20261002/input-order.json)
binds all 72 original files: 35 TS objects, 35 old raw probe files, playlist
and original receipt, totaling 38,566,323 bytes. TS objects total 38,066,240
bytes. The original files remained unchanged. Fresh Linux copies were owned
by UID 501 and read-only; the parser held each descriptor and verified its
path, ownership, stat identity and hash. Publication does not relabel this
older runtime as the newer effort source.

## Review artifacts — historical snapshots, not a new test surface

All snapshots live in [the evidence folder](evidence/s11-grain720-nal-20261002/manifest.json).
The [manifest](evidence/s11-grain720-nal-20261002/manifest.json) carries each
published identity. Its preparation-time `archive_upload` field intentionally
records the historical pre-upload state; actual publication and retrieval
facts belong to the task PR body, not a rewritten frozen manifest.

| Snapshot | Exact historical SHA-256 |
|---|---|
| [Parser](evidence/s11-grain720-nal-20261002/parser.py) | `56825d98839fa31757112bbc7ebeba731e8266cf3bc8ea93ec15cb45af689c80` |
| [Nine-control source](evidence/s11-grain720-nal-20261002/controls.py) | `132a1e910f645715af611871100e1c9dd87db9783c885239c9055c0df0f322c9` |
| [Control watchdog](evidence/s11-grain720-nal-20261002/controls-watchdog.py) | `918ce1b064bcce7f08026118df8b53b97b1aea20fc714f81f41762e938ed5485` |
| [Repaired corpus wrapper](evidence/s11-grain720-nal-20261002/corpus-wrapper.py) | `170546a4e77b318a5d7d7edfa93611bd6280ee2b9c057b26c883258a086a91df` |
| [Sanitized complete AU result](evidence/s11-grain720-nal-20261002/nal-result.json) | `0d6feca4f581571695ac512ffed16eded449094c58ade980099a13d5f2a14aa5` |
| [Worker terminal](evidence/s11-grain720-nal-20261002/worker-terminal.json) | `56fb59b1fb79edf0ca9e061ed6c7ac0d72addbeedbe6604a7b83f364794bdbc7` |
| [Exact cleanup final](evidence/s11-grain720-nal-20261002/cleanup-final.json) | `c14f3a045b0165f4957cb34751f2c536fcb8c77d4d2545643512b1ed65da020e` |

The parser implements PAT/PMT/continuity/CRC, PES/PTS and Annex-B parsing,
AUD/picture boundaries and SPS/PPS references. Its supported subset is
progressive 8-bit 4:2:0, profiles 66/77/88/100 and POC types 0/2. It refuses
unsupported interlacing, scaling matrices, POC type 1, extensions, missing
AUDs, ambiguous picture/PTS grouping and transport/parameter ambiguity.
It does not parse entropy-coded picture data or certify full H.264 conformance.
H.222.0/H.264 clause guidance and primary FFmpeg n8.0 implementation were
cross-checked; incomplete access to official clause extracts is not a
standards certification. Source comments retain the exact reference links.

The [control journal](evidence/s11-grain720-nal-20261002/control-journal.json)
names all nine once-successful synthetic IDs: cross-TS/PES IDR; non-IDR I
slice and SEI lookalike; truncation/continuity/CRC/program ambiguity;
missing AUD/multiple pictures/shared PTS; invalid parameter/IDR prefixes;
PTS marker/duplicate/continuation refusal; two independent AUs; held-root
hash/symlink/traversal/path-swap refusal; and exclusive output/duplicate JSON.
They used 25 tiny header-only cases totaling 15,050 bytes. Test body elapsed
0.006 seconds; external runner elapsed 0.106365542 seconds. All nine successes
are retained, not rerun for this documentation PR.

Historical scripts retain private path assumptions and their exact source
hashes. They are review snapshots, not installation instructions, shipped
tools or registered tests. Any future execution needs separately bounded
admission; copying evidence does not authorize a replay.

## Resource and cleanup evidence — actual caps, not wrapper assertions alone

The supplemental [runtime configuration](evidence/s11-grain720-nal-20261002/actual-runtime-config.json)
and [actual parser process](evidence/s11-grain720-nal-20261002/actual-parser-process.json)
are separate immutable receipts. Their
[manifest](evidence/s11-grain720-nal-20261002/runtime-manifest.json) and
[privacy report](evidence/s11-grain720-nal-20261002/runtime-privacy-report.json)
bind those actual facts, not a later wrapper or expiry assertion.

Actual parser PID 7/parent 1, executable `/usr/bin/python3.11`, held source
FD 3 and source hash match the parser snapshot. The offline NUL argv digest
is `e3ef75705467829072bbf111faf8887432e96361b5e510f1e37195f083a87ac1`;
it carries no session capability. The process receipt records actual Linux
inode/device/start ticks, not an invented wall-clock start.

One corpus worker elapsed 3.082841626 seconds, peak cgroup memory 33,263,616
bytes, with all max/OOM/OOM-kill counters zero. Actual CPU quota was
200,000/100,000, memory limit 1,073,741,824 bytes, swap limit zero, PID limit
64. Docker was 2 CPU / 1 GiB / equal total swap / network-none / read-only.
Bounds remained 90 seconds outer, 60 seconds parser, 512 MiB input, 1 MiB
aggregate results, 64 MiB tmpfs/logs; cleanup 60 seconds plus an explicit
15-second watcher margin and finite 240-second guardian. Aggregate retained
results were 398,021 bytes. Exported result identity matched the held worker
result and actual stdout; parser group absent and result existence were
required independently of exit code.

Preparation and parser containers exited zero, without OOM. Both exact
containers and both nonce-owned volumes were removed after export and
successful inventories proved absence. Guardian PID 41475 and outer watcher
PID 41485 were terminal zero/process-group absent; an independent exact PID
check was empty. An independent post-run full container/volume inventory was
successfully empty. Nonzero inspect alone was never accepted as absence.
Docker was explicitly returned; no parser, watchdog, container or volume lease
remains. Original host evidence and the pinned runtime were preserved.

## Privacy and durable bytes — sanitized is not exact raw replay

The [recursive privacy audit](evidence/s11-grain720-nal-20261002/privacy-audit.json)
covers every publication JSON, Python string literal, retained output and
all 35 probe files/archive members. Root independently checked all 13 initial
publication-file hashes, all 73 archive-member hashes, complete archive hash
and both actual private session values' absence. Supplemental receipts were
separately audited and hash-bound. No credential or capability-bearing
acquisition argv is a publication input.

Only `$.provenance.session_id` was removed from each original result and
acquisition receipt; its value is never published. JSON serialization changes
also change hashes. All AU/key facts and media/probe/playlist bytes remain
unchanged. Original raw-private identities above are not sanitized identities.
The original admitted receipt remains private and unchanged; the attachment
receipt is explicitly named `receipt.sanitized.json`. New descriptor/inode
identities and sanitized provenance prevent claiming exact raw-result replay.

The private task PR retains one audit attachment named
`grain720-retained-audit-bytes.zip`: 38,574,624 bytes, SHA-256
`8da906445a712a2416c485a3775dac69e96852a12a2ddd0da2602b7966a9c964`.
Use its verified attachment locator and full download hash in the PR body.
The 73 members are 35 exact TS objects, 35 exact old probes, exact playlist,
sanitized receipt and sanitization boundary. No TS/binary is committed to Git;
no source input, runtime image or lab executable is uploaded. A server refusal
is not permission to use a public host, release or package fallback.

Retained-evidence owner is root's architecture effort. The attachment remains
durable audit evidence while qualification and future random-access/fidelity
audits need it; root retires it only after verified durable replacement or
explicit disposition. Only owned upload staging is disposable. Original grain
input expiry `2026-10-02T23:32:56Z` and runtime expiry
`2026-10-03T04:42:56.098867Z` are unchanged. Retaining output evidence does not
extend source/runtime lifetimes or permit new operational use.

## Failures and remaining acceptance — no historical success inflation

The initial sandbox runner could not establish its observer; no tests or
success journal were credited until a runner-only repair and the once-only
nine successes. Corpus-wrapper admission findings were repaired before the
one corpus attempt: full inventory-based absence/lost-create discovery,
exception-safe final receipt, explicit cleanup margins and worker/result
identity success guards. These are retained failures/findings, not successful
measurements retroactively invented. A later summary formatter requested
nonexistent `nal_types`; corrected read-only inspection used `vcl_nal_type`.
The census was not repeated. All original acquisition/OOM/verifier failures
and reconstructed HDR360 admission limitations remain in the preceding ledger.

One different independent adversarial review of this evidence PR is required
before current-head effort integration. Author/root operational admission is
not that review. This evidence does not close original S11, change any GOP,
cache identity or qualified tuple, waive gates or authorize main promotion.
