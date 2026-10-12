package tv.plurx.app.diagnostics

import android.os.SystemClock
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import tv.plurx.app.data.Session
import java.security.MessageDigest
import java.time.Instant
import java.time.temporal.ChronoUnit
import java.util.UUID
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

@Serializable
internal data class DiagnosticEvent(val run_id: String, val sequence: String, val utc: String, val elapsed_ms: Long, val domain: String, val kind: String, val facts: JsonObject) {
    fun valid(): Boolean = runCatching {
        require(UUID.fromString(run_id).toString() == run_id && DiagnosticWire.decimal(sequence))
        require(utc.length == 20 && Instant.parse(utc).toString() == utc && elapsed_ms in 0..604800000)
        require(domain in DiagnosticFields.domains && kind in DiagnosticFields.kinds && facts.size <= 16)
        require(facts.all { (key, value) -> value is JsonPrimitive && DiagnosticFields.allowed(key, value) })
    }.isSuccess
}

internal object DiagnosticFields {
    val domains = setOf("ui", "network", "playback", "producer", "lifecycle", "coverage")
    val kinds = setOf("action", "state", "sample", "error", "terminal", "request", "availability")
    private val counters = setOf("frame_count", "dropped_frames", "dropped_events")
    private val measurements = setOf("duration_ms", "playhead_ms", "headroom_ms", "width", "height", "retry_count")
    private val codec = setOf("unknown","h264","hevc","av1","vp9","aac","ac3","eac3","dts","truehd","opus","flac","pcm","other")
    private val range = setOf("unknown","sdr","hdr10","hlg","dv")
    private val failures = DiagnosticWire.reasons + setOf("network","tls","dns","connect","authentication","http","decode","delivery","process_exit","browser_network")
    private val enums = mapOf(
        "source_codec" to codec, "delivered_codec" to codec, "render_codec" to codec,
        "source_range" to range, "delivered_range" to range, "render_range" to range,
        "mode" to setOf("unknown","direct","remux","transcode"), "failure" to failures, "reason" to failures,
        "action" to setOf("play","pause","stop","seek","audio","subtitle","quality","navigate","report","reproduce","cancel","check"),
        "state" to setOf("unknown","starting","playing","paused","waiting","stalled","recovered","failed","stopped","foreground","background","interrupted","clean_shutdown"),
        "route" to setOf("auth","browse","playback","settings","diagnostics","support","other"),
        "method" to setOf("GET","POST","PUT","DELETE","HEAD"), "phase" to setOf("dns","connect","tls","auth","readiness","delivery","unavailable")
    )
    private val correlation = Regex("s-[a-f0-9]{64}")
    private val node = Regex("node-[1-8]")
    fun allowed(key: String, value: JsonPrimitive): Boolean {
        if (value.isString) return when (key) {
            in counters -> DiagnosticWire.decimal(value.content)
            "correlation" -> correlation.matches(value.content)
            "node" -> node.matches(value.content)
            else -> value.content in enums[key].orEmpty()
        }
        if (key == "observed") return value.booleanOrNull != null
        val number = value.longOrNull ?: return false
        return if (key == "status") number == 0L || number in 100..599 else key in measurements && number in 0..604800000
    }
    fun hash(bytes: ByteArray): String = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    fun utc(): String = Instant.now().truncatedTo(ChronoUnit.SECONDS).toString()
}

@Serializable
internal data class DiagnosticCapture(
    val id: String, val namespace: String, val generation: Long, val capturedAt: String, val incidentMs: Long,
    val events: List<DiagnosticEvent>, val dropped: String, val persistence: String, val historyEnabled: Boolean,
    val mode: String = "ordinary", val windowMs: Long = 600000,
)

/** A non-waiting callback boundary; one utility owner performs persistence.
 * No player, media session or retry lease is retained in a capture. */
