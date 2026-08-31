@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.view.KeyEvent
import android.app.PictureInPictureParams
import android.content.pm.PackageManager
import android.graphics.Rect
import android.os.Build
import android.util.Log
import android.util.Rational
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ClosedCaption
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Forward10
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Pause
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.PictureInPictureAlt
import androidx.compose.material.icons.filled.Replay10
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.clipRect
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.Player
import androidx.media3.common.VideoSize
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.ui.AspectRatioFrameLayout
import androidx.media3.ui.PlayerView
import androidx.core.app.PictureInPictureModeChangedInfo
import androidx.core.util.Consumer
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.text.SimpleDateFormat
import java.util.Locale
import kotlin.math.roundToInt
import tv.plurx.app.BuildConfig
import tv.plurx.app.data.AudioTrack
import tv.plurx.app.data.Caps
import tv.plurx.app.data.Decision
import tv.plurx.app.data.DeviceCaps
import tv.plurx.app.data.Marker
import tv.plurx.app.data.MediaFileDto
import tv.plurx.app.data.PlaybackSessionStatus
import tv.plurx.app.data.Rung
import tv.plurx.app.data.Session
import tv.plurx.app.data.SubTrack
import tv.plurx.app.ui.AppViewModel
import tv.plurx.app.ui.PlaybackTarget
import tv.plurx.app.ui.catchingUnlessCancelled
import tv.plurx.app.ui.components.LoadingBox
import tv.plurx.app.ui.components.MediaFact
import tv.plurx.app.ui.components.MediaFactChip
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.TvButton
import tv.plurx.app.ui.components.TvIconButton
import tv.plurx.app.ui.components.dynamicRangeLabel
import tv.plurx.app.ui.components.formatTime
import tv.plurx.app.ui.components.playerMediaFacts
import tv.plurx.app.ui.components.sourceDynamicRange
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.theme.Accent
import tv.plurx.app.ui.theme.Muted
import tv.plurx.app.ui.theme.Surface

private data class Plan(
    override val title: String,
    val subtitle: String?,
    val releaseDate: String?,
    val overview: String?,
    override val durationMs: Long,
    override val videoCodec: String?,
    override val fileId: Long,
    override val playUrl: String,
    override val mode: String,
    override val sourceHeight: Int?,
    override val aac: Boolean,
    override val preserveDolbyVision: Boolean,
    override val deliveredDynamicRange: String?,
    /** Both protocol spellings from the route probe that produced the plan. */
    val legacyCaps: Map<String, String>,
    val decisionCaps: DeviceCaps,
    /**
     * `delivery.audio` — the audio index this plan already carries. Executed as
     * given rather than re-derived: it is what the server actually applied to
     * the remux URL, and what the HLS session body has to repeat.
     */
    val deliveryAudio: Long?,
    val markers: List<Marker>,
    val reasons: List<String>,
    val videoWidth: Int?,
    val videoHeight: Int?,
    val source: MediaFileDto?,
    override val audio: List<AudioTrack>,
    override val subtitles: List<SubTrack>,
    val ladder: List<Rung>,
    val declaredOffsetMs: Long?,
    val progressOffsetMs: Long,
    val itemDurationMs: Long?,
    val nextAudiobookPartId: Long?,
) : PlanLike {
    fun globalPosition(localPositionMs: Long): Long =
        audiobookGlobalPosition(localPositionMs, progressOffsetMs)
    val progressDurationMs: Long get() = itemDurationMs ?: durationMs
}

internal class PlanLoadException(
    val stage: String,
    cause: Throwable,
) : Exception(cause)

private suspend fun <T> planLoadStage(stage: String, block: suspend () -> T): T = try {
    block()
} catch (cancelled: CancellationException) {
    throw cancelled
} catch (error: Exception) {
    throw PlanLoadException(stage, error)
}

private suspend fun loadPlan(
    vm: AppViewModel,
    itemId: Long,
    fileId: Long,
    tracks: PreplayTracks,
): Plan {
    val detail = planLoadStage("item_detail") { vm.itemDetail(itemId) }
    // The pre-play choice reaches the *first* decision, so the plan that comes
    // back already carries it. Starting on the policy default and switching
    // afterwards is what criterion 4 forbids: it is a visible re-buffer to
    // apply something the viewer chose before playback began.
    val playbackDecision = planLoadStage("decision") { vm.playbackDecision(fileId, tracks) }
    val decision: Decision = playbackDecision.decision
    val file = detail.files.firstOrNull { it.id == fileId } ?: detail.files.firstOrNull()
    val mode = decision.delivery?.mode ?: when (decision.method) {
        "direct_play" -> "direct"
        "remux" -> "remux"
        else -> "transcode"
    }
    return planLoadStage("plan") {
        Plan(
            title = detail.item.title,
            subtitle = playerSubtitle(detail.item),
            releaseDate = playerDateLabel(detail.item.air_date, detail.item.year),
            overview = detail.item.overview,
            durationMs = file?.duration_ms ?: detail.item.runtime_ms ?: 0L,
            videoCodec = decision.source?.video_codec ?: file?.video_codec,
            fileId = fileId,
            playUrl = Session.url(decision.delivery?.url ?: decision.play_url),
            mode = mode,
            // The decision's own reading of the source: the number every height
            // promise is made of. The item's file row is the fallback for a
            // server too old to send `source`.
            sourceHeight = (decision.source?.height ?: file?.height)?.toInt(),
            aac = decision.delivery?.aac ?: decision.transcode_audio,
            // Direct delivery has no remux-specific field, but the flattened
            // decision still says whether these exact source bytes are DV. Keep
            // that fact so a decoder failure can try a DV-preserving MP4 remux
            // before the final SDR compatibility transcode.
            preserveDolbyVision = decision.delivery?.preserve_dolby_vision
                ?: decision.preserve_dolby_vision,
            deliveredDynamicRange = decision.delivered_dynamic_range,
            legacyCaps = playbackDecision.capabilities.legacyQuery,
            decisionCaps = playbackDecision.capabilities.document,
            deliveryAudio = decision.delivery?.audio,
            markers = decision.markers,
            reasons = decision.reasons,
            videoWidth = file?.width?.toInt(),
            videoHeight = file?.height?.toInt(),
            source = file,
            audio = decision.audio,
            subtitles = decision.subtitles,
            ladder = decision.ladder,
            declaredOffsetMs = decision.declared_offset_ms,
            progressOffsetMs = if (detail.item.isAudiobook) file?.part_offset_ms ?: 0L else 0L,
            itemDurationMs = if (detail.item.isAudiobook) detail.item.runtime_ms else null,
            nextAudiobookPartId = if (detail.item.isAudiobook) {
                nextAudiobookPartId(detail.files, fileId)
            } else null,
        )
    }
}

internal fun audiobookGlobalPosition(localPositionMs: Long, partOffsetMs: Long): Long =
    partOffsetMs.coerceAtLeast(0L) + localPositionMs.coerceAtLeast(0L)

internal fun nextAudiobookPartId(files: List<MediaFileDto>, currentFileId: Long): Long? {
    val index = files.indexOfFirst { it.id == currentFileId }
    return if (index >= 0) files.drop(index + 1).firstOrNull { it.available }?.id else null
}

internal fun playerSubtitle(item: tv.plurx.app.data.Item): String? = buildList {
    item.show_title?.takeIf { it.isNotBlank() }?.let(::add)
    if (item.season_number != null && item.episode_number != null) {
        add("S${item.season_number}E${item.episode_number}")
    }
}.takeIf { it.isNotEmpty() }?.joinToString("   ·   ")

internal fun playerDateLabel(airDate: String?, year: Int?): String? {
    val raw = airDate?.trim().orEmpty()
    if (raw.isNotEmpty()) {
        val parsed = runCatching {
            SimpleDateFormat("yyyy-MM-dd", Locale.US).apply { isLenient = false }
                .parse(raw.take(10))
        }.getOrNull()
        if (parsed != null) {
            return SimpleDateFormat("MMM d, yyyy", Locale.US).format(parsed)
        }
        return raw
    }
    return year?.toString()
}

internal fun playerRuntimeLabel(milliseconds: Long): String {
    val totalMinutes = milliseconds / 60_000
    val hours = totalMinutes / 60
    val minutes = totalMinutes % 60
    return if (hours > 0) "${hours}h ${minutes}m" else "${minutes}m"
}

private enum class PlayerPanel { Tracks, Settings, Info }

internal enum class PlaybackStatsMode(val label: String) {
    Mini("Mini"),
    Standard("Standard"),
    Debug("Debug"),
}

private enum class PlaybackStatTone(val color: Color) {
    Neutral(Color(0xFFEEEEF2)),
    Muted(Color(0xFF9697A2)),
    Good(Color(0xFF6DDB98)),
    Warning(Color(0xFFFFBD4A)),
    Critical(Color(0xFFFF6268)),
}

internal enum class PlayerBackAction { ClosePanel, HideControls, ExitPlayback }

internal fun playerBackAction(panelOpen: Boolean, controlsVisible: Boolean): PlayerBackAction = when {
    panelOpen -> PlayerBackAction.ClosePanel
    controlsVisible -> PlayerBackAction.HideControls
    else -> PlayerBackAction.ExitPlayback
}

/**
 * Directional playback shortcuts only own the bare video surface. Visible
 * controls keep normal D-pad focus navigation.
 */
internal fun playerSeekDeltaMs(keyCode: Int, controlsVisible: Boolean): Long? {
    if (controlsVisible) return null
    return when (keyCode) {
        KeyEvent.KEYCODE_DPAD_LEFT -> -10_000L
        KeyEvent.KEYCODE_DPAD_RIGHT -> 10_000L
        KeyEvent.KEYCODE_DPAD_DOWN -> -30_000L
        KeyEvent.KEYCODE_DPAD_UP -> 30_000L
        else -> null
    }
}

/** Accumulates bare-surface D-pad repeats against one frozen player position. */
internal class HiddenSeekAccumulator(private val durationMs: Long) {
    var pendingTargetMs: Long? = null
        private set

    fun nudge(currentPositionMs: Long, deltaMs: Long): Long {
        val base = pendingTargetMs ?: currentPositionMs.coerceAtLeast(0L)
        val ceiling = durationMs.takeIf { it > 0L } ?: Long.MAX_VALUE
        return (base + deltaMs).coerceIn(0L, ceiling).also { pendingTargetMs = it }
    }

    fun consume(): Long? = pendingTargetMs.also { pendingTargetMs = null }
}

private const val MAX_PIP_ASPECT_RATIO = 2.39

/** Returns an Android-supported PiP aspect ratio, using 16:9 when media metadata is unusable. */
internal fun calculatePipAspectRatio(
    width: Int,
    height: Int,
    pixelWidthHeightRatio: Float = 1f,
): Rational {
    if (width <= 0 || height <= 0 || !pixelWidthHeightRatio.isFinite() || pixelWidthHeightRatio <= 0f) {
        return Rational(16, 9)
    }

    val adjustedWidth = (width.toDouble() * pixelWidthHeightRatio).roundToInt()
    if (adjustedWidth <= 0) return Rational(16, 9)

    val aspectRatio = adjustedWidth.toDouble() / height
    val minimumAspectRatio = 1.0 / MAX_PIP_ASPECT_RATIO
    return if (aspectRatio in minimumAspectRatio..MAX_PIP_ASPECT_RATIO) {
        Rational(adjustedWidth, height)
    } else {
        Rational(16, 9)
    }
}

