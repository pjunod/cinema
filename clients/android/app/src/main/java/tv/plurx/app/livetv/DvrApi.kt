package tv.plurx.app.livetv

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Net
import tv.plurx.app.data.Session
import java.util.concurrent.TimeUnit

/**
 * Recording, the schedule, rules and reminders.
 *
 * Wire names are snake_case verbatim, exactly as in [LiveTvApi]; `Net.json`
 * ignores unknown keys, so a server that grows a field does not break an older
 * build. `state` stays a plain string rather than an enum for the same reason:
 * the server owns that vocabulary, and a row in a state this build has never
 * heard of must still decode and still draw.
 */
@Serializable
data class DvrRecording(
    val id: String,
    val origin: String = "manual",
    /** Set when a rule materialised this airing. The two-dot cell mark. */
    val rule_id: String? = null,
    val channel_id: String,
    val guide_number: String = "",
    val channel_name: String = "",
    val airing_start: Long,
    val airing_end: Long,
    val capture_start: Long,
    val capture_end: Long,
    val title: String,
    val episode_title: String? = null,
    val episode: String? = null,
    val synopsis: String? = null,
    val image_url: String? = null,
    val series_id: String? = null,
    val state: String,
    /** One sentence for anything that is not plainly scheduled. */
    val state_reason: String? = null,
    val attempt: Long = 0,
    val gap_s: Long = 0,
    val late_start_s: Long = 0,
    val bytes: Long = 0,
    val last_progress_ms: Long? = null,
    val stop_requested_at_ms: Long? = null,
    val stopped_by_user_id: Long? = null,
    val started_at_ms: Long? = null,
    /** Both arrive once the scan has linked the capture into the library. */
    val item_id: Long? = null,
    val file_id: Long? = null,
    val finished_at_ms: Long? = null,
) {
    val pending: Boolean get() = state in DVR_PENDING_STATES
    val recording: Boolean get() = state == "recording"
    val conflict: Boolean get() = state == "conflict"
    val cancelled: Boolean get() = state == "cancelled"

    /** A capture that produced a file the library can hold. */
    val hasMedia: Boolean get() = state == "done" || state == "partial"

    /** How far through its capture window a running recording is. */
    fun captureProgress(now: Long): Float {
        val span = capture_end - capture_start
        if (span <= 0) return 0f
        return ((now - capture_start).toFloat() / span).coerceIn(0f, 1f)
    }
}

private val DVR_PENDING_STATES = setOf("scheduled", "conflict", "withdrawn", "stale")

@Serializable
data class DvrRule(
    val id: String,
    /** Lower wins, unique across the server, renumbered on reorder. */
    val priority: Long = 0,
    val name: String,
    val match_mode: String = "title",
    val match_value: String = "",
    /** Absent means any channel. Always set for a title rule made from a cell. */
    val channel_id: String? = null,
    val new_only: Boolean = true,
    val keep_mode: String = "all",
    val keep_value: Long = 0,
    val pad_start_s: Long = 0,
    val pad_end_s: Long = 0,
    val enabled: Boolean = true,
) {
    /**
     * What the rule actually matches on, said plainly. A title rule is locked
     * to one channel and an id rule is not, and a list that implied the same
     * exactness for both would be telling the viewer something untrue.
     */
    val matchSummary: String get() = if (match_mode == "series_id") {
        "Series id · any channel"
    } else {
        "Title match" + (channel_id?.let { " · $it only" } ?: " · any channel")
    }
}

/**
 * One reminder. `GET /dvr/reminders` flattens the row and adds
 * `covered_by_recording` beside it, so one model decodes both the list and the
 * single row `POST` answers.
 */
@Serializable
data class DvrReminder(
    val id: String,
    val channel_id: String,
    val guide_number: String = "",
    val airing_start: Long,
    val airing_end: Long,
    val title: String,
    val lead_s: Long = 0,
    val state: String = "armed",
    val covered_by_recording: Boolean = false,
) {
    /** When the platform alarm for this reminder is due. */
    val fireAt: Long get() = airing_start - lead_s
}

