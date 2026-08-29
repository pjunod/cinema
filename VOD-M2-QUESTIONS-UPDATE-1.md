# VOD M2 questions — update 1: §2 is partly measured

**Status:** addendum to [VOD-M2-QUESTIONS.md](VOD-M2-QUESTIONS.md) as sent ·
**Changes:** §2 only · **Written:** 2026-08-24 ·
**Branch:** `agent/vod-m2c` @ `e4f7285c`

Nothing in §1, §3, §4 or §5 has changed. Every decision needed there still is.

This is the whole of the change: I ran the experiment §2 asked for instead of
only asking for it. It found something about the *existing* evidence, produced a
partial answer, and hit one dead end worth knowing about.

---

## 1. The evidence the rule rests on measures a different byte string

§2 named three things that answer to "the generation's init". The one M0-P0
clause (d) proved stable is **not** the one plan §2.2 governs.

`scripts/vod-plan-probe` hashes `Unit::Init`, and its own comment says so:

> The initialization segment is ftyp + moov, the same bytes `FragmentReader`
> publishes as `Unit::Init` (fmp4.rs:330-353) — not "everything before the first
> moof", which would fold interleaved free/skip padding into the hash and fail
> clause (d) on a byte the real init.mp4 never contains.

That is byte string **(1)**, ffmpeg's raw output. The probe never calls
`promote_hevc_parameter_sets` or `promote_hdr10_static_metadata`.

Byte string **(2)** — the promoted init, the one `copyseg` writes to `init.mp4`
and the one a viewer receives — has not been measured by anything.

So clause (d)'s 9/9 pass, including its seeked generation, is a true statement
about (1) and says nothing about (2). That is not a criticism of the probe: it
measured what it set out to. It is that §2.2's rule and clause (d)'s proof are
about different artifacts, and the gap was invisible until something had to
implement the rule.

## 2. What I could measure, and what it says

New probe: `crates/plurx-core/examples/init-promotion-probe.rs`. It runs the
video copy pipe once, then promotes the init from **every clean fragment in the
file** and compares the results.

| Fixture | Clean starts | In-band parameter sets | Promoted init |
|---|---|---|---|
| `closed-gop-2397` | 23 | byte-identical at all 23 | identical from all 23 |
| `clean-cra-2397` | 23 | byte-identical at all 23 | identical from all 23 |
| `open-gop-2397` | 1 | — | not measurable, one start |

On a single-pass libx265 encode with `repeat-headers=1`, every IDR carries the
same parameter sets, so a repositioned generation promotes the same bytes and
§2.2 holds.

**This removes the alarming reading.** The rule is not broken for everything,
and I am no longer worried that it is.

## 3. The dead end, and why the probe measures NAL units

**Promotion never fires on a synthetic fixture.** It is guarded on an `hvcC`
carrying zero NAL arrays, and `fmp4.rs:718` names the population it exists for:

> A few WEB-DL Matroska sources carry the 23-byte minimum hvcC record (zero NAL
> arrays) and put all three parameter sets in their first sample.

A libx265 encode always writes a rich `hvcC`. So no synthetic corpus reaches the
branch — not mine, and not M0's. A probe that only diffed promoted inits would
have reported "identical" for the uninteresting reason that promotion did
nothing, which is what my first run did.

So it measures the quantity that decides the answer either way: the VPS/SPS/PPS
NAL units in each candidate starting fragment's first sample, which are exactly
the bytes promotion copies into `hvcC`. Identical NALs at every legal start
means an identical promoted init whether or not the branch fires on that file.

## 4. What is still open

Every fixture measured was produced by one encoder, in one pass, with fixed
settings — the case *least* likely to vary. The sources that reach the promotion
branch at all are remuxes, and the ones this would most likely bite are a title
assembled from more than one encode, or one with per-scene parameter changes.
Neither is synthetic and neither is in the corpus. M0 recorded the same shape of
gap for Dolby Vision: "no DV source was reachable".

**One real title finishes it.** Any HEVC WEB-DL whose `ffprobe` shows an `hev1`
sample entry rather than `hvc1` — that is the shape that reaches the branch:

```bash
cargo run -p plurx-core --example init-promotion-probe -- /path/to/title.mkv
```

One line of output per file, no fixtures needed, nothing written. If the
parameter sets are byte-identical at every clean start there too, §2.2 is safe
as written, I implement it against byte string (2), and §2 closes.

## 5. What this does not change

The **decision needed** in §2 stands as written: which of the three byte strings
§2.2 governs. Measurement can tell us whether the rule is *satisfiable*; it
cannot tell us which artifact the rule was meant to be about, and the three
choices still write three different rules into the store.

If the answer is (2) — the promoted init, which is what a viewer receives and
therefore what I would guess — then the second half of §2's question also
stands: whether a differing promoted init on a reposition is a
`producer_failed`, or a
reason to re-promote and rewrite the stored init. The second is only safe if
every already-materialized segment still decodes against the new one, and I do
not think that is knowable without measuring it too.
