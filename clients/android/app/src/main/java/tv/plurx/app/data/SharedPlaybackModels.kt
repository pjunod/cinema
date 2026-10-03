package tv.plurx.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import tv.plurx.app.player.PGSOverlayCue
import tv.plurx.app.player.PGSOverlayPolicy

@Serializable
internal data class SharedPlaybackFileReference(
    val item: SharedPlaybackReference, val file_id: String, val revision: String,
) {
    fun validate(context: PlaybackFileContext) {
        item.validate()
        context.validateSharedReference(item, file_id, revision)
    }
}

/** Retains every server wire field; never decoded through the numeric Local Decision. */
internal class SharedDecision private constructor(
    val fileId: String, val reference: SharedPlaybackFileReference,
    val method: String, val playUrl: String, val wire: JsonObject,
) {
    fun validated(context: PlaybackFileContext): SharedDecision {
        reference.validate(context)
        context.validateDescriptiveUrl(playUrl)
        val delivery = wire.getValue("delivery").jsonObject
        require(delivery.strictString("mode") in setOf("direct", "remux", "transcode"))
        listOf("url", "sessions_url").forEach { key ->
            delivery[key]?.takeUnless { it == JsonNull }?.let {
                val url = delivery.strictString(key)
                context.validateDescriptiveUrl(url)
                if (key == "sessions_url") require(url == "${context.fileBase}/hls/sessions")
            }
        }
        return this
    }
    companion object {
        fun decode(text: String): SharedDecision {
            require(text.toByteArray().size <= 1_048_576)
            val wire = Json.parseToJsonElement(text).jsonObject
            val file = wire.strictString("file_id")
            require(PlaybackFileContext.canonicalId(file))
            val reference = decodeSharedReference(wire.getValue("reference"))
            require(reference.file_id == file)
            val method = wire.strictString("method")
            require(method in setOf("direct_play", "remux", "transcode"))
            wire.getValue("delivery").jsonObject
            return SharedDecision(file, reference, method, wire.strictString("play_url"), wire)
        }
    }
}

private fun decodeSharedReference(value: JsonElement): SharedPlaybackFileReference {
    val binding = value.jsonObject
    binding.strictString("file_id"); binding.strictString("revision")
    val item = binding.getValue("item").jsonObject
    listOf("import_id", "server_id", "catalogue_epoch", "library_id", "item_id").forEach(item::strictString)
    return Json.decodeFromJsonElement(value)
}

private fun JsonObject.strictString(key: String): String = getValue(key).jsonPrimitive.let {
    require(it.isString); it.content
}

internal sealed interface PlaybackSubject {
    data class Local(val itemId: String, val context: PlaybackFileContext) : PlaybackSubject
    data class Shared(val context: PlaybackFileContext) : PlaybackSubject
    fun validated(): PlaybackSubject {
        when (this) {
            is Local -> { require(PlaybackFileContext.canonicalId(itemId)); context.localId() }
            is Shared -> context.validateSharedReference(requireNotNull(context.reference), context.sourceFileId, requireNotNull(context.revision))
        }
        return this
    }
}

/** Preserves the full additive start wire beside the existing session-only projection. */
internal class SharedStart private constructor(val response: HlsStart, val wire: JsonObject) {
    fun validated(context: PlaybackFileContext): SharedStart {
        SharedStartValidation.validated(response, context)
        return this
    }
    companion object {
        fun decode(text: String): SharedStart {
            require(text.toByteArray().size <= 1_048_576)
            val wire = Json.parseToJsonElement(text).jsonObject
            wire.strictString("session_id"); wire.strictString("playlist_url")
            return SharedStart(Json { ignoreUnknownKeys = true }.decodeFromJsonElement(wire), wire)
        }
    }
}

internal object SharedStartValidation {
    fun validated(start: HlsStart, context: PlaybackFileContext): HlsStart {
        context.validateSessionPlaylist(start.playlist_url, start.session_id)
        val control = requireNotNull(start.control)
        require(control.isValid && control.url == "/api/v1/hls/${start.session_id}/control")
        return start
    }
}

@Serializable
internal data class SharedPGSManifest(
    val schema: Int,
    val generation: String,
    @kotlinx.serialization.SerialName("file_id") val fileId: String,
    val reference: SharedPlaybackFileReference,
    @kotlinx.serialization.SerialName("track_index") val trackIndex: Long,
    val kind: String,
    val timebase: String,
    @kotlinx.serialization.SerialName("duration_ms") val durationMs: Long,
    val cues: List<PGSOverlayCue>,
) {
    fun validated(context: PlaybackFileContext, expectedTrackIndex: Long): SharedPGSManifest {
        reference.validate(context)
        require(
            schema == 1 &&
                Regex("[0-9a-f]{64}").matches(generation) &&
                fileId == reference.file_id &&
                trackIndex == expectedTrackIndex &&
                kind == "pgs" &&
                timebase == "source_ms" &&
                durationMs > 0 &&
                cues.size <= PGSOverlayPolicy.maximumManifestCues,
        ) { "invalid PGS overlay manifest header" }

        var previousEnd = 0L
        val dimensions = mutableMapOf<String, Pair<Int, Int>>()
        cues.forEach { cue ->
            require(
                cue.id.isNotBlank() &&
                    cue.startMs >= previousEnd &&
                    cue.endMs > cue.startMs &&
                    cue.endMs <= durationMs &&
                    cue.canvasWidth in 1..PGSOverlayPolicy.maximumCanvasWidth &&
                    cue.canvasHeight in 1..PGSOverlayPolicy.maximumCanvasHeight &&
                    cue.objects.size <= PGSOverlayPolicy.maximumObjectsPerCue,
            ) { "invalid PGS overlay cue" }
            previousEnd = cue.endMs

            cue.objects.forEach { object_ ->
                require(
                    object_.x >= 0 &&
                        object_.y >= 0 &&
                        object_.width > 0 &&
                        object_.height > 0 &&
                        object_.x <= cue.canvasWidth &&
                        object_.y <= cue.canvasHeight &&
                        object_.width <= cue.canvasWidth - object_.x &&
                        object_.height <= cue.canvasHeight - object_.y &&
                        objectHash(object_.image) != null,
                ) { "invalid PGS overlay object" }
                val existing = dimensions[object_.image]
                require(existing == null || existing == (object_.width to object_.height)) {
                    "PGS overlay object dimensions changed"
                }
                if (existing == null) {
                    dimensions[object_.image] = object_.width to object_.height
                }
            }
        }
        return this
    }

    companion object {
        fun decode(text: String): SharedPGSManifest {
            require(text.toByteArray().size <= 1_048_576)
            val wire = Json.parseToJsonElement(text).jsonObject
            wire.strictString("file_id")
            decodeSharedReference(wire.getValue("reference"))
            return Json.decodeFromJsonElement(wire)
        }
    }

    fun objectHash(path: String): String? {
        val prefix = "overlay/$generation/objects/"
        if (!path.startsWith(prefix) || !path.endsWith(".png")) return null
        return path.removePrefix(prefix).removeSuffix(".png")
            .takeIf { Regex("[0-9a-f]{64}").matches(it) }
    }
}

