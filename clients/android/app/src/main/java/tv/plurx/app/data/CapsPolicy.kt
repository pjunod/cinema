package tv.plurx.app.data

import kotlinx.serialization.Serializable

/**
 * The pure half of [Caps] — what the device's probes *mean*, with no Android
 * framework in sight so the whole table is one screen of code and one JVM
 * unit test.
 *
 * Two rules run through it, both from the server's side of the wire
 * (CLIENTS-REMEDIATION-PLAN §5.1/§5.2):
 *  - Claim only the intersection of what is true. A capability the server
 *    believes and the hardware can't honor is worse than silence: the server
 *    hands over a stream nothing can play.
 *  - Dolby Vision profile 7 is claimed only when the decoder enumeration
 *    names `DvheDtb`. M5a can convert P7 to P8.1 for every other decoder, but
 *    a decoder that genuinely takes the dual layer should keep it; guessing
 *    from a display bit or manufacturer string still risks a black screen.
 */

@Serializable
data class DeviceCaps(
    // Required on the wire: kotlinx.serialization otherwise omits a property
    // whose value equals its default, which would make this look like v1.
    val v: Int,
    val client: ClientInfo,
    val video: List<VideoEntry>,
    val audio: List<String>,
    val containers: List<String>,
    val transports: List<String>,
    val display: DisplayCaps,
    val learned_limits: List<LearnedLimit> = emptyList(),
)

@Serializable
data class ClientInfo(
    val kind: String,
    val build: String,
    val ua: String,
)

@Serializable
data class VideoEntry(
    val codec: String,
    val profiles: List<String> = emptyList(),
    val max_height: Int? = null,
    val present: List<String>,
    val dv_profiles: List<Int>? = null,
)

@Serializable
data class DisplayCaps(
    val hdr: Boolean,
    val dolby_vision: Boolean,
)

/** Native learned limits are M6 work; native documents send none today. */
@Serializable
data class LearnedLimit(
    val identity: String,
    val label: String,
    val lost: Long,
    val secs: Long,
    val rate: Long,
    val at_ms: Long,
)

/**
 * `MediaCodecInfo.CodecProfileLevel` Dolby Vision constants, restated here so
 * this file stays framework-free (they are compile-time constants either way,
 * and several were added across API levels).
 */
internal object DolbyVisionCodecProfile {
    const val DVAV_PER = 0x1
    const val DVAV_PEN = 0x2
    const val DVHE_DER = 0x4
    const val DVHE_DEN = 0x8
    const val DVHE_DTR = 0x10
    const val DVHE_STN = 0x20
    const val DVHE_DTH = 0x40
    const val DVHE_DTB = 0x80
    const val DVHE_ST = 0x100
    const val DVAV_SE = 0x200
    const val DVAV_110 = 0x400
}

/**
 * `android.media.AudioFormat` encodings, restated for the same reason. These
 * are what an HDMI sink reports it will take as a bitstream — a passthrough
 * receiver decodes formats this box has no decoder for.
 */
internal object AudioSinkEncoding {
    const val AC3 = 5
    const val E_AC3 = 6
    const val DTS = 7
    const val DTS_HD = 8
    const val DOLBY_TRUEHD = 14
    const val E_AC3_JOC = 18
}

/** One codec decoder's highest movie-sized frame proven by MediaCodec. */
internal data class VideoDecoderLimit(
    val codec: String,
    val maxHeight: Int,
    /** False for platform/software components such as `c2.android.av1-dav1d.decoder`. */
    val hardwareAccelerated: Boolean = true,
)

/**
 * A registered software decoder is useful capability evidence, but its
 * advertised size range is not a realtime performance guarantee. Android's
 * dav1d component can report 3840x2160 on a mid-grade CPU while dropping a
 * material share of the frames. Keep the software fallback for ordinary HD;
 * require hardware evidence above it.
 */
internal const val SOFTWARE_VIDEO_MAX_HEIGHT = 1080

