package tv.plurx.app.livetv

import android.content.Context
import android.os.SystemClock
import android.util.AtomicFile
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import tv.plurx.app.data.Net
import java.io.File
import java.security.SecureRandom

/**
 * The one thing this client persists about a start.
 *
 * It is a *hint*, never a capability: an id the owner recognises, and when this
 * client last knew the session behind it was alive. No token, no channel, no
 * tuner URL — guardrail §4.5. Everything it can do is ask the owner a question
 * over the authenticated channel, and the owner answers with the verdict.
 */
@Serializable
internal data class LiveTvStartHint(val request_id: String, val touched_at: Long)

private const val LIVE_TV_HEX = "0123456789abcdef"

/**
 * A fresh start id: 32 lower-case hex characters, which is exactly what the
 * ingress' `parse_public_request_id` accepts. `SecureRandom` rather than
 * `Random` because two viewers colliding on an id would hand one of them the
 * other's session.
 */
internal fun newLiveTvRequestId(random: SecureRandom = SecureRandom()): String {
    val bytes = ByteArray(16)
    random.nextBytes(bytes)
    return buildString(32) {
        bytes.forEach { byte ->
            append(LIVE_TV_HEX[(byte.toInt() shr 4) and 0xf])
            append(LIVE_TV_HEX[byte.toInt() and 0xf])
        }
    }
}

internal fun isLiveTvRequestId(value: String): Boolean =
    value.removePrefix("v4_").let { raw -> raw.length == 32 && raw.all { it in '0'..'9' || it in 'a'..'f' } }

/** How often a touched hint is actually committed to disk. See [LiveTvHintStore.touch]. */
internal const val LIVE_TV_HINT_WRITE_INTERVAL_MS: Long = 60_000L

internal interface LiveTvHintStore {
    /** The hint this process holds, or — in a fresh process — the one on disk. */
    fun read(): LiveTvStartHint?
    fun remember(hint: LiveTvStartHint)

    /**
     * Move `touched_at` forward. In memory always; on disk at most once per
     * [LIVE_TV_HINT_WRITE_INTERVAL_MS].
     *
     * The heartbeat runs every five seconds on the main thread. The barrier
     * this replaced re-armed a durable marker from there, and the 2026-09-05
     * review found the ANR that produced: an fsync of an unchanged byte, on the
     * main thread, twelve times a minute. `touched_at` only ever has to
     * separate a live session from one a killed process left behind, and a
     * minute is far finer than that question needs.
     */
    fun touch(at: Long)
    fun forget()
}

/**
 * The durable half, over the same [AtomicFile] the barrier's marker used.
 *
 * Nothing here refuses anything. A hint that cannot be written costs a start
 * that cannot be retired early, and the owner's 45 s idle reap already covers
 * that; a client that refused to tune because of it would be exactly the
 * behaviour guardrail §4.4 forbids.
 */
internal class LiveTvStartHintStore(context: Context) : LiveTvHintStore {
    private val directory = context.noBackupFilesDir
    private val file = AtomicFile(File(directory, HINT_FILE))
    private var loaded = false
    private var cached: LiveTvStartHint? = null
    private var writtenAt = Long.MIN_VALUE

    init {
        // The 90-second start barrier left a marker file in the same directory.
        // Nothing reads it now, and `noBackupFilesDir` is not somewhere a
        // viewer can reach, so the first run of this build is the only chance
        // anything has to clear it.
        listOf(BARRIER_MARKER, "$BARRIER_MARKER.bak", "$BARRIER_MARKER.new").forEach { name ->
            runCatching { File(directory, name).delete() }
        }
    }

    override fun read(): LiveTvStartHint? {
        if (!loaded) {
            loaded = true
            cached = runCatching {
                Net.json.decodeFromString<LiveTvStartHint>(file.readFully().decodeToString())
            }.getOrNull()?.takeIf { isLiveTvRequestId(it.request_id) }
        }
        return cached
    }

    override fun remember(hint: LiveTvStartHint) {
        loaded = true
        cached = hint
        write(hint)
    }

    override fun touch(at: Long) {
        val hint = read() ?: return
        val touched = hint.copy(touched_at = at)
        cached = touched
        // `at < writtenAt` covers a clock that moved backwards: the alternative
        // is a hint that is never committed again for the life of the process.
        if (at - writtenAt >= LIVE_TV_HINT_WRITE_INTERVAL_MS || at < writtenAt) write(touched)
    }

    override fun forget() {
        loaded = true
        cached = null
        writtenAt = Long.MIN_VALUE
        runCatching { file.delete() }
    }

    private fun write(hint: LiveTvStartHint) {
        val output = runCatching { file.startWrite() }.getOrNull() ?: return
        try {
            output.write(Net.json.encodeToString(hint).encodeToByteArray())
            file.finishWrite(output)
            writtenAt = hint.touched_at
        } catch (_: Exception) {
            runCatching { file.failWrite(output) }
        }
    }

