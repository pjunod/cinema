# macOS video processing — execution status and decisions

**Status:** follow-up implementation active; prior contribution landed · **Updated:** 2026-10-09 · **Owner:** managing agent with
three GPT-6.1 Sol builders · **Integration:** `effort/video-processing-followups`.

Companion to the [design](MACOS-VIDEO-PROCESSING-DESIGN.md) and
[implementation plan](MACOS-VIDEO-PROCESSING-IMPLEMENTATION.md). This page is
the execution ledger: current work, evidence, decisions and remaining work.
A plan or compiled change is not hardware qualification. The
[experiment evidence](MACOS-VIDEO-PROCESSING-EVIDENCE.md) retains exact
measurements, negative results, provenance and reproducible raw receipts.

## 1. Current position

On 2026-10-08 the user explicitly promoted GPU subtitle compositing,
non-Mac Profile 5 hardware decoding and the caption-bearing file-VOD
VideoToolbox failure into active implementation. These are now required
work, not conditional follow-ups. The accepted PR #904 source is an ancestor
of Batch03's main landing `1088d7529401297bddced74bfa11c166457468d8`.
This separate effort began at reviewed combined source
`b8461f30f5ae6f69c32ec671a81f3d1aedb4f219` and is integrating that landing.

| Follow-up | Sol 6.1 owner | State | Remaining evidence |
|---|---|---|---|
| F1 GPU subtitle compositing | Native builder | Bitmap API, all eight complete-graph raw comparisons and final text API pass; inactive font-rule identity repair passes on the signed `e9fdc02a` release | Measure performance and preserve final qualification through integration |
| F2 non-Mac P5 hardware decoding | Route builder | Neutral provenance integrated; full Bookworm FFmpeg and matching static parser builds pass; exact `e9fdc02a` Linux check/Clippy pass | Shipping package, amd64 daemon, runtime driver/graph observation and real encoder/size/rate envelope |
| F3 caption-bearing VOD VideoToolbox | Dolby/native builders | All three file-VOD cases pass on the final `2139fa9c` signed bundle, including caption payload/timing checks and clean owned shutdown | Preserve this evidence through final integration; broader DVR and client presentation qualification remain tracked separately |

Builders use separate owned clones. Native owns `0004`, compositor/planner
integration and shared package helpers; Dolby owns `0003`, caption delivery
and the shared finite-VOD source-clock repair;
Routes owns Linux runtime/strict context and shared pipeline integration.
Dolby also owns the Linux SDK, shipping assembly and sealed-parser packaging.
The manager owns this ledger and serializes shared file/compiler ownership.
Accepted earlier packages and evidence remain read-only. The pinned Rust
1.97.1 native and Linux compiler lanes each have one job and an explicit
owner; independent lanes run concurrently with the isolated SDK build.
Unit execution and main landing remain with the designated merge coordinator.
No final review or implementation completion is claimed for F1–F3 yet.

**Latest checkpoint:** the producer/parser build evidence uses
`354bd87e5f4356ae677bc16c67672985dd58f261`; subsequent packaging repairs
retain those exact compiled inputs and their original build authority.
The `e9fdc02a` font-identity repair passes pinned Rust 1.97.1 all-target compilation
and the normal commit hook (catalog, formatting, Clippy and 86 embedded
JavaScript checks). The repaired release build and focused ordinary-API
qualification both pass. Exact-source Linux check and Clippy also pass in
a 192.074-second phase. Subsequent changes affect build packaging and
documentation. No unit suite has run; final review has not started.

The earlier `2139fa9c` release continuation finishes in 1,991.254 seconds,
with executable and matching arm64 debug symbols captured within 1,992.202
seconds. Its owned process group is absent. That signed bundle passes all
eight complete subtitle graph comparisons, normal text API delivery, and
all three caption file-VOD cases. The evidence archive records the exact
bundle and original failures; it does not qualify the later Rust repair.

The first text API attempt selected CPU processing with
`runtime_probe_failed`. A measurement-wrapper assertion had restored the
build prefix before its child finished, exposing 44 inactive Fontconfig
rules. All 23 loaded rules and 394 consumed font/rule object versions were
unchanged. Hashing the full listing nevertheless changed the font identity.
The corrected wrapper settles its child before restoration; ordinary text
API delivery then passes in 70.911 seconds with matching startup and Source
font digests, native graph selection, and cue/control/parser/shutdown checks.

The committed application repair hashes only ordered active configuration
rows under a new identity domain. It retains font, tool and active-file
checks. Its focused release qualification passes in 86.630 seconds:
inactive rules change from zero to 44, a fresh Source profile retains the
startup digest, and native GPU text delivery passes all 25 cue and lifecycle
assertions. Performance remains separate: three admissions declined competing
processes before starting any timing child. The latest refusal found an
active Clippy compiler with normal system memory pressure and about 21 GB
reclaimable memory. Benchmark contention, rather than memory exhaustion,
prevents an uncontaminated timing claim.

The earlier `2cd6000b3` Mac release completed in 1,590.286 seconds with its
owned process group fully reaped. Matching executable and debug symbols are
preserved. The initial capture rejected Cargo's debug-symbol symlink; a
separate capture verified its contained regular target and matching UUID.
That capture correction does not change the successful compiler result.
Warning-level pressure did not abort the build; final source changes still
require the new release. Linux check and Clippy passes on earlier sources
are retained as historical evidence, not substituted for current inputs.

