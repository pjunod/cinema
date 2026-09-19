# reference film G on TCL — an E-AC-3 reporting change became a source-replacement refusal

**Status:** root cause reproduced on the real movie; the narrow repair is built
and merged. The architectural repair in §7 is scoped, not started.
**Incident:** 2026-09-17 UTC (2026-09-16 Eastern).

An Android tablet refused to play one movie. The server had decided the file on
disk was no longer the file it scanned. It was: two FFprobe releases described
the same bytes differently, and the comparison that guards encoded playback
read the difference as a replaced source.

## 1. Finding — one omitted E-AC-3 profile blocked the whole movie

The scan of reference film G kept in the catalog omits `profile` on global stream index 2,
an E-AC-3 audio track. The playback node's FFprobe reports it:

```json
{"index": 2, "codec_type": "audio", "codec_name": "eac3",
 "profile": "Dolby Digital Plus + Dolby Atmos"}
```

After the comparator's existing normalization, `/streams/2/profile` was the only
remaining difference between the two documents.
[`probes_describe_same_input`](../../crates/plurxd/src/ffmpeg.rs) returned
false, [`prepare_vod_encoding`](../../crates/plurxd/src/transcode.rs) turned
that into `vod_source_rescan_required`, and the
[HTTP handler](../../crates/plurxd/src/http/hls.rs) answered 409 before Media3
started decoding.

The server already tolerated the equivalent omitted-versus-reported
`Dolby TrueHD + Dolby Atmos` profile on TrueHD audio. Its codec-specific
exception did not include E-AC-3. The earlier repair and its regression fixture
covered TrueHD and several FFprobe defaults and left this pair uncovered.

**Underlying mechanism.** Scanner output is retained across engine upgrades;
playback compares it with the current engine's report. FFprobe 5.1.9 and
Jellyfin FFprobe 8.1.2 describe this file differently. This is a reporting
compatibility defect, not a demonstrated source replacement and not a TCL
decoder failure.

One related case shares the branch: a file with **no** stored probe also
reaches the same refusal (`.unwrap_or(false)`). That is an unknown source
reported as a changed one, and §7 treats it as the clearest example of the
category error.

## 2. Evidence

### 2.1 The Android requests failed before playback began

The client is Android Media3; the device was identified by the user as the TCL
tablet, and no independently captured serial is part of this evidence. The
requests were served by **lab6**, build `v0.3.0-2633-gc9e4edf4`, full commit
`c9e4edf451e12247a7aa4188903e5ba36888e7e9`.

| UTC on September 17 | Observation | Reading |
|---|---|---|
| 01:45:36.938 | File 120 session creation returns the stored-probe mismatch message | Comparison refusal, not an FFprobe timeout |
| 01:45:37.061 | Android reports `stage=session_create`, `RefusalException`, code 409 | Media3 never began decoding |
| 01:45:47.187 | Retry receives the same refusal | Retrying does not change stored metadata |
| 01:45:54.682 | A cold-start attempt receives the same refusal | Not restricted to resuming a session |
| 01:50:57.582 | Targeted item reanalysis reports one repaired file, zero failures | Operational mitigation completed |

The selected method was transcode with audio index 0. Global stream index 1 is
the default TrueHD track; index 2 is the second audio track. The comparator
examines the whole probe, so an **unselected** track refused the whole session.
Do not confuse global FFprobe stream indices with the audio-relative indices in
[`AudioStream`](../../crates/plurx-core/src/domain.rs).

### 2.2 Two real reporters, one real movie

The pre-repair catalog row was recovered read-only from lab6's retained Hiqlite
SQLite snapshot `01a0ad04-0658-7253-b05d-c399bb18559b`; its `scanned_at` is
`1784685955`, or 2026-07-22 02:05:55 UTC. No production database was restored
or edited.