    private companion object {
        const val HINT_FILE = "live-tv-start.hint"
        const val BARRIER_MARKER = "live-tv-start.pending"
    }
}

/**
 * The typed 4xx codes the ingress decides before a request ever reaches an
 * owner. They are the only refusals with no verdict field that are still safe
 * to forget a hint on: no owner saw the request, so no tuner can be open for
 * it.
 *
 * Everything else without `owner_decided: true` — including a code this build
 * has never heard of — leaves a start that may or may not exist, and the hint
 * is the only handle for retiring it.
 */
internal val INGRESS_DECIDED_START_REFUSALS = setOf(
    "invalid_request",
    "admin_required",
    "invalid_settings",
    "live_tv_disabled",
)

/**
 * Whether an answer is one of those refusals *and* arrived the way an ingress
 * refusal arrives.
 *
 * The status is not a formality. These four codes mean "the ingress rejected
 * the request before it reached an owner" only at a 4xx; the same code at a
 * 5xx is a server that got as far as trying, and may have left a tuner open
 * behind it. A missing status is a failure this client minted itself, which is
 * no evidence of an ingress refusal either — so both fall to the safe side and
 * keep the hint.
 */
internal fun isIngressDecidedRefusal(code: String, status: Int?): Boolean =
    code in INGRESS_DECIDED_START_REFUSALS && status != null && status in 400..499

/** A start answer, as the transport saw it. */
internal sealed interface LiveTvStartAnswer {
    /**
     * The server did not answer: a timeout, a dropped connection, an untyped
     * HTTP failure, or a 2xx whose body was not a session. Nothing about it is
     * a refusal to start — the next press repeats, with a fresh id.
     */
    data object NoAnswer : LiveTvStartAnswer

    /**
     * A typed envelope: `code`, plus the optional `retry` and `owner_decided`,
     * and the [status] it arrived with — see [isIngressDecidedRefusal].
     */
    data class Typed(
        val code: String,
        val retry: String? = null,
        val ownerDecided: Boolean? = null,
        val status: Int? = null,
    ) : LiveTvStartAnswer
}

/**
 * What the transport handed back, as an answer. Anything that is not a typed
 * envelope — a timeout, a dropped connection, an untyped body, a 2xx that was
 * not a session — is no answer, and no answer is never a refusal.
 */
internal fun liveTvStartAnswerOf(error: Exception): LiveTvStartAnswer = when {
    error !is LiveTvFailure -> LiveTvStartAnswer.NoAnswer
    error.code == "no_answer" -> LiveTvStartAnswer.NoAnswer
    else -> LiveTvStartAnswer.Typed(error.code, error.retry, error.ownerDecided, error.status)
}

/** What the lease and the screen do with one start answer. */
internal data class LiveTvStartDecision(
    /** The copy key to render. Every one of these has a string in `liveTvMessage`. */
    val render: String,
    val offerRetry: Boolean,
    val keepHint: Boolean,
    val replay: Boolean = false,
)

/** The answer to a resume, as the transport saw it. */
internal sealed interface LiveTvResumeResult {
    data class Answered(val outcome: String, val session: LiveTvStarted? = null) : LiveTvResumeResult
    data object NoAnswer : LiveTvResumeResult
    data class Refused(val answer: LiveTvStartAnswer.Typed) : LiveTvResumeResult
}

internal enum class LiveTvResumeAction { Reattach, ClearHintWait, KeepHintWait }

/** Which halves of protocol 3 the last channels answer said both ends carry. */
internal data class LiveTvProtocolSupport(val requestId: Boolean, val recoveryRoutes: Boolean, val intents: Boolean = false) {
    companion object {
        /**
         * An ingress older than this contract omits the field entirely. It gets
         * no `request_id` and no recovery route — but the hint is still
         * written, because the ingress it talks to tomorrow may carry both.
         */
        val legacy = LiveTvProtocolSupport(requestId = false, recoveryRoutes = false)

        fun from(protocols: List<Int>?): LiveTvProtocolSupport =
            if (protocols != null && 3 in protocols) {
                LiveTvProtocolSupport(requestId = true, recoveryRoutes = true, intents = 4 in protocols)
            } else {
                legacy
            }
    }
}

/**
 * One reducer, three clients. Every rule below is a row in
 * `tests/playback/live-tv-start-cases.json`, which the web and Apple suites
 * read from the same file: if this disagrees with them, one of the clients is
 * lying about what the server said.
 */