The SDK generator has passed the genuine FreeType metadata target and
Vulkan-Headers export consumed by Vulkan-Loader. Its next missing input was
Fontconfig's `gperf` bootstrap program. The committed repair supplies an
authenticated Bookworm binary, copyright and source offer through the
private tool owner. After a recorded raw-lock versus staged-record refusal,
`4481f8e9` normalizes the comparison without weakening archive, module or
license checks. The full actual SDK generator now passes in 92.66 seconds.
Producer preparation passes. Configuration exposed a missing link-time
search path for dependencies of SDK shared libraries; the repair adds
verified SDK `rpath-link` paths while preserving deployment RPATH and all
features. The next configure attempt passes that dependency search and the
affected Chromaprint, FDK, Fontconfig and FreeType probes. It then identifies
a missing Fribidi public header generated by the authentic upstream Ninja
target. A bounded preprocessing pass covers all 38 enabled public-header
groups and identifies 36 passes and two failures. Fribidi is a confirmed
missing upstream-generated output. The initial Opus diagnostic omitted the
producer's `--define-prefix`; the focused production query also fails and
establishes incorrect multiarch prefix resolution. The source repair
generates Fribidi's declared header and projects unchanged authenticated
Debian metadata to the standard prefix-relative lookup depth. All 30 source
metadata files have the required `/usr` prefix and no destination collisions;
their original paths and source associations remain recorded. Fresh
generation now passes on `829bf9a9` in 91.92 seconds, producing all 514
declared outputs. The two repaired Fribidi/Opus preprocessing checks also
pass against the actual generated SDK. Full producer configuration passes
those dependency stages, then reveals that uppercase `PKG_CONFIG` does not
select FFmpeg's lower-case configure tool variable. The repair supplies
verified GCC/G++ and the intended pkg-config command through configure's
actual command-line options in `e815c4b8`. The next configure attempt passes
those tool and metadata stages, then fails because the temporary container
lacks `libnuma.so.1`. Auditing all 14 system-library providers finds exactly
three missing base packages: `libnuma1`, `libelf1` and `libpng16-16`. The
shipping Docker stage inherits these through the pinned Jellyfin runtime
package; they belong to the container base, not new SDK roles. Their exact
binary packages and source offers are verified against the pinned Debian
snapshot. The isolated installation passes in 3.023 seconds, changes only
those three package rows and resolves the recursive/versioned closure of
197 ELF objects. The SDK remains unchanged. The next configure passes x265,
then exposes a separate development dependency: ZVBI metadata links `-lpng`,
but the SDK does not yet include the authentic `libpng-dev` linker alias.
The combined audit covers 85 metadata files and 68 link names. The repair
adds the pinned authentic development package to the existing Docker compiler
base, with no SDK role or output change. Its isolated installation passes
and the next configure passes those dependency probes. It then identifies a
missing `clang` executable in the diagnostic container. The shipping Docker
compiler stage already declares Clang. After a complete inventory audit,
27 authenticated packages restore the declared compiler base; versioned
package dependencies, Perl's API provider, exact package deltas and six
tool identities pass verification. Configuration then passes in 16.692
seconds with all 57 official options and the complete patch set retained.
The full compile uses normal Make output within the existing 8 MiB log
bound; forced verbose commands are unnecessary because actual configuration
and command evidence are retained separately. Runtime base parity
and SDK development closure are separate requirements.
The full producer compiles successfully in a 475-second phase on `354bd87e`,
with all features retained and no OOM under the fixed container limits.
Installation also passes. The matching sealed parser passes in 170.238
seconds: it is static, enables AC-4, and uses the same source and patch set.
The retained build records and all four actual log hashes are independently
verified in [evidence §37](MACOS-VIDEO-PROCESSING-EVIDENCE.md#37-full-bookworm-producer-and-matching-static-parser-build).
Final package auditing is checking the actual redistributed static system
libraries and their source records. The amd64 parser packaging path now
automatically verifies the installed static archives against authenticated
Debian members and retains their matching source offers. A supplement mode
can attach those records to the existing compiled parser without changing
its binary or original build recipe. Its 22 public inputs are size/hash
verified, and the actual offline supplement and explicit verification pass.
The first assembly then fails before ELF audit because a recursive copy
follows SDK scratch-tree aliases: some point to deliberately unbuilt library
outputs and others to host build tools. The repair copies regular provenance
bytes and records all original link members/targets as hash-bound metadata.
It neither invents libraries nor copies host tools through those aliases.
Assembly retains the original compilation recipe and all three verified
build logs, while identifying its corrected packaging recipe separately.
The failed package remains preserved. Corrected assembly, ELF audit and
hardware/API qualification remain pending. No global installation or network
fallback substitutes for dependencies.

Final Linux hardware acceptance also requires an actual amd64 daemon;
ARM Linux type/lint checks alone cannot supply that executable. A warm
build-cache candidate and bounded build are prepared. Automatic approval
review rejected transferring the 54.2 MB committed-source archive to the
build node; explicit user approval for that payload and destination is
pending. Nothing from that archive has transferred or started. The
independent package and hardware preparation continue on their existing
inputs. Performance measurements, shipping Linux runtime qualification,
final adversarial review and merge handoff remain outstanding.
The designated merge coordinator owns unit execution and main landing.

**Complete subtitle graphs:** all eight frozen SDR/PQ, text/bitmap and
resolution workloads now pass, with zero CPU/GPU pixel difference, equal
120-frame timestamps and exact expected blank frames. Text cases were not
replayed after passing. The bitmap diagnostic independently observed equal-
timestamp heartbeat/display events; the pinned one-frame-next scheduling rule
makes frames 7–18 active for the frozen 24 fps fixture. The source-derived
oracle reassesses retained per-frame evidence and applies only to that bound
fixture/rate. Both original oracle failures remain preserved. A combined
ledger binds the separate executions to identical package, font, source,
cue and plan identities; it does not pretend they were one run. Performance
measurements remain pending a quiet window.

**Integrated source:** `84ad32b51` prepares pinned Linux sources and generic
strict Dolby patches; `622c1ee42` repairs caption SEI parsing; `a9e0f2c98`
binds strict processing to typed Mac/Linux provenance. The latter preserves
existing Mac identity bytes and adds Linux executable, source, linked-library,
driver and kernel/device bindings. It grants no Linux route by itself. Normal
compile/lint hooks pass; regression sources are retained for final validation.

**GPU subtitles:** the predeclared oracle requires at most two code values
of difference from complete native-scale/CPU-burn output, and byte identity
in cue-free frames. A colored BGRA flattening approach failed with maximum
luma/chroma error 4/11 and 263 chroma samples above tolerance. An affine
foreground/coverage approach passed two layers across 256 backgrounds but
failed ten translucent layers with maximum error 7 and 5,258 samples above
tolerance. Both negatives remain recorded; tolerance was not relaxed.

The replacement preserves ordered libass masks/colors, FFDraw integer
rounding, shaping/fonts and the input frame clock. A typed refcounted command
payload travels on a secondary frame clone; it never reads main video pixels
or modifies shared pixel-buffer attachments. The existing overlay owner
executes the ordered GPU composition with one writer per chroma sample.
Its first actual hardware comparison passes: all 24 frames of ten overlapping
translucent colored text layers are byte-identical to the CPU reference on
both planes, with exact PTS. Input is 320×180 and output 160×90. This is
correctness evidence, not a performance or full lifecycle result.

A subsequent no-cue control exposed inconsistent hardware-frame pool
ownership: a cloned blank main frame did not belong to the overlay's declared
output pool. The repair makes active output allocation and blank clones share
the captured main pool, retaining download validation. Error teardown then
exposed the context's missing first-field `AVClass` slot; cleanup must not
interpret that class pointer as an owned device reference. The repaired animated control now passes all 24 frames with exact pixels and
PTS, including blank→active→blank. Its first attempt exposed a separate
clock mismatch: rounded milliseconds differed from incumbent libass's
truncation every third frame. Both paths now share the existing conversion;
tolerance remains unchanged. Candidate FFmpeg is
`1657bda3830d667ad3d4cbdb57422117f3b31da297633f1d632c37e4ff2f2a2b`.
Colored bitmap subtitles then exposed a last-row/column chroma-alpha
mismatch (maximum error 15, luma exact). Matching the incumbent CPU boundary
arithmetic repairs it without relaxing tolerance. The repaired 24-frame
colored/odd case is byte-identical, and additional top-left odd-position,
final-column and final-row controls pass. Candidate is
`71fcb4d4bc88ffae930bf2611e6e7de3cbe623e889fec6efe854a63d621c71ba`.
Bounded native-child seek, EOF and SIGINT controls pass frame/PTS comparison
and cleanup. They do not establish normal-daemon cancellation or source-position
cue correctness: input seeking resets timestamps for both CPU and GPU text
rendering, so a matched result may still display the wrong source cue. Normal
API confirmation and a shared timing-owner repair, if reproduced, remain
required before seek acceptance. Production-graph checks remain open. Generic BGRA overlay
behavior is outside the new ASS/YUVA modes and has no new qualification claim.

**Caption-preserving hardware encode:** the repaired baseline binary is
`9c9e91054999531e7543f603ca4d4a6db32a6399f92f9b283b245ae0d104c3ea`.
All seven direct controls pass on public reordered HEVC/AC-4 and original
MPEG-2/A53 inputs. Default H.264 VideoToolbox encoding preserves 48 caption
records and their normalized presentation timestamps; caption-off and
caption-free controls contain none; copy/remux retains all 48. Decoded image
planes are byte-identical with forwarding on/off. HEVC encoding still yields
zero A53 records, a separately recorded pre-existing limitation. The first normal API run on the coherent new package delivered 120 video/audio
frames and 120 caption-bearing frames, but failed the stricter source-payload
comparison. Almost all records contained padding; only two 608 tuples and three
valid 708 tuples remained. That exposed the shared finite-VOD clock defect:
the captured producer command fed source PTS near 66,272 seconds into
`fps` starting at zero. A bounded reproduction consumes two source frames
while producing 17 scheduled observations with the same picture checksum.
The integrated repair normalizes the held source clock while preserving A/V
offsets and already-normalized subtitle sidecars. Direct controls match an
independently normalized reference in all 17 video observations and 144 audio
frames, including a seek and a 175 ms operator audio adjustment. Corrected
normal-API delivery now passes the three cases detailed below.
The initial command observer missed descriptor-based input; its corrected
capture supplied the decisive graph evidence. Bounded shutdown required forced termination; no owned child
remains, and that debug failure stays recorded separately from the release
lifecycle result below. The
[caption record](VIDEOTOOLBOX-CAPTION-VOD-FOLLOWUP.md) explains the root cause.

**Intel hardware P5:** deployed Jellyfin lacks the strict options, so private
source-built diagnostics were required. The first Intel attempt failed before
decode because pinned Jellyfin libva searches its canonical driver directory
and renamed environment variable; ordinary libva overrides do not apply.
A read-only canonical driver binding corrected that experiment configuration.
A changed production epoch and a stale transfer-file preflight were separately
refused before GPU access and remain recorded.

The corrected Intel window passes all 28 controls: twelve software and twelve
VAAPI strict metadata/render/loss/seek/terminal-NAL cases, plus four raw output
comparisons. Fresh and variable-metadata hardware/software output is exactly
equal. Selected opaque VAAPI frames, mandatory hardware download, current
frame Dolby state and independent colored-patch bounds are observed. The
window is closed and all owned GPU work reaped. These 320×180→160×90 controls
use an Ubuntu diagnostic binary, not the production Debian package; they do
not qualify production encoder graphs, 4K, AMD, CUDA, D3D or performance.

The first AMD window was returned unused when admission found production
FFmpeg. A later shipping-build CPU window was also returned unused because
unrelated compiler/container jobs were active. The coordinator retains CI
priority; no alternative host is authorized. Source integration continued during that refusal. A subsequent finite
Bookworm SDK/bootstrap lease completed with exit zero and its owned container
stopped: GCC 12.2, glibc 2.36, all 100 official source patches plus the strict
patch, and exact original FFmpeg/driver hashes are retained. No C compilation
or GPU access occurred. The next public-header stage is prepared but remains
unused: the coordinator has held further shared-host work while current-batch
blockers are cleared, and the warm Linux compiler lane remains reserved.
A newly observed foreign candidate has not been attributed; no alternate host
or second cold build is authorized. Local source work continues. Whole-image transfer was
rejected by automatic approval; the successful diagnostic used only an
explicit binary/public-dependency/original-fixture allowlist. No deployed
service, package or host driver was changed.

The rebuilt common Mac package must retain verified archive-backed source
offers as well as Git-backed dependencies. The helper change is integrated as `8287f734c`: future builds bind every
archive role to its exact transformed recipe/helper, while historical builds
retain their actual source tree and executed transformation/log closure.
The original 54-role audit accounts for 43 changes, and a fresh preparation
verifies 69 recipe bindings. `9211fc71f` tightens generated-file allowances
to exact audited hashes; command text alone must not bless arbitrary changed
source. No fabricated Git metadata or stale provenance substitutes for the
actual compiled source. The normal API text-burn check found a provider mismatch before it could
prove seek timing: Mac libass's automatic provider is CoreText, but the
existing font environment freezes and verifies Fontconfig. The request
correctly refuses an unproven font environment. Native now owns an explicit
provider option shared by CPU and GPU rendering and the freeze probe; Plurx
will select supported Fontconfig under its existing authority and bind that
choice to the recipe. Standalone automatic provider selection remains
unchanged. No trace check is bypassed and no second font authority is added.
The explicit-provider candidate compiles and passes the same-provider
24-frame animation/PTS comparison; actual libass logs show Fontconfig and
the same selected face. An unsupported-provider negative refuses cleanly.
The app projection/freeze changes still need the newly built daemon's normal
API proof and actual source-position seek checks.

The GPU/projection/provider foundation is committed as `1098374bb`, with
conditional package-help validation in `27ab44d8f`. The latter applies to
prepared `0004` builds and preserves official baseline packaging mode. Exact
all-target compilation and normal hooks pass; unit execution remains deferred.
A private daemon and coherent FFmpeg package now exist for source-bound API
checks. They are not yet a clean-install-qualified release.

Clean-install inspection found two actual font dependencies that the current
host could conceal: `fc-list`/`fc-conflist` are found through host `PATH`, and
the static Fontconfig default configuration points into the build prefix.
Native packaged the pinned tools and relocatable configuration through
the existing tool resolver and font authority. Explicit configuration overrides
and non-Mac behavior stay supported. No global install, Homebrew dependency
or retained temporary prefix may substitute for the repair. Qualification must
exercise the package with the owned build prefix unavailable. Caption-only
API checks can proceed independently because they do not use text rendering.
The font-authority repair `ae9996b1e` is integrated: exact all-target
compilation and normal hooks pass, and a prefix-hidden/system-PATH CPU control
can query fonts and render. It binds font identity only to text plans and uses
the existing held Source environment. The source-clock and shared package-helper union `874a5efd4` is integrated
with reporter/origin identity, per-input normalization and existing engine
publication fences. All-target compilation and normal hooks pass; corrected
caption API qualification passes as recorded below. The mechanism evidence is retained in
[evidence §29](MACOS-VIDEO-PROCESSING-EVIDENCE.md#29-required-follow-up-mechanisms--2026-10-08).

The combined source `1f1545456` passes the normal pinned-toolchain hook and
produces the private qualification daemon. The assembled native package now
contains the exact font tools, 23 relocatable configuration rules and nine
font provenance files; its manifest is
`74e300dd743ff74d92c29908cce567859fc5f009785a3b9f4af83efc356446d7`.
Assembly validation is not normal-API acceptance. This first daemon was a debug
qualification build; the later release daemon and matching symbols are recorded
below with their separate lifecycle evidence.

Linux runtime preparation now includes an original 3840×2160, 24 fps,
24-frame P5 fixture and its missing-metadata negative. Generation and bounded
source validation pass. Neither this corpus nor the earlier small Intel
diagnostic grants a production graph or size/rate envelope; exact shipping
package and device-bound observations remain required.

The Linux runtime source draft now connects two-phase library/device discovery,
a retained execution binding, subsequent complete-graph qualification, bounded
size/rate observations, and the existing resolver/startup/admin owner. Active
plans retain their qualified context through a temporarily pending reprobe;
positive identity changes and live object changes still refuse execution.
Loader policy files and source-bound runtime objects are included in the
binding. Thirteen regression references resolve statically. This draft has
not passed Linux type checking or hardware execution and advertises no new
available route on that basis.

The SDK staging helper `5a056cd81` retains immutable source, recipe, configure,
license and official-package member identities. It explicitly reports that
generated headers/package metadata and link readiness are still absent.
Dolby now owns completing that SDK preparation while Native concentrates on
Mac delivery; Routes retains the Linux runtime. Docker FFmpeg and
sealed-parser packaging now share Dolby’s shipping ownership. Actual upstream generation and the unchanged full FFmpeg
configure/link closure are required before a shipping SDK claim.

The completed static preparation now verifies 57 source roles (25 private
dependency sources and 32 distributed Bookworm roles), 194 actual sysroot
members and 100 unchanged official ELF imports. All 57 official configure
tokens remain intact. The helper retains 511 expected project-generated
output selectors, but has not generated them or claimed link readiness.
Actual Bookworm generation and full linking await their resource window.

A subsequent transitive `pkg-config` audit finds additional development
metadata behind GnuTLS, OpenMPT and Blu-ray dependencies. The matching public
Bookworm packages and source offers are being added. The earlier 57-role
result establishes direct configured-role coverage and retained input
integrity, not a completed transitive build closure. The old generation
admission is therefore historical; execution requires freshly staged inputs
bound to the completed helper and an updated resource grant.

Fresh static preparation now covers 72 roles, 330 sysroot members and 51
authenticated package-namespace aliases. The declared transitive `Requires`
coverage has no missing roles; actual generated metadata and link closure
remain unproven. Preflight also identifies a real tool-version mismatch:
pinned Fontconfig requires Meson at least 1.6.1, while the retained Bookworm
bootstrap has 1.0.1. A private pinned Meson 1.12.1 provider is now prepared
without global installation; its isolated launcher and retained source tree
have static identity evidence, but Meson has not run. Further concrete
prerequisites bring the source lock to 75 roles. Static staging now passes
for all 75 after correcting authenticated Debian source-format handling. The
first actual Bookworm generator attempt stopped after 21.60 seconds because
the FFTW recipe omitted its upstream metadata target. The source repair invokes
`make fftw3f.pc` after float configure; it does not manufacture the file or
weaken missing-output checks. Fresh static staging passes in 6.59 seconds.
The corrected committed-source retry verifies all 6,596 input files and
passes FFTW's actual upstream metadata target. It then stops after 21.91
seconds because FreeType's archive lacks its required pinned `dlg` submodule.
The source repair supplies six required submodules from exact parent gitlinks
and replaces eleven unusable repository download URLs with verified canonical
archives of the same pinned commits. Their normalized source trees match the
retained originals, including executable bits and symlink targets. Fresh
static staging and actual generation of the repaired inputs remain pending.
The container is stopped and its lease released. Actual generation must complete before producer configure/link,
parser and shipping qualification. [Evidence §33](MACOS-VIDEO-PROCESSING-EVIDENCE.md#33-linux-sdk-generation-and-release-integration--2026-10-08)
records the failure, authentic rule and current input identity.

Shipping source `6cdd3755f780bfef215ebd09e190ada2504d3ef1` now connects
ordinary amd64 `make docker` to the audited package output through a named
BuildKit context. Installation uses a temporary Python stage and root access;
the final image restores `USER plurx`, excludes the installer tools, and retains
build ref, SHA and source-epoch arguments. ARM and existing default/CI targets
retain their incumbent paths. The real package is still a build prerequisite:
no producer image, link closure or hardware route is qualified by this source
integration alone. Combined commit `070ac8814` integrates the Linux runtime
and shipping source, preserving the same captured engine and font authority.
Its portable source check and corrected normal hook passed without unit tests.

The local build seam did not cover published images or CI smoke images: both
used a separate runtime-assets/renderer path. The current shipping candidate
adds one reusable Bookworm package-export stage and connects both paths to
its audited output. Media-asset publication does not compile daemon Rust;
ARM retains its incumbent path. This is source integration, not a claim that
the package or final image has built successfully.


Corrected normal API caption delivery now passes on the signed debug package:
public HEVC retains 120 due 608 and 66 valid 708 records across 120 frames;
original MPEG-2 retains all 48 caption records; the caption-free control has
none. The MPEG-2 initial oracle failure and narrowly modeled `fps`/608 FIFO
startup assignment remain recorded in [evidence §30](MACOS-VIDEO-PROCESSING-EVIDENCE.md#30-caption-bearing-normal-api-delivery--2026-10-08).
All three debug shutdowns still exceed the settlement bound. Owned stack
samples identify unfinished startup WebAssembly compilation, not a stuck
VideoToolbox producer. The first release-build window expired at 600 seconds
during optimization, without a source error; its exact unfinished crate was
not recorded. Its partial cache and failure receipt are retained. The
separately leased 1,800-second exact-source retry passed in 792.69 seconds,
using pinned Rust 1.97.1 and two jobs. The release daemon and matching symbols
are retained, and the compiler lease is released.

The signed release bundle uses the same FFmpeg, FFprobe and sealed-parser
bytes as the caption delivery matrix. Its first prefix-hidden CPU subtitle
API run passes the independent source-cue oracle across 49 frames, has no
startup parser identity warning and shuts down with exit zero. The overall
case remains failed because its resume request used `play`, whereas protocol
v1 requires `active`. The first hold request had already passed. The driver
is corrected against the current protocol without changing application
behavior; GPU text and bitmap delivery and complete control checks remain.
This release lifecycle result is separate from the three caption payload
matrices and does not replace their retained debug shutdown failures.

The next normal local API text case exposed a production ordering error:
the runtime report has matching available text/bitmap observations, but
selection receives a font digest only from shared-source evidence. Local
playback captures the same encoded engine later, after resolution, so its
GPU text path cannot qualify. The repair moves that existing capture before
resolution and keeps the same engine through publication. It adds no font
authority, diagnostic override or probe. The failed selection receipt is
retained; bitmap qualification can proceed on the unchanged release while
the text repair is implemented and compiled.

The narrow repair `b7d21934b` is integrated by manager commit
`c0691d5ffdf34d1dc06e83089c970a1c3d89f46a`: the same captured engine
supplies font identity to both fresh-fact and held-fact resolution, then
remains retained for encoding. Its focused regression source covers matching,
absent and mismatched font identities. Pinned all-target compilation and the
normal hook pass; no unit tests ran. Updated release API acceptance is next.

The bitmap API case selects the actual GPU graph and a nonzero producer
trim of two seconds. Its source-time oracle fails on the last cue frame:
the cue is visible at 3.333–3.666 seconds but absent at 3.750, while the
source clear packet is at 3.771. Parser initialization and shutdown pass.
The builder is tracing the normalized sidecar, decoded display/clear events
and frame scheduling against the incumbent CPU graph before deciding whether
the defect is in scheduling or in the oracle. The failed receipt remains
unchanged; a graph selection alone is not subtitle acceptance.

The incumbent CPU bitmap graph has the same last-frame result. Exact source
and normalized sidecar timestamps retain the 21 ms video offset. Tracing the
pinned frame scheduler shows its secondary clear event rescaled to the main
1/12 time base with nearest rounding. A separate oracle reassessment must
derive both onset and clear-event ordering; CPU/GPU equality alone cannot
justify changing the independent timing expectation.

That trace now establishes both boundaries: the blank heartbeat immediately
before the active subtitle and the active event quantize to the same tick,
and the scheduler emits the main frame on the first tie. The separate
deterministic reassessment passes all 25 retained CPU and GPU frames; the
original failures remain. A corrected-driver GPU bitmap run then passes
all 25 frames, hold/resume/seek-back request acceptance, DELETE 204, parser
initialization and shutdown zero. This does not claim rendered seek-back or
client presentation. [Evidence §32](MACOS-VIDEO-PROCESSING-EVIDENCE.md#32-gpu-bitmap-api-delivery-and-local-text-authority--2026-10-08)
records the exact scheduling model, artifacts and limits.

The merge coordinator has frozen its current batch. This effort's new
features will be handed off for the next batch after implementation and the
single final adversarial review; the coordinator retains final test and
landing ownership.

The coordinator admitted two sequential, bounded native commit hooks. The
manager integration `c0691d5ff` passed in 186 seconds and shipping source
`6cdd3755f` passed in 172 seconds; both compiler leases are released. Linux
runtime changes were combined in `070ac8814`: source compilation passed in
263.84 seconds; a catalog-only hook failure was corrected without changing
Rust or replaying that check, and the normal hook passed in 319.05 seconds.
The bounded phase used 583.66 seconds in total and released its target. The
first Bookworm generator phase also settled and released its container. GPU,
Linux-specific compilation and native release windows remain separately
controlled; no unit tests ran in these builder hooks.

The coordinator has returned a separate earlier-batch HEVC publication
failure for priority repair. Its hvcC parser discards `array_completeness`
and accepts presence-only VPS/SPS/PPS arrays where `hvc1` requires complete
ones. The narrow repair preserves the distinct `hev1`/`dvhe` contract and
is published as [PR #938](http://192.168.4.7:3000/noirr/plurx/pulls/938),
head `21af407bdacdbde1a39682d9d11bff1a75045182`. Its one adversarial review
found no actionable issues. Pinned all-target compilation and the normal
commit hook passed on the preceding base; the reviewed patch and both full
modified files remain byte-identical after the requested base port. The
coordinator has the exact committed source archive and six focused regression
IDs for Linux qualification. All six focused controls subsequently passed
with the coordinator, which is combining the earlier-batch blocking repairs
under the existing PR. No units ran in the builder, and the native compiler
target is released. This earlier-batch repair is separate from
the follow-up candidate and does not count as its final review.

The temporary Metal compiler component is tracked for exact
removal during final cleanup. Direct experimental binaries are immutable
snapshots, not assembled production packages.

The accepted-batch fixture-license repair is separately complete. Sol commit
`c56c1d04` updates a stale generator constant to the authentic expanded CC0
license without changing media, manifests or quality checks. Independent
review accepted it; the coordinator reran only the affected case, passing
8/8 in 0.369 seconds. This effort retains identical reviewed blobs in
`4f5fee3f0`. The temporary repair clone was removed after import and lease
release; its review and cleanup receipts remain retained.

### Prior contribution retained for the merge coordinator

The initial SDR/HDR10 implementation is built and independently reviewed.
PR #870 was composed into coordinator PR #882 and landed on `main` as
`339abbced98a5a167556aa011ee2dbe31cd332d4` on 2026-10-08. The coordinator
reports its unit/normal-hook checks passing, current preflight/web passing,
and retained source-equivalent Rust success; Windows was explicitly waived.
This is not a claim of a fully green promotion across every platform.
This is completion of the first implementation contribution, not the full
proposal. The user renewed the full-scope instruction on 2026-10-08.

All planned production source in the supported scope is built, integrated and
independently reviewed on `effort/macos-video-completion`. Final release source
is `1bf84609`; later commits retain documentation and evidence. The five
original review repairs, terminal-NAL follow-up, shared Jellyfin tool resolver
and Live terminal-classification repair are independently accepted.

Actual final packaged-daemon SDR/HDR10, finite HEVC, ordinary Auto, strict P5
and affected Live lifecycle checks pass. The shared resolver passes normal
AC-4 source attestation and explicit software-encoder AAC/AVC delivery. The
public caption-bearing sample's incumbent CPU/VideoToolbox SEI insertion
failure remains the documented file-VOD limitation; no caption-stripping
workaround was introduced. Empty/corrupt/permission/full-cache recovery and
restart pass. Source-bound earlier successes and all failed controls remain
visible in the evidence document.

Thirty CPU and thirty native uncached starts meet the p95 regression
allowance; one/two/four independent sessions admit and publish. Four sessions
complete a 1,800.052-second demand/control soak with 1,096 successful objects
and no recorded control or producer failure. This is HTTP availability and
demand evidence, not physical presentation, sustained encoder capacity or
memory/energy/copy instrumentation. The initial soak driver used a stale
control sequence; its failure is retained, and only the corrected soak was
repeated.

The designated merge session owns the final fast lane and main landing of
PR #904. The newly assigned F1–F3 work above proceeds separately. No unit suite ran in
this effort. Broad physical-client, calibrated-display, deployment, fleet
upgrade and other-host qualification remains explicitly open; the Developer
switches retain their advisory graduation status. This is not an all-device
or all-qualification completion claim.

No deployed FFmpeg package or live service has changed. The matching Apple
Metal compiler component was temporarily installed for the private package
build and the exact owned build `27A266a` has now been removed. All twenty-two
final runtime observations and actual SDR/HEVC/Auto delivery pass afterward.
Missing qualification stays explicitly open.

| Workstream | Owner | State | Evidence or next dependency |
|---|---|---|---|
| Integration and status | Manager | reviewed handoff ready | Final production source1bf84609; source-equivalent docs/evidence commits; seventy regression references resolve |
| M0/M1 Jellyfin package and harness | Sol builders | source complete; package controls recorded | Final strict terminal-NAL package `c8c4b525…`; pinned source offer, AC-4 byte preservation and prior synthetic benefit evidence |
| M2/M3 typed routes and observations | Sol route builder | source complete; inventory passed | All twenty-two supported graph classes available in actual packaged-daemon reports |
| Native parser and symbols | Sol native builder | complete | Signed final package, matching dSYM, normal scanner/held-parser identity and explicit software AC-4 delivery pass |
| M4 normalized VOD | Sol native and route builders | final SDR/HDR10 delivery demonstrated | 120-second HTTP lifetime, CPU/native pixel comparison, seek/resume and cancellation pass; timing/concurrency/soak pending |
| E1 strict Dolby P5 | Sol Dolby and route builders | source complete; sixty controls pass | Sixty selected-graph controls and sixteen observer mutations pass; terminal-NAL fixture registration independently accepted. Final normal-API strict P5 delivery passes on `a2ee4a221` |
| E2–E4 HLG, burns, BWDIF, HEVC and Live | Sol builders | source complete within demonstrated scope | HLG text/bitmap controls pass; actual HDR High10 interlace decode fails and keeps incumbent. HEVC master/Auto pass; twelve Live cases and EOF pass. Source-change repair independently accepted; final actual EOF/change/reprobe/cadence checks pass |
| Client transport declarations | Sol Dolby builder | source complete; compilation passed | iOS/tvOS builds pass on the final Apple source; Android C1 app, instrumentation and JVM regression source compile. Physical presentation unqualified |
| M6 acceptance and promotion | Manager and merge coordinator | implementation contribution ready | Independent reviews and available-host runtime checks complete; coordinator owns final fast lane/main landing; physical/deployment qualification remains explicit |

Jellyfin FFmpeg is the required implementation baseline. Local Homebrew
FFmpeg is not a comparison target or substitute. The official package is an
experiment candidate; its capability inventory is not a performance result
or proof of the deployed daemon version. No local daemon has been found.
The SDR result establishes synthetic processing feasibility, not production
capacity or perceptual qualification. The original HDR comparison is invalid:
the CPU output retains stale HDR side data, while Metal tone-maps before
scaling and therefore processes four times as many pixels. Both causes are
corrected explicitly in a separate revised comparison: five valid paired
HDR runs show 97.40% lower CPU cost and +2.14% median paired throughput.
Four of five pairs meet the CPU/throughput consistency bound; the fifth
is 5.51% slower and remains recorded. Energy and
copy counts remain unmeasured. The four-way artificial P5 diagnostic retained
metadata, without establishing RPU visual influence or hardware reconstruction.
Small SDR/Metal outputs have factual geometry, color-signaling and ramp
observations; these are not full visual or playback qualification.

## 2. User-directed workflow and file ownership

On 2026-10-07 the user authorized parallel Sol 6.1 builders, proper commits,
batched PRs, and autonomous routine decisions. These explicit instructions
supersede the earlier sequential-task and per-task test/review language:

- All work occurs in task-owned separate clones. User checkouts are untouched.
- Builders commit normally; the manager integrates onto the effort branch
  and prepares a batched main-bound PR. No per-builder PR/review is required.
- Adversarial review happens once the main-bound PR is ready. Address its
  findings before running the fast lane. Do not run unit suites while building.
- **Merge handoff amendment:** the user subsequently assigned queueing,
  batched validation and main merging to
  [Coordinate PR merge batches](codex://threads/01a11907-f720-71b1-8c51-89902b919e6f).
  This effort hands off the reviewed commits/PR, regression commands, evidence
  and limitations; it does not independently run the merge queue or duplicate
  that session's unit validation. The coordinator may fix superficial test
  failures without behavior changes. Behavioral failures return to this
  effort for a proper fix and review before rejoining the queue.
- Compiler, formatter and lint feedback remains available during building;
  hardware experiments establish feasibility and are distinct from unit suites.
- Each required check needs one valid passing result for the code being merged.
  Fix failures and rerun affected failed checks, retaining valid passing evidence
  where supported. Full unrelated unit failures belong to the separate process.
- No hidden feature gates or additional playback watchdogs. Any necessary
  manual switch belongs in Developer with advisory readiness and a saved choice
  that is always accepted. Existing owners handle real runtime incompatibility.
- Retain only useful source/evidence and remove task-owned transient artifacts
  during cleanup. No credentials, private media or real infrastructure identifiers
  belong in committed receipts.

The session supports four concurrent agents including the manager, so three
Sol builders are active. Subsequent work reuses those slots. Builders have
received and acknowledged the full user instruction packet.

| Owner | Initial exclusive file ownership |
|---|---|
| Manager | This status page, document index and workflow amendments; integration and PR metadata |
| Native builder | `scripts/build-macos-video-ffmpeg` and `scripts/macos-video-build/` helpers if needed |
| Harness builder | `scripts/bench-macos-video`, `scripts/macos_video_bench/`, focused harness test source |
| Fixture builder | `crates/plurxd/fixtures/macos-processing/`, fixture generator and focused fixture test source |

The initial files above are committed. The next table records the initial
application ownership; active continuation ownership is recorded in §9:

| Owner | Initial application ownership |
|---|---|
| Native builder | Core `transcode/{pipeline,decode,mod,vod,recipe}.rs`, associated core regressions and decoder-selection test source; atomic exhaustive daemon metric/count-array rows |
| Harness builder | Daemon main registration, transcode manager construction/planning/recovery, subsequent diagnostics, settings HTTP and Developer UI integration; one stored-key constant and reprobe route registration |
| Fixture builder | New daemon `macos_video.rs` probe module, embedded-fixture preparation and its focused test source |
| Manager | Documentation, integration, final review coverage and validation/PR records |

Agents do not edit another workstream's files. Shared catalog rows integrate
as separately owned hunks. Hardware benchmarks run serially without concurrent
compiler load to avoid contaminating measurements.

## 3. Decisions and limitations

| Decision | Reason | Revisit when |
|---|---|---|
| Start three disjoint M0/M1 workstreams | Packaging, harness and fixture construction can proceed independently before planner integration | First usable graph and corpus are available |
| Keep deployed services/packages unchanged during experiments | Private candidate builds allow attribution and preserve incumbent AC-4/Dolby behavior | Qualification supplies a deployment decision |
| Use explicitly pinned Rust executables | The interactive shell selects a different Homebrew compiler | Every Rust integration/compile session |
| Use official checksum-verified Jellyfin FFmpeg for all candidate and baseline experiments | Preserve the required AC-4 and Dolby functionality; stock Homebrew FFmpeg is irrelevant to this effort | A demonstrated package defect requires a different pinned Jellyfin build |
| Prefer the official complete Mac archive to a source rebuild | The archive already exposes the needed filters and Apple-only dynamic library closure | A required behavior cannot be supplied by the official package |
| Record negative or unavailable results explicitly | Missing evidence cannot justify performance or color claims | Hardware/corpus/provenance becomes available |
| Scale HDR at 10-bit precision before the Metal mapper | Original graph performed tone mapping on four times as many pixels as the production CPU comparator | Revised full comparison and temporal/visual acceptance |
| Fix stale MDCV/CLL only on the shared CPU + zscale HDR-to-SDR chain | That route reproduced incorrect residual HDR signaling; `tonemapx` already removes it, so unrelated Dolby/Metal paths need no change | Separate corrective commit and focused regression review |
| Version identity only for routes changed by consumed-HDR metadata cleanup | This root-cause fix changes legacy output bytes; preserving their previous cache key would be incorrect | Verify unaffected SDR and HDR-preserving hashes remain stable |

## 4. Validation and handoff ledger

- Pinned compiler availability verified: Rust 1.97.1, Cargo 1.97.1.
- First three builder commits plus harness correction `308921d3f` integrated
  on the effort branch. Each normal
  tracked hook passed catalog lint, pinned Rust format/Clippy and JavaScript
  syntax. No standalone unit suite was invoked.
- Unit suites: not run for this effort; deferred per user direction.
- Official Jellyfin source tag: `v8.1.3-1`, source commit
  `253db2a7b0a8045c54ce68ce33d7f601229b1822`.
- Official arm64 archive SHA-256 checked against the release API:
  `22445d7299742749ad2eeb9ce87963d50def0357e45b3e6c7b69987c8365dbf6`.
- Capability listings show `scale_vt`, `tonemap_videotoolbox`,
  `tonemapx` with `apply_dovi`, and AC-4 decoding. SDR and Metal HDR10 graphs
  executed; listings alone do not establish AC-4 or Dolby sample acceptance.
- Fixture generation/probe: 12 decoded frames each for SDR8, SDR10 and PQ;
  the PQ source includes explicit VUI and mastering/content-light metadata.
  The generator was corrected after probing showed x265 omitted primaries and
  transfer unless explicitly supplied in its VUI options.
- Native graph smoke checks: SDR scaling and Metal HDR10 each produced three
  decodable H.264 frames at 160×90, BT.709 matrix/transfer/primaries and TV
  range, with ordered timestamps and no retained HDR/Dolby side data.
- Literal retained surface format: `videotoolbox_vld`. The native color-scale
  HDR alternative remains diagnostic; it is not an accepted production path.
- Execution required ordinary host IOSurface access outside the tool sandbox;
  sandbox denial is recorded as an environment limit, not a codec failure.
- Hardware evidence: VideoToolbox filter/encoder surfaces, Metal device and
  hardware-required H.264 encoding observed. Pinned FFmpeg only requests,
  rather than requires, hardware HEVC decoding; actual HEVC hardware use is
  not proved by the surface spelling and remains unavailable evidence.
- Output gray ramps were observed to be monotone. SDR scale gray values
  matched the analytic input within one code value. These small observations
  do not replace full SDR/HDR perceptual or temporal qualification.
- Harness integration smoke caught two-flag versus three-flag FFmpeg filter
  table differences; parser corrected before repeating only the failed smoke.
- Harness wiring smoke: four 1-second runs passed expected output contracts.
  They are reported as smoke-only, with zero sustained pairs.
- Sustained hardware benchmarks: synthetic 60-second, 2160p24 SDR/PQ
  sources built. Five warm SDR pairs pass output contracts: median paired
  CPU cost reduction 87.80%, throughput change +0.71%. CPU reduction is
  consistent in all five pairs. These fast runs lack enough steady 5-second
  windows for capacity qualification.
- Original HDR rows retained: CPU outputs fail stale MDCV/CLL checks;
  candidate outputs pass, but throughput is approximately 19% lower by the
  median paired comparison. Neither its CPU reduction nor its throughput
  establishes an accepted HDR result. Revised scale-before-Metal graph
  passed a three-frame correctness smoke. Revised five-pair results pass
  output contracts and meet the synthetic CPU criterion in four of five
  pairs; the 5.51% throughput regression in pair four is retained explicitly.
- Status/checklist and merge handoff committed as `fcbef06ac`; normal hook
  passed. The merge coordinator owns subsequent batch validation/landing.
- Mac core contract committed as `47eab3a75`: pinned core check with
  `--features hiqlite-store --all-targets`, plus normal hook passed. Tests
  compiled, not executed. Runtime module `532e78d5f` is unregistered at that
  commit; its standalone hook is not module type-check evidence. The daemon
  builder now owns registration and the integrated compiler check.
- Registered daemon all-targets check and executable build passed on
  `a17de21fe` plus daemon changes. Normal hook identified denied unwraps in
  new test source; these were corrected without executing tests.
- The final registered daemon check passed after narrowly invalidating stale
  `plurx-core` artifacts in the shared target. The earlier failure reported a
  staged constant missing from cached metadata produced by another clone;
  no application workaround was added. Builders use exclusive compiler-target
  ownership during the final integration checks.
- Isolated daemon settings, pending/available observations, admin reprobe,
  corrupted fixture repair, persistence and orderly restart passed.
- Actual non-normalized encoded VOD delivered the first SDR and HDR10
  fragments through the canonical Mac plans: 48 decoded 1080p BT.709 frames
  each. Normalized continuous VOD retains its native source-probe refusal;
  public live presentation is removed. Neither is claimed qualified.
- Native bound-source investigation found reusable source/cache/lifetime
  ownership but no small supported equivalent for immutable parser execution
  and descendant containment. This is an explicit parser-packaging and
  confinement follow-up, not permission to bypass the source boundary.
- Initial adversarial implementation review: all five findings corrected and
  independently accepted at source `575a68abc`.
- Initial PR #870: handed off and composed into coordinator #882. The
  coordinator reported 128 focused checks before main landing `339abbced`. Remaining implementation
  is on a separate effort branch; final review follows its frozen candidate.

Later entries will record commit IDs, root-cause observations, commands,
accepted graphs, failed checks and their targeted reruns, qualification
limits, review dispositions and cleanup. Completion requires honest closure
of the applicable acceptance rows, not merely a successful compilation.

## 5. Full scope checklist

This checklist traces the complete agreed design, including deferred work.
**Integrated** means committed to the effort, not merged or qualified.
**Building** means an assigned implementation is active. **Awaiting evidence**
means proof is still owed; it never means passed. **Planned** is initial-scope
work not yet started. **Follow-up** is an explicitly separate extension,
not cancellation or completion. A measured negative result may retain the
incumbent, with its reason recorded. The linked design and implementation
sections remain the detailed acceptance specifications.

### 5.1 Inventory, native processing and experiments

| ID / plan reference | Agreed item | State / remaining evidence |
|---|---|---|
| M0-01 · implementation §3 | Exact deployed daemon FFmpeg/FFprobe and service environment | Awaiting evidence: no local daemon found; official Jellyfin candidate must not be called the deployed baseline |
| M0-02 | Executable/library hashes, source/patch provenance, arch, SDK/minimum OS | Integrated package acquisition; arm64 archive and Apple-only library closure observed; x86 runtime unmeasured |
| M0-03 | Incumbent feature inventory, including AC-4, Dolby, subtitles, mux/audio/pacing | 38 required declarations checked; end-to-end sample matrix awaiting evidence |
| M0-04 | Existing SDR/HDR10/P5 baseline outputs or documented failures | Reconstructed SDR/HDR10 exercised; stale HDR metadata failure retained; deployed and P5 baseline awaiting evidence |
| M1-01 · implementation §4 | Bounded reproducible A/B harness, raw failures retained, explicit tools and cache conditions | Integrated; first-use and five paired warm runs recorded separately |
| M1-02 | Lawful embedded SDR8/SDR10/PQ corpus, recipe, hashes and expectations | Integrated; genuine PQ values and explicit VUI, not merely SDR relabeled HDR |
| M1-03 | Complete decode/process/encode graph and observed representations | SDR and Metal smokes observed; final HDR order is 10-bit VT scale, then Metal tone map |
| M1-04 | Native Apple HDR color mapping alternative | Diagnostic only; no accepted production variant; execution and retagging alone are insufficient color proof |
| M1-05 | SDR benefit at equal output and encoder controls | Five valid synthetic pairs support CPU benefit; broader workload and production evidence still owed |
| M1-06 | HDR10 benefit with fair work ordering and correct output | Five revised pairs complete; CPU feasibility criterion met in 4/5, visual/production acceptance open; original invalid comparison retained |
| M1-07 · design §§2, 6 | Unified-memory attribution: mapping, allocation, conversion, synchronization and copies | Awaiting instrumentation; shared physical memory does not prove zero-copy, and CPU reduction is not a measured copy count |
| M1-08 | Actual hardware reconstruction, separate from VT surfaces/hardware encode | H.264 hardware-required encoder observed; corrected synthetic P5 now has an observed hardware-required HEVC session and byte-identical software/hardware reconstruction; other source/device tuples remain unqualified |
| M1-09 · implementation §§1.1, 9.1 | Early P5 four-way software/hardware decode × CPU/Metal renderer experiment | Original analytic P5 controls replace the early transport-only smoke; strict selected-PTS metadata and independent color observations are integrated and accepted. Separate RPU-influence and hardware-reconstruction controls pass; mastered content remains unavailable |
| M1-10 | Same-build A/B plus deployed-build baseline when replacing package | Same official Jellyfin build used; deployed comparison awaiting access/evidence |
| M1-11 · design §3.3 | Direct Core Image/VideoToolbox helper, custom Metal kernels, Vulkan/libplacebo alternatives | Deferred alternatives only if existing filters fail a concrete requirement; no parallel processing stack being added |

### 5.2 Core contracts, runtime and production delivery

| ID / plan reference | Agreed item | State / remaining evidence |
|---|---|---|
| M2-01 · implementation §5 | Typed VT surface and measured SDR/Metal pipeline variants | Integrated; only `VtScaleSdr` and `VtToneMapMetal` admitted initially |
| M2-02 | One immutable resolver and shared rolling/VOD argv | Integrated; no mutable filters behind recipe identity or second planner |
| M2-03 | Decoder overrides, continuation restrictions, source facts and output grade | Integrated; explicit software choice and incompatible presentations retain incumbent |
| M2-04 | Conditional Mac identity: graph, binary/libraries, OS, arch, hardware class | Integrated; excludes host name, probe time and benchmark score |
| M2-05 | Unaffected plan/recipe hashes preserved | Integrated; separate scoped identity revision for genuine consumed-HDR metadata fix recorded in §3 |
| M2-06 | Audit `on_gpu`, surface consumers and copy-related comments | Integrated; frame representation must not imply physical memory placement |
| M3-01 · implementation §6 | Bounded asynchronous post-startup probes and immutable generations | Integrated; one graph at a time, cancellation/reaping through existing child owner, no watchdog |
| M3-02 | Embedded fixture integrity, private cache, atomic corrupt-cache repair | Integrated; no network/developer files/runtime encoding dependency |
| M3-03 | Preparation/probe deadlines, disk/permission/cancellation failures and reprobe | Integrated; explicit unavailable diagnostics and restart/reprobe recovery |
| M3-04 | Saved processing switch and readback/restart/new-plan semantics | Integrated; absent default false, saved choice always accepted on all hosts |
| M3-05 | Developer readiness and concrete graduation requirement | Integrated; readiness advisory only, no offline benchmark receipt in enable/runtime path |
| M3-06 | Selected graph/decoder/surface/grade/revision and fallback reason diagnostics | Integrated; bounded metric labels and one accepted-start counting boundary |
| M3-07 | Worker-local resolution and capability change between offer and acceptance | HEVC source includes actual catalog/selected-recipe acceptance and old-worker rejection regression source; compiled, not executed; no new required graph protocol field |
| M3-08 | Old→new/new→old dispatch, takeover and recipe/cache isolation | Old H.264 re-resolution of a selected HEVC candidate has an actual full-recipe rejection regression; compiled, not executed. Fleet takeover evidence remains open |
| M4-01 · implementation §7 | SDR VT decode → native scale → H.264 VT encode | Integrated; signed a2ee normal-API native normalized SDR delivery passes, including 120 seconds of paced HTTP demand |
| M4-02 | Same-size/downscale, SDR8/10, seek/resume and long-GOP behavior | Actual normal-API 48-frame output and seek target PTS 60 pass; broader long-GOP, client presentation and resume matrix remain unqualified |
| M4-03 | SAR, rotation, odd dimensions, VFR, interlace and burn controls | Integrated eligibility boundary; preserve incumbent semantics instead of dropping required processing |
| M4-04 | Prepublication retry excludes failed Mac graph, new plan and asset identity | Integrated in existing retry owner; no CPU fallback immediately upgraded back to failed graph |
| M4-05 | Postpublication replacement/error, cancellation and resource release | Normal cancellation, resource refusal and idempotent release observed on a2ee; injected postpublication replacement/error remains unqualified |
| M4-06 | Native bound-source qualification for normalized continuous VOD | Bounded held-source Jellyfin WASM/JIT parser, signing and packaging integrated. Actual normalized native SDR/HDR10 delivery, 120-second demand, seek/resume and cancellation pass on a2ee without fixture mode or stored-facts bypass |
| M5-01 · implementation §8 | HDR10 → SDR with 10-bit precision through scale and explicit BT.709 output | Integrated scale-first Metal graph; actual normalized HDR10 to SDR delivery passes with BT.709 output on a2ee |
| M5-02 | Consumed HDR metadata removed only after actual SDR conversion | Native output observed clean; scoped shared CPU/zscale-boundary correction integrated |
| M5-03 | Peak provenance, missing/1000/4000-nit metadata, gamut/gradients/scenes | Awaiting broader corpus and visual evidence; do not claim CPU Hable controls applied to Metal |
| M5-04 | Segment restart versus continuous tone-map temporal consistency | Awaiting independent VOD segment/seek evidence |
| M5-05 | P5/P7/P8, HLG, HDR10+ and HDR-preserving boundaries | Incumbent controls retained; no wildcard HDR or generic PQ fallback for P5 |

### 5.3 Qualification, packaging and acceptance

| ID / plan reference | Agreed item | State / remaining evidence |
|---|---|---|
| Q-01 · design §6.1 | Target production Mac, second Apple Silicon generation and separate Intel matrix | M3 Max experiment host observed; deployed/second-generation/Intel evidence unavailable so far |
| Q-02 | 1080p light workload, 4K24/60, SDR8/10 and varied HDR corpus | Synthetic 4K24 and tiny probes built; rest awaiting evidence |
| Q-03 · design §6.3 | Five paired ≥60-second source runs and precise first-use/warm labels | SDR and revised HDR complete; no claim of cold disk cache |
| Q-04 | One/two/four sessions, steady-window p10 and 30-minute soak | One/two/four independent session admission/publication and four-session1,800.052-second demand/control soak pass. Steady-window p10 and sustained encoder capacity remain unqualified |
| Q-05 | ≥30 starts, first publishable media and first presented frame, p95 | Thirty CPU and thirty native distinct uncached starts: HTTP-availability p95 1.242879s vs1.216253s, within1.367167s allowance. First presented frame remains unmeasured |
| Q-06 | CPU, throughput, peak RSS/memory pressure, energy, copy/sync instrumentation | CPU/throughput measured on synthetic workload; remaining measurements unmeasured, not zero |
| Q-07 · design §6.4 | SDR aligned quality metrics plus ringing/aliasing/banding inspection | Actual normalized 48-frame CPU/native delivery matches paired patches within one code value; whole-frame PSNR minimum 51.92 dB, mean 53.23 dB. This does not establish all-content perceptual acceptance |
| Q-08 | HDR blinded review on named calibrated display, highlight/shadow/gamut/temporal findings | Awaiting human/display evidence; synthetic ramp and VMAF alone are insufficient |
| Q-09 | Real plurxd rolling/VOD playback, two-minute play, seek/resume, errors/rebuffer/A/V | Signed a2ee normal-API native normalized SDR completes 120-second paced HTTP demand, seek target PTS 60, resume and cancellation; finite HEVC/Auto and HDR10 delivery pass. HTTP delivery is not client presentation |
| Q-10 | Correctness first, then ≥20% throughput or ≥20% CPU/≥15% energy benefit within throughput bound | SDR/revised HDR synthetic CPU criterion met; visual/production correctness still unqualified |
| M6-01 · implementation §10 | Full incumbent AC-4 and P5 package acceptance in daemon environment | Strict P5 normal-API delivery passes. AC-4 original/patched packages produce byte-identical 239,999-frame 48 kHz stereo PCM. Shared scanner resolver repair passes normal catalog and held-source attestation; public broadcast incumbent VT-encoder SEI failure retained; explicit software-encoder normal-API AAC stereo48k/AVC delivery and cancellation pass |
| M6-02 | Native install, runtime libraries, signing/distribution and architecture compatibility | Pinned Jellyfin package, Apple-only native dependencies, signed isolated arm64 daemon and matching dSYM UUID verified. Developer ID/notarization, distribution install and Intel remain unqualified |
| M6-03 | Fresh offline install, empty/corrupt/full/unwritable cache and reprobe | All twenty-two graph observations available; empty/corrupt cache and explicit permission refusal/recovery pass. Bounded-volume ENOSPC, exact recovery and restart pass; distribution installation remains unqualified |
| M6-04 | Upgrade/rollback while workers active, old binaries retained or workers drained | Planned; no installed package changed yet |
| M6-05 | Switch off affects new plans, existing sessions retain captured implementation | Saved/reprobe/restart checks passed; active normalized native VOD retained its captured candidate/media after switch-off while a new disabled plan selected distinct CPU processing |
| M6-06 | Graduate switch only with claimed workload/client evidence, preserve saved value | Planned; stays in Developer while evidence incomplete; no automatic default flip |

### 5.4 Explicit extensions and cross-platform follow-up

These rows remain visible even when the initial SDR/HDR10 batch ships.
Separate scope does not imply that implementation or qualification is done.

| ID / plan reference | Agreed item | State / dependency |
|---|---|---|
| E1-01 · implementation §9.1 | P5 hardware decode feeding existing CPU `tonemapx` independently of GPU renderer | Strict immutable production recipe integrated; required hardware reconstruction matches software on original fixtures. Actual normal-API strict hardware/Metal P5 delivery passes; CPU-renderer tuple has independent runtime controls |
| E1-02 | Software decode + Metal and hardware decode + Metal Dolby rendering | Four decoder/renderer graphs integrated with separate observations; sixty final selected-graph controls and actual normal-API hardware/Metal delivery pass. Complete-graph benefit remains unmeasured |
| E1-03 | Strict per-frame effective Dolby metadata at renderer boundary | Integrated current-AU policy and selected-graph pre-renderer observer. Exact PTS/metadata and independent colored-patch controls accepted after B1 review; no claim of precise tone-curve qualification |
| E1-04 | Minimal pinned FFmpeg strict-mode patch if existing mode insufficient | Integrated; final package `c8c4b525…` repairs legal terminal EOS/EOB while refusing missing RPU. AC-4 PCM remains byte-identical |
| E1-05 | Lawful Dolby runtime smoke, P7/P8/direct-play/remux controls and Dolby-aware fallback | Original analytic P5 fixtures and strict software recovery integrated; all twenty-two final daemon graph observations available. No P7/P8/direct-play/remux policy expansion |
| E1-06 | HLG reference-white, color and temporal qualification | Foundation integrated: analytic HLG fixture and independent runtime observation; 203-nit reference-white mapping observed. Broader color/temporal qualification remains open; no inheritance from HDR10 |
| E2-01 · implementation §9.2 | Native scale/tone-map then CPU subtitle burn at output resolution | Foundation integrated: bounded SDR/HDR processing-before-burn smokes produced output; PGS half-alpha timing observed. Full benefit/semantics evidence remains open |
| E2-02 | GPU compositing of prepared text/bitmap images | Follow-up only if beneficial; keep libass shaping/fonts and explicit alpha/color semantics |
| E2-03 | PGS color, ASS animation/position, active-cue seek, EOF/free intervals and cancellation | Follow-up acceptance; no new HDR burn policy |
| E3-01 · implementation §9.3 | Hardware BWDIF where supplied; separately judged YADIF alternative | Integrated pinned parameter-buffer repair and shared moving-field acceptance: eight raw CPU/native, twelve encoded positive and four old-package negative controls retained. Earlier cadence-only proof is explicitly limited |
| E3-02 | Native Live TV plan, frame/field cadence, rational rate, bitrate and manifests | Existing Live owner delivers twelve ordinary frame/field/repeated-start cases and EOF reconnect on a2ee. Reviewed source-change classification repair4be9603 integrated; affected actual EOF/source-change/reprobe/cadence checks pass on final1bf |
| E3-03 | 1080i TFF/BFF, 720p59.94, A/53, late audio, AC-4, reconnect/stop/start | Follow-up hardware/delivery matrix |
| E3-04 | Scoped live `-a53cc 0` and caption-bearing VOD limitation | Existing scoped live behavior retained in native upload plan; no claim to repair caption-bearing file VOD |
| E4-01 · implementation §9.4 | HEVC SDR and HDR10 Main10 output separately | Finite SDR/Main10 HDR producer, initialization and master codec assertions pass through normal API on a2ee; continuous AVC envelope remains unchanged |
| E4-02 | Explicit HEVC output setting with advisory Developer readiness | Saved choice, finite candidate negotiation and ordinary Auto preference pass through normal API with Display-aware Auto off. Readiness remains advisory |
| E4-03 | Actual HDR presentation, metadata semantics, no implicit Dolby passthrough | Follow-up; preserve existing VOD B-frame policy and promised output grade |
| E4-04 | Apple/native/web client play/seek/quality/recovery and bitrate-quality comparison | Follow-up; H.264 compatibility fallback through existing negotiation |
| X-01 · implementation §9.5 | Intel native VA-API/DXVA and NVIDIA NVDEC P5 investigations; other device tuples individually | Separate cross-platform follow-up; do not globally remove or make permanent the software-decode restriction from one Mac result |

### 5.5 Review, merge and cleanup obligations

| ID | Agreed item | State / remaining work |
|---|---|---|
| R-01 | All builders receive user constraints and have disjoint ownership | Done for active builders; packet applies to subsequent assignments |
| R-02 | Normal commits, tracked hooks, pinned compile loop before pushing Rust | Final source includes shared resolver4adb34c0 and reviewed Live repair4be9603; pinned all-target checks and normal catalog/format/Clippy/JS hooks pass; units deferred |
| R-03 | Explicit adversarial coverage of native/package/core builder | Final scope A accepted after A1/A2 repair `9acf814a8`; strict native B2 correction `184c937d` accepted |
| R-04 | Explicit adversarial coverage of harness/manager/settings builder | Final scope C accepted after Android capability repair `0d6a466cd`; prior harness and daemon repairs retained |
| R-05 | Explicit adversarial coverage of fixture/runtime builder | B1 selected-graph observer repair `a0a3734b` accepted; terminal-NAL runtime registration `54c3a3fe8` independently accepted |
| R-06 | Cross-builder integration review, root causes and no watchdog/gate cruft | Combined scopes A/B/C and five repairs accepted; terminal fixtures, shared resolver and Live terminal ownership follow-ons independently accepted |
| R-07 | Address findings, then hand off for coordinator-owned fast lane/regressions | All repairs accepted; available-host final runtime qualification complete. PR904 ready for designated coordinator fast lane/regressions; none executed here |
| R-08 | Current-main integration, exact-tree gate/qualification receipt and regression landing lines | Final signed source1bf84609 and seventy valid regression references prepared for coordinator; current-main integration, exact candidate qualification and landing remain its responsibility |
| R-09 | Retain reproducible sanitized receipts, exact commands, limits and autonomous decisions | Sanitized source-bound receipts, original failures, exact hashes and driver copies retained in indexed evidence document; runtime, cache, Live and timing/soak archives independently hash-verified |
| R-10 | Remove own transient clones, benchmark media, caches and obsolete branches after retention | Initial-batch builder clones, media/private daemon state and redundant sources removed after verified archive retention. Continuation clones, final package, active receipts and compiler targets remain needed; their cleanup follows final retention, queue ownership and qualification |

## 6. Completion accounting

The initial batch is M0–M6 with explicit honest disposition of unavailable
qualification rows; incomplete evidence keeps the feature's graduation
pending and remains visible here. It must not be relabeled a fully qualified
release. E1–E4 and X-01 retain their separate scope and acceptance. Update this
ledger when ownership, evidence, a decision or a PR disposition changes, and
link the actual integrated commits and durable receipts before closing rows.

A reviewed implementation handed to the merge coordinator is not completion
of M6. The control remains in Developer while qualification is incomplete,
with every unresolved acceptance row still open. No missing row is converted
to a pass merely to close the implementation PR.

The design's explicit non-goals remain excluded: client/player rewrite,
direct-play policy changes, source-media modification, Linux access to Apple
engines, interpolation/denoising/super-resolution/sharpening, assumed AV1
encoding, and generic non-Mac backend rewrites. They are not missing delivery
tasks. Priority and effort estimates remain in the companion plan/design;
this page records execution and proof rather than replacing those estimates.

## 7. Coordinator validation packet

These commands are **not execution results**. Unit suites remain unrun by this
effort. After the independent review and its corrections, the merge coordinator
owns their scheduling on the composed tree, required promotion checks and
preservation of valid source-bound passes. Run only failed or invalidated
cohorts again; a passing check from a different effective implementation is not
proof of the new behavior.

Use the pinned Rust 1.97.1 environment. The core tests require `hiqlite-store`.

```bash
cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection macos_
cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection cpu_hdr_to_sdr_consumes_only_static_hdr_metadata_in_both_producers
cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection consumed_hdr_metadata_policy_leaves_unaffected_routes_unchanged
cargo test --locked -p plurx-core --features hiqlite-store --lib transcode::recipe::tests::planned_v4_recipe_hash_is_a_golden_fixture
cargo test --locked -p plurx-core --features hiqlite-store --lib transcode::recipe::tests::output_codec_contract_is_in_recipe_identity_without_invalidating_legacy_bytes
cargo test --locked -p plurxd --bin plurxd macos_
cargo test --locked -p plurxd --bin plurxd a_part_with_no_record_resumes_unobserved
python3 tests/operations/test_macos_video_bench.py
python3 tests/operations/test_macos_video_fixtures.py
node tests/web/settings-sections.test.js
```

The web file has its own queued test runner; Node's `--test-name-pattern`
does not select its internal cases. Its correct scheduling granularity is the
whole file. Runtime probe tests have platform-specific cases: a Linux pass is
not execution of the Mac-only child/cache/graph cases. Record the host with each
result. Actual display, source confinement, client playback, concurrency and
soak gaps remain the open acceptance rows above, not unit-test substitutes.

The PR carries concrete `Regression-Test:` fields that must survive in the
landing commit. Superficial test repairs may be made by the coordinator;
behavior-changing findings return to this effort for implementation and review.

## 8. Initial independent implementation review

Three independent reviewers examined candidate `93658e211` against main
`8e242787c`. The core/package and harness/daemon reviewers independently
confirmed the same recovery defect; it is counted once. Runtime/fixture review
found three additional defects. Repair review exposed one further producer
identity mismatch, confirmed by tracing existing launch ownership. No reviewer
ran unit tests or compiled source.

| ID | Priority / owner | Confirmed issue | Disposition |
|---|---|---|---|
| AR-01 | P2 · native builder | Re-resolving already-selected Mac options under required software decode rejects before the existing grade-preserving downgrade; production recovery alternate disappears | Resolved: `85e1d9b24` uses existing CPU downgrade under software restriction; selected-options and stale-readiness regressions compiled; independent core re-review accepted |
| AR-02 | P2 · runtime builder | FFprobe-only dynamic dependencies are not covered by the FFmpeg-derived implementation digest | Resolved: `4e93665ef`; both-program system-only inventories and OS identity; independent re-review and pinned compile/hook passed |
| AR-03 | P2 · runtime builder | Dependency capture/stat awaits escape the identity deadline and cancellation owner | Resolved: `4e93665ef`; one aggregate identity deadline and off-runtime resolution; independent re-review and pinned compile/hook passed |
| AR-04 | P2 · runtime builder | Dropped blocking output writer can rename after cleanup, leaving an unowned probe file | Resolved: `4e93665ef`; writer settles before cleanup; deterministic regressions compiled, not run; independent re-review accepted |
| AR-05 | P1 · daemon builder | Configured FFmpeg symlink can resolve to B while the frozen Mac plan records observed implementation A, including offline parts | Resolved: `181a9494b` binds VOD/rolling/offline launches to the frozen implementation; `575a68abc` requires sealed positive Mac part completion before resume, invalidates stale receipts before launch and refuses failed writes/cleanup. Both deltas independently accepted; pinned compile and normal hooks passed |

Coverage includes all three builders: official package acquisition and every
core plan/recipe change; harness/provenance, daemon manager/settings/auth/UI,
metrics and retry ownership; corpus/generator/runtime probes, private cache,
child lifetime and identity. Cross-builder interfaces were traced. The review
is complete for this source candidate: every correction was independently
checked. Review and compilation do not substitute for coordinator-owned
regression execution or the open M6 acceptance rows.

## 9. Full-scope continuation — 2026-10-08

The initial handoff did not discharge the remaining design. The user directed
work to continue through all implementation and available acceptance work.
Do not close this effort merely because another contribution reaches the
merge queue. Qualification requiring unavailable samples, machines or a
human display assessment remains explicitly open while other work proceeds.

| Builder | Exclusive initial continuation ownership | Immediate acceptance |
|---|---|---|
| Dolby/package | Jellyfin package script, upstream patch/build helpers, Dolby/HLG experiment generators and associated operation regressions | Strict effective per-frame Dolby metadata before rendering; actual VT hardware requirement and observation; reproducible complete package |
| Native parser | `decode_facts.rs`, associated confinement/parser module and regression source, new Wasm parser build/ABI, dependency manifest/lock | Same pinned parser semantics; only held-source read capability and bounded results; immutable module identity, bounded memory/execution and existing cancellation/cache ownership |
| Processing routes | Core transcode/filter/decode/recipe/output contracts; daemon runtime/planning and associated regressions | Observed HLG, processing before existing CPU subtitle burn, native BWDIF cadence, negotiated HEVC SDR/HDR10 and strict P5 recipes without changing promised grade |
| Manager | Existing design/implementation/status/evidence docs, integration, workload scheduling and final review/handoff | Every scope row has evidence or a precise remaining dependency; all builders independently reviewed before queue handoff |

Client HLS capability producers and serialization belong to the Dolby builder.
After the strict package/client commit, that builder also owns the existing
`live_tv.rs` and `live_tv_delivery.rs` integration and narrowly agreed Live
upload work-list/argv branches in `macos_video.rs`. The route builder retains
core Live TV graph types and strict P5 runtime branches; both reuse the same
existing child, deadline and output-validation owners. The native
builder owns signed daemon assembly and actual normalized VOD qualification.
These assignments keep the remaining production work parallel with explicit
file or hunk ownership; the manager resolves any shared-file integration once.

The coordinator explicitly returned the warm compiler target to this effort.
Only one builder owns it at a time. Heavy native builds and hardware timing
experiments are scheduled separately. All three builders have the complete
user constraints: own clones, no unit execution while building, normal hooks,
Jellyfin only, no hidden enable gates, no symptom watchdogs, no confinement
bypass, and no change to deployed packages/services. The coordinator retains
final unit/fast-lane execution and main merge ownership.

The supported native parser design is being changed at its isolation boundary,
not by removing the existing Mac refusal. A capability-limited Wasm build of
the same Jellyfin FFprobe is the implementation candidate: the module receives
only explicit source-descriptor reads and bounded output, with no directory,
network, process or descendant authority. Toolchain feasibility, parser
parity, artifact size and startup costs must be measured before acceptance.
The existing source snapshot, observation cache and producer contracts remain.

The exact Jellyfin candidate contains `bwdif_videotoolbox`; earlier unrelated
Homebrew inventories are not capability evidence. Filter presence alone does
not qualify cadence, captions or performance. Hardware BWDIF and HLG/burn
processing are being exercised before extending the runtime observations.

### 9.1 Integrated foundation and remaining root causes

`b8b7a9574`, integrated by `8a78affb7`, adds independently observed graph
classes for HLG, native processing before the existing text/bitmap burner,
and file-source native BWDIF. New graph classes start unavailable; ordinary
HDR10 observation does not authorize them. The pinned all-target core/daemon
check and normal tracked hook passed. Regression source compiled; unit
execution remains with the merge coordinator.

Correctness experiments observed 12 progressive HLG output frames with BT.709
limited-range signaling; the analytic 203-nit patch mapped to code value 156.
BWDIF produced 12 send-frame or 24 send-field progressive frames with the
corresponding frame rates. The PGS generator needed an explicit 0.25-second
mux offset because the SUP demuxer normalized its first timestamp to zero.
After correcting the fixture, half-alpha red appeared only in frames 3–8 of
12 across SDR8, SDR10 and HDR10. These small controlled observations establish
graph behavior, not full-resolution quality or production performance.

The native parser prototype exposed a performance defect before acceptance:
Wasmi interpretation exceeded a bounded 20-second experiment while processing
ten seconds of synthetic 1080p Main10 video. The production deadline remains
ten seconds. A compiled Wasmtime candidate processed all 240 frames in about
3.28 seconds, with module compilation measured separately. The then-current clean
parser image was 13,709,146 bytes and reports HEVC and both AC-4 streams from a
public broadcast sample. These provisional measurements ran during other
compilation and are not performance qualification. Hardened signing and cold
identity initialization remain unresolved acceptance work: a copied hardened
harness without the required executable-memory entitlement was killed by the
kernel. A supported signing preflight and non-JIT runtime comparison are in
progress; the installed daemon has not changed.

The strict Dolby patch has a separate concrete provenance problem to solve.
The upstream HEVC decoder can attach cached Dolby context when the current
access unit contains no RPU. Renderer-side presence checks alone would then
accept stale metadata. The builder is implementing decoder-owned frame
provenance, with explicit legitimate reuse distinguished from a missing RPU,
before enabling a production P5 route. Reordering, seek/flush and initial or
midstream metadata loss remain acceptance cases.

The Linux source-only compiler loop is also established: Rust 1.97.1 was
verified inside the coordinator-released isolated container. Final integrated
source will be archived without Git metadata or credentials for platform
checks. It does not duplicate the coordinator's unit-test execution.

`e83de7e99`, integrated by `0df5a10f7`, adds the independent HEVC output
preference and Developer advice, the original Dolby/HLG fixture generator,
the strict Jellyfin patch and private reproducible build tooling. Its exact
merged-base all-target compilation and tracked hook passed; units were not
executed. The C patch now compiles into native FFmpeg/FFprobe; the resulting package is
not yet qualified. A positive
software-rendering control changed 908,148 decoded bytes when applying the
generated RPU, establishing pixel influence without claiming mastered-content
visual acceptance or hardware reconstruction.

HEVC client negotiation also needs an optional HLS/fMP4 sample-entry claim.
Existing original-progressive MP4 support is not sufficient evidence for that
transport. The route builder owns the core/session contract; the Dolby builder
owns narrow Apple/Android/web capability serialization as applicable, based on
actual transport/decoder checks. An absent claim preserves the existing AVC
choice and does not prevent saving the independent HEVC preference.

The initial #882 contribution landed on main as `339abbced` at
2026-10-08T05:36:18Z. This continuation incorporates its K05/K06 ownership,
API documentation, regression-contract and erased checkbox-type repairs.
Only the stale status paragraph conflicted; it now records the completed
initial review and active continuation instead of restoring an obsolete
active-review claim. No builder source was discarded to resolve the merge.

### 9.2 Native package and representative parser checks

The full strict Jellyfin native build completed. Two build failures had
specific packaging causes: GNU libtool shadowed Apple's archive combiner in
the x265 recipe, and applying an upstream deletion without `patch -E` left an
empty C file that make selected ahead of its replacement Objective-C file.
The reusable preparation helper now fixes those tool/deletion semantics.
Incumbent x265 support was retained. Strict positive/reuse/loss controls are
running against the compiled CPU and Metal filters; a successful build alone
does not admit production P5.

The parser's real broadcast check is stricter than the synthetic 24-fps case:
600 Main10 1080p frames representing ten seconds at 59.94 fps took 15.35
seconds in the scalar compiled module while other compilation was active.
That exceeds the unchanged ten-second production execution deadline. The
pinned Jellyfin source contains standard SIMD128 HEVC kernels; the builder
is enabling them and requiring configure to actually report SIMD availability.
A first attempt exposed a missing `-msimd128` compiler flag and is retained as
a failed configuration, not a SIMD result. Relaxed SIMD remains disabled.
Cold module compilation has a separate existing identity deadline; the two
deadlines must not be conflated. Idle-host and final-module measurements
remain open.

The 600-frame progressive broadcast decode is an explicit stress experiment.
The existing production collector runs `idet` only for sources whose held
metadata reports interlace; ordinary progressive sources require metadata
projection, not a ten-second pixel scan. That admission rule is unchanged.
An optional `idet` deadline miss yields `IdetUnavailable` while retaining
otherwise attested facts. Real 1080i TFF/BFF and progressive normalized VOD
remain the primary production checks; neither is replaced by the stress run.

The native parser foundation is committed as `35f7a8cf6` and integrated by
`d1683a682`. Its pinned Rust check and normal catalog/format/Clippy/JavaScript
hook passed; units remain unrun. The final SIMD module is 15,311,140 bytes,
SHA-256 `36f7e0298eac14d1a664dfc083702bab812f6c83d196c651420af92f1080f21c`.
All 600 decoded frame and plane checksums on the broadcast sample match native
FFprobe. A copied hardened harness with the intended entitlement succeeds;
the same harness without it now fails through the signing preflight instead
of being killed by the kernel. Actual daemon assembly/delivery remains a
separate pending check. The compiler-free parser window below records final-module timing separately
from actual daemon acceptance.

The compiler-free parser window passed field-marked 1080i cases with the signed
final runtime: 300 TFF coded frames at 29.97 fps took 2.512 seconds for `idet`,
and 250 BFF frames at 25 fps took 1.517 seconds. Metadata projection took
roughly 0.002 seconds each. Fresh eager module compilation took 7.69–7.84
seconds, under its separate ten-second identity budget. Encoded parity and
all observed frame verdicts agreed. No competing task compiler/encoder ran
during the timed sections. The thermal query was unavailable; aggregate load
averages include preparation and JIT work, so these are bounded feasibility
results, not capacity or thermal qualification. A later synthetic 4K MPEG-2
field-marked case completed `idet` in 5.901 seconds and matched native counts
(167 TFF, 73 undetermined); this does not qualify 4K HEVC or HDR decoding.

### 9.3 Earlier Dolby reuse acceptance withdrawn

The deeper same-context flush trace invalidated the earlier reuse-positive
claim. That fixture omitted Dolby DM color data; pinned FFmpeg substituted
`ff_dovi_color_default` (observed peak 3696 and signal EOTF 39322) instead of
reusing valid Profile 5 color semantics. Completing 24 frames was therefore
not proof of correct reuse. The old immutable strict package is retained as
a superseded P5 diagnostic and must not authorize final P5 runtime admission.
Its ordinary AC-4 comparison remains evidence for those exact binary bytes.

The repair exercises explicit mapping reuse with P5 DM color data, adds
an omitted-color negative, and makes the opt-in strict renderer reject the
default/non-P5 color semantics. Default and unrelated routes remain unchanged.
The corrected package and repeated affected controls are recorded in §9.4;
the superseded package does not inherit those results. Fresh and varying-DM positives and
missing/malformed-RPU negatives remain recorded individually; they do not
rescue the invalid reuse claim. HEVC source compilation continues separately
while this root cause is repaired.

Dolby's [public profile table](https://ott.dolby.com/OnDelKits/Dolby_Vision_Online_Delivery_Kit/v1/Documentation/Specs/Visio_Profiles/help_files/topics/c_dovi_profiles_public.html)
specifies no metadata compression for Profile 5. The generated mapping-reuse
case is therefore a decoder robustness experiment, not a conforming P5
positive or mastered-content proof. Fresh per-frame P5 and changing-DM
fixtures are the main synthetic positives. The original plan's reuse check
must record this scope correction instead of inventing standardized P5 reuse;
flush-state and unsupported-metadata negatives remain required.

### 9.4 Corrected P5 layout and hardware reconstruction

The corrected synthetic P5 sources use Dolby's preferred IPT-C2 VUI matrix
value 15. Pinned swscale does not support that matrix during planar/P010
layout conversion. The strict adapter removes only that layout-stage tag
before rearranging samples; mandatory parsed P5 RPU metadata remains the sole
color interpreter. It is not a generic unknown-color fallback. Native
VideoToolbox-to-Metal needs no layout adapter.

On fresh and varying-DM 24-frame fixtures, software decoding and required
hardware VideoToolbox decoding produce byte-identical planar samples:
4,147,200 bytes each, maximum and mean difference zero. Using the same strict
CPU mapper after each decode path also gives byte-identical output:
2,073,600 bytes each. This establishes sample preservation for these corrected
fixtures; it does not establish calibrated visual quality or all-source
coverage. Strict positive/negative controls and the replacement package
remain part of the final production admission work.

The native builder has produced a separate 120-second 2160p24 SDR source for
actual normalized VOD delivery: 2,880 decoded video frames and 48 kHz stereo
AAC. Its driver uses ordinary scanning/session APIs and explicitly verifies
the normalized 1440p candidate, rather than accepting an old non-normalized
route as a pass. Signed daemon assembly, seek/resume/cancel and paced delivery
are pending the combined source and final strict package.

The repaired immutable package now passes all 48 strict graph controls across
software/hardware decoding and CPU/Metal rendering, plus eight true decoder
seek/flush controls. The renderer checks P5 DM color fields (`signal_eotf=65535`,
`signal_bit_depth=12`, `signal_color_space=2`, `signal_chroma_format=0`,
`signal_full_range_flag=1`); the coded video samples remain 10-bit. It does
not blacklist the particular default peak value. Nonconforming mapping reuse
remains labeled a parser diagnostic. The manager independently verified the
final binary/module/manifest hashes and repeated the affected AC-4 candidate:
its five-second PCM output is byte-identical to the retained original Jellyfin
reference. The [evidence document](MACOS-VIDEO-PROCESSING-EVIDENCE.md#91-repaired-final-package-preserves-the-same-ac-4-output)
records exact hashes and scope. Strict P5 production recipes and actual
daemon delivery still require completion.

The corrected package and client source is committed as `a542ab140` and
integrated by `6978c9a5d`. It includes actual HLS transport claims for web,
Apple and Android, strict color/default rejection, fixture association and
flush tooling, and independent native/host HEVC readiness advice. Pinned Rust
compilation, Apple iOS/tvOS and test-source compilation, and Android app/test
APK/test-source compilation passed; no unit tests were executed by this
effort. Live TV integration now proceeds in the Dolby builder's owned files.

The first optimized daemon dependency build passed on `d1683a682` in 17 min
23 s. It is used for an early bounded signed-daemon correctness smoke to
expose integration faults while the remaining routes are built. This older
source is not final-candidate qualification. The frozen integrated source
will be rebuilt before the final startup/concurrency/soak measurements; no
performance result is claimed while compilers are active.

### 9.5 HEVC catalog integration and normalized daemon findings

`432a617c2`, integrated by `7fa940953`, adds separately observed native and
host-memory HEVC SDR/HDR10 graphs and additive H.264/HEVC candidates. The
server restores the chosen candidate's codec before resolving its immutable
recipe; a saved preference does not overwrite an already-selected contract.
The optional `planned_codec` describes output and grants no authority. Full
source and recipe validation remains required. The source includes actual
catalog reconstruction and old-worker mismatch regression cases; the pinned
check and normal hook passed, with no unit execution.

The existing continuous family explicitly promises AVC, including its client
codec envelope. Those candidates are preserved. The new HEVC route targets
finite immutable VOD; an actual accepted producer and matching manifest must
still be demonstrated before claiming its delivery complete. The saved HEVC
preference remains independent and accepted.

The early signed `d1683a682` daemon successfully scanned the normal source,
selected normalized 2560×1440 output from 3840×2160, and delivered 48 decoded
frames with that canvas. However, its diagnostics show
`presentation_constraint` and CPU processing despite the enabled preference
and available SDR graph. This proves parser/geometry integration, not native
processing. The builders are tracing that exact admissibility boundary.
A subsequent control request returned 400; the experimental driver protocol
is being corrected separately. Neither result is counted as final-candidate
playback qualification.

### 9.6 Live moving-field check exposes a pinned filter defect

Twelve tiny upload/scale/BWDIF cases and four paced 1080i TFF/BFF cases passed
geometry, cadence and static gray checks. A subsequent independently generated
moving-field pattern did not preserve the expected BFF/second-field artwork.
Those earlier receipts therefore establish execution and cadence only; they
do not qualify motion/parity correctness.

The pinned `vf_bwdif_videotoolbox.m` binds its parameter buffer at Metal index
4, while the shader declares parameters at buffer index 0. Texture indices
use a separate namespace. The corrected binding passes eight raw CPU/native motion comparisons and
twelve encoded motion controls across ordinary file and Live TV graphs. Four
negative controls reject the older package using the same final motion guard.
The runtime observations now include that moving-field check. Broken native BWDIF
must remain unavailable even when static patches and timestamps pass.

A new immutable native package will carry the correction; the existing
`85b1ab41…` package and its strict P5/AC-4 receipts remain unchanged and
hash-bound. Only affected controls are repeated during repair. Final runtime
qualification will identify the corrected artifact rather than silently
attributing old binary results to it. Progressive upload/scale is independently
observed and does not borrow interlace acceptance.

### 9.7 Effective encoder and actual native normalized delivery

The normalized profile correction is committed as `656407c0f`, integrated
by `46f31ec6e`. Its pinned core check and normal workspace hook passed. The
ordinary finite HEVC preference correction is `e3034c73d`, integrated by
`b4bc77b54`; it uses the existing selection owner and saved HEVC preference,
without requiring the separate display-aware Auto switch. It preserves exact
requested candidates, existing cache/sustainability decisions and the AVC
continuous family. Unit execution remains with the coordinator.

A subsequent daemon run still chose CPU because the existing Auto encoder
benchmark selected software x264, despite valid VT capability. That is a
distinct encoder-policy boundary, not failure of the normalized geometry fix.
The controlled hardware-path run uses the existing documented VideoToolbox
preference, with normal probes/source admission intact. Developer advice is
being updated to expose the effective-encoder prerequisite. The new processing
and HEVC preferences do not override an explicit software encoder.

With that preference, the signed `46f31ec6e` daemon selected the actual native
normalized graph and delivered 48 decoded 1440p frames. An active session kept
its captured candidate/media after the processing switch changed; a new
disabled plan selected CPU processing. The disabled CPU reference times out before producing initialization bytes,
even after releasing the original session. Existing producer diagnostics
establish the cause: the experiment configured a two-thread software pool,
but the 4K CPU-decode route requests eight threads. Admission reports
`over_budget=true`, and no FFmpeg child starts. The driver is correcting that
ordinary resource setting; production admission and deadlines stay unchanged.
The previous timeouts remain evidence of configured-budget refusal, not a
hardware-capacity limit. Full final-candidate qualification remains in progress.

### 9.8 Live TV and corrected-package integration

`74aa40592`, integrated by `d403ee6ea`, freezes independently observed upload
graphs through the existing Live TV plan, captures the exact executable and
package identity, and revalidates before launch/warm publication. Unknown
source facts retain incumbent behavior. The existing lifecycle owns
cancellation, reconnect and format-change recovery; no watchdog was added.

The same commit includes the minimal pinned BWDIF binding correction, moving
field controls for ordinary and Live graphs, encoded square-pixel P5 fixtures,
local versioned dependency source offers and expanded Developer advice.
The final pinned workspace all-target check passed in 16.42 seconds; the
normal hook passed catalog, formatting, Clippy and 77 served JavaScript files
in 1 minute 41 seconds. No unit tests ran. The supplementary combined-package
archive retains exact positive/negative controls and selected P5 refreshes;
actual daemon Live TV start/stop/reconnect remains separate work in progress.

### 9.9 Actual delivery exposes remaining integration faults

The signed `46f31ec6e` daemon completed 120.010 seconds of paced native
normalized delivery (60 objects), plus seek/resume/cancel and active-recipe
immutability. With the normal eight-thread CPU pool, 48-frame CPU/native
comparisons pass: paired patch difference at most one code value and
whole-frame PSNR minimum 51.92 dB, mean 53.23 dB. Earlier admission failures
remain recorded as experiment configuration failures.

A finite HEVC candidate produced actual `hvc1` Main 1920×1080 BT.709 media
and 48 decoded frames, but the master endpoint returned `hls_init_invalid`.
The cause is identified: the existing ladder codec-family helper falls back
to `avc1` for SDR and therefore sends the HEVC initialization to the AVC
inspector. The correction selects the family from the immutable output
contract, then retains exact codec/profile derivation from the served init.
Valid decoded media alone is not complete manifest acceptance. Separately,
the older daemon's embedded HLG, SDR text-burn and ordinary BWDIF observations
returned `output_contract_failed` with the corrected package. Final moving
field controls may supersede the older BWDIF fixture; each remaining runtime
failure still needs diagnosis. The HLG source is missing encoded square-pixel SAR, which the observation
requires. Its narrowly regenerated fixture now has explicit SAR while all
decoded planes remain unchanged; complete-graph acceptance is still being
checked. Standalone graph success does not substitute for the embedded
production observation. These findings remain open, and
there is no final-candidate completion claim.

### 9.10 Runtime-observation repairs and symbol packaging

Matched dSYM packaging is integrated by `5b23539dc` from `ddfa81888`. The
normal hook passed; an actual signed package preserves the daemon's UUID and
copied symbol hashes. Mismatched UUIDs and unsafe symlink members have focused
regression source. Unit execution remains deferred to the coordinator.

A bounded Rust diagnostic parses the actual HEVC fragment successfully,
including complete parameter sets and the single-sample-description layout.
This confirms the master failure is codec-family dispatch, not malformed
HEVC initialization; structural validation will remain unchanged.

The regenerated HLG graph produces twelve square-pixel frames with the
expected reference-white value 156. SDR text rendering is also present: a
bright stationary source background masks the glyph in the old peak-only
comparison. The repair checks cue-versus-blank change in a stationary region
outside the moving marker, with missing/stale-cue negative controls. These source corrections are committed as `21110a5`, integrated by
`d92700dea`. Exact all-target compilation passed in 13.37 seconds and the
normal hook in 1 minute 34 seconds; no units ran. Actual missing/stale-cue
negatives pass the new guard. Final combined daemon observations remain
in progress.

### 9.11 HEVC correction integrated; final declared graph gaps

`ce443c20a`, integrated by `720676da5`, selects the finite HEVC codec family
from the immutable plan, retains existing initialization/parameter-set/sample
entry validation, and freezes actual-init HEVC facts. The actual retained
init parses correctly; no core fMP4 parser relaxation was needed. Pinned
daemon all-target compilation passed in 1 minute 21 seconds and the normal
workspace hook in 1 minute 40 seconds. The focused regression source covers
real resolved-plan/init binding and incomplete-parameter refusal; no units
ran. Final daemon master acceptance still awaits the combined binary.

The strict P5 combined source now compiles and is entering 45 bounded
correctness controls across nine sources and five decoder/renderer/encoder
tuples. A narrowly extended normalized source predicate will admit progressive
HDR10 into the existing normalized 1440p SDR-AVC contract; all other geometry,
rate, pixel and burn restrictions remain. Its final daemon proof is required.

The complete graph inventory also exposed four declared but unobserved
combinations: HLG text/bitmap burn and HDR10/HLG frame BWDIF. The fixture
builder is completing independent observations for those combinations; a
concrete unsupported result must retain the incumbent and be recorded, never
be represented as an implemented available graph. The normal synthetic tuner
protocol preflight has succeeded through the existing private-network rules;
its temporary container and port listener were removed immediately afterward.

### 9.12 Final graph scope and current-main integration

`f57040d83`, integrated by `3ea037239`, completes strict P5 production
projection and progressive HDR10 admission into the normalized SDR-AVC
contract. Forty-five standalone controls pass across nine strict sources and
five decoder/renderer/encoder tuples. `0f8a63e62`, integrated by `6e5015385`,
adds the two complete HLG burn observations. Exact High10 HDR woven-field
controls fail hardware decoding, so those inputs retain the incumbent;
unreachable HDR BWDIF classes are removed. Twenty-two supported graph
classes remain.

Main `c746b6c9b` adds a Pi software-frame pipeline. Integration preserves the
18-pipeline union and gives it a distinct metric slot. The strict P5 API
preparation also exposed an older RPU-only compatibility guard that rejected
already-authorized strict graphs; the guard now admits those graphs while
retaining generic CPU and unsupported Dolby refusals. Existing plan debug
records expose the strict-policy boolean and captured identity digest for
qualification without adding another owner. The exact combined all-target
Rust check passes in 13.48 seconds on pinned Rust 1.97.1. Regression source
covers the guard and stable metric slots; unit execution remains deferred.

### 9.13 Draft PR and independent implementation review

Candidate `2979112df` is pushed in draft PR #904, with all regression anchors
in its description. The normal hook passes (Clippy 2 minutes 27 seconds,
77 served JavaScript files). The merge coordinator has acknowledged the draft
and will not queue tests before the reviewed handoff.

Independent security review found A1 native executable/provenance mismatch
acceptance during packaging and A2 immutable snapshot leakage on parser
initialization failure. Both are assigned to their existing native owners;
repairs and independent re-review are required before acceptance. The
superseded release build was stopped, preserving its warm target. Core
contract review and platform compilation continue in parallel.

The native builder repaired A1/A2 in `9acf814a8`; its pinned all-target check
and normal hook pass, and the corrections are integrated for independent
re-review. Core review adds B1 selected-graph P5 metadata/color observation
and B2 legal trailing EOS/EOB handling. Client review adds C1 Android measured
HEVC capabilities when Display-aware Auto is off. Each remains owned by its
existing builder. Candidate `2979112df` also passes source-only Linux all-target
check and Clippy; subsequent affected source must be rechecked. iOS/tvOS
compile-only builds pass on the same tree; unchanged Android source retains
its prior app/test APK compilation evidence.

C1 is repaired in `0d6a466cd` and independently accepted by the delivery
reviewer. Android app and instrumentation APK compilation pass in 1 minute
5 seconds, explicit JVM regression compilation in 48 seconds, and the normal
hook in 1 minute 35 seconds. B2 is reproduced before repair: legal EOS, EOB
and combined terminators each yield 21 frames/exit 183 under exact strict
flags, while the unchanged positive yields 24 frames/exit 0. The native fix
will skip only those legal terminators when locating the current RPU.

Independent re-review accepts A1/A2 in `9acf814a8`. The strengthened B1
selected-graph observer passes all 45 current controls: ten positive tuple/
source pairs expose 24 actual renderer-input PTS/metadata records and six
independent colored-patch bounds per frame; 35 strict-loss controls still
fail at the required boundary. An initial external log-token parsing error
is retained as a failed diagnostic and corrected before the accepted run.
The rebuilt package and legal trailing-NAL controls remain in progress.

- Final strict-P5 runtime registration `54c3a3fe8` independently accepted:
  twelve original analytic sources × five graph controls = sixty controls,
  all passing on `c8c4b525…`. Existing strict-loss cases and the 210-second
  aggregate deadline remain unchanged. All five final review findings and
  their source follow-ons are accepted; final daemon qualification remains.

### 9.14 Final contribution closure

All supported production implementation and assigned builder repairs are
complete. The final signed package passes available-host runtime checks;
startup sampling and the corrected four-session demand soak complete the
remaining local experiments. Evidence §§24–28 retain cache, compilation,
package, Live and timing/soak results with explicit limits. Independent review
accepts every production change, including the two runtime-discovered root
causes. Unit execution and main landing belong to the designated merge
coordinator. No hidden feature gate or additional watchdog was added.

Task-installed MetalToolchain27A266a is removed; final runtime checks pass
without it. All cache images, private daemons and tuner containers/listeners
are removed. Remaining task-owned clones, targets and private experiment
files are removed after durable handoff; PR904 records that final cleanup
confirmation. User checkouts, credentials and the shared compiler target
are outside cleanup scope.
