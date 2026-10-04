package tv.plurx.app.data

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.ColorSpace
import android.graphics.Paint
import java.io.Closeable
import java.lang.ref.PhantomReference
import java.lang.ref.ReferenceQueue
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.InternalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable

@Serializable
internal data class SharedArtworkDescriptor(val kind: String, val variant: String, val url: String) {
    fun validate(reference: SharedPlaybackReference) {
        reference.validate()
        val prefix = "/api/v1/shared/imports/${reference.import_id}/art/"
        require(kind in setOf("poster", "backdrop") && variant in setOf("original", "w300", "w500", "w780"))
        require(url.startsWith(prefix) && Regex("[A-Za-z0-9_-]{272}").matches(url.removePrefix(prefix)))
    }
    companion object {
        fun validate(rows: List<SharedArtworkDescriptor>?, poster: String?, backdrop: String?, reference: SharedPlaybackReference) {
            val values = rows.orEmpty()
            require(values.size <= 8 && values.map { it.kind to it.variant }.toSet().size == values.size)
            values.forEach { it.validate(reference) }
            poster?.let { require(values.any { row -> row.kind == "poster" && row.variant == "w300" && row.url == it }) }
            backdrop?.let { require(values.any { row -> row.kind == "backdrop" && row.variant == "w780" && row.url == it }) }
        }
    }
}

/** Credits do not follow LRU membership. A verified owner release is required. */
internal class SharedArtworkBudget(private val limit: Long, private val operationLimit: Int) {
    private var used = 0L
    private var active = 0
    private val issued = mutableMapOf<Lease, Long>()
    @Synchronized fun acquire(bytes: Long): Lease {
        require(bytes > 0 && bytes <= limit && active < operationLimit && used <= limit - bytes) { "Shared artwork capacity is busy" }
        used += bytes; active++
        return Lease(this, bytes).also { issued[it] = bytes }
    }
    @Synchronized private fun release(lease: Lease) { val bytes = requireNotNull(issued.remove(lease)); used -= bytes; active-- }
    @Synchronized private fun owns(lease: Lease): Boolean = issued[lease] == lease.bytes
    @Synchronized fun retainedBytes(): Long = used
    class Lease internal constructor(private val owner: SharedArtworkBudget, val bytes: Long) : Closeable {
        private val closed = AtomicBoolean(false)
        fun validFor(budget: SharedArtworkBudget, count: Long): Boolean = owner === budget && bytes == count && !closed.get() && owner.owns(this)
        override fun close() { if (closed.compareAndSet(false, true)) owner.release(this) }
    }
    companion object {
        const val ASSET_LIMIT = 15 * 1024 * 1024
        val compressed = SharedArtworkBudget(64L * 1024 * 1024, 4)
        val bitmaps = SharedArtworkBudget(64L * 1024 * 1024, Int.MAX_VALUE)
    }
}