@Serializable
data class DvrSlots(val max: Int = 0, val reserve: Int = 0, val recording: Int = 0) {
    /** What a viewer can still tune, given what recordings are allowed to hold. */
    val freeForViewers: Int get() = (max - recording).coerceAtLeast(0)
}

@Serializable
data class DvrStatus(
    val enabled: Boolean = false,
    val owner_node_id: String = "",
    val root: String = "",
    /** Absent on a node that cannot read the root, which is not an error. */
    val free_bytes: Long? = null,
    val floor_bytes: Long = 0,
    val slots: DvrSlots = DvrSlots(),
    val next_start: Long? = null,
    val pad_start_s: Long = 0,
    val pad_end_s: Long = 0,
    val reminder_lead_s: Long = 0,
)

@Serializable
data class DvrSchedule(
    /** Rows with no tuner. The number the Scheduled chip carries. */
    val conflicts: Int = 0,
    val rows: List<DvrRecording> = emptyList(),
    val next: String? = null,
)

@Serializable
data class DvrRecordingsPage(
    val rows: List<DvrRecording> = emptyList(),
    val next: String? = null,
)

@Serializable
data class DvrCaptureObservation(
    val recording_id: String,
    val channel_id: String,
    val airing_start: Long,
    val owner_node_id: String = "",
    val config_generation: Long = 0,
    val serving_generation: Long = 0,
    val attempt: Long = 0,
    /** Open vocabulary: unknown owner phases still decode and render safely. */
    val phase: String,
    val observation_age_ms: Long = 0,
    val last_write_age_ms: Long? = null,
    val first_write_at_ms: Long? = null,
    val attempt_bytes_written: Long = 0,
    val prior_attempt_bytes: Long? = null,
    val write_bps: Long? = null,
    val reason_code: String? = null,
)

@Serializable
data class DvrOverviewCounts(
    val recording: Int? = null,
    val starting: Int? = null,
    val reconnecting: Int? = null,
    val finishing: Int? = null,
    val unconfirmed: Int? = null,
    val attention: Int? = null,
) {
    val active: Int get() = listOf(recording, starting, reconnecting, finishing, unconfirmed)
        .filterNotNull().sum()
}

@Serializable
data class DvrOverviewDiagnostics(
    val owner_node_id: String,
    val recording_sinks: Int? = null,
    val recording_transports: Int? = null,
    val storage_free_bytes: Long? = null,
)

@Serializable
data class DvrActiveRecording(
    val recording_id: String,
    val channel_id: String,
    val airing_start: Long,
    val title: String,
    val episode_title: String? = null,
    val guide_number: String = "",
    val channel_name: String = "",
    val durable_state: String,
    val state_reason: String? = null,
    val airing_end: Long,
    val capture_start: Long,
    val capture_end: Long,
    val stop_requested_at_ms: Long? = null,
    val last_confirmed_bytes: Long? = null,
    val total_bytes_written: Long? = null,
    val observation: DvrCaptureObservation? = null,
    val display_state: String,
    val display_detail: String = "",
    val can_stop: Boolean = false,
    val can_skip: Boolean = false,
    val can_restore: Boolean = false,
    val can_delete: Boolean = false,
    val can_edit_rule: Boolean = false,
    val can_reorder_rules: Boolean = false,
    val can_view_diagnostics: Boolean = false,
) {
    fun captureProgress(now: Long): Float {
        val span = capture_end - capture_start
        return if (span <= 0) 0f else ((now - capture_start).toFloat() / span).coerceIn(0f, 1f)
    }
}