private fun pictureInPictureParams(
    aspectRatio: Rational,
    sourceRect: Rect?,
    autoEnter: Boolean,
): PictureInPictureParams? {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return null

    val builder = PictureInPictureParams.Builder().setAspectRatio(aspectRatio)
    if (sourceRect != null && !sourceRect.isEmpty) builder.setSourceRectHint(sourceRect)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        builder
            .setAutoEnterEnabled(autoEnter)
            .setSeamlessResizeEnabled(true)
    }
    return builder.build()
}

private fun setPictureInPictureParams(
    activity: android.app.Activity,
    params: PictureInPictureParams?,
) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && params != null) {
        activity.setPictureInPictureParams(params)
    }
}

private fun enterPictureInPicture(
    activity: android.app.Activity,
    params: PictureInPictureParams?,
) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && params != null) {
        activity.enterPictureInPictureMode(params)
    }
}

private fun isInPictureInPicture(activity: android.app.Activity): Boolean =
    Build.VERSION.SDK_INT >= Build.VERSION_CODES.N && activity.isInPictureInPictureMode

@Composable
fun PlayerScreen(
    vm: AppViewModel,
    itemId: Long,
    fileId: Long,
    startMs: Long,
    /**
     * What the viewer chose on the detail screen, before pressing play. It is a
     * property of this one playback: nothing here writes a server setting, and
     * the next item starts from its own route with no memory of it.
     */
    preplayTracks: PreplayTracks = PreplayTracks.NONE,
    onPlayNext: (PlaybackTarget) -> Unit,
    onExit: () -> Unit,
) {
    var plan by remember(itemId, fileId) { mutableStateOf<Plan?>(null) }
    var failed by remember(itemId, fileId) { mutableStateOf(false) }
    var generation by remember(itemId, fileId) { mutableIntStateOf(0) }
    var resumeAt by remember(itemId, fileId) { mutableLongStateOf(startMs) }
    var startReason by remember(itemId, fileId) {
        mutableStateOf(if (startMs > 0) "resume" else "cold-start")
    }
    var attemptOpenedAtMs by remember(itemId, fileId) { mutableLongStateOf(monotonicNowMs()) }
    // Survives the plan, like the A/V correction beside it: a quality change
    // reloads the plan and rebuilds the controller, and the viewer's audio and
    // subtitle picks must come back with them.
    var playbackAudioOffset by remember(itemId, fileId) { mutableLongStateOf(0) }
    // Seeded from the pre-play choice, so a quality change mid-playback keeps
    // the tracks the viewer picked on the detail screen rather than falling
    // back to the server's policy default.
    var playbackAudio by remember(itemId, fileId) { mutableStateOf(preplayTracks.audio) }
    var playbackSubtitle by remember(itemId, fileId) {
        mutableStateOf(preplayTracks.subtitle)
    }

    ImmersivePlaybackEffect()

    LaunchedEffect(itemId, fileId, generation) {
        failed = false
        // Anchor TTFF before the detail and decision requests. This is the
        // Android equivalent of the web click timestamp, not merely decoder
        // preparation latency.
        attemptOpenedAtMs = monotonicNowMs()
        try {
            plan = loadPlan(
                vm,
                itemId,
                fileId,
                PreplayTracks(audio = playbackAudio, subtitle = playbackSubtitle),
            )
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            val failure = error as? PlanLoadException
            val cause = failure?.cause ?: error
            val stage = failure?.stage ?: "unknown"
            postPlaybackClientLog(
                this,
                planLoadFailureEvent(fileId, startReason, stage, cause),
            )
            Log.w(
                "plurx-playback",
                "file=$fileId playback plan failed stage=$stage type=${cause.javaClass.simpleName}",
                cause,
            )
            failed = true
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        when {
            failed -> PlaybackFailed(
                message = "Couldn't start playback.",
                onRetry = {
                    startReason = "fallback"
                    generation++
                },
                onExit = onExit,
            )
            plan == null -> {
                LoadingBox()
                BackChip(onExit)
            }
            else -> PlayerContent(
                vm = vm,
                itemId = itemId,
                plan = plan!!,
                startMs = resumeAt,
                startReason = startReason,
                attemptOpenedAtMs = attemptOpenedAtMs,
                audioOffsetMs = playbackAudioOffset,
                onAudioOffsetChanged = { playbackAudioOffset = it },
                // The plan's own answer wins over the request that produced it:
                // `delivery.audio` is the index the server actually applied,
                // and the HLS session body has to repeat exactly that.
                retainedAudio = plan!!.deliveryAudio ?: playbackAudio,
                onAudioChanged = { playbackAudio = it },
                retainedSubtitle = playbackSubtitle,
                onSubtitleChanged = { playbackSubtitle = SubtitleChoice(it) },
                onReload = { position, reason ->
                    resumeAt = position
                    startReason = reason
                    plan = null
                    generation++
                },
                onPlayNext = onPlayNext,
                onExit = onExit,
            )
        }
    }
}

@Composable
private fun ImmersivePlaybackEffect() {
    val activity = androidx.activity.compose.LocalActivity.current ?: return
    val lifecycleOwner = LocalLifecycleOwner.current

    DisposableEffect(activity, lifecycleOwner) {
        val window = activity.window
        val decorView = window.decorView
        val insetsController = WindowCompat.getInsetsController(window, decorView)
        var active = true

        val enterImmersiveMode = Runnable {
            if (!active) return@Runnable
            WindowCompat.setDecorFitsSystemWindows(window, false)
            insetsController.systemBarsBehavior =
                WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            insetsController.hide(WindowInsetsCompat.Type.systemBars())
        }
        val focusListener = android.view.ViewTreeObserver.OnWindowFocusChangeListener { hasFocus ->
            if (hasFocus) decorView.post(enterImmersiveMode)
        }
        val lifecycleObserver = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) decorView.post(enterImmersiveMode)
        }

        decorView.viewTreeObserver.addOnWindowFocusChangeListener(focusListener)
        lifecycleOwner.lifecycle.addObserver(lifecycleObserver)
        // Waiting for the decor view's next frame prevents the initial request from
        // being lost while Compose and PlayerView are still taking window focus.
        decorView.post(enterImmersiveMode)

        onDispose {
            active = false
            decorView.removeCallbacks(enterImmersiveMode)
            if (decorView.viewTreeObserver.isAlive) {
                decorView.viewTreeObserver.removeOnWindowFocusChangeListener(focusListener)
            }
            lifecycleOwner.lifecycle.removeObserver(lifecycleObserver)
            insetsController.show(WindowInsetsCompat.Type.systemBars())
            WindowCompat.setDecorFitsSystemWindows(window, true)
        }
    }
}

/**
 * A real failure state. The rescue in [Controller] has already been spent by
 * the time this shows, so the viewer gets the reason and a way out — never a
 * frozen black surface with a stream that will not come back.
 */
