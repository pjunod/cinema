@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@file:kotlin.OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)

package tv.plurx.app.player

import android.content.Context
import android.content.res.Configuration
import android.net.Uri
import android.util.Log
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusGroup
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewModelScope
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.ForwardingPlayer
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.TrackGroup
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.TransferListener
import androidx.media3.exoplayer.source.LoadEventInfo
import androidx.media3.exoplayer.source.MediaLoadData
import androidx.media3.datasource.HttpDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.hls.HlsManifest
import androidx.media3.exoplayer.analytics.AnalyticsListener
import androidx.media3.exoplayer.audio.AudioSink
import androidx.media3.session.MediaSession
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.Job
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import tv.plurx.app.data.Caps
import tv.plurx.app.data.HlsStart
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.DeviceCaps
import tv.plurx.app.data.AudioTrack
import tv.plurx.app.data.AudioOutputRoute
import tv.plurx.app.data.SubTrack
import tv.plurx.app.data.SubtitleReadiness
import tv.plurx.app.data.Net
import tv.plurx.app.data.RefusalException
import tv.plurx.app.data.refusalStatusOf
import tv.plurx.app.data.PlaybackSessionStatus
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.Session
import tv.plurx.app.ui.AppViewModel
import tv.plurx.app.ui.theme.Accent
import tv.plurx.app.ui.theme.Muted
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.components.RequestInitialFocus
import kotlinx.serialization.json.JsonObject
import java.util.Locale
import java.util.UUID

/**
 * A subtitle selection the viewer actually made, where `null` inside means
 * "Off". Distinct from no wrapper at all, which means nobody has chosen yet
 * and the server's own pick still applies.
 */
@JvmInline
value class SubtitleChoice(val index: Long?)

/**
 * Bridges plurx's delivery plan to one ExoPlayer, executing the mode the
 * server chose instead of re-deriving policy from the verdict:
 *  - direct → the original file over HTTP range; ExoPlayer seeks natively.
 *  - remux → `stream.mp4?start=…`, a live fast-seek remux that can't be
 *    range-sought, so a seek re-requests the stream at the new offset.
 *  - transcode → an HLS session (this used to go through `stream.mp4` too,
 *    which never re-encodes video — a tone-map or downscale verdict shipped
 *    the copied source anyway). A seek opens a session at the new offset,
 *    like the web player, and the old one is released with a DELETE rather
 *    than left to the server's idle reaper — unless the session is `vod`, in
 *    which case the whole stream is already on disk and the player just seeks.
 *
 * There is exactly one piece of mutable transport state here, and it is
 * [subtitleDelivery]: where the current subtitle selection is being carried
 * (`SubtitlePolicy.kt`). Everything else — direct or session, copy or
 * re-encode, what the info panel calls it — is *derived* from that plus
 * [planMode], so two answers about the same stream cannot drift apart. A
 * subtitle also opens a session on remux/transcode plans, but a *copy* one
 * advertising HLS text renditions: the video recipe never changes, so an SRT
 * on a 4K HDR remux costs neither an encoder slot nor its HDR.
 *
 * Either way [realPosition] reports the true timeline position (base + player
 * pos), which is what gets scrobbled.
 */