@Serializable
data class DvrOverview(
    val version: Int = 1,
    val server_now_ms: Long,
    val availability: String,
    val runtime_supported: Boolean = true,
    val observation_age_ms: Long? = null,
    val counts: DvrOverviewCounts = DvrOverviewCounts(),
    val active_total: Int? = null,
    val active_truncated: Boolean = false,
    val active: List<DvrActiveRecording> = emptyList(),
    val next_capture_start: Long? = null,
    val diagnostics: DvrOverviewDiagnostics? = null,
) {
    fun isFresh(clientAgeMs: Long = 0): Boolean {
        val age = (observation_age_ms ?: 0).coerceAtLeast(0)
        return availability != "unavailable" &&
            (availability == "complete" || observation_age_ms != null) &&
            age <= 20_000 - clientAgeMs.coerceIn(0, 20_001)
    }

    fun indicatorText(clientAgeMs: Long = 0): String? {
        if (!isFresh(clientAgeMs)) return if (counts.active > 0) "Status unavailable" else null
        return listOfNotNull(
            counts.recording?.takeIf { it > 0 }?.let { "$it recording" },
            counts.starting?.takeIf { it > 0 }?.let { "$it starting" },
            counts.reconnecting?.takeIf { it > 0 }?.let { "$it reconnecting" },
            counts.finishing?.takeIf { it > 0 }?.let { "$it finishing" },
            counts.unconfirmed?.takeIf { it > 0 }?.let { "$it unconfirmed" },
        ).takeIf { it.isNotEmpty() }?.joinToString(" · ")
    }
}

@Serializable
data class DvrEvent(
    val recording_id: String,
    val sequence: Long,
    val event_id: String,
    val kind: String,
    val occurred_at_ms: Long,
    val attempt: Long? = null,
    val actor_user_id: Long? = null,
    val reason_code: String? = null,
)

@Serializable
data class DvrEventsPage(
    val rows: List<DvrEvent> = emptyList(),
    val next: String? = null,
    val history_complete: Boolean = false,
    val truncated_before_sequence: Long? = null,
)

@Serializable
data class DvrAttentionProjection(
    val recording: DvrRecording,
    val latest_attention_sequence: Long,
    val latest_attention_at_ms: Long,
    val acknowledged_through_sequence: Long = 0,
)

@Serializable
data class DvrAttentionPage(
    val rows: List<DvrAttentionProjection> = emptyList(),
    val next: String? = null,
    val total: Int = 0,
)

@Serializable
data class DvrAttentionAck(val recording_id: String, val through_sequence: Long)

@Serializable
data class DvrHolderSink(val recording_id: String, val title: String, val ends_at: Long)

/** One channel whose transport is holding a tuner, and what is writing to it. */
@Serializable
data class DvrHolder(
    val channel_id: String,
    val guide_number: String = "",
    val channel_name: String = "",
    val sinks: List<DvrHolderSink> = emptyList(),
) {
    /**
     * Stopping one of two recordings on a channel frees no tuner, so only a
     * single-sink transport is offered as a stop.
     */
    val stoppable: DvrHolderSink? get() = sinks.singleOrNull()
}

/**
 * A typed refusal, with the recovery data the server attached to it.
 *
 * `holders` rides on a `tuner_capacity` body. The server flattens its detail
 * map into the top level beside `code` and `message` (`http/error.rs`
 * `into_response`), so it is read from there rather than from a nested
 * `detail` object.
 */
class DvrFailure(
    val code: String,
    val holders: List<DvrHolder> = emptyList(),
) : Exception(dvrMessage(code))

internal fun dvrMessage(code: String): String = when (code) {
    "dvr_disabled" -> "Recording is switched off. An administrator can enable it in Settings → Developer."
    "airing_unknown" -> "The guide no longer has that programme at that time. Reload the guide and try again."
    "airing_past" -> "That programme has already started, or has less than a minute left."
    "rule_limit" -> "This server already holds the maximum number of recording rules."
    "reminder_limit" -> "You already have the maximum number of reminders set."
    "delete_file_required" -> "This recording has a file. Confirm again to delete the recording and the file."
    "tuner_capacity" -> "Every tuner is in use. Stop a recording or a live session and try again."
    "admin_required" -> "An administrator account is required to reorder recording rules."
    "invalid_settings" -> "The saved server address is unusable. Reconnect in Settings."
    else -> "The recording service could not be reached. Try again."
}