@Composable
private fun PlaybackFailed(
    message: String,
    onRetry: (() -> Unit)?,
    onExit: () -> Unit,
) {
    val retryFocusRequester = remember { FocusRequester() }
    RequestInitialFocus(retryFocusRequester, enabled = onRetry != null)
    Column(
        Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(message, color = Color.White)
        Spacer(Modifier.size(12.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            if (onRetry != null) {
                TvButton(
                    onClick = onRetry,
                    modifier = Modifier.focusRequester(retryFocusRequester),
                ) { Text("Retry") }
            }
            TvButton(onClick = onExit) { Text("Back") }
        }
    }
}

@Composable
private fun PlayerContent(
    vm: AppViewModel,
    itemId: Long,
    plan: Plan,
    startMs: Long,
    startReason: String,
    attemptOpenedAtMs: Long,
    audioOffsetMs: Long,
    onAudioOffsetChanged: (Long) -> Unit,
    retainedAudio: Long?,
    onAudioChanged: (Long) -> Unit,
    retainedSubtitle: SubtitleChoice?,
    onSubtitleChanged: (Long?) -> Unit,
    onReload: (Long, String) -> Unit,
    onPlayNext: (PlaybackTarget) -> Unit,
    onExit: () -> Unit,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val activity = androidx.activity.compose.LocalActivity.current
    val componentActivity = activity as? ComponentActivity
    val canUsePip = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
        context.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)
    val scope = rememberCoroutineScope()
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    var playFailure by remember { mutableStateOf<String?>(null) }
    val controller = remember(plan) {
        Controller(
            context,
            buildPlayer(context, vm),
            plan,
            plan.legacyCaps,
            plan.decisionCaps,
            vm,
            scope,
            initialAudioOffsetMs = audioOffsetMs,
            retainedAudio = retainedAudio,
            retainedSubtitle = retainedSubtitle,
            onError = { playFailure = it },
        )
    }
    val surfaceFocusRequester = remember { FocusRequester() }
    // Which grades this panel can show. Probed once per playback: it is a
    // property of the cable, and this screen owns one HDMI route for its life.
    val displayHdrTypes = remember(context) { Caps.displayHdrTypes(context) }

    var positionMs by remember { mutableLongStateOf(startMs) }
    val hiddenSeekAccumulator = remember(controller, plan.durationMs) {
        HiddenSeekAccumulator(plan.durationMs)
    }
    var hiddenSeekTargetMs by remember(controller) { mutableStateOf<Long?>(null) }
    var seekHudPositionMs by remember(controller) { mutableStateOf<Long?>(null) }
    var scrubbing by remember { mutableStateOf(false) }
    var scrubPreview by remember { mutableLongStateOf(startMs) }
    var isPlaying by remember { mutableStateOf(true) }
    var buffering by remember { mutableStateOf(true) }
    var controlsVisible by remember { mutableStateOf(true) }
    // Height of the bottom control block as it was last laid out. The info
    // panel has to clear that block, but the block is a title, chips, a context
    // line, an overview and a transport row — its height is content, not a
    // constant — and it is only on screen while `controlsVisible`. Measuring it
    // is the only way to reserve the right amount, and zero the rest of the time.
    var transportHeightPx by remember { mutableIntStateOf(0) }
    var panel by remember { mutableStateOf<PlayerPanel?>(null) }
    var statsMode by remember { mutableStateOf(PlaybackStatsMode.Standard) }
    var playerView by remember { mutableStateOf<PlayerView?>(null) }
    var isInPip by remember(activity) {
        mutableStateOf(
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
                activity?.isInPictureInPictureMode == true,
        )
    }
    var pipAspectRatio by remember(plan) {
        mutableStateOf(calculatePipAspectRatio(plan.videoWidth ?: 0, plan.videoHeight ?: 0))
    }
    var videoAspectRatio by remember(plan) {
        mutableStateOf(
            if ((plan.videoWidth ?: 0) > 0 && (plan.videoHeight ?: 0) > 0) {
                (plan.videoWidth ?: 1).toFloat() / (plan.videoHeight ?: 1).toFloat()
            } else {
                16f / 9f
            },
        )
    }
    var lastInteraction by remember { mutableLongStateOf(0L) }
    var lastAutoSkipped by remember(plan) { mutableLongStateOf(-1L) }
    var lastMarkerSkipEndMs by remember(plan) { mutableLongStateOf(-1L) }
    val markerOfferLedger = remember(controller, plan) { MarkerOfferLedger() }
    val markerOfferGeneration = remember(controller, plan) {
        java.util.UUID.randomUUID().toString()
    }
    var findingNext by remember { mutableStateOf(false) }

    fun poke() {
        controlsVisible = true
        lastInteraction += 1
    }

    fun recordMarkerEvent(event: String, detail: String, message: String) {
        postPlaybackClientLog(
            scope,
            PlaybackClientLog(
                level = "info",
                event = event,
                message = message,
                method = plan.mode,
                fileId = plan.fileId,
                detail = detail,
                ua = "Android Media3",
            ),
        )
    }

    fun seekWithMarkerUndo(targetMs: Long) {
        if (lastMarkerSkipEndMs > 0 &&
            targetMs < lastMarkerSkipEndMs - 1_000 &&
            controller.realPosition() >= lastMarkerSkipEndMs - 1_000
        ) {
            recordMarkerEvent(
                "marker_seek_back",
                "undo",
                "viewer sought behind the last marker destination",
            )
            lastMarkerSkipEndMs = -1
        }
        controller.seekTo(targetMs)
    }

    fun nudgeHiddenSeek(deltaMs: Long) {
        hiddenSeekAccumulator.nudge(controller.realPosition(), deltaMs).let { target ->
            hiddenSeekTargetMs = target
            seekHudPositionMs = target
        }
    }

    BackHandler(enabled = !isInPip) {
        when (playerBackAction(panel != null, controlsVisible)) {
            PlayerBackAction.ClosePanel -> {
                panel = null
                poke()
            }
            PlayerBackAction.HideControls -> controlsVisible = false
            PlayerBackAction.ExitPlayback -> onExit()
        }
    }

    // Keyed on the controller alone. Keying it on the preference too meant
    // toggling "Autoplay next episode" mid-play disposed the effect,
    // released the live player, and re-registered on the corpse — restarting
    // the episode at its original position. The listener reads the current
    // value instead of being rebuilt for it.
    val autoplayNext by rememberUpdatedState(preferences.autoplayNext)
    val playNext by rememberUpdatedState(onPlayNext)
    DisposableEffect(controller) {
        val listener = object : Player.Listener {
            override fun onIsPlayingChanged(playing: Boolean) {
                isPlaying = playing
                if (!playing) vm.postProgress(itemId, plan.globalPosition(controller.realPosition()), plan.progressDurationMs)
            }

            override fun onPlaybackStateChanged(state: Int) {
                buffering = state == Player.STATE_BUFFERING
                if (state == Player.STATE_ENDED) {
                    vm.postProgress(itemId, plan.globalPosition(plan.durationMs), plan.progressDurationMs)
                    controlsVisible = true
                    if (plan.nextAudiobookPartId != null) {
                        playNext(PlaybackTarget(itemId, plan.nextAudiobookPartId, 0))
                    } else if (autoplayNext && !findingNext) {
                        findingNext = true
                        scope.launch {
                            val next = catchingUnlessCancelled { vm.nextEpisode(itemId) }.getOrNull()
                            findingNext = false
                            if (next != null) playNext(next)
                        }
                    }
                }
            }

            override fun onVideoSizeChanged(videoSize: VideoSize) {
                pipAspectRatio = calculatePipAspectRatio(
                    videoSize.width,
                    videoSize.height,
                    videoSize.pixelWidthHeightRatio,
                )
                if (videoSize.width > 0 && videoSize.height > 0) {
                    videoAspectRatio =
                        videoSize.width * videoSize.pixelWidthHeightRatio / videoSize.height
                }
            }
        }
        controller.player.addListener(listener)
        controller.startAt(startMs, startReason, attemptOpenedAtMs)
        onDispose {
            vm.postProgress(itemId, plan.globalPosition(controller.realPosition()), plan.progressDurationMs)
            controller.player.removeListener(listener)
            controller.release()
        }
    }

    fun currentPipParams(autoEnter: Boolean = isPlaying): PictureInPictureParams? {
        val sourceRect = playerView?.let { view ->
            Rect().takeIf { view.getGlobalVisibleRect(it) && !it.isEmpty }
        }
        return pictureInPictureParams(pipAspectRatio, sourceRect, autoEnter)
    }

    val canEnterPip = canUsePip && !controller.pgsOverlayIsActive

    DisposableEffect(componentActivity, canEnterPip) {
        if (!canEnterPip || componentActivity == null) {
            onDispose { }
        } else {
            val pipModeListener = Consumer<PictureInPictureModeChangedInfo> { info ->
                isInPip = info.isInPictureInPictureMode
                panel = null
                if (info.isInPictureInPictureMode) {
                    controlsVisible = false
                } else {
                    controlsVisible = true
                    lastInteraction += 1
                }
            }
            componentActivity.addOnPictureInPictureModeChangedListener(pipModeListener)
            onDispose {
                componentActivity.removeOnPictureInPictureModeChangedListener(pipModeListener)
            }
        }
    }

    // Android 12+ reads auto-enter from the current PiP parameters. Android 8–11
    // instead needs an explicit request when the user leaves the activity.
    DisposableEffect(componentActivity, canEnterPip, isPlaying, pipAspectRatio, playerView) {
        if (
            !canEnterPip || componentActivity == null ||
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.S
        ) {
            onDispose { }
        } else {
            val leaveHintListener = Runnable {
                if (isPlaying && !isInPictureInPicture(componentActivity)) {
                    controlsVisible = false
                    panel = null
                    enterPictureInPicture(componentActivity, currentPipParams(autoEnter = false))
                }
            }
            componentActivity.addOnUserLeaveHintListener(leaveHintListener)
            onDispose { componentActivity.removeOnUserLeaveHintListener(leaveHintListener) }
        }
    }

    DisposableEffect(activity, canEnterPip, isPlaying, pipAspectRatio, playerView) {
        if (!canEnterPip || activity == null) {
            onDispose { }
        } else {
            val updateParams = Runnable {
                setPictureInPictureParams(activity, currentPipParams())
            }
            val layoutListener = android.view.View.OnLayoutChangeListener { _, _, _, _, _, _, _, _, _ ->
                updateParams.run()
            }
            playerView?.addOnLayoutChangeListener(layoutListener)
            playerView?.post(updateParams) ?: updateParams.run()
            onDispose {
                playerView?.removeOnLayoutChangeListener(layoutListener)
                playerView?.removeCallbacks(updateParams)
            }
        }
    }

    // Do not leave automatic PiP armed after navigating away from the player.
    DisposableEffect(activity, canEnterPip) {
        onDispose {
            if (canUsePip && activity != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                setPictureInPictureParams(
                    activity,
                    pictureInPictureParams(pipAspectRatio, null, autoEnter = false),
                )
            }
        }
    }

    LaunchedEffect(controller) {
        while (true) {
            if (!scrubbing) positionMs = controller.realPosition()
            delay(500)
        }
    }
    LaunchedEffect(hiddenSeekTargetMs) {
        val target = hiddenSeekTargetMs ?: return@LaunchedEffect
        delay(300)
        if (hiddenSeekTargetMs == target) {
            hiddenSeekAccumulator.consume()?.let(controller::seekTo)
            hiddenSeekTargetMs = null
        }
    }
    LaunchedEffect(seekHudPositionMs) {
        val target = seekHudPositionMs ?: return@LaunchedEffect
        delay(1_600)
        if (seekHudPositionMs == target) seekHudPositionMs = null
    }
    LaunchedEffect(controller.playbackNotice) {
        val notice = controller.playbackNotice ?: return@LaunchedEffect
        delay(5_000)
        if (controller.playbackNotice == notice) controller.clearPlaybackNotice()
    }
    LaunchedEffect(controller) {
        while (true) {
            delay(10_000)
            if (isPlaying) vm.reportProgress(itemId, plan.globalPosition(controller.realPosition()), plan.progressDurationMs)
        }
    }
    LaunchedEffect(lastInteraction, isPlaying, panel) {
        if (isPlaying && panel == null) {
            delay(3_800)
            controlsVisible = false
        }
    }

    val activeMarker = plan.markers.firstOrNull { positionMs in it.start_ms until it.end_ms }
    LaunchedEffect(
        activeMarker?.kind,
        activeMarker?.start_ms,
        activeMarker?.isAutoSkipEligible,
        isInPip,
        scrubbing,
        preferences.autoSkip,
    ) {
        val marker = activeMarker ?: return@LaunchedEffect
        val automatic = preferences.autoSkip && marker.isAutoSkipEligible
        if (!isInPip && !scrubbing && !automatic &&
            markerOfferLedger.shouldReport(markerOfferGeneration, marker.kind, marker.start_ms)
        ) {
            recordMarkerEvent("marker_offer", marker.kind, "playback marker offered")
        }
    }
    LaunchedEffect(activeMarker?.start_ms, activeMarker?.isAutoSkipEligible, preferences.autoSkip) {
        val marker = activeMarker
        if (preferences.autoSkip && marker?.isAutoSkipEligible == true && marker.start_ms != lastAutoSkipped) {
            lastAutoSkipped = marker.start_ms
            recordMarkerEvent("marker_automatic_skip", marker.kind, "playback marker skipped")
            recordMarkerEvent("marker_prewarm", "miss", "skip destination was not prewarmed")
            lastMarkerSkipEndMs = marker.end_ms
            seekWithMarkerUndo(marker.end_ms)
        }
    }

    LaunchedEffect(controlsVisible, panel, isInPip) {
        if (!isInPip && !controlsVisible && panel == null) {
            surfaceFocusRequester.requestFocus()
        }
    }

    playFailure?.let { message ->
        PlaybackFailed(
            message = message,
            onRetry = { onReload(controller.realPosition(), "fallback") },
            onExit = onExit,
        )
        return
    }

    Box(
        Modifier.fillMaxSize()
            .focusRequester(surfaceFocusRequester)
            .focusable()
            .onPreviewKeyEvent { event ->
                if (event.nativeKeyEvent.action != KeyEvent.ACTION_DOWN) return@onPreviewKeyEvent false
                val seekDelta = playerSeekDeltaMs(
                    keyCode = event.nativeKeyEvent.keyCode,
                    controlsVisible = controlsVisible,
                )
                if (seekDelta != null) {
                    nudgeHiddenSeek(seekDelta)
                    return@onPreviewKeyEvent true
                }
                when (event.nativeKeyEvent.keyCode) {
                    KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE -> {
                        controller.playPause()
                        true
                    }
                    KeyEvent.KEYCODE_DPAD_CENTER, KeyEvent.KEYCODE_ENTER -> {
                        if (!controlsVisible) {
                            poke()
                            true
                        } else {
                            poke()
                            false
                        }
                    }
                    KeyEvent.KEYCODE_MEDIA_REWIND -> {
                        seekWithMarkerUndo(controller.realPosition() - 10_000); poke(); true
                    }
                    KeyEvent.KEYCODE_MEDIA_FAST_FORWARD -> {
                        seekWithMarkerUndo(controller.realPosition() + 10_000); poke(); true
                    }
                    KeyEvent.KEYCODE_DPAD_LEFT,
                    KeyEvent.KEYCODE_DPAD_RIGHT,
                    KeyEvent.KEYCODE_DPAD_UP,
                    KeyEvent.KEYCODE_DPAD_DOWN,
                    -> {
                        poke()
                        false
                    }
                    else -> false
                }
            },
    ) {
        AndroidView(
            factory = {
                PlayerView(it).apply {
                    player = controller.player
                    useController = false
                    resizeMode = AspectRatioFrameLayout.RESIZE_MODE_FIT
                    setShutterBackgroundColor(android.graphics.Color.BLACK)
                    keepScreenOn = true
                    playerView = this
                }
            },
            update = { playerView = it },
            modifier = Modifier.fillMaxSize(),
        )

        if (!isInPip) {
            PGSBitmapOverlay(
                frame = controller.pgsOverlayFrame,
                videoAspectRatio = videoAspectRatio,
            )
        }

        if (!isInPip) {
            Box(
                Modifier.fillMaxSize().focusProperties { canFocus = false }.clickable(
                    interactionSource = remember { MutableInteractionSource() },
                    indication = null,
                ) { if (controlsVisible) controlsVisible = false else poke() },
            )
        }

        if (!isInPip && (buffering || findingNext)) {
            Column(Modifier.align(Alignment.Center), horizontalAlignment = Alignment.CenterHorizontally) {
                CircularProgressIndicator(color = Accent)
                if (findingNext) Text("Up next…", color = Color.White, modifier = Modifier.padding(top = 12.dp))
            }
        }

        if (!isInPip) {
            seekHudPositionMs?.let { target ->
                Text(
                    text = "Seek ${formatTime(target)}",
                    color = Color.White,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier
                        .align(Alignment.Center)
                        .background(Color.Black.copy(alpha = 0.78f), MaterialTheme.shapes.medium)
                        .padding(horizontal = 18.dp, vertical = 10.dp),
                )
            }
        }

        if (!isInPip && activeMarker != null && !scrubbing &&
            !(preferences.autoSkip && activeMarker.isAutoSkipEligible)
        ) {
            TvButton(
                onClick = {
                    recordMarkerEvent("marker_manual_skip", activeMarker.kind, "playback marker skipped")
                    recordMarkerEvent("marker_prewarm", "miss", "skip destination was not prewarmed")
                    lastMarkerSkipEndMs = activeMarker.end_ms
                    seekWithMarkerUndo(activeMarker.end_ms)
                    poke()
                },
                modifier = Modifier.align(Alignment.BottomEnd).padding(end = 28.dp, bottom = 112.dp),
            ) { Text(activeMarker.displayLabel, fontWeight = FontWeight.SemiBold) }
        }

        if (!isInPip && controlsVisible) {
            Controls(
                title = plan.title,
                subtitle = plan.subtitle,
                releaseDate = plan.releaseDate,
                runtimeLabel = plan.durationMs.takeIf { it > 0 }?.let(::playerRuntimeLabel),
                overview = plan.overview,
                positionMs = if (scrubbing) scrubPreview else positionMs,
                durationMs = plan.durationMs,
                isPlaying = isPlaying,
                requestInitialFocus = panel == null,
                mediaFacts = playerMediaFacts(
                    file = plan.source,
                    audio = plan.audio.firstOrNull { it.index == controller.selectedAudio }
                        ?: plan.audio.firstOrNull { it.default },
                    delivered = controller.deliveredRange,
                    rendered = renderedRange(
                        delivered = controller.deliveredRange,
                        decoderMime = controller.player.videoFormat?.sampleMimeType,
                        decoderColorTransfer = controller.player.videoFormat?.colorInfo?.colorTransfer,
                        hdrTypes = displayHdrTypes,
                    ),
                ),
                onTransportHeight = { transportHeightPx = it },
                onBack = onExit,
                onPlayPause = { controller.playPause(); poke() },
                onSeekBack = { seekWithMarkerUndo(controller.realPosition() - 10_000); poke() },
                onSeekForward = { seekWithMarkerUndo(controller.realPosition() + 10_000); poke() },
                onScrubStart = { scrubbing = true; scrubPreview = positionMs },
                onScrub = { scrubPreview = it },
                onScrubEnd = { seekWithMarkerUndo(scrubPreview); scrubbing = false; poke() },
                onTracks = { panel = PlayerPanel.Tracks },
                onSettings = { panel = PlayerPanel.Settings },
                onInfo = {
                    controlsVisible = false
                    panel = PlayerPanel.Info
                },
                onPip = if (canUsePip && activity != null) {
                    {
                        if (controller.allowsPictureInPictureCommand()) {
                            controlsVisible = false
                            panel = null
                            val params = currentPipParams(autoEnter = false)
                            setPictureInPictureParams(activity, params)
                            enterPictureInPicture(activity, params)
                        }
                    }
                } else null,
            )
        }

        when (if (isInPip) null else panel) {
            PlayerPanel.Tracks -> TrackMenu(
                player = controller.player,
                serverAudio = plan.audio,
                serverSubtitles = plan.subtitles,
                serverControlledAudio = controller.deliveryMode != "direct",
                selectedServerAudio = controller.selectedAudio,
                selectedServerSubtitle = controller.selectedSubtitle,
                onServerAudio = { onAudioChanged(it); controller.switchAudio(it); panel = null; poke() },
                onServerSubtitle = {
                    if (controller.switchSubtitle(it)) onSubtitleChanged(it)
                    panel = null
                    poke()
                },
                onDismiss = { panel = null; poke() },
            )
            PlayerPanel.Settings -> PlayerSettings(
                vm = vm,
                qualityOptions = qualityOptions(plan.ladder),
                audioOffsetMs = controller.audioOffsetMs,
                declaredOffsetMs = plan.declaredOffsetMs,
                currentPosition = controller::realPosition,
                onReload = onReload,
                onAudioOffset = {
                    controller.setAudioOffset(it)
                    onAudioOffsetChanged(it)
                },
                onDismiss = { panel = null; poke() },
            )
            PlayerPanel.Info -> PlayerInfo(
                plan = plan,
                controller = controller,
                positionMs = positionMs,
                displayHdrTypes = displayHdrTypes,
                transportReserve = playbackTransportReserve(controlsVisible, transportHeightPx),
                mode = statsMode,
                onMode = { statsMode = it },
                onDismiss = { panel = null; poke() },
            )
            null -> Unit
        }

        controller.playbackNotice?.let { notice ->
            Text(
                text = notice,
                color = Color.White,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier
                    .align(Alignment.TopCenter)
                    .padding(top = 24.dp, start = 40.dp, end = 40.dp)
                    .background(Color.Black.copy(alpha = 0.82f), MaterialTheme.shapes.medium)
                    .padding(horizontal = 14.dp, vertical = 10.dp),
            )
        }
    }
}

