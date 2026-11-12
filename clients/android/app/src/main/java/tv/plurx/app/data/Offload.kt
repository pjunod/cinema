package tv.plurx.app.data

import retrofit2.Response

/**
 * Offload: Curator removed a title's files to free disk. The answer to Get it
 * back, or the server's own sentence when it refused (Curator unreachable, too
 * old, not paired, not offloaded) — the page shows either inline.
 */
internal fun offloadAnswerOrThrow(response: Response<OffloadAnswer>): OffloadAnswer {
    val body = response.body()
    if (response.isSuccessful && body != null) return body
    val text = runCatching { response.errorBody()?.string() }.getOrNull()
    throw parseRefusal(response.code(), text)
        ?: IllegalStateException("Curator could not be asked (HTTP ${response.code()})")
}

internal fun offloadStatusLine(answer: OffloadAnswer): String = "Offloaded \u00b7 ${answer.message}"

internal const val OFFLOAD_UNREACHABLE = "Offloaded \u00b7 curator unreachable"
