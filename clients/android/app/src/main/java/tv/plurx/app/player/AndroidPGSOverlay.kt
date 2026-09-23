package tv.plurx.app.player

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.SystemClock
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.decodeFromString
import okhttp3.ResponseBody
import retrofit2.Response
import tv.plurx.app.data.Net
import tv.plurx.app.data.PlurxApi
import tv.plurx.app.data.parseRefusal
import java.util.Locale
import kotlin.math.ceil

internal data class PGSOverlayRenderedObjectOf<I : Any>(
    val object_: PGSOverlayObject,
    val bitmap: I,
)

internal data class PGSOverlayFrameOf<I : Any>(
    val revision: Long,
    val cue: PGSOverlayCue,
    val objects: List<PGSOverlayRenderedObjectOf<I>>,
)

internal typealias PGSOverlayRenderedObject = PGSOverlayRenderedObjectOf<Bitmap>
internal typealias PGSOverlayFrame = PGSOverlayFrameOf<Bitmap>

internal sealed interface PGSOverlayManifestFetch {
    data class Preparing(val retryAfterMs: Int) : PGSOverlayManifestFetch
    data class Ready(val manifest: PGSOverlayManifest) : PGSOverlayManifestFetch
}

/**
 * Where the overlay's bytes come from. Production is [RetrofitPGSOverlaySource];
 * the JVM suite supplies its own image type, which is the only reason the
 * controller is generic.
 */
internal interface PGSOverlaySource<I : Any> {
    suspend fun manifest(fileId: Long, index: Long): PGSOverlayManifestFetch

    suspend fun image(
        fileId: Long,
        index: Long,
        generation: String,
        hash: String,
        object_: PGSOverlayObject,
    ): I

    fun imageBytes(image: I): Long
}

private const val MAXIMUM_MANIFEST_BYTES = 64 * 1_024 * 1_024
private const val MAXIMUM_PNG_BYTES = 36 * 1_024 * 1_024
private const val MAXIMUM_REFUSAL_BYTES = 16_384L

/** A refusal is a sentence, not a payload; anything larger is not one. */
internal fun boundedErrorBody(response: Response<*>): String? =
    runCatching {
        response.errorBody()?.use { body ->
            val source = body.source()
            source.request(MAXIMUM_REFUSAL_BYTES + 1)
            if (source.buffer.size > MAXIMUM_REFUSAL_BYTES) null else source.buffer.readUtf8()
        }
    }.getOrNull()

/**
 * One manifest answer, read. Retrofit leaves `body()` null for every non-2xx,
 * so the answer the server gave is only in `errorBody()`; reading `body()`
 * first made a 404, 422 or 500 read "empty PGS overlay response". Held to
 * `tests/playback/pgs-overlay-cases.json` `manifest_responses` through a real
 * `retrofit2.Response`.
 */
internal fun readPGSOverlayManifestResponse(response: Response<ResponseBody>): PGSOverlayManifestFetch {
    if (!response.isSuccessful) {
        return when (
            val refusal = PGSOverlayPolicy.manifestRefusal(
                response.code(),
                response.headers()["Retry-After"],
                boundedErrorBody(response),
            )
        ) {
            is PGSOverlayRefusal.Wait -> PGSOverlayManifestFetch.Preparing(refusal.retryAfterMs)
            is PGSOverlayRefusal.Terminal -> error(refusal.message)
        }
    }
    val disposition = PGSOverlayPolicy.manifestDisposition(response.code())
    val body = response.body() ?: error("The server returned an empty PGS overlay response.")
    body.use {
        require(
            response.headers()["Content-Type"]
                ?.lowercase(Locale.ROOT)
                ?.startsWith("application/json") == true,
        ) {
            "The server returned an invalid PGS overlay response."
        }
        require(it.contentLength() <= MAXIMUM_MANIFEST_BYTES) {
            "The PGS overlay manifest is too large."
        }
        val bytes = it.bytes()
        require(bytes.size <= MAXIMUM_MANIFEST_BYTES) {
            "The PGS overlay manifest is too large."
        }
        return when (disposition) {
            PGSOverlayManifestDisposition.Ready ->
                PGSOverlayManifestFetch.Ready(Net.json.decodeFromString<PGSOverlayManifest>(bytes.decodeToString()))
            PGSOverlayManifestDisposition.Preparing -> {
                val preparing = Net.json.decodeFromString<PGSOverlayPreparing>(bytes.decodeToString())
                require(preparing.state == "preparing") { "invalid PGS preparation response" }
                PGSOverlayManifestFetch.Preparing(preparing.retryAfterMs.coerceIn(250, 5_000))
            }
            PGSOverlayManifestDisposition.Terminal ->
                error("The PGS overlay request failed (${response.code()}).")
        }
    }
}