/**
 * What `DELETE /dvr/recordings/{id}` meant, decided by what the row was doing.
 *
 * Three outcomes rather than one because they are three different promises: a
 * planned airing is off the schedule when the call returns, a running capture
 * is only *asked* to stop and the owner's next tick closes the file, and a
 * finished recording is not touched at all until the caller says in the URL
 * that it means the file too.
 */
sealed interface DvrStopOutcome {
    data object Cancelled : DvrStopOutcome
    data class StopRequested(val requestedAt: Long) : DvrStopOutcome
    data object FileConfirmationRequired : DvrStopOutcome
}

/** One field set of a rule edit, so every call site names what it means to change. */
sealed interface DvrRuleChange {
    data class Enabled(val enabled: Boolean) : DvrRuleChange
    data class NewOnly(val newOnly: Boolean) : DvrRuleChange
    data class Keep(val mode: String, val value: Long) : DvrRuleChange
    data class Padding(val startSeconds: Long, val endSeconds: Long) : DvrRuleChange
    /** An explicit `any_channel` is different from an absent `channel_id`. */
    data class Channel(val channelId: String?) : DvrRuleChange

    fun body(): JsonObject = buildJsonObject {
        when (val change = this@DvrRuleChange) {
            is Enabled -> put("enabled", change.enabled)
            is NewOnly -> put("new_only", change.newOnly)
            is Keep -> {
                put("keep_mode", change.mode)
                put("keep_value", change.value)
            }
            is Padding -> {
                put("pad_start_s", change.startSeconds)
                put("pad_end_s", change.endSeconds)
            }
            // An `if`, not an elvis on `put`: `put` answers with the *previous*
            // value, which is null for a key being written for the first time,
            // so `put(…) ?: put(…)` would always write both.
            is Channel -> if (change.channelId == null) {
                put("any_channel", true)
            } else {
                put("channel_id", change.channelId)
            }
        }
    }
}

/**
 * Immutable profile-bound DVR client, built exactly like [LiveTvApi].
 *
 * The base client carries no interceptor and the bearer is attached here
 * rather than by `Net.client`. Not because these routes are narrow — every one
 * of them is an account route — but because `Net.client` reads the *global*
 * `Session.token` on each request, and a schedule read that outlived a profile
 * switch would then carry the next profile's bearer.
 */
class DvrApi(origin: String, private val token: String) {
    private companion object {
        /**
         * A fortnight of schedule rows and a page of recordings are both far
         * smaller than this; the cap exists so a wrong origin answering with a
         * web page is a failure rather than a read of unbounded length.
         */
        const val MAX_BODY_BYTES: Long = 1_048_576
    }

    private val base = (Session.canonicalOrigin(origin) ?: throw DvrFailure("invalid_settings")).toHttpUrl()
    private val client: OkHttpClient = Net.capabilityClient.newBuilder()
        .connectTimeout(8, TimeUnit.SECONDS).readTimeout(20, TimeUnit.SECONDS)
        .callTimeout(20, TimeUnit.SECONDS).build()

    private fun url(vararg parts: String): HttpUrl = base.newBuilder()
        .addPathSegments("api/v1/dvr").apply { parts.forEach(::addPathSegment) }.build()

    /** An answer's status alongside its body: three DELETE codes mean three things. */
    private data class Answer(val status: Int, val body: String)

    private suspend fun request(
        target: HttpUrl,
        method: String = "GET",
        body: JsonObject? = null,
        timeoutSeconds: Long? = null,
    ): Answer = withContext(Dispatchers.IO) {
        try {
            val payload = if (method == "POST" || method == "PUT") {
                (body?.let { Net.json.encodeToString(it) } ?: "{}")
                    .toRequestBody("application/json".toMediaType())
            } else {
                null
            }
            val request = Request.Builder().url(target).method(method, payload)
                .header("Authorization", "Bearer $token").build()
            val call = client.newCall(request)
            timeoutSeconds?.let { call.timeout().timeout(it, TimeUnit.SECONDS) }
            call.execute().use { response ->
                val source = response.body?.source()
                if (source?.request(MAX_BODY_BYTES + 1) == true) throw DvrFailure("dvr_unreachable")
                val text = source?.readUtf8().orEmpty()
                if (!response.isSuccessful) throw refusal(response.code, text)
                Answer(response.code, text)
            }
        } catch (error: DvrFailure) {
            throw error
        } catch (cancelled: CancellationException) {
            // Cancellation is this coroutine being told to stop, not the server
            // refusing. Turning it into a typed failure would report a screen
            // being left as a recording that could not be scheduled.
            throw cancelled
        } catch (_: Exception) {
            throw DvrFailure("dvr_unreachable")
        }
    }

