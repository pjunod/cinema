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
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.TrackGroup
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.exoplayer.trackselection.DefaultTrackSelector
import androidx.media3.session.MediaSession
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import tv.plurx.app.data.Caps
import tv.plurx.app.data.HlsStart
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.DeviceCaps
import tv.plurx.app.data.ReopenReason
import tv.plurx.app.data.AudioTrack
import tv.plurx.app.data.SubTrack
import tv.plurx.app.data.SubtitleReadiness
import tv.plurx.app.data.Net
import tv.plurx.app.data.PlaybackSessionStatus
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.Session
import tv.plurx.app.ui.AppViewModel
import tv.plurx.app.ui.theme.Accent
import tv.plurx.app.ui.theme.Muted
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.components.RequestInitialFocus
import java.util.Locale
import java.util.UUID

import retrofit2.HttpException
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
class Controller(
    private val context: Context,
    builtPlayer: BuiltPlayer,
    private val plan: PlanLike,
    private val caps: Map<String, String>,
    private val decisionCaps: DeviceCaps,
    private val playbackIntent: PlaybackIntent,
    private val vm: AppViewModel,
    private val scope: CoroutineScope,
    initialAudioOffsetMs: Long = 0,
    retainedAudio: Long? = null,
    retainedSubtitle: SubtitleChoice? = null,
    private val onError: (String) -> Unit = {},
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

    /** Immutable quality of this exact decision; pending intent is separate. */
    private val activeQuality = plan.requestedQuality
    private val planReplacement = PlaybackPlanReplacement(activeQuality)

    private var progressiveMediaOrigin = builtPlayer.progressiveMediaOrigin
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

    var playbackNotice: String? by mutableStateOf(null)
        private set

    internal var pgsOverlayFrame: PGSOverlayFrame? by mutableStateOf(null)
        private set

    internal var pgsOverlayStatus: PGSOverlayStatus by mutableStateOf(PGSOverlayStatus.Off)
        private set

    /** A direct DV failure may use one lossless remux before the final rescue. */
    private var compatibilityRemuxUsed = false

    /** One final H.264 compatibility transcode; failure after that is terminal. */
    private var compatibilityTranscodeUsed = false

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
    private val sessionCreateCoordinator = SessionCreateCoordinator(
        createSession = { body -> vm.createHlsSession(plan.fileId, body) },
        isBadRequest = { failure -> failure is HttpException && failure.code() == 400 },
        freshRequestId = { UUID.randomUUID().toString() },
        releaseSession = vm::endHlsSession,
    )

    /**
     * The delivery this plan would use with no subtitle in play. A manual A/V
     * correction is only expressible by the remuxer, so it moves direct play
     * one step down even before subtitles are considered; the compatibility
     * rescue moves everything to a transcode, because "the device refused
     * these bytes" is exactly a demand for different ones.
     */
    private val planMode: String
        get() = when {
            forceCompatibilityTranscode -> "transcode"
            forceCompatibilityRemux -> "remux"
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
            quality = playbackIntent.desiredQuality,
            mode = planMode,
            audioIndex = selectedAudio,
            subtitleIndex = selectedSubtitle,
            subtitleDelivery = subtitleDelivery,
            audioOffsetMs = audioOffsetMs,
            compatibilityTranscode = forceCompatibilityTranscode,
        ),
    )

    private fun attachRecipe(recipe: PlaybackRecipeOwnership.Claim) {
        recipeOwnership.attach(recipe)
        selectionRecipe = recipe
        textSelectionArmed = true
        audioSelectionArmed = true
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
            val recipe = recipeOwnership.attached?.recipe
            val mode = recipe?.mode ?: planMode
            return when (recipe?.subtitleDelivery ?: subtitleDelivery) {
                SubtitleDelivery.Burn -> "transcode"
                SubtitleDelivery.NativeSession -> if (mode == "transcode") "transcode" else "remux"
                SubtitleDelivery.BitmapOverlay,
                SubtitleDelivery.Plan -> mode
            }
        }

    val pgsOverlayIsActive: Boolean
        get() = pgsOverlayStatus != PGSOverlayStatus.Off

    /** True while the original file is being read directly, base timeline = 0. */
    private val directTransport: Boolean
        get() = (recipeOwnership.attached?.recipe?.subtitleDelivery ?: subtitleDelivery).usesPlanTransport &&
            (recipeOwnership.attached?.recipe?.mode ?: planMode) == "direct"

    /** True while Media3 is reading the live progressive remux response. */
    private val progressiveTransport: Boolean
        get() = (recipeOwnership.attached?.recipe?.subtitleDelivery ?: subtitleDelivery).usesPlanTransport &&
            (recipeOwnership.attached?.recipe?.mode ?: planMode) == "remux"

    var encoder: String? = null
        private set

    var sessionStatus: PlaybackSessionStatus? by mutableStateOf(null)
        private set
    private var sessionStatusObservedAtMs: Long? by mutableStateOf(null)
    val sessionStatusAgeMs: Long?
        get() = sessionStatusObservedAtMs?.let { (monotonicNowMs() - it).coerceAtLeast(0) }
    val presentationProgressAgeMs: Long?
        get() = openStallTracker.progressAgeMs(monotonicNowMs())
    val isPlaybackWaiting: Boolean
        get() = player.playWhenReady && player.playbackState == Player.STATE_BUFFERING

    val currentSessionId: String? get() = sessionId
    val currentSessionIsVod: Boolean get() = sessionIsVod

    private val mediaSession = MediaSession.Builder(context, player).build()

    /** The HLS session this player owns, if the plan opened one. */
    private var sessionId: String? = null
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
                method = if (deliveryMode == "direct") "direct_play" else deliveryMode,
                encoder = encoder,
                sessionId = sessionId,
            )
        },
        emit = { event -> postPlaybackClientLog(scope, event) },
    )
    private val stallWatchdogJob: Job
    private val targetPresentationWatchdogJob: Job
    private val targetPresentationDeadline = playbackIntent.targetPresentationDeadline
    private val targetPresentationOwner = targetPresentationDeadline.claimOwner(monotonicNowMs())
    private var presentationForeground = true
    private var mediaMutationEpoch = 0L
    private var statusPollingJob: Job? = null
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
        onFailure = { playbackNotice = it },
    )

    private val listener = object : Player.Listener {
        override fun onPlayerError(error: PlaybackException) {
            if (!playbackControlBootstrapFence.isActive()) return
            // The departing item's failure does not own a newer requested
            // recipe (or seek create). That create and its target deadline
            // still own success/failure; failover must not steal the request.
            if (stallGuard.defersPredecessorRecovery(recipeOwnership.needsMediaReplacement(currentRecipe()))) return
            val mediaCompatibilityFailure = isCompatibilityPlaybackError(error.errorCode)
            // Only a transport failure can be answered by another node. A
            // terminal answer — an ended session's 404, a refused
            // credential — is the same on every ingress, and walking the list
            // for one costs a full player prepare per node before the viewer
            // sees the error they were always going to see.
            reportControlEvidence(
                ClientObservation(
                    decoderState = DecoderState.FAILED,
                    errorCode = controlErrorCode(error.errorCode),
                    errorDetail = error.errorCodeName,
                ),
                render = RenderState.FAILED,
            )
            if (isTransportPlaybackError(error.errorCode) && retryMediaOnNextNode(error)) return
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
                        onError(
                            armed.message
                                ?: error.errorCodeName.let { "Playback stopped ($it)." },
                        )
                        return
                    }
                    val position = realPosition()
                    sameHdrRetryUsed = true
                    restartAt(position, "fallback")
                }
                PlaybackErrorAction.RetryAsDolbyVisionRemux -> {
                    val position = realPosition()
                    compatibilityRemuxUsed = true
                    forceCompatibilityRemux = true
                    subtitleDelivery =
                        routeSubtitle(trackFor(selectedSubtitle), subtitleDelivery).delivery
                    restartAt(position, "fallback")
                }
                PlaybackErrorAction.RetryAsCompatibilityTranscode -> {
                    // Read the position before the mode moves: which timeline
                    // the player is on depends on the delivery about to change.
                    val position = realPosition()
                    compatibilityTranscodeUsed = true
                    forceCompatibilityTranscode = true
                    // `planMode` is a transcode now, and that can move the
                    // selection's route with it: an embedded track on a
                    // directly-played file has to become a rendition.
                    subtitleDelivery =
                        routeSubtitle(trackFor(selectedSubtitle), subtitleDelivery).delivery
                    restartAt(position, "fallback")
                }
                PlaybackErrorAction.Fail -> onError(
                    // A verdict says production stopped for a reason retrying
                    // cannot change. A dropped link is a different cause with
                    // a different answer, so a transport failure keeps the
                    // client's own words rather than borrowing the server's.
                    (if (isTransportPlaybackError(error.errorCode)) null
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
            playbackNotice = null
            // A commit outstanding here is the successor's very first frame on
            // the surface, which is exactly what `first_frame_unix_ms` means.
            settleCommitOnFirstFrame(System.currentTimeMillis())
        }

        override fun onTracksChanged(tracks: Tracks) {
            applyTextSelection()
            applyAudioSelection()
            completeRecipeExecution()
        }

        override fun onPositionDiscontinuity(
            oldPosition: Player.PositionInfo,
            newPosition: Player.PositionInfo,
            reason: Int,
        ) {
            pgsOverlay.reconcile()
        }

        override fun onPlaybackParametersChanged(playbackParameters: PlaybackParameters) {
            settleAudioPlaybackIntentIfPresented()
            pgsOverlay.reconcile()
        }

        override fun onIsPlayingChanged(isPlaying: Boolean) {
            settleAudioPlaybackIntentIfPresented()
            if (isPlaying && plan.videoCodec == null) {
                establishedPlayback = true
                openStallTracker.reset()
            }
            pgsOverlay.reconcile()
        }

        override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
            if (!playbackControlBootstrapFence.isActive()) return
            // Includes MediaSession transport controls. Internal replacement
            // writes always apply this same latest value; transient buffering
            // and audio-focus suppression do not replace viewer intent.
            if (reason == Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST) {
                playbackIntent.setPlaybackRequested(playWhenReady)
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
        val captured = object : Player.Listener {
            override fun onRenderedFirstFrame() {
                if (!playbackIntent.isCurrent(sequence)) return
                if (recipeExecution?.sequence == sequence) {
                    recipePresentationFrame = sequence to realPosition()
                    completeRecipeExecution()
                    return
                }
                val recipe = selectionRecipe ?: return
                if (!recipeOwnership.canPresent(recipe)) return
                if (!playbackIntent.presentedVideoFrame(realPosition(), sequence)) return
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
                ?.let { playbackIntent.presentedVideoFrame(it.second, sequence) } == true
        }
        if (presented) {
            playbackControl.playerChanged()
            disarmVideoPresentation()
        }
    }

    init {
        player.playWhenReady = playbackIntent.playbackRequested
        player.addListener(listener)
        pgsOverlay.select(selectedSubtitle.takeIf { subtitleDelivery == SubtitleDelivery.BitmapOverlay })
        targetPresentationWatchdogJob = scope.launch {
            while (isActive) {
                sampleTargetPresentationDeadline()
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
                val openStall = openStallTracker.sample(
                    playbackRequested = player.playWhenReady && presentationForeground &&
                        playbackIntent.pendingSeek == null,
                    playbackEnded = player.playbackState == Player.STATE_ENDED,
                    establishedPlayback = establishedPlayback,
                    positionMs = realPosition(),
                    observedAtMs = observedAtMs,
                )
                if (openStall != null) {
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
        restartAt(position, reason, observedAtMs)
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

    /** The viewer's newest film target, even while the predecessor still renders. */
    fun positionForPlaybackIntent(): Long =
        playbackIntent.positionForPlaybackIntent(realPosition())

    /** An explicit Retry is a new viewer command, unlike an automatic reopen. */
    fun prepareViewerRetry(): Long {
        val target = positionForPlaybackIntent()
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
        establishedPlayback = false
        openStallTracker.reset()
        return playbackTelemetry.begin(reason, observedAtMs)
    }

    fun seekTo(targetMs: Long) {
        val t = targetMs.coerceIn(0, if (plan.durationMs > 0) plan.durationMs else Long.MAX_VALUE)
        enqueueSeek { playbackIntent.beginSeek(t, realPosition()) }
    }

    /** Repeated transport nudges accumulate while the newest seek is coalescing. */
    fun seekBy(deltaMs: Long) {
        enqueueSeek {
            playbackIntent.beginRelativeSeek(deltaMs, realPosition(), plan.durationMs)
        }
    }

    private fun enqueueSeek(begin: () -> PlaybackIntent.PendingSeek) {
        if (!playbackControlBootstrapFence.isActive()) return
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
                    !playbackIntent.isCurrent(pending.sequence)
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
        if (planReplacement.route(playbackIntent)) return
        val recipe = currentRecipe()
        if (recipeOwnership.needsMediaReplacement(recipe)) {
            restartAt(t, "selection")
            return
        }
        armTrackSelections(recipe)
        when {
            directTransport -> {
                beginPlaybackAttempt("seek")
                player.seekTo(t)
                markIntentExecuted(sequence)
            }
            subtitleDelivery.usesPlanTransport && planMode == "remux" -> {
                val attempt = beginPlaybackAttempt("seek")
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
                player.playWhenReady = playbackIntent.playbackRequested
                armTrackSelections()
            }
            // A cached session holds the whole stream: native seeking, no
            // session churn. A live one can't be range-sought, so it reopens.
            sessionIsVod -> {
                beginPlaybackAttempt("seek")
                player.seekTo(t)
                markIntentExecuted(sequence)
            }
            else -> {
                val attempt = beginPlaybackAttempt("seek")
                openSession(t, attempt, sequence)
            }
        }
    }

    /** Publish a screen-owned replacement before Compose disposes this controller. */
    fun prepareReplacement(
        positionMs: Long,
        quality: PlaybackQuality,
        onPrepared: (Long, PlaybackQuality) -> Unit,
    ) {
        if (!playbackControlBootstrapFence.isActive()) return
        planReplacement.retain(onPrepared)
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        val pending = playbackIntent.beginSeek(
            playbackIntent.positionForPlaybackIntent(positionMs),
            realPosition(),
            quality,
        )
        sampleTargetPresentationDeadline()
        val publicationEpoch = mediaMutationEpoch
        scope.launch {
            val current = publishIntent(pending, quality, publicationEpoch)
            if (current) planReplacement.route(playbackIntent, force = true)
        }
    }

    fun playPause() {
        if (!playbackControlBootstrapFence.isActive()) return
        playbackControl.clearVerdict()
        stallGuard.setPlaybackRequested(playbackIntent, !playbackIntent.playbackRequested) {
            player.playWhenReady = it
        }
        playbackControl.playerChanged()
    }

    fun release() {
        if (!playbackControlBootstrapFence.isActive()) return
        playbackControlBootstrapFence.release()
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
        val settling = settlingSnapshotIfOwed()
        val endingSession = sessionId
        // Every read below this line is against a player that is about to be
        // released, so the observation is closed first.
        controlObservationIsClosed = true
        preparedRollbackReopen = null
        collectRetiredPlayer()
        endPlaybackControl(settling) { endingSession?.let(vm::endHlsSession) }
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

        player.removeListener(listener)
        disarmVideoPresentation()
        mediaSession.release()
        player.release()
    }

    fun switchAudio(index: Long) {
        if (!playbackControlBootstrapFence.isActive()) return
        if (index == selectedAudio) return
        val position = positionForPlaybackIntent()
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        selectedAudio = index
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
            playbackNotice = HDR_SUBTITLE_NOTICE
            return false
        }
        playbackNotice = null
        // Keep an executed seek's optimistic film target even while the
        // predecessor item still exposes its old clock.
        val position = positionForPlaybackIntent()
        val inheritedSeek = playbackIntent.needsSeekForInPlaceSelection()
        val route = routeSubtitle(track, subtitleDelivery)
        stallGuard.invalidateForUserAction()
        playbackControl.clearVerdict()
        selectedSubtitle = index
        subtitleDelivery = route.delivery
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

    fun clearPlaybackNotice() {
        playbackNotice = null
    }

    fun allowsPictureInPictureCommand(): Boolean {
        if (!pgsOverlayIsActive) return true
        playbackNotice = PGS_OVERLAY_PIP_NOTICE
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
        when {
            !subtitleDelivery.usesPlanTransport ->
                openSession(positionMs, attempt, executionSequence)
            planMode == "direct" -> {
                leaveSessionPlayback()
                activeMediaPath = relativeMediaPath(plan.playUrl)
                player.setMediaItem(MediaItem.fromUri(plan.playUrl), positionMs)
                attachRecipe(recipe)
                executionSequence?.let { sequence ->
                    markIntentExecuted(sequence, recipe)
                }
                player.prepare()
                playbackTelemetry.prepared(attempt)
                player.playWhenReady = playbackIntent.playbackRequested
                armTrackSelections()
            }
            planMode == "remux" -> {
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
                player.playWhenReady = playbackIntent.playbackRequested
                armTrackSelections()
            }
            else -> openSession(positionMs, attempt, executionSequence)
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
        endPlaybackControl { endingSession?.let(vm::endHlsSession) }
        sessionId = null
        clearStatusPolling()
        encoder = null
        sessionIsVod = false
        scope.launch {
            try {
                val hls = try {
                    sessionCreateCoordinator.create(
                        body = createBody.copy(control_sequence = playbackIntent.orderedControlSequence(playbackControl.controlSequence())),
                        isCurrent = { stallGuard.isCurrent(requestVersion) },
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
                            code = (error as? HttpException)?.code(),
                            detail = redactedFailureDetail("session_create", error),
                            attempt = attempt,
                        )
                        playbackTelemetry.cancel(attempt)
                        Log.w(
                            "PlurxPlayback",
                            "session create failed ${redactedFailureDetail("session_create", error)}",
                        )
                        onError("The server couldn't start this stream.")
                    }
                    return@launch
                }
                // A later seek or track switch won while this request was in
                // flight. Release this now-stale server session instead of letting
                // its older timeline replace the current one.
                sessionCreateCoordinator.attachIfCurrent(hls, { stallGuard.isCurrent(requestVersion) }) { hls ->
                    sessionId = hls.session_id
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
                    val timeline = sessionPlaybackTimeline(hls, requestedStartMs = ms)
                    baseMs = timeline.baseMs
                    activeMediaPath = relativeMediaPath(hls.playlist_url)
                    player.setMediaItem(
                        MediaItem.fromUri(Session.url(hls.playlist_url)),
                        timeline.attachPositionMs,
                    )
                    attachRecipe(recipe)
                    executionSequence?.let { sequence ->
                        markIntentExecuted(sequence, recipe)
                    }
                    player.prepare()
                    playbackTelemetry.prepared(attempt)
                    player.playWhenReady = playbackIntent.playbackRequested
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
            // Ruling D1: the verdict is armed, not executed. This player is
            // stalled with nothing left to render, so the only thing the
            // verdict changes is whose words the viewer reads.
            onError(verdict.message ?: "Playback stopped.")
            true
        }
        "hold", "retry_resource" -> {
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
                playbackNotice = if (verdict.type == "hold") {
                    "Waiting briefly for the server. Your place is saved."
                } else {
                    "The server asked playback to retry shortly. Your place is saved."
                }
                true
            }
        }
        else -> false
    }

    /**
     * Called when a detected stall measurement is available. Reopens with the
     * stall-specific fields (previous_session_id, reopen_reason) and enforces
     * the client-side retry budget: the budget counts consecutive reopen
     * responses at the same resolved rung (two or more stalls at 1080 that
     * the server answers with 1080 each time).  A multi-rung downgrade —
     * 2160 → 1080 → 720 — resets the count at each step, so it is never
     * stopped early.  Once the budget is exhausted at the ladder floor the
     * session stays on that rung without further reopen attempts.
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
                        ClientObservation(decoderState = DecoderState.STARVED),
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
        if (verdict != null && applyStallVerdict(verdict, event)) return
        if (nativeReevaluation && openStallTracker.defer(monotonicNowMs())) return
        // A reopen is a new recovery episode. If the replacement freezes at
        // the same playhead, it must receive its own bounded deadline rather
        // than inheriting the fired latch from the item it replaced.
        openStallTracker.reset()
        if (session == null) {
            if (sessionlessStallRecoveryUsed) {
                onError("Playback stopped responding after retrying this stream.")
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
            onError("Playback stopped responding after exhausting recovery attempts.")
            return
        }
        val reason = "stall"
        val attempt = beginPlaybackAttempt(reason, observedAtMs)
        val recipe = currentRecipe()
        val requestVersion = stallGuard.beginRequest()
        // Use the stall-specific session body that carries the predecessor
        // info. `sessionBody` is also called for seeks and track switches;
        // those paths must NOT carry stall fields.
        val prevId = sessionId
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
        endPlaybackControl()
        sessionId = null
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
                        copyableVideo = recipe.recipe.mode != "transcode",
                        aac = plan.aac,
                        preserveDolbyVision = plan.preserveDolbyVision,
                        audioIndex = recipe.recipe.audioIndex,
                        audioOffsetMs = recipe.recipe.audioOffsetMs,
                        quality = recipe.recipe.quality,
                        sourceHeight = plan.sourceHeight,
                        deliveredDynamicRange = deliveredRange,
                        previousSessionId = prevId,
                        reopenReason = ReopenReason.Stall,
                    ),
                    caps = decisionCaps,
                    requestHDR10 = sessionHDR10Request(
                        decisionMode = plan.mode,
                        deliveredDynamicRange = plan.deliveredDynamicRange,
                        compatibilityTranscode = recipe.recipe.compatibilityTranscode,
                        delivery = recipe.recipe.subtitleDelivery,
                    ),
                )
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
                            code = (error as? HttpException)?.code(),
                            detail = redactedFailureDetail("session_reopen", error),
                            attempt = attempt,
                        )
                        playbackTelemetry.cancel(attempt)
                        Log.w(
                            "PlurxPlayback",
                            "session reopen failed ${redactedFailureDetail("session_reopen", error)}",
                        )
                        onError(
                            playbackControl.terminalVerdict?.message
                                ?: "The stream stalled and recovery failed.",
                        )
                    }
                    return@launch
                }
                sessionCreateCoordinator.attachIfCurrent(hls, { stallGuard.isCurrent(requestVersion) }) { hls ->
                    // Update the same-rung budget: the budget counts consecutive
                    // reopen responses that do NOT resolve a strictly lower rung than
                    // the predecessor (same rung, absent/zero height, or a higher
                    // rung).  A genuine strict downgrade resets the count.
                    // Absent or zero height counts as no step down — it is the server
                    // saying "this session is already at its answer" without a rung
                    // the client can compare.
                    stallReopenBudget.record(hls.height)
                    sessionId = hls.session_id
                    beginPlaybackControl(hls)
                    startStatusPolling(hls.session_id)
                    encoder = hls.encoder
                    sessionIsVod = hls.vod
                    adoptSessionDelivery(hls)
                    val timeline = sessionPlaybackTimeline(hls, requestedStartMs = positionMs)
                    baseMs = timeline.baseMs
                    activeMediaPath = relativeMediaPath(hls.playlist_url)
                    player.setMediaItem(
                        MediaItem.fromUri(Session.url(hls.playlist_url)),
                        timeline.attachPositionMs,
                    )
                    attachRecipe(recipe)
                    player.prepare()
                    playbackTelemetry.prepared(attempt)
                    player.playWhenReady = playbackIntent.playbackRequested
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
        body = subtitleSessionBody(
            playbackId = playbackIntent.playbackId,
            requestId = UUID.randomUUID().toString(),
            controlSequence = controlSequence,
            startSeconds = ms / 1000.0,
            delivery = recipe.subtitleDelivery,
            subtitleIndex = recipe.subtitleIndex,
            // A transcode verdict is the only one that forbids copying the video;
            // direct and remux verdicts both mean the source stream is playable
            // as-is, which is what makes the native-rendition session free. The
            // compatibility rescue turns `planMode` into a transcode precisely so
            // it lands here — the copy is the thing the device just refused.
            copyableVideo = recipe.mode != "transcode",
            aac = plan.aac,
            preserveDolbyVision = plan.preserveDolbyVision,
            audioIndex = recipe.audioIndex,
            audioOffsetMs = recipe.audioOffsetMs,
            quality = recipe.quality,
            sourceHeight = plan.sourceHeight,
            deliveredDynamicRange = deliveredRange,
        ),
        caps = decisionCaps,
        requestHDR10 = sessionHDR10Request(
            decisionMode = plan.mode,
            deliveredDynamicRange = plan.deliveredDynamicRange,
            compatibilityTranscode = recipe.compatibilityTranscode,
            delivery = recipe.subtitleDelivery,
        ),
    )

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
        endPlaybackControl { endingSession?.let(vm::endHlsSession) }
        sessionId = null
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
     * the browser and avoids a useful panel vanishing during teardown.
     */
    private fun startStatusPolling(polledSessionId: String) {
        statusPollingJob?.cancel()
        sessionStatus = null
        sessionStatusObservedAtMs = null
        statusPollingJob = scope.launch {
            while (isActive && sessionId == polledSessionId) {
                try {
                    val observed = vm.hlsSessionStatus(polledSessionId)
                    if (!isActive || sessionId != polledSessionId) return@launch
                    sessionStatus = observed
                    sessionStatusObservedAtMs = monotonicNowMs()
                } catch (_: Exception) {
                    // Keep the last real sample. A completed session may
                    // disappear before the player finishes its buffered tail.
                }
                delay(2_000)
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
        val path = activeMediaPath ?: return false
        val next = Session.nextMediaFailoverUrl(path) ?: return false
        val recipe = recipeOwnership.attached ?: currentRecipe()
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
            detail = "delivery=$deliveryMode compatibility_ladder=false",
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
        attachRecipe(recipe)
        presentationSequence?.let { sequence ->
            markIntentExecuted(sequence, recipe)
        }
        player.prepare()
        player.playWhenReady = playbackIntent.playbackRequested
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
                onSubtitleReady = ::retryNativeSubtitleAfterReadiness,
                onPrepare = ::onPrepareAction,
                onAcknowledged = ::acknowledgementDelivered,
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
     */
    private fun retryNativeSubtitleAfterReadiness() {
        val index = selectedSubtitle ?: return
        if (subtitleDelivery != SubtitleDelivery.NativeSession) return
        val session = sessionId ?: return
        val claim = playbackControlBootstrapFence.snapshot(session)
        val intentGeneration = playbackIntent.generation()
        if (!playbackControlBootstrapFence.isCurrent(claim, sessionId)) return
        textSelectionArmed = false
        player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
            .clearOverridesOfType(C.TRACK_TYPE_TEXT)
            .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
            .build()
        scope.launch {
            kotlinx.coroutines.yield()
            if (
                playbackControlBootstrapFence.isCurrent(claim, sessionId) &&
                playbackIntent.generation() == intentGeneration &&
                selectedSubtitle == index &&
                subtitleDelivery == SubtitleDelivery.NativeSession
            ) {
                textSelectionArmed = true
                applyTextSelection()
            }
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
        val capabilities = deviceControlCapabilities ?: return null
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
            quality = when (val requested = playbackIntent.desiredQuality) {
                PlaybackQuality.Auto -> QualitySelection.Auto
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
    private fun playbackControlPlayerChanged() {
        refreshControlWaiting()
        expireControlEvidenceIfProgressed()
        pollPreparedReplacement()
        playbackControl.playerChanged()
    }

    /** Audio-only playback has no video-frame callback; an advancing active clock is presentation. */
    private fun settleAudioPlaybackIntentIfPresented() {
        if (plan.videoCodec != null) return
        if (playbackIntent.presentedAudio(
                realPosition(),
                observedAtMs = monotonicNowMs(),
                playbackActive = player.isPlaying,
                playbackRate = player.playbackParameters.speed.toDouble(),
                presentationReady = selectionRecipe?.let(recipeOwnership::canPresent) == true &&
                    !textSelectionArmed && !audioSelectionArmed,
            )
        ) playbackControl.playerChanged()
    }

    /** STARTED includes visible PiP; stopped/background activities suspend this deadline. */
    fun setPresentationForeground(foreground: Boolean) {
        if (!playbackControlBootstrapFence.isActive()) return
        if (presentationForeground != foreground) stallGuard.invalidateObservation()
        presentationForeground = foreground
        sampleTargetPresentationDeadline()
    }

    private fun sampleTargetPresentationDeadline() {
        if (!playbackControlBootstrapFence.isActive()) return
        val now = monotonicNowMs()
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
        stallGuard.invalidateForPlaybackAttempt()
        reportControlEvidence(
            ClientObservation(decoderState = DecoderState.STARVED),
            render = RenderState.STALLED,
        )
        playbackTelemetry.report(
            event = "playback_target_timeout",
            level = "warn",
            message = "The requested playback destination did not present.",
            detail = "target_ms=${event.targetMs} terminal=${event.terminal}",
        )
        if (event.terminal) {
            onError("Playback couldn't reach the requested position after retrying. Your place is saved.")
        } else if (targetPresentationDeadline.recover(event, now, expectedOwner = targetPresentationOwner)) {
            restartAt(event.targetMs, "presentation-recovery", now)
        }
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

    private val preparedLedger = PreparedReplacementLedger()
    private var preparedPlayer: ExoPlayer? = null
    /** Film position used for the readiness seek; commit samples again. */
    private var preparedAlignedFilmMs: Long? = null
    /** A final seek is asynchronous; the incumbent stays frozen until a later
     * poll proves the successor landed with fresh runway. */
    private var preparedCommitAlignmentFilmMs: Long? = null
    private var preparedCommitAlignmentObserved = false
    private var preparedOrigin: ProgressiveMediaOrigin? = null
    private var preparedListener: Player.Listener? = null
    private var preparedStartedAtMs = 0L

    /**
     * When the switch happened, while the commit still waits for the frame that
     * proves it. Null when no commit is outstanding.
     */
    private var awaitingCommitFrameSinceMs: Long? = null

    /** Everything overwritten when a prepared successor becomes incumbent. */
    private data class PreparedPredecessor(
        val player: ExoPlayer,
        val filmPositionMs: Long,
        val progressiveMediaOrigin: ProgressiveMediaOrigin,
        val baseMs: Long,
        val sessionId: String?,
        val activeMediaPath: String?,
        val deliveredRange: String?,
        val deliveredDolbyVisionProfile: Int?,
        val encoder: String?,
        val sessionStatus: PlaybackSessionStatus?,
        val sessionStatusObservedAtMs: Long?,
        val establishedPlayback: Boolean,
        val playWhenReady: Boolean,
        val playbackParameters: PlaybackParameters,
        val volume: Float,
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
        if (awaitingCommitFrameSinceMs != null && preparedPredecessor?.player === retired) {
            // The surface moved, but the successor has not proved a frame. A
            // timeout must still be able to put this exact pipeline back.
            return
        }
        retiredPlayer = null
        retiredParkedAtMs = 0L
        if (preparedPredecessor?.player === retired) preparedPredecessor = null
        retired.release()
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
                releaseSuccessor()
                startSuccessor(offer.action)
            }
        }
    }

    private fun startSuccessor(action: ControlAction) {
        val playlist = action.playlistUrl ?: return
        val originMs = action.mediaOriginMs ?: return
        val built = try {
            buildSuccessorPlayer(context, vm)
        } catch (_: Exception) {
            // A device that cannot stand up a second pipeline at all is the
            // measured Google TV case. It is a `failed`, not a crash.
            publishAcknowledgement(preparedLedger.failed())
            return
        }
        preparedStartedAtMs = monotonicNowMs()
        preparedPlayer = built.player
        preparedAlignedFilmMs = null
        preparedCommitAlignmentFilmMs = null
        preparedCommitAlignmentObserved = false
        preparedOrigin = built.progressiveMediaOrigin
        val successorListener = object : Player.Listener {
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
                    preparedCommitAlignmentFilmMs != null
                ) {
                    preparedCommitAlignmentObserved = true
                }
            }
        }
        preparedListener = successorListener
        built.player.addListener(successorListener)
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
        awaitingCommitFrameSinceMs?.let { since ->
            if (monotonicNowMs() - since > PREPARED_COMMIT_FRAME_BOUND_MS) {
                val restored = rollbackSwitchedReplacement()
                failSwitchedReplacement()
                if (!restored) {
                    restartAt(realPosition(), "prepared successor rendered no frame")
                }
                return
            }
        }
        val successor = preparedPlayer ?: return
        if (!preparedLedger.isLive) return
        if (monotonicNowMs() - preparedStartedAtMs > PREPARED_READINESS_BOUND_MS) {
            abandonPreparedReplacement(failed = true)
            return
        }
        if (successor.playbackState != Player.STATE_READY) return
        // Tracks known is what separates "playable" from "prepared": a player
        // at STATE_READY with no published tracks has nothing to switch to.
        if (successor.currentTracks.groups.isEmpty()) return
        publishAcknowledgement(preparedLedger.metadataReady())
        val originMs = preparedLedger.action?.mediaOriginMs ?: return
        val incumbentFilmMs = realPosition()
        if (preparedAlignedFilmMs == null) {
            preparedAlignedFilmMs = incumbentFilmMs
            successor.seekTo(successorAttachPositionMs(originMs, incumbentFilmMs))
            return
        }
        val bufferedThrough = successorFilmPositionMs(originMs, successor.bufferedPosition)
        if (!successorIsBuffered(bufferedThrough, incumbentFilmMs)) return
        val successorFilmMs = successorFilmPositionMs(originMs, successor.currentPosition)
        val pendingAlignment = preparedCommitAlignmentFilmMs
        if (pendingAlignment != null) {
            // `seekTo` only requests alignment. A later tick must observe the
            // completed position and re-check runway against the incumbent,
            // which remains the surface owner while frozen on this frame.
            if (!preparedCommitAlignmentObserved) return
            if (kotlin.math.abs(incumbentFilmMs - pendingAlignment) > PREPARED_ALIGNMENT_SLACK_MS ||
                kotlin.math.abs(successorFilmMs - incumbentFilmMs) > PREPARED_ALIGNMENT_SLACK_MS
            ) {
                preparedCommitAlignmentFilmMs = incumbentFilmMs
                preparedCommitAlignmentObserved = false
                player.playWhenReady = false
                successor.seekTo(successorAttachPositionMs(originMs, incumbentFilmMs))
                return
            }
        } else if (kotlin.math.abs(successorFilmMs - incumbentFilmMs) > PREPARED_ALIGNMENT_SLACK_MS) {
            preparedCommitAlignmentFilmMs = incumbentFilmMs
            preparedCommitAlignmentObserved = false
            // Freeze only transport execution, not `playbackIntent`; a user
            // request during alignment remains the intent copied at commit.
            player.playWhenReady = false
            successor.seekTo(successorAttachPositionMs(originMs, incumbentFilmMs))
            return
        }
        publishAcknowledgement(preparedLedger.bufferReady(bufferedThrough))
        commitPreparedReplacement(incumbentFilmMs)
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
    private fun commitPreparedReplacement(commitFilmMs: Long) {
        val successor = preparedPlayer ?: return
        val action = preparedLedger.action ?: return
        val originMs = action.mediaOriginMs ?: return
        val successorFilmMs = successorFilmPositionMs(originMs, successor.currentPosition)
        val bufferedThrough = successorFilmPositionMs(originMs, successor.bufferedPosition)
        if (kotlin.math.abs(successorFilmMs - commitFilmMs) > PREPARED_ALIGNMENT_SLACK_MS ||
            !successorIsBuffered(bufferedThrough, commitFilmMs)
        ) return
        // The ledger enters its unabortable state before anything moves. From
        // here the viewer is looking at this pipeline, so a Back press or a
        // seek in the seconds before its first frame must settle the commit
        // rather than publish an `aborted` for the staging the server is about
        // to move its pointer to.
        if (!preparedLedger.switched()) return
        // Armed here rather than at the end of this function. `switched()` is
        // what makes an abandon settle instead of abort, and the settle is
        // gated on this clock — so any window where one is set and the other is
        // not is a window where a preparation can be left permanently
        // unsettleable.
        awaitingCommitFrameSinceMs = monotonicNowMs()
        val previous = player
        val previousVolume = previous.volume
        val previousPlayWhenReady = playbackIntent.playbackRequested
        val previousPlaybackParameters = previous.playbackParameters
        val predecessor = PreparedPredecessor(
            player = previous,
            filmPositionMs = commitFilmMs,
            progressiveMediaOrigin = progressiveMediaOrigin,
            baseMs = baseMs,
            sessionId = sessionId,
            activeMediaPath = activeMediaPath,
            deliveredRange = deliveredRange,
            deliveredDolbyVisionProfile = deliveredDolbyVisionProfile,
            encoder = encoder,
            sessionStatus = sessionStatus,
            sessionStatusObservedAtMs = sessionStatusObservedAtMs,
            establishedPlayback = establishedPlayback,
            playWhenReady = previousPlayWhenReady,
            playbackParameters = previousPlaybackParameters,
            volume = previousVolume,
        )

        successor.volume = previousVolume
        successor.playbackParameters = previousPlaybackParameters
        successor.playWhenReady = previousPlayWhenReady
        preparedOrigin?.let { progressiveMediaOrigin = it }

        preparedListener?.let { successor.removeListener(it) }
        preparedListener = null
        preparedPlayer = null
        preparedOrigin = null
        preparedCommitAlignmentFilmMs = null
        preparedCommitAlignmentObserved = false

        previous.removeListener(this.listener)
        externalListeners.forEach { previous.removeListener(it) }
        successor.addListener(this.listener)
        externalListeners.forEach { successor.addListener(it) }

        player = successor
        mediaSession.setPlayer(successor)

        // The successor is its own session on its own timeline. Everything the
        // controller derives from "which session am I playing" moves with it,
        // in one place, so no two answers about the same stream can drift.
        activeMediaPath = action.playlistUrl?.let(::relativeMediaPath)
        baseMs = sessionPlaybackTimeline(
            mediaOriginMs = originMs,
            isVod = sessionIsVod,
            requestedStartMs = 0L,
        ).baseMs
        action.sessionId?.let { successorSession ->
            sessionId = successorSession
            startStatusPolling(successorSession)
        }
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
        // The commit is owed a `first_frame_unix_ms`, and the honest source is
        // the successor's own first render — which cannot have happened yet,
        // because a prepared successor has no surface and a surfaceless player
        // renders nothing. So the acknowledgement waits for
        // `onRenderedFirstFrame` on the pipeline that is now the incumbent;
        // [pollPreparedReplacement] fails and takes the ordinary reopen if no
        // frame arrives within [PREPARED_COMMIT_FRAME_BOUND_MS]. Only this
        // callback may claim that a successor actually rendered.
    }

    /**
     * The successor rendered its first frame after taking the surface, so the
     * commit can name the moment the viewer actually saw it.
     */
    private fun settleCommitOnFirstFrame(firstFrameUnixMs: Long) {
        if (awaitingCommitFrameSinceMs == null) return
        awaitingCommitFrameSinceMs = null
        publishAcknowledgement(preparedLedger.committed(firstFrameUnixMs))
        // A rendered successor is now the last known-good picture, so the
        // predecessor is no longer rollback authority and may release.
        preparedPredecessor = null
        collectRetiredPlayer()
    }

    /**
     * Put the last proven player back before publishing failure.
     *
     * The ordinary reopen is deferred until [collectRetiredPlayer] observes
     * the surface move. Starting it here would replace the predecessor's item
     * while the view was still attached to the black successor, losing the
     * exact frame this rollback exists to preserve.
     */
    private fun rollbackSwitchedReplacement(): Boolean {
        val predecessor = preparedPredecessor ?: return false
        if (retiredPlayer !== predecessor.player) return false
        val failedSuccessor = player
        val reopenAt = predecessor.filmPositionMs

        failedSuccessor.removeListener(this.listener)
        externalListeners.forEach { failedSuccessor.removeListener(it) }
        predecessor.player.addListener(this.listener)
        externalListeners.forEach { predecessor.player.addListener(it) }

        progressiveMediaOrigin = predecessor.progressiveMediaOrigin
        baseMs = predecessor.baseMs
        sessionId = predecessor.sessionId
        activeMediaPath = predecessor.activeMediaPath
        deliveredRange = predecessor.deliveredRange
        deliveredDolbyVisionProfile = predecessor.deliveredDolbyVisionProfile
        encoder = predecessor.encoder
        establishedPlayback = predecessor.establishedPlayback
        predecessor.sessionId?.let(::startStatusPolling) ?: clearStatusPolling()
        sessionStatus = predecessor.sessionStatus
        sessionStatusObservedAtMs = predecessor.sessionStatusObservedAtMs
        predecessor.player.volume = predecessor.volume
        predecessor.player.playbackParameters = predecessor.playbackParameters
        predecessor.player.playWhenReady = predecessor.playWhenReady

        preparedPredecessor = null
        preparedRollbackReopen = reopenAt to "prepared successor rendered no frame"
        retiredPlayer = failedSuccessor
        retiredParkedAtMs = monotonicNowMs()
        player = predecessor.player
        mediaSession.setPlayer(predecessor.player)
        return true
    }

    /** Settle a switched-but-black successor without inventing a frame. */
    private fun failSwitchedReplacement() {
        if (awaitingCommitFrameSinceMs == null) return
        awaitingCommitFrameSinceMs = null
        publishAcknowledgement(preparedLedger.failedAfterSwitch())
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
    }

    /**
     * Every exit releases the successor: disposal, session end, a seek, another
     * quality change, backgrounding. A prepared player that outlives its reason
     * is a second decoder the viewer is paying for and cannot see.
     */
    private fun releaseSuccessor() {
        val successor = preparedPlayer ?: return
        val restorePlayback = preparedCommitAlignmentFilmMs != null
        preparedListener?.let { successor.removeListener(it) }
        preparedListener = null
        preparedPlayer = null
        preparedOrigin = null
        preparedAlignedFilmMs = null
        preparedCommitAlignmentFilmMs = null
        preparedCommitAlignmentObserved = false
        successor.release()
        if (restorePlayback) player.playWhenReady = playbackIntent.playbackRequested
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
    val title: String
    val fileId: Long
    val playUrl: String
    val mode: String // "direct" | "remux" | "transcode"
    val durationMs: Long
    val videoCodec: String?
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

@UnstableApi
class BuiltPlayer internal constructor(
    val player: ExoPlayer,
    internal val progressiveMediaOrigin: ProgressiveMediaOrigin,
)

@UnstableApi
fun buildPlayer(context: Context, vm: AppViewModel): BuiltPlayer =
    buildPipeline(context, vm, tunneling = isTelevision(context))

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
fun buildSuccessorPlayer(context: Context, vm: AppViewModel): BuiltPlayer {
    val built = buildPipeline(context, vm, tunneling = isTelevision(context))
    // Never audible and never visible before the switch. Silence is set here
    // rather than relied on from the composition not rendering it: a prepared
    // successor that is merely off-screen is still an audio stream.
    built.player.volume = 0f
    built.player.setVideoSurface(null)
    built.player.playWhenReady = true
    return built
}

@UnstableApi
private fun buildPipeline(context: Context, vm: AppViewModel, tunneling: Boolean): BuiltPlayer {
    val selector = DefaultTrackSelector(context).apply {
        parameters = buildUponParameters()
            .setPreferredAudioLanguage(vm.audioLang)
            // Tunneled playback hands decode and A/V sync to the TV SoC's own
            // pipeline, which is what 4K HDR on a Shield or a Chromecast is
            // built around. Requested only on television devices: on a phone it
            // buys nothing and some handset decoders refuse the mode outright.
            // Media3 falls back to normal playback when the device says no.
            .setTunnelingEnabled(tunneling)
            // Text selection is the server's policy, carried by [Controller] —
            // not the selector's: a preferred language here re-enables the
            // "merely the same language" tail that policy deletes, and the
            // server's own renditions are deliberately DEFAULT=NO/AUTOSELECT=NO
            // so the client must say which.
            .setPreferredTextLanguage(null)
            .setSelectUndeterminedTextLanguage(false)
            .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
            .build()
    }
    val progressiveMediaOrigin = ProgressiveMediaOrigin()
    val dataSource: OkHttpDataSource.Factory = Net.dataSourceFactory()
        .setTransferListener(progressiveMediaOrigin)
    val renderers = DefaultRenderersFactory(context)
        // A flaky hardware decoder degrades to software instead of erroring
        // into the compatibility rescue and costing the viewer a restart.
        .setEnableDecoderFallback(true)
    val player = ExoPlayer.Builder(context)
        .setTrackSelector(selector)
        .setRenderersFactory(renderers)
        .setMediaSourceFactory(DefaultMediaSourceFactory(dataSource))
        // Duck and pause for other apps rather than talking over them, and
        // stop when the headphones come out.
        .setAudioAttributes(
            AudioAttributes.Builder()
                .setUsage(C.USAGE_MEDIA)
                .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                .build(),
            /* handleAudioFocus = */ true,
        )
        .setHandleAudioBecomingNoisy(true)
        .build()
    return BuiltPlayer(player, progressiveMediaOrigin)
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
    // "Burn-in" is a warning about cost, so it follows the question that
    // actually decides cost: can the server serve this as a rendition? ASS/SSA
    // carry text and still burn, which `text` alone would have hidden.
    if (!track.isNativeHls) "Burn-in" else null,
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
