package tv.plurx.app.data

import kotlinx.serialization.Serializable

/**
 * The server's typed refusal body, read instead of discarded.
 *
 * Playlist, segment and session-create routes answer a refusal with
 * `{code, message}` (and `film_position_ms` where the answer names a place to
 * resume); several older routes still answer `{error}`, which carries a
 * readable sentence too. Android used to drop all of it and show
 * "Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS).", which is why a 503
 * "still building" read as fatal
 * (docs/clients/PLAYBACK-SURFACE-CONTRACT.md §2.3, §3.5).
 */
@Serializable
internal data class RefusalBody(
    val code: String? = null,
    val message: String? = null,
    val error: String? = null,
    val film_position_ms: Long? = null,
)

/**
 * A non-2xx the server explained.
 *
 * Thrown only when the body really is a legible refusal; a bodiless or
 * unparseable answer keeps `retrofit2.HttpException`, so every existing
 * status matcher — the create coordinator's 400, the saved-session 401/403 —
 * keeps working on the exception it already matches.
 */
internal class RefusalException(
    val status: Int,
    val code: String?,
    override val message: String,
    val positionMs: Long?,
) : Exception(message)

/** Bodies larger than this are not refusals; they are something else entirely. */
private const val REFUSAL_BODY_LIMIT = 16_384

/**
 * `null` for anything that is not a legible refusal.
 *
 * Guessing would explain the wrong failure with complete confidence, which is
 * worse than the generic sentence it replaces — the same rule the web's
 * `parseStreamFailure` follows, and the same fields.
 */
internal fun parseRefusal(status: Int, body: String?): RefusalException? {
    if (status < 400) return null
    if (body == null || body.isEmpty() || body.length > REFUSAL_BODY_LIMIT) return null
    val parsed = try {
        Net.json.decodeFromString(RefusalBody.serializer(), body)
    } catch (_: Exception) {
        return null
    }
    val message = parsed.message?.trim()?.takeIf { it.isNotEmpty() }
        ?: parsed.error?.trim()?.takeIf { it.isNotEmpty() }
        ?: return null
    return RefusalException(
        status = status,
        code = parsed.code?.trim()?.takeIf { it.isNotEmpty() },
        message = message,
        positionMs = parsed.film_position_ms,
    )
}

/** The HTTP status behind a failure, whichever of the two shapes it took. */
internal fun refusalStatusOf(error: Throwable?): Int? = when (error) {
    is RefusalException -> error.status
    is retrofit2.HttpException -> error.code()
    else -> null
}