/** Raw bytes remain private; only the admitted thumbnail decoder borrows them. */
internal class SharedArtworkPayload private constructor(private var data: ByteArray?, val length: Int, val mime: String,
    private var admission: SharedArtworkBudget.Lease?) : Closeable {
    @Synchronized fun thumbnail(maximum: Int): SharedArtworkBitmap {
        require(maximum == 300 || maximum == 780)
        val bytes = requireNotNull(data)
        val workspace = SharedArtworkBudget.bitmaps.acquire(maximum.toLong() * maximum * 48)
        var decoded: Bitmap? = null
        var result: Bitmap? = null
        try {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeByteArray(bytes, 0, length, bounds)
            require(bounds.outMimeType == mime && bounds.outWidth in 1..8192 && bounds.outHeight in 1..8192 && bounds.outWidth.toLong() * bounds.outHeight <= 16_777_216)
            var sample = 1
            while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= maximum) sample *= 2
            val options = BitmapFactory.Options().apply {
                inSampleSize = sample; inPreferredConfig = Bitmap.Config.ARGB_8888
                inPreferredColorSpace = ColorSpace.get(ColorSpace.Named.SRGB)
            }
            val first = requireNotNull(BitmapFactory.decodeByteArray(bytes, 0, length, options)); decoded = first
            require(first.width <= maximum * 2 && first.height <= maximum * 2 && first.allocationByteCount.toLong() <= workspace.bytes)
            val dimension = maxOf(first.width, first.height)
            val target = if (dimension > maximum) Bitmap.createScaledBitmap(first,
                maxOf(1, first.width * maximum / dimension), maxOf(1, first.height * maximum / dimension), true) else first
            result = target
            require(target.width <= maximum && target.height <= maximum && target.allocationByteCount <= maximum * maximum * 8)
            if (target !== first) { first.recycle(); require(first.isRecycled); decoded = null }
            val credit = SharedArtworkBudget.bitmaps.acquire(target.allocationByteCount.toLong())
            val owned = SharedArtworkBitmap.own(target, credit); result = null; decoded = null
            return owned
        } finally {
            result?.recycle(); if (decoded !== result) decoded?.recycle()
            // Confirmed synchronous recycle precedes workspace credit release.
            if ((result == null || result.isRecycled) && (decoded == null || decoded.isRecycled)) workspace.close()
        }
    }
    @Synchronized override fun close() { data = null; admission?.close(); admission = null }
    companion object {
        @OptIn(InternalCoroutinesApi::class)
        suspend fun fetch(plan: SharedArtworkReadPlan): SharedArtworkPayload {
            requireAuthenticatedSharedArtworkPlan(plan)
            val pending = AtomicReference<SharedArtworkPayload?>(null)
            try {
                val delivered = withContext(Dispatchers.IO) {
                    plan.requireCurrent()
                    val admission = SharedArtworkBudget.compressed.acquire(SharedArtworkBudget.ASSET_LIMIT.toLong())
                    val call = plan.transport.newCall(plan.request)
                    val cancellation = currentCoroutineContext()[Job]?.invokeOnCompletion(onCancelling = true, invokeImmediately = true) { cause -> if (cause != null) call.cancel() }
                    val observation = Session.observeAuthorizationChanges { call.cancel() }
                    var transferred = false
                    try {
                        require(observation.generation == plan.generation); plan.requireCurrent(); currentCoroutineContext().ensureActive()
                        call.execute().use { response ->
                            plan.requireCurrent(); require(response.request.url == plan.request.url && !response.isRedirect)
                            require(response.code == 200) { "Shared artwork returned ${response.code}" }
                            val body = requireNotNull(response.body)
                            require(body.contentLength() < 0 || body.contentLength() <= SharedArtworkBudget.ASSET_LIMIT)
                            val mime = response.header("Content-Type")?.substringBefore(';')?.trim()?.lowercase()
                            require(mime in setOf("image/png", "image/jpeg", "image/webp"))
                            val bytes = ByteArray(SharedArtworkBudget.ASSET_LIMIT)
                            val source = body.source()
                            var length = 0
                            while (length < bytes.size) {
                                currentCoroutineContext().ensureActive(); plan.requireCurrent()
                                val count = source.read(bytes, length, minOf(65536, bytes.size - length))
                                if (count < 0) break
                                length += count
                            }
                            require(length > 0 && source.exhausted()) { "Shared artwork exceeds its bound" }
                            currentCoroutineContext().ensureActive(); plan.requireCurrent()
                            transferred = true
                            SharedArtworkPayload(bytes, length, requireNotNull(mime), admission).also { pending.set(it) }
                        }
                    } catch (error: Exception) {
                        currentCoroutineContext().ensureActive(); throw error
                    } finally {
                        Session.removeAuthorizationObserver(observation.id); cancellation?.dispose(); call.cancel()
                        if (!transferred) admission.close()
                    }
                }
                pending.set(null); return delivered
            } finally { pending.getAndSet(null)?.close() }
        }
    }
}

/** Raw Bitmap never escapes. UI consumers retain this owner until actual
 * reachability retirement; queueing cleanup alone does not release credits. */
internal class SharedArtworkBitmap private constructor(private val record: Record) {
    fun retireUnpublished() { record.retire() }
    val width: Int get() = record.bitmap.width
    val height: Int get() = record.bitmap.height
    fun draw(canvas: Canvas, paint: Paint) { synchronized(record) { check(!record.bitmap.isRecycled); canvas.drawBitmap(record.bitmap, 0f, 0f, paint) } }
    private class Record(val bitmap: Bitmap, val credit: SharedArtworkBudget.Lease) {
        fun retire(): Boolean = synchronized(this) {
            try { bitmap.recycle(); if (!bitmap.isRecycled) return false; credit.close(); true } catch (_: Exception) { false }
        }
    }
    private class Retirement(owner: SharedArtworkBitmap, queue: ReferenceQueue<SharedArtworkBitmap>, val record: Record) : PhantomReference<SharedArtworkBitmap>(owner, queue)
    companion object {
        private val queue = ReferenceQueue<SharedArtworkBitmap>()
        private val tracked = mutableSetOf<Retirement>()
        private val unresolved = mutableSetOf<Record>()
        init {
            Thread({ while (true) { try {
                val retirement = queue.remove() as Retirement
                val released = retirement.record.retire()
                synchronized(tracked) { tracked.remove(retirement); if (!released) unresolved.add(retirement.record) }
                retirement.clear()
            } catch (_: InterruptedException) { /* No credit is returned for interrupted cleanup. */ } } }, "shared-artwork-retirement").apply { isDaemon = true; start() }
        }
        internal fun own(bitmap: Bitmap, credit: SharedArtworkBudget.Lease): SharedArtworkBitmap {
            require(credit.validFor(SharedArtworkBudget.bitmaps, bitmap.allocationByteCount.toLong()))
            val record = Record(bitmap, credit); val owner = SharedArtworkBitmap(record)
            synchronized(tracked) { tracked.add(Retirement(owner, queue, record)) }; return owner
        }
    }
}