internal data class VideoCodecCaps(
    val codecs: List<String>,
    val maxHeights: Map<String, Int>,
) {
    fun queryParams(): Map<String, String> = mapOf(
        "vcodec" to codecs.joinToString(","),
        "vmaxheight" to codecs.joinToString(",") { "$it:${maxHeights.getValue(it)}" },
    )

    fun videoEntries(present: List<String>, dvProfiles: List<Int>): List<VideoEntry> =
        codecs.map { codec ->
            VideoEntry(
                codec = codec,
                max_height = maxHeights.getValue(codec),
                present = present,
                // Dolby Vision is an HEVC profile claim. A display bit alone
                // never manufactures a decoder profile.
                dv_profiles = dvProfiles.takeIf { codec == "hevc" && it.isNotEmpty() },
            )
        }
}

/**
 * `Display.HdrCapabilities.HDR_TYPE_*`, restated so policy stays free of the
 * Android framework (these are API-24 stable constants).
 */
internal object HdrType {
    const val DOLBY_VISION = 1
    const val HDR10 = 2
    const val HLG = 3
    const val HDR10_PLUS = 4
}

internal fun presentationTransfers(hdrTypes: Set<Int>): List<String> = buildList {
    add("sdr")
    if (HdrType.HDR10 in hdrTypes || HdrType.HDR10_PLUS in hdrTypes) add("pq")
    if (HdrType.HLG in hdrTypes) add("hlg")
}

internal fun displayIsHdr(hdrTypes: Set<Int>): Boolean = hdrTypes.isNotEmpty()

internal fun capsDocument(
    video: VideoCodecCaps,
    audio: List<String>,
    hdrTypes: Set<Int>,
    decoderDolbyVisionProfiles: List<Int>,
    client: ClientInfo,
): DeviceCaps {
    val dvProfiles = decoderDolbyVisionProfiles.takeIf {
        HdrType.DOLBY_VISION in hdrTypes
    }.orEmpty()
    return DeviceCaps(
        v = 2,
        client = client,
        video = video.videoEntries(presentationTransfers(hdrTypes), dvProfiles),
        audio = audio,
        containers = DIRECT_PLAY_CONTAINERS.split(','),
        transports = listOf("progressive", "hls"),
        display = DisplayCaps(
            hdr = displayIsHdr(hdrTypes),
            dolby_vision = HdrType.DOLBY_VISION in hdrTypes,
        ),
    )
}

internal fun shouldFallBackToLegacyDecision(statusCode: Int): Boolean =
    statusCode == 400 || statusCode == 404 || statusCode == 405

/**
 * Normalize MediaCodec evidence into the two server capability fields.
 * Duplicate decoders are intentional — the best decoder for a codec wins —
 * while unknown codecs and non-positive limits are not capability evidence.
 */
internal fun videoCodecCaps(limits: Iterable<VideoDecoderLimit>): VideoCodecCaps {
    val supported = setOf("h264", "hevc", "av1", "vp9")
    val maxima = mutableMapOf<String, Int>()
    for (limit in limits) {
        val codec = limit.codec.lowercase()
        if (codec !in supported || limit.maxHeight <= 0) continue
        val effectiveHeight = if (limit.hardwareAccelerated) {
            limit.maxHeight
        } else {
            minOf(limit.maxHeight, SOFTWARE_VIDEO_MAX_HEIGHT)
        }
        maxima[codec] = maxOf(maxima[codec] ?: 0, effectiveHeight)
    }
    val ordered = listOf("h264", "hevc", "av1", "vp9").filter(maxima::containsKey)
    return VideoCodecCaps(ordered, ordered.associateWith(maxima::getValue))
}

/**
 * Dolby Vision profile numbers this client is willing to claim, from the raw
 * profile constants a `video/dolby-vision` decoder advertises.
 *
 * The HEVC profiles map through only when the decoder names them: DvheDtr → 4,
 * DvheStn → 5, DvheDtb → 7, DvheSt → 8. P7 is dual-layer and therefore the
 * most dangerous one to guess, but a real enumeration should receive the
 * untouched stream instead of an M5a conversion that discards its enhancement
 * layer. The AVC- and AV1-based profiles (9, 10) stay out until the library
 * contains such a file.
 */