@UnstableApi
class Controller internal constructor(
    private val context: Context,
    builtPlayer: BuiltPlayer,
    private val plan: PlanLike,
    private val caps: Map<String, String>,
    private val decisionCaps: DeviceCaps,
    private val playbackIntent: PlaybackIntent,
    private val vm: AppViewModel,
    private val scope: CoroutineScope,
    private val displayModeMatcher: DisplayModeMatcher? = null,
    private val displayModeMatchEnabled: Boolean = false,
    initialAudioOffsetMs: Long = 0,
    retainedAudio: Long? = null,
    retainedSubtitle: SubtitleChoice? = null,
    private val replan: (Long, String, PlaybackQuality) -> Unit,
) {
    /**
     * The authoritative player — the one on the surface, with the volume up.
     *
     * Observable rather than final because a committed prepared replacement
     * swaps it: the successor was primed on its own pipeline and becomes the
     * incumbent in one step. Everything that reads `controller.player` from a
     * composition therefore re-reads it after a commit, and everything that
     * *registers* against it goes through [addPlayerListener] so the
     * registration moves with it.
     */
    var player: ExoPlayer by mutableStateOf(builtPlayer.player)
        private set

    /**
     * Listeners the screen owns, tracked here so a commit can move them.
     *
     * `player.addListener` from outside this class would bind to whichever
     * instance was current at the time and silently stop firing after a swap.
     */
    private val externalListeners = mutableListOf<Player.Listener>()

    fun addPlayerListener(value: Player.Listener) {
        externalListeners += value
        player.addListener(value)
    }

    fun removePlayerListener(value: Player.Listener) {
        externalListeners -= value
        player.removeListener(value)
    }

    /**
     * M3. What the last prepared switch in this player measured.
     *
     * Observation only. Every input is a listener callback Media3 already
     * dispatches, every method is a bounded list append, and nothing in this
     * controller reads a single value back to decide anything — so
     * instrumenting the switch cannot be the thing that changes it. The
     * arithmetic lives in `PreparedSwitchMeasurement.kt` as pure functions.
     */
    private val preparedSwitch = PreparedSwitchRecorder()

    /**
     * The three rows the info panel shows, computed on demand. A closed panel
     * costs nothing, and the panel cannot change what it is reading.
     */
    internal val preparedSwitchReading: PreparedSwitchReading get() = preparedSwitch.reading()

    /**
     * The analytics listener each pipeline gets, tagged with that pipeline's
     * identity so a second directed change cannot file the first switch's
     * successor on the wrong side of the second one.
     *
     * `onDroppedVideoFrames` is an increment and `onAudioUnderrun` an event;
     * the recorder knows the difference. Attached for the life of the player
     * rather than removed on a timer: a listener is cheaper than a clock, and
     * a clock on the commit path is precisely what must not be added.
     */
    private fun preparedSwitchAnalytics(pipeline: ExoPlayer): AnalyticsListener =
        object : AnalyticsListener {
            private val tag = System.identityHashCode(pipeline)
            private var cumulativeDropped = 0L

            override fun onDroppedVideoFrames(
                eventTime: AnalyticsListener.EventTime,
                droppedFrames: Int,
                elapsedMs: Long,
            ) {
                preparedSwitch.noteDroppedFrames(monotonicNowMs(), droppedFrames, tag)
                if (droppedFrames < 0 || cumulativeDropped > Long.MAX_VALUE - droppedFrames) return
                cumulativeDropped += droppedFrames
                if (autoDecodePressure.observe(pipeline, autoActiveCandidateId,
                        monotonicNowMs(), realPosition(), cumulativeDropped,
                        pipeline === player && establishedPlayback && presentationForeground && player.isPlaying &&
                            playbackIntent.pendingSeek == null && directedChange == null &&
                            player.bufferedPosition - player.currentPosition >= 10_000L)) {
                    val evidence = autoDecodePressure.evidence
                    scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) {
                        if (noteAutoDecodeFailure(pressure = evidence)) restartAt(realPosition(), "decode-quality")
                    }
                }
            }

            override fun onAudioUnderrun(
                eventTime: AnalyticsListener.EventTime,
                bufferSize: Int,
                bufferSizeMs: Long,
                elapsedSinceLastFeedMs: Long,
            ) {
                preparedSwitch.noteAudioUnderrun(monotonicNowMs(), tag)
            }
        }

    /** Immutable quality of this exact decision; pending intent is separate. */
    private val activeQuality = plan.requestedQuality
    private val planReplacement = PlaybackPlanReplacement(activeQuality)

    private var progressiveMediaOrigin = builtPlayer.progressiveMediaOrigin
    private val autoTransfersByPlayer = java.util.WeakHashMap<ExoPlayer, AutoTransferEvidence>().apply {
        put(builtPlayer.player, builtPlayer.autoTransfers)
    }
    internal val latestAutoCompletedTransfer: AutoCompletedTransfer?
        get() = autoTransfersByPlayer[player]?.latest()
    val observedBitsPerSecond: Long? get() = progressiveMediaOrigin.currentObservedBitsPerSecond()

    var audioOffsetMs: Long = initialAudioOffsetMs.coerceIn(-15_000, 15_000)
        private set

    private var baseMs = 0L

    // A quality change rebuilds the plan and this controller. The viewer's
    // track choices belong to the *playback*, not to the controller, so they
    // come in from the screen — picking 720p used to silently drop you back to
    // the default audio track and the server's automatic subtitle.
    var selectedAudio: Long? =
        retainedAudio?.takeIf { index -> plan.audio.any { it.index == index } }
            ?: plan.audio.firstOrNull { it.default }?.index
        private set

    /**
     * Absolute subtitle-stream index, or null for Off.
     *
     * With no retained choice this is the server's pick: `/decision` runs
     * `select_tracks` and stamps `default` on exactly one track or none, and
     * [autoSubtitleSelection] applies that rather than re-deriving a policy of
     * our own — re-deriving is how a client ends up behaving as subtitle mode
     * `Always` against a server configured for `Auto`.
     */
    var selectedSubtitle: Long? = run {
        val selected = when {
            retainedSubtitle == null -> autoSubtitleSelection(plan.subtitles)
            // "Off" is a choice; only a track that vanished falls back to policy.
            retainedSubtitle.index == null -> null
            else -> retainedSubtitle.index.takeIf { index ->
                plan.subtitles.any { it.index == index }
            }
        }
        val track = plan.subtitles.firstOrNull { it.index == selected }
        selected.takeUnless {
            subtitleBurnWouldDiscardHdr(track, plan.deliveredDynamicRange)
        }
    }
        private set

    internal var pgsOverlayFrame: PGSOverlayFrame? by mutableStateOf(null)
        private set

    internal var pgsOverlayStatus: PGSOverlayStatus by mutableStateOf(PGSOverlayStatus.Off)
        private set

    /** A direct DV failure may use one lossless remux before the final rescue. */
    private var compatibilityRemuxUsed = false

    /** One final H.264 compatibility transcode; failure after that is terminal. */
    private var compatibilityTranscodeUsed = false

    /** One route refresh per attached item, separate from the transcode budget. */
    private var sinkRetryUsed = false

    /** A direct DV decoder failure first gets the same video in normalized MP4. */
    private var forceCompatibilityRemux = false

    /** Set by the rescue: this playback must re-encode, whatever the plan said. */
    private var forceCompatibilityTranscode = false

    /** Once a real frame rendered, a later fault is transport, not capability. */
    private var establishedPlayback = false

    /** One reconnect of the exact HDR recipe; a repeated failure is visible. */
    private var sameHdrRetryUsed = false

    private val stallReopenBudget = StallReopenBudget()
    private val openStallTracker = OpenPlaybackStallTracker()
    private val stallGuard = ControllerStallGuard(stallReopenBudget, openStallTracker)
    private var sessionlessStallRecoveryUsed = false
    private var sessionlessStallRecoveryPositionMs: Long? = null
    private var seekJob: Job? = null

    /**
     * Every create for this playback passes through one coordinator. This
     * keeps a newer user restart behind an in-flight stall fallback, making
     * the user request the server's final replacement as well as the UI's.
     */
    private val continuousPlayer = builtPlayer.player
    private val continuousSources = builtPlayer.continuousSources
    private val continuousOutput = builtPlayer.continuousOutput
    private val continuousTransfers = builtPlayer.autoTransfers
    private val continuousClientInstance = UUID.randomUUID().toString()
    private var continuousAttempt: ContinuousEnrollment? = null
    private var continuousUsed = false
    private var continuousFailedRequest: String? = null
    private var continuousAttachment: ContinuousAttachment? = null
    private var continuousExecutedQuality = activeQuality
    private data class OwnedContinuous(val owner: ContinuousAttachment, val source: androidx.media3.exoplayer.source.MediaSource)
    private val continuousPending = LinkedHashMap<String, OwnedContinuous>()

    private suspend fun createOwnedSession(body: CreateSessionReq): HlsStart {
        if (body.request_id != null && body.request_id == continuousFailedRequest) {
            throw java.io.IOException("Controlled parent failed after admission; a new request identity is required")
        }
        if (continuousUsed || player !== continuousPlayer || predecessorAttached() || plan.isAudioOnly) {
            Log.i("PlurxPlayback", "continuous enrollment not offered: used=$continuousUsed ownPlayer=${player === continuousPlayer} " +
                "predecessor=${predecessorAttached()} audioOnly=${plan.isAudioOnly}")
            return vm.createHlsSession(plan.fileId, body, recoveryLinkReceipt(body))
        }
        val enrollment = continuousAttempt ?: ContinuousEnrollment(Session.origin, Session.token.orEmpty()).also { continuousAttempt = it }
        val started = enrollment.open(plan.fileId, body,
            continuousEnrollmentIntent(body, playbackIntent, playbackControlSelection()))
        if (started == null) {
            enrollment.close()
            continuousAttempt = null
            return vm.createHlsSession(plan.fileId, body, recoveryLinkReceipt(body))
        }
        continuousAttempt = null
        continuousUsed = true
        lateinit var attachment: ContinuousAttachment
        attachment = ContinuousAttachment(enrollment, started, continuousClientInstance,
            continuousSources, continuousOutput, continuousTransfers,
            presented = { row, _ -> scope.launch {
                if (continuousAttachment === attachment && player === continuousPlayer && playbackControlBootstrapFence.isActive()) {
                    continuousQualityPresented(row)
                }
            } },
            // Reported whenever the previous choice was restored, whether or not
            // the target was proven cancelled; either way this change is over and
            // its Auto preparation or directed change must not stay pending.
            retained = { row, request, cancelProven -> scope.launch {
                if (continuousAttachment === attachment && player === continuousPlayer && playbackControlBootstrapFence.isActive()) {
                    Log.i("PlurxPlayback", "continuous previous quality restored request=$request cancelProven=$cancelProven")
                    if (request < 0 && request == continuousAutoRequest && continuousAutoEpoch == mediaMutationEpoch &&
                        playbackIntent.desiredQuality == PlaybackQuality.Auto && autoDesiredCandidate?.id == row.text("candidate_id")) {
                        failAutoPreparation()
                    } else directedChange?.takeIf { it.pending?.sequence == request }?.let { change ->
                        val pending = change.pending
                        if (pending != null && change.incumbentSelection != null && playbackIntent.retainQualityChange(pending, null)) {
                            change.settleFailureOnce(mediaMutationEpoch, true, retain = {
                                if (playbackIntent.retainFailedQuality(pending, change.incumbentSelection)) {
                                    retainedQualityRequest = change.quality
                                    raiseDegradedNotice("Quality change did not complete. Playback continues. Retry or apply with restart in Playback settings.")
                                    logQualitySwitch("retained_current", change.quality)
                                    playbackControl.reportEvidence()
                                }
                            }, reopen = {})
                        }
                    }
                }
            } },
            expectedPresentation = { row, revision, boundaryUs, delayMs -> scope.launch {
                if (continuousAttachment === attachment && player === continuousPlayer && playbackControlBootstrapFence.isActive() &&
                    (playbackIntent.desiredQuality.rungHeight?.toLong() == row.number("height") ||
                        playbackIntent.desiredQuality == PlaybackQuality.Auto && autoDesiredCandidate?.id == row.text("candidate_id"))) {
                    playbackTelemetry.report(event = "continuous_quality_expected_presentation", level = "info",
                        message = "Target quality is scheduled after retained playable media.", ms = delayMs,
                        detail = "revision=$revision boundary_us=$boundaryUs expected_active_delay_ms=${delayMs ?: "unknown"}")
                }
            } },
            observationUnknown = { row -> scope.launch {
                if (continuousAttachment === attachment && player === continuousPlayer && playbackControlBootstrapFence.isActive() &&
                    (playbackIntent.desiredQuality == PlaybackQuality.Auto || playbackIntent.desiredQuality.rungHeight?.toLong() == row.number("height"))) {
                    raiseDegradedNotice("Target quality presentation has not been observed. Playback continues while it is checked.")
                    playbackTelemetry.report(event = "continuous_quality_observation_unknown", level = "warn",
                        message = "Committed target media has no observed hardware presentation.")
                }
            } },
            failed = { error -> Log.w("PlurxPlayback", "continuous ownership ${redactedFailureDetail("continuous", error)}") })
        try {
            val position = ((body.start ?: 0.0) * 1000).toLong().coerceAtLeast(0)
            val source = attachment.source(position, body.quality_auto == true)
            continuousPending[started.playback.session_id] = OwnedContinuous(attachment, source)
            return started.playback
        } catch (error: Exception) {
            continuousFailedRequest = body.request_id
            attachment.finishAfterRelease()
            throw error
        }
    }

    private fun releaseOwnedSession(id: String) {
        val pending = continuousPending.remove(id)
        if (pending != null) { pending.owner.finishAfterRelease(); return }
        val attached = continuousAttachment
        if (attached?.start?.playback?.session_id == id) {
            attached.end()
        } else vm.endHlsSession(id)
    }

    private suspend fun retireContinuousForReplacement() {
        val attachment = continuousAttachment ?: return
        if (player !== continuousPlayer) return
        attachment.closeAdmission()
        player.stop()
        player.clearMediaItems()
        attachment.finishAfterRelease()
        attachment.awaitFinished()
        if (continuousAttachment === attachment) continuousAttachment = null
    }

    private fun continuousOwnsQuality(quality: PlaybackQuality): Boolean {
        val attachment = continuousAttachment
        return ContinuousQualityOwnership.owns(attached = attachment != null, closed = attachment?.isClosed == true,
            ownPlayer = player === continuousPlayer, quality = quality) { attachment?.rendition(it) != null }
    }

    /** The attachment that may still execute a change: attached to the player
     * the viewer sees and not yet fenced. */
    private fun liveContinuous(): ContinuousAttachment? =
        continuousAttachment?.takeIf { player === continuousPlayer && !it.isClosed }

    /**
     * Leaving continuous HLS for Direct play or the progressive remux retires
     * the attachment rather than only ending its session. Otherwise it stayed
     * attached to the same player, kept claiming every quality change it could
     * only refuse, pinned [currentRecipe] to its last executed rung, and held
     * its scope and output subscription for the rest of the title.
     *
     * Only on its own player: once a prepared successor is the picture, the
     * parked continuous player is retired by [collectRetiredPlayer], which
     * releases it before finishing the attachment.
     */
    private fun retireContinuousForTransportChange(): ContinuousAttachment? {
        val attachment = continuousAttachment ?: return null
        if (player !== continuousPlayer) return null
        continuousAttachment = null
        attachment.finishAfterRelease()
        return attachment
    }

    private fun continuousQualityPresented(row: JsonObject) {
        val id = row.text("candidate_id") ?: return
        val pending = playbackIntent.pendingQualityChange
        val desired = playbackIntent.desiredQuality
        if (desired.rungHeight != null && row.number("height") != desired.rungHeight?.toLong()) return
        if (desired == PlaybackQuality.Original) return
        if (desired == PlaybackQuality.Auto && autoDesiredCandidate?.id?.let { it != id } == true) return
        val requested = currentRecipe().recipe.copy(quality = desired)
        val attached = recipeOwnership.attached ?: return
        if (!attached.recipe.copy(quality = desired).hasSameMedia(requested)) return
        continuousExecutedQuality = desired
        val current = currentRecipe()
        autoActiveCandidateId = id
        planReplacement.presented(desired)
        stallReopenBudget.seed(row.number("height")?.toInt())
        if (autoDesiredCandidate?.id == id) {
            autoLastSwitchMs = monotonicNowMs()
            if (autoVoluntary) autoSwitchTimes.add(monotonicNowMs())
            autoDesiredCandidate = null
            autoPreparing = false
            autoUpgradeSinceMs = null
        }
        if (pending != null) {
            playbackIntent.clearQualityChange(pending)
            directedChange?.takeIf { it.pending == pending }?.let { it.committed(); logQualitySwitch("continuous", it.quality) }
            retainedQualityRequest = null
        }
        recipeOwnership.attach(current)
        recipeOwnership.selectionApplied(current)
        selectionRecipe = current
        playbackControl.playerChanged()
    }

    private val sessionCreateCoordinator = SessionCreateCoordinator(
        createSession = ::createOwnedSession,
        // A refusal the server explained now arrives as RefusalException,
        // so "is this a 400" has to ask for the status rather than for one of
        // the two exception types that can carry it.
        isBadRequest = { failure -> refusalStatusOf(failure) == 400 },
        freshRequestId = { UUID.randomUUID().toString() },
        releaseSession = ::releaseOwnedSession,
    )

    /**
     * The delivery this plan would use with no subtitle in play. A manual A/V
     * correction is only expressible by the remuxer, so it moves direct play
     * one step down even before subtitles are considered; the compatibility
     * rescue moves everything to a transcode, because "the device refused
     * these bytes" is exactly a demand for different ones.
     */
    /** Effective plan facts adopted only when a prepared successor commits. */
    private var attachedModeOverride: String? = null
    private var attachedRequiresHlsOverride: Boolean? = null

    private val planMode: String
        get() = when {
            forceCompatibilityTranscode -> "transcode"
            forceCompatibilityRemux -> "remux"
            attachedModeOverride != null -> attachedModeOverride!!
            audioOffsetMs != 0L && plan.mode == "direct" -> "remux"
            else -> plan.mode
        }

    /**
     * Settings → Playback → "Subtitle switching", read once per controller so
     * it applies to the next title started rather than moving a stream that
     * is already up — the same per-title capture the Apple client makes.
     */
    private val subtitleReadiness: SubtitleReadiness = vm.preferences.value.subtitleReadiness

    /** Whether a session could publish any rendition at all for this file. */
    private val hasNativeSubtitleTrack: Boolean = plan.subtitles.any { it.isNativeHls }

    /** [subtitleRoute] with this playback's fixed inputs filled in. */
    private fun routeSubtitle(track: SubTrack?, current: SubtitleDelivery): SubtitleRoute =
        subtitleRoute(track, planMode, current, subtitleReadiness, hasNativeSubtitleTrack)

    /** How that selection is being carried — see `SubtitlePolicy.kt`. */
    private var subtitleDelivery: SubtitleDelivery =
        routeSubtitle(trackFor(selectedSubtitle), SubtitleDelivery.Plan).delivery
    private val recipeOwnership = PlaybackRecipeOwnership()
    private var selectionRecipe: PlaybackRecipeOwnership.Claim? = null
    private data class RecipeExecution(
        val sequence: Long,
        val recipe: PlaybackRecipeOwnership.Claim,
        val inPlace: Boolean,
    )
    private var recipeExecution: RecipeExecution? = null

    private fun currentRecipe(): PlaybackRecipeOwnership.Claim = recipeOwnership.request(
        PlaybackMediaRecipe(
            quality = playbackIntent.qualityForMedia().let { requested ->
                if (continuousOwnsQuality(requested)) continuousExecutedQuality else requested
            },
            mode = planMode,
            requiresHls = attachedRequiresHlsOverride ?: plan.requiresHls,
            audioIndex = selectedAudio,
            subtitleIndex = selectedSubtitle,
            subtitleDelivery = subtitleDelivery,
            audioOffsetMs = audioOffsetMs,
            compatibilityTranscode = forceCompatibilityTranscode,
        ),
    )

    /**
     * M5 addition 3, and its per-attach budget. The recovery itself
     * re-prepares WITHOUT a new item, so it cannot refill its own budget.
     */
    private val behindLiveWindow = BehindLiveWindowRecovery()

    private fun attachRecipe(
        recipe: PlaybackRecipeOwnership.Claim,
        transport: PlaybackMediaTransport = recipe.recipe.desiredTransport,
    ) {
        recipeOwnership.attach(recipe, transport)
        sinkRetryUsed = false
        selectionRecipe = recipe
        textSelectionArmed = true
        audioSelectionArmed = true
        // M5 addition 3's budget is per ATTACH, and every `setMediaItem` +
        // `prepare` in this file goes through here. It is NOT the only way an
        // item takes the screen — `commitPreparedReplacement` swaps in a whole
        // second `ExoPlayer` without coming near this function — so that site
        // re-arms it too, and this comment no longer claims otherwise.
        behindLiveWindow.attached()
    }

    /**
     * The dynamic range the *current* delivery puts on the wire, as the server
     * reported it (MEDIA-BADGES-PLAN.md §3.2). Seeded from the decision, then
     * overwritten by every session this controller opens — a burn or a picked
     * rung produces a transcode the decision never promised, and the session's
     * answer is the one that describes the bytes now arriving. A session that
     * omits it leaves the standing value alone.
     *
     * Compose state so the badge row and the info panel recompose on a change;
     * it steers nothing.
     */
    var deliveredRange: String? by mutableStateOf(plan.deliveredDynamicRange)
        private set

    /**
     * The Dolby Vision profile on the wire, when the delivery carries any.
     *
     * Moves with [deliveredRange] and never on its own — see
     * [adoptSessionDelivery]. Read together they say "Dolby Vision, and it is
     * Profile 8"; read apart they can say "Dolby Vision Profile 8" over an
     * HDR10 stream.
     */
    var deliveredDolbyVisionProfile: Int? by mutableStateOf(plan.deliveredDolbyVisionProfile)
        private set

    /**
     * Take a session's delivery answer, both halves at once.
     *
     * The two fields are one answer and are assigned together, deliberately.
     * A session that reports a range but omits the profile is saying "no
     * profile", not "keep the one you had": the legacy single-ffmpeg copy path
     * serves the HDR10 base for a title the decision said would be converted,
     * which is the *normal* first watch of a converting title, before its
     * fragment index exists. Two independent `?.let`s would keep the
     * decision's profile through exactly that and paint `DV → DV P8` over
     * HDR10 — and each half would look correct on its own, which is why this
     * is one function rather than two lines at each of the two call sites.
     *
     * A session that omits the range entirely says nothing about either, and
     * the standing values stand.
     */
    private fun adoptSessionDelivery(hls: HlsStart) {
        val range = hls.delivered_dynamic_range ?: return
        deliveredRange = range
        deliveredDolbyVisionProfile = hls.delivered_dolby_vision_profile
    }

    /**
     * What the stats overlay and the menu call this. A native-rendition
     * session on a direct or remux verdict is still a remux — the video is
     * copied; only the playlist gained a subtitle group.
     */
    val deliveryMode: String
        get() {
            return when (recipeOwnership.attachedTransport) {
                PlaybackMediaTransport.Direct -> "direct"
                PlaybackMediaTransport.ProgressiveRemux,
                PlaybackMediaTransport.HlsCopy -> "remux"
                PlaybackMediaTransport.HlsTranscode -> "transcode"
                null -> planMode
            }
        }

    val pgsOverlayIsActive: Boolean
        get() = pgsOverlayStatus != PGSOverlayStatus.Off

    /** True while the original file is being read directly, base timeline = 0. */
    private val directTransport: Boolean
        get() = recipeOwnership.attachedTransport == PlaybackMediaTransport.Direct

    /** True while Media3 is reading the live progressive remux response. */
    private val progressiveTransport: Boolean
        get() = recipeOwnership.attachedTransport == PlaybackMediaTransport.ProgressiveRemux

    var encoder: String? = null
        private set

    var sessionStatus: PlaybackSessionStatus? by mutableStateOf(null)
        private set
    private var sessionStatusObservedAtMs: Long? by mutableStateOf(null)
    val sessionStatusAgeMs: Long?
        get() = sessionStatusObservedAtMs?.let { (monotonicNowMs() - it).coerceAtLeast(0) }
    val presentationProgressAgeMs: Long?
        get() = openStallTracker.progressAgeMs(monotonicNowMs())
    val currentSessionId: String? get() = sessionId
    val currentSessionIsVod: Boolean get() = sessionIsVod

    private var currentMediaSessionTransport: Player? = null
    private val mediaSession = MediaSession.Builder(context.applicationContext,
        mediaSessionTransport(player, null)).build()
    init {
        if (plan.isAudioOnly) PlaybackService.attach(context, mediaSession)
    }

    /** The HLS session this player owns, if the plan opened one. */
    private var sessionId: String? = null

    /** Transport interception happens before Media3 mutates the delegate, so
     * MediaSession Play/Pause reaches the same viewer writer as the UI. Other
     * player commands still use ForwardingPlayer's ordinary forwarding and
     * command availability. */
    private fun mediaSessionTransport(delegate: ExoPlayer, capturedSession: String?): Player {
        lateinit var transport: Player
        transport = autoViewerTransport(delegate, {
            currentMediaSessionTransport === transport && player === delegate &&
                sessionId == capturedSession && playbackControlBootstrapFence.isActive()
        }, ::setViewerPlaybackRequested)
        currentMediaSessionTransport = transport
        return transport
    }

    private fun rebindMediaSessionTransport() {
        interceptedTransportCallbacks.clear()
        explicitViewerPause = null
        mediaSession.setPlayer(mediaSessionTransport(player, sessionId))
    }

    /**
     * Set while paused when the server retired this rolling session (see
     * [PausedRetirement]). [playPause] consumes it; it is honoured only while
     * it names the attached session, so any successor retires it by identity.
     */
    private var pausedRetirement: PausedRetirement? = null

    private fun currentPausedRetirement(): PausedRetirement? =
        pausedRetirement?.takeIf { it.sessionId == sessionId }
    private var activeMediaPath: String? = null

    private val playbackTelemetry = ControllerPlaybackTelemetry(
        plan = plan,
        player = object : PlaybackTelemetryPlayer {
            override val buffering: Boolean
                get() = player.playbackState == Player.STATE_BUFFERING
            override val playbackRequested: Boolean
                get() = player.playWhenReady
            override val playerPositionMs: Long
                get() = player.currentPosition
            override val mediaPositionMs: Long
                get() = realPosition()
            override val bufferedPositionMs: Long
                get() = player.bufferedPosition
            override val videoHeight: Int
                get() = player.videoSize.height
        },
        context = {
            PlaybackTelemetryContext(
                method = normalizedPlaybackMethod(deliveryMode) ?: "unknown",
                encoder = encoder,
                sessionId = sessionId,
            )
        },
        emit = vm::postPlaybackDiagnostic,
    )
    private val stallWatchdogJob: Job
    private val targetPresentationWatchdogJob: Job
    private val targetPresentationDeadline = playbackIntent.targetPresentationDeadline
    private val targetPresentationOwner = targetPresentationDeadline.claimOwner(monotonicNowMs())
    private var presentationForeground = true
    private var lifecyclePaused = false
    private var pendingLifecyclePauseCallback = false

    private fun effectivePlayWhenReady(): Boolean =
        playbackIntent.playbackRequested && !lifecyclePaused

    /** Keep the viewer's intent untouched while ON_STOP suppresses output. */
    private fun applyEffectivePlayWhenReady() {
        val target = effectivePlayWhenReady()
        samplePreparedCommitFrameBudget()
        if (lifecyclePaused && playbackIntent.playbackRequested && player.playWhenReady != target) {
            pendingLifecyclePauseCallback = true
            viewerTransport.ownerStopping()
        }
        player.playWhenReady = target
    }
    private var mediaMutationEpoch = 0L

    /**
     * Which of the two things that write `playWhenReady` wrote it.
     *
     * Media3 attributes both to the viewer; [ViewerTransportIntent] is where the
     * argument for keeping them apart lives, and where it is tested.
     */
    private val viewerTransport = ViewerTransportIntent()

    // ------------------------------------------------ the playback surface
    //
    // What the viewer is shown when playback is not simply playing, as a
    // projection of this player rather than a string pushed at a view:
    // docs/clients/PLAYBACK-SURFACE-CONTRACT.md. The reducer in
    // `PlaybackSurface.kt` is pure; this controller is the recovery owner, and
    // `PlaybackSurfaceOwner` is the one place its new obligation — stop the
    // player, then raise a blocking fault — is discharged.
    private val _surface = MutableStateFlow<PlaybackSurface>(PlaybackSurface.None)

    /** The only playback surface. Rendered from; never written to from a view. */
    internal val surface: StateFlow<PlaybackSurface> = _surface.asStateFlow()

    /**
     * Moves on every ledger change, including ones the drawn surface cannot
     * show — a fault cleared underneath the one on screen changes the history
     * and nothing else. Compose state, so Playback debug recomposes on it.
     */
    internal var surfaceRevision by mutableIntStateOf(0)
        private set

    private val surfaceOwner = PlaybackSurfaceOwner(
        player = object : SurfaceOwnerPlayer {
            // Read through `player` rather than captured: a committed prepared
            // replacement swaps the instance, and the owner must stop the one
            // on the surface now.
            override var playbackRequested: Boolean
                get() = player.playWhenReady
                set(value) {
                    // Marked BEFORE the write, not after: Media3 may deliver the
                    // resulting `onPlayWhenReadyChanged` before this setter has
                    // returned, and that callback is where the owner's own stop
                    // has to be told apart from a viewer's pause.
                    if (!value) {
                        playbackTelemetry.cancelPending()
                        viewerTransport.ownerStopping()
                    }
                    if (value && lifecyclePaused) applyEffectivePlayWhenReady()
                    else player.playWhenReady = value
                }
            override val positionMs: Long get() = realPosition()
            // `playbackParameters.speed` is the requested SETTING and stays
            // 1.0 while the player is stopped — it is not Apple's
            // `AVPlayer.rate`. The ledger's `rate` has to be able to read zero,
            // because "exhausted with rate 0" is what M4 measures.
            override val rate: Float
                get() = if (player.isPlaying) player.playbackParameters.speed else 0f
        },
        nowMs = { monotonicNowMs() },
        publish = { value ->
            // playback-surface-publish:begin
            _surface.value = value
            // playback-surface-publish:end
        },
        emit = { entry ->
            surfaceRevision += 1
            reportSurfaceLog(entry)
        },
    )

    /** Last `realPosition()` the surface sampled, or null after a generation change. */
    private var surfaceEvidencePositionMs: Long? = null
    private var surfaceEvidenceSampledAtMs: Long? = null
    private var surfaceIntentSequence: Long? = null

    /** Last 16 faults, for the ledger's SURFACE section. */
    internal val surfaceHistory: List<SurfaceLedgerRow> get() = surfaceOwner.history

    private var statusPollingJob: Job? = null
    /** Screen visibility for passive status readers. Recovery must supply true if it starts reading status. */
    var statusPollingVisible: () -> Boolean = { false }
    var playbackStallCount by mutableIntStateOf(0)
        private set
    var lastTimeToFirstFrameMs by mutableStateOf<Long?>(null)
        private set

    /**
     * This player's passive control reporting. Nothing about playback depends
     * on it: a server that offers no bootstrap leaves it silent.
     */
    private val playbackControl = PlaybackControlSession(scope)
    private val playbackControlBootstrapFence = PlaybackControlBootstrapFence()
    private val displayModeOwner = displayModeMatcher?.claimOwner()
    private var displayModeStartRequested = false
    private var lateDisplayModeRequested = false

    private fun reportDisplayMode(result: DisplayModeMatchResult) {
        logDisplayModeResult(result)
        playbackTelemetry.report(
            event = "playback_display_mode",
            level = "info",
            message = "Android display-mode decision",
            detail = result.detail(),
        )
    }

    /**
     * When the player began buffering, or null while it is not. The protocol
     * separates waiting from stalled by how long, and Media3 reports only that
     * it is buffering.
     */
    private var controlWaitingSince: Long? = null

    /**
     * What this device can take, resolved once. `Caps.query` probes the
     * decoder registry and suspends, and the answer does not change while a
     * player exists — so asking it per snapshot would be both illegal here and
     * wasteful.
     */
    private var autoCatalog = plan.qualityCandidates
    private var continuousAutoRequest = 0L
    private var continuousAutoEpoch = 0L
    private var autoMeasuredOutputs = emptyList<tv.plurx.app.data.MeasuredCandidateOutput>()
    private var autoDesiredCandidate: tv.plurx.app.data.QualityCandidate? = null
    private var autoActiveCandidateId: String? = plan.qualityCandidateId
    private var autoRouteProtocol: String? = plan.displayAwareAutoProtocol
    private var autoNextTickMs = 0L
    private val autoLinkClaims = LinkedHashMap<String, Triple<Long, Boolean, String>>()
    private var autoUpgradeSinceMs: Long? = null
    private val autoUpgradeEvidence = AutoUpgradeEvidenceWindow()
    private var autoLastSwitchMs: Long? = null
    private var autoLastEvaluation: Triple<Any, Long, Long>? = null
    private val autoSwitchTimes = mutableListOf<Long>()
    private val autoBlockedUntil = mutableMapOf<String, Long>()
    /** An original-quality preparation a viewer boundary (a seek, or a resume
     * after a long explicit pause) asked for. It is an ordinary Auto prepared
     * replacement in every respect — the incumbent keeps playing and the
     * rendezvous hands off — except that a fresh link proof stands in for the
     * mid-play quiet window. Nothing waits on it. */
    private class AutoBoundaryAttempt(val incumbent: ExoPlayer, val sessionId: String, val epoch: Long,
        val candidateId: String, val recipe: PlaybackMediaRecipe, val qualitySequence: Long?,
        val transportLifetime: Any, val audio: Long?, val subtitle: Long?, val audioOffsetMs: Long,
        val previousCandidate: tv.plurx.app.data.QualityCandidate?, val previousId: String?,
        val previousHeight: Int?)
    private var autoBoundaryAttempt: AutoBoundaryAttempt? = null
    /** The viewer boundary the next ordinary Auto evaluation owes a re-plan. */
    private val autoBoundaryReplan = AutoBoundaryReplan()
    private var viewerTransportLifetime: Any = Any()
    private data class ExplicitViewerPause(val player: ExoPlayer, val session: String?,
        val epoch: Long, val atMs: Long)
    private var explicitViewerPause: ExplicitViewerPause? = null
    private val interceptedTransportCallbacks = ArrayDeque<Pair<ExoPlayer, Boolean>>()
    private var autoPreparing = false
    private var autoVoluntary = true
    private var autoMildSamples = 0
    private var autoMildTransferCompletedMs: Long? = null
    private val autoDecoderRejected = playbackIntent.rejectedAutoCandidateIds
    private val autoDecodePressure = AutoDecodePressureWindow()
    private var autoDecodeQualityResponseUsed = false
    private var autoRecoveryCause: AutoRecoveryCauseTicket? = null
    private var autoPreparedTargetRevision: Long? = null
    private var autoPreparedMutationEpoch: Long? = null
    private var autoStagedStatus: PlaybackSessionStatus? = null
    private var autoStagedStatusObservedMs: Long? = null

    private var deviceControlCapabilities: DynamicCapabilities? = null

    /**
     * Resolved once, here, because `Caps.query` probes the decoder registry
     * and suspends while `context` is only reachable from an initializer. The
     * answer cannot change while a player exists, so asking per snapshot would
     * be both illegal and wasteful.
     */
    private val controlCapabilityProbe: Job = scope.launch {
        deviceControlCapabilities = controlCapabilities(
            query = Caps.query(context),
            // Read once, here, for the same reason the rest of the document is
            // read once: capabilities are required on the first request of a
            // generation and the server retains what it was told, so a value
            // that changed mid-player would never reach it anyway. Turning the
            // switch on takes effect on the next playback, which is also the
            // only honest moment to change a hardware claim.
            preparedReplacementEnabled = vm.preferences.value.preparedReplacement,
        )
    }

    /**
     * The open session is the whole stream on disk (a pre-transcode cache
     * hit): its timeline starts at zero, so it seeks in place rather than
     * churning a server session per scrub.
     */
    private var sessionIsVod = false

    /**
     * A text selection that still has to be applied to the player.
     *
     * Renditions and embedded tracks only exist once the source has been
     * prepared and its tracks published, and a forced rendition is always
     * `DEFAULT=NO` (crates/plurxd/src/http/hls.rs — Apple's authoring rules
     * make DEFAULT=YES mean something stronger than FORCED=YES), so the
     * playlist's own metadata can never be relied on to select one. Arm the
     * intent here and let [listener] land it whenever the tracks turn up.
     */
    private var textSelectionArmed = false

    /**
     * The audio equivalent of [textSelectionArmed], and it exists for the same
     * reason the text one does: only direct play hands the player a container
     * with every source track in it, and those tracks only exist once the
     * source has been prepared.
     *
     * Every other transport delivers the single audio stream the *server*
     * selected — the plan's `?audio=<index>` or the session body's `audio` —
     * so there is nothing to override there. Direct play has no such selector,
     * which is why the server refuses a `direct` verdict for any choice other
     * than the container's own default; ExoPlayer's language preference is
     * then the only thing left that could put a different track on the
     * speakers than the one the detail screen marked.
     */
    private var audioSelectionArmed = false

    private val pgsOverlay = AndroidPGSOverlayController(
        api = { vm.api() },
        scope = scope,
        fileId = plan.fileId,
        sourcePositionMs = ::realPosition,
        isPlaying = { player.isPlaying },
        playbackSpeed = { player.playbackParameters.speed },
        onFrame = { pgsOverlayFrame = it },
        onStatus = { pgsOverlayStatus = it },
        onFailure = { message -> raiseDegradedNotice(message) },
    )

    /** Audio output faults use their own bounded branch beside the existing ladder. */
    private fun handleAudioSinkFailure(error: PlaybackException, refusal: MediaRefusal?): Boolean {
        val failure = audioSinkFailure(error.errorCode)
        if (failure == AudioSinkFailure.None) return false
        val currentRoute = Caps.audioOutputRoute(context)
        val action = audioSinkAction(
            failure = failure,
            // A failed route query is unknown, not evidence of disconnection.
            outputDevicePresent = currentRoute?.present ?: true,
            routeChangedSinceSnapshot = currentRoute != null && plan.audioOutputRoute != null &&
                currentRoute != plan.audioOutputRoute,
            sinkRetryUsed = sinkRetryUsed,
            // Reopening an existing transcode with the same recipe is not a rescue.
            transcodeRescueAlreadyUsed = compatibilityTranscodeUsed || deliveryMode == "transcode",
        )
        playbackTelemetry.report(
            event = "playback_audio_sink_failure",
            level = "error",
            message = error.errorCodeName,
            code = error.errorCode,
            detail = audioSinkDiagnostic(error, failure, action, currentRoute),
        )
        when (action) {
            AudioSinkRecovery.ReSnapshotAndRetry -> {
                sinkRetryUsed = true
                // PlayerScreen's ordinary reload asks for a fresh Caps.snapshot
                // and /decision before attaching its replacement controller.
                replan(realPosition(), "audio-route", playbackIntent.desiredQuality)
                raiseRecoveryStep(refusal, "Checking the changed audio output.")
            }
            AudioSinkRecovery.TranscodeRescue -> retryAsCompatibilityTranscode(refusal)
            AudioSinkRecovery.FailDisconnected ->
                stopAndRaisePlaybackFailure(null, "Audio output disconnected.")
            AudioSinkRecovery.Fail ->
                stopAndRaisePlaybackFailure(refusal, "Audio playback stopped (${error.errorCodeName}).")
        }
        return true
    }

    private fun audioSinkDiagnostic(
        error: PlaybackException,
        failure: AudioSinkFailure,
        action: AudioSinkRecovery,
        route: AudioOutputRoute?,
    ): String {
        val causes = generateSequence(error.cause) { it.cause }.take(4).toList()
        val initialization = causes.filterIsInstance<AudioSink.InitializationException>().firstOrNull()
        val write = causes.filterIsInstance<AudioSink.WriteException>().firstOrNull()
        val format = initialization?.format ?: write?.format ?: player.audioFormat
        val mime = format?.sampleMimeType
        val passthrough = mime in setOf(
            "audio/ac3", "audio/eac3", "audio/eac3-joc", "audio/vnd.dts", "audio/vnd.dts.hd",
            "audio/true-hd",
        )
        val outputs = route?.devices?.sortedWith(compareBy({ it.type }, { it.id }))
            ?.take(8)?.joinToString(",") { "${it.type}:${it.id}" } ?: "unknown"
        val cause = causes.firstOrNull()
        return buildString {
            append("failure=").append(failure)
            append(" action=").append(action)
            append(" route=").append(outputs)
            append(" route_changed=").append(route != null && plan.audioOutputRoute != null &&
                route != plan.audioOutputRoute)
            append(" format=").append(mime ?: "unknown")
            append(" encoding=").append(format?.pcmEncoding ?: -1)
            append(" channels=").append(format?.channelCount ?: -1)
            append(" sample_rate=").append(format?.sampleRate ?: -1)
            append(" passthrough=").append(passthrough)
            initialization?.let {
                append(" audio_track_state=").append(it.audioTrackState)
                append(" recoverable=").append(it.isRecoverable)
            }
            write?.let {
                append(" sink_error_code=").append(it.errorCode)
                append(" recoverable=").append(it.isRecoverable)
            }
            append(" cause=").append(cause?.javaClass?.simpleName?.take(80) ?: "none")
            cause?.message?.let { message ->
                append(" message=").append(message.replace('\n', ' ').replace('\r', ' ').take(200))
            }
        }
    }

    private fun retryAsCompatibilityTranscode(refusal: MediaRefusal?) {
        val position = realPosition()
        compatibilityTranscodeUsed = true
        forceCompatibilityTranscode = true
        subtitleDelivery = routeSubtitle(trackFor(selectedSubtitle), subtitleDelivery).delivery
        restartAt(position, "fallback")
        raiseRecoveryStep(refusal, "Switching to a compatible stream.")
    }

    private val listener = object : Player.Listener {
        override fun onPlayerError(error: PlaybackException) {
            scope.launch(start = kotlinx.coroutines.CoroutineStart.UNDISPATCHED) { handlePlayerError(error) }
        }

        private suspend fun handlePlayerError(error: PlaybackException) {
            val failureAttachment = player
            val failureAttempt = stallGuard.observeStall()
            if (!playbackControlBootstrapFence.isActive()) return
            // The departing item's failure does not own a newer requested
            // recipe (or seek create). That create and its target deadline
            // still own success/failure; failover must not steal the request.
            if (stallGuard.defersPredecessorRecovery(recipeOwnership.needsMediaReplacement(currentRecipe()))) return
            val mediaCompatibilityFailure = isCompatibilityPlaybackError(error.errorCode)
            // Paused on a rolling session: this is the pause grace retiring the
            // presentation (§9.5), not a failure anyone is watching. Walking the
            // node list or raising "Playback stopped" here is what the latch
            // replaces — every node answers the same 404/410. Play reopens.
            val retiredSession = sessionId
            if (retiredSession != null && parksPausedPlaybackError(
                    playbackRequested = playbackIntent.playbackRequested,
                    rollingSession = !sessionIsVod,
                    compatibilityFailure = mediaCompatibilityFailure,
                )
            ) {
                val parkedAt = realPosition()
                pausedRetirement = PausedRetirement(retiredSession, parkedAt)
                playbackTelemetry.report(
                    event = "playback_paused_retirement",
                    level = "info",
                    message = error.errorCodeName,
                    code = error.errorCode,
                    detail = "parked at ${parkedAt}ms; Play reopens",
                )
                surfaceOwner.logOnly(mediaMutationEpoch, "paused error parked (${error.errorCodeName})")
                return
            }
            // Only a transport failure can be answered by another node, and
            // for 2004 only some of them: `nodeFailoverEligible` reads the
            // status the exception carries, so an ended session's 404 or a
            // refused credential is terminal here instead of costing a full
            // player prepare on every ingress before the viewer sees the error
            // they were always going to see.
            reportControlEvidence(
                ClientObservation(
                    decoderState = DecoderState.FAILED,
                    errorCode = controlErrorCode(error.errorCode),
                    errorDetail = error.errorCodeName,
                ),
                render = RenderState.FAILED,
            )
            // The surface adapter runs AFTER the ladder, on its outcome:
            // `playbackErrorAction` is untouched (contract §3.5).
            val refusal = mediaRefusal(error)
            if (nodeFailoverEligible(error.errorCode, httpResponseCode(error)) &&
                retryMediaOnNextNode(error)
            ) {
                raiseRecoveryStep(refusal, "Reconnecting on another server address.")
                return
            }
            // M5 addition 3 — `BEHIND_LIVE_WINDOW` (1002) on a FINITE timeline.
            //
            // Media3's own answer is `seekToDefaultPosition()`, which is a
            // LIVE-EDGE policy: on a finite timeline it jumps to the start of
            // the available window and skips content nobody asked to skip. The
            // contract forbids it, so this seeks back to where the picture
            // actually was and prepares again — ONCE per attach, because a
            // second 1002 on the same item is the recovery not having worked
            // and repeating it is a loop. A live item keeps today's `Fail`,
            // which is what the rest of this function does with it.
            //
            // Placed above `playbackErrorAction` deliberately: 1002 is neither
            // a transport nor a compatibility failure, so the ladder answers it
            // with `Fail` (or, on an established HDR delivery, with a recipe
            // retry that changes nothing about a window that moved).
            val filmPositionMs = realPosition()
            if (behindLiveWindow.recover(
                    errorCode = error.errorCode,
                    live = player.isCurrentMediaItemLive,
                    seekTargetMs = { playerTimelinePositionMs(filmPositionMs) },
                    seekTo = { target -> player.seekTo(target) },
                    prepare = { player.prepare() },
                )
            ) {
                playbackTelemetry.report(
                    event = "playback_behind_live_window",
                    level = "warn",
                    message = error.errorCodeName,
                    code = error.errorCode,
                    detail = "finite timeline — seeking back to ${filmPositionMs}ms and preparing again",
                )
                raiseRecoveryStep(refusal, "Reloading this stream.")
                return
            }
            if (handleAudioSinkFailure(error, refusal)) return
            val decodeRecovery = controlErrorCode(error.errorCode) == ClientErrorCode.DECODER && noteAutoDecodeFailure()
            if (player !== failureAttachment || !stallGuard.isCurrent(failureAttempt)) return
            if (decodeRecovery) {
                restartAt(realPosition(), "decode-quality")
                raiseRecoveryStep(refusal, "Retrying a decoder-compatible quality.")
                return
            }
            val action = playbackErrorAction(
                deliveryMode = deliveryMode,
                preservesDolbyVision = plan.preserveDolbyVision,
                remuxRescueAlreadyUsed = compatibilityRemuxUsed,
                transcodeRescueAlreadyUsed = compatibilityTranscodeUsed,
                mediaCompatibilityFailure = mediaCompatibilityFailure,
                deliveredRange = deliveredRange,
                establishedPlayback = establishedPlayback,
                sameHdrRetryAlreadyUsed = sameHdrRetryUsed,
            )
            playbackTelemetry.report(
                event = "playback_error",
                level = "error",
                message = error.errorCodeName,
                code = error.errorCode,
                detail = buildString {
                    append("action=").append(action)
                    append(" media_compatibility=").append(mediaCompatibilityFailure)
                    append(" preserves_dv=").append(plan.preserveDolbyVision)
                    append(" established=").append(establishedPlayback)
                    if (caps.isNotEmpty()) {
                        append(" caps=")
                        append(
                            caps.entries.sortedBy { it.key }
                                .joinToString(",") { "${it.key}=${it.value}" },
                        )
                    }
                },
            )
            Log.w(
                "plurx-playback",
                "file=${plan.fileId} delivery=$deliveryMode preservesDv=" +
                    "${plan.preserveDolbyVision} action=$action caps=$caps " +
                    "mediaFailure=${isCompatibilityPlaybackError(error.errorCode)} " +
                    "error=${error.errorCodeName}",
                error,
            )
            when (action) {
                PlaybackErrorAction.RetrySameHDRDelivery -> {
                    // The one rung an armed verdict licenses skipping, and it
                    // is licensed by the server's own definition rather than
                    // by an argument made here. `is_permanent` is documented
                    // as "whether retrying this source, *unchanged*, can ever
                    // succeed", and this rung is the only one that retries it
                    // unchanged: it re-prepares the identical recipe and hopes.
                    //
                    // The other two rungs change the recipe, which is exactly
                    // what the verdict does not rule out. `unsupported` means
                    // "this source cannot be carried by *this delivery
                    // pipeline*" — a copy-producer exit — and
                    // `RetryAsCompatibilityTranscode` asks for a different
                    // pipeline; the server admits its own retry for the same
                    // reason. Skipping those would delete the rung most likely
                    // to produce a picture and caption it with a sentence
                    // about a pipeline nobody is proposing any more.
                    val armed = ladderVerdict(
                        errorCode = error.errorCode,
                        verdict = playbackControl.terminalVerdict,
                    )
                    if (armed != null) {
                        playbackTelemetry.report(
                            event = "playback_ladder_verdict",
                            level = "warn",
                            message = error.errorCodeName,
                            code = error.errorCode,
                            detail = "skipped=retry_same_hdr delivery=$deliveryMode " +
                                "verdict=${armed.type}",
                        )
                        // The armed verdict skipped the one rung that retries
                        // this source unchanged, so the ladder is spent here.
                        stopAndRaisePlaybackFailure(
                            refusal = refusal,
                            sentence = armed.message
                                ?: error.errorCodeName.let { "Playback stopped ($it)." },
                        )
                        return
                    }
                    val position = realPosition()
                    sameHdrRetryUsed = true
                    restartAt(position, "fallback")
                    raiseRecoveryStep(refusal, "Retrying this stream.")
                }
                PlaybackErrorAction.RetryAsDolbyVisionRemux -> {
                    val position = realPosition()
                    compatibilityRemuxUsed = true
                    forceCompatibilityRemux = true
                    subtitleDelivery =
                        routeSubtitle(trackFor(selectedSubtitle), subtitleDelivery).delivery
                    restartAt(position, "fallback")
                    raiseRecoveryStep(refusal, "Switching to a compatible copy.")
                }
                PlaybackErrorAction.RetryAsCompatibilityTranscode -> {
                    retryAsCompatibilityTranscode(refusal)
                }
                PlaybackErrorAction.Fail -> stopAndRaisePlaybackFailure(
                    refusal = refusal,
                    // A verdict says production stopped for a reason retrying
                    // cannot change. A dropped link is a different cause with
                    // a different answer, so a transport failure keeps the
                    // client's own words rather than borrowing the server's.
                    sentence = (if (isTransportPlaybackError(error.errorCode)) null
                    else playbackControl.terminalVerdict?.message)
                        ?: error.errorCodeName.let { "Playback stopped ($it)." },
                )
            }
        }

        /** Media3 calls this after output, unlike its pre-render metadata hook. */
        override fun onRenderedFirstFrame() {
            establishedPlayback = true
            openStallTracker.reset()
            lastTimeToFirstFrameMs = playbackTelemetry.firstFrame(monotonicNowMs())?.elapsedMs
            // The client's own presentation proof: an actual frame on the
            // surface, on the generation that is attached, foreground, with
            // playback requested (contract §3.4).
            if (player.playWhenReady && presentationForeground) {
                surfaceOwner.presenting(true, mediaMutationEpoch)
            }
            // A commit outstanding here is the successor's very first frame on
            // the surface, which is exactly what `first_frame_unix_ms` means.
            settleCommitOnFirstFrame(System.currentTimeMillis())
        }

        override fun onTracksChanged(tracks: Tracks) {
            applyTextSelection()
            applyAudioSelection()
            completeRecipeExecution()
            if (plan.sourceFrameRate == null && !lateDisplayModeRequested) {
                val lateFps = player.videoFormat?.frameRate?.toDouble()?.takeIf {
                    it.isFinite() && it > 0.0
                }
                val matcher = displayModeMatcher
                val owner = displayModeOwner
                if (lateFps != null && matcher != null && owner != null) {
                    lateDisplayModeRequested = true
                    scope.launch {
                        reportDisplayMode(
                            matcher.match(owner, lateFps, displayModeMatchEnabled, late = true),
                        )
                    }
                }
            }
        }

        override fun onPositionDiscontinuity(
            oldPosition: Player.PositionInfo,
            newPosition: Player.PositionInfo,
            reason: Int,
        ) {
            // A discontinuity is the viewer's move: it ends a failure backoff.
            pgsOverlay.reconcile(seeked = true)
        }

        override fun onPlaybackParametersChanged(playbackParameters: PlaybackParameters) {
            settleAudioPlaybackIntentIfPresented()
            pgsOverlay.reconcile()
        }

        override fun onIsPlayingChanged(isPlaying: Boolean) {
            // Inert by contract: a state flag is not proof that a picture is
            // moving, and it never moves a surface on its own.
            surfaceOwner.inert("isPlayingChanged")
            settleAudioPlaybackIntentIfPresented()
            if (isPlaying && plan.videoCodec == null) {
                establishedPlayback = true
                openStallTracker.reset()
            }
            pgsOverlay.reconcile()
        }

        override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
            if (!playbackControlBootstrapFence.isActive()) return
            if (pendingLifecyclePauseCallback && !playWhenReady) {
                pendingLifecyclePauseCallback = false
                sampleTargetPresentationDeadline()
                return
            }
            // Includes MediaSession transport controls. Internal replacement
            // writes always apply this same latest value; transient buffering
            // and audio-focus suppression do not replace viewer intent.
            if (reason == Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST) {
                // This exact edge was already recorded before delegate mutation
                // by our current UI/MediaSession viewer writer. Do not mint a
                // second viewer sequence when the SDK reports its consequence.
                val intercepted = interceptedTransportCallbacks.firstOrNull()
                if (intercepted?.first === player && intercepted.second == playWhenReady) {
                    interceptedTransportCallbacks.removeFirst()
                    sampleTargetPresentationDeadline()
                    return
                }
                // Play from the notification, a headset or the Assistant reaches
                // the player directly rather than through `playPause()`, and
                // must open the replacement for a retired paused session too.
                if (playWhenReady && !playbackIntent.playbackRequested) {
                    currentPausedRetirement()?.let(::reopenAfterPausedRetirement)
                }
                playbackIntent.setPlaybackRequested(playWhenReady)
                samplePreparedCommitFrameBudget()
                // The client's own fact, on the edge it already listens to: no
                // new detector and no new timer. A `buffering` fault is about a
                // player that wants media, so the viewer pausing makes it about
                // nothing — but the owner's stop-before-raise arrives here too,
                // and it is the owner deciding rather than the viewer.
                viewerTransport.report(playWhenReady)?.let(surfaceOwner::playbackRequested)
                // A notification can ask a stopped video activity to play.
                // Remember that viewer intent, then keep the owner pause in force.
                if (playWhenReady && !presentationForeground && !plan.isAudioOnly) {
                    setPresentationForeground(foreground = false)
                    applyEffectivePlayWhenReady()
                }
            }
            sampleTargetPresentationDeadline()
        }

        override fun onMediaItemTransition(mediaItem: MediaItem?, reason: Int) {
            pgsOverlay.itemChanged()
        }
    }

    /** One actual-output listener carrying the exact mutation it can settle. */
    private var presentationListener: Player.Listener? = null
    private var recipePresentationFrame: Pair<Long, Long>? = null
    private var firstVideoFrameForSeek: Triple<Long, Long, Int>? = null

    /**
     * Registered through [addPlayerListener], not on `player` directly.
     *
     * A prepared replacement swaps the player out from under this controller,
     * and only the listeners the registry knows about are carried across. A
     * listener bound to whichever player happened to be current stops firing at
     * the commit — and this is the one listener that clears a pending seek, so
     * losing it means the destination never settles: the target-presentation
     * deadline reports STARVED about a player that is rendering fine, reopens
     * the session, and on its second firing tells the viewer their place is
     * saved and stops.
     */
    private fun disarmVideoPresentation() {
        presentationListener?.let(::removePlayerListener)
        presentationListener = null
    }

    private fun armVideoPresentation(sequence: Long) {
        disarmVideoPresentation()
        // Audio-only destinations settle from two advancing player-clock
        // samples; they will never render a video frame.
        if (plan.videoCodec == null) return
        val presentationAttempt = playbackTelemetry.presentationAttempt()
        val captured = object : Player.Listener {
            override fun onRenderedFirstFrame() {
                if (presentationListener !== this || !playbackIntent.isCurrent(sequence)) return
                val position = realPosition()
                firstVideoFrameForSeek = Triple(
                    sequence,
                    position,
                    player.videoDecoderCounters?.renderedOutputBufferCount ?: 0,
                )
                if (recipeExecution?.sequence == sequence) {
                    recipePresentationFrame = sequence to position
                    completeRecipeExecution()
                    return
                }
                val recipe = selectionRecipe ?: return
                if (!recipeOwnership.canPresent(recipe)) return
                if (!playbackTelemetry.presentedVideoFrame(
                        playbackIntent, position, sequence, monotonicNowMs(), presentationAttempt,
                    )
                ) return
                playbackControl.playerChanged()
                removePlayerListener(this)
                if (presentationListener === this) presentationListener = null
            }
        }
        presentationListener = captured
        addPlayerListener(captured)
    }

    private fun markIntentExecuted(
        sequence: Long,
        recipe: PlaybackRecipeOwnership.Claim = currentRecipe(),
        inPlace: Boolean = false,
    ) {
        if (recipeOwnership.needsMediaReplacement(recipe) || !playbackIntent.isCurrent(sequence)) return
        recipeExecution = RecipeExecution(sequence, recipe, inPlace)
        recipePresentationFrame = null
        firstVideoFrameForSeek = null
        playbackIntent.markExecuted(
            sequence,
            observedAtMs = monotonicNowMs(),
            playbackActive = player.isPlaying,
            playbackRate = player.playbackParameters.speed.toDouble(),
        )
        armVideoPresentation(sequence)
        completeRecipeExecution()
    }

    private fun completeRecipeExecution() {
        if (textSelectionArmed || audioSelectionArmed) return
        val recipe = selectionRecipe ?: return
        if (!recipeOwnership.selectionApplied(recipe)) return
        val execution = recipeExecution ?: return
        if (execution.recipe != recipe || !recipeOwnership.canPresent(recipe) ||
            !playbackIntent.isCurrent(execution.sequence)
        ) return
        recipeExecution = null
        val sequence = execution.sequence
        val presented = if (execution.inPlace) {
            playbackIntent.presentedInPlace(sequence)
        } else {
            recipePresentationFrame?.takeIf { it.first == sequence }
                ?.let { playbackTelemetry.presentedVideoFrame(playbackIntent, it.second, sequence, monotonicNowMs()) } == true
        }
        if (presented) {
            playbackControl.playerChanged()
            disarmVideoPresentation()
        }
    }

    init {
        applyEffectivePlayWhenReady()
        if (playbackIntent.desiredQuality == PlaybackQuality.Auto &&
            playbackIntent.automaticCandidateId == null &&
            tv.plurx.app.data.Session.displayAwareAuto &&
            tv.plurx.app.data.Session.displayAwareAutoProtocol == "route-v1" &&
            plan.displayAwareAutoProtocol == "route-v1") {
            autoCatalog.firstOrNull { it.hasValidIdentity && it.id == plan.qualityCandidateId }?.let {
                playbackIntent.requestAutomaticCandidate(it.id, it.target_height)
            }
        }
        player.addListener(listener)
        player.addAnalyticsListener(preparedSwitchAnalytics(player))
        pgsOverlay.select(selectedSubtitle.takeIf { subtitleDelivery == SubtitleDelivery.BitmapOverlay })
        attachSurfaceGeneration()
        targetPresentationWatchdogJob = scope.launch {
            while (isActive) {
                sampleTargetPresentationDeadline()
                sampleSurface(realPosition(), monotonicNowMs())
                delay(targetPresentationDeadline.nextSampleDelayMs(monotonicNowMs()))
            }
        }
        stallWatchdogJob = scope.launch {
            while (isActive) {
                settleAudioPlaybackIntentIfPresented()
                playbackControlPlayerChanged()
                val observedAtMs = monotonicNowMs()
                // Retrospective telemetry still records the final interruption
                // duration, but recovery is owned by the open deadline below.
                playbackTelemetry.sampleStall(establishedPlayback, observedAtMs)
                val mediaPositionMs = realPosition()
                val openStall = openStallTracker.sample(
                    playbackRequested = player.playWhenReady && presentationForeground &&
                        playbackIntent.pendingSeek == null,
                    playbackEnded = player.playbackState == Player.STATE_ENDED,
                    establishedPlayback = establishedPlayback,
                    positionMs = mediaPositionMs,
                    observedAtMs = observedAtMs,
                )
                // The surface's presentation evidence is this same film-clock
                // sample (contract §3.4): no second detector and no second
                // timer, just the reading the stall tracker already took.
                sampleSurface(mediaPositionMs, observedAtMs)
                if (openStall != null) {
                    autoUpgradeEvidence.bind(autoTransfersByPlayer[player], mediaMutationEpoch, observedAtMs)
                    autoUpgradeEvidence.stalled(observedAtMs)
                    autoUpgradeSinceMs = null
                    if (openStall.controlMayDefer) {
                        playbackStallCount += 1
                    }
                    onStall(openStall)
                }
                sessionlessStallRecoveryPositionMs?.let { recoveredAt ->
                    if (kotlin.math.abs(realPosition() - recoveredAt) >= 5_000) {
                        sessionlessStallRecoveryUsed = false
                        sessionlessStallRecoveryPositionMs = null
                    }
                }
                delay(openStallTracker.nextSampleDelayMs(monotonicNowMs(), establishedPlayback))
            }
        }
    }

    fun startAt(
        ms: Long,
        reason: String = if (ms > 0) "resume" else "cold-start",
        observedAtMs: Long = monotonicNowMs(),
    ) {
        val position = ms.coerceAtLeast(0)
        if (displayModeStartRequested) return
        displayModeStartRequested = true
        val matcher = displayModeMatcher
        val owner = displayModeOwner
        if (matcher == null || owner == null) {
            restartAt(position, reason, observedAtMs)
            return
        }
        scope.launch {
            val result = matcher.match(owner, plan.sourceFrameRate, displayModeMatchEnabled)
            if (!playbackControlBootstrapFence.isActive()) return@launch
            reportDisplayMode(result)
            restartAt(position, reason, observedAtMs)
        }
    }

    fun realPosition(): Long {
        return realMediaPositionMs(
            playerPositionMs = player.currentPosition,
            directTransport = directTransport,
            sessionIsVod = sessionIsVod,
            progressiveTransport = progressiveTransport,
            progressiveOriginMs = progressiveMediaOrigin.currentOriginMs(),
            sessionBaseMs = baseMs,
        )
    }

    /**
     * Where a FILM position sits on the player's own timeline.
     *
     * [realPosition] maps the other way, and M5's `BEHIND_LIVE_WINDOW`
     * recovery needs this one: it reads the last real position in film time and
     * then has to hand `ExoPlayer.seekTo` a number in the player's.
     */
    private fun playerTimelinePositionMs(filmPositionMs: Long): Long =
        playerLocalPositionMs(
            filmPositionMs = filmPositionMs,
            directTransport = directTransport,
            sessionIsVod = sessionIsVod,
            progressiveTransport = progressiveTransport,
            progressiveOriginMs = progressiveMediaOrigin.currentOriginMs(),
            sessionBaseMs = baseMs,
        )

    /**
     * A refusal Media3 carried, classified.
     *
     * [source] is null when the body was legible but no source row claims it —
     * a 503 whose code is in no row's `codes` list. The sentence and the
     * position are still the server's and are still used; what the client will
     * not do is borrow a class from a row that does not name this refusal.
     */
    private data class MediaRefusal(val source: String?, val message: String?, val positionMs: Long?)

    /** The viewer's newest film target, even while the predecessor still renders. */
    fun positionForPlaybackIntent(): Long =
        playbackIntent.positionForPlaybackIntent(realPosition())

    /** An explicit Retry is a new viewer command, unlike an automatic reopen. */
    fun prepareViewerRetry(): Long {
        val target = positionForPlaybackIntent()
        // The owner's stop-before-raise set `playWhenReady = false`, which
        // Media3 reports as a USER_REQUEST and which therefore cleared the
        // viewer's transport intent. That intent outlives this controller — it
        // belongs to the playback, not the plan — so without this the
        // replacement Retry builds would attach paused behind a picture the
        // viewer just asked to see again.
        playbackIntent.setPlaybackRequested(true)
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        playbackIntent.beginSeek(target, realPosition())
        sampleTargetPresentationDeadline()
        return target
    }

    private fun beginPlaybackAttempt(
        reason: String,
        observedAtMs: Long = monotonicNowMs(),
    ): PlaybackAttempt {
        autoUpgradeEvidence.reset()
        establishedPlayback = false
        openStallTracker.reset()
        return playbackTelemetry.begin(reason, observedAtMs, playbackIntent.pendingSeek?.sequence)
    }

    fun seekTo(targetMs: Long) {
        playbackIntent.noteViewerDestination()
        resetAutoQualityBudgetForViewer()
        val t = targetMs.coerceIn(0, if (plan.durationMs > 0) plan.durationMs else Long.MAX_VALUE)
        enqueueSeek { playbackIntent.beginSeek(t, realPosition()) }
    }

    /** Repeated transport nudges accumulate while the newest seek is coalescing. */
    fun seekBy(deltaMs: Long) {
        playbackIntent.noteViewerDestination()
        resetAutoQualityBudgetForViewer()
        enqueueSeek {
            playbackIntent.beginRelativeSeek(deltaMs, realPosition(), plan.durationMs)
        }
    }

    private fun enqueueSeek(begin: () -> PlaybackIntent.PendingSeek) {
        if (!playbackControlBootstrapFence.isActive()) return
        revokeAutoBoundaryTransport()
        // The seek is executed on the incumbent without waiting for anything
        // optional. The original-quality re-plan this boundary is owed runs
        // afterwards, from the ordinary Auto evaluation, as a prepared
        // replacement the playing incumbent hands off to.
        autoBoundaryReplan.arm(viewerTransportLifetime)
        stallGuard.viewerSeek {
            playbackControl.clearVerdict()
            val pending = begin()
            sampleTargetPresentationDeadline()
            val publicationEpoch = mediaMutationEpoch
            seekJob?.cancel()
            seekJob = scope.launch {
                if (!publishIntent(pending, publicationEpoch = publicationEpoch)) return@launch
                delay(SEEK_COALESCE_MS)
                if (!playbackControlBootstrapFence.isActive() ||
                    mediaMutationEpoch != publicationEpoch ||
                    !playbackIntent.isCurrent(pending.sequence) || preparedLedger.isSwitched
                ) return@launch
                executeSeek(pending.targetMs, pending.sequence)
            }
        }
    }

    private suspend fun publishIntent(
        pending: PlaybackIntent.PendingSeek,
        quality: PlaybackQuality = playbackIntent.desiredQuality,
        publicationEpoch: Long,
    ): Boolean {
        return publishReplacementIntent(
            playbackIntent,
            pending,
            quality,
            isActive = {
                playbackControlBootstrapFence.isActive() && mediaMutationEpoch == publicationEpoch
            },
            publish = playbackControl::reportIntent,
        )
    }

    /** Execute only the final target after its immutable intent was enqueued. */
    private fun executeSeek(t: Long, sequence: Long) {
        if (!playbackControlBootstrapFence.isActive()) return
        // A VOD seek never reopens the session and so never reaches
        // [restartAt], but it moves the playhead away from the point the
        // successor was primed to — so it abandons the preparation too.
        abandonPreparedReplacement(failed = false)
        mediaMutationEpoch += 1
        attachSurfaceGeneration()
        if (!continuousOwnsQuality(playbackIntent.qualityForMedia()) && planReplacement.route(playbackIntent)) return
        val attempt = beginPlaybackAttempt("seek")
        val recipe = currentRecipe()
        if (recipeOwnership.needsMediaReplacement(recipe)) {
            restartAt(t, "selection")
            return
        }
        armTrackSelections(recipe)
        when (recipe.recipe.desiredTransport) {
            PlaybackMediaTransport.Direct -> {
                player.seekTo(t)
                playbackTelemetry.prepared(attempt)
                markIntentExecuted(sequence)
            }
            PlaybackMediaTransport.ProgressiveRemux -> {
                leaveSessionPlayback()
                Session.resetMediaFailover()
                baseMs = t
                val uri = remuxUri(t)
                activeMediaPath = relativeMediaPath(uri)
                progressiveMediaOrigin.begin(uri, t)
                player.setMediaItem(MediaItem.fromUri(uri))
                attachRecipe(recipe)
                markIntentExecuted(sequence, recipe)
                player.prepare()
                playbackTelemetry.prepared(attempt)
                applyEffectivePlayWhenReady()
                armTrackSelections()
            }
            // A cached session holds the whole stream: native seeking, no
            // session churn. A live one can't be range-sought, so it reopens.
            PlaybackMediaTransport.HlsCopy,
            PlaybackMediaTransport.HlsTranscode -> if (sessionIsVod) {
                player.seekTo(t)
                playbackTelemetry.prepared(attempt)
                markIntentExecuted(sequence)
            } else {
                openSession(t, attempt, sequence)
            }
        }
    }

    /**
     * The viewer chose a different rung. Tell the server, then wait for it.
     *
     * Until this waited, the M6 prepared-replacement path could not be reached
     * by a viewer at all: this routed the replacement unconditionally the
     * instant the server had been told, which disposes this controller and its
     * player — so the `prepare` the server was at that moment staging arrived
     * at a reporter that no longer existed. Every rung change was a reopen, and
     * the whole handoff was dead code behind a live wire.
     *
     * The wait costs the viewer nothing: playback continues throughout, and the
     * only thing that changes at the end of it is whether the new rung arrives
     * on a second pipeline or through the ordinary reopen this always did.
     */
    fun prepareReplacement(
        quality: PlaybackQuality,
        onPrepared: (Long, PlaybackQuality) -> Unit,
    ) {
        if (!playbackControlBootstrapFence.isActive()) return
        val incumbentSelection = playbackControlSelection().quality
        retainedQualityRequest = null
        resetAutoQualityBudgetForViewer()
        // A viewer-chosen rung ends the original-first re-plan a previous
        // boundary owed; Auto starts over from this selection.
        autoBoundaryReplan.clear()
        autoDesiredCandidate = null
        playbackIntent.requestAutomaticCandidate(null, null)
        planReplacement.retain(onPrepared)
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        val tappedAtMs = monotonicNowMs()
        // A rung is not a destination. Recording this as a pending seek pinned
        // the fallback's reopen to the playhead at the tap, which after a
        // seconds-long wait is seconds of playback thrown away.
        val pending = playbackIntent.beginQualityChange(quality, tappedAtMs)
        val publicationEpoch = mediaMutationEpoch
        scope.launch {
            val current = publishQualityChange(pending, publicationEpoch)
            if (!current) return@launch
            // One owner from here to commit, retention, or one recovery reopen.
            // A rung the viewer already left is not worth routing to, so the
            // previous change is told it has been replaced rather than left to
            // fire later.
            directedChange?.superseded()
            val change = DirectedChange(epoch = publicationEpoch, quality = quality,
                pending = pending, incumbentSelection = incumbentSelection)
            directedChange = change
            val continuous = liveContinuous()
            val row = quality.rungHeight?.let { continuous?.rendition(it) }
                ?: if (quality == PlaybackQuality.Auto) continuous?.rendition(player.videoSize.height) else null
            if (continuous != null && row != null) {
                try {
                    val applied = withTimeout(12_000) {
                        continuous.change(row, baseMs + maxOf(player.bufferedPosition, player.currentPosition).coerceAtLeast(0), quality == PlaybackQuality.Auto, pending.sequence)
                    }
                    if (!applied) continuousChangeFailed(change, retained = true)
                } catch (_: TimeoutCancellationException) { continuousChangeFailed(change, retained = false) }
                catch (error: CancellationException) { throw error }
                catch (_: Exception) { continuousChangeFailed(change, retained = false) }
                return@launch
            }
            when (val step = playbackControl.awaitPreparedOffer(tappedAtMs)) {
                // The same entry point the reporter's push uses, and idempotent
                // with it: the ledger answers `Same` for an id it has seen, so
                // whichever of the two arrives first builds the one pipeline.
                is PreparedOfferWait.Step.Offered -> onPrepareAction(step.action)
                is PreparedOfferWait.Step.Reopen -> fallBackDirectedChange(change, step.reason)
                is PreparedOfferWait.Step.KeepWaiting ->
                    error("awaitPreparedOffer returned a non-terminal step")
            }
        }
    }

    /**
     * The viewer's directed change, from the tap until it is honoured.
     *
     * Null when the last stream change was not one the viewer directed — a
     * fallback recipe, a recovery reopen, a title start. A preparation that did
     * not come from a directed change keeps the release-only behaviour it had:
     * there is no rung waiting to be applied, so there is nothing to apply.
     */
    private var directedChange: DirectedChange? = null

    var retainedQualityRequest by mutableStateOf<PlaybackQuality?>(null)
        private set

    fun applyRetainedQualityWithRestart() {
        val quality = retainedQualityRequest ?: return
        if (!playbackControlBootstrapFence.isActive()) return
        retainedQualityRequest = null
        directedChange?.superseded()
        playbackIntent.adoptQuality(quality)
        val observed = realPosition()
        val pending = playbackIntent.beginSeek(playbackIntent.positionForPlaybackIntent(observed), observed, quality)
        sampleTargetPresentationDeadline()
        val publicationEpoch = mediaMutationEpoch
        scope.launch {
            if (publishIntent(pending, quality, publicationEpoch)) {
                planReplacement.route(playbackIntent, force = true)
            }
        }
    }

    /** When the directed change was published, for the committed-in row. */
    private var directedChangeTappedAtMs: Long? = null

    private suspend fun publishQualityChange(
        pending: PlaybackIntent.PendingQualityChange,
        publicationEpoch: Long,
    ): Boolean {
        directedChangeTappedAtMs = pending.tappedAtMs
        return publishQualityChangeIntent(
            playbackIntent,
            pending,
            isActive = {
                playbackControlBootstrapFence.isActive() && mediaMutationEpoch == publicationEpoch
            },
            publish = playbackControl::reportIntent,
        )
    }

    private fun continuousChangeFailed(change: DirectedChange, retained: Boolean) {
        if (directedChange !== change || change.isSettled || change.epoch != mediaMutationEpoch) return
        retainedQualityRequest = change.quality
        if (retained) {
            val pending = change.pending ?: return
            val incumbent = change.incumbentSelection ?: return
            if (!playbackIntent.retainQualityChange(pending, null)) return
            change.settleFailureOnce(mediaMutationEpoch, true, retain = {
                if (playbackIntent.retainFailedQuality(pending, incumbent)) logQualitySwitch("retained_current", change.quality)
            }, reopen = {})
            raiseDegradedNotice("This quality could not be selected continuously. Playback continues. Retry or apply with restart in Playback settings.")
        } else {
            raiseDegradedNotice("Quality change could not be confirmed. Playback continues while its outcome is checked. Retry or apply with restart in Playback settings.")
        }
        playbackControl.reportEvidence()
    }

    /**
     * Settle a failed optional change once. Healthy playback retains its
     * standing recipe; only an unhealthy incumbent takes the recovery reopen.
     *
     * The position is sampled *here* rather than at the tap, which is the whole
     * reason a rung change no longer records itself as a seek: whatever played
     * while the viewer waited is theirs to keep. A viewer seek during the wait
     * has its own pending destination and owns this one, so it is preferred
     * over the playhead.
     */
    private fun fallBackDirectedChange(change: DirectedChange, reason: String): Boolean {
        if (directedChange !== change) return false
        val pending = change.pending
        if (pending != null && !playbackIntent.retainQualityChange(pending, null)) return false
        val incumbentHealthy = establishedPlayback && player.playerError == null &&
            player.playbackState == Player.STATE_READY && pending != null && change.incumbentSelection != null
        val routed = change.settleFailureOnce(mediaMutationEpoch, incumbentHealthy, retain = {
            if (playbackIntent.retainFailedQuality(pending!!, change.incumbentSelection!!)) {
                retainedQualityRequest = change.quality
                preparedRollbackReopen = null
                raiseDegradedNotice("Quality change did not complete. Playback continues. Retry or apply with restart in Playback settings.")
                playbackControl.reportEvidence()
            }
        }, reopen = {
            val observed = realPosition()
            playbackIntent.beginSeek(
                playbackIntent.positionForPlaybackIntent(observed), observed, change.quality,
            )
            sampleTargetPresentationDeadline()
            planReplacement.route(playbackIntent, force = true)
        })
        // Retention is a client outcome. Preserve a server refusal label only
        // when recovery really routes the requested quality through a reopen.
        val via = if (reason == "declined" || reason == "timed_out") reason else "fallback"
        if (routed) logQualitySwitch(if (incumbentHealthy) "retained_current" else via, change.quality)
        return routed
    }

    /**
     * A prepared attempt ended without putting the viewer's rung on screen.
     *
     * Every terminal failure of an offered successor comes through here, so the
     * rung the viewer asked for survives the failure of the mechanism that was
     * meant to deliver it. Before this, such a failure released the successor,
     * acknowledged `failed`, and left the viewer on the rung they had changed
     * away from with nothing to say so.
     *
     * A preparation that did not come from a directed change — the server may
     * stage one for its own reasons — keeps the release-only behaviour: there
     * is no viewer intent outstanding, so there is nothing to fall back to.
     */
    private fun fallBackAfterPreparedFailure(reason: String = "fallback"): Boolean {
        if (autoPreparing) { failAutoPreparation(); return true }
        val change = directedChange ?: return false
        return fallBackDirectedChange(change, reason)
    }

    /**
     * A reopen this controller did not start is still a reopen. The viewer's
     * rung rides it rather than being lost to whichever recovery got there
     * first — a stall reopen, a transport failover, a track change.
     */
    private fun carryDirectedChangeIntoReopen(positionMs: Long) {
        val change = directedChange ?: return
        if (change.isSettled) return
        // The route below needs a destination and a rung change deliberately
        // records none. This reopen has one: the position it is restarting at.
        if (playbackIntent.pendingSeek == null) {
            playbackIntent.beginSeek(positionMs, positionMs, change.quality)
        }
        change.superseded()
        logQualitySwitch("fallback", change.quality)
    }

    /**
     * How a rung change was delivered, as a client-log event.
     *
     * A prepared outcome names a proved handoff; retained current names no
     * delivery change. Other outcomes describe the actual recovery route.
     */
    private fun logQualitySwitch(via: String, quality: PlaybackQuality) {
        val elapsedMs = directedChangeTappedAtMs?.let {
            (monotonicNowMs() - it).coerceAtLeast(0L)
        }
        PreparedReplacementAdvisory.recordOutcome(
            PreparedReplacementAdvisory.outcomeLabel(via, elapsedMs),
        )
        // M3. The switch's own measurements, for the developer screen, which
        // is not inside a playback session and so cannot ask this controller.
        PreparedReplacementAdvisory.recordSwitch(preparedSwitch.reading())
        playbackTelemetry.report(
            event = "quality_switch",
            level = if (via == "prepared") "info" else "warn",
            message = "viewer quality change outcome: $via",
            detail = buildString {
                append("via=").append(via)
                append(" quality=").append(quality.rungHeight?.toString() ?: quality.toString())
                elapsedMs?.let { append(" elapsed_ms=").append(it) }
            },
        )
    }

    fun playPause() {
        setViewerPlaybackRequested(!playbackIntent.playbackRequested)
    }

    private fun writeViewerDelegate(requested: Boolean) {
        val effective = requested && !lifecyclePaused
        if (player.playWhenReady != effective) {
            if (interceptedTransportCallbacks.size >= 8) interceptedTransportCallbacks.clear()
            interceptedTransportCallbacks.addLast(player to effective)
        }
        player.playWhenReady = effective
    }

    /** A newer viewer transport edge supersedes the boundary that asked for
     * an optional original preparation. It is withdrawn, not failed: nothing
     * about the candidate was disproved, so it earns no backoff. A switch
     * already in flight is left to settle on its own first frame. */
    private fun revokeAutoBoundaryTransport() {
        viewerTransportLifetime = Any()
        val boundary = autoBoundaryAttempt ?: return
        if (preparedLedger.isSwitched) return
        abandonPreparedReplacement(failed = false)
        withdrawAutoBoundary(boundary)
    }

    /** The same viewer writer serves the UI and pre-dispatch MediaSession
     * transport. The delegate follows the viewer at once on every edge; a
     * resume after a long explicit pause only arms the Auto boundary re-plan,
     * which never holds the delegate. */
    private fun setViewerPlaybackRequested(requested: Boolean) {
        if (!playbackControlBootstrapFence.isActive()) return
        val now = monotonicNowMs()
        val paused = explicitViewerPause
        val wasRequested = playbackIntent.playbackRequested
        // Latest viewer intent must already govern any rollback caused by
        // revocation; an explicit Pause cannot transiently restart its owner.
        stallGuard.setPlaybackRequested(playbackIntent, requested) { }
        viewerTransport.report(requested)?.let(surfaceOwner::playbackRequested)
        revokeAutoBoundaryTransport()
        if (requested && !wasRequested) {
            currentPausedRetirement()?.let { retired ->
                explicitViewerPause = null
                reopenAfterPausedRetirement(retired)
                return
            }
        }
        if (plan.isAudioOnly && requested && !wasRequested) {
            PlaybackService.attach(context, mediaSession)
        }
        if (!requested) {
            // A repeated Pause revokes the optional owner, but does not renew
            // the original explicit pause's observation timestamp.
            if (wasRequested || paused == null || paused.player !== player || paused.session != sessionId ||
                paused.epoch != mediaMutationEpoch) {
                explicitViewerPause = ExplicitViewerPause(player, sessionId, mediaMutationEpoch, now)
            }
        } else explicitViewerPause = null
        playbackControl.clearVerdict()
        val edge = viewerTransportEdge(requested, wasRequested, paused?.atMs,
            paused != null && paused.player === player && paused.session == sessionId &&
                paused.epoch == mediaMutationEpoch, now)
        // The first-frame budget of a pending prepared commit is sampled
        // before the delegate changes, so a Pause does not spend it.
        samplePreparedCommitFrameBudget()
        writeViewerDelegate(edge.delegatePlayWhenReady)
        if (edge.armsAutoBoundaryReplan) autoBoundaryReplan.arm(viewerTransportLifetime)
        playbackControl.playerChanged()
    }

    /**
     * The presentation this pause held no longer exists on the server, so there
     * is nothing to resume in place. Record Play, then open the one replacement
     * at the saved position (or the seek the viewer made while paused).
     */
    private fun reopenAfterPausedRetirement(retired: PausedRetirement) {
        pausedRetirement = null
        // A MediaSession Play on an errored player has already called
        // `prepare()` on the dead playlist. Stop cancels that load, so its
        // 404/410 cannot reach `onPlayerError` with Play now requested.
        player.stop()
        if (plan.isAudioOnly) PlaybackService.attach(context, mediaSession)
        playbackControl.clearVerdict()
        stallGuard.setPlaybackRequested(playbackIntent, true) {
            samplePreparedCommitFrameBudget()
            player.playWhenReady = it && !lifecyclePaused
        }
        val position = pausedRetirementReopenPositionMs(
            playbackIntent.pendingSeek?.targetMs,
            retired,
        ) { realPosition() }
        surfaceOwner.logOnly(mediaMutationEpoch, "paused retirement reopen at ${position}ms")
        restartAt(position, "paused-retirement")
        playbackControl.playerChanged()
    }

    fun release() {
        if (!playbackControlBootstrapFence.isActive()) return
        playbackTelemetry.cancelPending()
        playbackControlBootstrapFence.release()
        displayModeOwner?.let { owner -> displayModeMatcher?.reset(owner) }
        seekJob?.cancel()
        stallWatchdogJob.cancel()
        targetPresentationWatchdogJob.cancel()
        targetPresentationDeadline.suspendOwner(targetPresentationOwner, monotonicNowMs())
        planReplacement.release()
        clearStatusPolling()
        pgsOverlay.release()
        stallGuard.invalidateForUserAction()
        // The terminal acknowledgement a live preparation is owed, and then one
        // exchange to carry it — on the view model's scope, because this
        // composition's is cancelled in the same pass that called this. Without
        // that hand-off the acknowledgement is published into a reporter whose
        // pump is cancelled before it can build a request, and the staging's
        // encoder runs for another 330 seconds after the viewer left.
        abandonPreparedReplacement(failed = false)
        awaitingCommitFrameSinceMs = null
        preparedCommitFrameBudget = null
        val settling = settlingSnapshotIfOwed()
        val endingSession = sessionId
        // Every read below this line is against a player that is about to be
        // released, so the observation is closed first.
        controlObservationIsClosed = true
        preparedRollbackReopen = null
        collectRetiredPlayer()
        endPlaybackControl(settling) { endingSession?.let(::releaseOwnedSession) }
        // A verdict survives a reopen because the failure it explains usually
        // arrives after one. It must not survive the title: a confident
        // sentence about the wrong film is worse than a generic one.
        //
        // This runs after the hand-off above and empties the slot the session
        // publishes captures into — which is why a settled reporter does not
        // read that slot again. See `PlaybackControlReporter.settled`: without
        // it, this line silently threw the teardown exchange away.
        playbackControl.clearVerdict()
        controlWaitingSince = null
        controlObservationOverride = null
        controlRenderOverride = null
        controlEvidencePositionMs = null
        sessionId = null
        currentMediaSessionTransport = null
        explicitViewerPause = null
        autoBoundaryReplan.clear()

        surfaceOwner.retire(mediaMutationEpoch)
        player.removeListener(listener)
        disarmVideoPresentation()
        if (plan.isAudioOnly) PlaybackService.detach(context, mediaSession)
        mediaSession.release()
        preparedVideoSurfaces?.release()
        continuousAttachment?.closeAdmission()
        player.release()
        continuousAttachment?.finishAfterRelease()
        continuousAttachment = null
        continuousPending.values.forEach { it.owner.finishAfterRelease() }
        continuousPending.clear()
        continuousAttempt?.close()
        continuousAttempt = null
    }

    fun switchAudio(index: Long) {
        if (!playbackControlBootstrapFence.isActive()) return
        if (index == selectedAudio) return
        val position = positionForPlaybackIntent()
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        selectedAudio = index
        refreshAutoCatalogForViewerRecipe()
        val pending = playbackIntent.beginSeek(position, position)
        currentRecipe()
        sampleTargetPresentationDeadline()
        val publicationEpoch = mediaMutationEpoch
        scope.launch {
            if (publishIntent(pending, publicationEpoch = publicationEpoch)) restartAt(position, "audio")
        }
    }

    /**
     * Show subtitle stream [index], or nothing when it is null.
     *
     * The routing lives in [subtitleRoute]; all this does is carry the answer.
     * The point of the split is that every arm below used to be one arm — a
     * burn — so an SRT on a 4K HDR remux paid for a full re-encode to show a
     * track the server can hand over as a WebVTT rendition for free.
     */
    fun switchSubtitle(index: Long?): Boolean {
        if (!playbackControlBootstrapFence.isActive()) return false
        val track = index?.let(::trackFor)
        // An index the decision never listed cannot be routed, and guessing
        // here means guessing "burn". Ignore it instead.
        if (index != null && track == null) return false
        if (subtitleBurnWouldDiscardHdr(track, deliveredRange)) {
            raiseDegradedNotice(HDR_SUBTITLE_NOTICE)
            return false
        }
        // Keep an executed seek's optimistic film target even while the
        // predecessor item still exposes its old clock.
        val position = positionForPlaybackIntent()
        val inheritedSeek = playbackIntent.needsSeekForInPlaceSelection()
        val route = routeSubtitle(track, subtitleDelivery)
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        selectedSubtitle = index
        subtitleDelivery = route.delivery
        refreshAutoCatalogForViewerRecipe()
        val recipe = currentRecipe()
        val pending = playbackIntent.beginSeek(position, position)
        sampleTargetPresentationDeadline()
        val publicationEpoch = mediaMutationEpoch
        scope.launch {
            if (!publishIntent(pending, publicationEpoch = publicationEpoch)) return@launch
            if (planReplacement.route(playbackIntent)) return@launch
            if (recipeOwnership.needsMediaReplacement(recipe)) {
                restartAt(position, "quality")
            } else {
                // A subtitle change supersedes the coalesced seek command, so
                // it must execute that destination itself. Only a track-only
                // change can settle without a new target frame.
                if (inheritedSeek) {
                    executeSeek(position, pending.sequence)
                } else {
                    armTrackSelections(recipe)
                    markIntentExecuted(pending.sequence, recipe, inPlace = true)
                }
            }
        }
        return true
    }

    fun allowsPictureInPictureCommand(): Boolean {
        if (!pgsOverlayIsActive) return true
        raiseDegradedNotice(PGS_OVERLAY_PIP_NOTICE)
        return false
    }

    /** Apply an A/V correction to this controller only and reopen in place. */
    fun setAudioOffset(offsetMs: Long) {
        if (!playbackControlBootstrapFence.isActive()) return
        val position = positionForPlaybackIntent()
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        audioOffsetMs = offsetMs.coerceIn(-15_000, 15_000)
        // The correction can move a direct play onto the remuxer, and the
        // remuxer's progressive stream carries no subtitle tracks — so the
        // current selection has to be re-routed, not just replayed.
        subtitleDelivery =
            routeSubtitle(trackFor(selectedSubtitle), subtitleDelivery).delivery
        refreshAutoCatalogForViewerRecipe()
        val pending = playbackIntent.beginSeek(position, position)
        currentRecipe()
        sampleTargetPresentationDeadline()
        val publicationEpoch = mediaMutationEpoch
        scope.launch {
            if (publishIntent(pending, publicationEpoch = publicationEpoch)) restartAt(position, "audio")
        }
    }

    private fun restartAt(
        positionMs: Long,
        reason: String,
        observedAtMs: Long = monotonicNowMs(),
    ) {
        if (!playbackControlBootstrapFence.isActive()) return
        mediaMutationEpoch += 1
        attachSurfaceGeneration()
        // The client is opening a stream (§3.3 row 10). Every open reaches this
        // function, and the attach above has just dropped every fault about the
        // generation being replaced, so this is the first thing true about the
        // new one. It carries no sentence: the spinner's own copy is what the
        // screen falls back to, which is the text this window had before the
        // presenter owned it. A recovery step raised AFTER its `restartAt` is
        // newer and wins the progress tie, so a reopen still says why.
        //
        // Not raised for a viewer who is paused: the presenter's evidence needs
        // `playWhenReady` (§3.4), so a preparing fault over a paused open would
        // have nothing that could ever retire it.
        if (playbackIntent.playbackRequested) {
            surfaceOwner.preparingClientOpen(mediaMutationEpoch, SurfaceContext.Start)
        }
        carryDirectedChangeIntoReopen(positionMs)
        if (planReplacement.route(playbackIntent, retry = reason == "presentation-recovery")) return
        // A user-initiated restart (seek, quality switch, track change) resets
        // the stall reopen budget and invalidates any in-flight stall.
        stallGuard.invalidateForUserAction()
        // And it abandons any prepared successor: it was staged against the
        // selection the viewer just replaced, so switching to it would serve
        // the change before last.
        abandonPreparedReplacement(failed = false)
        Session.resetMediaFailover()

        val attempt = beginPlaybackAttempt(reason, observedAtMs)
        val executionSequence = playbackIntent.pendingSeek?.sequence
        val recipe = currentRecipe()
        when (recipe.recipe.desiredTransport) {
            PlaybackMediaTransport.Direct -> {
                leaveSessionPlayback()
                activeMediaPath = relativeMediaPath(plan.playUrl)
                player.setMediaItem(MediaItem.fromUri(plan.playUrl), positionMs)
                attachRecipe(recipe)
                executionSequence?.let { sequence ->
                    markIntentExecuted(sequence, recipe)
                }
                player.prepare()
                playbackTelemetry.prepared(attempt)
                applyEffectivePlayWhenReady()
                armTrackSelections()
            }
            PlaybackMediaTransport.ProgressiveRemux -> {
                leaveSessionPlayback()
                baseMs = positionMs
                val uri = remuxUri(positionMs)
                activeMediaPath = relativeMediaPath(uri)
                progressiveMediaOrigin.begin(uri, positionMs)
                player.setMediaItem(MediaItem.fromUri(uri))
                attachRecipe(recipe)
                executionSequence?.let { sequence ->
                    markIntentExecuted(sequence, recipe)
                }
                player.prepare()
                playbackTelemetry.prepared(attempt)
                applyEffectivePlayWhenReady()
                armTrackSelections()
            }
            PlaybackMediaTransport.HlsCopy,
            PlaybackMediaTransport.HlsTranscode ->
                openSession(positionMs, attempt, executionSequence)
        }
    }

    /**
     * Open an HLS session at `ms`, releasing the one it replaces. A seek is a
     * new session, exactly as the web player does it — unless the server
     * answers `vod`, which is the whole stream already on disk and seeks in
     * place. What kind of session — copy or transcode, renditions or a burn —
     * is [subtitleSessionBody]'s answer, so the shape of every request this
     * client sends is unit-tested rather than assembled inline.
     */
    private fun openSession(
        ms: Long,
        attempt: PlaybackAttempt,
        executionSequence: Long? = playbackIntent.pendingSeek?.sequence,
    ) {
        // The successor was staged against the session this call is replacing.
        // A reopen is a different stream, so the preparation is abandoned
        // rather than left for the server's 330 s deadline — which would hold
        // this session's one preparation slot for the rest of the film.
        abandonPreparedReplacement(failed = false)
        val requestVersion = stallGuard.beginRequest()
        val recipe = currentRecipe()
        val createBody = sessionBody(ms, recipe = recipe.recipe)
        val endingSession = sessionId
        // A typed Auto recovery keeps its predecessor alive through the create:
        // the server validates previous_session_id against a live session and
        // supersedes it atomically. Every other reopen releases it here, through
        // the owner that holds it (a continuous family or an ordinary session).
        if (createBody.reopen_reason == null || createBody.reopen_reason == tv.plurx.app.data.ReopenReason.Stall) {
            endPlaybackControl { endingSession?.let(::releaseOwnedSession) }
            sessionId = null
            rebindMediaSessionTransport()
        }
        clearStatusPolling()
        encoder = null
        sessionIsVod = false
        scope.launch {
            try {
                // M5 §4.6.1: row 6 is the START context, so the whole sequence
                // — the ladder AND its deadline watchdog — runs only where
                // there is nothing behind this create. `openSession` is also
                // the create path for seeks and quality changes, and a create
                // over a predecessor the viewer is still watching is a refused
                // CHANGE (row 7): stopping that player after sixty seconds is
                // the outcome §7 recipe (b) forbids outright.
                val startContext = !predecessorAttached()
                val retryEpoch = mediaMutationEpoch
                val hls = try {
                    sessionCreateCoordinator.createRetryingNotYet(
                        body = createBody.copy(control_sequence = playbackIntent.orderedControlSequence(playbackControl.controlSequence())),
                        startContext = startContext,
                        isCurrent = { stallGuard.isCurrent(requestVersion) },
                        isNotYet = { failure -> createIsStillBuilding(failure) },
                        onRetrying = { failure ->
                            surfaceOwner.preparingSessionCreate(
                                retryEpoch,
                                failure.message ?: STILL_BUILDING_SENTENCE,
                            )
                        },
                        onExhausted = { _ ->
                            playbackTelemetry.cancel(attempt)
                            surfaceOwner.exhaustedAfterCreateRetries(
                                retryEpoch,
                                CREATE_RETRY_EXHAUSTED_SENTENCE,
                            )
                        },
                    ) ?: return@launch
                } catch (cancelled: CancellationException) {
                    // The screen left composition (or a newer request superseded
                    // this one) — the caller saying stop, not the server failing.
                    // Swallowing it here would show a failure state for a stream
                    // nobody is waiting for any more.
                    throw cancelled
                } catch (error: Exception) {
                    if (stallGuard.isCurrent(requestVersion)) {
                        playbackTelemetry.report(
                            event = "playback_error",
                            level = "error",
                            message = "session create failed before Media3 started",
                            code = refusalStatusOf(error),
                            detail = redactedFailureDetail("session_create", error),
                            attempt = attempt,
                        )
                        playbackTelemetry.cancel(attempt)
                        Log.w(
                            "PlurxPlayback",
                            "session create failed ${redactedFailureDetail("session_create", error)}",
                        )
                        raiseSessionCreateFailure(
                            error,
                            "The server couldn't start this stream.",
                            // Sound here: on a cold start nothing was ever set
                            // on the player, so there is genuinely nothing
                            // behind this failure.
                            predecessor = predecessorAttached(),
                        )
                    }
                    return@launch
                }
                // A later seek or track switch won while this request was in
                // flight. Release this now-stale server session instead of letting
                // its older timeline replace the current one.
                if (hls.session_id !in continuousPending) retireContinuousForReplacement()
                sessionCreateCoordinator.attachIfCurrent(hls, { stallGuard.isCurrent(requestVersion) }) { hls ->
                    sessionId = hls.session_id
                    rebindMediaSessionTransport()
                    beginPlaybackControl(hls)
                    startStatusPolling(hls.session_id)
                    // Save this session's resolved height so the stall-reopen budget
                    // can compare each stall response against the predecessor rung.
                    stallReopenBudget.seed(hls.height)
                    encoder = hls.encoder
                    sessionIsVod = hls.vod
                    adoptSessionDelivery(hls)
                    // A cached session is the whole stream on disk: its timeline
                    // starts at zero and the player seeks, exactly like direct play.
                    // The owner reporting that its recovery produced this
                    // generation; `recovering` faults are retired by it.
                    surfaceOwner.ownerSuccess(mediaMutationEpoch)
                    val timeline = sessionPlaybackTimeline(hls, requestedStartMs = ms)
                    baseMs = timeline.baseMs
                    activeMediaPath = relativeMediaPath(hls.playlist_url)
                    val owned = continuousPending.remove(hls.session_id)
                    if (owned != null) {
                        continuousAttachment = owned.owner
                        player.setMediaSource(owned.source, timeline.attachPositionMs)
                    } else player.setMediaItem(
                        MediaItem.fromUri(Session.url(hls.playlist_url)),
                        timeline.attachPositionMs,
                    )
                    attachRecipe(recipe)
                    executionSequence?.let { sequence ->
                        markIntentExecuted(sequence, recipe)
                    }
                    player.prepare()
                    playbackTelemetry.prepared(attempt)
                    applyEffectivePlayWhenReady()
                    armTrackSelections()
                }
            } finally {
                stallGuard.finishRequest(requestVersion)
            }
        }
    }

    /**
     * Act on a server verdict, or say it did not decide this stall.
     *
     * Returns true when the verdict is the decision, so the caller returns
     * without spending its own budget. False means today's path, unchanged —
     * which is the branch every node in the fleet takes.
     */
    private fun applyStallVerdict(
        verdict: ControlAction,
        event: OpenPlaybackStallTracker.Event,
    ): Boolean = when (verdict.type) {
        "terminal" -> {
            reportAutoRecoveryCause(tv.plurx.app.data.ReopenReason.Authority)
            // Ruling D1 keeps the verdict from tearing anything down: returning
            // true here is what stops a reopen from starting and a budget from
            // being spent, and that is unchanged.
            //
            // But the owner has nothing left to try either, and nothing else
            // will speak for it: this branch returns before
            // `openStallTracker.reset()`, so the tracker stays latched with
            // `fired = true` on a playhead that will never move; `playWhenReady`
            // is still true so the tracker's own escape never fires;
            // `sampleTargetPresentationDeadline` needs a `pendingSeek` a
            // mid-film stall does not have; and `onPlayerError` never comes,
            // because ExoPlayer is buffering rather than failing. There is no
            // later `stopped` to consume the wording. So the owner stops the
            // player and says so, in the server's words — see the amendment to
            // §3.4's Android row in
            // docs/clients/PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md.
            surfaceOwner.stoppedAfterControlVerdict(
                attached = mediaMutationEpoch,
                positionMs = event.positionMs,
                detail = verdict.message ?: "Playback stopped.",
            )
            true
        }
        "hold", "retry_resource" -> {
            reportAutoRecoveryCause(tv.plurx.app.data.ReopenReason.Hold)
            // Production is deliberately not advancing, or stopped for
            // something that may not recur. Either way a reopen would churn
            // against a server that already knows better, and it must not
            // spend the budget either.
            //
            // Control may defer the first recovery, never own the frozen
            // picture. The absolute twenty-second deadline fires again with
            // controlMayDefer=false and falls through to the client recovery.
            if (!event.controlMayDefer || !openStallTracker.defer(monotonicNowMs())) {
                false
            } else {
                surfaceOwner.controlHold(
                    mediaMutationEpoch,
                    if (verdict.type == "hold") {
                        "Waiting briefly for the server. Your place is saved."
                    } else {
                        "The server asked playback to retry shortly. Your place is saved."
                    },
                )
                true
            }
        }
        else -> false
    }

    /**
     * Called when a detected stall measurement is available. Reopens the same
     * recipe without the legacy stall ticket: a stationary presentation does
     * not prove decoder failure or insufficient capacity, so it cannot ask the
     * server to lower Auto quality. The existing same-rung budget still bounds
     * repeated repairs that do not improve presentation.
     */
    private suspend fun onStall(event: OpenPlaybackStallTracker.Event) {
        if (stallGuard.defersPredecessorRecovery(recipeOwnership.needsMediaReplacement(currentRecipe()))) return
        val positionMs = event.positionMs
        // The ask goes before the budget is consulted, and before anything
        // else this function does. The evidence is published from INSIDE it,
        // after it has read the sequence floor — publishing first lets the
        // pump start the next request before that read lands, which makes the
        // floor one too high and rejects the very exchange that carried this
        // stall's evidence.
        val session = sessionId
        // Capture media AND transport ownership before the ask. Pause must
        // reject this evidence even after Resume, but must not revoke an
        // already admitted replacement carrying the viewer's recipe.
        val observation = stallGuard.observeStall()
        // Measured before the wait, so the stall-recovery beacon includes the
        // time this ask itself costs. M5.5 exists to measure exactly that, and
        // an instrument that excludes it cannot.
        val observedAtMs = monotonicNowMs()
        val nativeReevaluation = event.controlMayDefer &&
            loadedWaitNeedsNativeReevaluation(
                bufferedPositionMs = player.bufferedPosition,
                currentPositionMs = player.currentPosition,
            ) && openStallTracker.claimNativeReevaluation()
        if (nativeReevaluation) {
            // Presentation recovery belongs to this client, not to the
            // producer verdict. Spend the one harmless Media3 reevaluation
            // before the bounded ask so even a suppressed hold (`none`) gets
            // the same chance under this episode's existing deadline.
            player.play()
            playbackTelemetry.report(
                event = "stall_recovery",
                level = "info",
                message = "Loaded media remained waiting; Media3 reevaluated playback.",
                detail = "action=native_reevaluation duration_ms=${event.durationMs}",
            )
        }
        val verdict = if (event.controlMayDefer) {
            playbackControl.askForAction(
                boundMs = CONTROL_ASK_MS,
                capMs = CONTROL_ASK_CAP_MS,
                publish = {
                    reportControlEvidence(
                        presentationStallEvidence(),
                        render = RenderState.STALLED,
                    )
                },
            )
        } else {
            // The hard deadline is recovery time, not another control window.
            // A terminal verdict already accepted for this unchanged intent is
            // still authoritative; hold/retry cannot move the deadline again.
            playbackControl.terminalVerdict?.takeIf { it.type == "terminal" }
        }
        // Seconds passed, and one session-id comparison is not enough to
        // notice. The predicate that let control in here was
        // `playWhenReady && establishedPlayback`; a viewer who paused, or a
        // transport failover that started on the same session, or any seek
        // that invalidated the guard, all leave the id alone.
        if (sessionId != session) return
        if (!stallGuard.isCurrent(observation)) return
        if (!playbackControlBootstrapFence.isActive() || !presentationForeground) return
        if (!player.playWhenReady || player.playbackState == Player.STATE_ENDED) return
        if (kotlin.math.abs(realPosition() - positionMs) >= 250L) return
        // The incumbent stalled: whatever recovery follows (a hold, a
        // downgrade, a reopen), the original-first re-plan a viewer boundary
        // armed is evidence-free now and is not carried into it.
        autoBoundaryReplan.clear()
        if (verdict != null && applyStallVerdict(verdict, event)) return
        selectAutoStallRecoveryCandidate(observation,
            observedAtMs, observedAtMs + openStallTracker.remainingRecoveryMs(event, observedAtMs, observedAtMs))
        if (sessionId != session || !stallGuard.isCurrent(observation)) return
        // A reopen is a new recovery episode. If the replacement freezes at
        // the same playhead, it must receive its own bounded deadline rather
        // than inheriting the fired latch from the item it replaced.
        openStallTracker.reset()
        if (session == null) {
            if (sessionlessStallRecoveryUsed) {
                // The owner's obligation: stop the player, THEN say so.
                // `playWhenReady = false` also resets the open-stall tracker
                // (PlaybackTelemetry.kt), which is what closes the
                // restart-under-overlay path. `stallWatchdogJob` stays: it is
                // this owner's detector, and Keep waiting needs it.
                surfaceOwner.exhaustedAfterSessionlessStall(mediaMutationEpoch, positionMs)
                return
            }
            sessionlessStallRecoveryUsed = true
            sessionlessStallRecoveryPositionMs = positionMs
            restartAt(positionMs, "stall", observedAtMs)
            return
        }
        // If we have already exhausted the budget at the current floor rung,
        // stop reopening — but do not leave the viewer on a frozen picture.
        // The server cannot step further down, so this is a visible terminal
        // failure with a retry affordance rather than silent infinite wait.
        if (!stallReopenBudget.canReopen()) {
            surfaceOwner.exhaustedAfterReopenBudget(mediaMutationEpoch, positionMs)
            return
        }
        val reason = "stall"
        raiseRecoveryStep(null, "Reconnecting to the stream.")
        val attempt = beginPlaybackAttempt(reason, observedAtMs)
        val recipe = currentRecipe()
        val requestVersion = stallGuard.beginRequest()
        // Capture the predecessor height for the same-rung budget
        // before nulling the session ID.  The first stall reopen
        // compares against this; subsequent stalls compare against
        // each previous stall response.
        // Keep the predecessor alive through the create request — the
        // server validates previous_session_id against a live session
        // map.  Session creation supersedes and kills the predecessor
        // atomically.
        //
        // The successor was staged against the session this reopen replaces,
        // and this is the one stream-replacing path that does not go through
        // `openSession`. Without this the preparation stays live against a
        // session that no longer exists, and the next tick commits it — putting
        // the viewer back on the stream that just stalled and orphaning the
        // replacement they are actually watching. It runs before
        // [endPlaybackControl] so the abandonment acknowledgement still has a
        // control session to leave on.
        abandonPreparedReplacement(failed = false)
        val typedRecovery = currentAutoRecoveryCause()
        if (typedRecovery == null) {
            endPlaybackControl()
            sessionId = null
            rebindMediaSessionTransport()
        }
        clearStatusPolling()
        encoder = null
        sessionIsVod = false
        scope.launch {
            try {
                val body = bindDecisionPlan(
                    body = subtitleSessionBody(
                        playbackId = playbackIntent.playbackId,
                        requestId = UUID.randomUUID().toString(),
                        controlSequence = playbackIntent.orderedControlSequence(
                            playbackControl.controlSequence(),
                        ),
                        startSeconds = positionMs / 1000.0,
                        delivery = recipe.recipe.subtitleDelivery,
                        subtitleIndex = recipe.recipe.subtitleIndex,
                        copyableVideo = recipe.recipe.desiredTransport ==
                            PlaybackMediaTransport.HlsCopy,
                        aac = plan.aac,
                        preserveDolbyVision = plan.preserveDolbyVision,
                        audioIndex = recipe.recipe.audioIndex,
                        audioOffsetMs = recipe.recipe.audioOffsetMs,
                        quality = recipe.recipe.quality,
                        sourceHeight = plan.sourceHeight,
                    ),
                    caps = decisionCaps,
                    requestHDR10 = sessionHDR10Request(
                        decisionMode = plan.mode,
                        deliveredDynamicRange = plan.deliveredDynamicRange,
                        compatibilityTranscode = recipe.recipe.compatibilityTranscode,
                        delivery = recipe.recipe.subtitleDelivery,
                    ),
                ).let(::applyAutoIntent)
                val hls = try {
                    sessionCreateCoordinator.reopenAfterStall(
                        body = body,
                        isCurrent = { stallGuard.isCurrent(requestVersion) },
                    ) ?: return@launch
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Exception) {
                    if (stallGuard.isCurrent(requestVersion)) {
                        playbackTelemetry.report(
                            event = "playback_error",
                            level = "error",
                            message = "session reopen failed before Media3 started",
                            code = refusalStatusOf(error),
                            detail = redactedFailureDetail("session_reopen", error),
                            attempt = attempt,
                        )
                        playbackTelemetry.cancel(attempt)
                        Log.w(
                            "PlurxPlayback",
                            "session reopen failed ${redactedFailureDetail("session_reopen", error)}",
                        )
                        raiseSessionCreateFailure(
                            error,
                            playbackControl.terminalVerdict?.message
                                ?: "The stream stalled and recovery failed.",
                            // S5: a stalled player still holds its item and sits
                            // in STATE_BUFFERING, so "an item is attached" is not
                            // the question here — "is there a picture behind this
                            // failure" is. Without the presentation test a frozen
                            // stream got a passive banner with `intent = null`,
                            // which `intent_superseded` can never match and which
                            // ten seconds of continuous presentation will never
                            // reach. See the amendment to §3.4's row for the
                            // stall-reopen site.
                            predecessor = predecessorAttached() && surfaceOwner.isPresenting,
                        )
                    }
                    return@launch
                }
                if (hls.session_id !in continuousPending) retireContinuousForReplacement()
                sessionCreateCoordinator.attachIfCurrent(hls, { stallGuard.isCurrent(requestVersion) }) { hls ->
                    // Update the same-rung budget: the budget counts consecutive
                    // reopen responses that do NOT resolve a strictly lower rung than
                    // the predecessor (same rung, absent/zero height, or a higher
                    // rung).  A genuine strict downgrade resets the count.
                    // Absent or zero height counts as no step down — it is the server
                    // saying "this session is already at its answer" without a rung
                    // the client can compare.
                    stallReopenBudget.record(hls.height)
                    surfaceOwner.ownerSuccess(mediaMutationEpoch)
                    sessionId = hls.session_id
                    rebindMediaSessionTransport()
                    beginPlaybackControl(hls)
                    startStatusPolling(hls.session_id)
                    encoder = hls.encoder
                    sessionIsVod = hls.vod
                    adoptSessionDelivery(hls)
                    val timeline = sessionPlaybackTimeline(hls, requestedStartMs = positionMs)
                    baseMs = timeline.baseMs
                    activeMediaPath = relativeMediaPath(hls.playlist_url)
                    val owned = continuousPending.remove(hls.session_id)
                    if (owned != null) {
                        continuousAttachment = owned.owner
                        player.setMediaSource(owned.source, timeline.attachPositionMs)
                    } else player.setMediaItem(
                        MediaItem.fromUri(Session.url(hls.playlist_url)),
                        timeline.attachPositionMs,
                    )
                    attachRecipe(recipe)
                    player.prepare()
                    playbackTelemetry.prepared(attempt)
                    applyEffectivePlayWhenReady()
                    armTrackSelections()
                }
            } finally {
                stallGuard.finishRequest(requestVersion)
            }
        }
    }

    internal fun sessionBody(
        ms: Long,
        controlSequence: Long? = null,
        recipe: PlaybackMediaRecipe = currentRecipe().recipe,
    ): CreateSessionReq = bindDecisionPlan(
        body = playbackSessionBody(
            playbackId = playbackIntent.playbackId,
            requestId = UUID.randomUUID().toString(),
            controlSequence = controlSequence,
            startSeconds = ms / 1000.0,
            recipe = recipe,
            aac = plan.aac,
            preserveDolbyVision = plan.preserveDolbyVision,
            sourceHeight = plan.sourceHeight,
        ),
        caps = decisionCaps,
        requestHDR10 = sessionHDR10Request(
            decisionMode = plan.mode,
            deliveredDynamicRange = plan.deliveredDynamicRange,
            compatibilityTranscode = recipe.compatibilityTranscode,
            delivery = recipe.subtitleDelivery,
        ),
    ).let(::applyAutoIntent)

    private fun applyAutoIntent(body: CreateSessionReq): CreateSessionReq =
        if (tv.plurx.app.data.Session.displayAwareAuto &&
            tv.plurx.app.data.Session.displayAwareAutoProtocol == "route-v1" &&
            plan.displayAwareAutoProtocol == "route-v1") body.copy(
                caps = decisionCaps.copy(display = decisionCaps.display.copy(presentation_target = autoPresentationTarget)),
                intent = playbackIntent.mediaIntent(playbackControlSelection()),
                height = if (playbackIntent.desiredQuality == PlaybackQuality.Auto)
                    playbackIntent.automaticCandidateId?.let {
                        playbackIntent.automaticCandidateHeight?.takeIf { height ->
                            height in PlaybackControl.MIN_HEIGHT..PlaybackControl.MAX_HEIGHT
                        }
                    } ?: if (playbackIntent.automaticCandidateId == null) body.height else null
                else body.height,
            ).let(::applyAutoRecoveryCause) else body

    private fun currentAutoRecoveryCause(): AutoRecoveryCauseTicket? = autoRecoveryCause?.takeIf {
        playbackIntent.desiredQuality == PlaybackQuality.Auto &&
            it.isCurrent(sessionId, player, autoActiveCandidateId, autoDesiredCandidate?.id, monotonicNowMs())
    }

    private fun applyAutoRecoveryCause(body: CreateSessionReq): CreateSessionReq {
        val ticket = currentAutoRecoveryCause() ?: return body
        return body.copy(previous_session_id = ticket.session, reopen_reason = ticket.cause)
    }

    private fun recoveryLinkReceipt(body: CreateSessionReq): String? {
        if (body.reopen_reason != tv.plurx.app.data.ReopenReason.Link) return currentLinkReceipt()
        val ticket = currentAutoRecoveryCause() ?: return null
        return ticket.receiptForRequest(body.previous_session_id,
            (body.intent?.selection?.quality as? QualitySelection.AutoCandidate)?.candidateId, body.reopen_reason)
    }

    private fun trackFor(index: Long?): SubTrack? =
        index?.let { i -> plan.subtitles.firstOrNull { it.index == i } }

    /**
     * Record that the current selections still have to reach the player.
     *
     * An in-place switch can use the published tracks immediately. A new
     * media item keeps each selector armed until its tracks arrive and
     * Media3 confirms the requested option, rather than acknowledging an
     * override merely because it was submitted.
     */
    private fun armTrackSelections(recipe: PlaybackRecipeOwnership.Claim? = recipeOwnership.attached) {
        if (recipe == null || recipeOwnership.needsMediaReplacement(recipe)) return
        selectionRecipe = recipe
        pgsOverlay.select(recipe.recipe.subtitleIndex.takeIf { recipe.recipe.subtitleDelivery == SubtitleDelivery.BitmapOverlay })
        textSelectionArmed = true
        audioSelectionArmed = true
        applyTextSelection()
        applyAudioSelection()
        completeRecipeExecution()
    }

    private fun applyTextSelection() {
        if (!textSelectionArmed) return
        val recipe = selectionRecipe?.recipe ?: return
        val index = recipe.subtitleIndex
        // Off, and a burn, are the same instruction to the renderer: show no
        // text track. A burn's cues are already in the picture, and letting
        // ExoPlayer's own language preference pick something here would put a
        // second subtitle policy in front of the one the server decided.
        if (
            index == null ||
            recipe.subtitleDelivery == SubtitleDelivery.Burn ||
            recipe.subtitleDelivery == SubtitleDelivery.BitmapOverlay
        ) {
            textSelectionArmed = player.currentTracks.isTypeSelected(C.TRACK_TYPE_TEXT)
            player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                .clearOverridesOfType(C.TRACK_TYPE_TEXT)
                .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                .build()
            return
        }
        val ordinal = if (recipe.subtitleDelivery == SubtitleDelivery.NativeSession) {
            nativeSubtitleOrdinal(index, plan.subtitles)
        } else {
            embeddedTextTrackIndex(index, plan.subtitles, embeddedTextLanguages())
        }
        // Nothing to select yet — the media is still being prepared, or this
        // source genuinely lacks the track. Stay armed; onTracksChanged retries.
        val target = ordinal?.let(::textTrackAt) ?: return
        textSelectionArmed = !player.currentTracks.groups.any {
            it.mediaTrackGroup == target.first && it.isTrackSelected(target.second)
        }
        player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
            .setOverrideForType(TrackSelectionOverride(target.first, target.second))
            .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false)
            .build()
    }

    /**
     * Pin the audio track the server said would play.
     *
     * Only direct play needs this, and only direct play may have it: the raw
     * file carries every stream, so ExoPlayer picks one — and it picks with
     * `setPreferredAudioLanguage`, a client-side language preference that can
     * disagree with the server's `select_tracks` (the dual-audio anime rule
     * selects Japanese original audio against an English preference). Leaving
     * that unpinned would make the detail screen's default marker, and any
     * pre-play choice the server answered with a `direct` verdict, describe a
     * track other than the one on the speakers.
     *
     * Every other transport already delivers exactly one audio stream — the
     * one the plan's `?audio=` or the session body's `audio` selected — so an
     * override there would index into a list of one.
     */
    private fun applyAudioSelection() {
        if (!audioSelectionArmed) return
        val index = selectionRecipe?.recipe?.audioIndex
        if (index == null || !directTransport) {
            audioSelectionArmed = false
            return
        }
        val ordinal = embeddedAudioTrackIndex(index, plan.audio, embeddedLanguages(C.TRACK_TYPE_AUDIO))
        // Nothing to select yet — the media is still being prepared, or the
        // player never published this track. Stay armed and let the next
        // onTracksChanged retry; ExoPlayer's own pick is the honest fallback
        // for a track that is genuinely not there.
        val target = ordinal?.let { trackAt(C.TRACK_TYPE_AUDIO, it) } ?: return
        audioSelectionArmed = !player.currentTracks.groups.any {
            it.mediaTrackGroup == target.first && it.isTrackSelected(target.second)
        }
        player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
            .setOverrideForType(TrackSelectionOverride(target.first, target.second))
            .setTrackTypeDisabled(C.TRACK_TYPE_AUDIO, false)
            .build()
    }

    /** The player's tracks of one type, flattened in publication order. */
    private fun embeddedLanguages(type: Int): List<String?> =
        player.currentTracks.groups
            .filter { it.type == type }
            .flatMap { group -> (0 until group.length).map { group.getTrackFormat(it).language } }

    private fun embeddedTextLanguages(): List<String?> = embeddedLanguages(C.TRACK_TYPE_TEXT)

    private fun trackAt(type: Int, ordinal: Int): Pair<TrackGroup, Int>? {
        var seen = 0
        for (group in player.currentTracks.groups) {
            if (group.type != type) continue
            for (i in 0 until group.length) {
                if (seen == ordinal) return group.mediaTrackGroup to i
                seen++
            }
        }
        return null
    }

    private fun textTrackAt(ordinal: Int): Pair<TrackGroup, Int>? =
        trackAt(C.TRACK_TYPE_TEXT, ordinal)

    private fun leaveSessionPlayback() {
        stallGuard.invalidateForUserAction()
        val endingSession = sessionId
        // The final control exchange may release the session after the
        // attachment slot is already empty; the retired attachment still owns
        // that session's End, so it is never ended a second time by id.
        val retired = retireContinuousForTransportChange()
        endPlaybackControl {
            endingSession?.let { id ->
                if (retired != null && retired.start.playback.session_id == id) retired.end() else releaseOwnedSession(id)
            }
        }
        sessionId = null
        rebindMediaSessionTransport()
        clearStatusPolling()
        encoder = null
        sessionIsVod = false
        // Back on the plan's own delivery, so back to the plan's own grade —
        // otherwise a chip would keep reporting the session that just ended.
        // Both halves, for the same reason they are adopted together.
        deliveredRange = plan.deliveredDynamicRange
        deliveredDolbyVisionProfile = plan.deliveredDolbyVisionProfile
    }

    /**
     * Poll only while this controller owns an HLS session. The endpoint does
     * not count as playback activity, so showing Standard or Debug cannot keep
     * an abandoned encoder alive; keeping the last successful sample mirrors
     * the browser and avoids a useful panel vanishing during teardown. The
     * playback-info panel, quality label, wait overlay and prepared replacement
     * are the current readers. Any future recovery reader must keep [statusPollingVisible]
     * true while it depends on this sample.
     */
    private fun startStatusPolling(polledSessionId: String) {
        statusPollingJob?.cancel()
        sessionStatus = null
        sessionStatusObservedAtMs = null
        statusPollingJob = scope.launch {
            // Report the first failed sample once. A closed panel uses a
            // bounded ten-second interval; visible readers keep two seconds.
            var reportedFailure = false
            while (isActive && sessionId == polledSessionId) {
                try {
                    val observed = vm.hlsSessionStatus(polledSessionId)
                    if (!isActive || sessionId != polledSessionId) return@launch
                    if (autoPreparing && autoVoluntary) {
                        val staged = preparedLedger.action
                        val stagedId = staged?.sessionId
                        if (stagedId != null) {
                            val stagedStatus = try { vm.hlsSessionStatus(stagedId) } catch (_: Exception) { null }
                            if (!isActive || sessionId != polledSessionId) return@launch
                            if (preparedLedger.action?.sessionId == stagedId) {
                                autoStagedStatus = stagedStatus
                                autoStagedStatusObservedMs = if (stagedStatus != null) monotonicNowMs() else null
                            }
                        }
                    }
                    sessionStatus = observed
                    sessionStatusObservedAtMs = monotonicNowMs()
                } catch (_: Exception) {
                    // Keep the last real sample. A completed session may
                    // disappear before the player finishes its buffered tail.
                    // The picture is untouched either way, so the surface is
                    // too — but the stats stopping is worth one line.
                    if (!reportedFailure) {
                        reportedFailure = true
                        surfaceOwner.logOnly(
                            mediaMutationEpoch,
                            "session status polling stopped answering",
                        )
                    }
                }
                delay(statusPollIntervalMs(
                    statusPollingVisible() || preparedPlayer != null || preparedPredecessor != null,
                ))
            }
        }
    }

    private fun clearStatusPolling() {
        statusPollingJob?.cancel()
        statusPollingJob = null
        sessionStatus = null
        sessionStatusObservedAtMs = null
    }

    private fun remuxUri(ms: Long): String = progressiveRemuxUri(
        plannedUrl = if (plan.mode == "direct") {
            Session.url("/api/v1/files/${plan.fileId}/stream.mp4")
        } else {
            plan.playUrl
        },
        startSeconds = ms / 1000.0,
        audioIndex = selectedAudio,
        audioOffsetMs = audioOffsetMs,
        caps = caps,
        encode = Uri::encode,
    )

    /** Retry the exact delivery URL through another advertised ingress. The
     * media recipe, session capability, and compatibility flags do not move. */
    private fun retryMediaOnNextNode(error: PlaybackException): Boolean {
        if (!playbackControlBootstrapFence.isActive()) return false
        if (continuousAttachment != null && player === continuousPlayer) return false
        val path = activeMediaPath ?: return false
        val next = Session.nextMediaFailoverUrl(path) ?: return false
        val recipe = recipeOwnership.attached ?: currentRecipe()
        val transport = recipeOwnership.attachedTransport ?: recipe.recipe.desiredTransport
        val presentationSequence = playbackIntent.executedSequence()
        // Same session, different ingress — but the incumbent is about to be
        // re-attached and the successor was primed against the playhead the
        // failure interrupted, so it is abandoned rather than left to commit
        // over the recovery.
        abandonPreparedReplacement(failed = false)
        val attachPosition = if (progressiveTransport) 0L else player.currentPosition.coerceAtLeast(0)
        playbackTelemetry.report(
            event = "playback_transport_failover",
            level = "warn",
            message = error.errorCodeName,
            code = error.errorCode,
            detail = "delivery=$deliveryMode compatibility_ladder=false " +
                "http_status=${httpResponseCode(error) ?: "none"}",
        )
        // A failover is a new media generation. Any stall owner awaiting a
        // verdict for the failed item is stale, and the successor needs the
        // startup deadline rather than the predecessor's established one.
        stallGuard.invalidateForPlaybackAttempt()
        beginPlaybackAttempt("node-failover")
        // A progressive remux answers its achieved origin in a response
        // header, and the tracker only accepts a response whose URI it is
        // expecting. Re-arming it here is what keeps every position after a
        // failover honest: without it the tracker keeps the dead node's URI,
        // discards the successor's origin, and every reported position stays
        // off by the successor's keyframe snap for the rest of the stream.
        if (progressiveTransport) {
            progressiveMediaOrigin.begin(next, realPosition())
        }
        player.setMediaItem(MediaItem.fromUri(next), attachPosition)
        rebindMediaSessionTransport()
        attachRecipe(recipe, transport)
        presentationSequence?.let { sequence ->
            markIntentExecuted(sequence, recipe)
        }
        player.prepare()
        applyEffectivePlayWhenReady()
        armTrackSelections()
        return true
    }

    /**
     * The server-relative form of a delivery URL, or null when it does not
     * belong to this server.
     *
     * The origin check is the security half: whatever comes back here is
     * concatenated onto another node's origin and requested with the account
     * bearer attached, so a URL pointing anywhere else must not be reduced to
     * a path and replayed against the cluster.
     */
    private fun relativeMediaPath(value: String): String? {
        val uri = Uri.parse(value)
        if (uri.scheme.isNullOrEmpty()) {
            return value.takeIf { it.startsWith('/') && !it.startsWith("//") }
        }
        val primary = Session.canonicalPrimaryOrigin() ?: return null
        val authority = uri.authority?.let { "${uri.scheme}://$it" } ?: return null
        if (Session.canonicalOrigin(authority) != primary) return null
        val path = uri.encodedPath?.takeIf { it.startsWith('/') } ?: return null
        return uri.encodedQuery?.let { "$path?$it" } ?: path
    }

    // ---------------------------------------------------- passive control

    /**
     * Start reporting for a session the server said is controllable.
     *
     * A server that sends no bootstrap, or one this client cannot address,
     * leaves the reporter silent. That is the passive M2 behaviour: playback
     * does not depend on the control plane and never should.
     */
    private fun beginPlaybackControl(hls: HlsStart) {
        autoRouteProtocol = hls.display_aware_auto_protocol
        if (hls.display_aware_auto_protocol == "route-v1") {
            autoCatalog = hls.quality_candidates
            autoMeasuredOutputs = hls.measured_candidate_outputs
            autoActiveCandidateId = hls.quality_candidate_id
        }
        endPlaybackControl()
        // The override describes the session that just ended. Carrying it into
        // the replacement would make its very first exchange — a session that
        // has rendered nothing yet — report a stall belonging to another.
        // Cleared before the bootstrap is judged, so a reopen against a server
        // that offers no control plane cannot leave one behind either.
        controlObservationOverride = null
        controlRenderOverride = null
        controlEvidencePositionMs = null
        val claim = playbackControlBootstrapFence.claim(hls.session_id)
        val bootstrap = hls.control
        if (bootstrap == null || !bootstrap.isValid) {
            return
        }
        // Capabilities are the one input that has to be probed rather than
        // read, and the protocol requires them on the first request of a
        // generation, so reporting waits for that one probe. Until it lands
        // the reporter has nothing complete to say.
        scope.launch {
            controlCapabilityProbe.join()
            if (!playbackControlBootstrapFence.isCurrent(claim, sessionId)) return@launch
            playbackControl.begin(
                bootstrap = bootstrap,
                observe = ::playbackControlObservation,
                linkReceipt = { if (playbackControlBootstrapFence.isCurrent(claim, sessionId)) currentLinkReceipt() else null },
                onSubtitleReady = ::retryNativeSubtitleAfterReadiness,
            onSubtitleUnavailable = {
                raiseDegradedNotice(SUBTITLE_UNAVAILABLE_NOTICE)
            },
                onPrepare = ::onPrepareAction,
                onAcknowledged = ::acknowledgementDelivered,
                onEffectiveSelection = { effective ->
                    if (tv.plurx.app.data.Session.displayAwareAutoProtocol == "route-v1")
                        autoActiveCandidateId = effective.candidateId
                },
                onGaveUp = ::controlReportingGaveUp,
            )
        }
    }

    /**
     * The acknowledgement a just-abandoned preparation owes, as a snapshot.
     *
     * Null unless one is actually owed, so every caller's ordinary path is the
     * plain teardown it always was.
     */
    private fun settlingSnapshotIfOwed(): PlaybackControlSnapshot? =
        pendingAcknowledgement?.let {
            playbackControlObservation()
                ?.let(PlaybackControlMapping::snapshot)
        }

    /**
     * End this session's reporting — carrying an owed acknowledgement out if
     * there is one.
     *
     * Every caller of this abandons a preparation immediately before it, and an
     * abandonment publishes its acknowledgement by *queueing* an urgent report
     * on the composition's dispatcher. `end()` runs synchronously and empties
     * the slot that report would read from, so the queued report found nothing
     * and the acknowledgement was dropped on the session that owned the
     * staging — leaving a real encoder and that session's one preparation slot
     * held for the server's 330 s deadline. This is [release]'s hand-off,
     * applied to the reopen paths for the same reason.
     *
     * The acknowledgement may still ride the next session's first exchange as
     * well: the delivery callback that would clear it is fenced by a
     * generation this teardown invalidates. A duplicate is inert — the server
     * ignores an `action_id` not bound to the session it arrives on — and it is
     * strictly what happened before this hand-off existed.
     */
    private fun endPlaybackControl(
        settling: PlaybackControlSnapshot? = settlingSnapshotIfOwed(),
        afterFinalExchange: () -> Unit = {},
    ) {
        playbackControlBootstrapFence.invalidate()
        if (settling != null) {
            playbackControl.endAfterFinalExchange(
                vm.viewModelScope,
                settling,
                afterFinalExchange,
            )
        } else {
            playbackControl.end()
            afterFinalExchange()
        }
    }

    /**
     * Media3 may keep the first empty `no-store` subtitle segment. Disable and
     * re-apply only the text override when control reports the demanded window
     * ready; the media item and video producer stay attached throughout.
     *
     * The disable and the re-arm are one operation, and the old shape could
     * perform only the first half. It disabled the text renderer, then
     * re-armed inside a coroutine behind four predicates — and any of them
     * failing left text disabled with nothing scheduled to turn it back on.
     * The viewer's answer to "the subtitle you selected is ready now" was
     * their subtitles switching off for good.
     *
     * So the disable moved inside the guard: either both halves run, or
     * neither does. A predicate that no longer holds means a newer intent
     * owns the selection, and that intent's own [applyTextSelection] is the
     * right thing to leave in charge.
     */
    private fun retryNativeSubtitleAfterReadiness() {
        val index = selectedSubtitle ?: return
        if (subtitleDelivery != SubtitleDelivery.NativeSession) return
        val session = sessionId ?: return
        val claim = playbackControlBootstrapFence.snapshot(session)
        val intentGeneration = playbackIntent.generation()
        if (!playbackControlBootstrapFence.isCurrent(claim, sessionId)) return
        scope.launch {
            if (
                !playbackControlBootstrapFence.isCurrent(claim, sessionId) ||
                playbackIntent.generation() != intentGeneration ||
                selectedSubtitle != index ||
                subtitleDelivery != SubtitleDelivery.NativeSession
            ) {
                return@launch
            }
            textSelectionArmed = false
            player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                .clearOverridesOfType(C.TRACK_TYPE_TEXT)
                .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                .build()
            kotlinx.coroutines.yield()
            textSelectionArmed = true
            applyTextSelection()
        }
    }

    /**
     * Everything the mapping needs, read from the player once.
     *
     * This is the only place Media3 meets the control protocol, and it is
     * deliberately all reads: nothing here decides anything, so every rule
     * that could be wrong lives in [PlaybackControlMapping], where it is
     * tested.
     */
    /**
     * Evidence a recovery owner is about to act on, published for the next
     * exchange. The mapper has consumed an override since M2 — a recovery
     * path knows things Media3 cannot report — and until now nothing set one.
     */
    private var controlObservationOverride: ClientObservation? = null
    private var controlRenderOverride: RenderState? = null

    /**
     * The film position when the override was published, so progress past it
     * can expire the override. A moving film clock is the only proof the stall
     * the evidence describes is actually over; `playbackState` is not, because
     * Android's own stall detector cannot fire during an open-ended freeze.
     */
    private var controlEvidencePositionMs: Long? = null

    /**
     * Publish what a recovery owner is about to act on, and send it now.
     *
     * Two things are load-bearing: the override is set before the snapshot is
     * taken, or the exchange carries Media3's vaguer version of the same
     * moment; and the report is urgent rather than coalesced, because the
     * owner's own reopen normally ends the reporter before its next scheduled
     * exchange.
     */
    private fun reportControlEvidence(
        observation: ClientObservation?,
        render: RenderState? = null,
    ) {
        refreshControlWaiting()
        controlObservationOverride = observation
        controlRenderOverride = render
        controlEvidencePositionMs = realPosition()
        playbackControl.reportEvidence()
    }

    /**
     * Set the moment the players are torn down. A final exchange may still be
     * in flight on a scope that outlives this controller, and every field below
     * is read off an ExoPlayer that no longer exists.
     */
    private var controlObservationIsClosed = false

    private fun playbackControlObservation(): PlayerControlObservation? {
        if (controlObservationIsClosed) return null
        val baseCapabilities = deviceControlCapabilities ?: return null
        val capabilities = if (tv.plurx.app.data.Session.displayAwareAuto &&
            tv.plurx.app.data.Session.displayAwareAutoProtocol == "route-v1" &&
            autoRouteProtocol == "route-v1") baseCapabilities.copy(
                presentationTarget = autoPresentationTarget,
                decoderCaps = DecoderCapabilitySnapshot.fromVideo(1L, decisionCaps.video),
            ) else baseCapabilities
        val position = realPosition()
        val duration = player.duration
        // Only the runway *ahead* is protocol runway. Media3's bufferedPosition
        // is in player time while the protocol wants title time, so the range
        // is reported from the playhead forward rather than converted with an
        // offset a copy session's keyframe start would make wrong.
        val runway = (player.bufferedPosition - player.currentPosition).coerceAtLeast(0L)
        return PlayerControlObservation(
            positionMs = position,
            durationMs = if (duration > 0) duration else 0L,
            bufferedFromMs = position,
            bufferedThroughMs = position + runway,
            rate = player.playbackParameters.speed.toDouble(),
            isPaused = !player.playWhenReady,
            isEnded = player.playbackState == Player.STATE_ENDED,
            // A seek Media3 has accepted but not yet rendered is exactly the
            // discontinuity the protocol calls seeking.
            isSeeking = playbackIntent.pendingSeek != null,
            seekTargetMs = playbackIntent.pendingSeek?.targetMs,
            hasStarted = establishedPlayback,
            waitingForMs = controlWaitingSince?.let {
                (monotonicNowMs() - it).coerceAtLeast(0L)
            },
            isLikelyToKeepUp = player.playbackState == Player.STATE_READY,
            droppedFrames = null,
            // The server's second-pipeline floor wants twice what this session
            // is already delivering, and until now this client sent nothing,
            // so the floor refused on a missing input rather than on a tight
            // link. This rate is counted off the wire by `MediaOrigin`'s
            // transfer listener over a rolling 500 ms window — bytes actually
            // received, not a declared variant bitrate — and is already what
            // the player's own stats panel shows the viewer. Null until the
            // first window closes, which the server reads as a refusal: the
            // honest answer, and the one thing the floor exists to enforce.
            observedDownloadBps = observedBitsPerSecond,
            observationOverride = controlObservationOverride,
            renderOverride = controlRenderOverride,
            acknowledgement = pendingAcknowledgement,
            selection = playbackControlSelection(),
            capabilities = capabilities,
        )
    }

    /**
     * What the viewer chose, in the protocol's vocabulary.
     *
     * Codec and dynamic range report `auto` on purpose: after `/decision` this
     * client forces neither, and saying otherwise would tell the server it had
     * made a choice it has not made.
     */
    private fun playbackControlSelection(): ClientSelection {
        val mode = when {
            selectedSubtitle == null -> SubtitleMode.OFF
            subtitleDelivery == SubtitleDelivery.Burn -> SubtitleMode.BURN
            subtitleDelivery == SubtitleDelivery.BitmapOverlay -> SubtitleMode.OVERLAY
            else -> SubtitleMode.NATIVE
        }
        return ClientSelection(
            quality = playbackIntent.retainedControlQuality ?: when (val requested = playbackIntent.desiredQuality) {
                PlaybackQuality.Auto -> if (tv.plurx.app.data.Session.displayAwareAuto &&
                    autoRouteProtocol == "route-v1" &&
                    tv.plurx.app.data.Session.displayAwareAutoProtocol == "route-v1") {
                    playbackIntent.automaticCandidateId?.let { id ->
                        QualitySelection.AutoCandidate(playbackIntent.automaticCandidateHeight?.takeIf {
                            it in PlaybackControl.MIN_HEIGHT..PlaybackControl.MAX_HEIGHT
                        }, id)
                    } ?: QualitySelection.Auto
                } else QualitySelection.Auto
                PlaybackQuality.Original -> QualitySelection.Original
                else -> requested.rungHeight
                    ?.let(QualitySelection::Manual)
                    ?: QualitySelection.Auto
            },
            audioTrack = selectedAudio?.toInt(),
            subtitle = SubtitleSelection(
                mode = mode,
                track = if (mode == SubtitleMode.OFF) null else selectedSubtitle?.toInt(),
            ),
            audioOffsetMs = audioOffsetMs,
            codec = CodecPolicy.AUTO,
            dynamicRange = DynamicRangePolicy.AUTO,
        )
    }

    /**
     * Called from the tick that already runs every second. The reporter
     * coalesces, so a notification between exchanges costs nothing but
     * replaces what the next exchange will carry.
     */
    private var autoPresentationTarget = decisionCaps.display.presentation_target

    fun updatePresentationTarget(target: tv.plurx.app.data.PresentationTarget?) {
        if (autoPresentationTarget != target) autoUpgradeSinceMs = null
        autoPresentationTarget = target
    }

    private fun refreshAutoCatalogForViewerRecipe() {
        autoDesiredCandidate = null
        autoPreparing = false
        autoUpgradeSinceMs = null
        playbackIntent.requestAutomaticCandidate(null, null)
        if (!tv.plurx.app.data.Session.displayAwareAuto ||
            tv.plurx.app.data.Session.displayAwareAutoProtocol != "route-v1") return
        autoCatalog = emptyList()
        val selectionKey = Triple(selectedAudio, selectedSubtitle, audioOffsetMs)
        val target = autoPresentationTarget
        scope.launch {
            val fresh = try {
                vm.playbackDecision(plan.fileId, PreplayTracks(selectedAudio, SubtitleChoice(selectedSubtitle)),
                    PlaybackQuality.Auto, target, selectionKey.third, currentLinkReceipt()).decision
            } catch (_: Exception) { return@launch }
            if (controlObservationIsClosed || selectionKey != Triple(selectedAudio, selectedSubtitle, audioOffsetMs)) return@launch
            if (fresh.display_aware_auto_protocol == "route-v1") {
                autoCatalog = continuousAttachment?.takeIf { player === continuousPlayer }?.boundCatalog(fresh.quality_candidates)
                    ?: fresh.quality_candidates
                autoMeasuredOutputs = fresh.measured_candidate_outputs
            }
            // Only an attached-session receipt may change the active identity.
        }
    }

    private fun resetAutoQualityBudgetForViewer() {
        autoSwitchTimes.clear()
        autoLastSwitchMs = null
        autoUpgradeSinceMs = null
        autoMildSamples = 0
        if (autoPreparing) {
            autoDesiredCandidate = null
            playbackIntent.requestAutomaticCandidate(null, null)
            autoPreparing = false
        }
    }

    /**
     * A receipted body is attested candidate evidence. A family object of the
     * live continuous session is the family's own evidence instead: its cost
     * is the family's enforced budget, and its role-split objects are not a
     * muxed candidate's output, so they carry no candidate receipt (and are
     * never reported as candidate link samples).
     */
    private fun autoLinkAttested(transfer: AutoCompletedTransfer, currentId: String): Boolean =
        transfer.receipt != null || liveContinuous()?.hasMember(currentId) == true

    private suspend fun selectAutoStallRecoveryCandidate(observation: ControllerStallGuard.Observation, capturedAtMs: Long, deadlineMs: Long) {
        if (!tv.plurx.app.data.Session.displayAwareAuto || !tv.plurx.app.data.Session.autoAbr ||
            tv.plurx.app.data.Session.displayAwareAutoProtocol != "route-v1" ||
            autoRouteProtocol != "route-v1" || playbackIntent.desiredQuality != PlaybackQuality.Auto ||
            !player.playWhenReady || playbackIntent.pendingSeek != null) return
        val policyCatalog = measuredCostCatalog()
        val current = policyCatalog.firstOrNull { it.hasValidIdentity && it.id == autoActiveCandidateId } ?: return
        val now = monotonicNowMs()
        val transfer = latestAutoCompletedTransfer
        val duration = transfer?.bodyDurationMs
        val link = if (transfer != null && duration != null && transfer.networkLoad &&
            transfer.fromLocalCache == false && transfer.producerPaced == false &&
            transfer.statusCode == 200 && autoLinkAttested(transfer, current.id) && transfer.etag != null &&
            autoTransferOriginCurrent(transfer) &&
            transfer.pipelineIdentity === autoTransfersByPlayer[player] && sessionId != null &&
            transfer.segmentId.contains("/$sessionId/") &&
            transfer.ageMs(now) <= 10_000L && duration > 0 && transfer.bodyBytes > 0)
            transfer.bodyBytes.toDouble() * 8_000.0 / duration else null
        val downsideCost = autoDownsideCostBps(current, transfer, sessionId, now)
        val severe = link?.let { bps -> downsideCost?.let { bps < it * 0.7 } } == true
        autoUpgradeEvidence.bind(autoTransfersByPlayer[player], mediaMutationEpoch, now)
        if (severe && transfer != null) autoUpgradeEvidence.cliff(transfer.completedAtMs, now)
        val mild = autoMildSamples >= 2 && link?.let { bps -> downsideCost?.let { bps < it * (if (current.peak_bps != null) 1.3 else 1.0) } } == true
        val producer = autoActiveProductionPressure(sessionStatus,
            sessionStatusAgeMs?.let { now - it }, now, sessionId, current.id,
            player.bufferedPosition - player.currentPosition)
        if (!severe && !producer && !mild) return
        autoSwitchTimes.removeAll { now - it >= 3_600_000L }
        if (!severe && (autoSwitchTimes.size >= 6 || autoLastSwitchMs?.let { now - it < 60_000L } == true)) return
        val next = autoRecoveryCandidate(policyCatalog, current, autoDecoderRejected,
            link.takeIf { severe || mild }) ?: return
        if (!producer) {
            val incumbent = player
            val incumbentSession = sessionId ?: return
            val stalledPosition = realPosition()
            val sample = transfer ?: return
            val receipt = sample.receipt ?: return
            val payload = negativeLinkPayload() ?: return
            val beforeAck = monotonicNowMs()
            if (beforeAck < capturedAtMs || !acknowledgeNegativeLink(payload, receipt, deadlineMs - beforeAck) ||
                monotonicNowMs() >= deadlineMs || monotonicNowMs() < capturedAtMs ||
                !stallGuard.isCurrent(observation) || player !== incumbent || sessionId != incumbentSession ||
                autoActiveCandidateId != current.id || playbackIntent.desiredQuality != PlaybackQuality.Auto ||
                !player.playWhenReady || !presentationForeground || playbackIntent.pendingSeek != null ||
                player.playbackState != Player.STATE_BUFFERING || preparedPlayer != null || directedChange != null ||
                kotlin.math.abs(realPosition() - stalledPosition) >= 250L ||
                latestAutoCompletedTransfer?.receipt != receipt ||
                latestAutoCompletedTransfer?.completedAtMs != sample.completedAtMs ||
                sample.ageMs(monotonicNowMs()) > 15_000L) return
            autoLinkClaims[receipt] = Triple(sample.completedAtMs, true, current.id)
        }
        reportAutoRecoveryCause(if (producer) tv.plurx.app.data.ReopenReason.Encode else tv.plurx.app.data.ReopenReason.Link)
        autoDesiredCandidate = next
        sessionId?.let { session -> autoRecoveryCause = AutoRecoveryCauseTicket(session, player,
            current.id, next.id, if (producer) tv.plurx.app.data.ReopenReason.Encode else tv.plurx.app.data.ReopenReason.Link, now,
            linkReceipt = if (producer) null else latestAutoCompletedTransfer?.receipt) }
        playbackIntent.requestAutomaticCandidate(next.id, next.target_height)
        autoPreparing = false
        autoUpgradeSinceMs = null
        surfaceOwner.logOnly(mediaMutationEpoch, "auto_quality_recovery:${if (severe) "link" else "encode"}:${next.id}")
    }

    private fun noteAutoDecodeFailure(pressure: AutoDecodePressureEvidence? = null): Boolean {
        if (!tv.plurx.app.data.Session.displayAwareAuto || !tv.plurx.app.data.Session.autoAbr ||
            tv.plurx.app.data.Session.displayAwareAutoProtocol != "route-v1" ||
            autoRouteProtocol != "route-v1" || playbackIntent.desiredQuality != PlaybackQuality.Auto) return false
        val current = autoCatalog.firstOrNull { it.id == autoActiveCandidateId } ?: return false
        val session = sessionId ?: return false
        val observedAt = monotonicNowMs()
        // The failure is this device's own evidence, so the candidate joins the
        // local rejected set now. The server's acceptance of the sample only
        // authorizes server-side consequences (recording the decode cause on
        // the reopen), which the server checks for itself; nothing waits on it.
        if (!rejectFailedDecoderCandidate(autoDecoderRejected, current.id)) return false
        postPlaybackClientLog(scope, PlaybackClientLog(level = "warn",
            event = "candidate_recovery", message = "Candidate decoder evidence", ua = "Android Media3", sessionId = session,
            candidateRecovery = CandidateRecoverySample(cause = "decode", event_id = UUID.randomUUID().toString(),
                candidate_id = current.id, recipe_digest = current.recipe_digest, age_ms = 0,
                decoder_failed = pressure == null,
                rendered_elapsed_ms = pressure?.elapsedMs ?: 0, position_progress_ms = pressure?.progressMs ?: 0,
                dropped_frames = pressure?.droppedFrames ?: 0, runway_ms = (player.bufferedPosition - player.currentPosition).coerceAtLeast(0))))
        if (autoDecodeQualityResponseUsed) return false
        val next = autoRecoveryCandidate(autoCatalog, current, autoDecoderRejected, decoderRecovery = true) ?: return false
        autoDecodeQualityResponseUsed = true
        autoRecoveryCause = AutoRecoveryCauseTicket(session, player, current.id, next.id,
            tv.plurx.app.data.ReopenReason.Decode, observedAt)
        autoDesiredCandidate = next
        playbackIntent.requestAutomaticCandidate(next.id, next.target_height)
        autoPreparing = false
        autoUpgradeSinceMs = null
        return true
    }

    private var autoGate: String? = null

    /** One bounded log line whenever the reason Auto cannot decide changes. */
    private fun noteAutoGate(reason: String?) {
        if (reason == autoGate) return
        autoGate = reason
        Log.i("PlurxPlayback", "auto gate ${reason ?: "open"}")
    }

    private fun tickDisplayAwareAuto() {
        val now = monotonicNowMs()
        autoUpgradeEvidence.bind(autoTransfersByPlayer[player], mediaMutationEpoch, now)
        if (now < autoNextTickMs) return
        autoNextTickMs = now + 5_000L
        val gate = autoDecisionGate(AutoTickState(
            protocol = tv.plurx.app.data.Session.displayAwareAuto && tv.plurx.app.data.Session.autoAbr &&
                tv.plurx.app.data.Session.displayAwareAutoProtocol == "route-v1" && autoRouteProtocol == "route-v1",
            automatic = playbackIntent.desiredQuality == PlaybackQuality.Auto,
            playing = establishedPlayback && player.isPlaying && presentationForeground,
            seeking = playbackIntent.pendingSeek != null,
            changePending = autoPreparing || autoBoundaryAttempt != null || preparedPlayer != null || directedChange != null,
            controlClosed = controlObservationIsClosed,
            producerState = sessionStatus?.producer_state.takeIf { sessionStatusAgeMs?.let { it <= 15_000L } == true },
        ))
        if (gate != null) {
            noteAutoGate(gate)
            autoUpgradeSinceMs = null
            return
        }
        // A viewer boundary re-plans original first, once, on the first
        // evaluation where the incumbent is established, playing and has the
        // runway a handoff needs. The incumbent is never held for it.
        // Without a fresh link proof there is no candidate, and the boundary
        // stays owed for the next evaluation rather than being spent on nothing.
        autoBoundaryReplan.take(viewerTransportLifetime, player.bufferedPosition - player.currentPosition) {
            autoOriginalBoundaryCandidate(now)
        }?.let { chosen ->
            if (beginAutoBoundaryPreparation(chosen, now)) return
        }
        val target = autoPresentationTarget ?: return noteAutoGate("no_presentation_target")
        val policyCatalog = measuredCostCatalog()
        val current = policyCatalog.firstOrNull { it.hasValidIdentity && it.id == autoActiveCandidateId }
            ?: return noteAutoGate("no_active_candidate")
        reportCandidateLinkSample(negative = false)
        val transfer = latestAutoCompletedTransfer
        val duration = transfer?.bodyDurationMs
        val link = if (transfer != null && duration != null && transfer.networkLoad &&
            transfer.fromLocalCache == false && transfer.producerPaced == false &&
            transfer.statusCode == 200 && autoLinkAttested(transfer, current.id) && transfer.etag != null &&
            autoTransferOriginCurrent(transfer) &&
            transfer.pipelineIdentity === autoTransfersByPlayer[player] && sessionId != null &&
            transfer.segmentId.contains("/$sessionId/") &&
            transfer.ageMs(now) <= 10_000L && duration > 0 && transfer.bodyBytes > 0) {
            (transfer.bodyBytes.toDouble() * 8_000.0 / duration).takeIf { it.isFinite() && it > 0 }
        } else null
        noteAutoGate(if (link == null) "no_link_sample" else null)
        autoSwitchTimes.removeAll { now - it >= 3_600_000L }
        val area = current.width.toLong() * current.height
        val eligible = policyCatalog.filter {
            it.hasValidIdentity && it.id !in autoDecoderRejected && it.width > 0 && it.height > 0 &&
            it.decoder_compatible && (it.route != "encode" || it.grade == current.grade) && (autoBlockedUntil[it.id] ?: 0L) <= now }
        val downsideCost = autoDownsideCostBps(current, transfer, sessionId, now)
        val severe = link?.let { bps -> downsideCost?.let { bps < it * 0.7 } } == true
        if (severe && transfer != null) autoUpgradeEvidence.cliff(transfer.completedAtMs, now)
        val mild = link?.let { bps -> downsideCost?.let { bps < it * (if (current.peak_bps != null) 1.3 else 1.0) } } == true
        if (!mild) { autoMildSamples = 0; autoMildTransferCompletedMs = null }
        else if (autoMildTransferCompletedMs != transfer?.completedAtMs) {
            autoMildSamples = (autoMildSamples + 1).coerceAtMost(2)
            autoMildTransferCompletedMs = transfer?.completedAtMs
        }
        val producerPressure = autoActiveProductionPressure(sessionStatus,
            sessionStatusAgeMs?.let { now - it }, now, sessionId, current.id,
            player.bufferedPosition - player.currentPosition)
        val pressure = ((severe || autoMildSamples >= 2) &&
            (current.peak_bps != null || player.bufferedPosition - player.currentPosition < 10_000L)) || producerPressure
        val aspect = current.width.toDouble() / current.height
        val neededWidth = minOf(target.width_px.toDouble(), target.height_px * aspect)
        val neededHeight = minOf(target.height_px.toDouble(), target.width_px / aspect)
        val chosen = if (pressure) {
            autoUpgradeSinceMs = null
            autoRecoveryCandidate(eligible, current, autoDecoderRejected,
                link.takeIf { severe || autoMildSamples >= 2 })
        } else {
            val fitting = eligible.filter { candidate -> link?.let { bps ->
                candidate.peak_bps?.let { peak -> bps >= peak * 1.8 } ?: autoCatalog.firstOrNull {
                    it.id == candidate.id && it.recipe_digest == candidate.recipe_digest
                }?.let { autoUnknownOriginalTrial(it, autoMeasuredOutputs) && bps > 0 } ?: false
            } == true }
            val pick = autoPreferredDisplayCandidate(fitting, neededWidth, neededHeight)
            if (pick == null || !(pick.width.toLong() * pick.height > area ||
                    current.route == "encode" && pick.route != "encode") ||
                player.bufferedPosition - player.currentPosition < 10_000L ||
                autoSwitchTimes.size >= 6 || autoLastSwitchMs?.let { now - it < 60_000L } == true) {
                autoUpgradeSinceMs = null
                return
            }
            if (autoUpgradeSinceMs == null) autoUpgradeSinceMs = now
            if (!autoUpgradeEvidence.allowsProposal(now, autoUpgradeSinceMs)) return
            autoLastEvaluation?.let { evaluation ->
                if (autoTransfersByPlayer[player] === evaluation.first && mediaMutationEpoch == evaluation.second &&
                    (now < evaluation.third || now - evaluation.third < 60_000L)) return
            }
            pick
        } ?: return
        if (!severe && (autoSwitchTimes.size >= 6 || autoLastSwitchMs?.let { now - it < 60_000L } == true)) return
        autoDesiredCandidate = chosen
        if (!pressure) autoTransfersByPlayer[player]?.let { autoLastEvaluation = Triple(it, mediaMutationEpoch, now) }
        autoVoluntary = !pressure
        playbackIntent.requestAutomaticCandidate(chosen.id, chosen.target_height)
        autoPreparing = true
        autoPreparedTargetRevision = target.revision
        autoPreparedMutationEpoch = mediaMutationEpoch
        val epoch = mediaMutationEpoch
        val incumbent = player
        val continuousRequest = --continuousAutoRequest
        continuousAutoEpoch = epoch
        scope.launch {
            playbackControl.reportIntent()
            val continuous = liveContinuous()
            val row = continuous?.rendition(chosen.height, chosen.id)
            if (continuous != null && row != null) {
                if (mediaMutationEpoch != epoch || player !== incumbent || playbackIntent.desiredQuality != PlaybackQuality.Auto || autoDesiredCandidate?.id != chosen.id) return@launch
                try {
                    if (!withTimeout(12_000) {
                        continuous.change(row, baseMs + maxOf(player.bufferedPosition, player.currentPosition).coerceAtLeast(0), true, continuousRequest)
                    }) failAutoPreparation()
                } catch (_: TimeoutCancellationException) {
                    continuousAutoUncertain(continuousRequest, epoch, chosen.id)
                }
                catch (error: CancellationException) { throw error }
                catch (_: Exception) {
                    continuousAutoUncertain(continuousRequest, epoch, chosen.id)
                }
                return@launch
            }
            when (val step = playbackControl.awaitPreparedOffer(now)) {
                is PreparedOfferWait.Step.Offered -> {
                    if (mediaMutationEpoch == epoch && player === incumbent &&
                        playbackIntent.desiredQuality == PlaybackQuality.Auto &&
                        step.action.effectiveSelection?.candidateId == chosen.id) {
                        onPrepareAction(step.action)
                    } else failAutoPreparation()
                }
                else -> failAutoPreparation()
            }
        }
    }

    private fun continuousAutoUncertain(request: Long, epoch: Long, candidate: String) {
        if (request != continuousAutoRequest || epoch != mediaMutationEpoch ||
            playbackIntent.desiredQuality != PlaybackQuality.Auto || autoDesiredCandidate?.id != candidate) return
        raiseDegradedNotice("Auto quality change could not be confirmed. Playback continues while its outcome is checked.")
    }

    private fun autoTransferOriginCurrent(sample: AutoCompletedTransfer): Boolean {
        val actual = sample.origin?.let(tv.plurx.app.data.Session::canonicalOrigin) ?: return false
        val primary = tv.plurx.app.data.Session.canonicalPrimaryOrigin() ?: return false
        return actual == primary
    }

    private fun currentLinkReceipt(): String? {
        if (playbackIntent.desiredQuality != PlaybackQuality.Auto) return null
        val sample = latestAutoCompletedTransfer ?: return null
        val currentSession = sessionId ?: return null
        val receipt = sample.receipt ?: return null
        val duration = sample.bodyDurationMs ?: return null
        val uri = android.net.Uri.parse(sample.segmentId)
        if (sample.pipelineIdentity !== autoTransfersByPlayer[player] ||
            !uri.pathSegments.contains(currentSession) || autoActiveCandidateId == null ||
            sample.statusCode != 200 || !sample.networkLoad || sample.fromLocalCache != false ||
            !autoTransferOriginCurrent(sample) ||
            sample.producerPaced != false || sample.ageMs(monotonicNowMs()) > 15_000L ||
            duration !in 1..120_000L || sample.bodyBytes <= 0 || sample.etag.isNullOrEmpty() ||
            autoLinkClaims[receipt]?.first != sample.completedAtMs ||
            autoLinkClaims[receipt]?.third != autoActiveCandidateId ||
            !Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").matches(receipt)) return null
        return receipt
    }

    private fun reportCandidateLinkSample(negative: Boolean) {
        val now = monotonicNowMs()
        autoLinkClaims.entries.removeAll { now < it.value.first || now - it.value.first > 15_000L }
        val sample = latestAutoCompletedTransfer ?: return
        val currentSession = sessionId ?: return
        val candidate = autoCatalog.firstOrNull { it.hasValidIdentity && it.id == autoActiveCandidateId } ?: return
        val duration = sample.bodyDurationMs ?: return
        val receipt = sample.receipt ?: return
        val etag = sample.etag ?: return
        val uri = android.net.Uri.parse(sample.segmentId)
        if (playbackIntent.desiredQuality != PlaybackQuality.Auto ||
            sample.pipelineIdentity !== autoTransfersByPlayer[player] ||
            !uri.pathSegments.contains(currentSession) || candidate.id != autoActiveCandidateId ||
            !Regex("seg[0-9]+\\.(m4s|ts)").matches(uri.lastPathSegment.orEmpty()) ||
            sample.statusCode != 200 || !sample.networkLoad || sample.fromLocalCache != false ||
            !autoTransferOriginCurrent(sample) ||
            sample.producerPaced != false || sample.ageMs(now) > 15_000L ||
            duration !in 1..120_000L || sample.bodyBytes <= 0 || receipt.length != 36 || etag.isEmpty()) return
        val runway = (player.bufferedPosition - player.currentPosition).coerceAtLeast(0L)
        if (negative) {
            if (autoLinkClaims[receipt]?.second != false || !establishedPlayback || !presentationForeground ||
                !player.playWhenReady || player.playbackState != Player.STATE_BUFFERING ||
                playbackIntent.pendingSeek != null || preparedPlayer != null || directedChange != null ||
                runway > 1500 || sample.observedMediaDurationMs?.let { duration > it } != true) return
        } else if (autoLinkClaims.containsKey(receipt)) return
        if (autoLinkClaims.size >= 32 && !autoLinkClaims.containsKey(receipt)) return
        postPlaybackClientLog(scope, PlaybackClientLog(level = "warn", event = "candidate_link_sample",
            message = "Completed candidate body", ua = "Android Media3", sessionId = currentSession,
            linkSample = CandidateLinkSample(receipt, uri.lastPathSegment.orEmpty(), etag,
                sample.bodyBytes, duration, sample.ageMs(now), true, false, false, "link", negative,
                sample.observedMediaDurationMs, establishedPlayback && presentationForeground,
                player.playbackState == Player.STATE_BUFFERING, runway)))
        autoLinkClaims[receipt] = Triple(sample.completedAtMs, negative, candidate.id)
    }

    private fun negativeLinkPayload(): PlaybackClientLog? {
        val sample = latestAutoCompletedTransfer ?: return null
        val receipt = currentLinkReceipt() ?: return null
        val currentSession = sessionId ?: return null
        val duration = sample.bodyDurationMs ?: return null
        val media = sample.observedMediaDurationMs ?: return null
        val runway = (player.bufferedPosition - player.currentPosition).coerceAtLeast(0L)
        if (autoLinkClaims[receipt]?.second != false || !establishedPlayback || !presentationForeground ||
            !player.playWhenReady || player.playbackState != Player.STATE_BUFFERING ||
            playbackIntent.pendingSeek != null || preparedPlayer != null || directedChange != null ||
            runway > 1500 || duration <= media) return null
        val uri = android.net.Uri.parse(sample.segmentId)
        return PlaybackClientLog(level = "warn", event = "candidate_link_sample",
            message = "Completed candidate body", ua = "Android Media3", sessionId = currentSession,
            linkSample = CandidateLinkSample(receipt, uri.lastPathSegment.orEmpty(), sample.etag ?: return null,
                sample.bodyBytes, duration, sample.ageMs(monotonicNowMs()), true, false, false, "link", true,
                media, true, true, runway))
    }

    private fun reportAutoRecoveryCause(cause: tv.plurx.app.data.ReopenReason) {
        val session = sessionId ?: return
        val candidate = autoCatalog.firstOrNull { it.hasValidIdentity && it.id == autoActiveCandidateId } ?: return
        postPlaybackClientLog(scope, PlaybackClientLog(level = "warn", event = "candidate_recovery",
            message = "Candidate recovery evidence", ua = "Android Media3", sessionId = session,
            candidateRecovery = CandidateRecoverySample(cause = cause.name.lowercase(),
                event_id = UUID.randomUUID().toString(), candidate_id = candidate.id,
                recipe_digest = candidate.recipe_digest, age_ms = 0, decoder_failed = false,
                rendered_elapsed_ms = 0, position_progress_ms = 0, dropped_frames = 0,
                runway_ms = (player.bufferedPosition - player.currentPosition).coerceAtLeast(0L))))
    }

    private fun measuredCostCatalog(): List<tv.plurx.app.data.QualityCandidate> {
        val family = liveContinuous()
        return autoCatalog.map { candidate ->
            val peak = tv.plurx.app.data.measuredCandidatePeak(candidate, autoMeasuredOutputs)
            val output = if (peak == null) null else autoMeasuredOutputs.firstOrNull { it.matches(candidate) }
            val measured = candidate.copy(average_bps = output?.average_bps, peak_bps = peak)
            // A member of the live continuous family is costed by the family's
            // own declared delivery budget (video plus shared audio), which
            // [ContinuousAttachment.boundCatalog] binds; an unmeasured
            // advertised peak is still discarded for every other candidate.
            if (peak != null || family == null) measured
            else runCatching { family.boundCatalog(listOf(measured)).single() }.getOrDefault(measured)
        }
    }

    private fun autoOriginalBoundaryCandidate(now: Long): tv.plurx.app.data.QualityCandidate? {
        if (!Session.displayAwareAuto || !Session.autoAbr || Session.displayAwareAutoProtocol != "route-v1" ||
            autoRouteProtocol != "route-v1" || playbackIntent.desiredQuality != PlaybackQuality.Auto ||
            !playbackIntent.playbackRequested || !establishedPlayback || !presentationForeground ||
            autoPreparing || autoBoundaryAttempt != null || preparedPlayer != null || directedChange != null ||
            autoPresentationTarget == null ||
            !playbackControlBootstrapFence.isActive() || currentLinkReceipt() == null ||
            player.bufferedPosition - player.currentPosition < 10_000L ||
            !vm.preferences.value.preparedReplacement) return null
        val sample = latestAutoCompletedTransfer ?: return null
        val link = autoCompletedTransferBps(sample, now, 15_000L) ?: return null
        val current = measuredCostCatalog().firstOrNull { it.id == autoActiveCandidateId } ?: return null
        if (autoDownsideCostBps(current, sample, sessionId, now)?.let { link < it } == true) return null
        return autoCatalog.filter { candidate ->
            candidate.id != current.id && candidate.hasValidIdentity && candidate.decoder_compatible &&
                candidate.route == "remux" && candidate.id !in autoDecoderRejected &&
                (autoBlockedUntil[candidate.id] ?: 0L) <= now &&
                (tv.plurx.app.data.measuredCandidatePeak(candidate, autoMeasuredOutputs)?.let { link >= it * 1.8 }
                    ?: autoUnknownOriginalTrial(candidate, autoMeasuredOutputs))
        }.maxByOrNull { it.width.toLong() * it.height }
    }

    private fun autoBoundaryOwnerIsCurrent(boundary: AutoBoundaryAttempt): Boolean =
        autoBoundaryAttempt === boundary && boundary.epoch == mediaMutationEpoch &&
            player === boundary.incumbent && sessionId == boundary.sessionId &&
            playbackControlBootstrapFence.isActive() && presentationForeground &&
            playbackIntent.playbackRequested && playbackIntent.desiredQuality == PlaybackQuality.Auto &&
            viewerTransportLifetime === boundary.transportLifetime &&
            selectedAudio == boundary.audio && selectedSubtitle == boundary.subtitle &&
            audioOffsetMs == boundary.audioOffsetMs &&
            currentRecipe().recipe == boundary.recipe &&
            playbackIntent.pendingQualityChange?.sequence == boundary.qualitySequence

    private fun autoBoundaryIsCurrent(boundary: AutoBoundaryAttempt): Boolean =
        autoBoundaryOwnerIsCurrent(boundary) && autoDesiredCandidate?.id == boundary.candidateId

    /**
     * Ask for the boundary's original candidate through the same accepted
     * prepared transaction the mid-play upgrade uses, and return at once. The
     * incumbent is already playing (or already sought) for the viewer; the
     * successor primes beside it and the rendezvous hands off, or it is
     * abandoned and the incumbent simply carries on. The commit, the failure
     * and the withdrawal all settle through the prepared ledger's own events.
     */
    private fun beginAutoBoundaryPreparation(chosen: tv.plurx.app.data.QualityCandidate, now: Long): Boolean {
        val currentSession = sessionId ?: return false
        val boundary = AutoBoundaryAttempt(player, currentSession, mediaMutationEpoch, chosen.id,
            currentRecipe().recipe, playbackIntent.pendingQualityChange?.sequence, viewerTransportLifetime,
            selectedAudio, selectedSubtitle, audioOffsetMs, autoDesiredCandidate,
            playbackIntent.automaticCandidateId, playbackIntent.automaticCandidateHeight)
        autoBoundaryAttempt = boundary
        autoDesiredCandidate = chosen
        autoVoluntary = true
        autoPreparing = true
        autoPreparedTargetRevision = autoPresentationTarget?.revision
        autoPreparedMutationEpoch = mediaMutationEpoch
        playbackIntent.requestAutomaticCandidate(chosen.id, chosen.target_height)
        scope.launch {
            playbackControl.reportIntent()
            val step = playbackControl.awaitPreparedOffer(now)
            if (step is PreparedOfferWait.Step.Offered && autoBoundaryIsCurrent(boundary) &&
                step.action.effectiveSelection?.candidateId == chosen.id) {
                onPrepareAction(step.action)
                // The offer can build nothing: refused, a replay of a staging
                // that already settled, a closed controller, or an action
                // without a playlist or media origin. No prepared event will
                // ever arrive to settle this boundary then, so it is withdrawn
                // here, as Apple does when its offer does not own the change.
                if (autoBoundaryOfferBuiltNothing(autoBoundaryAttempt === boundary,
                        built = preparedPlayer != null, switched = preparedLedger.isSwitched)) {
                    if (preparedLedger.isLive && preparedLedger.actionId == step.action.actionId) {
                        abandonPreparedReplacement(failed = false)
                    }
                    withdrawAutoBoundary(boundary)
                }
            } else withdrawAutoBoundary(boundary)
        }
        return true
    }

    /** Put Auto back where the boundary found it, if nothing newer owns it. */
    private fun withdrawAutoBoundary(boundary: AutoBoundaryAttempt) {
        if (autoBoundaryAttempt === boundary) autoBoundaryAttempt = null
        if (autoDesiredCandidate?.id != boundary.candidateId) return
        autoDesiredCandidate = boundary.previousCandidate
        playbackIntent.requestAutomaticCandidate(boundary.previousId, boundary.previousHeight)
        autoPreparing = false
        playbackControl.reportEvidence()
    }

    private fun failAutoPreparation() {
        autoBoundaryAttempt = null
        autoDesiredCandidate?.let { autoBlockedUntil[it.id] = monotonicNowMs() + 300_000L }
        autoDesiredCandidate = null
        playbackIntent.requestAutomaticCandidate(null, null)
        autoPreparing = false
        autoUpgradeSinceMs = null
    }

    private fun playbackControlPlayerChanged() {
        tickDisplayAwareAuto()
        refreshControlWaiting()
        expireControlEvidenceIfProgressed()
        pollPreparedReplacement()
        playbackControl.playerChanged()
    }

    /** Audio-only playback has no video-frame callback; an advancing active clock is presentation. */
    private fun settleAudioPlaybackIntentIfPresented() {
        if (plan.videoCodec != null) return
        val sequence = playbackIntent.pendingSeek?.sequence ?: return
        if (playbackTelemetry.presentedAudio(
                playbackIntent,
                realPosition(),
                sequence,
                observedAtMs = monotonicNowMs(),
                playbackActive = player.isPlaying,
                playbackRate = player.playbackParameters.speed.toDouble(),
                presentationReady = selectionRecipe?.let(recipeOwnership::canPresent) == true &&
                    !textSelectionArmed && !audioSelectionArmed,
            )
        ) playbackControl.playerChanged()
    }

    /** STARTED includes visible PiP; stopped video is owner-paused without changing intent. */
    fun setPresentationForeground(foreground: Boolean, inPictureInPicture: Boolean = false) {
        if (!playbackControlBootstrapFence.isActive()) return
        val visible = foreground || inPictureInPicture
        val visibilityChanged = presentationForeground != visible
        if (visibilityChanged) stallGuard.invalidateObservation()
        presentationForeground = visible
        val lifecycle = lifecyclePlaybackTransition(
            ownerPaused = lifecyclePaused,
            foreground = foreground,
            inPictureInPicture = inPictureInPicture,
            audioOnly = plan.isAudioOnly,
            viewerRequested = playbackIntent.playbackRequested,
        )
        lifecyclePaused = lifecycle.ownerPaused
        if (visibilityChanged) {
            explicitViewerPause = null
            revokeAutoBoundaryTransport()
        }
        when (lifecycle.effect) {
            LifecyclePlaybackEffect.PauseVideo -> {
                applyEffectivePlayWhenReady()
                playbackTelemetry.report(
                    event = "playback_lifecycle_pause",
                    level = "info",
                    message = "Video paused when its activity stopped",
                    detail = "intent=play",
                )
            }
            LifecyclePlaybackEffect.ResumeVideo -> {
                pendingLifecyclePauseCallback = false
                player.playWhenReady = true
            }
            LifecyclePlaybackEffect.None -> if (visible) pendingLifecyclePauseCallback = false
        }
        // Android has no hidden page; this is the contract's `hidden`.
        surfaceOwner.hidden(!visible)
        sampleTargetPresentationDeadline()
    }

    private fun sampleTargetPresentationDeadline() {
        if (!playbackControlBootstrapFence.isActive()) return
        playbackTelemetry.supersedeForIntent(playbackIntent.pendingSeek?.sequence)
        settleVideoPlaybackIntentIfPresented()
        val now = monotonicNowMs()
        continuousAttachment?.takeIf { player === continuousPlayer }?.playback(realPosition(),
            player.playbackParameters.speed.toDouble(), player.playWhenReady && presentationForeground && player.playbackState != Player.STATE_ENDED, now)
        samplePreparedCommitFrameBudget()
        val event = targetPresentationDeadline.sample(
            pending = playbackIntent.pendingSeek,
            playbackRequested = player.playWhenReady && player.playbackState != Player.STATE_ENDED,
            foreground = presentationForeground,
            nowMs = now,
            expectedOwner = targetPresentationOwner,
        ) ?: return
        if (!playbackIntent.isCurrent(event.sequence)) return
        // This deadline is about the requested output. Progress on a departed
        // timeline cannot cancel it, and a control hold cannot extend it.
        seekJob?.cancel()
        mediaMutationEpoch += 1
        attachSurfaceGeneration()
        stallGuard.invalidateForPlaybackAttempt()
        reportControlEvidence(
            presentationStallEvidence(),
            render = RenderState.STALLED,
        )
        playbackTelemetry.report(
            event = "playback_target_timeout",
            level = "warn",
            message = "The requested playback destination did not present.",
            detail = "target_ms=${event.targetMs} terminal=${event.terminal}",
        )
        if (event.terminal) {
            surfaceOwner.exhaustedAfterTargetDeadline(mediaMutationEpoch, event.targetMs)
        } else if (targetPresentationDeadline.recover(event, now, expectedOwner = targetPresentationOwner)) {
            restartAt(event.targetMs, "presentation-recovery", now)
            // §3.3 row 14: a readiness deadline that fires with a rung left is
            // its own source, so the ledger names what actually happened. It is
            // about the generation the restart just produced.
            surfaceOwner.recoveringReadinessDeadline(
                mediaMutationEpoch,
                surfaceContext(),
                "Getting back to the requested position.",
            )
        }
    }

    private fun settleVideoPlaybackIntentIfPresented() {
        val pending = playbackIntent.pendingSeek ?: return
        val first = firstVideoFrameForSeek ?: return
        // A progressive remux may start at the preceding keyframe. Its first
        // rendered frame proves the new surface is live; later rendered output
        // and a clock that has crossed the target prove arrival at the seek.
        if (!progressiveTransport || first.first != pending.sequence ||
            first.second !in (pending.targetMs - 2_000L)..pending.targetMs ||
            !presentationForeground || !player.isPlaying ||
            textSelectionArmed || audioSelectionArmed ||
            selectionRecipe?.let(recipeOwnership::canPresent) != true ||
            (player.videoDecoderCounters?.renderedOutputBufferCount ?: 0) <= first.third
        ) return
        if (playbackTelemetry.presentedVideoProgress(playbackIntent, realPosition(), pending.sequence, monotonicNowMs())) {
            playbackControl.playerChanged()
            disarmVideoPresentation()
            firstVideoFrameForSeek = null
        }
    }

    // ------------------------------------------------ surface, owner side

    /**
     * Which context a fault raised right now belongs to (contract §3.3).
     *
     * `start` until this playback has presented a picture, `attached` after.
     * `change` is decided at the two sites that know a viewer-requested
     * replacement is pending over a predecessor that is still attached.
     */
    private fun surfaceContext(): SurfaceContext =
        if (establishedPlayback) SurfaceContext.Attached else SurfaceContext.Start

    /**
     * True while an item the failed replacement was going to replace is still
     * attached and playing out of its buffer.
     *
     * `establishedPlayback` cannot answer this: `beginPlaybackAttempt` clears
     * it before the create is even issued, which is exactly the window this
     * question is asked in. The player's own item is what survives that.
     */
    private fun predecessorAttached(): Boolean =
        player.currentMediaItem != null && player.playbackState != Player.STATE_IDLE

    /** A generation change: the faults about the old one stop being about anything. */
    private fun attachSurfaceGeneration() {
        surfaceEvidencePositionMs = null
        surfaceEvidenceSampledAtMs = null
        surfaceOwner.attach(mediaMutationEpoch)
    }

    /**
     * Feed the presenter this player's own presentation proof (contract §3.4).
     *
     * No new detector and no new timer: [realPosition] is the sample the stall
     * watchdog already takes, and foreground plus `playWhenReady` are the gates
     * the contract names. A clock that has not moved between two samples is not
     * a presenting picture, whatever the player's state flags say.
     */
    private fun sampleSurfacePresentation(positionMs: Long, observedAtMs: Long) {
        val previousAt = surfaceEvidenceSampledAtMs
        if (previousAt != null && observedAtMs - previousAt < SURFACE_EVIDENCE_INTERVAL_MS) return
        val previous = surfaceEvidencePositionMs
        surfaceEvidencePositionMs = positionMs
        surfaceEvidenceSampledAtMs = observedAtMs
        surfaceOwner.presenting(
            presentationEvidence(
                previousPositionMs = previous,
                positionMs = positionMs,
                playbackRequested = player.playWhenReady,
                foreground = presentationForeground,
            ),
            mediaMutationEpoch,
        )
    }

    /**
     * `STATE_BUFFERING` after start, as the contract's `media_waiting` (row 12).
     *
     * This is the whole of the retired spinner's detection, unchanged:
     * [playbackIsWaiting] is the same predicate `isPlaybackWaiting` read, on the
     * same loop, and the 350 ms debounce the class carries is the reducer's. No
     * threshold, budget or detector moves — what moves is who owns the pixel.
     *
     * `establishedPlayback` picks the row: with a picture established the wait
     * is `media_waiting`; before the first frame it is not a wait but the open,
     * and [mediaWaitSource] says which. Both halves matter here and not only in
     * [restartAt] — `executeSeek` deliberately bypasses `restartAt`, and its
     * four transports and the node failover all call [beginPlaybackAttempt],
     * which clears `establishedPlayback` right after `attachSurfaceGeneration`
     * has retired every fault about the outgoing generation. Without the
     * `client_preparing` answer the sampler makes, every seek and every failover
     * is a frozen or black picture with nothing drawn over it until the first
     * frame renders — which is precisely what the retired spinner covered.
     *
     * `playWhenReady` is inside [playbackIsWaiting]: a viewer who paused is not
     * waiting for the stream, and the presenter's evidence needs the same flag,
     * so a fault raised over a paused player would have nothing to retire it.
     */
    private fun sampleSurfaceWait() {
        val waiting = playbackIsWaiting(player.playWhenReady, player.playbackState)
        when (mediaWaitSource(waiting, establishedPlayback)) {
            SurfaceSources.MEDIA_WAITING ->
                surfaceOwner.bufferingMediaWait(mediaMutationEpoch, realPosition())
            SurfaceSources.CLIENT_PREPARING ->
                surfaceOwner.preparingClientOpen(mediaMutationEpoch, SurfaceContext.Start)
            else -> Unit
        }
    }

    /** A destination that landed settles its faults; one replaced supersedes them. */
    private fun sampleSurfaceIntent() {
        val current = playbackIntent.pendingSeek?.sequence
        val previous = surfaceIntentSequence
        surfaceIntentSequence = current
        if (previous == null || previous == current) return
        if (current == null) surfaceOwner.intentSettled(previous) else surfaceOwner.intentSuperseded(previous)
    }

    /**
     * One pass of the presenter's inputs, on a loop that already runs.
     *
     * Called from both watchdogs: the stall sampler is where §3.4 puts the
     * evidence, and the target-deadline sampler keeps the timed notices moving
     * while the stall sampler is inside a bounded control ask.
     */
    private fun sampleSurface(positionMs: Long, observedAtMs: Long) {
        surfaceOwner.hidden(!presentationForeground)
        sampleSurfacePresentation(positionMs, observedAtMs)
        sampleSurfaceWait()
        sampleSurfaceIntent()
        surfaceOwner.tick()
    }

    /**
     * The control reporter has stopped and will not exchange again (§3.3 row 18).
     *
     * A 404 `session_gone` on a successor's first exchange, a body this client
     * cannot read, any refusal the protocol does not make retryable. The player
     * keeps its item and keeps playing — control reporting is not playback — so
     * the surface does not move and the event is the only trace.
     */
    private fun controlReportingGaveUp(failure: String) {
        surfaceOwner.logOnly(mediaMutationEpoch, "control reporting stopped ($failure)")
        // The item may still be attached and able to play out its buffer, but
        // the session behind it is gone: Play must open the replacement rather
        // than run into the playlist's 404.
        val retiredSession = sessionId
        if (isPauseGraceExpiry(failure) && !playbackIntent.playbackRequested &&
            retiredSession != null && !sessionIsVod && currentPausedRetirement() == null
        ) {
            pausedRetirement = PausedRetirement(retiredSession, realPosition())
        }
    }

    /** The contract's client-log events (§3.6), on the reporter that already exists. */
    private fun reportSurfaceLog(entry: SurfaceLog) {
        val level = when (entry.event) {
            SurfaceLogEvents.RAISED, SurfaceLogEvents.DISAGREEMENT -> "warn"
            SurfaceLogEvents.ERROR -> "error"
            else -> "info"
        }
        val detail = buildString {
            entry.cls?.let { append("class=").append(it.wire).append(' ') }
            append("source=").append(entry.source)
            append(" attached=").append(entry.attached)
            entry.intent?.let { append(" intent=").append(it) }
            entry.positionMs?.let { append(" position_ms=").append(it) }
            if (entry.actions.isNotEmpty()) {
                append(" actions=").append(entry.actions.joinToString(",") { action -> action.wire })
            }
            append(" player_stopped=").append(entry.playerStopped)
            append(" presenting=").append(surfaceOwner.isPresenting)
            append(" rate=").append(player.playbackParameters.speed)
            entry.by?.let { append(" by=").append(it) }
            entry.action?.let { append(" action=").append(it.wire) }
            entry.error?.let { append(" error=").append(it) }
            entry.context?.let { append(" context=").append(it) }
            entry.detail?.let { append(" detail=").append(it) }
        }
        playbackTelemetry.report(
            event = entry.event,
            level = level,
            message = entry.cls?.wire ?: entry.source ?: entry.event,
            detail = detail,
        )
    }

    /**
     * The HTTP refusal Media3 wrapped inside this error, if there is one.
     *
     * One walk, two readers: `mediaRefusal` wants the body and the status to
     * build a surface source id, `httpResponseCode` wants only the status to
     * decide whether another node is worth trying. The depth bound is the
     * original's and stays: Media3 wraps the loader exception a level or two
     * deep, four links covers every shape seen, and an unbounded walk over a
     * cyclic cause chain does not return.
     */
    private fun invalidResponse(
        error: PlaybackException,
    ): HttpDataSource.InvalidResponseCodeException? {
        var cause: Throwable? = error.cause
        var depth = 0
        while (cause != null && cause !is HttpDataSource.InvalidResponseCodeException && depth < 4) {
            cause = cause.cause
            depth += 1
        }
        return cause as? HttpDataSource.InvalidResponseCodeException
    }

    /**
     * The HTTP status this error carries, or `null` when it never got a
     * response. `nodeFailoverEligible` turns it into the failover decision.
     */
    private fun httpResponseCode(error: PlaybackException): Int? =
        invalidResponse(error)?.responseCode

    /**
     * A playlist or segment refusal Media3 wrapped, as a contract source id.
     *
     * The plan names `error.cause`; Media3 wraps the loader exception a level
     * or two deeper on some paths, so `invalidResponse` walks the chain to a
     * small bounded depth rather than reading only its first link.
     * Classification is by status, as
     * §3.5 gives it — the body only supplies the sentence and the position, and
     * a body that is not a legible refusal supplies neither.
     */
    private fun mediaRefusal(error: PlaybackException): MediaRefusal? {
        val response = invalidResponse(error) ?: return null
        val body = try {
            String(response.responseBody, Charsets.UTF_8)
        } catch (_: Exception) {
            null
        }
        val refusal = tv.plurx.app.data.parseRefusal(response.responseCode, body)
        // 401/403 and 410 are status-based by §3.5: the status IS the answer
        // and a body cannot take it away. A 503 is not — contract §3.3 row 8 is
        // a 503 "with a 'not yet' code", and Android is the one client that can
        // read one. Whether a code is required at all is the ROW's to say, so
        // `surfaceRowAdmitsCode` asks it rather than this adapter deciding:
        // while the row lists no codes it claims the status outright and this
        // is exactly today's behaviour; once it lists them, a 503 carrying
        // something else is not row 8's refusal and falls through to whatever
        // row the code does name, or to what the caller's own ladder was
        // already doing — rather than claiming `recovering` on the strength of
        // a status nobody explained.
        val source = when (response.responseCode) {
            410 -> SurfaceSources.MEDIA_OWNER_LOST_410
            401, 403 -> SurfaceSources.AUTH_401_403
            503 -> if (
                surfaceRowAdmitsCode(
                    SurfaceSources.SEGMENT_503_NOT_YET,
                    refusal?.code,
                    SurfaceContext.Attached,
                )
            ) {
                SurfaceSources.SEGMENT_503_NOT_YET
            } else {
                // Not row 8's refusal. Whatever row the code DOES name, or none.
                surfaceSourceForCode(refusal?.code, SurfaceContext.Attached)
            }
            else -> return null
        }
        if (source == null && refusal == null) return null
        return MediaRefusal(source, refusal?.message, refusal?.positionMs)
    }

    /**
     * A notice beside the picture: an automatic downshift, an HDR-subtitle
     * refusal, a PGS or PiP failure (§3.3 row 17). Banner, timed, no stop.
     */
    private fun raiseDegradedNotice(message: String) {
        surfaceOwner.degradedNotice(mediaMutationEpoch, surfaceContext(), message)
    }

    /**
     * The owner started a recovery step (§3.3 rows 8, 12 and 13): a node
     * failover, a compatibility rung, a reopen. A progress fault, no stop.
     */
    private fun raiseRecoveryStep(refusal: MediaRefusal?, reason: String) {
        val context = surfaceContext()
        val detail = refusal?.message ?: reason
        when {
            // `segment_503_not_yet` is an attached-context row; before the
            // first frame the same failure is the owner's own recovery step and
            // nothing more specific is true. A 410 is `any` context since
            // upstream 97231607, because it is an answer on a create too.
            refusal?.source == SurfaceSources.SEGMENT_503_NOT_YET &&
                context == SurfaceContext.Attached ->
                surfaceOwner.recoveringSegmentRefusal(mediaMutationEpoch, refusal.positionMs, detail)
            refusal?.source == SurfaceSources.MEDIA_OWNER_LOST_410 ->
                surfaceOwner.recoveringMediaOwnerLost(
                    mediaMutationEpoch,
                    context,
                    refusal.positionMs,
                    detail,
                )
            else -> surfaceOwner.recoveringOwnerStep(mediaMutationEpoch, context, detail)
        }
    }

    /**
     * `onPlayerError` reached the end of the ladder: stop, then raise (§3.4).
     *
     * A 410 names the place the viewer should be put back at, so it is raised
     * first as its own `recovering` fault and this owner's own stop promotes it
     * (contract row 12) — which is what carries the position and its Try again
     * into the terminal. A 401/403 keeps its own row so the prompt keeps its
     * Sign in. Anything else is the owner's `stopped` with today's sentence.
     */
    private fun stopAndRaisePlaybackFailure(refusal: MediaRefusal?, sentence: String) {
        playbackTelemetry.cancelPending()
        val attached = mediaMutationEpoch
        if (refusal != null && refusal.source == SurfaceSources.MEDIA_OWNER_LOST_410) {
            surfaceOwner.recoveringMediaOwnerLost(
                attached = attached,
                context = surfaceContext(),
                positionMs = refusal.positionMs ?: realPosition(),
                detail = refusal.message ?: sentence,
                actions = listOf(SurfaceAction.Retry, SurfaceAction.Close),
            )
        }
        surfaceOwner.stoppedAfterPlaybackError(
            attached = attached,
            source = if (refusal?.source == SurfaceSources.AUTH_401_403) {
                SurfaceSources.AUTH_401_403
            } else {
                SurfaceSources.OWNER_STOPPED
            },
            positionMs = refusal?.positionMs,
            detail = refusal?.message ?: sentence,
        )
    }

    /**
     * A session create this playback was waiting on failed (§3.4).
     *
     * With no predecessor attached there is nothing behind the failure, so the
     * owner stops and raises a terminal. With one attached — a quality change,
     * a seek that reopens, a stall reopen — the picture the viewer is watching
     * is not the one that failed: context `change`, a banner with Try again,
     * and NO stop.
     */
    private fun raiseSessionCreateFailure(
        error: Throwable,
        sentence: String,
        predecessor: Boolean,
    ) {
        val status = refusalStatusOf(error)
        val refusal = error as? RefusalException
        val attached = mediaMutationEpoch
        val message = refusal?.message ?: sentence
        // R1: 401/403 beats context, everywhere. §3.3's table is first-match and
        // the `auth_401_403` row (context `any`) sits above `change_failed`
        // precisely so a pending change cannot demote a refused credential into
        // a banner the viewer can ignore. It maps to a blocking class, so the
        // owner stops the player first.
        if (status == 401 || status == 403) {
            surfaceOwner.stoppedAfterSessionCreate(
                attached = attached,
                source = SurfaceSources.AUTH_401_403,
                positionMs = refusal?.positionMs,
                detail = message,
            )
            return
        }
        if (predecessor) {
            surfaceOwner.refusedChange(
                attached = attached,
                intent = playbackIntent.pendingSeek?.sequence,
                positionMs = refusal?.positionMs,
                detail = message,
            )
            return
        }
        surfaceOwner.stoppedAfterSessionCreate(
            attached = attached,
            source = createFailureSource(refusal),
            positionMs = refusal?.positionMs,
            detail = message,
        )
    }

    /**
     * Is this create refusal the fixture's `create_503_not_yet` row (M5)?
     *
     * The server's own code is what says so. A bodiless 503 is not a "still
     * building" answer — it is a 503 nobody explained — and retrying one would
     * be guessing.
     */
    private fun createIsStillBuilding(failure: Throwable): Boolean {
        val code = (failure as? RefusalException)?.code ?: return false
        return code in CreateRetry.codes
    }

    /**
     * A create refusal as a contract source id.
     *
     * The VOD refusals have their own terminal rows, and 401/403 keeps its Sign
     * in whether or not the body was legible.
     *
     * A "not yet" 503 is still the owner's `stopped` here, and after M5 that is
     * a narrower statement than it looks: the START-context creates that could
     * produce one are consumed by `createRetryingNotYet`, which raises
     * `create_503_not_yet` while it retries and `owner_exhausted` when it gives
     * up. What reaches this function is a "not yet" in a context row 6 does not
     * claim — a pending change, or a stall reopen — where the honest answer is
     * the one the client already gave.
     */
    private fun createFailureSource(refusal: RefusalException?): String = when {
        refusal == null -> SurfaceSources.OWNER_STOPPED
        refusal.code == "vod_source_rescan_required" -> SurfaceSources.VOD_SOURCE_RESCAN_REQUIRED
        refusal.code == "vod_source_unsupported" -> SurfaceSources.VOD_SOURCE_UNSUPPORTED
        refusal.code == "vod_transcode_unavailable" -> SurfaceSources.VOD_TRANSCODE_UNAVAILABLE
        refusal.code == "vod_subtitle_burn_unavailable" -> SurfaceSources.VOD_SUBTITLE_BURN_UNAVAILABLE
        refusal.code == "vod_disabled" -> SurfaceSources.VOD_DISABLED
        else -> SurfaceSources.OWNER_STOPPED
    }

    /**
     * The `exhausted` prompt's Keep waiting (§3.1): one more bounded attempt on
     * the ladder that already exists. The reopen budget and the sessionless
     * recovery are reset and the transport is asked for again; no budget,
     * threshold or detector changes.
     */
    fun keepWaiting() {
        if (!playbackControlBootstrapFence.isActive()) return
        stallReopenBudget.reset()
        sessionlessStallRecoveryUsed = false
        sessionlessStallRecoveryPositionMs = null
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        stallGuard.setPlaybackRequested(playbackIntent, true) {
            samplePreparedCommitFrameBudget()
            player.playWhenReady = it && !lifecyclePaused
        }
        playbackControl.playerChanged()
    }

    /**
     * The viewer answered the surface. The reducer clears the fault the action
     * belonged to; the EFFECT of the action belongs to the owner and the screen.
     */
    internal fun surfaceAction(action: SurfaceAction) {
        surfaceOwner.userAction(action)
    }

    private fun refreshControlWaiting() {
        if (player.playbackState == Player.STATE_BUFFERING) {
            if (controlWaitingSince == null) controlWaitingSince = monotonicNowMs()
        } else {
            controlWaitingSince = null
        }
    }

    private fun expireControlEvidenceIfProgressed() {
        val publishedAt = controlEvidencePositionMs ?: return
        // Any movement, not only forward movement. A VOD seek never reopens
        // the session, so a viewer scrubbing BACK from a stall would otherwise
        // leave the override in place for the rest of the title — and the
        // mapper ranks an override above everything the player reports, so
        // every exchange for the next hour of healthy playback would say
        // `stalled`.
        if (realPosition() == publishedAt) return
        controlObservationOverride = null
        controlRenderOverride = null
        controlEvidencePositionMs = null
    }

    // ------------------------------------------------ prepared replacement

    private val preparedVideoSurfaces = PreparedVideoSurfaces.create(!plan.isAudioOnly)
    internal val keepsWarmVideoOutputs: Boolean get() = preparedVideoSurfaces != null
    internal val presentationPlayer: Player
        get() = preparedVideoSurfaces?.presenter(player) ?: player

    private val preparedLedger = PreparedReplacementLedger()
    private var preparedPlayer: ExoPlayer? = null

    /**
     * Where the two pipelines are to meet, and the arithmetic of getting there.
     *
     * This replaces a chase. The successor used to be seeked onto the
     * incumbent's *current* position while the incumbent was frozen at that
     * frame — `playWhenReady = false` — so that it could not move away while
     * the asynchronous seek landed. That freeze was a visible pause on every
     * prepared switch: the one thing the whole handoff exists to avoid.
     *
     * The successor is now parked a lead ahead with `playWhenReady = false` and
     * waits there, and the incumbent keeps playing until it arrives. Seeking
     * the successor ahead and letting it *run* does not work and was tried on
     * paper first: two pipelines at the same rate keep whatever gap the seek
     * landed with, so a 1.5 s lead stays 1.5 s forever and never closes to the
     * 250 ms the commit needs.
     */
    private var rendezvous: RendezvousHold? = null

    /** The successor's parking seek has completed, per its own listener. */
    private var rendezvousSeekObserved = false

    /**
     * The coroutine waiting for the incumbent to reach the rendezvous.
     *
     * A coroutine rather than the one-second tick that drives the rest of this
     * path: the commit accepts 250 ms of error, and a one-second sampler misses
     * that window four times out of five.
     */
    private var rendezvousJob: Job? = null
    private var preparedOrigin: ProgressiveMediaOrigin? = null
    private var preparedListener: Player.Listener? = null
    private var preparedStartedAtMs = 0L
    private var preparedReadinessDeadlineJob: Job? = null
    private data class AutoStagedObservation(val player: ExoPlayer, val meter: AutoTransferEvidence,
        val sessionId: String, val candidateId: String, val recipeDigest: List<Int>,
        val startedAtMs: Long, val deadlineMs: Long, val credentialOrigin: String, val credentialToken: String?,
        var playlistUrl: String? = null, var immutableVod: Boolean = false)
    private var autoStagedObservation: AutoStagedObservation? = null
    private var autoStagedPlaylistJob: Job? = null

    private fun autoStagedObservationCurrent(observation: AutoStagedObservation, nowMs: Long): Boolean =
        nowMs >= observation.startedAtMs && nowMs < observation.deadlineMs &&
            Session.origin == observation.credentialOrigin && Session.token == observation.credentialToken &&
            preparedPlayer === observation.player && autoTransfersByPlayer[observation.player] === observation.meter &&
            preparedLedger.action?.sessionId == observation.sessionId &&
            preparedLedger.action?.effectiveSelection?.candidateId == observation.candidateId &&
            autoDesiredCandidate?.id == observation.candidateId &&
            autoDesiredCandidate?.recipe_digest == observation.recipeDigest

    /** One received media-playlist lookup, bound to the stage and original deadline. */
    private fun captureAutoStagedVodPlaylist(observation: AutoStagedObservation) {
        if (!autoStagedObservationCurrent(observation, monotonicNowMs()) || observation.playlistUrl != null) return
        val manifest = observation.player.currentManifest as? HlsManifest ?: return
        val url = manifest.mediaPlaylist.baseUri
        val uri = Uri.parse(url)
        if (Session.canonicalOrigin(url) != Session.canonicalOrigin(observation.credentialOrigin) ||
            !uri.pathSegments.contains(observation.sessionId) || uri.lastPathSegment?.endsWith(".m3u8") != true) return
        observation.playlistUrl = url
        val remaining = observation.deadlineMs - monotonicNowMs()
        if (remaining <= 0) return
        val client = (observation.credentialToken?.let(Net::profileClient) ?: Net.capabilityClient)
            .newBuilder().retryOnConnectionFailure(false)
            .callTimeout(remaining, java.util.concurrent.TimeUnit.MILLISECONDS).build()
        autoStagedPlaylistJob = scope.launch {
            val valid = withContext(Dispatchers.IO) {
                runCatching {
                    client.newCall(okhttp3.Request.Builder().url(url).header("Cache-Control", "no-cache").build())
                        .execute().use responseBody@{ response ->
                            val body = response.body
                            if (response.code != 200 || response.request.url.toString() != url || body == null ||
                                body.contentLength() > 1_048_576L) return@responseBody false
                            val bytes = java.io.ByteArrayOutputStream()
                            body.byteStream().use { input ->
                                val buffer = ByteArray(8192)
                                while (true) {
                                    if (monotonicNowMs() >= observation.deadlineMs) return@responseBody false
                                    val count = input.read(buffer)
                                    if (count < 0) break
                                    if (bytes.size() + count > 1_048_576) return@responseBody false
                                    bytes.write(buffer, 0, count)
                                }
                            }
                            (body.contentLength() < 0 || body.contentLength() == bytes.size().toLong()) &&
                                autoImmutableVodPlaylist(bytes.toByteArray())
                        }
                }.getOrDefault(false)
            }
            if (valid && autoStagedObservation === observation &&
                autoStagedObservationCurrent(observation, monotonicNowMs())) observation.immutableVod = true
        }
    }

    /**
     * When the switch happened, while the commit still waits for the frame that
     * proves it. Null when no commit is outstanding.
     */
    private var awaitingCommitFrameSinceMs: Long? = null
    private var preparedCommitFrameBudget: PreparedActiveWallBudget? = null

    private fun samplePreparedCommitFrameBudget(): Boolean? = preparedCommitFrameBudget?.update(
        nowMs = monotonicNowMs(),
        playbackRequested = playbackIntent.playbackRequested && presentationForeground,
    )

    /** Everything overwritten when a prepared successor becomes incumbent. */
    private data class PreparedPredecessor(
        val player: ExoPlayer,
        val filmPositionMs: Long,
        val recipe: PlaybackRecipeOwnership.Claim?,
        val transport: PlaybackMediaTransport?,
        val modeOverride: String?,
        val requiresHlsOverride: Boolean?,
        val progressiveMediaOrigin: ProgressiveMediaOrigin,
        val baseMs: Long,
        val sessionIsVod: Boolean,
        val sessionId: String?,
        val activeMediaPath: String?,
        val deliveredRange: String?,
        val deliveredDolbyVisionProfile: Int?,
        val encoder: String?,
        val sessionStatus: PlaybackSessionStatus?,
        val sessionStatusObservedAtMs: Long?,
        val establishedPlayback: Boolean,
    )

    /** Retained until a real successor frame makes rollback unnecessary. */
    private var preparedPredecessor: PreparedPredecessor? = null

    /** Ordinary reopen to begin only after the surface has returned to A. */
    private var preparedRollbackReopen: Pair<Long, String>? = null

    /**
     * The player the surface has moved off. During prepared handoff this is the
     * predecessor and remains retained until first-frame proof. During rollback
     * it becomes the frame-less successor and is collected once the surface is
     * visibly back on the predecessor.
     */
    private var retiredPlayer: ExoPlayer? = null

    /**
     * When it was parked, so the watchdog can collect one the composition never
     * came back for after commit or rollback. While first-frame proof is still
     * outstanding, [collectRetiredPlayer] deliberately refuses: that bounded
     * five-second window is the rollback authority, not a leaked decoder.
     */
    private var retiredParkedAtMs = 0L

    /**
     * Release the player the surface has just moved off.
     *
     * Called by the screen from the `AndroidView` update that re-points the
     * `PlayerView`, because that is the only place that *knows* the surface has
     * moved. A timer here would be guessing about a frame clock that is parked
     * whenever the window is not visible, and releasing early leaves the view
     * calling into a released player.
     */
    fun collectRetiredPlayer() {
        val retired = retiredPlayer ?: return
        if (preparedVideoSurfaces?.exposurePending == true &&
            monotonicNowMs() - retiredParkedAtMs < PREPARED_OVERLAP_BOUND_MS) return
        if (awaitingCommitFrameSinceMs != null && preparedPredecessor?.player === retired) {
            // The surface moved, but the successor has not proved a frame. A
            // timeout must still be able to put this exact pipeline back.
            return
        }
        retiredPlayer = null
        retiredParkedAtMs = 0L
        if (preparedPredecessor?.player === retired) preparedPredecessor = null
        preparedVideoSurfaces?.remove(retired)
        if (retired === continuousPlayer) continuousAttachment?.closeAdmission()
        retired.release()
        if (retired === continuousPlayer) {
            continuousAttachment?.finishAfterRelease()
            continuousAttachment = null
        }
        val reopen = preparedRollbackReopen ?: return
        preparedRollbackReopen = null
        restartAt(reopen.first, reopen.second)
    }

    /**
     * The acknowledgement the next exchange will carry.
     *
     * One slot, not a queue: the ladder is monotonic and the server keeps the
     * furthest state it was told, so an acknowledgement replaced before it went
     * out is one the newer one supersedes. Cleared when the exchange carrying
     * it comes back accepted; a failed exchange replays the same request, so
     * holding it until then is what makes a lost commit retry rather than
     * vanish.
     */
    private var pendingAcknowledgement: ActionAcknowledgement? = null

    /**
     * The server named a successor. Build the second pipeline and align it.
     *
     * Arm-and-poll, never await: this is reached from a `Player.Listener`
     * callback's descendants and from the reporter's exchange hook, and both
     * are non-suspending. Everything that waits — readiness, runway, the
     * readiness bound — is read by [pollPreparedReplacement] on the tick that
     * already runs every second.
     */
    private fun onPrepareAction(action: ControlAction) {
        // The session fences this too, and this is the second lock on the same
        // door: an action that reached a released controller would read the
        // playhead off a released ExoPlayer and stand up a pipeline nothing is
        // left running to release.
        if (controlObservationIsClosed) return
        when (val offer = preparedLedger.offer(action)) {
            // The server replays a staging byte-identically until it settles,
            // so this is what most exchanges carrying a `prepare` are. Building
            // again would put a third pipeline on the device.
            is PreparationOffer.Same -> Unit
            is PreparationOffer.Refuse -> Unit
            is PreparationOffer.Start -> {
                offer.supersedes?.let(::publishAcknowledgement)
                // Releasing a superseded pipeline must not drop the boundary
                // this very offer is the preparation for; a boundary for some
                // other candidate is gone with the pipeline as before.
                val boundary = autoBoundaryAttempt?.takeIf {
                    it.candidateId == offer.action.effectiveSelection?.candidateId
                }
                releaseSuccessor()
                if (boundary != null) autoBoundaryAttempt = boundary
                startSuccessor(offer.action)
            }
        }
    }

    private fun startSuccessor(action: ControlAction) {
        val playlist = action.playlistUrl ?: return
        val originMs = action.mediaOriginMs ?: return
        preparedStartedAtMs = monotonicNowMs()
        val built = try {
            buildSuccessorPlayer(context, vm, plan.isAudioOnly)
        } catch (_: Exception) {
            // A device that cannot stand up a second pipeline at all is the
            // measured Google TV case. It is a `failed`, not a crash.
            publishAcknowledgement(preparedLedger.failed())
            // And the viewer still asked for a different rung. The ordinary
            // reopen is what this device has; it is not seamless, and it is
            // very much better than the tap doing nothing at all.
            fallBackAfterPreparedFailure()
            return
        }
        preparedPlayer = built.player
        if (monotonicNowMs() - preparedStartedAtMs >= PREPARED_OVERLAP_BOUND_MS) {
            abandonPreparedReplacement(failed = true)
            return
        }
        val openedAtMs = preparedStartedAtMs
        preparedReadinessDeadlineJob?.cancel()
        preparedReadinessDeadlineJob = scope.launch {
            delay((PREPARED_OVERLAP_BOUND_MS - (monotonicNowMs() - openedAtMs)).coerceAtLeast(1))
            if (preparedPlayer === built.player && preparedStartedAtMs == openedAtMs) {
                abandonPreparedReplacement(failed = true)
            }
        }
        try {
            preparedVideoSurfaces?.stage(built.player)
        } catch (_: Exception) {
            releaseSuccessor()
            publishAcknowledgement(preparedLedger.failed())
            fallBackAfterPreparedFailure()
            return
        }
        autoTransfersByPlayer[built.player] = built.autoTransfers
        val desired = autoDesiredCandidate
        val stagedSessionId = action.sessionId
        autoStagedObservation = if (autoPreparing && desired != null && desired.recipe_digest.size == 32 &&
            stagedSessionId != null && action.effectiveSelection?.candidateId == desired.id)
            AutoStagedObservation(built.player, built.autoTransfers, stagedSessionId, desired.id,
                desired.recipe_digest.toList(), preparedStartedAtMs,
                preparedStartedAtMs + minOf(15_000L, PREPARED_READINESS_BOUND_MS), Session.origin, Session.token) else null
        rendezvousJob?.cancel()
        rendezvousJob = null
        rendezvous = null
        rendezvousSeekObserved = false
        preparedOrigin = built.progressiveMediaOrigin
        val successorListener = object : Player.Listener {
            override fun onTimelineChanged(timeline: androidx.media3.common.Timeline, reason: Int) {
                autoStagedObservation?.takeIf { it.player === built.player }?.let(::captureAutoStagedVodPlaylist)
            }
            override fun onPlayerError(error: PlaybackException) {
                // Posted, not called. Abandoning releases this very player, and
                // re-entering `release()` from inside its own `ListenerSet`
                // dispatch is the kind of thing that works until it does not.
                // A tick's delay costs a viewer who is not watching this
                // pipeline nothing.
                //
                // And it names the pipeline that errored. A seek in the gap
                // releases this one and the next `prepare` builds another, and
                // an unqualified post would then report `failed` for a
                // preparation that never failed and kill it — burning the
                // session's one preparation slot on a stale event.
                val errored = built.player
                scope.launch {
                    if (preparedPlayer === errored) abandonPreparedReplacement(failed = true)
                }
            }

            override fun onPositionDiscontinuity(
                oldPosition: Player.PositionInfo,
                newPosition: Player.PositionInfo,
                reason: Int,
            ) {
                if (reason == Player.DISCONTINUITY_REASON_SEEK &&
                    preparedPlayer === built.player &&
                    rendezvous?.rendezvousFilmMs != null
                ) {
                    rendezvousSeekObserved = true
                }
            }
        }
        preparedListener = successorListener
        built.player.addListener(successorListener)
        // M3. The successor's own dropped-frame and underrun stream, from
        // before it is exposed to after it is the incumbent. It is never
        // removed, so the two seconds *after* the commit are on the same
        // series as the two before it.
        built.player.addAnalyticsListener(preparedSwitchAnalytics(built.player))
        built.player.setMediaItem(
            MediaItem.fromUri(Session.url(playlist)),
            successorAttachPositionMs(originMs, realPosition()),
        )
        built.player.prepare()
    }

    /**
     * Read the successor once per tick and move the ladder if it has earned it.
     *
     * The whole readiness path is a poll rather than an await for the reason
     * `docs/playback-control/PLAYBACK-CONTROL-STATUS.md:281-311` records: `onPlayerError` is a
     * non-suspending override, so this platform *arms* where Apple *awaits*,
     * and introducing a suspending wait inside a listener callback is the thing
     * that document says not to do.
     */
    private fun pollPreparedReplacement() {
        // The composition normally collects this within a frame. When the frame
        // clock is parked it never does, and a paused player is still a decoder.
        if (retiredPlayer != null &&
            monotonicNowMs() - retiredParkedAtMs > PREPARED_RETIRED_COLLECT_MS
        ) {
            collectRetiredPlayer()
        }
        awaitingCommitFrameSinceMs?.let {
            if (samplePreparedCommitFrameBudget() != false) {
                val restored = rollbackSwitchedReplacement()
                // The directed owner settles retention or recovery exactly
                // once; a deferred rollback must not repeat either outcome.
                val routed = failSwitchedReplacement()
                if (routed) preparedRollbackReopen = null
                if (!restored && !routed) {
                    restartAt(realPosition(), "prepared successor rendered no frame")
                }
                return
            }
        }
        val successor = preparedPlayer ?: return
        if (!preparedLedger.isLive) return
        // A boundary preparation whose owner moved on (backgrounded, another
        // selection, a different rung) is withdrawn, not failed.
        autoBoundaryAttempt?.let { boundary ->
            if (!preparedLedger.isSwitched && !autoBoundaryIsCurrent(boundary)) {
                abandonPreparedReplacement(failed = false)
                withdrawAutoBoundary(boundary)
                return
            }
        }
        if (autoPreparing && autoVoluntary) {
            val now = monotonicNowMs()
            val observation = autoStagedObservation
            val current = measuredCostCatalog().firstOrNull { it.id == autoActiveCandidateId }
            val pressure = current?.let { autoActiveProductionPressure(sessionStatus,
                sessionStatusAgeMs?.let { now - it }, now, sessionId, it.id,
                player.bufferedPosition - player.currentPosition) } ?: true
            val sample = latestAutoCompletedTransfer
            val link = sample?.takeIf { it.pipelineIdentity === autoTransfersByPlayer[player] &&
                sessionId != null && it.segmentId.contains("/$sessionId/") && it.statusCode == 200 &&
                it.receipt != null && it.etag != null && autoTransferOriginCurrent(it) }
                ?.let { autoCompletedTransferBps(it, now, 15_000L) }
            val cost = current?.let { autoDownsideCostBps(it, sample, sessionId, now) }
            val linkPressure = link != null && cost != null && link < cost * (if (current?.peak_bps == null) 1.0 else 1.3)
            if (link != null && cost != null && link < cost * 0.7 && sample != null)
                autoUpgradeEvidence.cliff(sample.completedAtMs, now)
            if (observation == null || !autoStagedObservationCurrent(observation, now) ||
                player.bufferedPosition - player.currentPosition < 10_000L || pressure || linkPressure ||
                (autoBoundaryAttempt?.let(::autoBoundaryIsCurrent) != true && !autoUpgradeEvidence.allowsUpgrade(now))) {
                abandonPreparedReplacement(failed = true)
                return
            }
        }
        if (monotonicNowMs() - preparedStartedAtMs >=
            minOf(PREPARED_READINESS_BOUND_MS, PREPARED_OVERLAP_BOUND_MS)) {
            abandonPreparedReplacement(failed = true)
            return
        }
        if (successor.playbackState != Player.STATE_READY) return
        // Tracks known is what separates "playable" from "prepared": a player
        // at STATE_READY with no published tracks has nothing to switch to.
        if (successor.currentTracks.groups.isEmpty()) return
        publishAcknowledgement(preparedLedger.metadataReady())
        val originMs = preparedLedger.action?.mediaOriginMs ?: return
        if (rendezvous != null) return
        // From here the rendezvous owns this preparation. This tick keeps the
        // readiness bound and the terminal failures; it is far too coarse for
        // anything measured against a 250 ms window.
        val hold = RendezvousHold()
        rendezvous = hold
        // The incumbent is *not* touched here, and that is the change. It keeps
        // playing all the way to the meeting point.
        parkSuccessor(hold.park(monotonicNowMs(), realPosition(), player.playbackParameters.speed.toDouble()), originMs, successor)
        beginRendezvous(hold, originMs)
    }

    /** Send the successor to the meeting point and leave it waiting there. */
    private fun parkSuccessor(
        park: RendezvousHold.Park,
        originMs: Long,
        successor: ExoPlayer,
    ) {
        rendezvousSeekObserved = false
        // Parked, not chasing. Everything else about this handoff follows from
        // this one assignment being `false`.
        successor.playWhenReady = park.playWhenReady
        preparedVideoSurfaces?.invalidate(successor)
        successor.seekTo(successorAttachPositionMs(originMs, park.rendezvousFilmMs))
    }

    /**
     * Wait for the successor to reach the meeting point, then for the incumbent
     * to arrive at it, then swap.
     *
     * One coroutine for both halves, on its own cadence, because both are
     * measured against the same 250 ms window the one-second tick cannot see.
     * The delay is recomputed from the incumbent's own rate on every pass, so a
     * viewer who pauses during the hold simply postpones the swap: the
     * rendezvous does not move, the successor stays parked on it, and the wait
     * resumes from wherever the playhead is when play does.
     */
    private fun beginRendezvous(hold: RendezvousHold, originMs: Long) {
        rendezvousJob?.cancel()
        rendezvousJob = scope.launch {
            while (true) {
                if (rendezvous !== hold) return@launch
                val successor = preparedPlayer ?: return@launch
                if (!preparedLedger.isLive) return@launch
                if (monotonicNowMs() - preparedStartedAtMs >= PREPARED_OVERLAP_BOUND_MS) {
                    abandonPreparedReplacement(failed = true)
                    return@launch
                }
                if (!hold.isReady) {
                    delay(RENDEZVOUS_READY_POLL_MS)
                    val target = hold.rendezvousFilmMs ?: return@launch
                    // Two separate requirements, both measured at the
                    // rendezvous rather than at the moving playhead: the seek
                    // has landed there, and there is contiguous runway through
                    // it. A pipeline that is merely positioned correctly has
                    // nothing behind it to carry the viewer forward, and one
                    // that is buffered but still seeking is not where the swap
                    // needs it.
                    if (!rendezvousSeekObserved) continue
                    val bufferedThrough =
                        successorFilmPositionMs(originMs, successor.bufferedPosition)
                    if (!successorIsBuffered(bufferedThrough, target)) continue
                    // Buffering and seek completion can precede the decoder's
                    // first-frame callback. Keep the rendezvous alive until
                    // this exact parked output has proved its picture.
                    if (preparedVideoSurfaces != null &&
                        !preparedVideoSurfaces.ready(successor, (target - originMs).coerceAtLeast(0))) continue
                    hold.ready(monotonicNowMs())
                    publishAcknowledgement(preparedLedger.bufferReady(bufferedThrough))
                    continue
                }
                delay(hold.delayMs(realPosition(), player.playbackParameters.speed.toDouble()))
                if (rendezvous !== hold || preparedPlayer !== successor) return@launch
                if (!preparedLedger.isLive) return@launch
                val step = hold.fire(
                    nowMs = monotonicNowMs(),
                    incumbentFilmMs = realPosition(),
                    successorFilmMs = successorFilmPositionMs(originMs, successor.currentPosition),
                    successorReady = hold.isReady,
                    speed = player.playbackParameters.speed.toDouble(),
                    alignmentWindowMs = preparedVideoSurfaces?.alignmentWindowMs(successor)
                        ?: PREPARED_ALIGNMENT_SLACK_MS.toDouble(),
                )
                when (step) {
                    is RendezvousHold.Step.Commit -> {
                        // `commitPreparedReplacement` gives the successor the
                        // incumbent's own transport intent, which is what
                        // starts it: it has been parked with `playWhenReady`
                        // false since the moment it was seeked here.
                        if (commitPreparedReplacement(step.filmMs)) return@launch
                        // A fresh final reading can miss a frame boundary
                        // after fire(). Retry the bounded hold; do not leave
                        // the preparation without a rendezvous coroutine.
                        delay(8)
                    }
                    // Short of the meeting point — still on its way, or paused.
                    // The loop recomputes the delay from the new reading.
                    is RendezvousHold.Step.Wait -> Unit
                    // Past it: a seek that outran its lead, a forward seek, or a
                    // rate above one. Pick a later point, park again, and wait
                    // for it to land there.
                    is RendezvousHold.Step.Repark -> parkSuccessor(step.park, originMs, successor)
                    is RendezvousHold.Step.Abandon -> {
                        // Bounded. Two misses is enough evidence that this
                        // device is not going to meet a 250 ms window, and the
                        // viewer's rung is owed the ordinary reopen instead.
                        surfaceOwner.logOnly(
                            mediaMutationEpoch,
                            "prepared successor missed the rendezvous (${step.reason})",
                        )
                        abandonPreparedReplacement(failed = true)
                        return@launch
                    }
                }
            }
        }
    }

    /**
     * The successor takes the surface and the volume; the predecessor stays
     * paused and recoverable until the successor proves a frame.
     *
     * The client decides when to switch — there is no `commit_replacement`
     * action and the server's answer on the settling exchange is `none`. What
     * the server does with the commit is move its pointer from the predecessor
     * incarnation to the staged one, so the predecessor's session is retired by
     * that CAS rather than by an `endHlsSession` from here.
     */
    private fun commitPreparedReplacement(commitFilmMs: Long): Boolean {
        val successor = preparedPlayer ?: return false
        val action = preparedLedger.action ?: return false
        val originMs = action.mediaOriginMs ?: return false
        if (monotonicNowMs() - preparedStartedAtMs >= PREPARED_OVERLAP_BOUND_MS) {
            abandonPreparedReplacement(failed = true)
            return false
        }
        val stagedFilmLocalVod = autoStagedObservation?.let {
            autoStagedObservationCurrent(it, monotonicNowMs()) && it.immutableVod && originMs == 0L
        } == true
        if (autoPreparing && !autoTrialMayCommit(autoDesiredCandidate?.id, action.effectiveSelection?.candidateId,
                autoPreparedTargetRevision, autoPresentationTarget?.revision,
                autoPreparedMutationEpoch, mediaMutationEpoch,
                playbackIntent.desiredQuality == PlaybackQuality.Auto && tv.plurx.app.data.Session.autoAbr,
                presentationForeground && player.isPlaying,
                playbackIntent.pendingSeek != null)) {
            abandonPreparedReplacement(failed = true)
            return false
        }
        val desired = autoDesiredCandidate
        if (autoPreparing && autoVoluntary && desired != null) {
            if (desired.route == "encode" && !desired.complete_cache &&
                !autoStagedEncodeProof(autoStagedStatus, autoStagedStatusObservedMs, monotonicNowMs(),
                    action.sessionId ?: return false, desired.id)) return false
            val now = monotonicNowMs()
            val observation = autoStagedObservation ?: return false
            if (!autoStagedObservationCurrent(observation, now)) return false
            val peak = tv.plurx.app.data.measuredCandidatePeak(desired, autoMeasuredOutputs)
            val unknown = autoCatalog.firstOrNull { it.id == desired.id && it.recipe_digest == desired.recipe_digest }
                ?.let { autoUnknownOriginalTrial(it, autoMeasuredOutputs) } == true
            if (peak == null && !unknown) return false
            autoTransfersByPlayer[successor]?.recent().orEmpty().forEach { sample ->
                val receipt = sample.receipt
                val duration = sample.bodyDurationMs
                val uri = android.net.Uri.parse(sample.segmentId)
                if (preparedPlayer === successor && preparedLedger.action === action &&
                    sample.pipelineIdentity === autoTransfersByPlayer[successor] && receipt != null &&
                    Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").matches(receipt) &&
                    uri.pathSegments.contains(action.sessionId) && !sample.etag.isNullOrEmpty() &&
                    sample.statusCode == 200 && autoTransferOriginCurrent(sample) &&
                    autoCompletedTransferBps(sample, now, 15_000L) != null && duration != null &&
                    !autoLinkClaims.containsKey(receipt) && autoLinkClaims.size < 32) {
                    postPlaybackClientLog(scope, PlaybackClientLog(level = "warn", event = "candidate_link_sample",
                        message = "Completed staged candidate body", ua = "Android Media3", sessionId = action.sessionId,
                        linkSample = CandidateLinkSample(receipt, uri.lastPathSegment.orEmpty(), sample.etag!!,
                            sample.bodyBytes, duration, sample.ageMs(now), true, false, false, "link", false,
                            sample.observedMediaDurationMs, false, false, 0)))
                    autoLinkClaims[receipt] = Triple(sample.completedAtMs, false, desired.id)
                }
            }
            val samples = observation.meter.recent().filter { autoTransferOriginCurrent(it) }
            val margin = if (unknown) observation.immutableVod && autoStagedEmpiricalMargin(samples, observation.meter,
                observation.sessionId, now, observation.deadlineMs) else samples.any { sample ->
                sample.pipelineIdentity === observation.meter &&
                    sample.segmentId.contains("/${observation.sessionId}/") && sample.receipt != null && sample.etag != null &&
                    sample.statusCode == 200 && autoCompletedTransferBps(sample, now, 15_000L)?.let { it >= (peak ?: return false) * 1.8 } == true
            }
            if (!margin) return false
        }
        val successorFilmMs = successorFilmPositionMs(originMs, successor.currentPosition)
        val bufferedThrough = successorFilmPositionMs(originMs, successor.bufferedPosition)
        if (kotlin.math.abs(successorFilmMs - commitFilmMs) > PREPARED_ALIGNMENT_SLACK_MS ||
            !successorIsBuffered(bufferedThrough, commitFilmMs)
        ) return false
        // A warm output must have rendered before changing visibility. No
        // prepared output or callback from an old Surface can satisfy this.
        if (preparedVideoSurfaces != null && !preparedVideoSurfaces.ready(successor, (realPosition() - originMs).coerceAtLeast(0))) return false
        // The ledger enters its unabortable state before anything moves. From
        // here the viewer is looking at this pipeline, so a Back press or a
        // seek in the seconds before its first frame must settle the commit
        // rather than publish an `aborted` for the staging the server is about
        // to move its pointer to.
        if (!preparedLedger.switched()) return false
        // Armed here rather than at the end of this function. `switched()` is
        // what makes an abandon settle instead of abort, and the settle is
        // gated on this clock — so any window where one is set and the other is
        // not is a window where a preparation can be left permanently
        // unsettleable.
        preparedReadinessDeadlineJob?.cancel()
        preparedReadinessDeadlineJob = null
        awaitingCommitFrameSinceMs = monotonicNowMs()
        preparedCommitFrameBudget = PreparedActiveWallBudget(
            PREPARED_COMMIT_FRAME_BOUND_MS,
            monotonicNowMs(),
            playbackIntent.playbackRequested && presentationForeground,
            overlapBoundMs = PREPARED_OVERLAP_BOUND_MS,
            overlapStartedAtMs = preparedStartedAtMs,
        )
        // M3. Three assignments, on the line the swap is decided at. Nothing
        // is awaited, nothing is read back, and the picture is untouched.
        preparedSwitch.noteCommit(
            atMs = monotonicNowMs(),
            tappedAtMs = directedChangeTappedAtMs,
            predecessor = System.identityHashCode(player),
            successor = System.identityHashCode(successor),
        )
        val previous = player
        val previousVolume = previous.volume
        // An ON_STOP owner pause survives the prepared item becoming active;
        // intent remains Play and foreground entry can resume the new player.
        val previousPlayWhenReady = effectivePlayWhenReady()
        val previousPlaybackParameters = previous.playbackParameters
        val predecessor = PreparedPredecessor(
            player = previous,
            filmPositionMs = commitFilmMs,
            recipe = recipeOwnership.attached,
            transport = recipeOwnership.attachedTransport,
            modeOverride = attachedModeOverride,
            requiresHlsOverride = attachedRequiresHlsOverride,
            progressiveMediaOrigin = progressiveMediaOrigin,
            baseMs = baseMs,
            sessionIsVod = sessionIsVod,
            sessionId = sessionId,
            activeMediaPath = activeMediaPath,
            deliveredRange = deliveredRange,
            deliveredDolbyVisionProfile = deliveredDolbyVisionProfile,
            encoder = encoder,
            sessionStatus = sessionStatus,
            sessionStatusObservedAtMs = sessionStatusObservedAtMs,
            establishedPlayback = establishedPlayback,
        )

        handOverAudioFocus(previous.asAudioFocusOwner(), successor.asAudioFocusOwner())
        successor.volume = previousVolume
        successor.playbackParameters = previousPlaybackParameters
        successor.playWhenReady = previousPlayWhenReady
        preparedOrigin?.let { progressiveMediaOrigin = it }

        preparedListener?.let { successor.removeListener(it) }
        preparedListener = null
        preparedPlayer = null
        preparedOrigin = null
        rendezvousJob?.cancel()
        rendezvousJob = null
        rendezvous = null
        rendezvousSeekObserved = false

        previous.removeListener(this.listener)
        externalListeners.forEach { previous.removeListener(it) }
        successor.addListener(this.listener)
        externalListeners.forEach { successor.addListener(it) }

        player = successor
        // A different `ExoPlayer` with its own item is on the screen now, so
        // it gets its own single `BEHIND_LIVE_WINDOW` recovery. This path never
        // passed through the ordinary session attachment, so record both the
        // desired recipe and the HLS transport this prepared player actually
        // owns. The server-selected codec distinguishes copy from transcode.
        val preparedAdoption = preparedTransportAdoption(action.effectiveSelection?.codec)
        attachedModeOverride = preparedAdoption.modeOverride
        attachedRequiresHlsOverride = preparedAdoption.requiresHlsOverride
        attachRecipe(currentRecipe(), preparedAdoption.transport)

        // The successor is its own session on its own timeline. Everything the
        // controller derives from "which session am I playing" moves with it,
        // in one place, so no two answers about the same stream can drift.
        activeMediaPath = action.playlistUrl?.let(::relativeMediaPath)
        // ENDLIST is a delivery proof, never permission to erase a nonzero
        // explicit media origin or retain the incumbent's timeline regime.
        sessionIsVod = stagedFilmLocalVod
        baseMs = sessionPlaybackTimeline(
            mediaOriginMs = originMs,
            isVod = sessionIsVod,
            requestedStartMs = 0L,
        ).baseMs
        action.sessionId?.let { successorSession ->
            sessionId = successorSession
            startStatusPolling(successorSession)
        }
        rebindMediaSessionTransport()
        action.effectiveSelection?.let { stallReopenBudget.seed(it.height.toInt()) }
        // The badges and the info panel describe the stream on the screen, and
        // both halves of the grade move together or neither does.
        adoptedGrade(
            DeliveredGrade(deliveredRange, deliveredDolbyVisionProfile),
            action.effectiveSelection,
        ).let {
            deliveredRange = it.range
            deliveredDolbyVisionProfile = it.dolbyVisionProfile
        }
        // The successor is a different encoder on a different session. The
        // status poll started above refills both within a couple of seconds;
        // until it does, no answer is better than the predecessor's.
        encoder = null
        sessionStatus = null
        sessionStatusObservedAtMs = null
        establishedPlayback = false
        armTrackSelections()
        applyTextSelection()
        applyAudioSelection()

        // Not released here. `player` is snapshot state, so the `PlayerView`
        // re-points at the successor on the next composition pass — releasing
        // the predecessor synchronously would leave the view holding a released
        // player for a frame, and would tear the surface down before the
        // successor could take it. The surface owner collects it the moment it
        // has re-pointed, which is the only deterministic answer: a timer would
        // be guessing about a frame clock that is parked whenever the window is
        // not visible. It is paused rather than stopped, so the predecessor's
        // last frame stays on screen through the swap instead of a shutter.
        // Collect before parking. Two commits between two composition passes
        // would otherwise drop the first predecessor on the floor — never
        // released, never referenced again. An uncollected one here means
        // `player` has already moved twice, so the view is demonstrably past
        // it.
        collectRetiredPlayer()
        previous.playWhenReady = false
        retiredParkedAtMs = monotonicNowMs()
        retiredPlayer = previous
        preparedPredecessor = predecessor
        preparedVideoSurfaces?.expose(successor) {
            if (player === successor && awaitingCommitFrameSinceMs != null && presentationForeground &&
                !controlObservationIsClosed) settleCommitOnFirstFrame(System.currentTimeMillis(), warmPresented = true)
        }
        // Warm outputs settle only after their visibility transaction is
        // actually presented. A hidden render is readiness, never delivery.
        // The ordinary older-platform path still waits for the successor's
        // onRenderedFirstFrame after PlayerView moves its surface. Both paths
        // keep the finite first-frame and physical-overlap budgets.
        return true
    }

    /**
     * The successor rendered its first frame after taking the surface, so the
     * commit can name the moment the viewer actually saw it.
     */
    private fun settleCommitOnFirstFrame(firstFrameUnixMs: Long, warmPresented: Boolean = false) {
        if (awaitingCommitFrameSinceMs == null) return
        if (preparedVideoSurfaces != null && !warmPresented) return
        awaitingCommitFrameSinceMs = null
        preparedCommitFrameBudget = null
        preparedSwitch.noteFirstFrame(monotonicNowMs())
        autoDesiredCandidate?.let { requested ->
            if (preparedLedger.action?.effectiveSelection?.candidateId == requested.id) {
                autoActiveCandidateId = requested.id
                autoLastSwitchMs = monotonicNowMs()
                if (autoVoluntary) autoSwitchTimes.add(monotonicNowMs())
            }
            autoPreparing = false
            autoUpgradeSinceMs = null
        }
        // A boundary preparation ends with its own first frame, exactly like
        // the mid-play upgrade it now is.
        autoBoundaryAttempt = null
        publishAcknowledgement(preparedLedger.committed(firstFrameUnixMs))
        // The viewer is looking at the rung they asked for. The directed change
        // is honoured and owes nothing — least of all a reopen.
        directedChange?.let { change ->
            change.committed()
            logQualitySwitch("prepared", change.quality)
        }
        // A rendered successor is now the last known-good picture, so the
        // predecessor is no longer rollback authority and may release.
        preparedPredecessor = null
        collectRetiredPlayer()
    }

    /**
     * Put the last proven player back before publishing failure.
     *
     * A healthy predecessor remains the playback authority. If it also needs
     * recovery, defer its reopen until [collectRetiredPlayer] observes the
     * surface move; preserve the latest viewer destination through that wait.
     */
    private fun rollbackSwitchedReplacement(): Boolean {
        val predecessor = preparedPredecessor ?: return false
        if (retiredPlayer !== predecessor.player) return false
        val failedSuccessor = player
        val reopenAt = playbackIntent.positionForPlaybackIntent(predecessor.baseMs + predecessor.player.currentPosition)

        failedSuccessor.removeListener(this.listener)
        externalListeners.forEach { failedSuccessor.removeListener(it) }
        predecessor.player.addListener(this.listener)
        externalListeners.forEach { predecessor.player.addListener(it) }

        progressiveMediaOrigin = predecessor.progressiveMediaOrigin
        attachedModeOverride = predecessor.modeOverride
        attachedRequiresHlsOverride = predecessor.requiresHlsOverride
        predecessor.recipe?.let { recipe ->
            attachRecipe(recipe, predecessor.transport ?: recipe.recipe.desiredTransport)
        }
        baseMs = predecessor.baseMs
        sessionIsVod = predecessor.sessionIsVod
        sessionId = predecessor.sessionId
        activeMediaPath = predecessor.activeMediaPath
        deliveredRange = predecessor.deliveredRange
        deliveredDolbyVisionProfile = predecessor.deliveredDolbyVisionProfile
        encoder = predecessor.encoder
        establishedPlayback = predecessor.establishedPlayback
        predecessor.sessionId?.let(::startStatusPolling) ?: clearStatusPolling()
        sessionStatus = predecessor.sessionStatus
        sessionStatusObservedAtMs = predecessor.sessionStatusObservedAtMs
        // Viewer controls keep their latest values across media rollback.
        // ON_STOP still suppresses output without replacing standing Play intent.
        predecessor.player.volume = failedSuccessor.volume
        predecessor.player.playbackParameters = failedSuccessor.playbackParameters
        // The failed successor stays parked behind the surface overlap until it
        // is collected. Losing focus alone left it playing and audible over the
        // restored player, so it is paused and muted (after its viewer volume
        // was read above) before focus moves back.
        failedSuccessor.playbackSilencing().silence()
        handOverAudioFocus(failedSuccessor.asAudioFocusOwner(), predecessor.player.asAudioFocusOwner())
        predecessor.player.playWhenReady = effectivePlayWhenReady()

        preparedPredecessor = null
        preparedRollbackReopen = if (predecessor.establishedPlayback && predecessor.player.playerError == null &&
            predecessor.player.playbackState == Player.STATE_READY) null
        else reopenAt to "prepared successor rendered no frame"
        retiredPlayer = failedSuccessor
        retiredParkedAtMs = monotonicNowMs()
        player = predecessor.player
        rebindMediaSessionTransport()
        preparedVideoSurfaces?.expose(predecessor.player) { collectRetiredPlayer() }
        return true
    }

    /**
     * Settle a switched-but-black successor without inventing a frame.
     *
     * Returns whether the viewer's directed change took its one ordinary reopen
     * here, so the caller does not start a second one of its own.
     */
    private fun failSwitchedReplacement(): Boolean {
        if (awaitingCommitFrameSinceMs == null) return false
        awaitingCommitFrameSinceMs = null
        preparedCommitFrameBudget = null
        publishAcknowledgement(preparedLedger.failedAfterSwitch())
        return fallBackAfterPreparedFailure()
    }

    /**
     * Give up on the successor and fall back to the in-place path.
     *
     * The interruption that follows is Android's ordinary reopen, measured at
     * 353–766 ms on the Google TV. It works, and it is not seamless; nothing
     * here should ever describe it as such.
     */
    /**
     * Every path that replaces the stream calls this, and the list is the rule.
     *
     * There are five: [seekTo] (a VOD seek never reopens, so it never reaches
     * `restartAt`), [restartAt] (quality, audio, subtitle, fallback),
     * [openSession], [onStall]'s reopen, [retryMediaOnNextNode], and [release].
     * A sixth path that replaces the stream and does not appear here leaves the
     * successor priming against a session that is gone, and the next tick
     * commits it — which is what `onStall` did until a review traced it.
     *
     * The list is a comment rather than a test because reaching any of those
     * from the JVM unit lane needs a `Context` and a real ExoPlayer. Both
     * decisions this used to make inline — which grade to adopt, and what a
     * settling snapshot looks like — now live in `PreparedReplacement.kt`
     * where they are tested; this is the residue that genuinely needs a player.
     */
    private fun abandonPreparedReplacement(failed: Boolean) {
        // §3.3 row 18: a successor that failed is dropped and the incumbent is
        // untouched, so nothing is drawn and this event is the only trace the
        // viewer's session keeps of it. A deliberate abandonment — a seek, a
        // quality change, a release — is not a failure and says nothing.
        if (failed) {
            surfaceOwner.logOnly(
                mediaMutationEpoch,
                "prepared successor abandoned after it failed",
            )
        }
        // A switched preparation cannot be aborted, but neither may an exit
        // fabricate a rendered frame. Settle it as failed; the caller's normal
        // reopen/end path replaces the black successor immediately afterward.
        if (preparedLedger.isSwitched) {
            failSwitchedReplacement()
            return
        }
        val owed = if (failed) preparedLedger.failed() else preparedLedger.aborted()
        releaseSuccessor()
        publishAcknowledgement(owed)
        // A failure owes the viewer their rung through the ordinary path; a
        // deliberate abandonment means something newer already owns the stream,
        // and routing this one would drag the viewer back to it.
        if (failed) fallBackAfterPreparedFailure() else directedChange?.superseded()
    }

    /**
     * Every exit releases the successor: disposal, session end, a seek, another
     * quality change, backgrounding. A prepared player that outlives its reason
     * is a second decoder the viewer is paying for and cannot see.
     */
    private fun releaseSuccessor() {
        preparedReadinessDeadlineJob?.cancel()
        preparedReadinessDeadlineJob = null
        // The preparation a boundary asked for is gone with its pipeline.
        if (!preparedLedger.isSwitched) autoBoundaryAttempt = null
        autoStagedPlaylistJob?.cancel()
        autoStagedPlaylistJob = null
        autoStagedObservation = null
        autoStagedStatus = null
        autoStagedStatusObservedMs = null
        val successor = preparedPlayer ?: return
        preparedListener?.let { successor.removeListener(it) }
        preparedListener = null
        preparedPlayer = null
        preparedOrigin = null
        rendezvousJob?.cancel()
        rendezvousJob = null
        rendezvous = null
        rendezvousSeekObserved = false
        preparedVideoSurfaces?.remove(successor)
        successor.release()
        // Nothing to restore. The incumbent was never stopped: the rendezvous
        // is the successor waiting for it, not the other way round, so a
        // preparation that dies at any point leaves the picture exactly as the
        // viewer has been watching it. The `playWhenReady` this used to put
        // back is the visible pause the handoff exists to remove.
    }

    /**
     * Put an acknowledgement on the next exchange, now rather than at the next
     * cadence.
     *
     * Coalescing is right for a position update and wrong for this: the pump
     * sleeps for `next_exchange_ms`, and a commit that arrives after the
     * server's deadline reaped the staging comes back
     * `acknowledgement_rejected`.
     */
    private fun publishAcknowledgement(value: ActionAcknowledgement?) {
        val acknowledgement = value ?: return
        pendingAcknowledgement = acknowledgement
        playbackControl.reportEvidence()
    }

    /**
     * The exchange that carried an acknowledgement came back accepted.
     *
     * A `200` is not proof the *server* accepted the settlement — an
     * acknowledgement whose `action_id` does not match the bound staging is
     * silently ignored — but it is proof this client does not have to send this
     * one again, which is the only thing this slot decides.
     */
    private fun acknowledgementDelivered(value: ActionAcknowledgement) {
        if (pendingAcknowledgement == value) pendingAcknowledgement = null
    }
}

