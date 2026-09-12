package tv.plurx.app.librarychannels

import kotlinx.serialization.Serializable
import tv.plurx.app.data.Net

@Serializable
private data class LibraryChannelErrorBody(
    val code: String? = null,
    val message: String? = null,
    val error: String? = null,
)

internal class LibraryChannelStartFailure(
    val status: Int,
    val code: String?,
    message: String,
) : Exception(message)

internal fun libraryChannelStartFailure(status: Int, responseBody: String?): LibraryChannelStartFailure {
    val body = responseBody?.let {
        runCatching { Net.json.decodeFromString<LibraryChannelErrorBody>(it) }.getOrNull()
    }
    val code = body?.code?.takeIf(String::isNotBlank)
    val detail = body?.message?.takeIf(String::isNotBlank)
        ?: body?.error?.takeIf(String::isNotBlank)
    val suffix = if (code == null) "HTTP $status" else "$code, HTTP $status"
    val message = if (detail == null) {
        "The scheduled programme could not start ($suffix)."
    } else {
        "$detail ($suffix)"
    }
    return LibraryChannelStartFailure(status, code, message)
}

internal fun shouldRetryLibraryChannelStart(
    failure: LibraryChannelStartFailure,
    retryOccurrenceChange: Boolean,
): Boolean = retryOccurrenceChange && failure.status == 409 && failure.code == "channel_occurrence_changed"
