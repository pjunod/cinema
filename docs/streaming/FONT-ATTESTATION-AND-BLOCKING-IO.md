# Font attestation and blocking I/O — stat off the runtime now, freeze the font environment per recipe next

**Status:** M1 merged (PR #413, in `main`) · M2 built in draft PR #553 on
`plan/S-04-2`, not deployed; fleet evidence owed · **Executes:** §2.7, F-stream-7, assessment
correction 6, §5.1 item 7 and §5.2 "frozen font environment per recipe" from
[ARCHITECTURE-REVIEW-2026-09-20.md](../reviews/ARCHITECTURE-REVIEW-2026-09-20.md)
· **Written:** 2026-09-20 against `main` @ `88a3957a` · **Updated:**
2026-09-26

**Board:** row on the [work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim there before starting; record model and session id there and in the Execution log below.

Companion to [VOD-ENCODING.md](VOD-ENCODING.md) — its paragraph "Text burn
also fingerprints active Fontconfig rules and every discoverable font
object … Any dependency or font replacement in the running daemon withdraws
the recipe with a typed engine-changed result" is the contract this plan
keeps — and to
[ENCODED-VOD-HOLD-AND-RELEASE.md](ENCODED-VOD-HOLD-AND-RELEASE.md) (fewer
spawns; this plan is about what each spawn and each segment pays). Read §2
before touching `ffmpeg.rs:1560-1880`: the re-enumeration exists to catch
font *additions*, and M1 must not stop looking. M1 is one PR and can ship
this week. M2 is a design item with its own PR. If a step seems to require
a time-based cache of the font digest, stop: that is the withdrawn remedy.

**Correction to the review:** none on the mechanism; one on scope. §2.7
says the per-segment check "reaches `EncodedEngine::is_current` … every
time, nothing memoised". True for the font half. The media half is
memoised: `FRAGMENT_INDEX_ENGINE` is a `OnceCell` (`ffmpeg.rs:1526`), and
`fragment_index_engine_is_current` only re-stats the captured objects. So
the per-segment cost for a **non-burn** encoded rendition is a synchronous
stat of the ffmpeg dependency closure (tens of files); for a **text-burn**
rendition it is that plus `fc-list` + `fc-conflist` + a synchronous stat of
every font file and config file. Both are on a tokio worker; only the
second spawns.

**Execution boundary, 2026-09-21.** M1 is built at `da0b96a7`. The required
read-only media1 probe found the deployed Jellyfin FFmpeg 8.1.2 binary linked
to its bundled `libfontconfig.so.1`, so `FONTCONFIG_FILE` is a relevant child
boundary. The same inspection found that the active `fc-conflist` closure ends
with `/etc/fonts/fonts.conf`. That file names `/usr/share/fonts`,
`/usr/local/share/fonts`, XDG and home font directories, then includes
`conf.d`; `50-user.conf` and `51-local.conf` add more live includes. Copying
those bytes into the proposed private `conf.d` and including them from the new
`fonts.conf` would reopen the mutable system and user closures. It cannot pass
§5.2's two-font isolation test, so M2 is not safe to implement from this plan
literally. The required correction must define how discovery directives are
resolved into a closed snapshot while preserving font matching rules, then
prove both isolation and glyph parity. A TTL remains forbidden.

**M2 decision, 2026-09-26.** Taken by the Claude session in the execution
log under Paul's standing delegation of reasonable design decisions, and
recorded here for him to overturn: the frozen environment is a
**Fontconfig sysroot** holding byte copies of the loaded configuration files
and links to the listed fonts at their original paths (§3.2). The
alternative §7.3 offered — an XML-resolved snapshot — was built first and
failed the host `fc-match` parity proof, because Fontconfig applies a
parent's rules at its `<include>` positions and `fc-conflist` does not
report that order. Re-verified against `main` @ `91f36315`: the line
numbers in §2 have moved (`EncodedEngine` is now `ffmpeg.rs:1826-2026`,
`font_render_engine_inner` `:2251-2326`, the per-segment checks are
`recipe_engine_is_current` at `vod/generation.rs:150-165`, called from
`spawn_generation` `:11`, `:365`, `:765` and `regenerate_init_head`
`vod/init.rs:21`, `:91`), S-05's `producer_spawn::spawn` is the one env
site (`producer_spawn.rs:180-215`), and no `FONTCONFIG_*` variable was set
anywhere in `crates/` before this change.

## 1. Objective

1. **M1 (no contract change):** the synchronous `std::fs::metadata` calls in
   `engine_objects_are_current` / `engine_path_version` and in
   `font_render_engine_inner`'s per-path loop run on the blocking pool, so
   a slow filesystem stat (a font directory on NFS, a cold page cache) no
   longer stalls a runtime worker that is also serving segment bodies. What
   is checked, when, and what a mismatch does are unchanged.
2. **M2 (design):** a text-burn recipe captures a **frozen font
   environment** — a private fontconfig configuration whose only font
   directory contains the exact files enumerated at capture and whose
   config files are copies taken at capture — and the child runs under
   `FONTCONFIG_FILE` pointing at it. Then per-segment re-enumeration is
   unnecessary because the inputs libass can see cannot change under the
   recipe; what remains per launch is a stat of the captured objects, which
   is what non-burn recipes already pay.
3. The VOD spawn's missing `configure_ffmpeg_runtime` (F-stream-13) is
   named here because it decides where fontconfig's *cache* goes; the fix
   itself is [FFMPEG-SPAWN-UNIFICATION.md](FFMPEG-SPAWN-UNIFICATION.md)'s.

## 2. Contract today

Re-verify every line number at build time; they are from `88a3957a`.

### 2.1 What a text-burn recipe attests, and when

[`EncodedEngine`](../../crates/plurxd/src/ffmpeg.rs) (`ffmpeg.rs:1557-1563`):
`digest`, `process_identity`, `objects: Vec<(PathBuf, String)>`,
`font_digest: Option<String>`.

`capture(text_burn)` (`:1565-1605`): media closure from the `OnceCell`;
`engine_objects_are_current(&media.objects)`; then, for text burn,
`font_render_engine_inner().await` (below), `engine_objects_are_current(
&fonts.objects)`, and the font digest folded into the engine digest with
`font_digest = Some(fonts.digest)`; `objects` is the union. The digest also
folds `encoded_process_identity()` (`:1620-1624`, a per-daemon UUID), so an
encoded rendition key never survives a daemon restart — which is why M2 can
change the digest scheme without a durable-cache migration.

`is_current` (`:1610-1618`):

```rust
pub async fn is_current(&self) -> bool {
    if !engine_objects_are_current(&self.objects) { return false; }
    let Some(expected_font_digest) = self.font_digest.as_deref() else { return true; };
    let current_fonts = font_render_engine_inner().await;
    font_closure_is_current(expected_font_digest, &current_fonts)
}
```

The comment above it (`:1606-1609`) is the requirement: "Fontconfig must be
enumerated again: checking only the files captured earlier detects
replacements and removals, but not additions which change font resolution."

`font_render_engine_inner` (`:1806-1885`): `fc-list --format=%{file}\n` and
`fc-conflist`, each through `bounded_command_output_with_timeout(_,
FONT_ENGINE_PROBE_TIMEOUT = 30 s)` (`:1533`) — async child processes, not
blocking; their stdout is folded into the digest; paths are collected
(`fc-list` lines starting with `/`; `fc-conflist` lines `+ <path>: …`),
sorted, deduplicated; then **for every path** `engine_path_version(&path)`
(`:1861`) — a synchronous `std::fs::metadata` (`:2041-2046`) — with the
`dev:ino:size:mtime:mtime_nsec:ctime:ctime_nsec` string (`:2057-2069`)
folded into the digest. On a desktop-class font install that is 1,000–3,000
stats; on the media1 container it is whatever the image ships.

`engine_objects_are_current` (`:1693-1699`) is the same synchronous stat
per object, compared with the captured version string.

### 2.2 Where it is paid

| Site | When | Cost |
|---|---|---|
| `vodserve.rs:6738` `spawn_generation` → `recipe_engine_is_current` (`:6862-6870`) | every producer launch | `executable.is_current()` + `engine.is_current().await` + (cluster key) `fragment_index_engine_is_current()` |
| `vodserve.rs:7483` `materialize` | **every segment**, before the manifest lock | same |
| `vodencode` recipe capture | once per recipe | `capture(text_burn)` |

At 3× realtime a 2 s segment materialises every ~0.7 s; for a burn that is
two `fc-*` spawns and the full stat loop every ~0.7 s, plus the closure
stats for every encoded rendition. Unmeasured — §5.1 adds the counter.

### 2.3 libass, fontconfig and the child's environment — precisely

The burn is `subtitles='<path>'[:si=N]` (`transcode/mod.rs:1083-1088`); no
`fontsdir`, no `force_style`. FFmpeg's `subtitles` filter hands libass the
system font provider; on Linux builds that is **fontconfig**
(`ass_set_fonts(..., ASS_FONTPROVIDER_AUTODETECT)`; `fontsdir` only *adds*
a directory on top of it). Fontconfig in the child reads:

- `FONTCONFIG_FILE` — the configuration file to load instead of
  `/etc/fonts/fonts.conf`; relative paths are resolved against
  `FONTCONFIG_PATH` (default `/etc/fonts`). The config names the `<dir>`
  list it scans, the `<include>`s it reads (`conf.d/`, and by default the
  user's `~/.config/fontconfig/...` and `~/.fonts.conf` with
  `ignore_missing`), and the `<cachedir>` list.
- `FONTCONFIG_SYSROOT` — prefix for every path; not used here.
- Cache: the config's `<cachedir>` entries, by default
  `$XDG_CACHE_HOME/fontconfig` then `~/.cache/fontconfig`, then the
  system cache dir. With neither `XDG_CACHE_HOME` nor a writable `HOME`,
  fontconfig re-scans every font directory on **every process start**.

`configure_ffmpeg_runtime` (`transcode.rs:2373-2383`) sets `XDG_CACHE_HOME`
to the daemon's runtime cache dir and `AV_LOG_FORCE_NOCOLOR=1` for exactly
this reason (its doc comment: "fontconfig has no user cache while libass
initializes a text-subtitle burn … the entire producer startup budget
rebuilding font metadata"). It is called by the rolling spawns
(`transcode.rs:2164, 2300`) and the fragment-index spawn (`fragindex.rs:1665`)
and **not** by `spawn_generation` (`vodserve.rs:6778-6786`, a bare
`Command::new(recipe_program(recipe)) … .spawn()`), which is the primary
encoded path and the only one that burns text under an immutable recipe.
So on a Docker deployment with `HOME` unset the encoded burn producer
rebuilds the fontconfig cache at every launch today. F-stream-13.

No `FONTCONFIG_*` variable is set anywhere in `crates/` (grep, 2026-09-20).

## 3. Change

### 3.1 M1 — the stats move to the blocking pool, nothing else moves

Two functions change shape; their callers and their answers do not.

```rust
/// Same check, off the runtime: a stat on a cold or remote filesystem can
/// take milliseconds, and this runs per segment on a worker that is also
/// pumping media bodies.
async fn engine_objects_are_current_async(objects: Arc<[(PathBuf, String)]>) -> bool {
    tokio::task::spawn_blocking(move || engine_objects_are_current(&objects))
        .await
        .unwrap_or(false) // a panicked or cancelled check is "not current": fail closed
}
```

`EncodedEngine.objects` becomes `Arc<[(PathBuf, String)]>` (it is cloned
into the closure once per check; today it is a `Vec` cloned nowhere, so
this is the cheaper representation anyway). `is_current`,
`fragment_index_engine_is_current` and `capture` call the async form.
`engine_objects_are_current` itself stays synchronous and is what the
blocking closure runs — the comparison logic is not duplicated.

In `font_render_engine_inner`, the per-path loop (`:1858-1875`) moves into
one `spawn_blocking` that takes the sorted `paths` and returns
`(objects, object_digests, errors)`; the digest folding after it is
unchanged. One blocking task for the loop, not one per path — a thousand
`spawn_blocking`s would trade one stalled runtime worker for a saturated
blocking pool.

`engine_path_version` is unchanged; it is the leaf.

**The executable is in the batch, not in front of it.** `recipe_engine_is_current`
(`vodserve.rs`) used to read `encoding.executable.is_current() ||
encoding.engine.is_current().await`. `EncodedExecutable::is_current` was a
synchronous `engine_path_version`, and `||` short-circuits left to right, so
that stat ran inline on the runtime worker *before* either blocking batch, at
all five call sites — every producer launch and every `materialize`, for
non-burn renditions as well as burn ones. Moving the other stats does not help
while this one is in front of them. `EncodedExecutable` now exposes
`attestation_object()` and `EncodedEngine::is_current_with_executable` folds
that `(path, version)` pair into the same `spawn_blocking` as the dependency
closure, checked first inside the task. The answer is unchanged: the batch
compares with `all`, which short-circuits on the executable exactly as the
caller's `||` did. §2.2's cost table already listed `executable.is_current()`
as part of what `recipe_engine_is_current` pays per segment, so it was always
inside Objective 1 even though the first draft of this section enumerated only
the other two functions.

Cancellation: `spawn_blocking` work cannot be cancelled once started. The
callers already tolerate that — `materialize` awaits the answer before the
manifest lock, and a dropped `materialize` future (generation superseded)
leaves at most one stat loop finishing on the pool. The `JoinHandle` is
awaited, never detached, so nothing runs unobserved.

### 3.2 M2 — freeze the font environment at recipe capture (as built)

**The idea.** Today the recipe says "the fonts I saw" and re-looks every
segment to be sure nothing was added. Instead, the recipe *builds* the font
environment the child will see, from what it saw at capture, in a directory
only `plurxd` writes, and points the child at it. Additions to
`/usr/share/fonts` cannot reach libass because the child never scans
`/usr/share/fonts`; it scans the recipe's copy of the tree. Removals and
replacements of the underlying files are still caught by the per-launch
stat of the captured objects — the same check non-burn recipes pay.

**Decision (2026-09-26, Claude, under Paul's delegation; overturnable): a
Fontconfig sysroot, not a rewritten configuration.** §7.3 named two ways to
keep live `<dir>`, `<include>` and `<cachedir>` directives from reopening the
system and user closures: an XML-aware resolved snapshot, or a proved
sysroot layout. The first was built and failed its own proof. Fontconfig
applies a file's rules in document order with each `<include>` expanded *in
place* — at every include it flushes the rules read so far, then parses the
included files (`fcxml.c`, `FcParseInclude`: "flush the ruleset into the
queue") — while `fc-conflist` reports files in the order they *finish*
loading, root last. A snapshot that strips the includes and replays the
copies in `fc-conflist` order therefore runs `fonts.conf`'s own rules after
`conf.d`; on the build host that turned `fc-match mono` from DejaVu Sans Mono
into DejaVu Sans. Reproducing the real order from outside means
reimplementing include resolution (search path, `~`, XDG prefixes, directory
sorting, real-path deduplication). Under `FONTCONFIG_SYSROOT`, Fontconfig
does all of that itself, prefixing every path it resolves with the sysroot —
so byte-for-byte copies at the original paths give the same include tree and
the same rule order by construction, and no XML is parsed at all. It is also
the smaller design.

**Layout**, under the runtime cache (`TranscodeManager::runtime_cache`),
which `plurxd` already owns:

```text
<runtime_cache>/fontenv/<digest16>-<n>/
  root/            FONTCONFIG_SYSROOT
    <path of every "+" file fc-conflist reported>   byte copy
    <path of every file fc-list reported>           symlink → that file
    …              caches fc-cache writes (the configs' own <cachedir>s,
                   resolved under the sysroot)
  manifest.json    font_digest, sysroot, root config, and for every entry
                   its original path, attested version, frozen path, kind
```

Symlinks rather than copies for fonts, as §7.1 decided: the per-launch stat
goes *through* the link, so an in-place replacement of the target (or a
repointed link) changes the captured version. Fontconfig strips the sysroot
from `FC_FILE` at scan (`fcdir.c`), so libass opens the original path — the
same file the link names and the stat attests. Configuration files are
copied: a rule edit under an existing recipe must be invisible to it, and it
is, because the child reads the copy.

**Capture** (`EncodedEngine::capture(Some(runtime_cache))`,
`crates/plurxd/src/fontenv.rs`): the live `fc-list` / `fc-conflist`
enumeration is unchanged in purpose (it is still how the environment is
*discovered*; `fc-list` now prints one line per face). Then, on the blocking
pool, every loaded configuration file is read and required to still be the
version the enumeration attested, and every font link is statted through and
required to match; then `fc-cache` builds the environment's cache so no
launch pays a scan. Two parity proofs run before the recipe exists, each
under exactly the child's variables:

1. `fc-conflist` must load the same configuration files **in the same
   order** as the live one. Same bytes at the same paths under the same
   resolver is the same include tree; a divergence (a missing copy makes a
   required include fail and Fontconfig fall back to a rule-less default) is
   refused.
2. `fc-list` must list the same faces (file, index, family, style).

Either failing refuses the burn with `vod_engine_unattested`, rather than
rendering with other fonts or rules. The engine digest folds the frozen
digest (`plurx/font-render/engine-v2\0` + the live closure's digest + the
root configuration's path), so a v1 digest can never equal a v2 one.

**Launch** (`vod/generation.rs` `spawn_generation`, `vod/init.rs`
`regenerate_init_head`): `recipe_child_env` passes `FONTCONFIG_SYSROOT=<root>`
and `FONTCONFIG_FILE=<the root configuration's original path>` through the
unified `producer_spawn` builder that S-05 landed. The original absolute path
is used deliberately: Fontconfig prefixes it with the sysroot once in every
version that supports `FONTCONFIG_SYSROOT` (2.13.1 and later), whereas an
already-prefixed path is double-prefixed by versions without the
"avoid adding sysroot repeatedly" guard.

`is_current` for a frozen recipe is one `spawn_blocking` stat batch over the
environment's objects — every copy, every link (through it), and every
directory holding one (so an entry added *inside* the sysroot is caught) —
plus the media closure's batch. **No `fc-list`, no `fc-conflist`.** The
per-segment call in `materialize` stays, and is now the cheap stat. The
`font`/`spawn` attestation series is charged at capture only.

Why this is not the withdrawn TTL: a TTL would keep answering "current"
for 60 s while a newly installed font changed what libass resolves. Under
the frozen environment a newly installed font is **not visible to the
child at all**; there is nothing to detect, so not looking is correct. A
font *removed* or *replaced* behind a link is detected by the target's
version, per launch and per segment, as today. A config edit is invisible
to the child (the copy is what it reads); the copy's own version is
checked so tampering with the copy withdraws the recipe.

**Lifecycle.** Environments are shared per `(runtime_cache, frozen digest)`
through a process registry of weak references: a second recipe captured from
the same closure reuses the first one's environment after re-statting it. The
last recipe to drop its `Arc` removes the directory (on the blocking pool),
and the first capture in a process removes every `fontenv/` directory a
predecessor left — every encoded key dies with its process anyway (§7.4:
ownership, not age).

**Known limit, stated.** Capture proves parity with the *system* `fc-*`
tools; the producer links whatever libfontconfig its ffmpeg build bundles
(Jellyfin FFmpeg bundles its own on media1). That was already true of the
live enumeration before M2. A bundled library that ignored
`FONTCONFIG_SYSROOT` (older than 2.13.1) would read the live root config;
the fleet step in §5.2 checks the deployed binary directly.

**Windows.** libass on the Windows build uses DirectWrite, not fontconfig;
`font_render_engine_inner` already returns `usable = false` there (no
`fc-list`), so text burn is refused on Windows today and this plan does not
change that.

## 4. Guardrails (non-goals)

- **No time-based cache of the font digest**, anywhere, in either
  milestone. Assessment correction 6; §0 of the review.
- **M1 changes no answer.** Same functions compare the same strings; a
  test asserts `is_current()` flips to `false` when a captured font file's
  mtime is touched, before and after M1.
- **M1 does not remove the per-segment check.** That is M2's, and only
  because M2 removes the reason for it.
- **M2 does not skip the per-launch stat.** "Verified once per launch" is
  the objective's text; the per-segment stat also stays because it is
  cheap and it is the fence before publication (VOD-ENCODING.md: "every
  object identity is rechecked before spawn and publication").
- **Recipe identity changes in M2 and this is stated:** the engine digest
  scheme moves to `v2`. Nothing durable is invalidated beyond what a
  daemon restart already invalidates, because the digest already contains
  the per-process UUID. The *burn sidecar key* (which includes the recipe
  identity) also changes on restart today; no migration.
- **Not a shared system-wide fontconfig override.** `FONTCONFIG_SYSROOT`
  and `FONTCONFIG_FILE` are set on the child's `Command` only; the daemon's own environment and every
  non-burn child are untouched (the same locality rule
  `configure_ffmpeg_runtime` states for `XDG_CACHE_HOME`).
- **No `fontsdir`.** It is additive on top of fontconfig; it cannot exclude
  the system directories, which is the property M2 needs.
- **The rolling (non-VOD) burn path is out of scope.** It has no immutable
  recipe; a font change there changes the next segment and that is
  acceptable for a live transcode. It gets the frozen environment only if
  the spawn-unification plan routes it through the same builder later.

## 5. Milestones

### 5.1 M1 — blocking stats off the runtime, with a counter

Code: §3.1. Tests in `ffmpeg.rs`:

| Test | Asserts |
|---|---|
| `a_touched_engine_object_is_not_current_after_the_move` | `capture_test_objects` on two temp files; touch one's mtime; `is_current().await == false`. Same test body as before M1 (it exists in spirit at `capture_test_objects`, `:1627`); the move must not change it |
| `a_current_engine_is_current_on_the_blocking_pool` | with `tokio::runtime::Builder::new_current_thread().max_blocking_threads(1)`, `is_current()` completes while a second `spawn_blocking` is queued behind it — proves the stat is on the pool, not the runtime thread |
| `font_object_versions_are_computed_in_one_blocking_task` | instrument with a test-only counter of `spawn_blocking` calls inside `font_render_engine_inner`: exactly 1 for N paths |

| `a_stale_executable_is_detected_on_the_blocking_pool` | with the one blocking thread held, a check whose executable has been replaced must not finish — an inline `std::fs::metadata` would answer `false` from the runtime thread |
| `one_attestation_charges_each_series_once_by_what_it_stats` | a non-burn check charges `[(media, stat)]`; a burn check charges `[(font, spawn), (font, stat) × 3 batches, (media, stat)]` — its dependency closure under `media`, and its three internal font stat batches folded into one observation |

Metric, rendered in `system::metrics` beside the VOD gauges:
`plurx_engine_attestation_seconds{kind="media"|"font",phase="spawn"|"stat"}`
— a histogram with the repo's existing bucket set. Four series. Reason: §2.7's
cost is "real but unmeasured"; this is the measurement, and after M2 the
`font`/`spawn` series must go to zero for frozen recipes.

**Two rules the series must obey to be readable as a before/after.** §2.7's
cost is the sum of two separable things — the ffmpeg dependency closure, and
for a text burn that plus the Fontconfig closure — so:

1. **The label is what is statted, not what kind of recipe it is.** A burn
   recipe's `objects` used to be the union of both closures and the whole
   batch was labelled from `font_digest.is_some()`, so on a burn rendition the
   dependency-closure stats were counted under `kind="font"` and
   `kind="media",phase="stat"` recorded nothing at all. `EncodedEngine` now
   keeps `objects` (the dependency closure) and `font_objects` apart and each
   batch is charged to its own series.
2. **One observation per attestation, not per internal batch.** One
   `is_current()` on a burn recipe runs three font stat batches — the captured
   font objects, the re-enumeration's own stat loop, and the enumerated
   closure — and used to emit three `font,stat` observations. `_count` then
   read 3× the number of checks, so `_sum / _count` was not the mean cost of a
   check and M1's "before" was a 3×-inflated number against M2's 1× "after".
   `EngineAttestationCharges` sums the batches per `(kind, phase)` and flushes
   one observation each.

Also worth knowing when reading them: `ENGINE_ATTESTATION_BUCKETS_MS` starts at
100 ms, so every warm-cache stat lands in `le="0.1"` and the histogram carries
no information about the regime that dominates. Only `_sum / _count` is usable,
which is why rule 2 matters.

Acceptance: `cargo test -p plurxd ffmpeg::tests::.*current.*
ffmpeg::tests::.*font_object.*` green; `make unit` green; on lab4 with a
text-burn session running, `curl -s :32400/metrics | grep
plurx_engine_attestation_seconds_count` shows `font,spawn` incrementing
about once per materialised segment (the "before" for M2).

### 5.2 M2 — frozen font environment per recipe

Code: §3.2. Tests, all in `crates/plurxd/src/fontenv_tests.rs` and all run by
`cargo test -p plurxd fontenv::` (none ignored — they need Fontconfig's
tools and ffmpeg with libass, which text burn needs at runtime and the CI
runners have). As built, against the table this section first proposed:

| Test | Asserts |
|---|---|
| `a_frozen_environment_names_only_the_captured_fonts` | a two-font "system" config; capture; install a third font (the live config now lists three); `fc-list` spawned **through `producer_spawn`** with the recipe's `child_env()` lists exactly the two captured files; a non-burn recipe's `child_env()` is empty. (Absorbs the proposed `the_child_command_carries_fontconfig_file`.) |
| `a_new_system_font_does_not_withdraw_a_frozen_recipe` | after the addition, `is_current` is `true` and one check charges exactly `[(font, stat, 1), (media, stat, 1)]` — no `font,spawn` |
| `a_replaced_font_file_still_withdraws_the_recipe` | appending a byte to a captured font: `false` |
| `an_edited_config_copy_withdraws_the_recipe` | appending to the frozen copy of a rule: `false` |
| `a_deleted_config_copy_withdraws_the_recipe` | replaces `missing_conf_d_copy_is_a_hard_failure`: Fontconfig does not hard-fail a missing required include, it falls back to a rule-less default, so the stat fence refuses it before launch |
| `an_edited_system_config_does_not_reach_a_frozen_recipe` | a `PlurxProbe` alias rule; editing the *source* rule moves live `fc-match` to the other family while the frozen `fc-match` keeps the captured one, and `is_current` stays `true` |
| `a_frozen_host_environment_matches_and_renders_like_the_live_one` | the §7.5 proof on the host's real distro configuration: 18 `fc-match` patterns (generics, `mono`, metric aliases, `:lang=`, pixel sizes) give the same face, file and rendering properties (hintstyle, antialias, rgba, lcdfilter, embolden) under the live and frozen configs; an ASS script with three styles burnt by ffmpeg's `subtitles` filter gives the same `framemd5` under both, and a different one from an unburnt frame (so the burn is not vacuous). Not ignored. |
| `a_frozen_environment_that_loads_other_rules_is_refused` | the rule-order parity refuses a reordered or fallback `fc-conflist` |
| `a_frozen_environment_that_resolves_other_faces_is_refused` | the face parity refuses a missing or restyled face, and reads sysroot-prefixed and stripped paths alike |
| `recipes_frozen_from_one_closure_share_it_until_the_last_is_released` | two captures share one environment; it survives the first drop and is removed after the last |
| `a_process_clears_environments_its_predecessor_left` | a stale `fontenv/<id>` is removed by the first capture of a process |

Acceptance: those green; `make unit` green; `make vodencode-restart-check`
green; on lab4 or media1 with a text-burn session,
`plurx_engine_attestation_seconds_count{kind="font",phase="spawn"}` rises by
one capture's worth when the session starts and then stays flat for the
session's life while `phase="stat"` increments per segment; `pgrep -f
'fc-(list|conflist)'` during playback finds nothing.

GPT prompt for the fleet check after deploy:

> On media1 after PR #553 is deployed. (1) Find the runtime cache: `docker
> exec <plurx container> sh -c 'find / -xdev -type d -name fontenv
> 2>/dev/null'` (it is `<cache root>/runtime/fontenv` once a burn has run).
> (2) From the web client start any title with a *text* subtitle track burnt
> in (choose a quality that transcodes). While it plays, run inside the
> container `for i in $(seq 30); do pgrep -c -f 'fc-(list|conflist)'; sleep 2;
> done` and paste the output (expect zeros after the first few seconds), and
> `curl -s localhost:<port>/metrics | grep 'plurx_engine_attestation_seconds_count{kind="font"'`
> twice 30 s apart (expect `phase="spawn"` unchanged between the two reads
> and `phase="stat"` increasing). (3) `ls <fontenv>/` and `head -60
> <fontenv>/*/manifest.json`. (4) Prove the deployed ffmpeg's bundled
> libfontconfig honours the sysroot: with `R=<fontenv>/<dir>/root` and
> `C=$(python3 -c 'import json,glob;print(json.load(open(glob.glob("<fontenv>/*/manifest.json")[0]))["config"])')`,
> run `FONTCONFIG_SYSROOT=$R FONTCONFIG_FILE=$C FC_DEBUG=1024
> /usr/lib/jellyfin-ffmpeg/ffmpeg -v error -f lavfi -i color=c=black:s=480x270:d=1
> -vf "subtitles=/tmp/probe.ass" -frames:v 1 -f framemd5 - 2>&1 | grep -E
> 'Loading config file|^0,'` with any small `.ass` file containing a
> Dialogue line, and paste it: every `Loading config file from` line must
> start with `$R`, and the frame hash must equal the same command run
> without the two variables. (5) Copy any `.ttf` into
> `/usr/local/share/fonts/` inside the container, run `fc-cache`, confirm the
> session keeps playing and the journal has no `immutable media engine
> changed` line for it. (6) Stop the session, start it again, and confirm a
> new `fontenv/` directory appears whose manifest lists the new font (the
> first one is removed when its last rendition is purged, or at the next
> daemon start).

## 6. Verification and rollout

- Focused: `cargo test -p plurxd ffmpeg::tests` (both), plus `make
  vodencode-restart-check` for M2 because it changes what a restart reads.
- Lane: the one plan PR's ready-state fast lane runs `make unit`; this
  execution used focused tests only while the PR remained draft.
- Rollout: one draft plan PR, with M1 and the M2 decision as logical commits
  under the current workboard protocol. M1 has no identity change. When its
  corrected design is implementable, M2 stays in that same PR and changes the
  engine digest prefix (`v2`); the deploy's restart already invalidates every
  encoded key, so the only visible effect is the `fontenv/` directory appearing
  under the runtime cache. No setting; the frozen environment is not optional
  because an optional one would mean two attestation contracts.
- Rollback: revert; `fontenv/` directories are cleaned by the runtime-cache
  startup sweep. A rolled-back binary ignores them.
- Dependency: M2's two variables need the VOD spawn to have a place to set
  environment. [FFMPEG-SPAWN-UNIFICATION.md](FFMPEG-SPAWN-UNIFICATION.md) M1
  landed first, so they go through `producer_spawn::SpawnOptions::env` — the
  one env site — from both encoded spawns.

## 7. Decisions and the remaining M2 blocker

1. **Keep symlinks for font bytes.** The existing attestation includes target
   `ctime`, so a same-size, preserved-mtime replacement withdraws the recipe
   before another launch or publication. Copying hundreds of megabytes per
   digest would hide replacement rather than report it.
2. **Use fontconfig, but do not claim that linkage proves isolation.** On
   media1, `/usr/lib/jellyfin-ffmpeg/ffmpeg` 8.1.2-Jellyfin resolves
   `libfontconfig.so.1` from `/usr/lib/jellyfin-ffmpeg/lib/`. This settles the
   provider question only.
3. **Freeze every rule scope capture observed.** System and user rules both
   affect pixels and may not be silently dropped. Their live `<dir>`,
   `<include>` and `<cachedir>` discovery directives must not survive in the
   child configuration, however. The corrected design needs an XML-aware
   resolved snapshot or a proved fontconfig sysroot layout; line filtering is
   not an acceptable parser. **Decided 2026-09-26: the sysroot** (§3.2) —
   the directives survive byte-for-byte but can only resolve inside the
   sysroot, and the XML snapshot was shown to reorder rules.
4. **Use reference ownership, not age, for cleanup.** The eventual
   `fontenv/<digest>` owner is shared by recipes with that digest, released
   with the last rendition reference, and backed by a new-process startup
   sweep. A count/age cap could remove inputs from a live immutable recipe.
5. **M2 was blocked on the configuration representation; no longer.** The
   amended design demonstrates, as tests that run on every Rust lane, that a
   two-font source enumerates only those two fonts to the child
   (`a_frozen_environment_names_only_the_captured_fonts`) and that default
   and frozen `fc-match` and rendered glyph hashes agree on the host's real
   configuration (`a_frozen_host_environment_matches_and_renders_like_the_live_one`),
   and every capture re-proves rule-order and face parity before a recipe
   exists. What remains is fleet evidence (§5.2's GPT prompt), chiefly that
   Jellyfin FFmpeg's bundled libfontconfig honours `FONTCONFIG_SYSROOT`.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit
trailers `Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M1 | [#413](http://192.168.4.7:3000/noirr/plurx/pulls/413) | Built at `da0b96a7`: identical object-version comparison now runs in one blocking task per batch and fails closed; four attestation histogram series are rendered. Rust 1.97.1 check and Clippy passed; six focused currentness, blocking-task and metric tests passed. Broad `make unit` was deliberately not run. |
| 2026-09-21 | gpt-5.6-sol | agent:/root/p01_builder | M2 decision | [#413](http://192.168.4.7:3000/noirr/plurx/pulls/413) | Not implemented. media1 links fontconfig, but its captured config closure contains live system/user discovery directives, contradicting the proposed byte-copy isolation. Needs the §7.5 design correction and its two-font isolation proof. |
| 2026-09-22 | claude-opus-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M1 review fixes | [#413](http://192.168.4.7:3000/noirr/plurx/pulls/413) | Both adversarial-review findings fixed. The recipe's encoder executable is folded into the engine's blocking batch instead of being statted inline ahead of it at all five `recipe_engine_is_current` call sites; the four attestation series are labelled by what each batch stats and charged once per attestation instead of once per batch. `a_stale_executable_is_detected_on_the_blocking_pool` and `one_attestation_charges_each_series_once_by_what_it_stats` both fail on revert. Full `cargo test -p plurxd --bin plurxd`: 2542 passed, 0 failed. |
| 2026-09-26 | claude-opus-5-5 | https://claude.ai/code/session_01AZemhL7Y1nXGWxUGRC2tkK | M2 | [#553](http://192.168.4.7:3000/noirr/plurx/pulls/553) | Built at `b89c3a0d` from `main` @ `91f36315`: a text-burn recipe freezes a Fontconfig sysroot (byte copies of every loaded config, links to every listed font) and its producer runs under `FONTCONFIG_SYSROOT`/`FONTCONFIG_FILE`; `is_current` is one stat batch with no `fc-*` child. Design decision recorded in §3.2/§7.3: the XML-snapshot alternative was built first and failed `fc-match mono` on the host because Fontconfig applies rules at `<include>` positions. 11 `fontenv::` tests plus the updated `ffmpeg::tests` pass; each production hunk was reverted and its test failed (see PR body). **needs:** fleet evidence per §5.2's GPT prompt, including that media1's bundled libfontconfig honours the sysroot. |