internal fun dolbyVisionProfiles(decoderProfiles: Iterable<Int>): List<Int> {
    val claimed = sortedSetOf<Int>()
    for (profile in decoderProfiles) {
        when (profile) {
            DolbyVisionCodecProfile.DVHE_DTR -> claimed.add(4)
            DolbyVisionCodecProfile.DVHE_STN -> claimed.add(5)
            DolbyVisionCodecProfile.DVHE_DTB -> claimed.add(7)
            DolbyVisionCodecProfile.DVHE_ST -> claimed.add(8)
            else -> Unit
        }
    }
    return claimed.toList()
}

/**
 * The audio codecs to report, merging what this box can *decode* with what the
 * active route will take as a *bitstream*.
 *
 * A Shield feeding an AVR has no TrueHD decoder and never will — the receiver
 * does that job — so decoder-only probing is why lossless Atmos came back as
 * `transcode_audio` → AAC 256k. Sink support is route-dependent and must be
 * recomputed per decision; see [Caps.query].
 */
internal fun audioCodecClaims(
    decoders: Set<String>,
    sinkEncodings: Set<Int>,
): List<String> {
    val claimed = linkedSetOf("aac", "mp3", "opus", "flac")
    fun claim(codec: String, decoderMime: Boolean, vararg encodings: Int) {
        if (decoderMime || encodings.any { it in sinkEncodings }) claimed.add(codec)
    }
    claim("ac3", "audio/ac3" in decoders, AudioSinkEncoding.AC3)
    claim("eac3", "audio/eac3" in decoders, AudioSinkEncoding.E_AC3, AudioSinkEncoding.E_AC3_JOC)
    claim(
        "dts",
        "audio/vnd.dts" in decoders || "audio/vnd.dts.hd" in decoders,
        AudioSinkEncoding.DTS,
        AudioSinkEncoding.DTS_HD,
    )
    claim("truehd", "audio/true-hd" in decoders, AudioSinkEncoding.DOLBY_TRUEHD)
    return claimed.toList()
}

/**
 * The `dv` / `dvprofile` pair for the decision query, or an empty map.
 *
 * Both facts must hold: a decoder that produces Dolby Vision and a display
 * that shows it. Claiming DV on an HDR10-only panel buys a stream the TV
 * tone-maps blind. `dvprofile` is authoritative over `dv` server-side
 * (`stream.rs:193`), so they always travel together.
 */
internal fun dolbyVisionCaps(profiles: List<Int>, displaySupportsDolbyVision: Boolean): Map<String, String> =
    if (profiles.isEmpty() || !displaySupportsDolbyVision) {
        emptyMap()
    } else {
        mapOf("dv" to "1", "dvprofile" to profiles.joinToString(","))
    }

/**
 * Compact capability evidence carried with `/decision` and surfaced by the
 * server log. A remote playback failure is otherwise indistinguishable from
 * an old APK, a missing decoder, or a display route with Dolby Vision off.
 * These fields are diagnostic only and never affect the delivery decision.
 */
internal fun capabilityDiagnostics(
    version: String,
    hdrTypes: Set<Int>,
    decoderNames: List<String>,
    rawProfiles: List<Int>,
    claimedProfiles: List<Int>,
): Map<String, String> {
    val status = when {
        HdrType.DOLBY_VISION !in hdrTypes -> "display-no-dv"
        decoderNames.isEmpty() -> "decoder-missing"
        rawProfiles.isEmpty() -> "decoder-no-profiles"
        claimedProfiles.isEmpty() -> "unsupported-profiles"
        else -> "ready"
    }
    return mapOf(
        "capver" to version,
        "hdrtypes" to hdrTypes.sorted().joinToString(","),
        "dvdecoders" to decoderNames.joinToString(",").take(512),
        "dvraw" to rawProfiles.sorted().joinToString(","),
        "dvstatus" to status,
    )
}

internal const val DIRECT_PLAY_CONTAINERS =
    "mkv,mp4,webm,mov,ts,m4a,m4b,mp3,aac,flac,ogg,opus,wav"