/** Authored bitmap composition, above video and below every player control. */
@Composable
internal fun PGSBitmapOverlay(
    frame: PGSOverlayFrame?,
    videoAspectRatio: Float,
    modifier: Modifier = Modifier,
) {
    val composition = frame ?: return
    Canvas(
        modifier
            .fillMaxSize()
            .focusProperties { canFocus = false }
            .semantics {
                contentDescription = "PGS subtitle overlay"
                stateDescription = composition.cue.id
            },
    ) {
        val video = PGSOverlayPolicy.videoRect(size.width, size.height, videoAspectRatio)
        if (video.width <= 0 || video.height <= 0) return@Canvas
        clipRect(
            left = video.x,
            top = video.y,
            right = video.x + video.width,
            bottom = video.y + video.height,
        ) {
            composition.objects.forEach { rendered ->
                val destination = PGSOverlayPolicy.objectRect(
                    object_ = rendered.object_,
                    canvasWidth = composition.cue.canvasWidth,
                    canvasHeight = composition.cue.canvasHeight,
                    destination = video,
                )
                drawImage(
                    image = rendered.bitmap.asImageBitmap(),
                    dstOffset = IntOffset(destination.x.roundToInt(), destination.y.roundToInt()),
                    dstSize = IntSize(
                        destination.width.roundToInt().coerceAtLeast(1),
                        destination.height.roundToInt().coerceAtLeast(1),
                    ),
                )
            }
        }
    }
}

@Composable
internal fun Controls(
    title: String,
    subtitle: String? = null,
    releaseDate: String? = null,
    runtimeLabel: String? = null,
    overview: String? = null,
    positionMs: Long,
    durationMs: Long,
    isPlaying: Boolean,
    requestInitialFocus: Boolean,
    mediaFacts: List<MediaFact> = emptyList(),
    onBack: () -> Unit,
    onPlayPause: () -> Unit,
    onSeekBack: () -> Unit,
    onSeekForward: () -> Unit,
    onScrubStart: () -> Unit,
    onScrub: (Long) -> Unit,
    onScrubEnd: () -> Unit,
    onTracks: () -> Unit,
    onSettings: () -> Unit,
    onInfo: () -> Unit,
    onPip: (() -> Unit)?,
    onTransportHeight: (Int) -> Unit = {},
) {
    val playFocusRequester = remember { FocusRequester() }
    RequestInitialFocus(playFocusRequester, enabled = requestInitialFocus)
    Box(Modifier.fillMaxSize()) {
        Row(
            Modifier.align(Alignment.TopStart).fillMaxWidth().padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TvIconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back", tint = Color.White)
            }
            Spacer(Modifier.weight(1f))
            TvIconButton(onClick = onTracks) {
                Icon(Icons.Filled.ClosedCaption, contentDescription = "Audio and subtitles", tint = Color.White)
            }
            TvIconButton(onClick = onSettings) {
                Icon(Icons.Filled.Tune, contentDescription = "Playback settings", tint = Color.White)
            }
            TvIconButton(onClick = onInfo) {
                Icon(Icons.Filled.Info, contentDescription = "Playback info", tint = Color.White)
            }
            if (onPip != null) {
                TvIconButton(onClick = onPip) {
                    Icon(Icons.Filled.PictureInPictureAlt, contentDescription = "Picture in picture", tint = Color.White)
                }
            }
        }

        Column(
            Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                // Reported so the playback info panel can reserve exactly this
                // block's height instead of guessing at it.
                .onSizeChanged { onTransportHeight(it.height) }
                .background(
                    Brush.verticalGradient(
                        listOf(Color.Transparent, Color.Black.copy(alpha = 0.9f)),
                    ),
                )
                .padding(start = 28.dp, top = 72.dp, end = 28.dp, bottom = 22.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(
                playerHeading(title, subtitle),
                color = Color.White,
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.Bold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (mediaFacts.isNotEmpty()) {
                Row(
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    mediaFacts.forEach { fact -> MediaFactChip(fact) }
                }
            }

            playerContextLine(releaseDate, runtimeLabel)?.let { context ->
                Text(
                    context,
                    color = Color.White.copy(alpha = 0.74f),
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                )
            }

            overview?.trim()?.takeIf { it.isNotEmpty() }?.let { summary ->
                Text(
                    summary,
                    color = Color.White.copy(alpha = 0.88f),
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 3,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.widthIn(max = 980.dp),
                )
            }

            BoxWithConstraints(Modifier.fillMaxWidth().padding(top = 4.dp)) {
                if (maxWidth >= 700.dp) {
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        TransportButtons(
                            isPlaying = isPlaying,
                            playFocusRequester = playFocusRequester,
                            onPlayPause = onPlayPause,
                            onSeekBack = onSeekBack,
                            onSeekForward = onSeekForward,
                        )
                        PlaybackTime(positionMs)
                        PlaybackPositionSlider(
                            positionMs = positionMs,
                            durationMs = durationMs,
                            playFocusRequester = playFocusRequester,
                            onScrubStart = onScrubStart,
                            onScrub = onScrub,
                            onScrubEnd = onScrubEnd,
                            modifier = Modifier.weight(1f),
                        )
                        PlaybackTime(durationMs)
                    }
                } else {
                    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center) {
                            TransportButtons(
                                isPlaying = isPlaying,
                                playFocusRequester = playFocusRequester,
                                onPlayPause = onPlayPause,
                                onSeekBack = onSeekBack,
                                onSeekForward = onSeekForward,
                            )
                        }
                        PlaybackPositionSlider(
                            positionMs = positionMs,
                            durationMs = durationMs,
                            playFocusRequester = playFocusRequester,
                            onScrubStart = onScrubStart,
                            onScrub = onScrub,
                            onScrubEnd = onScrubEnd,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                            PlaybackTime(positionMs)
                            PlaybackTime(durationMs)
                        }
                    }
                }
            }
        }
    }
}