| Comparison | Result |
|---|---|
| Catalog size versus current source size, before repair | Equal |
| Catalog second-resolution mtime versus current source mtime, before repair | Equal |
| Historical probe versus `/usr/bin/ffprobe` 5.1.9-0+deb12u1 | Entire JSON equal after removing only `format.filename` |
| Historical probe versus `/usr/lib/jellyfin-ffmpeg/ffprobe` 8.1.2-Jellyfin | Unequal; existing normalization leaves only `/streams/2/profile` |
| Refreshed catalog versus descriptor-bound 8.1.2 probe on lab6 | Entire JSON equal after removing only `format.filename` |
| Refreshed catalog versus descriptor-bound probe on media1 | Entire JSON equal after removing only `format.filename` |

| Global stream | Codec | Historical / fresh 5.1.9 | Fresh 8.1.2 | Policy before this change |
|---|---|---|---|---|
| 1 | TrueHD | `profile` absent | `Dolby TrueHD + Dolby Atmos` | Accepted omission |
| 2 | E-AC-3 | `profile` absent | `Dolby Digital Plus + Dolby Atmos` | **Refused** omission |
| 3, 4 | AC-3 | `profile` absent | `profile` absent | Equal |

The first long diff also contained optional codec labels, audio downmix fields,
zero padding, default dispositions and Dolby Vision report defaults. Existing
rules already account for all of those; listing them as causes would obscure
the one missing rule.

### 2.3 Upstream explains why a newer probe reports Atmos

FFmpeg commit `a4e5b946332dd625affd0e259eb787575e5c32f2` (published March 2023)
reads the E-AC-3 extension type A flag and reports the named Atmos profile when
it is set, leaving the profile unknown otherwise. It adds reporting of a
bitstream property; it does not instruct anything to convert the media. Do not
describe Atmos reporting as an FFmpeg 8.1 feature.

The catalog does not retain the producing FFprobe version in its raw probe, so
the exact executable and node that wrote the July 22 record remain unproved.
The scanner and playback resolve the binary identically (`PLURX_FFPROBE`, else
`ffprobe` on `PATH`), so a same-node divergence cannot come from the resolution
logic; the July row came from an environment where the variable was unset, or
from an older image. The [container build](../../Dockerfile) installs a floating
Jellyfin major version and defaults both media-tool variables to Jellyfin, so
rebuilding an image can change its reporting engine independently of any
catalog content.

### 2.4 Executable replay, on the fixture and on the movie

[The retained replay](../evidence/probe-compatibility-replay.py) extracts
the comparator as it stood on the serving node, checks its SHA-256
(`d07b5601eb330904573fbcee28a2cd3e597f401937bc7b5611a7c29ba894ea6c`), compiles
it beside the comparator in the working tree, and runs both.

```bash
python3 docs/evidence/probe-compatibility-replay.py
```

Seven synthetic tests pass: the deployed comparator refuses the legacy/modern
E-AC-3 pair, removing only that profile makes it accept, the shipped comparator
admits the omission while every negative control still refuses, and a shipped
refusal names its field without leaking a title or a pathname.

The synthetic suite proves the rule; it does not prove the movie. The same
script takes two real FFprobe documents:

```bash
python3 docs/evidence/probe-compatibility-replay.py --real-pair stored.json held.json
```

Run on lab6 on 2026-09-17 over the real reference film G remux, with
`/usr/bin/ffprobe` 5.1.9-0+deb12u1 as the stored document and
`/usr/lib/jellyfin-ffmpeg/ffprobe` 8.1.2-Jellyfin as the held one:

```json
{"deployed_admits": false, "shipped_admits": true,
 "shipped_differences": "", "shipped_truncated": false}
```

That is the whole defect and the whole repair, on the movie, through the real
functions. Neither probe is stored in this repository; private media metadata
does not belong here.

The replay builds `--offline`, so `serde` and `serde_json` must already be in
the Cargo registry. It makes no network requests, needs no server access, and
removes its build directory when it finishes. A passing replay is a statement
about the comparator, not about a deployed server.

## 3. Why the defect persisted, and why the workaround helped

[`scan`](../../crates/plurx-core/src/scan/mod.rs) deliberately skips a
successful probe when size and mtime are unchanged. That avoids re-probing the
whole NAS on every scan, and it also retains reports from older engines. An
ordinary library scan was therefore not a repair for an error whose own text
said "rescan it" — which is why that text is gone (§4.2).

