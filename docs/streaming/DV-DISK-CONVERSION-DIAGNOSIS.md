# Dolby Vision disk conversion — diagnosis and proposal

**Status:** diagnosis complete, proposal unbuilt · **Written:** 2026-09-03 ·
**Evidence:** live fleet reads on nynuc, m6, nuc4, nuc3 at
`v0.3.0-575-gaa486f4b` · **Reviewer:** please check §2 against the code and
§5 against your own arithmetic before agreeing with any of it.

Automatic Dolby Vision Profile 7 → 8.1 disk conversion is enabled on the
fleet and has never once succeeded. It queued 38 titles on the evening of
2026-09-03 and failed every attempt it reached. This document is the
diagnosis of why, the cost model of what would happen if the immediate
blocker were removed, and three proposed changes — one that stops the
current failure loop, one that stops the conversion from spending 124 GB to
learn something knowable in 2 seconds, and one that makes both visible from
inside the product.

Read §1 and §2 for the failure. §3 is the cost model and is the part most
likely to change your mind about whether to enable this at all. §4 is the
policy question that belongs to Paul, not to an implementer. §5–§7 are the
proposal.

**Standing instruction for whoever builds this:** if a step seems to require
changing the conversion pipeline's *output* — the argv in
`dv_disk.rs:568-648`, the commit ordering, the recovery-guard ledger — stop
and flag it. Everything proposed here is a gate placed *in front of* that
pipeline. The pipeline itself is correct and is not what is broken.

---

## 1. What is happening on the fleet

All four nodes run `v0.3.0-575-gaa486f4b`. Automatic conversion is on.

```
22:19:24  INFO  plurxd::state: queued automatic Dolby Vision conversions
                queued=29 saturated=false                        [nynuc]
22:21:41  INFO  plurxd::state: queued automatic Dolby Vision conversions
                queued=9  saturated=false                        [m6]

22:30:14  WARN  plurxd::state: Dolby Vision conversion failed file_id=23
                error=creating /20t/movies/Bring Her Back (2025)/
                .Bring Her Back (2025) Remux-2160p.mkv.plurx-dv-23:
                Read-only file system (os error 30)
22:38:03  WARN  plurxd::state: Dolby Vision conversion failed file_id=34
                error=creating /20t/movies/Deep Blue Sea (1999)/
                .Deep Blue Sea (1999) Remux-2160p.mkv.plurx-dv-34:
                Read-only file system (os error 30)
```

38 jobs queued. Zero conversions committed on any node, ever. Failures
arrive roughly one every eight minutes as the worker reaches each job.

### The path that fails, and why it is that path

`ConversionPaths::for_source` (`crates/plurxd/src/dv_disk.rs:340-368`)
builds a hidden working **directory** beside the source file:

```rust
let directory_name = format!(".{name}.plurx-dv-{file_id}");
let directory     = parent.join(&directory_name);
```

so for file 34 the workspace is:

```
/20t/movies/Deep Blue Sea (1999)/
    Deep Blue Sea (1999) Remux-2160p.mkv          ← the source
    .Deep Blue Sea (1999) Remux-2160p.mkv.plurx-dv-34/
        BL_RPU.hevc            extracted video elementary stream
        BL_RPU.p81.hevc        the same stream, RPU rewritten to 8.1
        RPU.bin                extracted metadata
        replacement.mkv        the finished movie
        source.p7.original     the original, renamed in (not copied)
```

and on commit the original is renamed back out to
`<source>.mkv.p7.orig`, in the same directory, and kept.

This is deliberate: the conversion replaces the file in place and keeps the
original beside it, so every path it touches is inside the media directory.
There is no scratch-volume option. **The conversion requires the media
directory itself to be writable.**

### It is not the NAS, the export, or the user

| Layer | State | Verdict |
|---|---|---|
| QNAP NFS export, host mount | `qnap-storage:/20T on /mnt/qnap/20t nfs4 (rw,…)` | writable |
| Directory permissions | `drwxrwxrwx 220 1000 1000 /20t/movies` | writable |
| Container user | `Config.User=[1000:1000]`, `uid=1000 gid=1000 groups=1000,992` | owner |
| **Docker bind mount** | **`/mnt/qnap/20t:/20t:ro` → `RW=false`** | **the blocker** |

The container runs as the directory's owner, on a mode-0777 directory, on a
read-write NFS export. The single reason the write fails is the `:ro` flag
on the bind mount in `deploy/docker-compose.override.yml` lines 18-21:

```yaml
    volumes:
      - /mnt/qnap/media:/media:ro
      - /mnt/qnap/20t:/20t:ro
      - /mnt/qnap/8tb:/8tb:ro
      - /mnt/qnap/8t-2:/8t-2:ro
```

Identical on all four nodes; `docker inspect` reports `RW=false` for every
one of the sixteen mounts.

That flag is not a mistake. It is the reason a bug in the conversion
pipeline has never been able to touch a remux. Removing it is a decision,
not a fix — see §4.

---

## 2. What the conversion actually does

Four stages, none of which re-encode. All are stream copy or metadata
rewrite, so this is I/O-bound, not GPU- or CPU-bound
(`crates/plurxd/src/dv_disk.rs:568-648`):

```
  source.mkv  (P7 dual-layer: BL + EL + RPU, plus audio/subs/chapters)
      │
      │  ffmpeg -c:v copy -bsf:v hevc_mp4toannexb,filter_units=remove_types=63
      ▼
  BL_RPU.hevc        video elementary stream, EL (NAL 63) stripped   ~1× video
      │
      ├──── dovi_tool -m 2 convert --discard ───▶ BL_RPU.p81.hevc     ~1× video
      │                                            RPU rewritten 7→8.1
      │
      └──── dovi_tool extract-rpu ──────────────▶ RPU.bin             tiny
                                                      │
                                        dovi_tool info --summary
                                                      │
                                                      ▼
                                              el_type: MEL | FEL
                                              ↑ THIS IS LEARNED HERE,
                                                after ~2× video is written
      │
      │  mkvmerge -o replacement.mkv BL_RPU.p81.hevc --no-video source.mkv
      ▼
  replacement.mkv    converted video + every original audio/sub/chapter track
```

Two properties of this ordering matter:

**`BL_RPU.hevc` cannot be freed early.** `extract-rpu` at line 616 reads it
*after* `convert` has already produced `BL_RPU.p81.hevc`. Nothing removes
either intermediate until the whole scratch directory is torn down at the
end. All three large artifacts coexist at peak.

**The EL type is discovered at stage 3.** `parse_el_type` reads
`dovi_tool info --summary` output, and the result is what tells you whether
this title had a Full Enhancement Layer worth discarding or a Minimal one
worth nothing. By the time it is known, roughly two full copies of the video
stream are already on disk.

---

## 3. The cost model

Measured on a real title from the queue — *Deep Blue Sea (1999)*,
`/20t/movies/Deep Blue Sea (1999)/Deep Blue Sea (1999) Remux-2160p.mkv`:

```
size            68.7 GiB
video           hevc, 84,348 kbps, 2160p, Main 10 @ L5.1, ~62 GiB
audio           TrueHD/Atmos 3406k · DTS-HD MA 4355k · AC3 448k · 3× AC3 192k
```

**Peak disk, inside that one movie's folder:**

| Artifact | Size | Note |
|---|---|---|
| `source.p7.original` | 0 extra | renamed in, not copied — `rename_noreplace_durable` |
| `BL_RPU.hevc` | ~62 GiB | |
| `BL_RPU.p81.hevc` | ~62 GiB | |
| `RPU.bin` | ~50 MiB | |
| `replacement.mkv` | ~68.6 GiB | |
| **peak new bytes** | **~193 GiB** | ~2.8× the source |
| **peak folder total** | **~262 GiB** | including the original |

**Steady state, after commit:** the scratch directory is removed, but
`<name>.mkv.p7.orig` is kept permanently beside the converted file.
**Footprint is 2× the source, forever**, unless something deletes the
retained original.

**Fleet arithmetic for the 38 queued titles**, at ~65 GiB each:

```
retained originals    38 × ~65 GiB   ≈ 2.5 TiB   permanent
free on /20t          4.8 TiB of 19 TiB (74% used)
```

Converting the queue as it stands consumes over half the remaining space on
the share, permanently, and that is *before* the transient 193 GiB per title.

---

## 4. The finding that should change the policy — MEL vs FEL

Profile 7 comes in two shapes, and they are not equally worth converting.

**FEL (Full Enhancement Layer).** The EL carries real residual picture data,
typically several Mbps. Discarding it loses actual precision. The converted
file is meaningfully smaller than the source. Conversion is a real trade.

**MEL (Minimal Enhancement Layer).** The EL is a formality — it carries no
picture residual. Discarding it loses nothing perceptible, and saves almost
nothing, because there was almost nothing there.

*Deep Blue Sea* — one of the two titles that failed tonight — is MEL:

```
Dolby Vision MEL @ 77 kbps        of an 84,348 kbps video track
```