internal fun playerHeading(title: String, subtitle: String?): String =
    listOfNotNull(subtitle?.trim()?.takeIf { it.isNotEmpty() }, title.trim())
        .joinToString("   ·   ")

internal fun playerContextLine(releaseDate: String?, runtimeLabel: String?): String? =
    listOfNotNull(
        releaseDate?.trim()?.takeIf { it.isNotEmpty() },
        runtimeLabel?.trim()?.takeIf { it.isNotEmpty() },
    ).takeIf { it.isNotEmpty() }?.joinToString("   ·   ")

@Composable
private fun TransportButtons(
    isPlaying: Boolean,
    playFocusRequester: FocusRequester,
    onPlayPause: () -> Unit,
    onSeekBack: () -> Unit,
    onSeekForward: () -> Unit,
) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TvIconButton(onClick = onSeekBack, modifier = Modifier.size(48.dp)) {
            Icon(
                Icons.Filled.Replay10,
                contentDescription = "Back 10 seconds",
                tint = Color.White,
                modifier = Modifier.size(34.dp),
            )
        }
        TvIconButton(
            onClick = onPlayPause,
            modifier = Modifier.size(56.dp).focusRequester(playFocusRequester),
        ) {
            Icon(
                if (isPlaying) Icons.Filled.Pause else Icons.Filled.PlayArrow,
                contentDescription = if (isPlaying) "Pause" else "Play",
                tint = Color.White,
                modifier = Modifier.size(42.dp),
            )
        }
        TvIconButton(onClick = onSeekForward, modifier = Modifier.size(48.dp)) {
            Icon(
                Icons.Filled.Forward10,
                contentDescription = "Forward 10 seconds",
                tint = Color.White,
                modifier = Modifier.size(34.dp),
            )
        }
    }
}

@Composable
private fun PlaybackPositionSlider(
    positionMs: Long,
    durationMs: Long,
    playFocusRequester: FocusRequester,
    onScrubStart: () -> Unit,
    onScrub: (Long) -> Unit,
    onScrubEnd: () -> Unit,
    modifier: Modifier,
) {
    val range = if (durationMs > 0) durationMs.toFloat() else 1f
    Slider(
        value = positionMs.coerceIn(0, durationMs.coerceAtLeast(0)).toFloat(),
        onValueChange = { onScrubStart(); onScrub(it.toLong()) },
        onValueChangeFinished = onScrubEnd,
        valueRange = 0f..range,
        modifier = modifier
            .semantics { contentDescription = "Playback position" }
            .onPreviewKeyEvent { event ->
                val vertical = event.nativeKeyEvent.keyCode == KeyEvent.KEYCODE_DPAD_UP ||
                    event.nativeKeyEvent.keyCode == KeyEvent.KEYCODE_DPAD_DOWN
                if (vertical) {
                    if (event.nativeKeyEvent.action == KeyEvent.ACTION_DOWN) {
                        playFocusRequester.requestFocus()
                    }
                    true
                } else {
                    false
                }
            },
        colors = SliderDefaults.colors(
            thumbColor = Accent,
            activeTrackColor = Accent,
            inactiveTrackColor = Color(0x55FFFFFF),
        ),
    )
}

@Composable
private fun PlaybackTime(milliseconds: Long) {
    Text(
        formatTime(milliseconds),
        color = Color.White,
        style = MaterialTheme.typography.labelMedium,
    )
}

@Composable
private fun PlayerSettings(
    vm: AppViewModel,
    qualityOptions: List<QualityOption>,
    audioOffsetMs: Long,
    declaredOffsetMs: Long?,
    currentPosition: () -> Long,
    onReload: (Long, String) -> Unit,
    onAudioOffset: (Long) -> Unit,
    onDismiss: () -> Unit,
) {
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val initialFocusRequester = remember { FocusRequester() }
    var offset by remember(audioOffsetMs) { mutableLongStateOf(audioOffsetMs) }
    RequestInitialFocus(initialFocusRequester)
    PlayerPanelSurface("Playback settings", onDismiss) {
        Text("Quality", color = Muted, style = MaterialTheme.typography.labelMedium)
        // The rungs are the server's, filtered to what this source can feed —
        // a 1080p file never offers to upscale itself to 4K.
        qualityOptions.forEach { option ->
            val quality = option.quality
            PanelRow(
                label = option.label,
                selected = preferences.playbackQuality == quality,
                modifier = if (preferences.playbackQuality == quality) {
                    Modifier.focusRequester(initialFocusRequester)
                } else {
                    Modifier
                },
            ) {
                val position = currentPosition()
                vm.setPlaybackQuality(quality)
                onReload(position, "quality")
            }
        }
        Text("Audio sync", color = Muted, style = MaterialTheme.typography.labelMedium, modifier = Modifier.padding(top = 12.dp))
        Text(offsetLabel(offset), color = Color.White, style = MaterialTheme.typography.bodyMedium)
        declaredOffsetMs?.let {
            Text("Container declares ${if (it > 0) "+" else ""}$it ms (already honored)", color = Muted, style = MaterialTheme.typography.labelMedium)
        }
        listOf(-250L, -50L, 50L, 250L).forEach { delta ->
            PanelRow("${if (delta > 0) "+" else ""}$delta ms · audio ${if (delta > 0) "later" else "earlier"}", false) {
                offset = (offset + delta).coerceIn(-15_000, 15_000)
                onAudioOffset(offset)
            }
        }
        if (offset != 0L) {
            PanelRow("Reset sync to 0 ms", false) {
                offset = 0
                onAudioOffset(0)
            }
        }
        PanelSwitch("Auto-skip intro and credits", preferences.autoSkip, vm::setAutoSkip)
        PanelSwitch("Autoplay next episode", preferences.autoplayNext, vm::setAutoplayNext)
    }
}

@Composable
private fun PlayerInfo(
    plan: Plan,
    controller: Controller,
    positionMs: Long,
    displayHdrTypes: Set<Int>,
    transportReserve: Dp,
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
    onDismiss: () -> Unit,
) {
    val player = controller.player
    val selectedAudio = player.audioFormat?.let(::audioLabel)
        ?: plan.audio.firstOrNull { it.index == controller.selectedAudio }?.let(::serverAudioLabel)
        ?: plan.audio.firstOrNull { it.default }?.let(::serverAudioLabel)
    val selectedSubtitle = selectedSubtitleLabel(
        player,
        plan.subtitles,
        controller.selectedSubtitle,
    ).let { label ->
        controller.pgsOverlayStatus.label?.let { "$label · $it" } ?: label
    }
    PlaybackInfoOverlay(
        details = PlaybackInfoDetails(
            title = plan.title,
            fileId = plan.fileId,
            delivery = deliveryLabel(controller.deliveryMode),
            position = "${formatTime(positionMs)} / ${formatTime(plan.durationMs)}",
            buffer = "${formatTime((player.bufferedPosition - player.currentPosition).coerceAtLeast(0))} ahead · " +
                "${player.bufferedPercentage.coerceIn(0, 100)}%",
            videoHealth = videoHealthSummary(player),
            sourceFile = plan.source?.filename,
            sourceVideo = sourceVideoSummary(plan.source),
            sourceAudio = sourceAudioSummary(plan.source),
            playingVideo = videoFormatSummary(player.videoFormat),
            playingAudio = selectedAudio,
            dynamicRange = dynamicRangeSummary(
                source = sourceDynamicRange(plan.source),
                delivered = controller.deliveredRange,
                rendered = renderedRange(
                    delivered = controller.deliveredRange,
                    decoderMime = player.videoFormat?.sampleMimeType,
                    decoderColorTransfer = player.videoFormat?.colorInfo?.colorTransfer,
                    hdrTypes = displayHdrTypes,
                ),
                reasons = plan.reasons,
            ),
            subtitles = selectedSubtitle,
            encoder = controller.encoder,
            audioSync = controller.audioOffsetMs.takeIf { it != 0L }?.let(::offsetLabel),
            build = "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})",
            transport = if (controller.currentSessionId != null) {
                "Segmented HLS · Android Media3"
            } else {
                "Continuous file · range requests · Android Media3"
            },
            sessionId = controller.currentSessionId,
            sessionStatus = controller.sessionStatus,
            playerState = playerStateLabel(player),
        ),
        reasons = plan.reasons,
        mode = mode,
        onMode = onMode,
        onDismiss = onDismiss,
        transportReserve = transportReserve,
    )
}

/**
 * How much of the bottom of the screen the transport block is occupying right
 * now. Zero when the controls are not composed — the common case, since opening
 * the info panel hides them — the measured height once they have been laid out,
 * and only a floor in the window between the two.
 */
@Composable
private fun playbackTransportReserve(controlsVisible: Boolean, measuredPx: Int): Dp = when {
    !controlsVisible -> 0.dp
    measuredPx > 0 -> with(LocalDensity.current) { measuredPx.toDp() }
    else -> PlaybackTransportReserveFallback
}

internal data class PlaybackInfoDetails(
    val title: String,
    val fileId: Long,
    val delivery: String,
    val position: String,
    val buffer: String,
    val videoHealth: String? = null,
    val sourceFile: String? = null,
    val sourceVideo: String? = null,
    val sourceAudio: String? = null,
    val playingVideo: String? = null,
    val playingAudio: String? = null,
    val dynamicRange: String? = null,
    val subtitles: String = "Off",
    val encoder: String? = null,
    val audioSync: String? = null,
    val build: String = "development",
    val transport: String = "Android Media3",
    val sessionId: String? = null,
    val sessionStatus: PlaybackSessionStatus? = null,
    val playerState: String = "Unknown",
)

/** Floating playback details with the same Mini/Standard/Debug contract as Apple and web. */
@Composable
internal fun PlaybackInfoOverlay(
    details: PlaybackInfoDetails,
    reasons: List<String>,
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
    onDismiss: () -> Unit,
    /** Height of the transport block currently on screen; zero when it is not. */
    transportReserve: Dp = 0.dp,
) {
    val closeFocusRequester = remember { FocusRequester() }
    RequestInitialFocus(closeFocusRequester)
    Box(
        Modifier.fillMaxSize().focusProperties { canFocus = false }.clickable(
            interactionSource = remember { MutableInteractionSource() },
            indication = null,
            onClick = onDismiss,
        ),
    ) {
        // One corner for all three modes: the block grows downward from a fixed
        // point instead of re-centring itself when the mode changes, and the
        // reserve at the bottom keeps it clear of the transport controls — but
        // only while those controls are actually composed. Opening this panel
        // hides them, so the reserve is normally zero and the panel gets the
        // whole height.
        BoxWithConstraints(
            Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .padding(PlaybackOverlayInset),
        ) {
            val available = maxHeight
            val panelHeight = (available - transportReserve).coerceAtLeast(
                minOf(available, PlaybackPanelMinHeight),
            )
            val miniWidth = minOf(maxWidth, 820.dp)
            Box(Modifier.align(Alignment.TopEnd)) {
                when (mode) {
                    PlaybackStatsMode.Mini -> PlaybackInfoMini(
                        details = details,
                        mode = mode,
                        onMode = onMode,
                        onDismiss = onDismiss,
                        closeFocusRequester = closeFocusRequester,
                        maxPanelWidth = miniWidth,
                    )
                    PlaybackStatsMode.Standard -> PlaybackInfoLedgerPanel(
                        title = "Playback info",
                        subtitle = "${details.delivery} · ${details.position}",
                        sections = playbackStandardSections(details, reasons),
                        surface = Color(0xE617181E),
                        maxPanelWidth = 1040.dp,
                        maxPanelHeight = minOf(panelHeight, 620.dp),
                        mode = mode,
                        onMode = onMode,
                        onDismiss = onDismiss,
                        closeFocusRequester = closeFocusRequester,
                    )
                    PlaybackStatsMode.Debug -> PlaybackInfoLedgerPanel(
                        title = "Playback debug",
                        subtitle = "Player · network · server",
                        sections = playbackDebugSections(details, reasons),
                        surface = Color(0xF017181E),
                        maxPanelWidth = 1180.dp,
                        maxPanelHeight = minOf(panelHeight, 760.dp),
                        mode = mode,
                        onMode = onMode,
                        onDismiss = onDismiss,
                        closeFocusRequester = closeFocusRequester,
                    )
                }
            }
        }
    }
}

