package tv.plurx.app.data

import android.util.Log
import okhttp3.Call
import okhttp3.Connection
import okhttp3.EventListener
import java.io.IOException
import java.util.concurrent.atomic.AtomicLong

/** D-03 dispatcher measurement. No request identifier or authority enters a record. */
internal object NetCallDiagnostics {
    enum class Route { Images, Decision, Other }
    enum class Method { Get, Post, Head, Put, Delete, Patch, Options, Other }
    enum class Phase { CallStart, ConnectionAcquired, ResponseHeadersStart, CallEnd, CallFailed, Cancelled }

    data class Record(
        val call: Long,
        val route: Route,
        val method: Method,
        val phase: Phase,
        val monotonicUs: Long,
        val elapsedUs: Long,
        val outstandingImages: Int,
    ) {
        fun line(): String = "call=$call route=${route.name} method=${method.name} " +
            "phase=${phase.name} monotonic_us=$monotonicUs elapsed_us=$elapsedUs " +
            "images_outstanding=$outstandingImages"
    }

    private val decisionPath = Regex("^/api/v1/files/[0-9]+/decision$")

    internal fun route(path: String): Route = when {
        path.startsWith("/api/v1/images/") -> Route.Images
        decisionPath.matches(path) -> Route.Decision
        else -> Route.Other
    }

    internal fun method(value: String): Method = when (value) {
        "GET" -> Method.Get
        "POST" -> Method.Post
        "HEAD" -> Method.Head
        "PUT" -> Method.Put
        "DELETE" -> Method.Delete
        "PATCH" -> Method.Patch
        "OPTIONS" -> Method.Options
        else -> Method.Other
    }

    /** Installed only by Net's debug variant. Counts queued AND running image
     * calls to the same origin from callStart until callEnd/callFailed; it is
     * not server concurrency. Origin keys stay private and disappear at zero.
     * One first connection/header observation per call bounds retries' log volume.
     * All durations use the same monotonic clock, never wall-clock subtraction. */
    fun factory(
        clock: () -> Long = System::nanoTime,
        sink: (Record) -> Unit = { Log.d("PlurxNet", it.line()); Unit },
    ): EventListener.Factory {
        val ids = AtomicLong()
        val images = ImageCounts()
        return EventListener.Factory { call ->
            val request = call.request()
            val origin = Origin(request.url.scheme, request.url.host, request.url.port)
            Listener(ids.incrementAndGet(), route(request.url.encodedPath), method(request.method), origin, images, clock, sink)
        }
    }

    private data class Origin(val scheme: String, val host: String, val port: Int)

    private class ImageCounts {
        private val counts = mutableMapOf<Origin, Int>()
        @Synchronized fun add(origin: Origin) { counts[origin] = (counts[origin] ?: 0) + 1 }
        @Synchronized fun remove(origin: Origin) {
            val remaining = (counts[origin] ?: 0) - 1
            if (remaining <= 0) counts.remove(origin) else counts[origin] = remaining
        }
        @Synchronized fun get(origin: Origin): Int = counts[origin] ?: 0
    }

    private class Listener(
        private val id: Long,
        private val route: Route,
        private val method: Method,
        private val origin: Origin,
        private val images: ImageCounts,
        private val clock: () -> Long,
        private val sink: (Record) -> Unit,
    ) : EventListener() {
        private var start: Long? = null
        private var finished = false
        private var acquired = false
        private var headers = false

        @Synchronized override fun callStart(call: Call) {
            if (start != null || finished) return
            start = clock()
            if (route == Route.Images) images.add(origin)
            emit(Phase.CallStart)
        }

        @Synchronized override fun connectionAcquired(call: Call, connection: Connection) {
            if (start == null || finished || acquired) return
            acquired = true
            emit(Phase.ConnectionAcquired)
        }

        @Synchronized override fun responseHeadersStart(call: Call) {
            if (start == null || finished || headers) return
            headers = true
            emit(Phase.ResponseHeadersStart)
        }

        @Synchronized override fun callEnd(call: Call) = finish(Phase.CallEnd)

        @Synchronized override fun callFailed(call: Call, ioe: IOException) =
            finish(if (call.isCanceled()) Phase.Cancelled else Phase.CallFailed)

        private fun finish(phase: Phase) {
            if (start == null || finished) return
            finished = true
            if (route == Route.Images) images.remove(origin)
            emit(phase)
        }

        private fun emit(phase: Phase) {
            val now = clock()
            val record = Record(id, route, method, phase, now / 1_000L,
                ((now - requireNotNull(start)) / 1_000L).coerceAtLeast(0L), images.get(origin))
            // Diagnostics must never turn a logging failure into a network failure.
            runCatching { sink(record) }
        }
    }
}