internal object LiveTvStartReducer {
    fun decide(answer: LiveTvStartAnswer): LiveTvStartDecision = when (answer) {
        LiveTvStartAnswer.NoAnswer -> LiveTvStartDecision(
            render = "no_answer", offerRetry = true, keepHint = true, replay = true,
        )
        is LiveTvStartAnswer.Typed -> {
            val ingressDecided = isIngressDecidedRefusal(answer.code, answer.status)
            LiveTvStartDecision(
                // A typed refusal minted outside the Live TV module carries no
                // verdict and no copy of its own. `owner_unavailable` is the
                // honest thing to draw: something between here and the tuner
                // said no, and it may never have been the tuner.
                render = if (liveTvKnownMessage(answer.code) != null) answer.code else "owner_unavailable",
                offerRetry = !ingressDecided && answer.retry != "never",
                keepHint = !(answer.ownerDecided == true || ingressDecided),
                replay = false,
            )
        }
    }

    fun resume(result: LiveTvResumeResult): LiveTvResumeAction = when (result) {
        is LiveTvResumeResult.Answered -> when (result.outcome) {
            "live" -> if (result.session != null) {
                LiveTvResumeAction.Reattach
            } else {
                // `live` with nothing to attach to is not an answer about this
                // id at all; keeping the hint costs one more resume.
                LiveTvResumeAction.KeepHintWait
            }
            "ended", "retired" -> LiveTvResumeAction.ClearHintWait
            // `pending`, and any outcome a newer owner adds.
            else -> LiveTvResumeAction.KeepHintWait
        }
        // A resume that did not answer says nothing about the session.
        LiveTvResumeResult.NoAnswer -> LiveTvResumeAction.KeepHintWait
        // A refused resume is the same question a refused start asks — is this
        // id spent? — so it gets the same answer from the same reducer. An
        // owner that refuses has decided the id's fate and the hint is done
        // with; anything the owner did not decide (an ingress refusal, a
        // transport failure, a 404 from an owner too old to know the route)
        // leaves a session that may still exist, and the hint is the only
        // handle for it.
        is LiveTvResumeResult.Refused ->
            if (decide(result.answer).keepHint) {
                LiveTvResumeAction.KeepHintWait
            } else {
                LiveTvResumeAction.ClearHintWait
            }
    }
}

/**
 * Scope belongs to the application, so a caller leaving a screen cannot cancel
 * ownership bookkeeping. Every next start first confirms the old DELETE.
 *
 * The press flow, the open-time resume and the heartbeat touch are §3.16 of
 * `docs/features/LIVE-TV-RELIABILITY-IMPLEMENTATION.md` verbatim. Nothing here
 * ever refuses to start.
 */