`POST /api/v1/items/{id}/reanalyze` forces fresh probes through the existing
repair lease and replicated publication path. It refreshed item 120 on lab6,
and the refreshed catalog matches the current source on both lab6 and media1.
Physical tablet playback after the repair is not part of this evidence.

That is mitigation, not prevention: another old E-AC-3 Atmos probe would have
hit the same comparator, and a future reporting change can still expose an
unhandled difference. Earlier coverage in `fc3c8fbbbf1cb8e0ed1e7ffc41638011e5294acd`
exercises the added TrueHD profile and several defaults, but not this pair.

## 4. What shipped

### 4.1 One compatibility rule, written as a table

[`ignore_optional_stream_field_omissions`](../../crates/plurxd/src/ffmpeg.rs)
now carries the derived Atmos profiles as data rather than as branches:

```rust
const ATMOS_PROFILE_OMISSIONS: [(&str, &str); 2] = [
    ("truehd", "Dolby TrueHD + Dolby Atmos"),
    ("eac3", "Dolby Digital Plus + Dolby Atmos"),
];
```

`profile` is removed from both comparison copies only when the codec matches a
row on both sides, exactly one document omits `profile`, and the other reports
that exact string. The rule is symmetric, because a replicated catalog can be
newer than the node reading it. Nothing rewrites a persisted probe or the media.

The stream-pairing identity guard moved onto the shared predicate, so **every**
paired-audio removal — the downmix fields as well as the profile — now requires
the same explicit non-negative integer `index` on both sides. A missing, null,
negative, fractional, string, mismatched or reordered index is not a pairing and
buys no exception.

If both documents report a profile, it is compared normally: reported Atmos
versus reported non-Atmos still refuses. Missing is not null, an empty string, a
number, or an unrecognized name.

**Accepted limit.** A probe that never reported this property cannot prove the
historical presence or absence of Atmos. The exception treats the missing
observation as unknown, so it cannot detect a change in that one unmeasured
property. Held-source, storage-object, recipe and generation fences are
unchanged. Neither this comparator nor matching size and mtime is a
cryptographic proof that every byte is unchanged.

### 4.2 The refusal says what disagreed and what repairs it

`probes_describe_same_input` is gone as a production entry point. One function,
`compare_probe_documents`, normalizes once and returns the verdict together with
a bounded list of normalized field paths and a difference kind (`missing`,
`value`, `type`, `length`). Diagnosis and admission therefore cannot disagree by
construction.

The refusal carries it. `ps auxwww` on the box is not an acceptable answer for
work this server refuses, and neither is a log line only an operator with a
shell can read, so the 409 body now reads:

```
the stored probe differs from the source at /streams/2/profile missing;
reanalyze this item before playback
```

and, for a file that was never probed:

```
this file has no stored probe; reanalyze this item before playback
```

The typed code `vod_source_rescan_required` is unchanged, so every client keeps
its existing terminal classification; only the sentence became useful. The same
line is also written to the server log with the file id.

What the diagnostic may contain is bounded on purpose: at most eight paths, at
most 128 characters per rendered path, known FFprobe schema names and array
indices only, every container tag name collapsed to `tags/<field>` and every
unrecognized key to `<unknown-field>`. No values, no pathname, no free-form tag
text, no raw JSON.

### 4.3 Reanalyze is reachable on a healthy item

`unprobedNote` only ever rendered the Reanalyze button for a **failed** probe,
so the one action that repairs a stale scan was invisible on exactly the items
that need it. A healthy file now carries a **More actions** disclosure with the
same admin-only action, hidden on the television surfaces by the existing
`px-admin`/`th-admin` hooks. The API keeps its admin requirement and its repair
lease.

### 4.4 Settings → Developer says what is and is not known

No part of this is behind a feature flag; the compatibility rules are simply how
the comparison works. Settings → Developer carries a **Source verification**
section whose rows are advisory and gate nothing: the admitted omissions, the
media tools on this node, the absent scan provenance, the refusal diagnostics,
and the fact that typed source verification is not built.

## 5. Verification

