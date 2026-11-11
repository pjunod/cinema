# A1/B1 — what the first effort merge leaves for the next one

**Status:** open · **Owner:** Claude (claude-opus-5-5), session
`session_01BCWL9ypnQEiQUyRaZtMhh3` · **Updated:** 2026-11-11.

The [A1/B1 build contract](PLAYBACK-START-A1-B1-BUILD.md) is the design
authority; [the ledger](PLAYBACK-START-STATUS.md) records ownership and
merges. The first `effort/playback-start-phase2` merge delivers the Source
growing engine for authenticated bounded shared playback, canonical range
observations, the copy-range consumer and guarded v3 publication. A v3 index
publishes today only when playback itself observed the whole title. These
items remain, in build order. Each names the existing owner it extends; none
adds a gate, a timer or a watchdog.

## 1. Common spawn adapter

`copyrange::spawn_source_generation` and `run_source_census` take
`SourceRenditionOwners`, a `SourceGenerationDispatch` and a
`SourceGenerationAuthority`. Give both a typed owner instead:

- `Source { owners, dispatch, authority }`: today's arguments, unchanged.
- `Analysis { phase }`: an exact `AnalysisPhysicalPhase` whose custody is
  already active, with `SourceInput::open_background_held` reading under
  `SourceReadBudget::from_analysis_phase`. No admission permit, no Source
  grant and no media-serving authority.

The ffmpeg recipe, `ObservedCopyMux`, normalizer and companion verifier stay
the same code on both paths. AV output is discarded only after the companion
verifies it. Every pipe task and the reap join before the phase finishes.

## 2. Background gap filling

When a pending source set (`copy_analysis::work::PendingSources`) is
incomplete and `begin_analysis_source_phase` admits it (its capacity clause
is the idle test), run one Analysis-owned generation through the adapter:

- From title start when `begin` is missing; otherwise from the first gap.
- Feed its rows into the same `CopyObservationSet`, so overlap equality with
  playback rows is checked, not assumed.
- Stop at the next observed row or EOF. Foreground pressure pauses the budget
  through the existing pause controller.
- A seeked start uses the verified seek-bootstrap landing, never a long
  sequential fallback.

The full-source attestation phase reads every byte. Fold the whole-title
HDR10 maximum scan into that same read (item 3).

## 3. Whole-title HDR10 metadata in the attestation read

A sealed observation index carries the bounded head census's promotion. The
legacy local index promises `indexed_hdr10_metadata` with the scanned
`hdr10_sei` (`fragindex.rs`, the `--plurx-hdr10-index` argv revision).
Compute the same maximum over the attestation phase's bytes, record it with
the receipt, and seal it into the promotion. Until then, never bridge a v3
index into `put_fragment_index`: HDR10 titles would get different init
metadata than the legacy key claims.

## 4. v3 consumer and peer hydration

- VOD planning reads `copy_index_v3_head` for the exact source key.
- A healthy head's blob is read from `copy_index_v3_locations`, or hydrated
  from a listed holder and verified by digest before use.
- The hydrating node records its own verified location.
- Decode with `decode_cluster_fragment_index_blob_v3` against the head's
  source and output identity.

## 5. Discrepancy quarantine

A consumer that finds a healthy head's bytes contradict a fresh observation
calls `quarantine_copy_index_v3_head`. The deterministic successor
generation re-enters items 1–2. The old pointer stays readable until the
successor publishes.

## 6. Local playback observations

Local rolling copy and indexed-VOD copy become the same growing engine.
`CopyObservationAuthority` gains a typed local demand variant that never
reuses a Source grant.

## Acceptance for the next merge

- A partially played shared title publishes after background filling, with
  no playback interruption on any node.
- An HDR10 and a DV title's v3 index serves the same init as their legacy
  index.
- A second node plays the title through hydration.

## Execution log

| Date | Model / session | Action |
|---|---|---|
| 2026-11-11 | claude-opus-5-5 / session_01BCWL9ypnQEiQUyRaZtMhh3 | Written after the first effort merge's scope was fixed. |
