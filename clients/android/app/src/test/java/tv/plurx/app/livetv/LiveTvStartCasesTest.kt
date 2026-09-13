package tv.plurx.app.livetv

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Net

/**
 * `tests/playback/live-tv-start-cases.json` is the same fixture the web and
 * Apple suites read. Three clients, one reducer: if this file and
 * `tests/web/live-tv.test.js` disagree, one of them is lying about what the
 * server said — and the failure mode of being wrong here is a physical tuner
 * nobody reclaims.
 *
 * The resume rows carry a whole activation, so they are decoded through
 * `LiveTvResumeAnswer` exactly as `LiveTvApi.resume` decodes a real one — a
 * client that got the session's shape wrong fails here rather than in a living
 * room.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class LiveTvStartCasesTest {
    private val cases: JsonObject = Json.parseToJsonElement(
        checkNotNull(javaClass.classLoader?.getResource("live-tv-start-cases.json")) {
            "tests/playback/live-tv-start-cases.json is not on the JVM test classpath"
        }.readText(),
    ).jsonObject

    private val channel = LiveTvChannel("one", "7.1", "Fixture News")

    /**
     * The fixture's row, turned into the exception `LiveTvApi` would actually
     * raise for it — not into a description of one. `transport: "timeout"` is
     * the catch-all in `request`, an HTTP row goes through the same envelope
     * parser the call site uses, and an owner-typed body with no `transport` is
     * a typed non-2xx.
     */
    private fun failureFor(row: JsonObject): LiveTvFailure {
        if (row["transport"]?.jsonPrimitive?.content == "timeout") return LiveTvFailure("no_answer")
        // A row that names no status is an owner-typed answer, which the
        // ingress relays as a 503. That matters since F15: the four ingress
        // codes only count as ingress-decided at a real 4xx, and the two rows
        // here that carry one of them carry `owner_decided: true` as well, so
        // the owner's verdict — not the status — is what settles them.
        val status = row["status"]?.jsonPrimitive?.int ?: 503
        val body = row["body"]?.jsonObject?.toString() ?: ""
        return liveTvTypedFailure(body, status, starting = true)
    }

    @Test
    fun everySharedStartAnswerReducesTheWayTheFixtureSays() {
        val rows = cases.getValue("answers").jsonArray
        assertTrue("the fixture lost its start answers", rows.isNotEmpty())
        rows.forEach { element ->
            val row = element.jsonObject
            val name = row.getValue("case").jsonPrimitive.content
            val decision = LiveTvStartReducer.decide(liveTvStartAnswerOf(failureFor(row)))
            assertEquals(name, row.getValue("render").jsonPrimitive.content, decision.render)
            assertEquals(name, row.getValue("offer_retry").jsonPrimitive.boolean, decision.offerRetry)
            assertEquals(name, row.getValue("keep_hint").jsonPrimitive.boolean, decision.keepHint)
            assertEquals(
                name,
                row["replay"]?.jsonPrimitive?.booleanOrNull ?: false,
                decision.replay,
            )
        }
    }

    @Test
    fun everyRenderKeyTheFixtureNamesHasCopyOfItsOwn() {
        // A render key with no string is a viewer looking at the generic "the
        // live stream could not continue", which is exactly the sentence this
        // whole contract exists to stop showing.
        cases.getValue("answers").jsonArray.forEach { element ->
            val key = element.jsonObject.getValue("render").jsonPrimitive.content
            assertNotNull("no string for the render key $key", liveTvKnownMessage(key))
        }
        assertEquals(
            "The server did not answer. Press the channel again.",
            liveTvMessage("no_answer"),
        )
    }

    @Test
    fun everySharedResumeCaseReducesTheWayTheFixtureSays() {
        val rows = cases.getValue("resume").jsonArray
        assertTrue("the fixture lost its resume cases", rows.isNotEmpty())
        rows.forEach { element ->
            val row = element.jsonObject
            val name = row.getValue("case").jsonPrimitive.content
            val result = if (row["transport"]?.jsonPrimitive?.content == "timeout") {
                LiveTvResumeResult.NoAnswer
            } else {
                // Through the wire model, exactly as `LiveTvApi.resume` decodes
                // it — the fixture's session is a whole activation, so a client
                // that decoded it wrongly would fail here rather than in a
                // living room.
                val answer: LiveTvResumeAnswer =
                    Net.json.decodeFromString(row.getValue("body").jsonObject.toString())
                LiveTvResumeResult.Answered(answer.outcome, answer.session)
            }
            val wanted = when (val then = row.getValue("then").jsonPrimitive.content) {
                "reattach" -> LiveTvResumeAction.Reattach
                "clear_hint_wait" -> LiveTvResumeAction.ClearHintWait
                "keep_hint_wait" -> LiveTvResumeAction.KeepHintWait
                else -> error("$name: the fixture grew a `then` this client cannot answer: $then")
            }
            assertEquals(name, wanted, LiveTvStartReducer.resume(result))
        }
    }

    /**
     * The `reattach` row, decoded rather than described. A resumed session has
     * to carry everything a fresh start's does, because [LiveTvPlayer] takes
     * both down the same path: the capability the playlist URL is built from,
     * the channel to label and highlight, and `live`.
     */
    @Test
    fun theResumedSessionDecodesIntoEverythingPlaybackNeeds() {
        val body = cases.getValue("resume").jsonArray
            .map { it.jsonObject }
            .first { it["then"]?.jsonPrimitive?.content == "reattach" }
            .getValue("body").jsonObject
        val answer: LiveTvResumeAnswer = Net.json.decodeFromString(body.toString())

        assertEquals("live", answer.outcome)
        val session = checkNotNull(answer.session) { "the reattach row must carry a session" }
        assertEquals("ltv1.7de1a4c0.9f2b", session.session_id)
        assertTrue(session.live)
        assertEquals("7.1", session.channel.id)
        assertEquals("WABC", session.channel.guide_name)
        assertTrue("a resumed channel must be watchable", session.channel.watchable)
        assertEquals("7.1 · WABC", session.channel.title)
        // `playlist_url` and `output` are the owner's; this client builds its
        // own playlist URL from the capability and reads the delivery from
        // status, so they decode away rather than breaking the session.
        assertNull(session.delivery)
    }

    @Test
    fun protocolNegotiationAnswersEverySharedChannelsResponse() {
        val rows = cases.getValue("protocols").jsonArray
        assertTrue("the fixture lost its protocol cases", rows.isNotEmpty())
        rows.forEach { element ->
            val row = element.jsonObject
            val name = row.getValue("case").jsonPrimitive.content
            // Through the wire model, not around it: a lineup that decoded
            // `protocols` wrongly would negotiate wrongly.
            val response = row.getValue("channels_response").jsonObject
            val lineup: LiveTvLineup = Net.json.decodeFromString(
                JsonObject(response + ("channels" to Json.parseToJsonElement("[]"))).toString(),
            )
            val support = LiveTvProtocolSupport.from(lineup.protocols)
            assertEquals(name, row.getValue("request_id").jsonPrimitive.boolean, support.requestId)
            assertEquals(
                name,
                row.getValue("recovery_routes").jsonPrimitive.boolean,
                support.recoveryRoutes,
            )
        }
        // An ingress that omits the field is not the same fact as one that
        // publishes an empty list, and neither gets protocol 3.
        assertNull(Net.json.decodeFromString<LiveTvLineup>("""{"channels":[]}""").protocols)
    }

    /**
     * F15. The four ingress codes only mean "no owner ever saw this" when they
     * arrive as a 4xx. The same code at 5xx is a server that got as far as
     * trying, and a start that may have opened a tuner before it failed: that
     * hint is the only handle for retiring it, so it survives.
     */
    @Test
    fun anIngressCodeAtFiveHundredIsNotAnIngressDecision() {
        INGRESS_DECIDED_START_REFUSALS.forEach { code ->
            val fourxx = LiveTvStartReducer.decide(
                LiveTvStartAnswer.Typed(code, status = 400),
            )
            assertFalse("$code at 400 is the ingress' own refusal", fourxx.keepHint)
            assertFalse("$code at 400 offers no retry", fourxx.offerRetry)

            val fivexx = LiveTvStartReducer.decide(
                LiveTvStartAnswer.Typed(code, status = 503),
            )
            assertTrue("$code at 503 may have reached an owner", fivexx.keepHint)

            // A failure this client minted carries no status at all, and is no
            // evidence of an ingress refusal either.
            assertTrue(code, LiveTvStartReducer.decide(LiveTvStartAnswer.Typed(code)).keepHint)
        }
        // The owner's own verdict still settles it at any status.
        assertFalse(
            LiveTvStartReducer.decide(
                LiveTvStartAnswer.Typed("live_tv_disabled", "never", ownerDecided = true, status = 503),
            ).keepHint,
        )
        // And the status is carried off the wire, not invented here.
        assertEquals(503, liveTvTypedFailure("""{"code":"invalid_settings"}""", 503, true).status)
        assertNull(LiveTvFailure("no_answer").status)
    }

    /**
     * F16. A refused resume asks the same question a refused start asks — is
     * this id spent? — so it is answered by the same reducer.
     */
    @Test
    fun aRefusedResumeForgetsTheHintOnlyWhenTheOwnerDecidedIt() {
        // The owner answered and said no. The id is spent.
        assertEquals(
            LiveTvResumeAction.ClearHintWait,
            LiveTvStartReducer.resume(
                LiveTvResumeResult.Refused(
                    LiveTvStartAnswer.Typed("channel_not_found", "never", ownerDecided = true, status = 404),
                ),
            ),
        )
        // The ingress refused the request itself, at a real 4xx.
        assertEquals(
            LiveTvResumeAction.ClearHintWait,
            LiveTvStartReducer.resume(
                LiveTvResumeResult.Refused(LiveTvStartAnswer.Typed("admin_required", status = 403)),
            ),
        )
        // Everything the owner did not decide keeps the handle. The first of
        // these is the deploy blip: `owner_unavailable` is minted when no
        // reachable committed voter is the owner, and it is explicitly NOT
        // owner-decided — a resume during a rolling restart must not throw away
        // the id of a session that is still running.
        listOf(
            LiveTvStartAnswer.Typed("owner_unavailable", "now", ownerDecided = false, status = 503),
            LiveTvStartAnswer.Typed("node_maintenance", status = 503),
            // An owner too old to know the route answers 404, which the ingress
            // turns into a typed, not-owner-decided answer.
            LiveTvStartAnswer.Typed("owner_unavailable", "now", ownerDecided = false, status = 404),
            // An ingress code at 5xx, per F15.
            LiveTvStartAnswer.Typed("invalid_settings", status = 503),
        ).forEach {
            assertEquals(
                "${it.code}/${it.status} must keep the hint",
                LiveTvResumeAction.KeepHintWait,
                LiveTvStartReducer.resume(LiveTvResumeResult.Refused(it)),
            )
        }
    }

    @Test
    fun aResumeRefusedByTheOwnerForgetsTheHintThroughTheLease() = runTest {
        fun scope() = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))

        val spent = Hints(LiveTvStartHint(STRAY, 1_000))
        val decided = Requests().apply {
            resumeError = { LiveTvFailure("channel_not_found", "never", ownerDecided = true, status = 404) }
        }
        assertNull(LiveTvLease(decided, spent, scope()) { 90_000 }.resumeIfRecent())
        assertNull("an owner-decided refusal spends the id", spent.hint)

        val blip = Hints(LiveTvStartHint(STRAY, 1_000))
        val undecided = Requests().apply {
            resumeError = { LiveTvFailure("owner_unavailable", "now", ownerDecided = false, status = 503) }
        }
        assertNull(LiveTvLease(undecided, blip, scope()) { 90_000 }.resumeIfRecent())
        assertNotNull("a deploy blip must not throw the id away", blip.hint)

        val silent = Hints(LiveTvStartHint(STRAY, 1_000))
        val nothing = Requests().apply { resumeError = { LiveTvFailure("no_answer") } }
        assertNull(LiveTvLease(nothing, silent, scope()) { 90_000 }.resumeIfRecent())
        assertNotNull("a resume that did not answer says nothing", silent.hint)
    }

    @Test
    fun aStartIdIsThirtyTwoLowerCaseHexCharacters() {
        repeat(64) {
            val id = newLiveTvRequestId()
            assertEquals(id, 32, id.length)
            assertTrue(id, isLiveTvRequestId(id))
        }
        assertFalse(isLiveTvRequestId("ABCDEF0123456789abcdef0123456789"))
        assertFalse(isLiveTvRequestId("abc"))
        assertEquals(64, List(64) { newLiveTvRequestId() }.toSet().size)
    }

    @Test
    fun theHintIsARequestIdAndATimestampAndNothingElse() {
        val wire = Net.json.encodeToString(
            LiveTvStartHint.serializer(),
            LiveTvStartHint("7de1a4c09f2b4e5d8a1c3f6b9d2e4a70", 1_789_000_800_000),
        )
        assertEquals(
            setOf("request_id", "touched_at"),
            Json.parseToJsonElement(wire).jsonObject.keys,
        )
        // Guardrail §4.5: no capability, token, channel id or tuner URL.
        listOf("session", "capability", "token", "channel", "playlist", "url").forEach {
            assertFalse(wire, wire.contains(it))
        }
    }

    /**
     * The whole point of the hint. A process killed with a picture on screen
     * leaves an id behind; the next press hands it back to the owner so the
     * stray is retired — and the press does not wait a millisecond for that to
     * happen, because a viewer pressing a channel is not asking about the last
     * run.
     */
    @Test
    fun aHintFromAKilledProcessIsRetiredInTheBackgroundAndNeverDelaysThePress() = runTest {
        val store = Hints(LiveTvStartHint(STRAY, 1_000))
        val requests = Requests()
        // The retire is started and then never answers. If the press awaited
        // it, this test would hang rather than fail — which is the honest shape
        // of the regression it guards: a viewer whose channel press is held
        // behind an owner round trip about the *last* run.
        requests.retireBlocks = true
        val scope = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))
        val lease = LiveTvLease(requests, store, scope) { 90_000 }

        val started = lease.start("one").await()

        assertNotNull("the press must produce a session", started)
        assertTrue("the stray id must have been handed back", STRAY in requests.retired)
        assertEquals("and exactly once", 1, requests.retired.size)
        assertFalse("the retire must still be in flight", requests.retireFinished)
        // The press minted its own id and persisted it before the POST.
        val kept = checkNotNull(store.hint)
        assertTrue(isLiveTvRequestId(kept.request_id))
        assertFalse("the stray id must not be reused", kept.request_id == STRAY)
        assertEquals(listOf("start:one:${kept.request_id}"), requests.events)
        scope.cancel()
    }

    @Test
    fun aStartThatGetsNoAnswerReplaysOnceWithTheSameIdAndKeepsItsHint() = runTest {
        val store = Hints(null)
        val requests = Requests().apply { error = { LiveTvFailure("no_answer") } }
        val lease = LiveTvLease(
            requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)),
        ) { 1_000 }

        val failure = runCatching { lease.start("one").await() }.exceptionOrNull()

        assertEquals("no_answer", (failure as LiveTvFailure).code)
        assertEquals(
            "one attempt and exactly one replay",
            1 + LiveTvInputPolicy.START_REPLAY_ATTEMPTS,
            requests.events.size,
        )
        assertEquals("the replay carries the same id", 1, requests.events.toSet().size)
        assertNotNull("the next press needs this handle", store.hint)
    }

    @Test
    fun anOwnerDecidedRefusalForgetsItsHintAndAnUndecidedOneKeepsIt() = runTest {
        suspend fun press(error: LiveTvFailure): Hints {
            val store = Hints(null)
            val requests = Requests().apply { this.error = { error } }
            val scope = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))
            runCatching { LiveTvLease(requests, store, scope) { 1_000 }.start("one").await() }
            return store
        }
        assertNull(
            "the owner said no; nothing is open",
            press(LiveTvFailure("tuner_unavailable", "now", ownerDecided = true)).hint,
        )
        assertNotNull(
            "nobody decided; the start may exist",
            press(LiveTvFailure("owner_unavailable", "now", ownerDecided = false)).hint,
        )
        assertNull(
            "the ingress refused the body before an owner saw it",
            press(LiveTvFailure("invalid_request", status = 400)).hint,
        )
        assertNotNull(
            "the same code at 5xx reached something that could have tuned",
            press(LiveTvFailure("invalid_request", status = 503)).hint,
        )
    }

    @Test
    fun aConfirmedReleaseForgetsTheHintAndTheHeartbeatBarelyTouchesTheDisk() = runTest {
        val store = Hints(null)
        val requests = Requests()
        var clock = 1_000L
        val lease = LiveTvLease(
            requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)),
        ) { clock }

        lease.start("one").await()
        assertNotNull(store.hint)
        val afterStart = store.writes
        // Twelve heartbeats, one minute of wall clock: the barrier this
        // replaced wrote and fsynced on every one of them, on the main thread.
        repeat(12) { clock += 5_000; lease.touchHint() }
        assertTrue("a heartbeat must not write every time", store.writes - afterStart <= 1)
        assertEquals("but it must move `touched_at` in memory", clock, store.hint?.touched_at)

        lease.stop().await()
        assertNull("a confirmed DELETE forgets the hint", store.hint)
    }

    @Test
    fun anIngressWithoutProtocolThreeStillKeepsHintsForLater() = runTest {
        val store = Hints(LiveTvStartHint(STRAY, 1_000))
        val requests = Requests().apply { recoveryRoutes = false }
        val lease = LiveTvLease(
            requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)),
        ) { 90_000 }

        assertNull("there is no route to resume on", lease.resumeIfRecent())
        lease.start("one").await()

        assertTrue("there is no route to retire on either", requests.retired.isEmpty())
        assertNotNull("but the hint is still written for an ingress that grows one", store.hint)
    }

    @Test
    fun resumeReattachesALiveSessionAndForgetsTheHintTheOwnerHasBuried() = runTest {
        fun lease(store: Hints, requests: Requests) = LiveTvLease(
            requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)),
        ) { 90_000 }

        // The fixture's own activation, not a stand-in: the lease must hand
        // back exactly what the owner sent, because that is what playback is
        // built from.
        val fixtureAnswer: LiveTvResumeAnswer = Net.json.decodeFromString(
            cases.getValue("resume").jsonArray.map { it.jsonObject }
                .first { it["then"]?.jsonPrimitive?.content == "reattach" }
                .getValue("body").jsonObject.toString(),
        )
        val live = Hints(LiveTvStartHint(STRAY, 1_000))
        val reattached = lease(live, Requests().apply { resumeAnswer = fixtureAnswer }).resumeIfRecent()
        assertEquals(fixtureAnswer.session, reattached)
        assertEquals("7.1", reattached?.channel?.id)
        assertNotNull("a rejoined session keeps its handle", live.hint)

        listOf("ended", "retired").forEach { outcome ->
            val store = Hints(LiveTvStartHint(STRAY, 1_000))
            assertNull(lease(store, Requests().apply {
                resumeAnswer = LiveTvResumeAnswer(outcome)
            }).resumeIfRecent())
            assertNull("$outcome must forget the hint", store.hint)
        }

        val pending = Hints(LiveTvStartHint(STRAY, 1_000))
        assertNull(lease(pending, Requests().apply {
            resumeAnswer = LiveTvResumeAnswer("pending")
        }).resumeIfRecent())
        assertNotNull("an activation still in flight keeps its handle", pending.hint)
    }

    private companion object {
        const val STRAY = "0123456789abcdef0123456789abcdef"
    }

    private class Hints(var hint: LiveTvStartHint?) : LiveTvHintStore {
        var writes = 0
        private var writtenAt = Long.MIN_VALUE
        override fun read(): LiveTvStartHint? = hint
        override fun remember(hint: LiveTvStartHint) {
            this.hint = hint
            writtenAt = hint.touched_at
            writes += 1
        }
        override fun touch(at: Long) {
            val current = hint ?: return
            hint = current.copy(touched_at = at)
            if (at - writtenAt >= LIVE_TV_HINT_WRITE_INTERVAL_MS) {
                writtenAt = at
                writes += 1
            }
        }
        override fun forget() {
            hint = null
            writtenAt = Long.MIN_VALUE
        }
    }

    private inner class Requests : LiveTvRequests {
        val events = mutableListOf<String>()
        val retired = mutableListOf<String>()
        var error: (() -> LiveTvFailure)? = null
        var retireBlocks = false
        var retireFinished = false
        var resumeAnswer = LiveTvResumeAnswer("pending")
        var resumeError: (() -> LiveTvFailure)? = null
        override var recoveryRoutes = true
        private val neverAnswers = CompletableDeferred<Unit>()

        override suspend fun start(channel: String, requestId: String): LiveTvStarted {
            events += "start:$channel:$requestId"
            error?.let { throw it() }
            return LiveTvStarted("cap-$channel", this@LiveTvStartCasesTest.channel, live = true)
        }
        override suspend fun release(capability: String) { events += "release:$capability" }
        override suspend fun retire(requestId: String) {
            retired += requestId
            if (retireBlocks) neverAnswers.await()
            retireFinished = true
        }
        override suspend fun resume(requestId: String): LiveTvResumeAnswer {
            resumeError?.let { throw it() }
            return resumeAnswer
        }
    }
}