Run from a checkout with the pinned toolchain — see
[the compile-loop instructions](../ci/AGENT-COMPILE-LOOP.md). The default
Homebrew `rustc` on a Mac is newer; select 1.97.1 explicitly.

```bash
rustup run 1.97.1 cargo test -p plurxd --bin plurxd held_probe_comparison
rustup run 1.97.1 cargo test -p plurxd --bin plurxd eac3
rustup run 1.97.1 cargo test -p plurxd --bin plurxd bound_source_rejects_same_size_same_mtime_path_replacement
python3 docs/evidence/probe-compatibility-replay.py
make web-check ui-check
```

| Case | Required result |
|---|---|
| E-AC-3 omission versus the exact Atmos label, either direction | Accept |
| TrueHD omission and E-AC-3 omission on the same file | Accept |
| Identical reports, both profiles present | Accept |
| Both profiles present and different | Refuse |
| Missing versus null, empty, numeric or unrecognized profile | Refuse |
| Atmos label on AC-3, AAC or video | No exception |
| Missing, null, negative, fractional, string, mismatched or reordered index | No exception |
| Codec, type, channels, sample rate or channel layout change | Refuse |
| Geometry, cadence, duration, chapter or track-count change | Refuse |
| Same-size, same-mtime pathname replacement | Existing source fence still refuses |
| More than eight differing fields, or private tag names | Bounded diagnostic, no private values or names |

`encoded_vod_manager_admits_a_reported_eac3_atmos_profile_the_node_omits` is the
session-level regression: it builds a real E-AC-3 source, stores a catalog row
that reports the derived profile the node's FFprobe omits, and requires
`create_session` to succeed — then requires the same path to refuse, with the
field named, when the channel count genuinely changes. A comparator test alone
would not prove the HTTP and session path.

reference film G itself has already been reanalyzed, so success on that title cannot prove
the compatibility repair on its own; the legacy fixture is what retains that
requirement. The old probe was not put back into the live catalog.

## 6. Limits

Proved: the exact normalized field, the version-dependent reproduction, and —
on the real movie, through the real functions — that the deployed comparator
refuses the pair and the shipped one admits it.

Not proved: which executable and node wrote the July scan, and post-repair
playback on the tablet. File-stat equality and matching FFprobe reports are not
a whole-file integrity check. No deployment or client build is claimed here.

Out of scope for this repair, and deliberately so: broad semantic probe
fingerprints, dropping source attestation, changing track selection, automatic
whole-library reprobes, an FFmpeg downgrade, and the separate source-timeout
work. Recording probe-producer provenance is a separate change because it
alters the stored schema; the cheapest real version is `-show_program_version`
on the scanner's ffprobe plus one normalization line. Provenance alone would
not make legacy and current reports comparable, and pinning future container
dependencies would not repair metadata older engines already wrote.

## 7. Broader repair — stop using report equality as source verification

### 7.1 The recurring exceptions expose the wrong abstraction

FFprobe JSON is an observation format owned by another project; source
verification is a durable application contract. Comparing those documents makes
an external reporting change part of playback admission. One exception per
failed title reduces incidents and cannot define a complete contract.

1. **Observation and identity are conflated.** A newly reported property can
   become a source-replacement verdict even when every previously measured
   property agrees; report equality also does not prove byte equality.
2. **Missing knowledge has no explicit meaning.** Missing can mean an old engine
   could not report a property, a probe did not measure it, or a required fact
   could not be established. A boolean cannot express those.
3. **Validation is wider than the recipe's dependencies.** An added profile on
   an unselected audio track blocked a selected TrueHD transcode. Track layout
   still matters for mapping; every descriptive field on every unselected stream
   does not necessarily matter to the recipe.
4. **There is no durable compatibility or provenance contract for scans.**
   Reports survive upgrades without their producer version or a versioned
   interpretation policy, so operators discover incompatibility through Play
   while the scanner considers the file unchanged.

### 7.2 Extend the typed model already in the code

