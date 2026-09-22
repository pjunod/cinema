package tv.plurx.app.player

import java.util.Locale
import tv.plurx.app.data.DynamicRange
import tv.plurx.app.data.HdrType
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.Rung

/**
 * Every playback decision this client makes on its own, as pure functions.
 *
 * They live together and stay free of ExoPlayer and Android so the tables are
 * one screen of code each, unit-testable on the JVM, and editable in one
 * place: the height promise is one `when`, and the rescue is one predicate.
 *
 * The subtitle half of the contract — which track to show without being asked,
 * where a selection has to be carried, the rendition ordinals, and the body a
 * session create posts — lives next door in `SubtitlePolicy.kt`. It is one
 * question ("can the server serve this as a WebVTT rendition?") answered in one
 * place (`SubTrack.isNativeHls`), and splitting it across two files is how the
 * menu, the routing, and the ordinals came to disagree.
 */

// ---- §5.3 What the menu says ------------------------------------------------

/**
 * The `force` query parameter for `/decision`.
 *
 * Anything the server does not recognise parses as `Auto` (`Force::parse`), so
 * sending a bare rung — as this client used to — silently did nothing for
 * direct and remux verdicts. A rung is a request to *transcode*; how tall is
 * the session create's business, not the verdict's.
 */
internal fun decisionForce(quality: PlaybackQuality): String = when (quality) {
    PlaybackQuality.Auto -> "auto"
    PlaybackQuality.Original -> "original"
    else -> "transcode"
}

// ---- §4.2 The fidelity-preserving compatibility ladder ----------------------

internal enum class PlaybackErrorAction {
    /** Real HDR has already rendered: reconnect without changing its recipe. */
    RetrySameHDRDelivery,

    /** Keep the video/DV intact but give the decoder a normalized MP4 envelope. */
    RetryAsDolbyVisionRemux,

    /** Reopen as a forced transcode at the current position — H.264/AAC, guaranteed. */
    RetryAsCompatibilityTranscode,

    /** Out of rescues: show a real failure state instead of a frozen surface. */
    Fail,
}

/**
 * A direct Dolby Vision stream first gets a lossless container rescue. Some
 * Android decoders advertise the right profile but reject its source envelope;
 * normalizing that to MP4 is cheaper and preserves DV. The existing H.264
 * compatibility transcode remains the final rescue, and is attempted once.
 */
internal fun playbackErrorAction(
    deliveryMode: String,
    preservesDolbyVision: Boolean,
    remuxRescueAlreadyUsed: Boolean,
    transcodeRescueAlreadyUsed: Boolean,
    mediaCompatibilityFailure: Boolean = true,
    deliveredRange: String? = null,
    establishedPlayback: Boolean = false,
    sameHdrRetryAlreadyUsed: Boolean = false,
): PlaybackErrorAction = when {
    establishedPlayback &&
        deliveredRange?.lowercase() in setOf("dolby_vision", "hdr10", "hlg") &&
        !sameHdrRetryAlreadyUsed -> PlaybackErrorAction.RetrySameHDRDelivery
    establishedPlayback && deliveredRange?.lowercase() in
        setOf("dolby_vision", "hdr10", "hlg") -> PlaybackErrorAction.Fail
    !mediaCompatibilityFailure -> PlaybackErrorAction.Fail
    deliveryMode == "direct" && preservesDolbyVision && !remuxRescueAlreadyUsed ->
        PlaybackErrorAction.RetryAsDolbyVisionRemux
    deliveryMode in setOf("direct", "remux") && !transcodeRescueAlreadyUsed ->
        PlaybackErrorAction.RetryAsCompatibilityTranscode
    else -> PlaybackErrorAction.Fail
}

/**
 * Media3 error families are intentionally disjoint: 2xxx is I/O, 3xxx is
 * parsing, and 4xxx is decoding. Only container rejection and decoder failure
 * can be repaired by changing the encode. Network, HTTP, timeout, manifest,
 * and temporary resource errors stay on the current delivery.
 */
/**
 * Errors a *different node* could plausibly answer differently.
 *
 * Deliberately an allowlist. "Not a codec failure" is not the same question:
 * a 404 on an ended session, a 401, a DRM refusal and a behind-live-window
 * error are all reproducible on every node, and walking the whole ingress
 * list for one of those buys nothing but a full player prepare per node
 * before the viewer sees the error they were always going to see. Only the
 * 2xxx transport family can be answered by moving the request.
 */