/**
 * How long a recovery owner waits for the server's verdict before deciding for
 * itself. Ruling D3, and the same pair of numbers the web and Apple clients
 * carry: the fallback is the branch the whole fleet takes, so this is added to
 * every real stall on every device.
 */
internal const val CONTROL_ASK_MS = 1_500L
internal const val CONTROL_ASK_CAP_MS = 3_000L
internal const val SEEK_COALESCE_MS = 100L

/**
 * What M5's create retry says while it runs, and what the owner says when its
 * ladder or its absolute deadline is spent. The first is a fallback: the
 * server's own sentence is preferred wherever it sent one.
 */
internal const val STILL_BUILDING_SENTENCE = "The server is still building this stream."
internal const val CREATE_RETRY_EXHAUSTED_SENTENCE =
    "The server is still building this stream and did not finish in a minute."

/**
 * How far apart two `realPosition()` reads must be for "the film clock did not
 * move" to be evidence of anything.
 *
 * Not a threshold and not a detector: both watchdog loops can come round a
 * millisecond apart while a target deadline is pending, and a millisecond of
 * film is below the player's own clock resolution — answering "not presenting"
 * there would be inventing the answer, so a pass that arrives sooner takes no
 * sample at all. One second is the cadence the stall sampler and the screen's
 * position readout already run at.
 */