The system is not uniformly built on raw JSON equality.
[`DecodeFacts` and `FactsDigest`](../../crates/plurx-core/src/transcode/decode.rs)
already extract selected video properties into typed facts and hash those, and
separate descriptor identity from catalog cache identity.
[`Encoding::identity`](../../crates/plurxd/src/vodencode.rs) combines
source-object version, encoder identity, resolved plan, arguments and subtitle
digest rather than hashing a raw probe. Nothing here shows that caches are keyed
on raw reports or are currently corrupt. The task is to extend and reconcile the
typed contracts, replacing the raw-report admission gate without introducing a
competing identity system.

| Question | Contract |
|---|---|
| Is this still the object held for preparation? | Existing descriptor and generation fences |
| What did inspection actually establish? | Versioned typed observations, with known / unknown / invalid states |
| Are the stored facts this recipe needs still supported? | Explicit comparison of recipe dependencies and required track structure |
| Can this encoded output be reused? | Existing source, plan and engine identity, with deliberate schema-version invalidation |
| How was this evidence obtained? | Producer build, probe option profile, parser schema, timestamp, source binding |

Retain raw JSON for diagnosis and future interpretation. A newly added field does
not automatically become an authoritative playback dependency. Before
implementation, inventory every consumer of probe-derived values: selected video
and audio, HDR and Dolby Vision, cadence, subtitle burn inputs, stream mapping,
chapters and timeline, and recipe and cache keys — **and** the three that are
not initial-command inputs at all: a mid-session audio-track switch, the
fragment/VOD index's source attestation, and the subtitle window. A field cannot
be dropped from verification merely because the first typed model lacks it.

### 7.3 Define the outcomes before replacing the boolean

| Outcome | Required behavior |
|---|---|
| Equivalent required facts | Continue under existing object and recipe fences |
| Confirmed changed required fact | Refuse the stale recipe with a specific mismatch category |
| Required fact unknown in historical evidence | Refresh or refuse as insufficient evidence, per an explicit per-fact policy; never assume equality |
| Inspection failed or timed out | Preserve the typed inspection error and bounded recovery owner; do not claim the source changed |
| Only irrelevant or reporting facts differ | Continue; retain diagnostic and provenance information |

**A refresh is not a Play-time reprobe.** "Required fact unknown → refresh"
means a cold NAS source gets probed at Play, which is its own failure mode. Any
such refresh must run under the bounded source-preparation owner and publish
through the reanalyze/repair lease, so it happens once per file rather than once
per Play.

Define importance per operation. Structural stream changes must still invalidate
stale mappings; an unselected track's newly reported label must not become a
decoder requirement; a newly learned HDR fact on the selected video can change
the output contract and needs stricter treatment. Legacy rows must be readable
with provenance explicitly unknown, deriving typed observations from their
existing raw reports where possible, and reprobing only when a required fact
cannot be established. Publish against the expected source revision so an older
job cannot overwrite newer facts, and freeze the chosen facts for the session.
Recording a new probe version must not by itself invalidate an artifact;
changing an output-affecting fact, interpretation schema or engine contract may.
A generic hash of the new typed object is not a sufficient design.

### 7.4 Deliver it with a compatibility corpus and a staged comparison

1. Inventory current consumers and existing source, fact and cache types; agree
   the fact schema, unknown-value semantics and dependency matrix.
2. Add producer and parser provenance for new scans, plus a read path for legacy
   rows. Avoid a mandatory whole-library rescan.
3. Implement the typed comparator in observation-only mode beside the current
   gate, with bounded diagnostics and no duplicate live probe. A disagreement is
   evidence to review, not authority to allow or deny playback.
4. Validate a sanitized compatibility corpus across supported FFprobe builds,
   then replace the gate after reviewing the disagreements and preserving the
   existing object and recipe invariants.

The corpus must cover TrueHD and E-AC-3 Atmos, omitted chapters and added report
defaults, unselected-track reporting changes, required-fact changes, malformed or
missing required data, file replacement, mixed-version cluster nodes, and
source-inspection failures.

**Completion criterion:** a supported probe upgrade that adds an irrelevant
field no longer needs a codec-label exception, and the system can say whether it
found a changed source, insufficient knowledge, or a failed inspection. That is
the architectural closure. What shipped here is the incident closure.