internal fun isTransportPlaybackError(errorCode: Int): Boolean = errorCode in setOf(
    2000, // ERROR_CODE_IO_UNSPECIFIED
    2001, // ERROR_CODE_IO_NETWORK_CONNECTION_FAILED
    2002, // ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT
    2003, // ERROR_CODE_IO_INVALID_HTTP_CONTENT_TYPE
    2004, // ERROR_CODE_IO_BAD_HTTP_STATUS — only 5xx reaches this in practice;
          // a 4xx capability answer is terminal and is filtered at the call site
    2007, // ERROR_CODE_IO_NO_PERMISSION
    2008, // ERROR_CODE_IO_CLEARTEXT_NOT_PERMITTED
)

internal fun isCompatibilityPlaybackError(errorCode: Int): Boolean = errorCode in setOf(
    3001, // ERROR_CODE_PARSING_CONTAINER_MALFORMED
    3003, // ERROR_CODE_PARSING_CONTAINER_UNSUPPORTED
    4001, // ERROR_CODE_DECODER_INIT_FAILED
    4002, // ERROR_CODE_DECODER_QUERY_FAILED
    4003, // ERROR_CODE_DECODING_FAILED
    4004, // ERROR_CODE_DECODING_FORMAT_EXCEEDS_CAPABILITIES
    4005, // ERROR_CODE_DECODING_FORMAT_UNSUPPORTED
)

// ---- M5: the two bounded recovery additions this client owns ----------------

/**
 * The create "not yet" retry ladder (PLAYBACK-SURFACE-CONTRACT.md §3.3 row 6,
 * implementation plan §4.6).
 *
 * Restated from `crates/plurxd/src/web/playback-policy.js` (`CREATE_RETRY`),
 * because all three clients have to agree on these numbers and only one of them
 * can run that file; `tests/playback/web-policy.test.js` reads this declaration
 * back out of the Kotlin and fails if a number drifts.
 */
internal object CreateRetry {
    /** The review's ladder, as a CLOSED list: after the third retry it is spent. */
    val backoffMs: List<Int> = listOf(1_000, 2_000, 4_000)

    /**
     * ABSOLUTE, measured from the FIRST attempt — not per attempt. A server
     * that holds every create for three minutes cannot stretch the sequence,
     * which is why the review asked for a deadline rather than a retry count.
     */
    const val DEADLINE_MS: Int = 60_000

    /** The fixture's `create_503_not_yet` codes, and only these. */
    val codes: Set<String> = setOf(
        "startup_timeout",
        "media_owner_transition",
        "vod_index_pending",
        "vod_engine_unattested",
        "transcode_capacity_pending",
    )
}

internal sealed interface CreateRetryStep {
    /** Not a "not yet" answer: the caller's existing handling stands. */
    data object Fail : CreateRetryStep

    /** Wait this long, then re-post the SAME request identity. */
    data class Retry(val delayMs: Int) : CreateRetryStep

    /** Nothing left: stop the player, then raise `exhausted`. */
    data class Exhausted(val reason: String) : CreateRetryStep
}

/**
 * [attempt] counts retries already made (0 before the first one); [elapsedMs]
 * is measured from the first attempt.
 *
 * The deadline is checked twice on purpose: once for time already spent, and
 * once for the retry that would START after it. Scheduling an attempt that
 * could only begin past the deadline is how an "absolute" bound turns back into
 * a per-attempt one.
 */
internal fun createRetryStep(
    attempt: Int,
    elapsedMs: Long,
    isNotYet: Boolean,
): CreateRetryStep {
    if (!isNotYet) return CreateRetryStep.Fail
    val spent = maxOf(0L, elapsedMs)
    if (spent >= CreateRetry.DEADLINE_MS) return CreateRetryStep.Exhausted("deadline")
    val index = maxOf(0, attempt)
    if (index >= CreateRetry.backoffMs.size) return CreateRetryStep.Exhausted("ladder_spent")
    val delayMs = CreateRetry.backoffMs[index]
    if (spent + delayMs >= CreateRetry.DEADLINE_MS) return CreateRetryStep.Exhausted("deadline")
    return CreateRetryStep.Retry(delayMs)
}

