@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@file:kotlin.OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)

package tv.plurx.app.player

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
import androidx.compose.foundation.focusGroup
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
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.clipRect
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
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.Rung
import tv.plurx.app.data.Session
import tv.plurx.app.data.SubTrack
import tv.plurx.app.ui.AppViewModel
import tv.plurx.app.ui.FormFactor
import tv.plurx.app.ui.PlaybackTarget
import tv.plurx.app.ui.catchingUnlessCancelled
import tv.plurx.app.ui.currentFormFactor
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
    override val requiresHls: Boolean,
    override val sourceHeight: Int?,
    override val sourceFrameRate: Double?,
    override val aac: Boolean,
    override val preserveDolbyVision: Boolean,
    override val deliveredDynamicRange: String?,
    override val deliveredDolbyVisionProfile: Int?,
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
    /** Quality captured by the exact request that produced this plan. */
    override val requestedQuality: PlaybackQuality,
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
    requestedQuality: PlaybackQuality,
): Plan {
    val detail = planLoadStage("item_detail") { vm.itemDetail(itemId) }
    // The pre-play choice reaches the *first* decision, so the plan that comes
    // back already carries it. Starting on the policy default and switching
    // afterwards is what criterion 4 forbids: it is a visible re-buffer to
    // apply something the viewer chose before playback began.
    val playbackDecision = planLoadStage("decision") {
        vm.playbackDecision(fileId, tracks, requestedQuality)
    }
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
            requiresHls = decision.delivery?.requires_hls ?: false,
            // The decision's own reading of the source: the number every height
            // promise is made of. The item's file row is the fallback for a
            // server too old to send `source`.
            sourceHeight = (decision.source?.height ?: file?.height)?.toInt(),
            sourceFrameRate = parseFrameRateRational(decision.source?.frame_rate),
            aac = decision.delivery?.aac ?: decision.transcode_audio,
            // Direct delivery has no remux-specific field, but the flattened
            // decision still says whether these exact source bytes are DV. Keep
            // that fact so a decoder failure can try a DV-preserving MP4 remux
            // before the final SDR compatibility transcode.
            preserveDolbyVision = decision.delivery?.preserve_dolby_vision
                ?: decision.preserve_dolby_vision,
            deliveredDynamicRange = decision.delivered_dynamic_range,
            deliveredDolbyVisionProfile = decision.delivered_dolby_vision_profile,
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
            requestedQuality = requestedQuality,
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

internal enum class PlayerControlId {
    SkipBack,
    PlayPause,
    SkipForward,
    Tracks,
    Settings,
    Info,
    PictureInPicture,
    Timeline,
    Marker,
    ;

    val isTransport: Boolean get() = this !in setOf(Timeline, Marker)
}

internal class PlayerControlFocus {
    val skipBack = FocusRequester()
    val playPause = FocusRequester()
    val skipForward = FocusRequester()
    val tracks = FocusRequester()
    val settings = FocusRequester()
    val info = FocusRequester()
    val pictureInPicture = FocusRequester()
    val timeline = FocusRequester()
    val marker = FocusRequester()

    fun requester(control: PlayerControlId): FocusRequester = when (control) {
        PlayerControlId.SkipBack -> skipBack
        PlayerControlId.PlayPause -> playPause
        PlayerControlId.SkipForward -> skipForward
        PlayerControlId.Tracks -> tracks
        PlayerControlId.Settings -> settings
        PlayerControlId.Info -> info
        PlayerControlId.PictureInPicture -> pictureInPicture
        PlayerControlId.Timeline -> timeline
        PlayerControlId.Marker -> marker
    }
}

internal enum class PlaybackStatsMode(val storageValue: String, val label: String) {
    Mini("mini", "Compact"),
    Standard("standard", "Overview"),
    Details("details", "Details"),
    Debug("debug", "Diagnostics"),
    ;

    companion object {
        fun fromStorage(value: String?): PlaybackStatsMode = entries.firstOrNull {
            it.storageValue == value
        } ?: Standard
    }
}

internal enum class PlaybackStatTone(val color: Color) {
    Neutral(Color(0xFFEEEEF2)),
    Muted(Color(0xFF9697A2)),
    Good(Color(0xFF6DDB98)),
    Warning(Color(0xFFFFBD4A)),
    Critical(Color(0xFFFF6268)),
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
    returnChannelId: String? = null,
    onReturnToChannel: () -> Unit = {},
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
    var requestedQuality by remember(itemId, fileId) {
        mutableStateOf(vm.preferences.value.playbackQuality)
    }
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
    // Identity and outstanding destination belong to the presentation. A
    // quality change replaces both the plan and Controller, but not the viewer
    // or the seek that caused the replacement.
    val playbackIntent = remember(itemId, fileId) {
        PlaybackIntent(initialQuality = vm.preferences.value.playbackQuality)
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
                requestedQuality = requestedQuality,
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
                fault = preplayerStoppedFault("Couldn't start playback."),
                onAction = { action ->
                    if (action == SurfaceAction.Retry) {
                        startReason = "fallback"
                        generation++
                    } else {
                        onExit()
                    }
                },
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
                playbackIntent = playbackIntent,
                audioOffsetMs = playbackAudioOffset,
                onAudioOffsetChanged = { playbackAudioOffset = it },
                // The plan's own answer wins over the request that produced it:
                // `delivery.audio` is the index the server actually applied,
                // and the HLS session body has to repeat exactly that.
                retainedAudio = plan!!.deliveryAudio ?: playbackAudio,
                onAudioChanged = { playbackAudio = it },
                retainedSubtitle = playbackSubtitle,
                onSubtitleChanged = { playbackSubtitle = SubtitleChoice(it) },
                onReload = { position, reason, quality ->
                    resumeAt = position
                    startReason = reason
                    requestedQuality = quality
                    plan = null
                    generation++
                },
                onPlayNext = onPlayNext,
                onExit = onExit,
            )
        }
        if (returnChannelId != null) {
            TvButton(
                onClick = onReturnToChannel,
                modifier = Modifier.align(Alignment.TopEnd).padding(16.dp),
            ) { Text("Return to channel") }
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
 * A blocking surface the viewer has to answer: the `exhausted` prompt or a
 * `stopped` terminal (PLAYBACK-SURFACE-CONTRACT.md §3.1).
 *
 * Opaque, because the player behind it is stopped by the time this is drawn —
 * the recovery owner stops it before raising, and the presenter refuses to
 * render the fault at all otherwise. The transparent version of this view over
 * a stream that was still playing is the defect the contract exists to close.
 */
@Composable
private fun PlaybackFailed(
    fault: PlaybackFault,
    onAction: (SurfaceAction) -> Unit,
) {
    val firstFocusRequester = remember { FocusRequester() }
    val actions = surfaceActions(fault)
    RequestInitialFocus(firstFocusRequester, enabled = actions.isNotEmpty())
    Column(
        Modifier.fillMaxSize().background(Color.Black),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        fault.title?.let { title ->
            Text(title, color = Color.White, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.size(8.dp))
        }
        fault.detail?.let { detail -> Text(detail, color = Color.White) }
        Spacer(Modifier.size(12.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            actions.forEachIndexed { index, action ->
                TvButton(
                    onClick = { onAction(action) },
                    modifier = if (index == 0) {
                        Modifier.focusRequester(firstFocusRequester)
                    } else {
                        Modifier
                    },
                ) { Text(surfaceActionLabel(action)) }
            }
        }
    }
}

/**
 * The actions this client draws, from the ones the fault carries.
 *
 * `force_transcode` is the web's diagnosed-stall affordance (contract §3.3 row
 * 2): no Android owner ever puts it on a fault, and a button that does nothing
 * is worse than no button. Dropped HERE, once, rather than in each render —
 * this function is the single fact that makes it unreachable, which is what
 * [PlayerScreen]'s action handler points at instead of quietly swallowing one.
 */
private fun surfaceActions(fault: PlaybackFault): List<SurfaceAction> =
    fault.actions.filter { it != SurfaceAction.ForceTranscode }

/** Today's words for each action, moved rather than rewritten. */
private fun surfaceActionLabel(action: SurfaceAction): String = when (action) {
    SurfaceAction.KeepWaiting -> "Keep waiting"
    SurfaceAction.Retry -> "Retry"
    SurfaceAction.Close -> "Back"
    SurfaceAction.SignIn -> "Sign in"
    SurfaceAction.ForceTranscode -> "Force transcode"
}

/** The ledger's `surface_kind`. */
private fun surfaceKindLabel(surface: PlaybackSurface): String = when (surface) {
    is PlaybackSurface.None -> "None"
    is PlaybackSurface.Indicator -> "Indicator"
    is PlaybackSurface.Banner -> "Banner"
    is PlaybackSurface.Blocking -> "Blocking"
}

/**
 * The ledger's `surface_history`: newest first, so "what was that overlay" is
 * answered by the first entry.
 */
private fun surfaceHistoryLine(rows: List<SurfaceLedgerRow>): String? {
    if (rows.isEmpty()) return null
    return rows.asReversed().joinToString("  |  ") { row ->
        buildString {
            append(row.cls.wire).append(" · ").append(row.source)
            append(" · raised ").append(row.raisedAtMs).append(" ms → ")
            append(row.clearedAtMs?.let { "${it} ms (${row.clearedBy ?: "cleared"})" } ?: "open")
            if (row.disagreed) append(" · disagreement")
            append(" · rate ").append(row.playerAtRaise.rate)
            append(", ").append(row.playerAtRaise.positionMs).append(" ms")
            append(", presenting ").append(row.playerAtRaise.presenting)
            append(", stopped_by_owner ").append(row.playerAtRaise.stoppedByOwner)
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
    playbackIntent: PlaybackIntent,
    audioOffsetMs: Long,
    onAudioOffsetChanged: (Long) -> Unit,
    retainedAudio: Long?,
    onAudioChanged: (Long) -> Unit,
    retainedSubtitle: SubtitleChoice?,
    onSubtitleChanged: (Long?) -> Unit,
    onReload: (Long, String, PlaybackQuality) -> Unit,
    onPlayNext: (PlaybackTarget) -> Unit,
    onExit: () -> Unit,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val playbackLifecycleOwner = LocalLifecycleOwner.current
    val activity = androidx.activity.compose.LocalActivity.current
    val componentActivity = activity as? ComponentActivity
    val canUsePip = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
        context.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)
    val scope = rememberCoroutineScope()
    val displayModeMatcher = remember(activity) { activity?.let(::DisplayModeMatcher) }
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val controller = remember(plan) {
        // The decision and its session body must describe the same quality,
        // even if the stored preference changes between request and compose.
        playbackIntent.adoptQuality(plan.requestedQuality)
        Controller(
            context,
            buildPlayer(context, vm),
            plan,
            plan.legacyCaps,
            plan.decisionCaps,
            playbackIntent,
            vm,
            scope,
            displayModeMatcher = displayModeMatcher,
            displayModeMatchEnabled = Session.displayModeMatch,
            initialAudioOffsetMs = audioOffsetMs,
            retainedAudio = retainedAudio,
            retainedSubtitle = retainedSubtitle,
        )
    }
    // The one surface, projected from the player by the presenter.
    val collectedSurface by controller.surface.collectAsStateWithLifecycle()
    // A plain local, because a delegated property cannot be smart-cast.
    val surface: PlaybackSurface = collectedSurface
    // The input contract's `failed` state: a blocking surface with an answer
    // the viewer has to give. A full-screen progress surface covers pixels,
    // not routing (PLAYBACK-SURFACE-CONTRACT.md §4).
    val blockingFault = surface.fault?.takeIf { surface.entersFailedRouting }
    val bannerFault = (surface as? PlaybackSurface.Banner)?.fault
    // §3.1's progress rule: `preparing`/`buffering`/`recovering` are full-screen
    // while the attached picture is not presenting and an in-chrome indicator
    // while it is. Both halves are the waiting block below — the same spinner
    // this screen already draws for `isPlaybackWaiting`, which is the in-chrome
    // indicator by construction and covers the picture only because there is no
    // picture to cover when nothing is presenting.
    val progressFault = when (surface) {
        is PlaybackSurface.Indicator -> surface.fault
        is PlaybackSurface.Blocking -> surface.fault.takeUnless { surface.entersFailedRouting }
        is PlaybackSurface.Banner, is PlaybackSurface.None -> null
    }
    val surfaceFocusRequester = remember { FocusRequester() }
    // Which grades this panel can show. Probed once per playback: it is a
    // property of the cable, and this screen owns one HDMI route for its life.
    val displayHdrTypes = remember(context) { Caps.displayHdrTypes(context) }

    var positionMs by remember { mutableLongStateOf(startMs) }
    var pendingMs by remember(controller) { mutableStateOf<Long?>(null) }
    var timelineFocused by remember { mutableStateOf(false) }
    var isPlaying by remember(controller) { mutableStateOf(playbackIntent.playbackRequested) }
    var controlsVisible by remember { mutableStateOf(true) }
    // Height of the bottom control block as it was last laid out. The info
    // panel has to clear that block, but the block is a title, chips, a context
    // line, an overview and a transport row — its height is content, not a
    // constant — and it is only on screen while `controlsVisible`. Measuring it
    // is the only way to reserve the right amount, and zero the rest of the time.
    var transportHeightPx by remember { mutableIntStateOf(0) }
    var panel by remember { mutableStateOf<PlayerPanel?>(null) }
    var statsMode by remember(preferences.playbackInfoMode) {
        mutableStateOf(PlaybackStatsMode.fromStorage(preferences.playbackInfoMode))
    }
    var lastFocusedControl by rememberSaveable { mutableStateOf(PlayerControlId.PlayPause) }
    var panelOpener by rememberSaveable { mutableStateOf(PlayerControlId.PlayPause) }
    var focusAfterComposition by remember { mutableStateOf<PlayerControlId?>(null) }
    // Bumped by every focus() call. Two requests for the SAME control are two
    // moves — `focus_transport` from the timeline asks for the control it is
    // already remembering — so the request has to carry something that changes.
    var focusRequestTicket by remember { mutableIntStateOf(0) }
    // One surface for every producer on this screen — see OfflinePlayerScreen.
    val playerSurface = playerInputSurfaceFor(currentFormFactor())
    var repeatCount by remember { mutableIntStateOf(0) }
    val controlFocus = remember { PlayerControlFocus() }
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

    /**
     * The viewer answered a surface.
     *
     * The reducer clears the fault the action belonged to; the EFFECT is the
     * recovery owner's and this screen's. Shared by the blocking prompt and the
     * banner, so one action cannot come to mean two things.
     */
    fun onSurfaceAction(action: SurfaceAction) {
        controller.surfaceAction(action)
        when (action) {
            SurfaceAction.KeepWaiting -> controller.keepWaiting()
            // Re-issuing a failed change and retrying a spent ladder are the
            // same call: the reload carries the position and the quality the
            // viewer asked for, which is what the change was.
            SurfaceAction.Retry -> onReload(
                controller.prepareViewerRetry(),
                "fallback",
                playbackIntent.desiredQuality,
            )
            SurfaceAction.Close -> onExit()
            // The credential the server refused is the one this app is holding;
            // dropping it lands on the sign-in screen.
            SurfaceAction.SignIn -> {
                vm.logout()
                onExit()
            }
            // Unreachable: `surfaceActions` is the only thing that builds a
            // button, and it drops `force_transcode` — which no Android owner
            // raises in the first place. The arm exists because `when` over an
            // enum must be exhaustive, not because there is anything to do.
            SurfaceAction.ForceTranscode -> Unit
        }
    }

    fun inputState(): PlayerInputState = when {
        blockingFault != null -> PlayerInputState.Failed
        panel == PlayerPanel.Info && statsMode != PlaybackStatsMode.Mini -> PlayerInputState.Info
        panel != null && panel != PlayerPanel.Info -> PlayerInputState.Menu
        // Chrome-hidden outranks the timeline flag. `hide` removes Controls
        // from composition, and the flag is cleared by a focus callback the
        // runtime does not promise to deliver before the next key arrives —
        // so asking it first let a direction scrub chrome nobody could see,
        // which is the one thing ruling 1 forbids.
        !controlsVisible -> PlayerInputState.Hidden
        timelineFocused && pendingMs != null -> PlayerInputState.Scrub
        timelineFocused -> PlayerInputState.Timeline
        else -> PlayerInputState.Transport
    }

    fun focus(control: PlayerControlId) {
        // The marker chip is composed only while a marker is offered; asking
        // for it after the marker has passed is asking for a node that is not
        // there.
        val markerOnScreen = plan.markers.any { positionMs in it.start_ms until it.end_ms }
        focusAfterComposition = if (control == PlayerControlId.Marker && !markerOnScreen) {
            PlayerControlId.PlayPause
        } else {
            control
        }
        focusRequestTicket += 1
    }

    fun applyOutcome(outcome: PlayerInputOutcome, input: PlayerContractInput): Boolean {
        fun commitPreview() {
            pendingMs?.let(::seekWithMarkerUndo)
            pendingMs = null
            poke()
        }

        fun focusTransport() {
            val target = lastFocusedControl.takeIf { it.isTransport }
                ?: PlayerControlId.PlayPause
            controlsVisible = true
            focus(target)
        }

        fun focusMarkerOrIgnore() {
            if (plan.markers.any { positionMs in it.start_ms until it.end_ms }) {
                focus(PlayerControlId.Marker)
            }
        }

        return when (outcome) {
            PlayerInputOutcome.Reveal -> {
                controlsVisible = true
                poke()
                focus(lastFocusedControl)
                true
            }
            // These three want the focus engine or the button's own click, so
            // the event is deliberately not consumed.
            PlayerInputOutcome.FocusRow,
            PlayerInputOutcome.Activate,
            -> {
                poke()
                false
            }
            PlayerInputOutcome.MenuFocus -> false
            // `ignore` is "nothing happens", not "somebody else may act" — for
            // the media keys, which the Media3 session would otherwise act on,
            // seeking the player the contract just said to leave alone. A
            // directional or Select `ignore` stays unconsumed: on a touch
            // surface with a keyboard attached every direction is `ignore`,
            // and swallowing those would leave the focus engine with nothing.
            PlayerInputOutcome.Ignore -> input == PlayerContractInput.PlayPause ||
                input == PlayerContractInput.SkipBack ||
                input == PlayerContractInput.SkipForward
            PlayerInputOutcome.FocusMarkerOrIgnore -> {
                focusMarkerOrIgnore()
                true
            }
            PlayerInputOutcome.FocusTransport -> {
                focusTransport()
                true
            }
            PlayerInputOutcome.TogglePlay -> {
                controller.playPause()
                poke()
                true
            }
            PlayerInputOutcome.Skip -> {
                val delta = if (input == PlayerContractInput.SkipBack || input == PlayerContractInput.Left) {
                    -PlayerInputPolicy.SKIP_STEP_MS
                } else {
                    PlayerInputPolicy.SKIP_STEP_MS
                }
                seekWithMarkerUndo(controller.realPosition() + delta)
                poke()
                true
            }
            PlayerInputOutcome.Preview -> {
                // No trustworthy total means no position to preview against,
                // and no timeline row on screen either — the press is spent
                // waking the chrome, as it is on every other client.
                val ceiling = plan.durationMs
                if (ceiling > 0L) {
                    val direction = if (input == PlayerContractInput.Left) -1 else 1
                    pendingMs = ((pendingMs ?: controller.realPosition()) +
                        direction * PlayerInputPolicy.previewStepMs(repeatCount)).coerceIn(0L, ceiling)
                }
                poke()
                true
            }
            PlayerInputOutcome.Commit -> {
                commitPreview()
                true
            }
            PlayerInputOutcome.Cancel -> {
                pendingMs = null
                poke()
                true
            }
            PlayerInputOutcome.CancelThenFocusTransport -> {
                pendingMs = null
                focusTransport()
                poke()
                true
            }
            PlayerInputOutcome.CancelThenFocusMarkerOrIgnore -> {
                pendingMs = null
                focusMarkerOrIgnore()
                poke()
                true
            }
            PlayerInputOutcome.CommitThenTogglePlay -> {
                commitPreview()
                controller.playPause()
                true
            }
            PlayerInputOutcome.CloseMenu,
            PlayerInputOutcome.CloseInfo,
            -> {
                panel = null
                poke()
                focus(panelOpener)
                true
            }
            PlayerInputOutcome.Hide -> {
                if (panel == PlayerPanel.Info && statsMode == PlaybackStatsMode.Mini) {
                    panel = null
                }
                controlsVisible = false
                focusAfterComposition = null
                true
            }
            PlayerInputOutcome.Exit, PlayerInputOutcome.ReturnBrowser -> {
                onExit()
                true
            }
            PlayerInputOutcome.ToggleChrome -> {
                if (controlsVisible) controlsVisible = false else {
                    poke()
                    focus(lastFocusedControl)
                }
                true
            }
        }
    }

    BackHandler(enabled = !isInPip) {
        applyOutcome(
            PlayerInputPolicy.route(
                playerSurface,
                inputState(),
                PlayerContractInput.Back,
            ),
            PlayerContractInput.Back,
        )
    }

    // Keyed on the controller alone. Keying it on the preference too meant
    // toggling "Autoplay next episode" mid-play disposed the effect,
    // released the live player, and re-registered on the corpse — restarting
    // the episode at its original position. The listener reads the current
    // value instead of being rebuilt for it.
    val autoplayNext by rememberUpdatedState(preferences.autoplayNext)
    val playNext by rememberUpdatedState(onPlayNext)
    DisposableEffect(controller, playbackLifecycleOwner) {
        val lifecycle = playbackLifecycleOwner.lifecycle
        fun updateForeground() {
            controller.setPresentationForeground(lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED))
        }
        val observer = LifecycleEventObserver { _, _ -> updateForeground() }
        lifecycle.addObserver(observer)
        updateForeground()
        onDispose { lifecycle.removeObserver(observer) }
    }
    DisposableEffect(controller) {
        val listener = object : Player.Listener {
            override fun onIsPlayingChanged(playing: Boolean) {
                isPlaying = playing
                if (!playing) vm.postProgress(itemId, plan.globalPosition(controller.realPosition()), plan.progressDurationMs)
            }

            override fun onPlaybackStateChanged(state: Int) {
                // No screen-held copy of "the player is buffering": that is the
                // presenter's `media_waiting` now, and one of it is the point.
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
        // Registered through the controller, not on the player: a committed
        // prepared replacement swaps the ExoPlayer instance, and a listener
        // bound directly to the one that was current here would silently stop
        // firing at exactly the moment the screen most needs to hear from it.
        controller.addPlayerListener(listener)
        controller.startAt(startMs, startReason, attemptOpenedAtMs)
        onDispose {
            vm.postProgress(itemId, plan.globalPosition(controller.realPosition()), plan.progressDurationMs)
            controller.removePlayerListener(listener)
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
            if (pendingMs == null) positionMs = controller.realPosition()
            delay(500)
        }
    }
    LaunchedEffect(controller) {
        while (true) {
            delay(10_000)
            if (isPlaying) vm.reportProgress(itemId, plan.globalPosition(controller.realPosition()), plan.progressDurationMs)
        }
    }
    LaunchedEffect(lastInteraction, isPlaying, panel, pendingMs, blockingFault) {
        val miniInfo = panel == PlayerPanel.Info && statsMode == PlaybackStatsMode.Mini
        if (isPlaying && (panel == null || miniInfo) && pendingMs == null && blockingFault == null) {
            delay(PlayerInputPolicy.HIDE_AFTER_MS)
            applyOutcome(
                PlayerInputPolicy.route(
                    playerSurface,
                    inputState(),
                    PlayerContractInput.Idle,
                ),
                PlayerContractInput.Idle,
            )
        }
    }

    val activeMarker = plan.markers.firstOrNull { positionMs in it.start_ms until it.end_ms }
    LaunchedEffect(
        activeMarker?.kind,
        activeMarker?.start_ms,
        activeMarker?.isAutoSkipEligible,
        isInPip,
        pendingMs,
        preferences.autoSkip,
    ) {
        val marker = activeMarker ?: return@LaunchedEffect
        val automatic = preferences.autoSkip && marker.isAutoSkipEligible
        if (!isInPip && pendingMs == null && !automatic &&
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

    // `reveal` and `close_menu` re-enter Controls from scratch, and Controls
    // asks for its own initial focus a frame later — so a request made here
    // was overwritten every time. The target now travels into Controls as
    // `initialFocus` and there is one requester; this effect only parks focus
    // on the invisible surface when the chrome is gone.
    LaunchedEffect(controlsVisible, panel, isInPip) {
        if (!isInPip && !controlsVisible && panel == null) {
            surfaceFocusRequester.requestFocus()
        }
    }

    Box(
        Modifier.fillMaxSize()
            .focusRequester(surfaceFocusRequester)
            .focusable()
            .playerInputAdapter(surface = playerSurface, state = ::inputState) { outcome, input, repeats ->
                repeatCount = repeats
                applyOutcome(outcome, input)
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
            // `controller.player` is read here as well as in `factory` so a
            // committed prepared replacement re-attaches the surface to the
            // successor: the read makes this recompose when the instance
            // changes, and `PlayerView.setPlayer` moves the surface across.
            update = { view ->
                if (view.player !== controller.player) {
                    view.player = controller.player
                    // The surface has moved, so the player it moved off can go.
                    // This is the only place that knows that; the controller
                    // parks the predecessor and waits to be told.
                    controller.collectRetiredPlayer()
                }
                playerView = view
            },
            modifier = Modifier.fillMaxSize(),
        )

        if (!isInPip) {
            PGSBitmapOverlay(
                frame = controller.pgsOverlayFrame,
                videoAspectRatio = videoAspectRatio,
            )
        }

        if (!isInPip && blockingFault == null) {
            Box(
                Modifier.fillMaxSize().focusProperties { canFocus = false }.clickable(
                    interactionSource = remember { MutableInteractionSource() },
                    indication = null,
                ) {
                    val input = PlayerContractInput.TapSurface
                    applyOutcome(
                        PlayerInputPolicy.route(playerSurface, inputState(), input),
                        input,
                    )
                },
            )
        }

        // One owner for this pixel. `controller.isPlaybackWaiting` used to draw
        // here as well, which meant two things decided what covered the picture
        // and the `buffering` class could never be drawn on Android at all —
        // §2.3's defect wearing the contract's clothes. The wait now reaches
        // this block the same way every other reason does: as a fault the
        // presenter raised, debounced by its class and retired by the picture.
        if (!isInPip && (findingNext || progressFault != null)) {
            val waiting = playbackWaitPresentation(
                runwaySeconds = (controller.player.bufferedPosition - controller.player.currentPosition)
                    .coerceAtLeast(0) / 1_000.0,
                httpWaitCount = controller.sessionStatus?.http_wait_count,
            )
            // A typed fault says what the player is working on ("Reconnecting
            // on another server address."); `playbackWaitPresentation` only
            // knows that it is waiting. The fault wins when there is one.
            val title = when {
                findingNext -> "Up next…"
                progressFault?.detail != null -> progressFault.detail
                // A `client_preparing` or a `media_waiting` carries no sentence
                // of its own: the wait copy IS what this window said before the
                // presenter owned it, and moving text is not this change's job.
                else -> waiting.title
            }
            Column(Modifier.align(Alignment.Center), horizontalAlignment = Alignment.CenterHorizontally) {
                CircularProgressIndicator(color = Accent)
                Text(
                    title,
                    color = Color.White,
                    modifier = Modifier.padding(top = 12.dp),
                )
                // The runway and the server's wait count, as before. They were
                // drawn whenever the player was buffering, and a progress fault
                // is what that state is now: `preparing` while the stream is
                // opening, `buffering` once it has, `recovering` while the owner
                // is working on it. "Up next…" is the screen's own errand and
                // has no player runway to report.
                if (progressFault != null && !findingNext) {
                    Text(waiting.detail, color = Color.White.copy(alpha = 0.72f))
                }
            }
        }

        if (!isInPip && controlsVisible && blockingFault == null && activeMarker != null && pendingMs == null &&
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
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .padding(end = 28.dp, bottom = 112.dp)
                    .focusRequester(controlFocus.marker)
                    .onFocusChanged {
                        if (it.isFocused) lastFocusedControl = PlayerControlId.Marker
                    },
            ) { Text(activeMarker.displayLabel, fontWeight = FontWeight.SemiBold) }
        }

        val miniInfo = panel == PlayerPanel.Info && statsMode == PlaybackStatsMode.Mini
        if (!isInPip && controlsVisible && (panel == null || miniInfo) && blockingFault == null) {
            Controls(
                title = plan.title,
                subtitle = plan.subtitle,
                releaseDate = plan.releaseDate,
                runtimeLabel = plan.durationMs.takeIf { it > 0 }?.let(::playerRuntimeLabel),
                overview = plan.overview,
                positionMs = positionMs,
                durationMs = plan.durationMs,
                pendingMs = pendingMs,
                isPlaying = isPlaying,
                requestInitialFocus = panel == null,
                initialFocus = focusAfterComposition ?: PlayerControlId.PlayPause,
                focusRequestTicket = focusRequestTicket,
                focus = controlFocus,
                markerAvailable = activeMarker != null,
                lastFocusedControl = lastFocusedControl,
                onControlFocused = { control ->
                    lastFocusedControl = control
                    lastInteraction += 1
                },
                onTimelineFocused = { timelineFocused = it },
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
                    deliveredDolbyVisionProfile = controller.deliveredDolbyVisionProfile,
                ),
                onTransportHeight = { transportHeightPx = it },
                // The arrow is the `close` control, not the BACK key: `Back`
                // in `Transport` is `Hide`, so the arrow hid the chrome it was
                // drawn in. `closeSteps` closes what is open, then exits.
                onClose = {
                    PlayerInputPolicy.closeSteps(inputState()).forEach { outcome ->
                        applyOutcome(outcome, PlayerContractInput.Select)
                    }
                },
                onPlayPause = { controller.playPause(); poke() },
                onSeekBack = { controller.seekBy(-10_000); poke() },
                onSeekForward = { controller.seekBy(10_000); poke() },
                onScrub = { pendingMs = it.coerceIn(0L, plan.durationMs.coerceAtLeast(0L)) },
                onScrubEnd = {
                    pendingMs?.let(::seekWithMarkerUndo)
                    pendingMs = null
                    poke()
                },
                onTracks = {
                    panelOpener = PlayerControlId.Tracks
                    panel = PlayerPanel.Tracks
                },
                onSettings = {
                    panelOpener = PlayerControlId.Settings
                    panel = PlayerPanel.Settings
                },
                onInfo = {
                    panelOpener = PlayerControlId.Info
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

        when (if (isInPip || blockingFault != null) null else panel) {
            PlayerPanel.Tracks -> TrackMenu(
                player = controller.player,
                serverAudio = plan.audio,
                serverSubtitles = plan.subtitles,
                serverControlledAudio = controller.deliveryMode != "direct",
                selectedServerAudio = controller.selectedAudio,
                selectedServerSubtitle = controller.selectedSubtitle,
                onServerAudio = {
                    onAudioChanged(it)
                    controller.switchAudio(it)
                    panel = null
                    poke()
                    focus(panelOpener)
                },
                onServerSubtitle = {
                    if (controller.switchSubtitle(it)) onSubtitleChanged(it)
                    panel = null
                    poke()
                    focus(panelOpener)
                },
                onDismiss = {
                    panel = null
                    poke()
                    focus(panelOpener)
                },
            )
            PlayerPanel.Settings -> PlayerSettings(
                vm = vm,
                qualityOptions = qualityOptions(plan.ladder),
                audioOffsetMs = controller.audioOffsetMs,
                declaredOffsetMs = plan.declaredOffsetMs,
                currentPosition = controller::positionForPlaybackIntent,
                onReload = { _, reason, quality ->
                    // Publish the new rung on the old reporter and wait for the
                    // server to offer a successor. The position is deliberately
                    // not carried in: a rung change is not a seek, and a
                    // fallback samples the playhead when it actually reopens.
                    // The next controller inherits the same intent and identity.
                    controller.prepareReplacement(quality) { preparedPosition, preparedQuality ->
                        onReload(preparedPosition, reason, preparedQuality)
                    }
                },
                onAudioOffset = {
                    controller.setAudioOffset(it)
                    onAudioOffsetChanged(it)
                },
                onDismiss = {
                    panel = null
                    poke()
                    focus(panelOpener)
                },
            )
            PlayerPanel.Info -> PlayerInfo(
                plan = plan,
                controller = controller,
                positionMs = positionMs,
                displayHdrTypes = displayHdrTypes,
                transportReserve = playbackTransportReserve(controlsVisible, transportHeightPx),
                mode = statsMode,
                onMode = {
                    statsMode = it
                    vm.setPlaybackInfoMode(it.storageValue)
                },
                onDismiss = {
                    panel = null
                    poke()
                    focus(PlayerControlId.Info)
                },
            )
            null -> Unit
        }

        blockingFault?.let { fault ->
            PlaybackFailed(fault = fault, onAction = { action -> onSurfaceAction(action) })
        }

        bannerFault?.let { fault ->
            // The title first, because the one fault that has both is a
            // blocking surface the agreement rule demoted: "Playback recovered"
            // is the news, and the sentence under it is about the failure that
            // lost. The actions come with it — a failed change that offers no
            // Retry is the dead end this contract exists to remove.
            val notice = fault.title ?: fault.detail
            val actions = surfaceActions(fault)
            if (notice != null || actions.isNotEmpty()) {
                Column(
                    Modifier
                        .align(Alignment.TopCenter)
                        .padding(top = 24.dp, start = 40.dp, end = 40.dp)
                        .background(Color.Black.copy(alpha = 0.82f), MaterialTheme.shapes.medium)
                        .padding(horizontal = 14.dp, vertical = 10.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    if (notice != null) {
                        Text(
                            text = notice,
                            color = Color.White,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    if (actions.isNotEmpty()) {
                        Spacer(Modifier.size(8.dp))
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            actions.forEach { action ->
                                TvButton(onClick = { onSurfaceAction(action) }) {
                                    Text(surfaceActionLabel(action))
                                }
                            }
                        }
                    }
                }
            }
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
    pendingMs: Long? = null,
    isPlaying: Boolean,
    requestInitialFocus: Boolean,
    initialFocus: PlayerControlId = PlayerControlId.PlayPause,
    focusRequestTicket: Int = 0,
    mediaFacts: List<MediaFact> = emptyList(),
    markerAvailable: Boolean = false,
    focus: PlayerControlFocus? = null,
    lastFocusedControl: PlayerControlId = PlayerControlId.PlayPause,
    onControlFocused: (PlayerControlId) -> Unit = {},
    onTimelineFocused: (Boolean) -> Unit = {},
    onClose: () -> Unit,
    onPlayPause: () -> Unit,
    onSeekBack: () -> Unit,
    onSeekForward: () -> Unit,
    onScrub: (Long) -> Unit,
    onScrubEnd: () -> Unit,
    onTracks: (() -> Unit)?,
    onSettings: (() -> Unit)?,
    onInfo: () -> Unit,
    onPip: (() -> Unit)?,
    onTransportHeight: (Int) -> Unit = {},
) {
    val ownFocus = remember { PlayerControlFocus() }
    val resolvedFocus = focus ?: ownFocus
    val formFactor = currentFormFactor()
    RequestInitialFocus(
        resolvedFocus.requester(initialFocus),
        enabled = requestInitialFocus,
        token = focusRequestTicket,
        // Navigation, not arrival: a second request 80 ms later would undo a
        // press the viewer made in between.
        reinforce = false,
    )
    // The timeline row is composed only when a duration is known, so pointing
    // `up` at its requester on an unknown-duration stream aimed focus at a
    // node that was never attached.
    val upFromTransport = if (durationMs > 0L) resolvedFocus.timeline else FocusRequester.Cancel

    fun transportModifier(control: PlayerControlId): Modifier = Modifier
        .focusRequester(resolvedFocus.requester(control))
        .focusProperties { up = upFromTransport }
        .onFocusChanged { if (it.isFocused) onControlFocused(control) }

    @Composable
    fun TransportOptions() {
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (onTracks != null) {
                TvIconButton(
                    onClick = onTracks,
                    modifier = transportModifier(PlayerControlId.Tracks),
                ) {
                    Icon(Icons.Filled.ClosedCaption, contentDescription = "Audio and subtitles", tint = Color.White)
                }
            }
            if (onSettings != null) {
                TvIconButton(
                    onClick = onSettings,
                    modifier = transportModifier(PlayerControlId.Settings),
                ) {
                    Icon(Icons.Filled.Tune, contentDescription = "Playback settings", tint = Color.White)
                }
            }
            TvIconButton(
                onClick = onInfo,
                modifier = transportModifier(PlayerControlId.Info),
            ) {
                Icon(Icons.Filled.Info, contentDescription = "Playback info", tint = Color.White)
            }
            if (onPip != null) {
                TvIconButton(
                    onClick = onPip,
                    modifier = transportModifier(PlayerControlId.PictureInPicture),
                ) {
                    Icon(Icons.Filled.PictureInPictureAlt, contentDescription = "Picture in picture", tint = Color.White)
                }
            }
        }
    }

    Box(Modifier.fillMaxSize()) {
        Row(
            Modifier.align(Alignment.TopStart).fillMaxWidth().padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TvIconButton(
                onClick = onClose,
                modifier = Modifier.focusProperties {
                    canFocus = formFactor != FormFactor.Television
                },
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Close", tint = Color.White)
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

            if (durationMs > 0L) {
                TimelineRow(
                    positionMs = positionMs,
                    durationMs = durationMs,
                    pendingMs = pendingMs,
                    focusRequester = resolvedFocus.timeline,
                    upRequester = if (markerAvailable) resolvedFocus.marker else FocusRequester.Cancel,
                    downRequester = resolvedFocus.requester(
                        lastFocusedControl.takeIf { it.isTransport } ?: PlayerControlId.PlayPause,
                    ),
                    onFocusChanged = { focused ->
                        onTimelineFocused(focused)
                        if (focused) onControlFocused(PlayerControlId.Timeline)
                    },
                    onTouchPreview = onScrub,
                    onTouchCommit = onScrubEnd,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }

            BoxWithConstraints(Modifier.fillMaxWidth().padding(top = 4.dp)) {
                val splitTransport = formFactor == FormFactor.Compact && maxWidth < 700.dp
                if (!splitTransport) {
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        TransportButtons(
                            isPlaying = isPlaying,
                            timelineAbove = durationMs > 0L,
                            focus = resolvedFocus,
                            onFocused = onControlFocused,
                            onPlayPause = onPlayPause,
                            onSeekBack = onSeekBack,
                            onSeekForward = onSeekForward,
                        )
                        Spacer(Modifier.weight(1f))
                        TransportOptions()
                    }
                } else {
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center) {
                            TransportButtons(
                                isPlaying = isPlaying,
                                timelineAbove = durationMs > 0L,
                                focus = resolvedFocus,
                                onFocused = onControlFocused,
                                onPlayPause = onPlayPause,
                                onSeekBack = onSeekBack,
                                onSeekForward = onSeekForward,
                            )
                        }
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center) {
                            TransportOptions()
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
    timelineAbove: Boolean,
    focus: PlayerControlFocus,
    onFocused: (PlayerControlId) -> Unit,
    onPlayPause: () -> Unit,
    onSeekBack: () -> Unit,
    onSeekForward: () -> Unit,
) {
    fun modifier(control: PlayerControlId, size: Dp): Modifier = Modifier
        .size(size)
        .focusRequester(focus.requester(control))
        .focusProperties { up = if (timelineAbove) focus.timeline else FocusRequester.Cancel }
        .onFocusChanged { if (it.isFocused) onFocused(control) }

    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TvIconButton(onClick = onSeekBack, modifier = modifier(PlayerControlId.SkipBack, 48.dp)) {
            Icon(
                Icons.Filled.Replay10,
                contentDescription = "Back 10 seconds",
                tint = Color.White,
                modifier = Modifier.size(34.dp),
            )
        }
        TvIconButton(
            onClick = onPlayPause,
            modifier = modifier(PlayerControlId.PlayPause, 56.dp),
        ) {
            Icon(
                if (isPlaying) Icons.Filled.Pause else Icons.Filled.PlayArrow,
                contentDescription = if (isPlaying) "Pause" else "Play",
                tint = Color.White,
                modifier = Modifier.size(42.dp),
            )
        }
        TvIconButton(onClick = onSeekForward, modifier = modifier(PlayerControlId.SkipForward, 48.dp)) {
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
private fun PlayerSettings(
    vm: AppViewModel,
    qualityOptions: List<QualityOption>,
    audioOffsetMs: Long,
    declaredOffsetMs: Long?,
    currentPosition: () -> Long,
    onReload: (Long, String, PlaybackQuality) -> Unit,
    onAudioOffset: (Long) -> Unit,
    onDismiss: () -> Unit,
) {
    val preferences by vm.preferences.collectAsStateWithLifecycle()
    val initialFocusRequester = remember { FocusRequester() }
    var offset by remember(audioOffsetMs) { mutableLongStateOf(audioOffsetMs) }
    val focusIndex = playerSettingsFocusIndex(preferences.playbackQuality, qualityOptions)
    RequestInitialFocus(initialFocusRequester, enabled = qualityOptions.isNotEmpty())
    PlayerPanelSurface("Playback settings", onDismiss) {
        Text("Quality", color = Muted, style = MaterialTheme.typography.labelMedium)
        // The rungs are the server's, filtered to what this source can feed —
        // a 1080p file never offers to upscale itself to 4K.
        qualityOptions.forEachIndexed { index, option ->
            val quality = option.quality
            PanelRow(
                label = option.label,
                selected = preferences.playbackQuality == quality,
                modifier = if (index == focusIndex) {
                    Modifier.focusRequester(initialFocusRequester)
                } else {
                    Modifier
                },
            ) {
                val position = currentPosition()
                vm.setPlaybackQuality(quality)
                onReload(position, "quality", quality)
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
        PanelSwitch("Skip intros and credits", preferences.autoSkip, vm::setAutoSkip)
        PanelSwitch("Autoplay next episode", preferences.autoplayNext, vm::setAutoplayNext)
    }
}

internal fun playerSettingsFocusIndex(
    storedQuality: tv.plurx.app.data.PlaybackQuality,
    qualityOptions: List<QualityOption>,
): Int = qualityOptions.indexOfFirst { it.quality == storedQuality }.coerceAtLeast(0)

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
    // The ledger's SURFACE section.
    val surface by controller.surface.collectAsStateWithLifecycle()
    val surfaceFault = surface.fault
    // `surfaceRevision` is Compose state and moves on every ring change, which
    // the collected surface does not: a fault cleared underneath the one being
    // drawn changes the history and nothing else, and Playback debug has to
    // show that.
    val surfaceRevision = controller.surfaceRevision
    val surfaceHistory = remember(surfaceRevision) { controller.surfaceHistory }
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
    val videoFormat = player.videoFormat
    val source = plan.source
    val clientLoadedSeconds = (player.bufferedPosition - player.currentPosition).coerceAtLeast(0) / 1_000.0
    val method = buildList {
        add(deliveryLabel(controller.deliveryMode))
        if (controller.deliveryMode == "transcode") {
            controller.encoder?.takeIf { it.isNotBlank() }?.let(::add)
            controller.sessionStatus?.target_height?.takeIf { it > 0 }?.let { add("${it}p") }
        }
    }.joinToString(" · ")
    PlaybackInfoOverlay(
        details = PlaybackInfoDetails(
            title = plan.title,
            fileId = plan.fileId,
            delivery = method,
            position = "${formatTime(positionMs)} / ${formatTime(plan.durationMs)}",
            clientLoadedSeconds = clientLoadedSeconds,
            presentationAgeMs = controller.presentationProgressAgeMs,
            statusAgeMs = controller.sessionStatusAgeMs,
            videoHealth = videoHealthSummary(player),
            frames = videoFramesSummary(player),
            sourceFile = source?.filename,
            sourceVideo = sourceVideoCodecSummary(source),
            sourceResolution = if (source?.width != null && source.height != null) {
                "${source.width}×${source.height}"
            } else null,
            sourceBitrate = source?.bitrate?.takeIf { it > 0 }?.let(::formatBitrate),
            container = source?.container?.uppercase(),
            sourceAudio = sourceAudioSummary(source),
            playingVideo = videoFormatSummary(videoFormat),
            decodeResolution = player.videoSize.takeIf { it.width > 0 && it.height > 0 }
                ?.let { "${it.width}×${it.height}" },
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
            decoder = videoFormat?.codecs ?: videoFormat?.sampleMimeType,
            stalls = "${controller.playbackStallCount} (${controller.playbackStallCount} supply · 0 decode)",
            observedRate = controller.observedBitsPerSecond?.let(::formatBitrate),
            streamRate = videoFormat?.bitrate?.takeIf { it != Format.NO_VALUE && it > 0 }
                ?.toLong()?.let(::formatBitrate),
            startedIn = controller.lastTimeToFirstFrameMs?.let {
                String.format(Locale.US, "%.1f s", it / 1_000.0)
            },
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
            control = controller.currentSessionId?.let { "Client reporting active" },
            surfaceKind = surfaceKindLabel(surface),
            surfaceClass = surfaceFault?.cls?.wire,
            surfaceSource = surfaceFault?.source,
            surfaceIds = surfaceFault?.let { fault ->
                "${fault.attached} / ${fault.intent?.toString() ?: "—"}"
            },
            surfaceHistory = surfaceHistoryLine(surfaceHistory),
            preparedSwitch = controller.preparedSwitchReading,
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
    val preparedSwitch: PreparedSwitchReading = PreparedSwitchReading.unmeasured,
    val title: String,
    val fileId: Long,
    val delivery: String,
    val position: String,
    val clientLoadedSeconds: Double,
    val presentationAgeMs: Long? = null,
    val statusAgeMs: Long? = null,
    val videoHealth: String? = null,
    val frames: String? = null,
    val sourceFile: String? = null,
    val sourceVideo: String? = null,
    val sourceResolution: String? = null,
    val sourceBitrate: String? = null,
    val container: String? = null,
    val sourceAudio: String? = null,
    val playingVideo: String? = null,
    val decodeResolution: String? = null,
    val playingAudio: String? = null,
    val dynamicRange: String? = null,
    val subtitles: String = "Off",
    val decoder: String? = null,
    val stalls: String? = null,
    val observedRate: String? = null,
    val streamRate: String? = null,
    val startedIn: String? = null,
    val encoder: String? = null,
    val audioSync: String? = null,
    val build: String = "development",
    val transport: String = "Android Media3",
    val sessionId: String? = null,
    val sessionStatus: PlaybackSessionStatus? = null,
    val playerState: String = "Unknown",
    val control: String? = null,
    /** SURFACE: what is drawn over the picture, and why (contract §5). */
    val surfaceKind: String = "None",
    val surfaceClass: String? = null,
    val surfaceSource: String? = null,
    val surfaceIds: String? = null,
    val surfaceHistory: String? = null,
)

internal data class PlaybackWaitPresentation(
    val title: String,
    val detail: String,
)

/** Visible waiting copy that keeps client runway distinct from server response waits. */
internal fun playbackWaitPresentation(
    runwaySeconds: Double,
    httpWaitCount: Long?,
): PlaybackWaitPresentation {
    val waits = httpWaitCount?.coerceAtLeast(0)
    val waitText = when (waits) {
        null -> "server wait state unavailable"
        0L -> "no server HTTP waits"
        1L -> "1 server HTTP wait"
        else -> "$waits server HTTP waits"
    }
    return PlaybackWaitPresentation(
        title = "Presentation waiting",
        detail = String.format(Locale.US, "%.1f s client loaded · %s", runwaySeconds, waitText),
    )
}

/** Bounded overlay: controls stay outside the scrolling content on every device. */
@Composable
internal fun PlaybackInfoOverlay(
    details: PlaybackInfoDetails,
    reasons: List<String>,
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
    onDismiss: () -> Unit,
    transportReserve: Dp = 0.dp,
) {
    BoxWithConstraints(
        Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)
            .then(if (mode == PlaybackStatsMode.Mini) Modifier else Modifier.clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, onClick = onDismiss))
            .padding(PlaybackOverlayInset),
    ) {
        val panelHeight = (maxHeight - transportReserve).coerceAtLeast(minOf(maxHeight, PlaybackPanelMinHeight))
        val facts = playbackInfoRows(details, reasons).mapNotNull { row ->
            row.value?.let { value ->
                PlaybackInfoFact(row.id, row.label, value,
                    listOfNotNull(playbackInfoExplanation(row.id), row.note).joinToString(" ").ifEmpty { null },
                    playbackInfoGroup(row.section), PlaybackStatsMode.Standard !in row.modes)
            }
        }
        PlaybackInfoPanel(
            title = details.title, facts = facts, mode = mode, onMode = onMode, onClose = onDismiss,
            modifier = Modifier.align(Alignment.TopEnd).widthIn(max = 900.dp).fillMaxWidth().heightIn(max = panelHeight)
                .clickable(interactionSource = remember { MutableInteractionSource() }, indication = null, onClick = {}),
        )
    }
}

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

internal data class InfoRow(
    val id: String,
    val label: String,
    val section: String,
    val modes: Set<PlaybackStatsMode>,
    val value: String?,
    val note: String? = null,
    val placement: String = "grid",
    val tone: PlaybackStatTone = PlaybackStatTone.Neutral,
)

private val StandardAndDebug = setOf(PlaybackStatsMode.Standard, PlaybackStatsMode.Details, PlaybackStatsMode.Debug)
private val AllInfoModes = PlaybackStatsMode.entries.toSet()

/** Android's fixture-order row model. Platform-exclusive web/Apple fields are absent. */
internal fun playbackInfoRows(
    details: PlaybackInfoDetails,
    reasons: List<String>,
): List<InfoRow> {
    val status = details.sessionStatus
    val speed = status?.recent_speed ?: status?.speed
    val subtitleParts = splitLedgerValue(details.subtitles, facts = 1)
    val fetchReserveNote = buildList {
        if (status?.suspended == true) add("Holding buffer")
        status?.resume_below_seconds?.let { add("releases below $it s") }
        status?.suspend_count?.let { add("$it suspends") }
    }.takeIf { it.isNotEmpty() }?.joinToString(" · ")
    return listOf(
        InfoRow("method", "Method", "PLAYBACK", StandardAndDebug, details.delivery),
        InfoRow("position", "Position", "PLAYBACK", StandardAndDebug, details.position),
        InfoRow(
            "reason",
            "Reason",
            "PLAYBACK",
            StandardAndDebug,
            reasons.takeIf { it.isNotEmpty() }?.joinToString(" · "),
            placement = "notes",
            tone = PlaybackStatTone.Muted,
        ),
        InfoRow("build", "Build", "PLAYBACK", setOf(PlaybackStatsMode.Debug), details.build.ifBlank { "—" }),
        InfoRow("transport", "Transport", "PLAYBACK", setOf(PlaybackStatsMode.Debug), details.transport, placement = "notes"),
        InfoRow("file_id", "File ID", "PLAYBACK", setOf(PlaybackStatsMode.Debug), "#${details.fileId}"),
        InfoRow("session", "Session", "PLAYBACK", setOf(PlaybackStatsMode.Debug), details.sessionId, placement = "notes"),
        InfoRow("source_video", "Original video", "SOURCE", StandardAndDebug, details.sourceVideo, placement = "notes"),
        InfoRow("source_resolution", "Original resolution", "SOURCE", StandardAndDebug, details.sourceResolution),
        InfoRow("source_bitrate", "Source bitrate", "SOURCE", StandardAndDebug, details.sourceBitrate),
        InfoRow("container", "Container", "SOURCE", StandardAndDebug, details.container),
        InfoRow("source_audio", "Source audio track", "SOURCE", StandardAndDebug, details.sourceAudio, placement = "notes"),
        InfoRow("source_file", "File", "SOURCE", setOf(PlaybackStatsMode.Debug), details.sourceFile, placement = "notes"),
        InfoRow("av_offset", "AV offset", "SOURCE", setOf(PlaybackStatsMode.Debug), details.audioSync ?: "0 ms"),
        InfoRow("decode_resolution", "Playing resolution", "NOW DECODING", AllInfoModes, details.decodeResolution ?: "Not reported"),
        InfoRow("stream_format", "Stream format", "NOW DECODING", StandardAndDebug, details.playingVideo ?: "Not reported"),
        InfoRow("device_audio", "Device audio output", "NOW DECODING", StandardAndDebug, "Not reported"),
        InfoRow("dynamic_range", "Dynamic range", "NOW DECODING", StandardAndDebug, details.dynamicRange, placement = "notes"),
        InfoRow("decode_audio", "Stream audio track", "NOW DECODING", StandardAndDebug, details.playingAudio, placement = "notes"),
        InfoRow("frames", "Frames", "NOW DECODING", StandardAndDebug, details.frames, tone = videoHealthTone(details.videoHealth)),
        InfoRow(
            "player_state",
            "Player state",
            "NOW DECODING",
            AllInfoModes,
            details.playerState,
            tone = playerStateTone(details.playerState),
        ),
        InfoRow("decoder", "Decoder", "NOW DECODING", setOf(PlaybackStatsMode.Debug), details.decoder),
        InfoRow("stalls", "Buffering interruptions", "NOW DECODING", StandardAndDebug, details.stalls),
        InfoRow("subtitles", "Subtitles", "NOW DECODING", StandardAndDebug, subtitleParts.first, subtitleParts.second),
        InfoRow(
            "source_read",
            "Source read",
            "BUFFERING / DELIVERY",
            setOf(PlaybackStatsMode.Debug),
            "Unavailable",
            tone = PlaybackStatTone.Muted,
        ),
        InfoRow(
            "server_ready",
            "Ready on server",
            "BUFFERING / DELIVERY",
            StandardAndDebug,
            serverReadyValue(status),
            tone = serverReadyTone(status),
        ),
        InfoRow(
            "ready_state",
            "Ready state",
            "BUFFERING / DELIVERY",
            setOf(PlaybackStatsMode.Debug),
            status?.server_ready_state?.replaceFirstChar { it.uppercase() } ?: "Unavailable",
            tone = serverReadyTone(status),
        ),
        InfoRow(
            "ready_anchor",
            "Ready anchor",
            "BUFFERING / DELIVERY",
            setOf(PlaybackStatsMode.Debug),
            status?.server_ready_anchor_ms?.let { "$it ms" },
        ),
        InfoRow(
            "ready_end",
            "Ready end",
            "BUFFERING / DELIVERY",
            setOf(PlaybackStatsMode.Debug),
            status?.server_ready_end_ms?.let { "$it ms" },
        ),
        InfoRow(
            "later_ready",
            "Later ready",
            "BUFFERING / DELIVERY",
            setOf(PlaybackStatsMode.Debug),
            laterReadyValue(status),
            placement = "notes",
        ),
        InfoRow(
            "http_wait",
            "HTTP wait",
            "BUFFERING / DELIVERY",
            StandardAndDebug,
            status?.http_wait_count?.let(::httpWaitValue),
            note = httpWaitNote(status),
            tone = httpWaitTone(status?.http_wait_count),
        ),
        InfoRow(
            "client_loaded",
            "Buffered on device",
            "BUFFERING / DELIVERY",
            AllInfoModes,
            formatSeconds(details.clientLoadedSeconds),
            tone = bufferTone(details.clientLoadedSeconds),
        ),
        InfoRow(
            "presentation",
            "Presentation",
            "BUFFERING / DELIVERY",
            StandardAndDebug,
            presentationValue(details.playerState),
            tone = playerStateTone(details.playerState),
        ),
        InfoRow(
            "presentation_age",
            "Last advance",
            "BUFFERING / DELIVERY",
            StandardAndDebug,
            details.presentationAgeMs?.let { "$it ms" },
        ),
        InfoRow(
            "delivery_rate",
            "Server response rate",
            "BUFFERING / DELIVERY",
            StandardAndDebug,
            status?.delivered_bps?.let(::formatBitrate),
            note = status?.delivered_idle_ms?.takeIf { it > 0 }?.let { "idle for $it ms" },
            tone = networkTone(details),
        ),
        InfoRow("delivered", "Server responses completed", "BUFFERING / DELIVERY", StandardAndDebug, status?.delivered_bytes?.let(::formatBytes)),
        InfoRow(
            "delivery_idle",
            "Delivery idle",
            "BUFFERING / DELIVERY",
            setOf(PlaybackStatsMode.Debug),
            status?.delivered_idle_ms?.let { "$it ms" },
            tone = idleTone(status?.delivered_idle_ms, status?.suspended == true),
        ),
        InfoRow(
            "status_age",
            "Status sample age",
            "BUFFERING / DELIVERY",
            setOf(PlaybackStatsMode.Debug),
            details.statusAgeMs?.let { "$it ms" },
        ),
        InfoRow("observed_rate", "Observed download rate", "NETWORK", StandardAndDebug, details.observedRate),
        InfoRow("stream_rate", "Stream rate", "NETWORK", StandardAndDebug, details.streamRate),
        InfoRow("started_in", "Started in", "NETWORK", setOf(PlaybackStatsMode.Debug), details.startedIn),
        InfoRow(
            "status",
            "Server state",
            "SERVER",
            StandardAndDebug,
            when {
                status == null -> "No server-side session"
                status.suspended == true -> "Holding buffer"
                else -> "Active"
            },
            tone = if (status == null) PlaybackStatTone.Muted else PlaybackStatTone.Good,
        ),
        InfoRow("encoder", "Encoder", "SERVER", StandardAndDebug, status?.encoder ?: details.encoder),
        InfoRow(
            "encode_speed",
            "Encode speed",
            "SERVER",
            StandardAndDebug,
            speed?.let { String.format(Locale.US, "%.2f×", it) },
            note = if (status?.recent_speed == null && status?.speed != null) "average" else null,
            tone = speed?.let { encodeTone(it, status) } ?: PlaybackStatTone.Muted,
        ),
        InfoRow(
            "production_actual",
            "Production actual",
            "SERVER",
            StandardAndDebug,
            status?.production_ahead_seconds?.let { formatSeconds(it.toDouble()) },
            tone = bufferTone(status?.production_ahead_seconds?.toDouble(), status?.suspended == true),
        ),
        InfoRow(
            "production_target",
            "Production target",
            "SERVER",
            StandardAndDebug,
            status?.production_target_seconds?.let { formatSeconds(it.toDouble()) },
            note = status?.production_policy,
        ),
        InfoRow("producer_state", "Producer", "SERVER", StandardAndDebug, status?.producer_state),
        InfoRow(
            "fetch_reserve",
            "Fetch reserve",
            "SERVER",
            setOf(PlaybackStatsMode.Debug),
            status?.ahead_seconds?.let { formatSeconds(it.coerceAtLeast(0).toDouble()) },
            note = fetchReserveNote,
            tone = bufferTone(status?.ahead_seconds?.toDouble(), status?.suspended == true),
        ),
        InfoRow("ahead_bytes", "Fetch reserve bytes", "SERVER", setOf(PlaybackStatsMode.Debug), status?.ahead_bytes?.let(::formatBytes)),
        InfoRow("produced", "Produced", "SERVER", setOf(PlaybackStatsMode.Debug), status?.out_time_ms?.let(::formatTime)),
        InfoRow("pacing", "Pacing", "SERVER", setOf(PlaybackStatsMode.Debug), status?.readrate?.let { String.format(Locale.US, "%.2f×", it) }),
        InfoRow("held", "Held", "SERVER", setOf(PlaybackStatsMode.Debug), status?.let { if (it.suspended == true) "Yes" else "No" }),
        InfoRow("hold_reason", "Hold reason", "SERVER", setOf(PlaybackStatsMode.Debug), status?.hold_reason),
        InfoRow("suspend_count", "Suspend count", "SERVER", setOf(PlaybackStatsMode.Debug), status?.suspend_count?.toString()),
        InfoRow("request_idle", "Request idle", "SERVER", setOf(PlaybackStatsMode.Debug), status?.idle_seconds?.let { String.format(Locale.US, "%.1f s", it.toDouble()) }),
        InfoRow("last_request", "Last request", "SERVER", setOf(PlaybackStatsMode.Debug), status?.last_request, placement = "notes"),
        InfoRow("playlist", "Playlist", "SERVER", setOf(PlaybackStatsMode.Debug), status?.playlist_shape),
        InfoRow("published_end", "Published end", "SERVER", setOf(PlaybackStatsMode.Debug), status?.published_end_ms?.let { "$it ms" }),
        InfoRow("fetched_end", "Fetched end", "SERVER", setOf(PlaybackStatsMode.Debug), status?.fetched_end_ms?.let { "$it ms" }),
        InfoRow("control", "Control", "SERVER", StandardAndDebug, details.control),
        InfoRow("surface_kind", "Surface", "SURFACE", StandardAndDebug, details.surfaceKind),
        InfoRow("surface_class", "Fault", "SURFACE", StandardAndDebug, details.surfaceClass),
        InfoRow(
            "surface_source",
            "Source",
            "SURFACE",
            setOf(PlaybackStatsMode.Debug),
            details.surfaceSource,
        ),
        InfoRow(
            "surface_ids",
            "Attached/intent",
            "SURFACE",
            setOf(PlaybackStatsMode.Debug),
            details.surfaceIds,
        ),
        InfoRow(
            "surface_history",
            "History",
            "SURFACE",
            setOf(PlaybackStatsMode.Debug),
            details.surfaceHistory,
            placement = "notes",
        ),
        // PREPARED SWITCH (M3). Debug only, and always a string: "Not measured"
        // and "0 dropped" are different facts and the row has to be able to say
        // which. Nothing on these rows is read back by anything.
        InfoRow(
            "switch_frames",
            "Frames at the switch",
            "PREPARED SWITCH",
            setOf(PlaybackStatsMode.Debug),
            details.preparedSwitch.frames,
        ),
        InfoRow(
            "switch_audio",
            "Audio at the switch",
            "PREPARED SWITCH",
            setOf(PlaybackStatsMode.Debug),
            details.preparedSwitch.audio,
        ),
        InfoRow(
            "switch_visible_in",
            "Tap to new quality",
            "PREPARED SWITCH",
            setOf(PlaybackStatsMode.Debug),
            details.preparedSwitch.visibleIn,
        ),
    )
}

private fun videoHealthTone(summary: String?): PlaybackStatTone = when {
    summary == null -> PlaybackStatTone.Muted
    summary.contains(" 0 dropped") -> PlaybackStatTone.Good
    summary.contains(" max streak 1") -> PlaybackStatTone.Warning
    else -> PlaybackStatTone.Critical
}

private fun bufferTone(
    seconds: Double?,
    suspended: Boolean = false,
): PlaybackStatTone = when {
    suspended -> PlaybackStatTone.Good
    seconds == null -> PlaybackStatTone.Muted
    seconds < 2 -> PlaybackStatTone.Critical
    seconds < 5 -> PlaybackStatTone.Warning
    else -> PlaybackStatTone.Good
}

private fun formatSeconds(seconds: Double): String =
    String.format(Locale.US, "%.1f s", seconds.coerceAtLeast(0.0))

private fun serverReadyValue(status: PlaybackSessionStatus?): String = when (
    status?.server_ready_state?.lowercase(Locale.US)
) {
    "ready" -> status.server_ready_seconds?.let(::formatSeconds) ?: "Unavailable"
    "missing" -> "0.0 s"
    else -> "Unavailable"
}

private fun serverReadyTone(status: PlaybackSessionStatus?): PlaybackStatTone = when (
    status?.server_ready_state?.lowercase(Locale.US)
) {
    "ready" -> bufferTone(status.server_ready_seconds)
    "missing" -> PlaybackStatTone.Critical
    else -> PlaybackStatTone.Muted
}

private fun laterReadyValue(status: PlaybackSessionStatus?): String? {
    val start = status?.server_next_ready_start_ms ?: return null
    val end = status.server_next_ready_end_ms ?: return null
    return "$start–$end ms (not current runway)"
}

private fun httpWaitValue(count: Long): String = when (val waits = count.coerceAtLeast(0)) {
    0L -> "0 active"
    1L -> "1 active"
    else -> "$waits active"
}

private fun httpWaitNote(status: PlaybackSessionStatus?): String? = buildList {
    status?.http_wait_oldest_ms?.let { add("oldest $it ms") }
    status?.http_wait_segment?.let { add("segment $it") }
}.takeIf { it.isNotEmpty() }?.joinToString(" · ")

private fun httpWaitTone(count: Long?): PlaybackStatTone = when {
    count == null -> PlaybackStatTone.Muted
    count > 0 -> PlaybackStatTone.Warning
    else -> PlaybackStatTone.Good
}

private fun presentationValue(playerState: String): String = when (playerState) {
    "Playing" -> "Advancing"
    "Buffering" -> "Waiting"
    else -> playerState
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
    "remux" -> "Remux"
    "transcode" -> "Transcode"
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

private fun sourceVideoCodecSummary(file: MediaFileDto?): String? {
    if (file == null) return null
    return listOfNotNull(
        file.video_codec?.uppercase(),
        file.video_profile?.takeIf { it.isNotBlank() },
        file.bit_depth?.takeIf { it > 0 }?.let { "${it}-bit" },
        (file.hdr_format ?: file.hdr)?.takeIf { it.isNotBlank() },
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

internal fun formatBitrate(bitsPerSecond: Long): String = when {
    bitsPerSecond >= 10_000_000 -> String.format(Locale.US, "%.0f Mb/s", bitsPerSecond / 1_000_000.0)
    bitsPerSecond >= 1_000_000 -> String.format(Locale.US, "%.1f Mb/s", bitsPerSecond / 1_000_000.0)
    else -> String.format(Locale.US, "%.0f kb/s", bitsPerSecond / 1_000.0)
}

private fun formatBytes(bytes: Long): String {
    val value = bytes.coerceAtLeast(0)
    return when {
        value >= 1_000_000_000 -> String.format(Locale.US, "%.1f GB", value / 1_000_000_000.0)
        value >= 1_000_000 -> String.format(Locale.US, "%.1f MB", value / 1_000_000.0)
        value >= 1_000 -> String.format(Locale.US, "%.1f KB", value / 1_000.0)
        else -> "$value B"
    }
}

internal fun playerStateLabel(player: Player): String = when (player.playbackState) {
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

private fun videoFramesSummary(player: ExoPlayer): String? {
    val counters = player.videoDecoderCounters ?: return null
    counters.ensureUpdated()
    val dropped = counters.droppedBufferCount.coerceAtLeast(0)
    val total = dropped.toLong() + counters.renderedOutputBufferCount.coerceAtLeast(0)
    fun count(value: Long): String = String.format(Locale.US, "%,d", value).replace(',', ' ')
    return "${count(dropped.toLong())} / ${count(total)} frames"
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
                .focusGroup()
                .focusProperties { onExit = { cancelFocusChange() } }
                .clickable(onClick = {}),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(title, style = MaterialTheme.typography.titleLarge)
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
