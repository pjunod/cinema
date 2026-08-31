package tv.plurx.app.player

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Net
import tv.plurx.app.data.Session
import java.io.IOException
import java.util.UUID

/**
 * The playback-control exchange, over HTTP.
 *
 * Separate from [PlaybackControlReporter] because the reporter's rules are
 * about ordering and this one's are about what a server's refusal means. The
 * reporter classifies a failure by `status`, `code` and `retry_after_ms`;
 * producing those faithfully from an HTTP response is this file's whole job,
 * and getting it wrong would make a retryable refusal look terminal.
 */
class PlaybackControlTransport(
    private val origin: String,
    private val client: OkHttpClient = Net.client,
    private val json: Json = Net.json,
) {
    suspend fun send(path: String, request: ControlRequest): ControlResponse {
        // The bootstrap's url is server-relative and already shape-checked, so
        // joining it to this origin cannot reach another host. Re-checking here
        // means no caller can hand this an address the reporter never approved.
        if (!ControlBootstrap.isSessionControlPath(path)) {
            throw ControlProtocolException("url")
        }
        val url = origin.trimEnd('/') + path
        val body = json.encodeToString(ControlRequest.serializer(), request)
            .toRequestBody("application/json".toMediaType())
        val call = Request.Builder().url(url).post(body).build()
        return withContext(Dispatchers.IO) {
            val response = try {
                client.newCall(call).execute()
            } catch (failure: IOException) {
                // No status at all — the reporter treats that as retryable
                // transport, which is right: the server never saw this.
                throw ControlTransportException(status = null, code = null)
            }
            response.use {
                val text = it.body?.string().orEmpty()
                if (!it.isSuccessful) throw failure(it.code, text, json)
                try {
                    json.decodeFromString(ControlResponse.serializer(), text)
                } catch (_: Exception) {
                    // A 2xx that is not a control response is not a transport
                    // problem to retry; it is something other than this
                    // server's control route answering.
                    throw ControlProtocolException("body")
                }
            }
        }
    }

    companion object {
        /**
         * A refusal carries the fields the reporter classifies on. Every one is
         * optional on the wire, and a body that is missing or unparseable still
         * yields the status, which is enough to decide retryable from terminal.
         */
        fun failure(status: Int, body: String, json: Json = Net.json): ControlTransportException {
            var code: String? = null
            var generation: String? = null
            var epoch: Long? = null
            var retryAfter: Long? = null
            try {
                val fields = json.parseToJsonElement(body).jsonObject
                code = fields["code"]?.jsonPrimitive?.contentOrNullSafe()
                generation = fields["generation"]?.jsonPrimitive?.contentOrNullSafe()
                epoch = fields["control_epoch"]?.jsonPrimitive?.longOrNull
                retryAfter = fields["retry_after_ms"]?.jsonPrimitive?.longOrNull
            } catch (_: Exception) {
                // Status only. That is still enough to classify.
            }
            return ControlTransportException(status, code, generation, epoch, retryAfter)
        }

        /** `null` for a JSON null or a non-string, rather than the text "null". */
        private fun kotlinx.serialization.json.JsonPrimitive.contentOrNullSafe(): String? =
            if (isString) content else null
    }
}

/**
 * One player's control reporting, from the bootstrap the server handed back
 * with the session to the last exchange before release.
 *
 * This is the piece [Controller] holds. It exists so the controller deals in
 * "the player changed" rather than in reporters, transports, identities and
 * deadlines.
 */
class PlaybackControlSession(private val scope: CoroutineScope) {
    private var reporter: PlaybackControlReporter? = null

    /**
     * One identity per player instance, not per session: a reopen is the same
     * viewer on the same device continuing, and the server reads a new
     * `client_instance_id` as a different client.
     */
    private val clientInstanceId = UUID.randomUUID().toString()

    val isReporting: Boolean get() = reporter != null

    private val verdictLock = Any()
    private var verdict: ControlAction? = null
    private var verdictArmedAtMs = 0L
    private var verdictLeaseMs = 0L
    private var verdictGeneration = 0

