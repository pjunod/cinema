package tv.plurx.app.player

import kotlinx.serialization.json.JsonObject

/** An exact schedule route on the creating profile's bounded transport. */
internal class ContinuousQualityExchange private constructor(
    private val profile: ContinuousProfileHttp,
    private val path: String,
    private val ownsProfile: Boolean,
) : AutoCloseable {
    constructor(origin: String, schedulePath: String, token: String) : this(ContinuousProfileHttp(origin, token), schedulePath, true)
    constructor(profile: ContinuousProfileHttp, schedulePath: String) : this(profile, schedulePath, false)
    init {
        val session = path.removePrefix("/api/v1/hls/").removeSuffix("/quality-schedule")
        require(ContinuousQualityWire.uuid(session) && path == "/api/v1/hls/$session/quality-schedule")
    }
    suspend fun exchange(body: JsonObject): JsonObject = profile.request(path, body)
    override fun close() { if (ownsProfile) profile.close() }
}