internal const val SURFACE_EVIDENCE_INTERVAL_MS = 1_000L

/**
 * Whether this wait has crossed from supply into native presentation.
 *
 * Ten seconds is the shared client/server starvation ceiling. A smaller
 * runway can still be a fetch boundary; more than this is complete contiguous
 * media that a producer hold cannot make Media3 present.
 */
internal fun loadedWaitNeedsNativeReevaluation(
    bufferedPositionMs: Long,
    currentPositionMs: Long,
): Boolean = bufferedPositionMs - currentPositionMs > 10_000L

/** A timer-only presentation wait has no demonstrated decoder or network cause. */
internal fun presentationStallEvidence(): ClientObservation =
    ClientObservation(decoderState = DecoderState.UNKNOWN)

/**
 * The verdict that ends an unchanged retry, or null to take it anyway.
 *
 * Scoped to the one rung that retries the source *unchanged*, because that is
 * the exact scope of the server's `is_permanent` — see the call site. It is a
 * function rather than an inline `if` so that the rule it encodes is pinned by
 * a test: a transport failure never borrows the server's words.
 *
 * [verdict] is `terminalVerdict`, which deliberately outlives the session that
 * earned it — that is what lets a verdict explain a failure that arrives after
 * a reopen, and it is also exactly why a dropped link must not inherit it. A
 * transport failure is a different cause with a different answer, and the
 * client's own sentence is the honest one for it.
 */
