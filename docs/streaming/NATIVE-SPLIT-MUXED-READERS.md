# Native split: who still reads the muxed copy

**Status:** open — inventory built; the split-only change belongs to
[split HLS as the default](SPLIT-HLS-DEFAULT-INVENTORY.md) S2/S5 ·
**Effort:** `E-BFB7D832E3` P3, handed to `E-5F2C2E9EA4` ·
**Written:** 2026-11-27 against `main` @ `f7c06f0ee`

On the native Safari Dolby Vision split, the rolling copy writer stores each
window twice: the ordinary muxed copy (`init.mp4`, `segNNNNN.m4s`,
`index.m3u8`) and its projections (`split-audio-*`, `split-video-*`, and
`split-fel-*` once FEL is rendered). The duplicate is deliberate. The layout
is undecided until the master is requested
(`NativeSplitInventory::choose`), and the muxed copy is the fallback whenever
the split declines. This document lists every reader of the muxed copy after
the split is chosen, so that the change that stops writing it knows what has
to move first. It changes no behaviour.

`P/` = `crates/plurxd/src`. Lines are from `f7c06f0ee`; re-verify by function.

## 1. What is written and reserved

- **Writes.** After the split is chosen, every window is still written as
  muxed media plus the two projections (`P/copyseg.rs` `write_init`,
  `write_split_segment`, `publish_split_files`). The projections carry the
  same samples, so logical media writes are about twice the muxed payload,
  plus `split-fel-*` for rendered FEL windows.
- **Charge per write.** `publish_file` reserves and authorizes twice each
  object's size until its rename settles (`P/copyseg.rs:1324`, `:1326`).
- **Startup reservation.** A split start reserves `2 × startup_bytes`. A warm
  FEL head reserves `3 ×` (`P/transcode/manager/start.rs:2030`, `:2036`).
  The scratch envelope scales by the same factor (`:2264`, `:2266`).
- **Optional writes.** An undecided split may only spend growth that leaves
  the ordinary copy's startup remainder unspent (`ordinary_reserve`,
  `P/copyseg.rs:1393`). It declines with `startup_budget_exceeded` when both
  copies would pass the session ceiling (`:1519`).
- **No retained output.** A native split session gets no retained budget
  (`start.rs:2076`). `publish_file` never digests or captures `split-*`
  objects (`copyseg.rs:1329`, `:1376`).

Not measured here: peak scratch and unique allocations on a real FEL title.
They need a native Safari session on a Linux node, which this inventory did
not run. They are open under §3.

## 2. Readers of the muxed copy once the split is chosen

| Reader | Reads | Kind | Without the muxed write |
|---|---|---|---|
| FEL splice `run_native_fel_splice` (`P/transcode/manager/publication.rs:1840`, `:1934`) | `init.mp4`, `seg(n)` and `seg(n+1)` from disk, re-split with `NativeSplitAuthor` | FEL processing | Breaks. Needs the full-fidelity fragment from memory or a private store. |
| Publication cycle (`P/transcode/rolling/session.rs:898`, `refresh_segments` `:2547`) | `index.m3u8` and segment sizes. This builds `publication.served` and `session.segments`. | Publication clock, flow control, retention | Breaks. This is the clock the split also runs on. |
| Split child playlist (`publication.rs:2138`) | `TARGETDURATION` and `EXTINF` from the muxed snapshot | Serving | Needs the split's own playlist. |
| Split segment gating (`publication.rs:2271`–`2500`) | Split bytes, gated by the muxed snapshot and index | Serving | Needs gating on the split's own index. |
| Plain media URLs (`publication.rs:2310`, `P/http/hls/segment.rs:1193`) | Muxed `init.mp4` and `segN.m4s`; nothing checks the chosen layout | Serving | Must be refused or redirected once the split is chosen. |
| Master codec string (`P/http/hls/context.rs:194`, `P/http/hls/playlist.rs:231`) | `hvcC` in the muxed `init.mp4` feeds `native_split_master_text` | Serving | Needs the split-video init or a stored codec string. |
| `?diagnostic` master (`playlist.rs:233`, `:284`) | Serves the muxed master and media even after the split was chosen | Diagnostic | Must serve the split, or say that it is unavailable. |
| Retention sweep `gc_expired_segments` (`P/transcode/rolling/retention.rs:310`) | Retires the muxed `segN` first, and prunes `split-*` and FEL objects only inside that step | Retention | Breaks. Split objects would never be pruned. Needs split-keyed retention. |
| Live bytes (`retention.rs:397`) | Muxed index total plus `split.bytes()` | Accounting | Works if the muxed term becomes zero. |
| Scratch walk `measure_scratch_bytes` (`session.rs:766`) | Every regular file | Accounting | Works. |
| Exit classification (`P/transcode/producer/prepublication_executor.rs:846`) | `index.m3u8` for ENDLIST and the frontier | Recovery | Needs a split end-of-list signal. |
| Output measurement (`prepublication_executor.rs:742`, `copyseg.rs` `observe_object`) | Muxed digests and the playlist | Measurement | Degrades to no observation. |
| Retained collector (`P/vod/rolling_retained.rs:118`, `:321`; `P/rolling_output.rs:84`) | Hard links and digests of muxed members only | Retention | No retained artifact. A split session already has none (§1). |
| Startup validation (`P/transcode/session_request.rs:284`) | Muxed `init.mp4` and the first segment | Startup | Works: it runs before the layout can be chosen. |
| Retired-owner grace (`P/transcode/manager/control.rs:166`, `:2142`) | Muxed init name and retired muxed objects | Serving | Same as the plain media URLs. |
| Warm FEL head (`P/transcode/manager/native_fel.rs:29`, `:173`, `:629`) | Muxed media in its own `native-fel/head-*` scratch | FEL processing | Unaffected: it does not read the session directory. |
| `ordinary_video` digest (`copyseg.rs:1734`, `native_fel.rs:749`, `:855`) | In-memory projected video, not a disk read | FEL check | Unaffected. |
| Jellyfin native segments (`P/http/jellyfin/representation.rs:192`) | Muxed URLs | Serving | Unconfirmed. Only a `transport=native` master chooses the split (`P/http/hls/create.rs:5072`), so Jellyfin probably never has a split session. |
| Takeover and prepared successor | No reader of the old session's muxed files was found | Recovery | Unconfirmed. |

## 3. What the split-only change has to do

The muxed copy is load-bearing, not just a fallback. Its playlist is the
split's publication clock and retention key, and the FEL splice reads its
fragments. Skipping the write therefore means first giving the split its own
index, playlist, codec signal, end-of-list and retention. It also means
deciding the layout before the first media write, from a client capability.
Both are what S2 (contract, caps v2 split token) and S5 (rolling copy,
recovery, takeover) already build.

These acceptance items are the playback architecture follow-ups' P3
acceptance (`E-BFB7D832E3`), carried unchanged as S5 exit criteria for the
native FEL route:

- Unique allocations and peak scratch on a real FEL title, before and after,
  under matched cache and load.
- Atomic publication of the split-only layout, with disk-full and cancel
  recovery.
- Existing-client playback, including any client that still asks for the
  muxed form, after a fallback check before exposure.
- Exact source-picture adjacency, and uninterrupted AAC across the FEL
  splice (a historical Safari dual reset introduced a 44–46 ms gap).
- Rollback to the muxed layout.