/** Mini is a single shrink-wrapped line: the same facts, none of the stacking. */
@Composable
private fun PlaybackInfoMini(
    details: PlaybackInfoDetails,
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
    onDismiss: () -> Unit,
    closeFocusRequester: FocusRequester,
    maxPanelWidth: Dp,
) {
    val shape = MaterialTheme.shapes.large
    Row(
        Modifier
            .widthIn(max = maxPanelWidth)
            .clip(shape)
            .background(Color(0xE617181E))
            .border(1.dp, Color.White.copy(alpha = 0.14f), shape)
            .clickable(
                interactionSource = remember { MutableInteractionSource() },
                indication = null,
                onClick = {},
            )
            .padding(horizontal = 13.dp, vertical = 9.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            details.delivery,
            color = Color.White,
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.Bold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        PlaybackMiniDivider()
        PlaybackMiniValue(details.position)
        PlaybackMiniDivider()
        PlaybackMiniValue(
            details.playingVideo ?: "Waiting",
            playbackTone(details),
            Modifier.weight(1f, fill = false),
        )
        PlaybackMiniDivider()
        PlaybackMiniValue(
            details.buffer,
            bufferTone(details.sessionStatus?.ahead_seconds),
            Modifier.weight(1f, fill = false),
        )
        PlaybackMiniDivider()
        PlaybackMiniValue(
            details.sessionStatus?.delivered_bps?.let(::formatBitrate) ?: "Measuring",
            networkTone(details),
            Modifier.weight(1f, fill = false),
        )
        PlaybackMiniDivider()
        PlaybackHealthPill(details)
        PlaybackStatsModeSelector(mode, onMode)
        TvIconButton(
            onClick = onDismiss,
            modifier = Modifier.size(40.dp).focusRequester(closeFocusRequester),
        ) {
            Icon(Icons.Filled.Close, contentDescription = "Close playback info", tint = Color.White)
        }
    }
}

@Composable
private fun PlaybackMiniDivider() {
    VerticalDivider(
        modifier = Modifier.height(16.dp),
        color = Color.White.copy(alpha = 0.14f),
    )
}

@Composable
private fun PlaybackMiniValue(
    value: String,
    tone: PlaybackStatTone = PlaybackStatTone.Neutral,
    modifier: Modifier = Modifier,
) {
    Text(
        value,
        color = tone.color,
        style = MaterialTheme.typography.bodySmall,
        fontWeight = FontWeight.SemiBold,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier,
    )
}

/**
 * The pill says in one word what the SERVER card's Status row says in three or
 * four — same session status, so the two can never disagree, and the same word
 * the web client's pill shows for the same condition. It deliberately reports
 * the *server's* state, not the player's: "Buffering" is a fact about this
 * device, and the pill is about the line feeding it. Android has no cached-VOD
 * delivery, so the web's fourth word ("Cached") has no counterpart here.
 */
private fun serverHealthWord(details: PlaybackInfoDetails): String {
    val status = details.sessionStatus ?: return "Direct"
    return if (status.suspended == true) "Held" else "Active"
}

private fun serverHealthTone(details: PlaybackInfoDetails): PlaybackStatTone =
    if (details.sessionStatus == null) PlaybackStatTone.Muted else PlaybackStatTone.Good

/** Dot plus one word of the server's state — the line's health at a glance. */
@Composable
private fun PlaybackHealthPill(details: PlaybackInfoDetails) {
    val tone = serverHealthTone(details)
    Row(
        Modifier
            .background(Color.White.copy(alpha = 0.07f), MaterialTheme.shapes.extraLarge)
            .padding(horizontal = 9.dp, vertical = 5.dp),
        horizontalArrangement = Arrangement.spacedBy(5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(7.dp).background(tone.color, MaterialTheme.shapes.extraLarge))
        Text(
            serverHealthWord(details),
            color = tone.color,
            style = MaterialTheme.typography.labelSmall,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** Standard and Debug are the same ledger; only the field set and the caps differ. */
@Composable
private fun PlaybackInfoLedgerPanel(
    title: String,
    subtitle: String,
    sections: List<PlaybackStatSection>,
    surface: Color,
    maxPanelWidth: Dp,
    maxPanelHeight: Dp,
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
    onDismiss: () -> Unit,
    closeFocusRequester: FocusRequester,
) {
    val shape = MaterialTheme.shapes.large
    Column(
        Modifier
            .widthIn(max = maxPanelWidth)
            .fillMaxWidth()
            .heightIn(max = maxPanelHeight)
            .clip(shape)
            .background(surface)
            .border(1.dp, Color.White.copy(alpha = 0.14f), shape)
            .clickable(
                interactionSource = remember { MutableInteractionSource() },
                indication = null,
                onClick = {},
            )
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(7.dp),
    ) {
        PlaybackInfoHeader(
            title = title,
            subtitle = subtitle,
            mode = mode,
            onMode = onMode,
            onDismiss = onDismiss,
            closeFocusRequester = closeFocusRequester,
        )
        HorizontalDivider(color = Color.White.copy(alpha = 0.12f))
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val dense = maxWidth >= PlaybackDenseWidth
            Column(
                Modifier.fillMaxWidth().verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                PlaybackStatLedger(sections, dense)
            }
        }
    }
}

/**
 * Two columns with a fixed section→column assignment, then one full-width strip
 * for the values that are sentences. Nothing here measures a section to decide
 * where it goes: the same section is always on the same side.
 */
@Composable
private fun PlaybackStatLedger(sections: List<PlaybackStatSection>, dense: Boolean) {
    val visible = sections.filter { it.stats.isNotEmpty() }
    val left = PlaybackLedgerLeft.mapNotNull { title -> visible.firstOrNull { it.title == title } }
    val right = PlaybackLedgerRight.mapNotNull { title -> visible.firstOrNull { it.title == title } }
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            left.forEach { PlaybackStatCard(it, dense) }
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            right.forEach { PlaybackStatCard(it, dense) }
        }
    }
    val notes = (left + right).filter { it.notes.isNotEmpty() }
    if (notes.isNotEmpty()) {
        HorizontalDivider(color = Color.White.copy(alpha = 0.12f))
        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            PlaybackInfoSection("NOTES")
            notes.forEach { section ->
                if (notes.size > 1) {
                    // Says which card these lines came from, so it must not
                    // look like a card heading: headings are Accent and bold,
                    // this is dimmer than a row label, unbolded and tracked
                    // out, and it sits tight against the rows it introduces.
                    Text(
                        section.title,
                        color = Color.White.copy(alpha = 0.38f),
                        style = MaterialTheme.typography.labelSmall,
                        fontWeight = FontWeight.Normal,
                        letterSpacing = 1.2.sp,
                        modifier = Modifier
                            .padding(top = 3.dp)
                            .testTag(PlaybackNotesGroupTag),
                    )
                }
                section.notes.forEach { stat ->
                    PlaybackInfoRow(
                        label = stat.label,
                        value = stat.value,
                        tone = stat.tone,
                        maxLines = Int.MAX_VALUE,
                    )
                }
            }
        }
    }
}

/**
 * A section is aligned as a unit: mostly-numeric sections get their values
 * right-aligned on tabular figures, everything else stays start-aligned. A long
 * dense section splits into two sub-columns where the width can carry it.
 */
@Composable
private fun PlaybackStatCard(section: PlaybackStatSection, dense: Boolean) {
    val rows = section.grid
    if (rows.isEmpty()) return
    val numeric = rows.count { isNumericStat(it.value) }
    val alignEnd = numeric * 10 >= rows.size * 7
    val twoUp = dense && rows.size >= 10 && rows.all { it.value.length <= 12 }
    PlaybackDebugCard(section.title) {
        if (twoUp) {
            val split = (rows.size + 1) / 2
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    rows.take(split).forEach {
                        PlaybackInfoRow(it.label, it.value, it.tone, PlaybackDenseLabelWidth, alignEnd)
                    }
                }
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    rows.drop(split).forEach {
                        PlaybackInfoRow(it.label, it.value, it.tone, PlaybackDenseLabelWidth, alignEnd)
                    }
                }
            }
        } else {
            rows.forEach { PlaybackInfoRow(it.label, it.value, it.tone, alignEnd = alignEnd) }
        }
    }
}

@Composable
private fun PlaybackInfoHeader(
    title: String,
    subtitle: String,
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
    onDismiss: () -> Unit,
    closeFocusRequester: FocusRequester,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(title, color = Color.White, style = MaterialTheme.typography.titleMedium)
            Text(
                subtitle,
                color = Color.White.copy(alpha = 0.62f),
                style = MaterialTheme.typography.labelSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        PlaybackStatsModeSelector(mode, onMode)
        TvIconButton(
            onClick = onDismiss,
            modifier = Modifier.size(40.dp).focusRequester(closeFocusRequester),
        ) {
            Icon(Icons.Filled.Close, contentDescription = "Close playback info", tint = Color.White)
        }
    }
}

@Composable
private fun PlaybackStatsModeSelector(
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(3.dp), modifier = Modifier.padding(horizontal = 7.dp)) {
        PlaybackStatsMode.entries.forEach { candidate ->
            Text(
                candidate.label,
                color = if (candidate == mode) Color.White else Color.White.copy(alpha = 0.62f),
                style = MaterialTheme.typography.labelSmall,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier
                    .clip(MaterialTheme.shapes.extraLarge)
                    .background(if (candidate == mode) Accent else Color.White.copy(alpha = 0.07f))
                    .tvFocusRing(MaterialTheme.shapes.extraLarge)
                    .clickable { onMode(candidate) }
                    .focusable()
                    .padding(horizontal = 9.dp, vertical = 5.dp),
            )
        }
    }
}

@Composable
private fun PlaybackDebugCard(
    title: String,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(
        modifier
            .fillMaxWidth()
            .background(Color.White.copy(alpha = 0.045f), MaterialTheme.shapes.medium)
            .border(1.dp, Color.White.copy(alpha = 0.08f), MaterialTheme.shapes.medium)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        PlaybackInfoSection(title)
        content()
    }
}

@Composable
private fun PlaybackInfoSection(title: String) {
    Text(
        title,
        color = Accent,
        style = MaterialTheme.typography.labelSmall,
        fontWeight = FontWeight.Bold,
        modifier = Modifier.padding(bottom = 2.dp).testTag(PlaybackSectionHeadTag),
    )
}