internal fun ladderVerdict(errorCode: Int, verdict: ControlAction?): ControlAction? = when {
    verdict == null -> null
    verdict.type != "terminal" -> null
    isTransportPlaybackError(errorCode) -> null
    else -> verdict
}

/**
 * Which class of failure Media3 reported, in the protocol's vocabulary.
 *
 * The classes are not decoration. A decoder error says this device cannot play
 * this recipe and a different rung might; a network error says nothing about
 * the recipe at all; a manifest or source error is about what the server
 * produced. Sending `unknown` for all of them would hand the arbiter one word
 * where it has to choose between three different answers.
 */
internal fun controlErrorCode(errorCode: Int): ClientErrorCode = when (errorCode) {
    // Media3's 3xxx family is *parsing*, and it splits: the container codes
    // are about the media, the manifest codes are about what the server
    // produced. This client's own compatibility ladder already treats 3001
    // and 3003 as media failures (`isCompatibilityPlaybackError`), so
    // reporting them as `manifest` would tell the arbiter the playlist was
    // bad at the moment the client is about to re-encode the file.
    3001, 3003 -> ClientErrorCode.MEDIA
    1003 -> ClientErrorCode.NETWORK // ERROR_CODE_TIMEOUT
    in 2000..2999 -> ClientErrorCode.NETWORK
    in 3000..3999 -> ClientErrorCode.MANIFEST
    // 4xxx is decoder/renderer init and decode; 5xxx is the AudioTrack
    // renderer, which is the same class of answer: this device could not
    // render this recipe.
    in 4000..5999 -> ClientErrorCode.DECODER
    in 6000..6999 -> ClientErrorCode.DRM
    else -> ClientErrorCode.UNKNOWN
}