internal class DiagnosticRecorder(
    private val history: () -> Boolean = { true },
    private val persist: suspend (DiagnosticCapture) -> String = { "memory_only" },
    private val complete: suspend (DiagnosticCapture) -> Unit = {},
    private val monotonic: () -> Long = SystemClock::elapsedRealtime,
) {
    companion object { const val MAX_BYTES = 2097152; const val MAX_EVENTS = 10000 }
    private data class Row(val event: DiagnosticEvent, val size: Int)
    private val owner = CoroutineScope(SupervisorJob() + Dispatchers.IO.limitedParallelism(1))
    private val lock = ReentrantLock()
    private val run = UUID.randomUUID().toString()
    private val started = monotonic()
    private var sequence = 0uL
    private val contention = AtomicLong()
    private var dropped = 0uL
    private val rows = mutableListOf<Row>()
    private var bytes = 0
    private var namespace = "anonymous"
    private var generation = Session.playbackAuthorization().generation
    private var persistence = "memory_only"
    private val flushRequests = Channel<Unit>(Channel.CONFLATED)
    private val completedCaptures = Channel<DiagnosticCapture>(Channel.CONFLATED)
    private var reproduction: DiagnosticCapture? = null
    private val reproductionRows = mutableListOf<Row>()
    private var reproductionBytes = 0
    private var reproductionDropped = 0uL
    private var reproductionTimer: Job? = null
    private var bound = false
    init {
        owner.launch { for (ignored in flushRequests) { delay(3000); val captured = freeze(); val result = runCatching { persist(captured) }.getOrDefault("memory_only"); lock.withLock { if (generation == captured.generation && namespace == captured.namespace) persistence = result } } }
        owner.launch { for (captured in completedCaptures) runCatching { complete(captured) } }
    }
    fun retireAuthorization(auth: Long) = lock.withLock {
        namespace = "anonymous"; generation = auth; bound = false; rows.clear(); bytes = 0; dropped = 0uL; contention.set(0)
        reproduction = null; reproductionRows.clear(); reproductionBytes = 0; reproductionTimer?.cancel()
    }
    fun bind(scope: String, auth: Long, recovered: DiagnosticCapture?) = lock.withLock {
        if (Session.playbackAuthorization().generation != auth) return@withLock
        if (bound && namespace != scope) { rows.clear(); bytes = 0; dropped = 0uL; reproduction = null; reproductionTimer?.cancel() }
        namespace = scope; generation = auth; bound = true
        val recent = rows.toList(); rows.clear(); bytes = 0
        recovered?.events?.filter { it.valid() && Instant.parse(it.utc).toEpochMilli() >= System.currentTimeMillis() - 600000 }?.forEach { append(it) }
        recent.forEach { append(it.event) }; dropped += recovered?.dropped?.toULongOrNull() ?: 0uL
        persistence = recovered?.persistence ?: "memory_only"
    }
    fun record(domain: String, kind: String, facts: Map<String, JsonPrimitive> = emptyMap(), auth: Long? = null): Boolean {
        if (!history() || domain !in DiagnosticFields.domains || kind !in DiagnosticFields.kinds || facts.size > 16) return false
        if (!lock.tryLock()) { contention.updateAndGet { if (it == Long.MAX_VALUE) it else it + 1 }; return false }
        try {
            if (auth != null && auth != generation || sequence == ULong.MAX_VALUE) return false
            val event = DiagnosticEvent(run, (++sequence).toString(), DiagnosticFields.utc(), (monotonic() - started).coerceIn(0,604800000), domain, kind,
                JsonObject(facts.filter { (key, value) -> DiagnosticFields.allowed(key, value) }))
            append(event)
            if (reproduction != null) {
                val row = Row(event, DiagnosticJson.encodeToString(DiagnosticEvent.serializer(), event).toByteArray().size + 1)
                if (kind == "sample" && (reproductionRows.size >= 8000 || reproductionBytes + row.size > MAX_BYTES * 4 / 5)) reproductionDropped++
                else {
                    while (reproductionRows.size >= MAX_EVENTS || reproductionBytes + row.size > MAX_BYTES) {
                        val index = reproductionRows.indexOfFirst { it.event.kind == "sample" }.takeIf { it >= 0 } ?: 0
                        reproductionBytes -= reproductionRows.removeAt(index).size; reproductionDropped++
                    }
                    reproductionRows += row; reproductionBytes += row.size
                }
            }
            flushRequests.trySend(Unit)
            return true
        } finally { lock.unlock() }
    }
    private fun append(event: DiagnosticEvent) {
        val row = Row(event, DiagnosticJson.encodeToString(DiagnosticEvent.serializer(), event).toByteArray().size + 1)
        val oldest = Instant.ofEpochMilli(System.currentTimeMillis() - 600000).truncatedTo(ChronoUnit.SECONDS).toString()
        while (rows.firstOrNull()?.event?.utc?.let { it < oldest } == true) { bytes -= rows.removeAt(0).size }
        if (event.kind == "sample" && (rows.size >= 8000 || bytes + row.size > MAX_BYTES * 4 / 5)) { dropped++; return }
        while (rows.size >= MAX_EVENTS || bytes + row.size > MAX_BYTES) {
            val index = rows.indexOfFirst { it.event.kind == "sample" }.takeIf { it >= 0 } ?: 0
            bytes -= rows.removeAt(index).size; dropped++
        }
        rows += row; bytes += row.size
    }
    fun freeze(): DiagnosticCapture = lock.withLock {
        DiagnosticCapture(UUID.randomUUID().toString(), namespace, generation, DiagnosticFields.utc(), System.currentTimeMillis(), rows.map { it.event },
            (dropped + contention.get().toULong()).toString(), persistence, history())
    }
    val recording get() = lock.withLock { reproduction != null }
    fun startReproduction(capture: DiagnosticCapture): Boolean = lock.withLock {
        if (reproduction != null || capture.namespace != namespace || capture.generation != generation) return@withLock false
        reproduction = capture.copy(mode = "reproduction"); reproductionDropped = capture.dropped.toULong()
        reproductionRows.clear(); reproductionRows += capture.events.map { Row(it, DiagnosticJson.encodeToString(DiagnosticEvent.serializer(), it).toByteArray().size + 1) }
        reproductionBytes = reproductionRows.sumOf { it.size }
        reproductionTimer = owner.launch { delay(300000); stopReproduction("timeout") }; true
    }
    fun stopReproduction(reason: String = "cancelled"): DiagnosticCapture? {
        record("lifecycle", "terminal", mapOf("state" to JsonPrimitive(if (reason == "interrupted") "interrupted" else "stopped"), "reason" to JsonPrimitive(reason)))
        val result = lock.withLock {
            val original = reproduction ?: return@withLock null
            val stopped = original.copy(events = reproductionRows.map { it.event }, dropped = reproductionDropped.toString(), windowMs = (600000 + System.currentTimeMillis() - original.incidentMs).coerceIn(0,900000))
            reproduction = null; reproductionRows.clear(); reproductionBytes = 0; reproductionTimer?.cancel(); reproductionTimer = null; stopped
        }
        if (result != null) completedCaptures.trySend(result); return result
    }
    fun background() { stopReproduction("interrupted"); record("lifecycle", "state", mapOf("state" to JsonPrimitive("background"))); flushRequests.trySend(Unit) }
}

internal val DiagnosticJson = Json { encodeDefaults = true; explicitNulls = false; ignoreUnknownKeys = false }