internal class LiveTvLease(
    private val requests: LiveTvRequests,
    private val hints: LiveTvHintStore,
    private val scope: CoroutineScope,
    private val now: () -> Long = System::currentTimeMillis,
) {
    private val mutex = Mutex()
    private var generation = 0L
    private var recovery: Job? = null
    private var retirement: Job? = null
    var current: LiveTvStarted? = null
        private set

    fun start(channel: String): Deferred<LiveTvStarted?> {
        val mine = ++generation
        return scope.async {
            mutex.withLock {
                releaseCurrent()
                if (mine != generation) return@withLock null
                retireStaleHint()
                val id = requests.issueIntent(channel, newLiveTvRequestId())
                hints.remember(LiveTvStartHint(id, now()))
                val info = attemptStart(channel, id)
                current = info
                if (mine != generation || !info.live) { releaseCurrent(); null } else info
            }
        }
    }

    /**
     * The open-time step: a hint from a previous run is handed straight back to
     * the owner, and a session the viewer never meant to leave comes back
     * instead of the channel list.
     *
     * "Recent" is the hint's own meaning rather than a number this client
     * invents: a hint only exists while it might still name a session, and the
     * owner's registry — with its 45 s idle reap and its retired-id tombstones
     * — is the one thing that knows whether it does. This never auto-tunes
     * (guardrail §4.6): it rejoins, or it returns null and the list is shown.
     */
    suspend fun resumeIfRecent(): LiveTvStarted? = mutex.withLock {
        if (!requests.recoveryRoutes || current != null) return@withLock null
        val hint = hints.read() ?: return@withLock null
        val result = try {
            val answer = requests.resume(hint.request_id)
            LiveTvResumeResult.Answered(answer.outcome, answer.session)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            when (val answer = liveTvStartAnswerOf(error)) {
                LiveTvStartAnswer.NoAnswer -> LiveTvResumeResult.NoAnswer
                is LiveTvStartAnswer.Typed -> LiveTvResumeResult.Refused(answer)
            }
        }
        when (LiveTvStartReducer.resume(result)) {
            LiveTvResumeAction.Reattach -> {
                val session = checkNotNull((result as LiveTvResumeResult.Answered).session)
                current = session
                if (session.live) {
                    hints.touch(now())
                    session
                } else {
                    // The same rule a fresh start follows: a session that is
                    // not live is one to hand back, not one to draw.
                    runCatching { releaseCurrent() }
                    null
                }
            }
            LiveTvResumeAction.ClearHintWait -> { hints.forget(); null }
            LiveTvResumeAction.KeepHintWait -> null
        }
    }

    fun stop(): Deferred<Unit> {
        ++generation
        return scope.async { mutex.withLock { releaseCurrent() } }
    }

    /**
     * Release only the session an overtaken attach actually started.
     *
     * A newer channel may already have replaced [target] by the time a
     * delayed display-mode wait resumes. Ordinary [stop] would then release
     * the newer channel. Identity-checking under the lease mutex makes stale
     * cleanup exact without invalidating a newer start generation.
     */
    fun stopIfCurrent(target: LiveTvStarted): Deferred<Boolean> = scope.async {
        mutex.withLock {
            if (current !== target) return@withLock false
            releaseCurrent()
            true
        }
    }

    /** Called from the five-second heartbeat. Almost never touches the disk. */
    fun touchHint() { if (current != null) hints.touch(now()) }

    /**
     * A start that got no answer replays once with the same id — the owner
     * joins the replay to the session the first attempt may already have
     * opened, which is the whole reason the id is the client's to mint.
     */
    private suspend fun attemptStart(channel: String, id: String): LiveTvStarted {
        var replays = 0
        while (true) {
            val answer = try {
                val info = requests.start(channel, id)
                if (info.session_id.isEmpty() || info.session_id.length > 1024) {
                    LiveTvStartAnswer.NoAnswer
                } else {
                    return info
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                liveTvStartAnswerOf(error)
            }
            val decision = LiveTvStartReducer.decide(answer)
            if (decision.replay && replays < LiveTvInputPolicy.START_REPLAY_ATTEMPTS) {
                replays += 1
                continue
            }
            if (!decision.keepHint) hints.forget()
            throw LiveTvFailure(decision.render)
        }
    }

    /**
     * Retire whatever the last run left behind, in the background.
     *
     * A hint still on disk here is never the live session's: [releaseCurrent]
     * has already run and forgets on a confirmed DELETE, and this client keeps
     * one hint rather than the web's list of them. So it is either a previous
     * process's — the app was killed with a picture on screen — or one this
     * process's last press left with no answer. Both are orphans.
     *
     * Fire and forget, on the application scope: the press does not wait for
     * it, and a hint that cannot be retired is simply left (guardrail §4.4).
     */
    private fun retireStaleHint() {
        if (!requests.recoveryRoutes) return
        val stale = hints.read() ?: return
        if (retirement?.isActive == true) return
        retirement = scope.launch { runCatching { requests.retire(stale.request_id) } }
    }

    private suspend fun releaseCurrent() {
        val previous = current ?: return
        try {
            requests.release(previous.session_id)
        } catch (error: Exception) {
            // Retaining the capability is right, but nothing else will come
            // back for it: the heartbeat is already cancelled, and on sign-out
            // the screen that offers "Stop / retry cleanup" is gone. Without
            // this the tuner is held until the server's idle timeout.
            scheduleRecovery(previous)
            throw error
        }
        current = null
        hints.forget()
    }

    /**
     * Bounded, self-cancelling cleanup for a capability whose DELETE failed.
     * It is bound to [target] by identity: a newer capability is never the one
     * this recovery was scheduled for, so it can only ever release its own.
     */
    private fun scheduleRecovery(target: LiveTvStarted) {
        if (recovery?.isActive == true) return
        recovery = scope.launch {
            for (backoff in longArrayOf(2_000L, 8_000L, 30_000L)) {
                delay(backoff)
                val settled = mutex.withLock {
                    if (current !== target) return@withLock true
                    try {
                        requests.release(target.session_id)
                        current = null
                        hints.forget()
                        true
                    } catch (_: Exception) {
                        false
                    }
                }
                if (settled) return@launch
            }
        }
    }
}

internal class LiveTvWatchdog(private val now: () -> Long = SystemClock::elapsedRealtime) {
    private var lastProgress = now()
    private var lastFrames = 0
    fun observe(renderedFrames: Int, advancing: Boolean): Boolean {
        // ExoPlayer position is relative to the sliding window and can move
        // backwards while healthy. Only actual rendered video renews a lease.
        // A decoder replacement may reset the counter; one positive new count
        // is progress, but a frozen/repeated count never extends the budget.
        if (advancing && renderedFrames > 0 && renderedFrames != lastFrames) {
            lastFrames = renderedFrames
            lastProgress = now()
            return true
        }
        return false
    }
    val expired: Boolean get() = now() - lastProgress >= 30_000
}