/**
 * A ten-foot device, by the same signal the launcher uses. `UI_MODE_TYPE_
 * TELEVISION` is what `currentFormFactor()` reads in Compose; this is the
 * non-composable path for the player's construction.
 */
internal fun isTelevision(context: Context): Boolean =
    context.resources.configuration.uiMode and Configuration.UI_MODE_TYPE_MASK ==
        Configuration.UI_MODE_TYPE_TELEVISION

/** Minimal view of [Plan] so the controller doesn't depend on the screen file. */
interface PlanLike {
    val qualityCandidates: List<tv.plurx.app.data.QualityCandidate> get() = emptyList()
    val qualityCandidateId: String? get() = null
    val displayAwareAutoProtocol: String? get() = null
    val title: String
    val isAudioOnly: Boolean get() = false
    val fileId: Long
    val playUrl: String
    val mode: String // "direct" | "remux" | "transcode"
    /** `delivery.requires_hls`: this remux needs the copy-HLS producer. */
    val requiresHls: Boolean get() = false
    val durationMs: Long
    val videoCodec: String?
    /** Active media output captured by the route probe that produced this decision. */
    val audioOutputRoute: AudioOutputRoute? get() = null
    val requestedQuality: PlaybackQuality
    val audio: List<AudioTrack>
    val subtitles: List<SubTrack>