@Composable
private fun PlaybackInfoRow(
    label: String,
    value: String,
    tone: PlaybackStatTone = PlaybackStatTone.Neutral,
    labelWidth: Dp = PlaybackLabelWidth,
    alignEnd: Boolean = false,
    maxLines: Int = 1,
) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(7.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Text(
            label,
            color = Color.White.copy(alpha = 0.48f),
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.width(labelWidth),
        )
        Text(
            value,
            color = tone.color,
            style = if (alignEnd) {
                MaterialTheme.typography.bodySmall.copy(fontFeatureSettings = "tnum")
            } else {
                MaterialTheme.typography.bodySmall
            },
            fontWeight = FontWeight.Medium,
            maxLines = maxLines,
            overflow = TextOverflow.Ellipsis,
            textAlign = if (alignEnd) TextAlign.End else TextAlign.Start,
            modifier = Modifier.weight(1f),
        )
    }
}

/**
 * The two roles a section title can play in the ledger: heading its own card,
 * and naming the group its prose lines were collected into. Tagged so tests can
 * tell the two apart by role instead of counting how many times the text occurs.
 */
internal const val PlaybackSectionHeadTag = "playback-section-head"
internal const val PlaybackNotesGroupTag = "playback-notes-group"

private val PlaybackOverlayInset = 12.dp

/**
 * Only used for the frame or two between the controls appearing and their first
 * layout pass; after that the real measurement replaces it. Derived from the
 * fixed chrome of the bottom control block in [Controls]: 72.dp top padding +
 * one headlineSmall title line (~32.dp) + 6.dp spacing + the 56.dp transport
 * row + 22.dp bottom padding = 188.dp, rounded up. That is the block's floor;
 * with chips, a context line and a three-line overview it is closer to 318.dp,
 * which is exactly why this is a fallback and not the reserve.
 */
private val PlaybackTransportReserveFallback = 192.dp
private val PlaybackPanelMinHeight = 180.dp
private val PlaybackLabelWidth = 86.dp
private val PlaybackDenseLabelWidth = 62.dp
private val PlaybackDenseWidth = 760.dp

private val PlaybackLedgerLeft = listOf("PLAYBACK", "SOURCE", "NOW DECODING")
private val PlaybackLedgerRight = listOf("NETWORK", "SERVER")

/** One row of the ledger. `note` sends it to the full-width strip instead of the grid. */
private data class PlaybackStat(
    val label: String,
    val value: String,
    val tone: PlaybackStatTone = PlaybackStatTone.Neutral,
    val note: Boolean = false,
)

private class PlaybackStatSection(val title: String, val stats: List<PlaybackStat>) {
    val grid: List<PlaybackStat> = stats.filter { !it.note }
    val notes: List<PlaybackStat> = stats.filter { it.note }
}

/**
 * A source value is a run of separator-joined facts, sometimes with a clause
 * hung off the end ("+250 ms — audio plays later"). The leading facts are a
 * datum and belong in the grid; the clause, or a tail longer than the eye can
 * scan in a single line, is prose and belongs in the notes strip. Returns the
 * head and, when there is one, the tail — between them they still spell out
 * every fact in the original value.
 */
private fun splitLedgerValue(value: String, facts: Int = 3): Pair<String, String?> {
    val clause = value.indexOf(" — ")
    if (clause >= 0) return value.take(clause) to value.substring(clause + 3)
    val parts = value.split(" · ")
    if (parts.size <= facts) return value to null
    return parts.take(facts).joinToString(" · ") to parts.drop(facts).joinToString(" · ")
}

/** The grid row, plus the note carrying whatever did not fit on it. */
private fun ledgerSplitStats(label: String, value: String): List<PlaybackStat> {
    val (head, tail) = splitLedgerValue(value)
    return listOfNotNull(
        PlaybackStat(label, head),
        tail?.let { PlaybackStat(label, it, note = true) },
    )
}

private fun isNumericStat(value: String): Boolean {
    val trimmed = value.trimStart()
    val head = trimmed.firstOrNull() ?: return false
    if (head.isDigit()) return true
    return (head == '#' || head == '+' || head == '-') && trimmed.getOrNull(1)?.isDigit() == true
}

private fun playbackStandardSections(
    details: PlaybackInfoDetails,
    reasons: List<String>,
): List<PlaybackStatSection> {
    val status = details.sessionStatus
    return listOf(
        PlaybackStatSection(
            "PLAYBACK",
            listOfNotNull(
                reasons.takeIf { it.isNotEmpty() }?.let {
                    PlaybackStat("Reason", it.joinToString(" · "), PlaybackStatTone.Muted, note = true)
                },
            ),
        ),
        // The codec/resolution head of each line is a datum and carries the
        // card; only the filename and the trailing facts are prose enough to
        // go to the notes strip. Left as all-notes this card never rendered.
        PlaybackStatSection(
            "SOURCE",
            buildList {
                details.sourceFile?.let { add(PlaybackStat("File", it, note = true)) }
                addAll(ledgerSplitStats("Video", details.sourceVideo ?: "Unknown"))
                details.sourceAudio?.let { addAll(ledgerSplitStats("Audio", it)) }
            },
        ),
        PlaybackStatSection(
            "NOW DECODING",
            listOfNotNull(
                PlaybackStat("Video", details.playingVideo ?: "Waiting", note = true),
                details.dynamicRange?.let { PlaybackStat("Range", it, note = true) },
                details.playingAudio?.let { PlaybackStat("Audio", it, note = true) },
                PlaybackStat("Subtitles", details.subtitles),
                PlaybackStat("Buffer", details.buffer, bufferTone(status?.ahead_seconds)),
                details.videoHealth?.let {
                    PlaybackStat("Frames", it, videoHealthTone(it), note = true)
                },
            ),
        ),
        PlaybackStatSection(
            "SERVER",
            listOfNotNull(
                PlaybackStat(
                    "Status",
                    when {
                        status == null -> "No server-side session"
                        status.suspended == true -> "Holding buffer"
                        else -> "Active"
                    },
                    if (status == null) PlaybackStatTone.Muted else PlaybackStatTone.Good,
                ),
                (status?.encoder ?: details.encoder)?.let { PlaybackStat("Encoder", it) },
                (status?.recent_speed ?: status?.speed)?.let {
                    PlaybackStat("Encode", String.format(Locale.US, "%.2f×", it), encodeTone(it, status))
                },
                status?.ahead_seconds?.let {
                    PlaybackStat(
                        "Ahead",
                        "${it.coerceAtLeast(0)} s",
                        bufferTone(it, status.suspended == true),
                    )
                },
                status?.delivered_bps?.let {
                    PlaybackStat("Delivery", formatBitrate(it), networkTone(details))
                },
            ),
        ),
    )
}

private fun playbackDebugSections(
    details: PlaybackInfoDetails,
    reasons: List<String>,
): List<PlaybackStatSection> {
    val status = details.sessionStatus
    return listOf(
        PlaybackStatSection(
            "PLAYBACK",
            listOfNotNull(
                PlaybackStat("Build", details.build),
                PlaybackStat("Method", details.delivery),
                PlaybackStat("Transport", details.transport, note = true),
                PlaybackStat("Position", details.position),
                PlaybackStat("Player state", details.playerState, playerStateTone(details.playerState)),
                PlaybackStat("File ID", "#${details.fileId}"),
                details.sessionId?.let { PlaybackStat("Session", it, note = true) },
                reasons.takeIf { it.isNotEmpty() }?.let {
                    PlaybackStat("Reason", it.joinToString("; "), PlaybackStatTone.Muted, note = true)
                },
            ),
        ),
        // Same split as Standard: datum head in the grid, prose tail in notes.
        // "AV offset" is "+250 ms — audio plays later"; the measurement is the
        // datum, the explanation of it is the clause.
        PlaybackStatSection(
            "SOURCE",
            buildList {
                details.sourceFile?.let { add(PlaybackStat("File", it, note = true)) }
                details.sourceVideo?.let { addAll(ledgerSplitStats("Video", it)) }
                details.sourceAudio?.let { addAll(ledgerSplitStats("Audio", it)) }
                details.audioSync?.let { addAll(ledgerSplitStats("AV offset", it)) }
            },
        ),
        PlaybackStatSection(
            "NOW DECODING",
            listOfNotNull(
                details.playingVideo?.let { PlaybackStat("Video", it, note = true) },
                details.dynamicRange?.let { PlaybackStat("Range", it, note = true) },
                details.playingAudio?.let { PlaybackStat("Audio", it, note = true) },
                PlaybackStat("Subtitles", details.subtitles),
                PlaybackStat("Buffer", details.buffer, bufferTone(status?.ahead_seconds)),
                details.videoHealth?.let {
                    PlaybackStat("Frames", it, videoHealthTone(it), note = true)
                },
            ),
        ),
        PlaybackStatSection(
            "NETWORK",
            listOfNotNull(
                status?.delivered_bps?.let {
                    PlaybackStat("Delivery", formatBitrate(it), networkTone(details))
                },
                status?.delivered_bytes?.let { PlaybackStat("Transferred", formatBytes(it)) },
                status?.delivered_idle_ms?.let {
                    PlaybackStat("Delivery idle", "$it ms", idleTone(it, status.suspended == true))
                },
            ),
        ),
        PlaybackStatSection("SERVER", playbackDebugServerStats(details)),
    )
}

private fun playbackDebugServerStats(details: PlaybackInfoDetails): List<PlaybackStat> {
    val status = details.sessionStatus
        ?: return listOf(PlaybackStat("Status", "No server session", PlaybackStatTone.Muted))
    val speed = status.recent_speed ?: status.speed
    return listOfNotNull(
        PlaybackStat(
            "Status",
            if (status.suspended == true) "Holding" else "Active",
            PlaybackStatTone.Good,
        ),
        PlaybackStat("Encoder", status.encoder ?: details.encoder ?: "—"),
        PlaybackStat(
            "Encode speed",
            speed?.let { String.format(Locale.US, "%.2f×", it) } ?: "—",
            speed?.let { encodeTone(it, status) } ?: PlaybackStatTone.Muted,
        ),
        status.ahead_seconds?.let {
            PlaybackStat(
                "Server ahead",
                "${it.coerceAtLeast(0)} s",
                bufferTone(it, status.suspended == true),
            )
        },
        status.ahead_bytes?.let { PlaybackStat("Ahead bytes", formatBytes(it)) },
        status.out_time_ms?.let { PlaybackStat("Produced", formatTime(it)) },
        status.progress_idle_ms?.let {
            PlaybackStat("Progress idle", "$it ms", idleTone(it, status.suspended == true))
        },
        PlaybackStat(
            "Held",
            if (status.suspended == true) "Yes" else "No",
            if (status.suspended == true) PlaybackStatTone.Good else PlaybackStatTone.Neutral,
        ),
        status.hold_reason?.let { PlaybackStat("Hold reason", it, PlaybackStatTone.Good) },
        status.suspend_count?.let {
            PlaybackStat(
                "Suspend count",
                it.toString(),
                if (it > 8) PlaybackStatTone.Warning else PlaybackStatTone.Neutral,
            )
        },
        status.readrate?.let { PlaybackStat("Pacing", String.format(Locale.US, "%.2f×", it)) },
        status.playlist_shape?.let { PlaybackStat("Playlist", it) },
        status.last_request?.let { PlaybackStat("Last request", it) },
        status.idle_seconds?.let {
            PlaybackStat("Request idle", "$it s", requestIdleTone(it, status.suspended == true))
        },
        status.published_end_ms?.let { PlaybackStat("Published end", "$it ms") },
        status.fetched_end_ms?.let { PlaybackStat("Fetched end", "$it ms") },
        status.fetched_segment?.let { PlaybackStat("Fetched segment", it.toString()) },
        status.first_retained_segment?.let { PlaybackStat("First retained", it.toString()) },
    )
}