/** `ERROR_CODE_BEHIND_LIVE_WINDOW`. Restated rather than imported for the same
 *  reason the colour constants below are: this file stays free of ExoPlayer. */
internal const val ERROR_CODE_BEHIND_LIVE_WINDOW = 1002

/**
 * Does a `BEHIND_LIVE_WINDOW` error get M5's seek-and-prepare recovery?
 *
 * Finite timelines ONLY. Media3's own answer is `seekToDefaultPosition()`,
 * which is a LIVE-EDGE policy: on a finite timeline it jumps to the start of
 * the window and skips content the viewer has not watched. The contract forbids
 * it outright, so the recovery here is `seekTo(lastRealPosition)` + `prepare()`
 * and it is offered only where that position means something.
 *
 * A live item keeps today's `Fail`: there is no last real position on a window
 * that has moved past the viewer, and inventing one is the skip this rule
 * exists to prevent.
 *
 * Once per attach. [used] is that attach's spent budget — a second 1002 on the
 * same item is the recovery not having worked, and repeating it is a loop.
 */
internal fun behindLiveWindowRecovers(
    errorCode: Int,
    live: Boolean,
    used: Int,
): Boolean = errorCode == ERROR_CODE_BEHIND_LIVE_WINDOW && !live && used < 1

/**
 * M5 addition 3, as an object a test can hold.
 *
 * `Controller` cannot be constructed in a JVM unit test — it takes an
 * `ExoPlayer`, a `MediaSession` and the app's view model — so the recovery
 * lives here, with the player reached through three lambdas, and the test
 * drives it directly. What it pins is exactly what the addition claims: ONE
 * `seekTo(target)` and ONE `prepare()` for the first qualifying error, nothing
 * for the second on the same attach, and a fresh budget when a new item takes
 * the screen.
 *
 * The budget is per ATTACH, and "attach" means the item the viewer is looking
 * at changed — a `setMediaItem` + `prepare`, or a prepared successor being
 * committed onto the surface. The recovery itself re-prepares WITHOUT a new
 * item, so it cannot refill its own budget.
 */
internal class BehindLiveWindowRecovery {
    private var used = 0

    /** A new item is on screen: this attach gets its own single recovery. */
    fun attached() {
        used = 0
    }

    /** Whether this attach has already spent its recovery. For the ledger and the tests. */
    val spent: Boolean get() = used >= 1

    /**
     * `true` when the recovery ran. [seekTargetMs] is a lambda so the position
     * is read only on the path that actually seeks.
     */
    fun recover(
        errorCode: Int,
        live: Boolean,
        seekTargetMs: () -> Long,
        seekTo: (Long) -> Unit,
        prepare: () -> Unit,
    ): Boolean {
        if (!behindLiveWindowRecovers(errorCode, live, used)) return false
        used += 1
        seekTo(seekTargetMs())
        prepare()
        return true
    }
}

// ---- §5.6 The quality menu is the server's ladder ---------------------------

internal data class QualityOption(val quality: PlaybackQuality, val label: String)

/**
 * Auto, Original, then the rungs the server actually advertised for this
 * source — never an upscale, and never a rung the ladder dropped. An empty
 * ladder means an older server, so the stored enum is the fallback menu.
 */
internal fun qualityOptions(ladder: List<Rung>): List<QualityOption> {
    val head = listOf(
        QualityOption(PlaybackQuality.Auto, PlaybackQuality.Auto.label),
        QualityOption(PlaybackQuality.Original, PlaybackQuality.Original.label),
    )
    if (ladder.isEmpty()) {
        return PlaybackQuality.entries.map { QualityOption(it, it.label) }
    }
    val rungs = ladder
        .sortedByDescending { it.height }
        .mapNotNull { rung ->
            val quality = PlaybackQuality.entries.firstOrNull { it.rungHeight == rung.height }
                ?: return@mapNotNull null
            QualityOption(quality, rungLabel(quality.label, rung.total_kbps))
        }
        .distinctBy { it.quality }
    return head + rungs
}

// ---- Badges M3: what grade is actually on screen ----------------------------