    /**
     * `DecisionResponse.source.height`. The height a session must send back
     * whenever its own height is a promise rather than a rung — a burn, or
     * Quality = Original — so a 4K burn stays 2160-tall instead of restarting
     * at the server's Auto rung (§3.2).
     */
    val sourceHeight: Int?

    /** Source cadence from `/decision`, available before Media3 prepares. */
    val sourceFrameRate: Double? get() = null

    /** `delivery.aac`: a copy session must re-encode the audio. */
    val aac: Boolean

    /** `delivery.preserve_dolby_vision`: the copy keeps the DV layer. */
    val preserveDolbyVision: Boolean

    /** `decision.delivered_dynamic_range`: the badge's starting truth. */
    val deliveredDynamicRange: String?

    /**
     * `decision.delivered_dolby_vision_profile`: which Dolby Vision profile
     * that grade is, when it is Dolby Vision at all. Null means "no answer" —
     * see the field's own doc on [tv.plurx.app.data.Decision].
     */
    val deliveredDolbyVisionProfile: Int?
}

internal fun autoRecoveryCandidate(candidates: List<tv.plurx.app.data.QualityCandidate>,
    current: tv.plurx.app.data.QualityCandidate, rejected: Set<String>, linkCeiling: Double? = null,
    decoderRecovery: Boolean = false): tv.plurx.app.data.QualityCandidate? =
    candidates.filter { it.hasValidIdentity && it.decoder_compatible && it.id !in rejected &&
        (decoderRecovery || it.route != "encode" || it.grade == current.grade) &&
        it.width.toLong() * it.height < current.width.toLong() * current.height &&
        (linkCeiling == null || it.peak_bps?.let { peak -> peak > 0 && peak <= linkCeiling } == true)
    }.maxByOrNull { it.width.toLong() * it.height }

internal fun autoPreferredDisplayCandidate(candidates: List<tv.plurx.app.data.QualityCandidate>,
    neededWidth: Double, neededHeight: Double): tv.plurx.app.data.QualityCandidate? =
    candidates.firstOrNull { it.route != "encode" } ?: candidates.filter {
        autoDisplayFits(it.width, it.height, neededWidth, neededHeight)
    }.minByOrNull { it.width.toLong() * it.height }

internal fun autoDisplayFits(width: Int, height: Int, neededWidth: Double, neededHeight: Double): Boolean =
    width > 0 && height > 0 && width.toDouble() * 11 >= neededWidth * 10 &&
        height.toDouble() * 11 >= neededHeight * 10

internal fun autoDownsideCostBps(candidate: tv.plurx.app.data.QualityCandidate,
    sample: AutoCompletedTransfer?, sessionId: String?, nowMs: Long): Double? {
    candidate.peak_bps?.takeIf { it > 0 }?.let { return it.toDouble() }
    if (candidate.route == "encode" || sample == null || sessionId == null ||
        !sample.segmentId.contains("/$sessionId/") || autoCompletedTransferBps(sample, nowMs) == null) return null
    val measured = sample.mediaDurationMs?.takeIf { it > 0 }?.let { sample.bodyBytes.toDouble() * 8_000 / it }
    return listOfNotNull(candidate.average_bps?.takeIf { it > 0 }?.toDouble(), measured).maxOrNull()
}

/** A source-body sample confirmed by Media3's completed media load event. */
internal data class AutoCompletedTransfer(
    val bodyBytes: Long,
    val bodyDurationMs: Long?,
    val completedAtMs: Long,
    val origin: String?,
    val networkLoad: Boolean,
    val fromLocalCache: Boolean?,
    val producerPaced: Boolean?,
    val segmentId: String = "",
    val mediaDurationMs: Long? = null,
    val receipt: String? = null,
    val etag: String? = null,
    val statusCode: Int? = null,
    val pipelineIdentity: Any? = null,
    val observedMediaDurationMs: Long? = null,
    val mediaStartTimeMs: Long? = null,
    val mediaEndTimeMs: Long? = null,
    val fullObject: Boolean = false,
) {
    fun ageMs(nowMs: Long): Long = if (nowMs >= completedAtMs) nowMs - completedAtMs else Long.MAX_VALUE
}