    private fun refusal(status: Int, text: String): DvrFailure {
        val parsed = runCatching { Net.json.decodeFromString<JsonObject>(text) }.getOrNull()
        val code = parsed?.get("code")?.jsonPrimitive?.contentOrNull ?: when (status) {
            401, 403 -> "admin_required"
            else -> "dvr_unreachable"
        }
        val holders = parsed?.get("holders")
            ?.let { runCatching { Net.json.decodeFromJsonElement<List<DvrHolder>>(it) }.getOrNull() }
            .orEmpty()
        return DvrFailure(code, holders)
    }

    suspend fun status(): DvrStatus = Net.json.decodeFromString(request(url("status")).body)

    suspend fun overview(): DvrOverview = Net.json.decodeFromString(
        request(url("overview"), timeoutSeconds = 4).body,
    )

    suspend fun schedule(from: Long, to: Long, after: String? = null): DvrSchedule =
        Net.json.decodeFromString(
            request(
                url("schedule").newBuilder()
                    .addQueryParameter("from", from.toString())
                    .addQueryParameter("to", to.toString())
                    .addQueryParameter("limit", "100")
                    .apply { after?.let { addQueryParameter("after", it) } }
                    .build(),
            ).body,
        )

    /**
     * Rows in the named states. An empty list asks for every state except
     * `deleted`, which is the server's own default rather than a filter this
     * client invents.
     */
    suspend fun recordingsPage(
        states: List<String> = emptyList(),
        after: String? = null,
    ): DvrRecordingsPage =
        Net.json.decodeFromString(
            request(
                url("recordings").newBuilder()
                    .apply {
                        if (states.isNotEmpty()) {
                            addQueryParameter("state", states.joinToString(","))
                        }
                        if (after != null) addQueryParameter("after", after)
                    }
                    .build(),
            ).body,
        )

    suspend fun recordings(states: List<String> = emptyList()): List<DvrRecording> =
        recordingsPage(states).rows

    suspend fun recording(id: String): DvrRecording =
        Net.json.decodeFromString(request(url("recordings", id)).body)

    suspend fun events(
        id: String,
        before: String? = null,
        after: String? = null,
        limit: Int = 50,
    ): DvrEventsPage = Net.json.decodeFromString(
        request(
            url("recordings", id, "events").newBuilder().apply {
                addQueryParameter("limit", limit.coerceIn(1, 100).toString())
                before?.let { addQueryParameter("before", it) }
                after?.let { addQueryParameter("after", it) }
            }.build(),
        ).body,
    )

    suspend fun attention(after: String? = null): DvrAttentionPage = Net.json.decodeFromString(
        request(
            url("attention").newBuilder().apply { after?.let { addQueryParameter("after", it) } }.build(),
        ).body,
    )

    suspend fun acknowledgeAttention(id: String, throughSequence: Long): DvrAttentionAck =
        Net.json.decodeFromString(
            request(
                url("recordings", id, "attention", "ack"),
                "POST",
                buildJsonObject { put("through_sequence", throughSequence) },
            ).body,
        )

    /**
     * Record one airing from the guide. Two viewers pressing Record on the same
     * cell both get the same row — one airing is one recording — so an existing
     * row comes back `200` and is the answer, not a conflict.
     */
    suspend fun record(channelId: String, airingStart: Long): DvrRecording = Net.json.decodeFromString(
        request(
            url("recordings"),
            "POST",
            buildJsonObject {
                put("channel_id", channelId)
                put("airing_start", airingStart)
            },
        ).body,
    )

