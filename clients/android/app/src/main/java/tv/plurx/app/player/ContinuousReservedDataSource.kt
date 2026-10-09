@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.net.Uri
import androidx.media3.common.C
import androidx.media3.datasource.BaseDataSource
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.HttpDataSource
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.net.URI
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject

/** Loader-thread provenance lives until close, including extractor callbacks
 * made after read returns. It is never an append acknowledgement by itself. */
internal object ContinuousLoadContext {
    data class Verified(val owner: Any, val resource: ContinuousQualityMedia.Resource, val authorized: ContinuousQualityMedia.Authorized)
    private val local = ThreadLocal<Verified?>()
    fun current(): Verified? = local.get()
    fun bind(value: Verified?) { if (value == null) local.remove() else local.set(value) }
}

/** Bound to one attachment and its captured profile factory. Playlists never
 * fall back to the process-wide transport, and retry ranges are verified using
 * the complete artifact before any requested slice is exposed to extraction. */
internal class ContinuousReservedDataSource(
    private val owner: Any,
    private val upstreamFactory: DataSource.Factory,
    origin: String,
    schedulePath: String,
    family: JsonObject,
    private val media: ContinuousQualityMedia,
    private val reserve: suspend (ContinuousQualityMedia.Resource, ByteArray?) -> Unit,
    private val loads: ContinuousLoads = ContinuousLoads(),
    private val cancelNetwork: (() -> Unit)? = null,
    private val beforeReserve: suspend (ContinuousQualityMedia.Resource) -> Unit = {},
    private val publish: (ContinuousLoadContext.Verified) -> Unit = {},
    private val retainFailure: suspend (ContinuousQualityMedia.Resource) -> Boolean = { false },
    private val beforeAuthorize: suspend (ContinuousQualityMedia.Resource, ByteArray) -> Unit = { _, _ -> },
    private val eofObserved: (ContinuousNetworkEof) -> Unit = {},
    private val cacheAbsent: Boolean? = null,

) : BaseDataSource(false) {
    private val diagnosticFamilyId = family.text("family_id").orEmpty()
    private val diagnosticOriginSha256 = ContinuousQualityMedia.digest(origin.toByteArray(Charsets.UTF_8))
    private val origin = URI(origin)
    private val parent = schedulePath.removeSuffix("quality-schedule")
    private val playlists = buildSet {
        add(requireNotNull(family.text("master")))
        family.getValue("video").jsonArray.forEach { add(requireNotNull(it.jsonObject.text("playlist"))) }
        family.obj("audio")?.text("playlist")?.let(::add)
    }
    private val lifetime = Any()
    private val opening = AtomicReference<Job?>(null)
    private val upstream = AtomicReference<DataSource?>(null)
    private var payload: ByteArray? = null
    private var position = 0
    private var end = 0
    private var uri: Uri? = null
    private var started = false
    private var headers: Map<String, List<String>> = emptyMap()

    override fun open(dataSpec: DataSpec): Long {
        check(uri == null) { "Continuous source already open" }
        ContinuousLoadContext.bind(null)
        val parsed = try { URI(dataSpec.uri.toString()) } catch (_: Exception) { throw IOException("Continuous media URI") }
        if (!sameOrigin(parsed) || parsed.rawUserInfo != null || parsed.rawFragment != null ||
            !parsed.rawPath.startsWith(parent) || dataSpec.httpMethod != DataSpec.HTTP_METHOD_GET ||
            dataSpec.httpBody != null || dataSpec.position < 0 || dataSpec.length < C.LENGTH_UNSET) {
            throw IOException("Continuous media request outside captured attachment")
        }
        val resource = media.resource(dataSpec.uri.toString())
        if (resource == null && parsed.rawPath.removePrefix(parent) !in playlists) {
            throw IOException("Continuous media path outside captured family")
        }
        val job = Job()
        if (!opening.compareAndSet(null, job)) throw IOException("Continuous media open already pending")
        try {
            loads.opened(this)
            if (resource != null && !resource.initialization) blocking(job) { beforeReserve(resource) }
            transferInitializing(dataSpec)
            if (resource?.role == "video" && !resource.initialization) blocking(job) { reserve(resource, null) }
            if (!job.isActive || !loads.isAlive()) throw IOException("Continuous media request cancelled")
            val source = upstreamFactory.createDataSource()
            upstream.set(source)
            if (!job.isActive || !loads.isAlive()) throw IOException("Continuous media request cancelled")
            // Always request the whole immutable resource, even on Media3 retry.
            val whole = dataSpec.buildUpon().setPosition(0).setLength(C.LENGTH_UNSET.toLong())
                .setHttpRequestHeaders(dataSpec.httpRequestHeaders.filterKeys { !it.equals("Range", true) && !it.equals("If-Range", true) }).build()
            val known = source.open(whole)
            // Headers have arrived; measure actual body consumption, not producer/header wait.
            val bodyStartedAtMs = runCatching { android.os.SystemClock.elapsedRealtime() }.getOrNull()
            val bound = if (resource == null) 2 * 1024 * 1024 else ContinuousQualityMedia.MAX_MEDIA_BYTES
            if (known > bound) throw IOException("Continuous media payload bound")
            val bytes = ByteArrayOutputStream()
            val buffer = ByteArray(8192)
            var readEof = false
            while (true) {
                if (!job.isActive || !loads.isAlive()) throw IOException("Continuous media request cancelled")
                val count = source.read(buffer, 0, buffer.size)
                if (count == C.RESULT_END_OF_INPUT) { readEof = true; break }
                if (count <= 0) throw IOException("Continuous media read made no progress")
                if (bytes.size() + count > bound) throw IOException("Continuous media payload bound")
                bytes.write(buffer, 0, count)
            }
            val eofAtMs = runCatching { android.os.SystemClock.elapsedRealtime() }.getOrNull()
            val bodyDurationMs = if (eofAtMs != null && bodyStartedAtMs != null)
                (eofAtMs - bodyStartedAtMs).takeIf { it > 0 } else null
            val network = source is HttpDataSource
            val responseCode = runCatching { (source as? HttpDataSource)?.responseCode }.getOrNull()
            headers = source.responseHeaders
            source.close()
            upstream.compareAndSet(source, null)
            val retained = bytes.toByteArray()
            if (resource != null && !resource.initialization) blocking(job) { beforeAuthorize(resource, retained) }
            if (resource?.role == "audio" && !resource.initialization) blocking(job) { reserve(resource, retained) }
            val authorization = resource?.let { media.authorize(it, retained) }
            val slice = ContinuousVerifiedRange.resolve(retained.size, dataSpec.position, dataSpec.length)
            synchronized(lifetime) {
                if (!job.isActive || !loads.isAlive() || opening.get() !== job) throw IOException("Continuous media request cancelled")
                if (resource != null && authorization != null) publish(ContinuousLoadContext.Verified(owner, resource, authorization))
                payload = retained
                position = slice.first
                end = slice.second
                uri = dataSpec.uri
                if (resource != null && authorization != null) ContinuousLoadContext.bind(ContinuousLoadContext.Verified(owner, resource, authorization))
                transferStarted(dataSpec)
                started = true
                // Reporting is downstream of EOF, immutable authorization and
                // the same current-owner fence as publication. It cannot fail open().
                runCatching {
                    val paced = headers.entries.firstOrNull { it.key.equals("X-Plurx-Producer-Paced", true) }
                        ?.value?.singleOrNull().let { when(it) { "0" -> false; "1" -> true; else -> null } }
                    continuousNetworkEof(readEof, job.isActive && loads.isAlive() && opening.get() === job,
                        diagnosticFamilyId, diagnosticOriginSha256, resource, authorization,
                        retained.size.toLong(), network, responseCode, cacheAbsent, paced, bodyDurationMs, eofAtMs)?.let(eofObserved)
                }
            }
            return (slice.second - slice.first).toLong()
        } catch (error: Exception) {
            val retained = if (error !is ContinuousStaleVideoLoad && resource?.role == "video" && !resource.initialization &&
                !started && job.isActive && loads.isAlive()) {
                try { blocking(job) { retainFailure(resource) } } catch (_: Exception) { false }
            } else false
            close()
            if (retained) throw ContinuousStaleVideoLoad()
            throw if (error is IOException) error else IOException("Continuous media load refused", error)
        } finally {
            job.complete()
            opening.compareAndSet(job, null)
        }
    }

    override fun read(buffer: ByteArray, offset: Int, length: Int): Int {
        if (length == 0) return 0
        if (!loads.isAlive()) throw IOException("Continuous attachment closed")
        val bytes = payload ?: throw IOException("Continuous media source not open")
        if (position == end) return C.RESULT_END_OF_INPUT
        val count = minOf(length, end - position)
        bytes.copyInto(buffer, offset, position, position + count)
        position += count
        bytesTransferred(count)
        return count
    }
    override fun getUri(): Uri? = uri
    override fun getResponseHeaders(): Map<String, List<String>> = headers
    /** May run on the attachment thread. It fences I/O without retiring the
     * loader-thread lease or clearing that thread's extraction provenance. */
    fun cancelPending() {
        synchronized(lifetime) { opening.get()?.cancel() }
        try {
            if (cancelNetwork != null) cancelNetwork.invoke() else upstream.get()?.close()
        } catch (_: Exception) { /* Loader finally still owns retirement. */ }
    }

    override fun close() {
        try {
            synchronized(lifetime) { opening.getAndSet(null)?.cancel() }
            val source = upstream.getAndSet(null)
            try { source?.close() } finally {
                ContinuousLoadContext.bind(null)
                payload = null
                uri = null
                headers = emptyMap()
                position = 0
                end = 0
                if (started) { started = false; transferEnded() }
            }
        } finally { loads.closed(this) }
    }
    private fun <T> blocking(job: Job, action: suspend () -> T): T = try {
        runBlocking(job) { action() }
    } catch (error: CancellationException) { throw IOException("Continuous media reservation cancelled", error) }
    private fun sameOrigin(other: URI): Boolean = other.scheme.equals(origin.scheme, true) &&
        other.host?.equals(origin.host, true) == true && port(other) == port(origin)
    private fun port(value: URI): Int = if (value.port >= 0) value.port else if (value.scheme.equals("https", true)) 443 else 80
}

/** A retry consumes a slice only after its complete artifact is verified. */
internal object ContinuousVerifiedRange {
    fun resolve(size: Int, position: Long, length: Long): Pair<Int, Int> {
        if (size < 0 || position !in 0..size.toLong() || length < -1 ||
            length != -1L && length > size - position) throw IOException("Continuous media range outside verified artifact")
        val end = if (length == -1L) size else (position + length).toInt()
        return position.toInt() to end
    }
}