/** The authenticated manifest and PNG routes, decoded to `Bitmap`s. */
internal class RetrofitPGSOverlaySource(
    private val api: () -> PlurxApi,
) : PGSOverlaySource<Bitmap> {
    override suspend fun manifest(fileId: Long, index: Long): PGSOverlayManifestFetch =
        readPGSOverlayManifestResponse(api().pgsOverlayManifest(fileId, index))

    override suspend fun image(
        fileId: Long,
        index: Long,
        generation: String,
        hash: String,
        object_: PGSOverlayObject,
    ): Bitmap {
        val response = api().pgsOverlayObject(fileId, index, generation, hash)
        if (!response.isSuccessful) {
            val code = parseRefusal(response.code(), boundedErrorBody(response))?.code
            error(
                PGSOverlayPolicy.refusalMessage(
                    code,
                    "The PGS subtitle image request failed (${response.code()}).",
                ),
            )
        }
        val body = response.body() ?: error("The server returned an empty PGS subtitle image.")
        body.use {
            require(
                response.headers()["Content-Type"]
                    ?.lowercase(Locale.ROOT)
                    ?.startsWith("image/png") == true,
            ) {
                "The server returned a non-PNG subtitle image."
            }
            require(it.contentLength() <= MAXIMUM_PNG_BYTES) { "The PGS subtitle image is too large." }
            val bytes = it.bytes()
            require(bytes.size <= MAXIMUM_PNG_BYTES) { "The PGS subtitle image is too large." }
            return withContext(Dispatchers.Default) {
                val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
                require(bounds.outWidth == object_.width && bounds.outHeight == object_.height) {
                    "The PGS subtitle image dimensions do not match its manifest."
                }
                val expectedBytes = Math.multiplyExact(
                    Math.multiplyExact(object_.width.toLong(), object_.height.toLong()),
                    4L,
                )
                require(expectedBytes in 1..PGSOverlayPolicy.maximumObjectBytes) {
                    "The PGS subtitle image exceeds the decoded-image limit."
                }
                val bitmap = BitmapFactory.decodeByteArray(
                    bytes,
                    0,
                    bytes.size,
                    BitmapFactory.Options().apply { inPreferredConfig = Bitmap.Config.ARGB_8888 },
                ) ?: error("The server returned an invalid PGS subtitle image.")
                require(bitmap.width == object_.width && bitmap.height == object_.height) {
                    "The decoded PGS subtitle image dimensions changed."
                }
                require(bitmap.allocationByteCount.toLong() in 1..PGSOverlayPolicy.maximumObjectBytes) {
                    "The decoded PGS subtitle image exceeds the memory limit."
                }
                bitmap
            }
        }
    }

    override fun imageBytes(image: Bitmap): Long = image.allocationByteCount.toLong()
}

/** The production controller: Retrofit, `Bitmap`s and the device's clock. */
@Suppress("FunctionName")
internal fun AndroidPGSOverlayController(
    api: () -> PlurxApi,
    scope: CoroutineScope,
    fileId: Long,
    sourcePositionMs: () -> Long,
    isPlaying: () -> Boolean,
    playbackSpeed: () -> Float,
    onFrame: (PGSOverlayFrame?) -> Unit,
    onStatus: (PGSOverlayStatus) -> Unit,
    onFailure: (String) -> Unit,
): PGSOverlayController<Bitmap> = PGSOverlayController(
    source = RetrofitPGSOverlaySource(api),
    scope = scope,
    fileId = fileId,
    sourcePositionMs = sourcePositionMs,
    isPlaying = isPlaying,
    playbackSpeed = playbackSpeed,
    onFrame = onFrame,
    onStatus = onStatus,
    onFailure = onFailure,
    clockMs = SystemClock::elapsedRealtime,
    workDispatcher = Dispatchers.Default,
)

