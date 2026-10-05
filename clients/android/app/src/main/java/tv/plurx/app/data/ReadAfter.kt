package tv.plurx.app.data

import okhttp3.Call
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response

/**
 * The signed-in session's watch-write floor: the newest commit index a write
 * reply carried, echoed on later API reads so a non-leader node can answer
 * them locally behind its fence (`docs/cluster/BOUNDED-REPLICA-READS-ROLLOUT.md`).
 *
 * This is the web contract (`crates/plurxd/src/web/core/api.js`, test
 * `tests/web/read-after.test.js`) as one lock-guarded type, because native
 * requests are concurrent and nothing here may depend on the main thread:
 *
 * - capture `^[1-9][0-9]{0,19}$` within u64; `unknown` or anything malformed
 *   forgets the held value, and that check runs before the epoch check so a
 *   malformed reply from a stale epoch still forgets;
 * - forward only: a larger value replaces, an equal value restarts the 60 s
 *   monotonic expiry, a smaller one is ignored;
 * - every request records the epoch it was sent under and a reply from an
 *   older epoch is ignored; every forget bumps the epoch, because an unknown
 *   write invalidates already-in-flight indexed replies too;
 * - a reply to a mutation (neither GET nor HEAD) with no header, or a
 *   mutation that failed in transport, forgets (an older peer answered, or the
 *   write's outcome is unknown);
 * - the auth generation is the signed-in session: a change of origin or
 *   bearer forgets the value and bumps the epoch, and a reply sent under an
 *   older generation is ignored;
 * - one value per signed-in session, whichever node answers.
 */
class ReadAfterFloor(
    /** Monotonic nanoseconds. */
    private val clock: () -> Long = System::nanoTime,
) {
    /** What one request was sent under. [index] is the header to send, if any. */
    data class Ticket(val generation: Long, val epoch: Long, val index: String?)

    private val lock = Any()
    private var generation = 0L
    private var epoch = 0L
    private var index: ULong? = null
    private var expiresAtNanos = 0L

    /** The current auth generation, for tagging a request at creation. */
    fun generation(): Long = synchronized(lock) { generation }

    /** Sign-in, sign-out, an account change or an origin change. */
    fun signedInSessionChanged() = synchronized(lock) {
        generation += 1
        epoch += 1
        index = null
        expiresAtNanos = 0L
    }

    /**
     * Start a request bound to [requestGeneration]. A request bound to an
     * older signed-in session sends nothing, and its reply is ignored.
     */
    fun request(requestGeneration: Long): Ticket = synchronized(lock) {
        if (index != null && clock() - expiresAtNanos >= 0) index = null
        val held = if (requestGeneration == generation) index?.toString() else null
        Ticket(requestGeneration, epoch, held)
    }

    /** A reply arrived. [commitIndex] is the raw header, null when absent. */
    fun observe(ticket: Ticket, method: String, commitIndex: String?) = synchronized(lock) {
        if (ticket.generation != generation) return@synchronized
        if (commitIndex == null) {
            // Older peers may acknowledge a write without an indexed receipt.
            if (isMutation(method)) forgetLocked()
            return@synchronized
        }
        val value = parse(commitIndex)
        if (value == null) {
            forgetLocked()
            return@synchronized
        }
        // An unknown write invalidates already-in-flight indexed replies too.
        if (ticket.epoch != epoch) return@synchronized
        val previous = index
        if (previous == null || value >= previous) {
            index = value
            expiresAtNanos = clock() + EXPIRY_NANOS
        }
    }

    /** The request never produced a reply. A write's outcome is now unknown. */
    fun transportFailed(ticket: Ticket, method: String) = synchronized(lock) {
        if (ticket.generation != generation) return@synchronized
        if (isMutation(method)) forgetLocked()
    }

    private fun forgetLocked() {
        epoch += 1
        index = null
        expiresAtNanos = 0L
    }

    companion object {
        const val READ_AFTER_HEADER = "x-plurx-read-after"
        const val COMMIT_INDEX_HEADER = "x-plurx-commit-index"
        const val EXPIRY_NANOS = 60_000_000_000L
        private val SHAPE = Regex("^[1-9][0-9]{0,19}$")

        fun isMutation(method: String): Boolean = method != "GET" && method != "HEAD"

        /** A decimal u64 greater than zero, or null for `unknown` and anything malformed. */
        fun parse(raw: String): ULong? = if (SHAPE.matches(raw)) raw.toULongOrNull() else null
    }
}

/**
 * Marks a request as an API call of a signed-in session, so [ReadAfterInterceptor]
 * echoes and captures for it. Images, media, segments, downloads, logs, the
 * captured-token logout, offline books and LAN probes carry no binding.
 */
data class ReadAfterBinding(val generation: Long)

/** The one interceptor. Requests without a [ReadAfterBinding] pass untouched. */
class ReadAfterInterceptor(private val floor: ReadAfterFloor) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        val binding = request.tag(ReadAfterBinding::class.java) ?: return chain.proceed(request)
        val ticket = floor.request(binding.generation)
        val outgoing = ticket.index
            ?.let { request.newBuilder().header(ReadAfterFloor.READ_AFTER_HEADER, it).build() }
            ?: request
        val response = try {
            chain.proceed(outgoing)
        } catch (error: Exception) {
            floor.transportFailed(ticket, request.method)
            throw error
        }
        // Repeated headers read as one comma-joined value, as `fetch` does,
        // and are therefore malformed.
        val values = response.headers(ReadAfterFloor.COMMIT_INDEX_HEADER)
        floor.observe(ticket, request.method, if (values.isEmpty()) null else values.joinToString(", "))
        return response
    }
}

/** The app's floor. [Session] advances its generation; [Net] binds API calls to it. */
object ReadAfter {
    val floor = ReadAfterFloor()
    val interceptor = ReadAfterInterceptor(floor)

    /** Calls made through the returned factory carry the binding [generation] chooses at call creation. */
    fun binding(client: OkHttpClient, generation: () -> Long): Call.Factory = object : Call.Factory {
        override fun newCall(request: Request): Call =
            client.newCall(request.newBuilder().tag(ReadAfterBinding::class.java, ReadAfterBinding(generation())).build())
    }
}