77 kbps out of 84,348. **0.09%.** Converting that title means writing ~193
GiB, doubling its footprint permanently, and producing a file the same size
as the input, in order to remove nine-hundredths of one percent of the
bitrate. The only thing gained is that P8.1 plays in browsers where P7 does
not — which is real, and is the entire point of the feature, but it is worth
knowing that for a MEL title *that is the whole benefit*: the size and
quality columns are both approximately zero.

Nothing in the current eligibility check knows this.
`eligibility_reason` (`crates/plurx-core/src/store/dv_conversion.rs:569-592`)
gates on five things and the EL type is not among them:

```rust
if !container.is_some_and(|v| v.eq_ignore_ascii_case("mkv")) { … }
if profile != Some(7)                                        { … }
if !matches!(bl_compat_id, Some(1 | 6))                      { … }
if el_present  != Some(true)                                 { … }
if rpu_present != Some(true)                                 { … }
None
```

`el_present` is true for MEL and FEL alike. Every P7 title in the library is
equally eligible, and the distinction is only discovered 124 GiB into the
work.

### The EL type is knowable in 2 seconds

This was tested on the fleet, on both shapes, not reasoned about:

```bash
# 10 seconds of the head, same bitstream filter the real extract uses
ffmpeg -nostdin -v error -t 10 -i "$F" -map 0:v:0 -c:v copy \
  -bsf:v hevc_mp4toannexb,filter_units=remove_types=63 -f hevc head.hevc
dovi_tool extract-rpu -i head.hevc -o head.rpu
dovi_tool info -i head.rpu --summary
```

| Title | Size | Extract | Time | Verdict |
|---|---|---|---|---|
| Deep Blue Sea (1999) | 68.7 GiB | 70 MiB | 2 s | `Profile: 7 (MEL)` |
| The Sound of Music (1965) | 77.0 GiB | 49 MiB | 2 s | `Profile: 7 (FEL)` |

Two seconds and ~60 MiB, versus ~124 GiB and roughly an hour of NFS I/O, for
exactly the same answer.

> The container-metadata route does not generalise: of 45 large remuxes
> scanned on `/20t`, only two carried a `Dolby Vision MEL|FEL` string in the
> video track title. The EL type has to come from the RPU. It just does not
> have to come from *all* of the RPU.

---

## 5. Proposal

Three changes, in dependency order. All three are gates in front of the
existing pipeline; none alters what the pipeline produces.

### 5.1 M1 — refuse before queueing when the destination is not writable

**The failure this prevents:** 38 jobs accepted, each discovering
independently, eight minutes apart, that the filesystem is read-only. The
only record is a `WARN` line per title.