/** Empirical stage-only margin; never a full-output peak qualification. */
/** Strict bounded immutable media-playlist type proof; no guessed timeline. */
internal fun autoImmutableVodPlaylist(bytes: ByteArray): Boolean {
    if (bytes.isEmpty() || bytes.size > 1_048_576) return false
    val text = runCatching { java.nio.charset.StandardCharsets.UTF_8.newDecoder()
        .onMalformedInput(java.nio.charset.CodingErrorAction.REPORT)
        .onUnmappableCharacter(java.nio.charset.CodingErrorAction.REPORT)
        .decode(java.nio.ByteBuffer.wrap(bytes)).toString() }.getOrNull() ?: return false
    val lines = text.lineSequence().filter { it.isNotEmpty() }.toList()
    if (lines.firstOrNull() != "#EXTM3U" || lines.lastOrNull() != "#EXT-X-ENDLIST" ||
        "#EXT-X-PLAYLIST-TYPE:VOD" !in lines) return false
    var pending = false
    val names = mutableSetOf<String>()
    for (line in lines.drop(1)) {
        if (line.startsWith("#EXTINF:")) {
            val duration = line.removePrefix("#EXTINF:").removeSuffix(",").toDoubleOrNull()
            if (pending || !line.endsWith(",") || duration == null || !duration.isFinite() || duration <= 0 || duration > 120)
                return false
            pending = true
        } else if (line.startsWith("#")) {
            if (pending || !(line == "#EXT-X-ENDLIST" || line == "#EXT-X-PLAYLIST-TYPE:VOD" ||
                    line == "#EXT-X-MEDIA-SEQUENCE:0" || line == "#EXT-X-MAP:URI=\"init.mp4\"" ||
                    line.startsWith("#EXT-X-VERSION:") || line.startsWith("#EXT-X-TARGETDURATION:"))) return false
        } else {
            val index = line.removePrefix("seg").removeSuffix(".m4s").toUIntOrNull()
            if (!pending || index == null || line != "seg${index.toString().padStart(5, '0')}.m4s" ||
                names.size >= 8192 || !names.add(line)) return false
            pending = false
        }
    }
    return !pending && names.isNotEmpty()
}

internal fun autoStagedEmpiricalMargin(samples: List<AutoCompletedTransfer>, pipeline: Any,
    sessionId: String, nowMs: Long, deadlineMs: Long): Boolean {
    if (nowMs < 0 || nowMs >= deadlineMs) return false
    val names = mutableSetOf<String>()
    val receipts = mutableSetOf<String>()
    val etags = mutableSetOf<String>()
    val intervals = mutableListOf<Pair<Long, Long>>()
    var duration = 0L
    var minimumLink = Double.POSITIVE_INFINITY
    var maximumObservedCost = 0.0
    samples.forEach { sample ->
        val uri = runCatching { java.net.URI(sample.segmentId) }.getOrNull() ?: return@forEach
        val path = uri.rawPath ?: return@forEach
        val name = path.substringAfterLast('/')
        val index = name.removePrefix("seg").removeSuffix(".m4s").toLongOrNull() ?: return@forEach
        val start = sample.mediaStartTimeMs ?: return@forEach
        val end = sample.mediaEndTimeMs ?: return@forEach
        val advertised = sample.observedMediaDurationMs ?: return@forEach
        val receipt = sample.receipt ?: return@forEach
        val etag = sample.etag?.takeIf { it.isNotEmpty() } ?: return@forEach
        val link = autoCompletedTransferBps(sample, nowMs, 15_000L) ?: return@forEach
        if (sample.pipelineIdentity !== pipeline || !sample.fullObject || sample.statusCode != 200 ||
            !path.split('/').contains(sessionId) || index !in 0..4_294_967_295L ||
            name != "seg${index.toString().padStart(5, '0')}.m4s" || start < 0 || end <= start ||
            advertised <= 0 || kotlin.math.abs(advertised - (end - start)) > 2 ||
            !Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").matches(receipt) ||
            name in names || receipt in receipts || etag in etags ||
            intervals.any { (a, b) -> !(b <= start || end <= a) }) return@forEach
        names.add(name); receipts.add(receipt); etags.add(etag)
        intervals.add(start to end)
        duration += end - start
        minimumLink = minOf(minimumLink, link)
        maximumObservedCost = maxOf(maximumObservedCost, sample.bodyBytes.toDouble() * 8_000 / (end - start))
    }
    return intervals.size >= 2 && duration >= 2_000 && maximumObservedCost > 0 &&
        maximumObservedCost.isFinite() && minimumLink >= maximumObservedCost * 1.8
}

internal fun autoCompletedTransferBps(sample: AutoCompletedTransfer, nowMs: Long, maximumAgeMs: Long = 10_000L): Double? {
    val duration = sample.bodyDurationMs ?: return null
    if (!sample.networkLoad || sample.fromLocalCache != false || sample.producerPaced != false ||
        sample.ageMs(nowMs) > maximumAgeMs || sample.bodyBytes <= 0 || duration <= 0) return null
    return (sample.bodyBytes.toDouble() * 8_000.0 / duration).takeIf { it.isFinite() && it > 0 }
}

/** Unknown source-copy exposure is a bounded trial, never a measured peak. */
internal fun autoUnknownOriginalTrial(candidate: tv.plurx.app.data.QualityCandidate,
    outputs: List<tv.plurx.app.data.MeasuredCandidateOutput>): Boolean =
    candidate.hasValidIdentity && candidate.decoder_compatible && candidate.route == "remux" &&
        candidate.peak_bps == null && outputs.size <= 64 && outputs.none {
            it.candidate_id == candidate.id && it.recipe_digest == candidate.recipe_digest && it.route == candidate.route
        }

/** Private observational windows; attachment changes cannot inherit old proof. */
internal class AutoUpgradeEvidenceWindow {
    private var attachment: Any? = null
    private var epoch: Long? = null
    private var observationStartMs: Long? = null
    var lastStallMs: Long? = null
        private set
    var lastCliffMs: Long? = null
        private set
    fun reset() { attachment = null; epoch = null; observationStartMs = null; lastStallMs = null; lastCliffMs = null }
    fun bind(attachment: Any?, epoch: Long, nowMs: Long) {
        if (this.attachment === attachment && this.epoch == epoch) return
        reset()
        this.attachment = attachment
        this.epoch = epoch
        observationStartMs = if (attachment != null && nowMs >= 0) nowMs else null
    }
    fun stalled(nowMs: Long) {
        if (attachment != null && nowMs >= 0) lastStallMs = maxOf(lastStallMs ?: nowMs, nowMs)
    }
    fun cliff(completedAtMs: Long, nowMs: Long) {
        if (attachment != null && completedAtMs >= 0 && nowMs >= completedAtMs && nowMs - completedAtMs <= 15_000L)
            lastCliffMs = maxOf(lastCliffMs ?: completedAtMs, completedAtMs)
    }
    fun allowsUpgrade(nowMs: Long): Boolean {
        val start = observationStartMs ?: return false
        val quietStart = maxOf(start, lastStallMs ?: start)
        return attachment != null && nowMs >= quietStart && nowMs - quietStart >= 60_000L &&
            (lastCliffMs?.let { nowMs >= it && nowMs - it >= 90_000L } ?: true)
    }
    fun allowsProposal(nowMs: Long, headroomStartedAtMs: Long?): Boolean =
        headroomStartedAtMs != null && nowMs >= headroomStartedAtMs &&
            nowMs - headroomStartedAtMs >= 45_000L && allowsUpgrade(nowMs)
}

/** Two progressing supplied intervals, never a wait callback or stale player. */
internal data class AutoDecodePressureEvidence(val elapsedMs: Long, val progressMs: Long, val droppedFrames: Long)
internal class AutoDecodePressureWindow {
    private var attachment: Any? = null
    private var candidate: String? = null
    private var previous: Triple<Long, Long, Long>? = null
    private var intervals = 0
    private var first: Triple<Long, Long, Long>? = null
    var evidence: AutoDecodePressureEvidence? = null
        private set
    fun observe(attachment: Any?, candidate: String?, nowMs: Long, positionMs: Long,
                cumulativeDropped: Long?, eligible: Boolean): Boolean {
        if (this.attachment !== attachment || this.candidate != candidate) {
            this.attachment = attachment; this.candidate = candidate; previous = null; first = null; evidence = null; intervals = 0
        }
        if (attachment == null || candidate == null || !eligible || nowMs < 0 || positionMs < 0 ||
            cumulativeDropped == null || cumulativeDropped < 0) {
            previous = null; first = null; evidence = null; intervals = 0; return false
        }
        val old = previous
        previous = Triple(nowMs, positionMs, cumulativeDropped)
        if (old == null || nowMs < old.first || positionMs < old.second || cumulativeDropped < old.third) {
            first = null; evidence = null; intervals = 0; return false
        }
        val elapsed = nowMs - old.first
        if (elapsed !in 1_000L..5_000L || positionMs - old.second < elapsed / 2 || cumulativeDropped - old.third < 3) {
            first = null; evidence = null; intervals = 0; return false
        }
        intervals = (intervals + 1).coerceAtMost(2)
        if (first == null) first = old
        val start = first ?: return false
        if (intervals < 2 || nowMs - start.first < 4_000L) return false
        evidence = AutoDecodePressureEvidence(nowMs - start.first, positionMs - start.second, cumulativeDropped - start.third)
        return true
    }
}

internal data class AutoRecoveryCauseTicket(val session: String, val attachment: Any,
    val incumbentCandidate: String, val proposedCandidate: String,
    val cause: tv.plurx.app.data.ReopenReason, val observedAtMs: Long, val linkReceipt: String? = null) {
    fun receiptForRequest(previous: String?, candidate: String?, cause: tv.plurx.app.data.ReopenReason?): String? =
        linkReceipt.takeIf { this.cause == tv.plurx.app.data.ReopenReason.Link && cause == this.cause &&
            previous == session && candidate == proposedCandidate }
    fun isCurrent(session: String?, attachment: Any?, candidate: String?, proposed: String?, nowMs: Long): Boolean =
        this.session == session && this.attachment === attachment && incumbentCandidate == candidate &&
            proposedCandidate == proposed && nowMs >= observedAtMs && nowMs - observedAtMs <= 15_000L
}

internal fun autoActiveProductionPressure(status: PlaybackSessionStatus?, observedAtMs: Long?, nowMs: Long,
                                          sessionId: String?, candidateId: String, runwayMs: Long?): Boolean {
    if (status == null || observedAtMs == null || nowMs < observedAtMs) return false
    val age = status.active_encode_age_ms ?: return false
    val elapsed = nowMs - observedAtMs
    return elapsed <= 15_000L && age >= 0 && age <= 15_000L - elapsed &&
        sessionId != null && status.id == sessionId && status.active_encode_candidate_id == candidateId &&
        status.active_encode_segments?.let { it >= 2 } == true &&
        status.active_encode_active_ms?.let { it >= 2_000L } == true &&
        status.active_encode_milli_realtime?.let { it in 1..999 } == true &&
        runwayMs?.let { it in 0..9_999L } == true
}

internal fun autoOriginalTransferMarginProven(samples: List<AutoCompletedTransfer>, sessionId: String, nowMs: Long): Boolean {
    val usable = samples.filter { it.segmentId.contains("/$sessionId/") &&
        autoCompletedTransferBps(it, nowMs, 15_000L) != null && (it.mediaDurationMs ?: 0L) > 0
    }.associateBy { it.segmentId }.values
    if (usable.size < 2 || usable.sumOf { it.mediaDurationMs ?: 0L } < 2_000L) return false
    val link = usable.mapNotNull { autoCompletedTransferBps(it, nowMs, 15_000L) }.minOrNull() ?: return false
    val wire = usable.maxOf { it.bodyBytes.toDouble() * 8_000.0 / (it.mediaDurationMs ?: 1L) }
    return wire.isFinite() && link >= wire * 1.8
}

internal fun autoStagedEncodeProof(status: PlaybackSessionStatus?, observedAtMs: Long?, nowMs: Long,
                                  sessionId: String, candidateId: String): Boolean {
    if (status == null || observedAtMs == null || nowMs < observedAtMs) return false
    val age = status.active_encode_age_ms ?: return false
    val elapsed = nowMs - observedAtMs
    return elapsed <= 15_000L && age >= 0 && age <= 15_000L - elapsed &&
        status.id == sessionId && status.active_encode_candidate_id == candidateId &&
        status.active_encode_segments?.let { it >= 2 } == true &&
        status.active_encode_active_ms?.let { it >= 2_000L } == true &&
        status.active_encode_milli_realtime?.let { it >= 1_150 } == true
}

internal fun autoTrialMayCommit(requestedId: String?, offeredId: String?, requestedTargetRevision: Long?,
                                targetRevision: Long?, requestedViewerEpoch: Long?, viewerEpoch: Long,
                                automatic: Boolean, presenting: Boolean, seeking: Boolean): Boolean =
    requestedId != null && requestedId == offeredId && requestedTargetRevision != null &&
        requestedTargetRevision == targetRevision && requestedViewerEpoch == viewerEpoch &&
        automatic && presenting && !seeking

internal data class AutoTickState(
    val protocol: Boolean,
    val automatic: Boolean,
    val playing: Boolean,
    val seeking: Boolean,
    val changePending: Boolean,
    val controlClosed: Boolean,
    val producerState: String?,
)

/** Why display-aware Auto cannot decide on this tick, or null. A held producer
 * is not a reason: it is the ordinary paced steady state of a producer running
 * ahead of its viewer and evidence in neither direction (the `producer-paced`
 * case of tests/playback/auto-quality-policy.json). Transfers it paced are
 * excluded by their own provenance. */
internal fun autoDecisionGate(state: AutoTickState): String? = when {
    !state.protocol -> "protocol"
    !state.automatic -> "manual"
    !state.playing -> "not_playing"
    state.seeking -> "seeking"
    state.changePending -> "change_pending"
    state.controlClosed -> "control_closed"
    else -> null
}

/** A completed body that measures the link: an ordinary session segment, or a
 * continuous family's video segment. Shared AAC objects are too small to be a
 * link sample, and playlists, initialization maps and subtitles are excluded. */
internal fun autoLinkMediaSegment(path: String): Boolean =
    Regex("seg[0-9]+\\.(m4s|ts)").matches(path.substringAfterLast('/')) ||
        Regex(".*/video/[0-9a-f]{64}/segment/[0-9]+\\.m4s").matches(path)

/** Bounded per-pipeline evidence; a failed/closed transfer is not completion.
 * The existing HTTP factory has no local HTTP cache. A nonnetwork source is
 * nonetheless unknown cache provenance rather than a network-link measurement. */
@UnstableApi
internal class AutoTransferEvidence(private val delegate: TransferListener) : TransferListener {
    private data class Body(val uri: String, val startedAtMs: Long, val network: Boolean,
                            val origin: String?, val paced: Boolean?, val receipt: String?,
                            val etag: String?, val statusCode: Int?, val observedMediaDurationMs: Long?,
                            val fullObject: Boolean,
                            var bytes: Long = 0)
    private val active = java.util.IdentityHashMap<DataSource, Body>()
    private val ended = LinkedHashMap<String, AutoCompletedTransfer>()
    private var completed: AutoCompletedTransfer? = null
    private val recent = mutableListOf<AutoCompletedTransfer>()
    @Synchronized fun latest(): AutoCompletedTransfer? = completed
    @Synchronized fun recent(): List<AutoCompletedTransfer> = recent.toList()
    @Synchronized fun reset() { active.clear(); ended.clear(); completed = null; recent.clear() }
    @Synchronized fun discard(uri: String) { ended.remove(uri) }
    @Synchronized fun complete(load: LoadEventInfo, media: MediaLoadData) {
        val sample = ended.remove(load.uri.toString()) ?: return
        // Exclude playlists, initialization maps, subtitles and progressive
        // resources. These are server media segments, not arbitrary requests.
        if (!autoLinkMediaSegment(load.uri.path.orEmpty()) || load.bytesLoaded <= 0 ||
            sample.bodyBytes != load.bytesLoaded) return
        val duration = if (media.mediaStartTimeMs != C.TIME_UNSET && media.mediaEndTimeMs != C.TIME_UNSET)
            (media.mediaEndTimeMs - media.mediaStartTimeMs).takeIf { it > 0 } else null
        val confirmed = sample.copy(segmentId = load.uri.toString(), mediaDurationMs = duration,
            mediaStartTimeMs = media.mediaStartTimeMs.takeIf { it != C.TIME_UNSET && it >= 0 },
            mediaEndTimeMs = media.mediaEndTimeMs.takeIf { it != C.TIME_UNSET && it >= 0 })
        completed = confirmed
        recent.removeAll { it.segmentId == confirmed.segmentId }
        recent.add(confirmed)
        if (recent.size > 4) recent.removeAt(0)
    }
    override fun onTransferInitializing(source: DataSource, dataSpec: DataSpec, isNetwork: Boolean) =
        delegate.onTransferInitializing(source, dataSpec, isNetwork)
    override fun onTransferStart(source: DataSource, dataSpec: DataSpec, isNetwork: Boolean) {
        delegate.onTransferStart(source, dataSpec, isNetwork)
        val headers = source.responseHeaders
        val paced = headers.entries.firstOrNull { it.key.equals("X-Plurx-Producer-Paced", true) }?.value?.singleOrNull()
        val receipt = headers.entries.firstOrNull { it.key.equals("X-Plurx-Link-Receipt", true) }?.value?.singleOrNull()
        val etag = headers.entries.firstOrNull { it.key.equals("ETag", true) }?.value?.singleOrNull()
        val observedMediaDuration = headers.entries.firstOrNull {
            it.key.equals("X-Plurx-Link-Media-Duration-Ms", true)
        }?.value?.singleOrNull()?.toLongOrNull()?.takeIf { it in 1..4_294_967_295L }
        val uri = source.uri ?: dataSpec.uri
        val origin = if (uri.scheme != null && uri.host != null) "${uri.scheme}://${uri.host}${if (uri.port >= 0) ":${uri.port}" else ""}" else null
        synchronized(this) {
            if (active.size >= 32) active.clear()
            active[source] = Body(uri.toString(), monotonicNowMs(), isNetwork, origin,
                when (paced) { "0" -> false; "1" -> true; else -> null }, receipt, etag,
                (source as? HttpDataSource)?.responseCode, observedMediaDuration,
                dataSpec.position == 0L && dataSpec.length == C.LENGTH_UNSET.toLong())
        }
    }
    override fun onBytesTransferred(source: DataSource, dataSpec: DataSpec, isNetwork: Boolean, bytesTransferred: Int) {
        delegate.onBytesTransferred(source, dataSpec, isNetwork, bytesTransferred)
        synchronized(this) { active[source]?.let { if (bytesTransferred > 0) it.bytes += bytesTransferred } }
    }
    override fun onTransferEnd(source: DataSource, dataSpec: DataSpec, isNetwork: Boolean) {
        delegate.onTransferEnd(source, dataSpec, isNetwork)
        synchronized(this) {
            val body = active.remove(source) ?: return
            val now = monotonicNowMs()
            val duration = (now - body.startedAtMs).takeIf { it in 1..120_000 }
            ended[body.uri] = AutoCompletedTransfer(body.bytes, duration, now, body.origin, body.network,
                if (body.network) false else null, body.paced, receipt = body.receipt, etag = body.etag,
                statusCode = body.statusCode, pipelineIdentity = this,
                observedMediaDurationMs = body.observedMediaDurationMs, fullObject = body.fullObject)
            while (ended.size > 32) ended.remove(ended.keys.first())
        }
    }
}

@UnstableApi
class BuiltPlayer internal constructor(
    val player: ExoPlayer,
    internal val progressiveMediaOrigin: ProgressiveMediaOrigin,
    internal val autoTransfers: AutoTransferEvidence = AutoTransferEvidence(progressiveMediaOrigin),
    internal val continuousSources: ContinuousSourceRegistry = ContinuousSourceRegistry(),
    internal val continuousOutput: ContinuousOutputEvidence = ContinuousOutputEvidence(),
)

@UnstableApi
fun buildPlayer(context: Context, vm: AppViewModel, audioOnly: Boolean = false): BuiltPlayer =
    buildPipeline(context, vm, if (audioOnly) PlayerRole.Audio else PlayerRole.Finite)

/**
 * The second pipeline a prepared replacement primes.
 *
 * Not [buildPlayer] called twice: a successor is muted and has no surface until
 * the moment it commits, and it must be visibly a different construction so
 * that neither one can quietly acquire the other's properties.
 *
 * **Tunneling inherits the incumbent's setting**, which on a television means
 * the successor is tunneled too. That is deliberate and it is the *lossy*
 * choice: M5.5's only hard failure was same-codec dual prime on a tunneled
 * Google TV — two identical tunneled pipelines appear to contend where two
 * different codecs get distinct decoder instances — so this reproduces that
 * failure rather than dodging it. The alternative, building the successor with
 * tunneling forced off, would probably make that device pass, but the pipeline
 * that primed would not be the pipeline that serves after the switch, and a
 * measurement that cannot fail is the same class of error as M5.5's rejected
 * first instrument set. The failure path here is `failed` plus the fallback
 * ladder, which is a bounded cost; a successful prime that proves nothing is
 * not. Revisit with evidence if the tunneling-off re-run is ever taken
 * (`docs/playback-control/PLAYBACK-CONTROL-STATUS.md:1240-1243`).
 */
@UnstableApi
fun buildSuccessorPlayer(context: Context, vm: AppViewModel, audioOnly: Boolean = false): BuiltPlayer {
    val built = buildPipeline(context, vm, if (audioOnly) PlayerRole.Audio else PlayerRole.Successor)
    // Never audible and never visible before the switch. Silence is set here
    // rather than relied on from the composition not rendering it: a prepared
    // successor that is merely off-screen is still an audio stream.
    built.player.volume = 0f
    built.player.setVideoSurface(null)
    // An audio-only successor is built in the Audio role, which holds focus
    // for ordinary playback; a successor never does until its switch.
    built.player.asAudioFocusOwner().ownAudioFocus(false)
    built.player.playWhenReady = true
    return built
}

@UnstableApi
private fun buildPipeline(context: Context, vm: AppViewModel, role: PlayerRole): BuiltPlayer {
    val progressiveMediaOrigin = ProgressiveMediaOrigin()
    val autoTransfers = AutoTransferEvidence(progressiveMediaOrigin)
    val continuousSources = ContinuousSourceRegistry()
    val continuousOutput = ContinuousOutputEvidence()
    val player = PlurxPlayerBuilder(context, role).build(
        dataSource = Net.dataSourceFactory(),
        audioLanguage = vm.audioLang,
        transferListener = autoTransfers,
        continuousSources = continuousSources,
        continuousOutput = continuousOutput,
    )
    player.addAnalyticsListener(object : AnalyticsListener {
        override fun onMediaItemTransition(eventTime: AnalyticsListener.EventTime, mediaItem: MediaItem?, reason: Int) {
            autoTransfers.reset()
        }
        override fun onLoadCompleted(eventTime: AnalyticsListener.EventTime, loadEventInfo: LoadEventInfo, mediaLoadData: MediaLoadData) {
            if (mediaLoadData.dataType == C.DATA_TYPE_MEDIA) autoTransfers.complete(loadEventInfo, mediaLoadData)
        }
        override fun onLoadError(eventTime: AnalyticsListener.EventTime, loadEventInfo: LoadEventInfo, mediaLoadData: MediaLoadData, error: java.io.IOException, wasCanceled: Boolean) {
            autoTransfers.discard(loadEventInfo.uri.toString())
        }
        override fun onLoadCanceled(eventTime: AnalyticsListener.EventTime, loadEventInfo: LoadEventInfo, mediaLoadData: MediaLoadData) {
            autoTransfers.discard(loadEventInfo.uri.toString())
        }
    })
    return BuiltPlayer(player, progressiveMediaOrigin, autoTransfers, continuousSources, continuousOutput)
}

/**
 * A slide-in panel listing the embedded audio and subtitle tracks ExoPlayer
 * found (populated for direct play; a transcode usually carries the single
 * server-selected track). Selecting one pins it via track-selection overrides.
 */
@UnstableApi
@Composable
fun TrackMenu(
    player: ExoPlayer,
    serverAudio: List<AudioTrack>,
    serverSubtitles: List<SubTrack>,
    serverControlledAudio: Boolean,
    selectedServerAudio: Long?,
    selectedServerSubtitle: Long?,
    onServerAudio: (Long) -> Unit,
    onServerSubtitle: (Long?) -> Unit,
    onDismiss: () -> Unit,
) {
    val tracks = player.currentTracks
    val audio = tracks.groups.filter { it.type == C.TRACK_TYPE_AUDIO }
    val text = tracks.groups.filter { it.type == C.TRACK_TYPE_TEXT }
    val initialFocusRequester = remember { FocusRequester() }
    var initialFocusAttached = false

    fun initialFocusModifier(enabled: Boolean): Modifier {
        if (!enabled || initialFocusAttached) return Modifier
        initialFocusAttached = true
        return Modifier.focusRequester(initialFocusRequester)
    }

    Box(
        Modifier
            .fillMaxSize()
            .background(Color(0x99000000))
            .focusProperties { canFocus = false }
            .clickable(onClick = onDismiss),
    ) {
        Column(
            Modifier
                .align(Alignment.CenterEnd)
                .fillMaxHeight()
                .widthIn(max = 380.dp)
                .fillMaxWidth(0.92f)
                .background(Color(0xFF141418))
                .verticalScroll(rememberScrollState())
                .focusGroup()
                .focusProperties { onExit = { cancelFocusChange() } }
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (serverControlledAudio && serverAudio.isNotEmpty()) {
                Text("Audio", style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(bottom = 4.dp))
                serverAudio.forEach { track ->
                    TrackRow(
                        label = serverAudioLabel(track),
                        selected = selectedServerAudio == track.index,
                        enabled = true,
                        modifier = initialFocusModifier(enabled = true),
                    ) { onServerAudio(track.index) }
                }
            } else if (audio.isNotEmpty()) {
                Text("Audio", style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(bottom = 4.dp))
                audio.forEach { group ->
                    for (i in 0 until group.length) {
                        val enabled = group.isTrackSupported(i)
                        TrackRow(
                            label = audioLabel(group.getTrackFormat(i)),
                            selected = group.isTrackSelected(i),
                            enabled = enabled,
                            modifier = initialFocusModifier(enabled),
                        ) {
                            player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                                .setOverrideForType(TrackSelectionOverride(group.mediaTrackGroup, i))
                                .setTrackTypeDisabled(C.TRACK_TYPE_AUDIO, false)
                                .build()
                            onDismiss()
                        }
                    }
                }
            }

            // One row per subtitle, whichever way it will be delivered.
            //
            // These used to be two lists — the decision's tracks and whatever
            // ExoPlayer had demuxed — so on direct play every track appeared
            // twice, and the two rows behaved differently: one switched the
            // embedded track, the other started a burn. The decision's list is
            // the authoritative one (it names every track, carries the absolute
            // stream index the server routes on, and knows which are servable
            // as renditions), so it is the only list shown when it exists. The
            // player's own groups remain the fallback for a source the server
            // told us nothing about.
            if (text.isNotEmpty() || serverSubtitles.isNotEmpty()) {
                Text("Subtitles", style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(top = 14.dp, bottom = 4.dp))
                val serverOwnsSubtitles = serverSubtitles.isNotEmpty()
                TrackRow(
                    label = "Off",
                    selected = selectedServerSubtitle == null &&
                        (serverOwnsSubtitles || !tracks.isTypeSelected(C.TRACK_TYPE_TEXT)),
                    enabled = true,
                    modifier = initialFocusModifier(enabled = true),
                ) {
                    if (!serverOwnsSubtitles) {
                        player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                            .clearOverridesOfType(C.TRACK_TYPE_TEXT)
                            .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                            .build()
                    }
                    onServerSubtitle(null)
                    onDismiss()
                }
                if (serverOwnsSubtitles) {
                    serverSubtitles.forEach { track ->
                        TrackRow(
                            label = serverSubtitleLabel(track),
                            selected = selectedServerSubtitle == track.index,
                            enabled = true,
                            modifier = initialFocusModifier(enabled = true),
                        ) { onServerSubtitle(track.index); onDismiss() }
                    }
                } else {
                    text.forEach { group ->
                        for (i in 0 until group.length) {
                            val enabled = group.isTrackSupported(i)
                            TrackRow(
                                label = subLabel(group.getTrackFormat(i)),
                                selected = group.isTrackSelected(i),
                                enabled = enabled,
                                modifier = initialFocusModifier(enabled),
                            ) {
                                player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                                    .setOverrideForType(TrackSelectionOverride(group.mediaTrackGroup, i))
                                    .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false)
                                    .build()
                                onDismiss()
                            }
                        }
                    }
                }
            }

            if (audio.isEmpty() && text.isEmpty() && serverAudio.isEmpty() && serverSubtitles.isEmpty()) {
                Text("No selectable tracks", color = Muted, style = MaterialTheme.typography.bodyMedium)
            }
        }
    }

    if (initialFocusAttached) {
        RequestInitialFocus(initialFocusRequester)
    }
}

@Composable
private fun TrackRow(
    label: String,
    selected: Boolean,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    onClick: () -> Unit,
) {
    Text(
        text = (if (selected) "● " else "   ") + label,
        color = when {
            selected -> Accent
            !enabled -> Muted
            else -> Color(0xFFECECEF)
        },
        fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
        style = MaterialTheme.typography.bodyMedium,
        modifier = modifier
            .fillMaxWidth()
            .tvFocusRing(MaterialTheme.shapes.small, focusedScale = 1.02f)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(vertical = 8.dp),
    )
}

internal fun audioLabel(f: Format): String {
    val parts = mutableListOf<String>()
    languageName(f.language)?.let { parts.add(it) }
    f.label?.let { parts.add(it) }
    if (f.channelCount != Format.NO_VALUE) {
        parts.add(
            when (f.channelCount) {
                1 -> "Mono"; 2 -> "Stereo"; 6 -> "5.1"; 8 -> "7.1"
                else -> "${f.channelCount}ch"
            }
        )
    }
    codecShort(f.sampleMimeType)?.let { parts.add(it) }
    return parts.distinct().joinToString(" · ").ifBlank { "Audio" }
}

internal fun subLabel(f: Format): String {
    val parts = mutableListOf<String>()
    languageName(f.language)?.let { parts.add(it) }
    f.label?.let { parts.add(it) }
    if (f.selectionFlags and C.SELECTION_FLAG_FORCED != 0) parts.add("Forced")
    return parts.distinct().joinToString(" · ").ifBlank { "Subtitle" }
}

internal fun serverAudioLabel(track: AudioTrack): String = listOfNotNull(
    languageName(track.language),
    track.title,
    track.channels?.let {
        when (it) {
            1 -> "Mono"; 2 -> "Stereo"; 6 -> "5.1"; 8 -> "7.1"; else -> "${it}ch"
        }
    },
    track.codec.uppercase(),
).distinct().joinToString(" · ").ifBlank { "Audio" }

internal fun serverSubtitleLabel(track: SubTrack): String = listOfNotNull(
    languageName(track.language),
    track.title,
    if (isForcedSubtitle(track)) "Forced" else null,
    // Bitmap overlays are composited by the client; only a server-rendered
    // subtitle should carry the burn-in warning.
    if (track.isPgsOverlay) "Overlay" else if (!track.isNativeHls) "Burn-in" else null,
).distinct().joinToString(" · ").ifBlank { "Subtitle" }

internal fun languageName(code: String?): String? {
    if (code.isNullOrBlank() || code == "und") return null
    return try {
        Locale.forLanguageTag(code).displayLanguage.ifBlank { code }
    } catch (_: Exception) {
        code
    }
}

internal fun codecShort(mime: String?): String? = when {
    mime == null -> null
    mime.contains("hevc", true) || mime.contains("h265", true) -> "HEVC"
    mime.contains("avc", true) || mime.contains("h264", true) -> "H.264"
    mime.contains("av01", true) || mime.contains("av1", true) -> "AV1"
    mime.contains("vp9", true) -> "VP9"
    mime.contains("ac3", true) && mime.contains("e", true) -> "E-AC3"
    mime.contains("ac3", true) -> "AC3"
    mime.contains("dts", true) -> "DTS"
    mime.contains("truehd", true) -> "TrueHD"
    mime.contains("aac", true) -> "AAC"
    mime.contains("flac", true) -> "FLAC"
    mime.contains("opus", true) -> "Opus"
    else -> null
}

/** A hidden status panel cannot justify two-second network polling. */
internal fun statusPollIntervalMs(visible: Boolean): Long = if (visible) 2_000L else 10_000L

/** Private-controller transport interception; unchanged commands remain the
 * SDK delegate's commands, while stale transport wrappers have no authority. */
internal fun autoViewerTransport(delegate: Player, isCurrent: () -> Boolean,
    request: (Boolean) -> Unit): Player = object : ForwardingPlayer(delegate) {
    override fun play() { if (isCurrent()) request(true) }
    override fun pause() { if (isCurrent()) request(false) }
    override fun setPlayWhenReady(playWhenReady: Boolean) {
        if (isCurrent()) request(playWhenReady)
    }
}

/** What one viewer Play/Pause edge does to the player right now. */
internal data class ViewerTransportEdge(
    /** Written to the delegate immediately. Nothing optional may hold it. */
    val delegatePlayWhenReady: Boolean,
    /** The edge is a resume after at least [LONG_VIEWER_PAUSE_MS] of explicit
     * pause on the same attachment, which owes Auto one original-first
     * re-plan. The re-plan runs later, beside the playing incumbent. */
    val armsAutoBoundaryReplan: Boolean,
)

internal const val LONG_VIEWER_PAUSE_MS = 60_000L

/**
 * The viewer's Play reaches the player on the press, however long the pause
 * was. A long pause used to defer that write until an optional original
 * preparation either committed at the paused frame or timed out — seconds of
 * a pressed Play doing nothing. The re-plan is still owed; it is just no
 * longer allowed to stand between the viewer and the picture.
 */
internal fun viewerTransportEdge(requested: Boolean, wasRequested: Boolean, pausedAtMs: Long?,
    pauseOwnerCurrent: Boolean, nowMs: Long): ViewerTransportEdge =
    ViewerTransportEdge(
        delegatePlayWhenReady = requested,
        armsAutoBoundaryReplan = requested && !wasRequested && pauseOwnerCurrent && pausedAtMs != null &&
            nowMs >= pausedAtMs && nowMs - pausedAtMs >= LONG_VIEWER_PAUSE_MS,
    )

/**
 * The one Auto re-plan a viewer boundary is owed, held until the ordinary
 * Auto evaluation can serve it as a prepared replacement.
 *
 * It is keyed by the viewer transport lifetime that armed it, so any newer
 * viewer edge (a Pause, another seek) makes it stale without a timer, and it
 * is consumed by the first evaluation where the incumbent has the runway a
 * handoff needs. Nothing waits on it.
 */
internal class AutoBoundaryReplan {
    private var armedBy: Any? = null

    val isArmed: Boolean get() = armedBy != null

    fun arm(lifetime: Any) { armedBy = lifetime }

    fun clear() { armedBy = null }

    /**
     * The candidate [produce] names, exactly once per armed boundary, when
     * [runwayMs] can carry it. A boundary is spent only on a candidate: with
     * none (no fresh link proof yet, every original blocked) it stays owed
     * until it is served, superseded or [clear]ed.
     */
    fun <T : Any> take(lifetime: Any, runwayMs: Long, produce: () -> T?): T? {
        val owner = armedBy ?: return null
        if (owner !== lifetime) {
            armedBy = null
            return null
        }
        if (runwayMs < AUTO_BOUNDARY_RUNWAY_MS) return null
        val chosen = produce() ?: return null
        armedBy = null
        return chosen
    }

    companion object {
        const val AUTO_BOUNDARY_RUNWAY_MS = 10_000L
    }
}

/**
 * After a boundary's offer reached `onPrepareAction`: true when it
 * left no pipeline behind for the boundary that still owns Auto. A switched
 * preparation is on screen (its player has already moved to the incumbent
 * slot) and settles through its own first frame, so it never counts.
 */
internal fun autoBoundaryOfferBuiltNothing(boundaryStillOwns: Boolean, built: Boolean, switched: Boolean): Boolean =
    boundaryStillOwns && !built && !switched
