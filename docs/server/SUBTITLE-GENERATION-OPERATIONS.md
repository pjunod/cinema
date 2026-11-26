# Generate subtitles offline — operations

**Status:** current · **Updated:** 2026-11-26 · Moved from [Operations](../OPERATIONS.md#generate-subtitles-offline) to keep that page under its size ceiling.

Offline generation is optional and runs locally on CPU. In
**Settings → Maintenance → Generate subtitles**, open **Configure transcription**
and choose Managed mode. The service package supplies the recognizer; choose
**Base English** (`base.en`) for English speech or **Base multilingual**
(`base`) for its listed spoken languages. Save the enable choice and sweep interval. Saving is allowed
before any model is installed; readiness explains what work must wait for.

Choose named workers, then **Install**. Each accepted worker gets its own
Activity job. The job downloads the server-pinned model, checks its bytes and
runs a speech diagnostic automatically. Wait for **Verified** on a current
worker; a missing or stale observation means unknown readiness, not confirmed
absence. **Test** requests verification again. Restart retains model bytes but
requires automatic verification for the new boot before ready. There is no
model-removal control in this release.

Stop cancels the durable operation and prevents its success/readiness receipt.
Complete checksum-valid bytes may remain as unverified download cache after
Stop converges; this is not an installed/Verified model. Use a fresh explicit
Install or Test to finish setup. Restart may reverify an unchanged model that
already has a successful receipt; it cannot complete a cancelled first install
or replacement merely because its cache file exists. Stop does not uninstall
an unchanged model that was already accepted before the stopped operation.

In the web player, on a local title or while it is playing, choose **Generate subtitles**, select
the exact audio track and its spoken language, then submit. An untagged track
needs an explicit language; an English-only model cannot recognize another
language. The accepted request keeps this source/audio/language/model identity
even if Settings later changes. Activity shows the exact durable job and Stop.
Queued work explains missing model, media access, worker readiness or playback
capacity rather than inventing a progress percentage.

Web completion adds the matching track to the subtitle picker without reloading
the page or changing the current Off/track selection. Choose **Use** to enable
it. The video element and playback position remain owned by the current player.
Source replacement, a stale worker lease or Stop cannot publish for a different
source. Captions and provenance persist in the acquired-caption store; source
media stays unchanged. Android direct playback cannot yet attach an acquired or
newly generated external caption without rebuilding playback; native playback
qualification remains open in the [usability ledger](../features/SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md#qualification-ledger--integration-snapshot-2026-10-15).

Interval zero permits manual requests only; a positive interval scans bounded
pages for captions missing in the configured known language. Disabling or
switching modes prevents new admission; an already-owned attempt can finish
unless Stop, shutdown or foreground resource pressure interrupts it. Shared
playback admission gives foreground work priority.

Advanced mode preserves the command and absolute model path for an operator's
existing installation. Saving it explicitly activates that mode and pauses
Managed admission. Managed activation preserves the Advanced values and
atomically pauses the legacy configuration. Older servers or stale legacy
settings pages are competing administrative writers: a conflicting legacy
write is visible and blocks new Managed work until Settings is reviewed and
saved. Do not assume universal mixed-version atomicity.

The admission ceiling is four hours of finite, probed audio/video. This is a
safety bound, not a qualified four-hour workload or throughput promise. Recognition
uses two CPU threads and no GPU. Managed processing rejects exact digital
silence and bounds cue times to decoded media; it does not remove every
hallucination or guarantee accuracy. A silence refusal means choose another
audio track. Recognition language is the speech language, not a translation
target. See the [implementation and qualification ledger](../features/SUBTITLE-GENERATION-USABILITY-IMPLEMENTATION.md)
for the current platform, workload and client evidence; incomplete acceptance
is not a reason to overwrite the saved enable choice.