/**
 * The grade this device is *rendering*, from the grade the server says it is
 * *delivering* plus the two local signals Android actually has
 * (MEDIA-BADGES-PLAN.md §2.2). A reporter: nothing here changes a decision, a
 * capability claim, or a byte of playback.
 *
 * ```
 * delivered == null                 → null   (no session answer yet; source-only badge)
 * the panel shows no HDR at all     → sdr    (an HDR stream on an SDR screen is SDR)
 * the decoder says something        → that   (it is looking at the actual samples)
 * otherwise                         → delivered, gated by what the panel can show
 * ```
 *
 * The decoder wins over the server because it is downstream of every fallback:
 * a Dolby Vision copy that the device quietly played as its HDR10 base reports
 * `COLOR_TRANSFER_ST2084` with a non-DV sample MIME, and the badge should say
 * "DV → HDR10" rather than repeat the server's plan back at the viewer. A
 * *missing* signal is not a contradiction — an HLS variant can publish no
 * `ColorInfo` at all — so silence leaves the server's answer standing.
 *
 * @param decoderMime `Format.sampleMimeType`, or null before the first frame.
 * @param decoderColorTransfer `Format.colorInfo.colorTransfer`, or null when
 *   the stream published none.
 * @param hdrTypes `Caps.displayHdrTypes` — `Display.HdrCapabilities` constants.
 */
internal fun renderedRange(
    delivered: String?,
    decoderMime: String?,
    decoderColorTransfer: Int?,
    hdrTypes: Set<Int>,
): String? {
    if (delivered == null) return null
    if (hdrTypes.isEmpty()) return DynamicRange.SDR

    val transfer = decoderColorTransfer?.takeIf { it != COLOR_TRANSFER_UNSET }
    val decoded = when {
        decoderMime != null && decoderMime.equals(VIDEO_DOLBY_VISION, ignoreCase = true) ->
            DynamicRange.DOLBY_VISION
        transfer == COLOR_TRANSFER_ST2084 -> DynamicRange.HDR10
        transfer == COLOR_TRANSFER_HLG -> DynamicRange.HLG
        transfer != null -> DynamicRange.SDR
        else -> null
    }

    return when (decoded ?: delivered) {
        DynamicRange.DOLBY_VISION ->
            if (HdrType.DOLBY_VISION in hdrTypes) DynamicRange.DOLBY_VISION else DynamicRange.SDR
        DynamicRange.HDR10 -> if (hdrTypes.any { it in PQ_DISPLAY_TYPES }) {
            DynamicRange.HDR10
        } else {
            DynamicRange.SDR
        }
        DynamicRange.HLG -> if (hdrTypes.any { it in HLG_DISPLAY_TYPES }) {
            DynamicRange.HLG
        } else {
            DynamicRange.SDR
        }
        else -> DynamicRange.SDR
    }
}

/**
 * Media3's `MimeTypes.VIDEO_DOLBY_VISION` and `C.COLOR_TRANSFER_*`, restated
 * for the same reason `CapsPolicy` restates the framework's: this file stays
 * free of ExoPlayer so the tables above are readable and JVM-testable, and
 * these are compile-time constants either way. (`C`'s are also `@UnstableApi`,
 * which an import would drag into every caller.)
 */
private const val VIDEO_DOLBY_VISION = "video/dolby-vision"
private const val COLOR_TRANSFER_HLG = 7
private const val COLOR_TRANSFER_ST2084 = 6

/** `Format.NO_VALUE` — a `ColorInfo` that carries no transfer at all. */
private const val COLOR_TRANSFER_UNSET = -1

/** A Dolby Vision panel is a PQ panel; HDR10+ is HDR10 with dynamic metadata. */
private val PQ_DISPLAY_TYPES = setOf(HdrType.HDR10, HdrType.HDR10_PLUS, HdrType.DOLBY_VISION)
private val HLG_DISPLAY_TYPES = setOf(HdrType.HLG, HdrType.DOLBY_VISION)

private fun rungLabel(label: String, totalKbps: Int): String = when {
    totalKbps <= 0 -> label
    totalKbps >= 1_000 -> "%s · %.1f Mbps".format(Locale.US, label, totalKbps / 1_000.0)
    else -> "$label · $totalKbps kbps"
}