**What to build.** A destination pre-flight in the eligibility path, run once
per library root rather than once per file — the answer is a property of the
mount, not of the movie. Create and remove a probe entry in the library root
(or in the source's parent, if roots can span mounts); on `EROFS`, `EACCES`
or `EPERM`, refuse the whole batch with a reason naming the path and the
errno.

**Where.** `eligibility_reason` is pure and takes no filesystem — do not make
it impure. Add the check at the queueing site,
`work_dv_disk_queue` (`crates/plurxd/src/state.rs:2230`), before
`queue_dv_conversion` is called for any file in the batch, and surface it as
a queue-level refusal rather than 38 per-file failures.

**Reason for the batch scope:** a per-file check would still emit 38
refusals. The operator has one problem, not thirty-eight, and the product
should say so once.

**Acceptance:** with `/20t` mounted `:ro`, a scan cycle logs exactly one
refusal naming `/20t` and `EROFS`, `dv_conversions` gains no queued rows, and
the operator surface shows the library as *conversion blocked — destination
read-only*, not as thirty-eight failed titles.

### 5.2 M2 — learn the EL type before spending the extract

**The failure this prevents:** ~124 GiB written before discovering the title
was MEL.

**What to build.** A bounded head probe, run at eligibility time, using the
exact command tested in §4 — `-t 10`, the same bitstream filter as the real
extract, into the *node's own* scratch (`/tmp` or the plurx data dir),
**never** beside the source. Record `el_type` on the file's DV columns so it
is known without re-probing, and invalidate it the way every other
source-derived fact is invalidated: on size or mtime change.

Then let `el_type` participate in eligibility, driven by a per-library
setting alongside the existing `LIBRARY_DV_DISK_CONVERT` mode:

| Setting | Converts | Rationale |
|---|---|---|
| `fel-only` *(proposed default)* | FEL only | the case where discarding the EL is a real change and the output is smaller |
| `all` | MEL + FEL | when browser playability is worth the footprint regardless |
| *(off)* | nothing | today's `disabled` |

**Reason `fel-only` is the proposed default:** for a MEL title the conversion
costs 2× permanent footprint and buys only the playability. That may well be
worth it — but it should be a choice someone made, not the outcome of a check
that could not see the difference.

**Acceptance:** a MEL title in a `fel-only` library is refused with
*enhancement layer is minimal (MEL); conversion would not reduce size*, the
probe leaves nothing in the media directory, and it completes in under 5
seconds on a 68 GiB source. A FEL title in the same library queues normally.

### 5.3 M3 — make both visible from inside the product

**The rule this serves:** anything on this server that spends real hardware
must be attributable from inside the product — what it is, why it chose that
work, and a way to stop it. A `WARN` line in `docker logs` is not that.

**What to build.** On the conversion operator surface: the queue-level
refusal from M1 with its path and errno; the `el_type` per title once probed;
the projected cost before the queue runs — peak scratch and retained-original
footprint, summed for the batch, against free space on the destination; and a
stop control for the batch.

**Acceptance:** with `/20t` read-only, the reason is legible in the UI without
opening a shell. With it writable and 38 titles eligible, the UI states the
projected ~2.5 TiB of retained originals against 4.8 TiB free *before*
anything is written.

---

## 6. Non-goals

Each of these is a thing an implementer will be tempted to do. Do not.

1. **Do not change the conversion pipeline's argv or ordering**
   (`dv_disk.rs:568-648`). It is correct: stream copy, RPU rewrite, no
   re-encode. The defect is everything that fails to happen *before* it
   starts.
2. **Do not move the scratch directory off the media volume.** Commit is a
   same-filesystem rename; a scratch dir elsewhere turns it into a 68 GiB
   copy and breaks the crash-recovery guard that relies on the workspace
   sitting beside the source.
3. **Do not delete `.p7.orig` automatically**, and do not propose it as part
   of this work. It is the only rollback, the 2× footprint is the price of
   having one, and reclaiming it is a separate decision with its own
   consequences.
4. **Do not make `eligibility_reason` do I/O.** It is a pure function over
   stored columns, tested as one. The filesystem probe belongs at the
   queueing site; the EL type belongs in a stored column that the pure
   function can then read.
5. **Do not change the `:ro` bind mount from code, or recommend it as the
   fix.** Whether the media volume becomes writable is Paul's call (§7), and
   the product must behave correctly either way.
6. **Do not couple this to the P7-on-the-web effort** (#869, merged as
   `206ab3c3`). That work is about what a *browser* is handed at playback
   time and converts nothing on disk. The two meet only at M5's requirement
   for one converted artifact to exist.

---

## 7. The decision that is not an implementer's to make

**Should `/20t` become writable at all?**

Making it writable is what unblocks conversion. It also hands a pipeline that
rewrites files in place, and has never run successfully in production, direct
write access to a 19 TiB library of remuxes. The `:ro` flag is currently the
only thing standing between a bug in that pipeline and the originals.

Options, in increasing order of exposure:

| Option | Exposure | Notes |
|---|---|---|
| Leave everything `:ro` | none | conversion stays off; M1 makes that legible instead of a failure loop |
| `/20t:rw`, others `:ro` | one share | `/media`, `/8tb`, `/8t-2` stay protected |
| All shares `:rw` | whole library | no reason to do this for conversion alone |

**Recommendation:** build M1 and M2 first, keep everything `:ro` while they
land, then make `/20t` writable and run **one FEL title** end to end with the
retained original verified byte-for-byte before letting the queue drain. The
2× footprint and the in-place rewrite both argue for one supervised
conversion before thirty-eight unsupervised ones.

---

## 8. Reviewer checklist

Things this document asserts that are worth an independent look:

- [ ] §1: that `:ro` on the bind is the *only* blocker — host mount `rw`,
      dir `0777`, container uid matches. Is there a `no_root_squash` or
      NFSv4 ACL layer that would still refuse a write?
- [ ] §2: that `BL_RPU.hevc` genuinely cannot be freed before `mkvmerge`.
      Read `dv_disk.rs:612-626`.
- [ ] §3: the ~193 GiB peak. Video size is derived from bitrate × runtime,
      not measured. Check the arithmetic.
- [ ] §4: that a 10-second head probe is reliable for EL type across the
      whole library, not just the two titles tested. Is there a shape where
      the first 10 s of RPU disagrees with the rest?
- [ ] §5.2: whether `fel-only` is the right default, or whether browser
      playability makes MEL conversion worth 2× footprint after all.
- [ ] §6.2: whether the crash-recovery guard really depends on the workspace
      being on the same filesystem, or whether that is a rename optimisation
      that could be relaxed.