    /**
     * The last terminal verdict this session was given, if any.
     *
     * It deliberately outlives the reporter. A terminal verdict stops
     * reporting — correctly, since the reporter owns no recovery — so a
     * verdict that died with it would be discarded exactly when it mattered:
     * at the failure it explains, later.
     */
    val terminalVerdict: ControlAction?
        get() = synchronized(verdictLock) {
            val armed = verdict ?: return@synchronized null
            // A verdict outlives its reporter and its session, but not the
            // lease the server gave that session. Past it the session the
            // verdict described is gone, and a confident sentence about a
            // production attempt that ended an hour ago would caption an
            // unrelated failure. The bound is the server's own number rather
            // than one invented here.
            if (System.currentTimeMillis() - verdictArmedAtMs > verdictLeaseMs) {
                verdict = null
                return@synchronized null
            }
            armed
        }

    /**
     * A new title. The old verdict described a source that is no longer
     * playing, so keeping it would show a confident sentence about the wrong
     * film. A reopen deliberately does not clear it: the failure a verdict
     * explains normally arrives on the far side of one.
     */
    fun clearVerdict() {
        synchronized(verdictLock) { verdict = null }
    }

    /** Test seam: what the verdict's staleness bound is measured against. */
    internal fun verdictArmedAtMsForTest(): Long = synchronized(verdictLock) { verdictArmedAtMs }

    /**
     * Begin reporting for a session the server said is controllable. A
     * bootstrap this client cannot address leaves it silent, which is the
     * passive behaviour rather than a playback failure.
     */
    fun begin(
        bootstrap: ControlBootstrap,
        observe: () -> PlayerControlObservation?,
        transport: PlaybackControlTransport = PlaybackControlTransport(Session.origin),
    ) {
        end()
        // A generation, not a reset. `end()` stops the old reporter in a
        // launched coroutine, so the stop does not necessarily land before
        // this begin — and an old in-flight exchange completing in that window
        // would otherwise carry a previous generation's verdict into this one.
        val generation = synchronized(verdictLock) { ++verdictGeneration }
        val leaseMs = bootstrap.leaseTimeoutMs
        val subject = PlaybackControlReporter.create(
            bootstrap = bootstrap,
            clientInstanceId = clientInstanceId,
            snapshot = { observe()?.let(PlaybackControlMapping::snapshot) },
            send = { path, request -> transport.send(path, request) },
            pace = { kotlinx.coroutines.delay(it) },
            now = { System.currentTimeMillis() },
            // The return path. Until now this defaulted to a no-op, so the
            // server could send a verdict the player would never see.
            //
            // Only `terminal` is retained, and retained rather than acted on.
            // `hold` and `retry_resource` are exchange-level and the reporter
            // already honours them; a player acting on them here would be
            // deciding, which is the next slice.
            onExchange = { exchange ->
                val action = exchange.response?.action
                if (action != null &&
                    action.type == "terminal" &&
                    !action.message.isNullOrEmpty()
                ) {
                    synchronized(verdictLock) {
                        if (generation == verdictGeneration) {
                            verdict = action
                            verdictArmedAtMs = System.currentTimeMillis()
                            verdictLeaseMs = leaseMs
                        }
                    }
                }
            },
        ) ?: return
        reporter = subject
        scope.launch { subject.start(scope) }
    }

    /**
     * The player changed. Cheap enough to call from a one-second tick: the
     * reporter coalesces, so a notification between exchanges costs nothing
     * but replaces what the next exchange will carry.
     */
    fun playerChanged() {
        val subject = reporter ?: return
        scope.launch { subject.notify() }
    }

    /**
     * A recovery owner published evidence and is about to act on it.
     *
     * Coalescing is right for a position update and wrong for this: the pump
     * sleeps for `next_exchange_ms` and the owner's own reopen normally ends
     * this reporter before it wakes, so the evidence would be discarded rather
     * than sent late. Restricted to callers holding evidence, so the ordinary
     * cadence is unchanged.
     */
    fun reportEvidence() {
        val subject = reporter ?: return
        scope.launch { subject.notifyUrgently(scope) }
    }

    fun end() {
        val subject = reporter ?: return
        reporter = null
        scope.launch { subject.stop() }
    }
}