    suspend fun recordManual(
        channelId: String,
        captureStart: Long,
        captureEnd: Long,
        title: String,
    ): DvrRecording = Net.json.decodeFromString(
        request(
            url("recordings"),
            "POST",
            buildJsonObject {
                put("channel_id", channelId)
                put("capture_start", captureStart)
                put("capture_end", captureEnd)
                if (title.isNotBlank()) put("title", title.trim())
            },
        ).body,
    )

    suspend fun stop(id: String, deleteFile: Boolean = false): DvrStopOutcome {
        val target = url("recordings", id).newBuilder()
            .apply { if (deleteFile) addQueryParameter("delete_file", "1") }
            .build()
        val answer = try {
            request(target, "DELETE")
        } catch (error: DvrFailure) {
            if (error.code == "delete_file_required") {
                return DvrStopOutcome.FileConfirmationRequired
            }
            throw error
        }
        if (answer.status != 202) return DvrStopOutcome.Cancelled
        val requestedAt = runCatching {
            Net.json.decodeFromString<JsonObject>(answer.body)["requested_at"]
                ?.jsonPrimitive?.longOrNull
        }.getOrNull()
        return DvrStopOutcome.StopRequested(requestedAt ?: 0)
    }

    suspend fun restore(id: String): DvrRecording =
        Net.json.decodeFromString(request(url("recordings", id, "restore"), "POST").body)

    suspend fun rules(): List<DvrRule> = Net.json.decodeFromString(request(url("rules")).body)

    /**
     * Record series in two presses rather than a form: the server reads the
     * cell, decides whether the guide gave it a series id or only a title, and
     * fills the rule accordingly.
     */
    suspend fun createRule(channelId: String, airingStart: Long): DvrRule = Net.json.decodeFromString(
        request(
            url("rules"),
            "POST",
            buildJsonObject {
                putJsonObject("from_airing") {
                    put("channel_id", channelId)
                    put("airing_start", airingStart)
                }
            },
        ).body,
    )

    suspend fun updateRule(id: String, change: DvrRuleChange): DvrRule =
        Net.json.decodeFromString(request(url("rules", id), "PUT", change.body()).body)

    suspend fun deleteRule(id: String) {
        request(url("rules", id), "DELETE")
    }

    /** Admin only: the order decides which rule gets a tuner when two want one. */
    suspend fun reorderRules(ids: List<String>): List<DvrRule> = Net.json.decodeFromString(
        request(url("rules", "order"), "PUT", dvrReorderBody(ids)).body,
    )

    suspend fun reminders(due: Boolean = false): List<DvrReminder> = Net.json.decodeFromString(
        request(
            url("reminders").newBuilder()
                .apply { if (due) addQueryParameter("due", "1") }
                .build(),
        ).body,
    )

    suspend fun remind(channelId: String, airingStart: Long, leadSeconds: Long? = null): DvrReminder =
        Net.json.decodeFromString(
            request(
                url("reminders"),
                "POST",
                buildJsonObject {
                    put("channel_id", channelId)
                    put("airing_start", airingStart)
                    leadSeconds?.let { put("lead_s", it) }
                },
            ).body,
        )

    suspend fun deleteReminder(id: String) {
        request(url("reminders", id), "DELETE")
    }

    /** Acknowledged here, gone from every other device the household uses. */
    suspend fun ackReminder(id: String) {
        request(url("reminders", id, "ack"), "POST")
    }
}

/**
 * The body `PUT /dvr/rules/order` takes: the rule ids, in the order the owner
 * should try them.
 *
 * Built here rather than inline in the suspending call so it can be asserted
 * without a socket. It is also where this client's one compile error lived:
 * `add(id)` on a `JsonArrayBuilder` takes a `JsonElement`, not a `String`, and
 * nothing on this branch had ever compiled the Kotlin to say so.
 */
internal fun dvrReorderBody(ids: List<String>): JsonObject =
    buildJsonObject { putJsonArray("ids") { ids.forEach { id -> add(JsonPrimitive(id)) } } }