private fun playbackTone(details: PlaybackInfoDetails): PlaybackStatTone =
    videoHealthTone(details.videoHealth)

private fun videoHealthTone(summary: String?): PlaybackStatTone = when {
    summary == null -> PlaybackStatTone.Muted
    summary.contains(" 0 dropped") -> PlaybackStatTone.Good
    summary.contains(" max streak 1") -> PlaybackStatTone.Warning
    else -> PlaybackStatTone.Critical
}

private fun bufferTone(
    seconds: Long?,
    suspended: Boolean = false,
): PlaybackStatTone = when {
    suspended -> PlaybackStatTone.Good
    seconds == null -> PlaybackStatTone.Muted
    seconds < 2 -> PlaybackStatTone.Critical
    seconds < 5 -> PlaybackStatTone.Warning
    else -> PlaybackStatTone.Good
}

private fun networkTone(details: PlaybackInfoDetails): PlaybackStatTone {
    val status = details.sessionStatus ?: return PlaybackStatTone.Muted
    return idleTone(status.delivered_idle_ms, status.suspended == true).let {
        if (it == PlaybackStatTone.Neutral && (status.delivered_bps ?: 0) > 0) {
            PlaybackStatTone.Good
        } else {
            it
        }
    }
}

private fun encodeTone(
    speed: Double,
    status: PlaybackSessionStatus?,
): PlaybackStatTone {
    if (status?.suspended == true) return PlaybackStatTone.Good
    val ahead = status?.ahead_seconds ?: 0
    return when {
        speed < 0.65 && ahead < 2 -> PlaybackStatTone.Critical
        speed < 1.0 && ahead < 10 -> PlaybackStatTone.Warning
        else -> PlaybackStatTone.Good
    }
}

private fun playerStateTone(state: String): PlaybackStatTone {
    val normalized = state.lowercase()
    return when {
        "error" in normalized || "fail" in normalized -> PlaybackStatTone.Critical
        "buffer" in normalized || "wait" in normalized -> PlaybackStatTone.Warning
        "ready" in normalized || "play" in normalized -> PlaybackStatTone.Good
        else -> PlaybackStatTone.Neutral
    }
}

private fun idleTone(milliseconds: Long?, suspended: Boolean): PlaybackStatTone = when {
    suspended -> PlaybackStatTone.Good
    milliseconds == null -> PlaybackStatTone.Muted
    milliseconds > 20_000 -> PlaybackStatTone.Critical
    milliseconds > 8_000 -> PlaybackStatTone.Warning
    else -> PlaybackStatTone.Neutral
}

private fun requestIdleTone(seconds: Long, suspended: Boolean): PlaybackStatTone = when {
    suspended -> PlaybackStatTone.Good
    seconds > 20 -> PlaybackStatTone.Critical
    seconds > 8 -> PlaybackStatTone.Warning
    else -> PlaybackStatTone.Neutral
}

internal fun deliveryLabel(mode: String): String = when (mode) {
    "direct" -> "Direct play"
    "remux" -> "Direct stream · remux"
    "transcode" -> "Transcode · HLS"
    else -> mode.ifBlank { "Unknown" }.replace('_', ' ').replaceFirstChar { it.uppercase() }
}

internal fun sourceVideoSummary(file: MediaFileDto?): String? {
    if (file == null) return null
    return listOfNotNull(
        file.video_codec?.uppercase(),
        file.video_profile?.takeIf { it.isNotBlank() },
        if (file.width != null && file.height != null) "${file.width}×${file.height}" else file.height?.let { "${it}p" },
        (file.hdr_format ?: file.hdr)?.takeIf { it.isNotBlank() },
        file.bit_depth?.takeIf { it > 0 }?.let { "${it}-bit" },
        file.bitrate?.takeIf { it > 0 }?.let(::formatBitrate),
    ).joinToString(" · ").ifBlank { null }
}

internal fun sourceAudioSummary(file: MediaFileDto?): String? {
    val streams = file?.audio_streams.orEmpty()
    val stream = streams.firstOrNull { it.default } ?: streams.firstOrNull() ?: return null
    val base = listOfNotNull(
        stream.codec?.uppercase(),
        stream.channels?.let(::channelLabel),
        languageName(stream.language),
        stream.title?.takeIf { it.isNotBlank() },
    ).distinct().joinToString(" · ")
    val others = streams.size - 1
    return if (others > 0) "$base · +$others track${if (others == 1) "" else "s"}" else base
}

/**
 * The info panel's "Dynamic range" row — the chip's arrow, in words, with the
 * server's own reason for it where one exists.
 *
 * ```
 * Dolby Vision (rendering)
 * HDR10 — Dolby Vision metadata removed for this device; compatible HDR base kept
 * SDR — tone-mapped from HDR10
 * ```
 */
internal fun dynamicRangeSummary(
    source: String?,
    delivered: String?,
    rendered: String?,
    reasons: List<String>,
): String? {
    if (source == null) return null
    if (delivered == null) return dynamicRangeLabel(source)
    val onScreen = rendered ?: delivered
    if (onScreen == source) return "${dynamicRangeLabel(source)} (rendering)"
    val why = reasons.firstOrNull { reason ->
        reason.contains("dolby vision", ignoreCase = true) || reason.contains("hdr", ignoreCase = true) ||
            reason.contains("tone", ignoreCase = true)
    } ?: "${dynamicRangeLabel(source)} source is not what this session is putting on screen"
    return "${dynamicRangeLabel(onScreen)} — $why"
}

private fun videoFormatSummary(format: Format?): String? {
    if (format == null) return null
    val hdr = when (format.colorInfo?.colorTransfer) {
        C.COLOR_TRANSFER_ST2084 -> "HDR10 / PQ"
        C.COLOR_TRANSFER_HLG -> "HLG"
        else -> null
    }
    return listOfNotNull(
        codecShort(format.sampleMimeType) ?: format.codecs?.takeIf { it.isNotBlank() },
        if (format.width != Format.NO_VALUE && format.height != Format.NO_VALUE) "${format.width}×${format.height}" else null,
        hdr,
        format.bitrate.takeIf { it != Format.NO_VALUE && it > 0 }?.toLong()?.let(::formatBitrate),
    ).joinToString(" · ").ifBlank { null }
}

private fun selectedSubtitleLabel(
    player: Player,
    serverTracks: List<SubTrack>,
    selectedServerTrack: Long?,
): String {
    // The controller's selection is the truth now that one menu row covers
    // both deliveries: a burnt track has no selected text track to read back,
    // and a rendition's own label is the less informative of the two.
    serverTracks.firstOrNull { it.index == selectedServerTrack }?.let { return serverSubtitleLabel(it) }
    player.currentTracks.groups.filter { it.type == C.TRACK_TYPE_TEXT }.forEach { group ->
        repeat(group.length) { index ->
            if (group.isTrackSelected(index)) return subLabel(group.getTrackFormat(index))
        }
    }
    return "Off"
}

private fun channelLabel(channels: Int): String = when (channels) {
    1 -> "Mono"
    2 -> "Stereo"
    6 -> "5.1"
    8 -> "7.1"
    else -> "${channels}ch"
}

private fun formatBitrate(bitsPerSecond: Long): String = if (bitsPerSecond >= 1_000_000) {
    "%.1f Mbps".format(bitsPerSecond / 1_000_000.0)
} else {
    "${bitsPerSecond / 1_000} kbps"
}

private fun formatBytes(bytes: Long): String {
    val value = bytes.coerceAtLeast(0)
    return when {
        value >= 1_000_000_000 -> String.format(Locale.US, "%.2f GB", value / 1_000_000_000.0)
        value >= 1_000_000 -> String.format(Locale.US, "%.1f MB", value / 1_000_000.0)
        value >= 1_000 -> String.format(Locale.US, "%.1f KB", value / 1_000.0)
        else -> "$value B"
    }
}

private fun playerStateLabel(player: Player): String = when (player.playbackState) {
    Player.STATE_IDLE -> "Idle"
    Player.STATE_BUFFERING -> "Buffering"
    Player.STATE_READY -> if (player.isPlaying) "Playing" else "Ready / paused"
    Player.STATE_ENDED -> "Ended"
    else -> "Unknown"
}

private fun videoHealthSummary(player: ExoPlayer): String? {
    val counters = player.videoDecoderCounters ?: return null
    counters.ensureUpdated()
    return videoHealthSummary(
        renderedFrames = counters.renderedOutputBufferCount,
        droppedFrames = counters.droppedBufferCount,
        maxConsecutiveDroppedFrames = counters.maxConsecutiveDroppedBufferCount,
    )
}

internal fun videoHealthSummary(
    renderedFrames: Int,
    droppedFrames: Int,
    maxConsecutiveDroppedFrames: Int,
): String {
    val rendered = renderedFrames.coerceAtLeast(0)
    val dropped = droppedFrames.coerceAtLeast(0)
    val total = rendered.toLong() + dropped
    val dropRate = if (total == 0L) 0.0 else dropped * 100.0 / total
    return String.format(
        Locale.US,
        "%,d rendered · %,d dropped (%.1f%%) · max streak %,d",
        rendered,
        dropped,
        dropRate,
        maxConsecutiveDroppedFrames.coerceAtLeast(0),
    )
}

@Composable
private fun PlayerPanelSurface(title: String, onDismiss: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    Box(
        Modifier
            .fillMaxSize()
            .background(Color(0x99000000))
            .focusProperties { canFocus = false }
            .clickable(onClick = onDismiss),
    ) {
        Column(
            Modifier.align(Alignment.CenterEnd).fillMaxHeight().widthIn(max = 420.dp).fillMaxWidth(0.92f)
                .background(Surface).verticalScroll(rememberScrollState()).padding(24.dp)
                .focusProperties { canFocus = false }
                .clickable(onClick = {}),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(title, style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
                TvIconButton(onClick = onDismiss) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Close")
                }
            }
            content()
        }
    }
}

@Composable
private fun PanelRow(
    label: String,
    selected: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    Text(
        (if (selected) "●  " else "    ") + label,
        color = if (selected) Accent else Color.White,
        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
        modifier = modifier
            .fillMaxWidth()
            .tvFocusRing(MaterialTheme.shapes.small, focusedScale = 1.02f)
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 9.dp),
    )
}

@Composable
private fun PanelSwitch(label: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(top = 8.dp)
            .tvFocusRing(MaterialTheme.shapes.small, focusedScale = 1.02f)
            .clickable { onCheckedChange(!checked) }
            .padding(horizontal = 8.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(label, color = Color.White, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = null)
    }
}

private fun offsetLabel(ms: Long): String = if (ms == 0L) {
    "0 ms (no correction)"
} else {
    "${if (ms > 0) "+" else ""}$ms ms — audio plays ${if (ms > 0) "later" else "earlier"}"
}

@Composable
private fun BackChip(onExit: () -> Unit) {
    TvIconButton(onClick = onExit, modifier = Modifier.padding(4.dp)) {
        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back", tint = Color.White)
    }
}