/**
 * Owns the Android overlay lifecycle. The server remains the only PGS parser;
 * this class accepts a bounded JSON/PNG contract, rejects stale work with
 * selection and item generations, and publishes one active composition.
 */
internal class PGSOverlayController<I : Any>(
    private val source: PGSOverlaySource<I>,
    private val scope: CoroutineScope,
    private val fileId: Long,
    private val sourcePositionMs: () -> Long,
    private val isPlaying: () -> Boolean,
    private val playbackSpeed: () -> Float,
    private val onFrame: (PGSOverlayFrameOf<I>?) -> Unit,
    private val onStatus: (PGSOverlayStatus) -> Unit,
    private val onFailure: (String) -> Unit,
    private val clockMs: () -> Long,
    private val workDispatcher: CoroutineDispatcher,
) {
    private var trackIndex: Long? = null
    private var manifest: PGSOverlayManifest? = null
    private var loadedWindow: PGSOverlayTimeWindow? = null

    /**
     * The window the in-flight load will publish, so a player event cannot
     * cancel the very load that covers its position.
     */
    private var loadingWindow: PGSOverlayTimeWindow? = null

    /** The cue last handed to [onFrame], so a seek can tell a stale frame. */
    private var shownCueId: String? = null

    /**
     * Consecutive failed window loads in this failure episode. The first says
     * so; the retries on `PGSOverlayPolicy.windowRetryDelaysMs` are silent.
     * A success, a seek, a reselection or a new item ends the episode.
     */
    private var windowFailures = 0
    private var lastFailureMs = 0L
    private var selectionGeneration = 0L
    private var itemGeneration = 0L
    private var revision = 0L
    private var windowGeneration = 0L
    private var prepareJob: Job? = null
    private var windowJob: Job? = null
    private var boundaryJob: Job? = null
    private var retryJob: Job? = null

    private data class CacheEntry<I>(val image: I, val bytes: Long)

    private val imageCache = object : LinkedHashMap<String, CacheEntry<I>>(16, 0.75f, true) {}
    private var imageCacheBytes = 0L

    val isActive: Boolean
        get() = trackIndex != null

    fun select(index: Long?) {
        if (index == trackIndex && index != null) {
            // A reselection is the viewer asking again: past any backoff.
            reconcile(seeked = true)
            return
        }
        clearState()
        if (index == null) return

        trackIndex = index
        onStatus(PGSOverlayStatus.Preparing)
        val selection = selectionGeneration
        prepareJob = scope.launch {
            val deadline = clockMs() + PGSOverlayPolicy.maximumPrepareMs
            try {
                while (clockMs() < deadline) {
                    ensureSelection(selection, index)
                    when (val fetch = source.manifest(fileId, index)) {
                        is PGSOverlayManifestFetch.Ready -> {
                            val ready = withContext(workDispatcher) {
                                fetch.manifest.validated(fileId, index)
                            }
                            ensureSelection(selection, index)
                            manifest = ready
                            reconcile(forceWindow = true)
                            return@launch
                        }
                        is PGSOverlayManifestFetch.Preparing -> delay(fetch.retryAfterMs.toLong())
                    }
                }
                fail("PGS subtitles took too long to prepare.", selection)
            } catch (_: CancellationException) {
                // Selection/item lifecycle cancellation is expected.
            } catch (error: Exception) {
                fail(error.message ?: "PGS subtitles are unavailable.", selection)
            }
        }
    }

    fun itemChanged() {
        itemGeneration++
        windowGeneration++
        windowJob?.cancel()
        boundaryJob?.cancel()
        retryJob?.cancel()
        loadedWindow = null
        loadingWindow = null
        windowFailures = 0
        show(null)
        if (trackIndex != null) reconcile(forceWindow = true)
    }

    /**
     * Every player event lands here. [seeked] is a position discontinuity: the
     * viewer's move, which ends a failure backoff. Anything else (play state,
     * speed, a cue boundary, a backoff retry) keeps it, and none of them may
     * cancel a load that already covers the position.
     */
    fun reconcile(forceWindow: Boolean = false, seeked: Boolean = false) {
        val ready = manifest ?: return
        val index = trackIndex ?: return
        val position = sourcePositionMs().coerceAtLeast(0)
        if (forceWindow || seeked) windowFailures = 0
        if (forceWindow) {
            loadWindow(
                ready,
                index,
                position,
                PGSOverlayPolicy.windowAt(position, ready.durationMs),
                clearFrame = shownCueId != null &&
                    shownCueId != PGSOverlayPolicy.activeCueIndex(ready.cues, position)?.let { ready.cues[it].id },
            )
            return
        }
        val plan = PGSOverlayPolicy.seekPlan(
            positionMs = position,
            loadedWindow = loadedWindow,
            loadingWindow = loadingWindow,
            shownCueId = shownCueId,
            cues = ready.cues,
            durationMs = ready.durationMs,
            windowFailures = windowFailures,
            msSinceFailure = clockMs() - lastFailureMs,
        )
        when (plan.action) {
            PGSOverlaySeekAction.Load ->
                loadWindow(ready, index, position, checkNotNull(plan.window), clearFrame = plan.clearNow)
            PGSOverlaySeekAction.Await -> if (plan.clearNow) show(null)
            PGSOverlaySeekAction.Publish -> publishActive(ready, position)
            PGSOverlaySeekAction.Hold -> Unit
        }
    }

    fun release() = clearState()

    private fun show(frame: PGSOverlayFrameOf<I>?) {
        shownCueId = frame?.cue?.id
        onFrame(frame)
    }

    private fun clearState() {
        selectionGeneration++
        windowGeneration++
        prepareJob?.cancel()
        windowJob?.cancel()
        boundaryJob?.cancel()
        retryJob?.cancel()
        prepareJob = null
        windowJob = null
        boundaryJob = null
        retryJob = null
        trackIndex = null
        manifest = null
        loadedWindow = null
        loadingWindow = null
        windowFailures = 0
        imageCache.clear()
        imageCacheBytes = 0
        show(null)
        onStatus(PGSOverlayStatus.Off)
    }

    private fun loadWindow(
        ready: PGSOverlayManifest,
        index: Long,
        position: Long,
        window: PGSOverlayTimeWindow,
        clearFrame: Boolean,
    ) {
        val cues = PGSOverlayPolicy.cuesInWindow(ready.cues, window)
        val selection = selectionGeneration
        val item = itemGeneration
        val windowToken = ++windowGeneration
        windowJob?.cancel()
        boundaryJob?.cancel()
        retryJob?.cancel()
        loadingWindow = window
        if (clearFrame) show(null)
        windowJob = scope.launch {
            try {
                require(cues.size <= PGSOverlayPolicy.maximumWindowCues) {
                    "The PGS subtitle window contains too many cues."
                }
                require(PGSOverlayPolicy.decodedWindowBytes(cues) != null) {
                    "The PGS subtitle window exceeded the device memory limit."
                }
                val active = PGSOverlayPolicy.activeCueIndex(cues, position)
                    ?.let(cues::get)

                suspend fun load(object_: PGSOverlayObject) {
                    ensureCurrent(selection, item, windowToken, index)
                    if (imageCache[object_.image] != null) return
                    val hash = ready.objectHash(object_.image)
                        ?: error("The server returned an invalid PGS subtitle path.")
                    val image = source.image(fileId, index, ready.generation, hash, object_)
                    ensureCurrent(selection, item, windowToken, index)
                    store(object_.image, image)
                }

                val activePaths = active?.objects.orEmpty().mapTo(mutableSetOf()) { it.image }
                active?.objects.orEmpty().distinctBy { it.image }.forEach { load(it) }

                // Publish the current composition before filling the look-ahead
                // cache. A dense dialogue window must not delay the subtitle
                // already due on screen.
                ensureCurrent(selection, item, windowToken, index)
                loadedWindow = window
                windowFailures = 0
                onStatus(PGSOverlayStatus.Ready)
                publishActive(ready, sourcePositionMs().coerceAtLeast(0))

                cues.asSequence()
                    .flatMap { it.objects.asSequence() }
                    .filterNot { it.image in activePaths }
                    .distinctBy { it.image }
                    .forEach { load(it) }

                ensureCurrent(selection, item, windowToken, index)
                loadedWindow = window
                publishActive(ready, sourcePositionMs().coerceAtLeast(0))
            } catch (_: CancellationException) {
                // A newer selection, item, seek window, or release won.
            } catch (error: Exception) {
                if (isCurrent(selection, item, windowToken, index)) {
                    failWindow(error.message ?: "PGS subtitles are unavailable.")
                }
            } finally {
                // Every exit of the current load gives its window back.
                if (windowToken == windowGeneration) loadingWindow = null
            }
        }
    }

    private fun publishActive(ready: PGSOverlayManifest, position: Long) {
        boundaryJob?.cancel()
        val cue = PGSOverlayPolicy.activeCueIndex(ready.cues, position)?.let(ready.cues::get)
        if (cue == null) {
            show(null)
        } else {
            val objects = cue.objects.map { object_ ->
                val image = imageCache[object_.image]?.image ?: run {
                    show(null)
                    reconcile(forceWindow = true)
                    return
                }
                PGSOverlayRenderedObjectOf(object_, image)
            }
            revision++
            show(PGSOverlayFrameOf(revision, cue, objects))
        }
        scheduleBoundary(ready, position)
    }

    private fun scheduleBoundary(ready: PGSOverlayManifest, position: Long) {
        if (!isPlaying()) return
        val boundary = PGSOverlayPolicy.nextBoundaryMs(ready.cues, position) ?: return
        val speed = playbackSpeed().takeIf { it > 0f } ?: 1f
        val waitMs = ceil((boundary - position).coerceAtLeast(1) / speed.toDouble())
            .toLong()
            .coerceAtLeast(1)
        val selection = selectionGeneration
        val item = itemGeneration
        val window = windowGeneration
        val index = trackIndex ?: return
        boundaryJob = scope.launch {
            delay(waitMs)
            try {
                ensureCurrent(selection, item, window, index)
                reconcile()
            } catch (_: CancellationException) {
                // A playback event rescheduled this boundary.
            }
        }
    }

    private fun store(key: String, image: I) {
        val bytes = source.imageBytes(image)
        require(bytes in 1..PGSOverlayPolicy.decodedImageBudgetBytes)
        imageCache.remove(key)?.let { imageCacheBytes -= it.bytes }
        while (imageCacheBytes > PGSOverlayPolicy.decodedImageBudgetBytes - bytes) {
            val oldest = imageCache.entries.firstOrNull() ?: break
            imageCache.remove(oldest.key)
            imageCacheBytes -= oldest.value.bytes
        }
        require(imageCacheBytes <= PGSOverlayPolicy.decodedImageBudgetBytes - bytes) {
            "The PGS subtitle cache exceeded the device memory limit."
        }
        imageCache[key] = CacheEntry(image, bytes)
        imageCacheBytes += bytes
    }

    private fun ensureCurrent(selection: Long, item: Long, window: Long, index: Long) {
        if (!isCurrent(selection, item, window, index)) {
            throw CancellationException("stale PGS overlay work")
        }
    }

    private fun isCurrent(selection: Long, item: Long, window: Long, index: Long): Boolean =
        selection == selectionGeneration &&
            item == itemGeneration &&
            window == windowGeneration &&
            index == trackIndex

    private fun ensureSelection(selection: Long, index: Long) {
        if (selection != selectionGeneration || index != trackIndex) {
            throw CancellationException("stale PGS overlay selection")
        }
    }

    /**
     * A window load failed. Said once per episode; then retried on the
     * bounded backoff without saying it again, and left alone once the
     * retries are spent until the viewer seeks or reselects.
     */
    private fun failWindow(message: String) {
        boundaryJob?.cancel()
        loadedWindow = null
        show(null)
        windowFailures++
        lastFailureMs = clockMs()
        onStatus(PGSOverlayStatus.Failed)
        if (windowFailures == 1) onFailure(PGSOverlayPolicy.failureNotice(message))
        val retryAfter = PGSOverlayPolicy.windowRetryDelayMs(windowFailures) ?: return
        val selection = selectionGeneration
        val item = itemGeneration
        retryJob = scope.launch {
            delay(retryAfter)
            if (selection == selectionGeneration && item == itemGeneration) reconcile()
        }
    }

    private fun fail(message: String, selection: Long) {
        if (selection != selectionGeneration) return
        prepareJob?.cancel()
        windowJob?.cancel()
        boundaryJob?.cancel()
        retryJob?.cancel()
        loadedWindow = null
        loadingWindow = null
        show(null)
        onStatus(PGSOverlayStatus.Failed)
        onFailure(PGSOverlayPolicy.failureNotice(message))
    }
}
