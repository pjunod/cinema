package tv.plurx.app.livetv

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The `live` section of the shared input contract. Kotlin cannot read the
 * fixture at runtime, so `LiveTvInputPolicy` transcribes it — and this is the
 * half that keeps the transcription honest.
 */
class LiveTvInputPolicyTest {
    private val live = checkNotNull(
        javaClass.classLoader?.getResource("player-input-contract.json"),
    ) { "tests/playback/player-input-contract.json is not on the JVM test classpath" }
        .readText()
        .let { Json.parseToJsonElement(it).jsonObject }
        .getValue("live").jsonObject

    @Test
    fun everySharedLiveRoutingCellMatches() {
        val routing = live.getValue("routing").jsonObject
        LiveTvInputSurface.entries.forEach { surface ->
            val states = routing.getValue(surface.contractName).jsonObject
            LiveTvInputState.entries.forEach { state ->
                val inputs = states.getValue(state.contractName).jsonObject
                LiveTvContractInput.entries.forEach { input ->
                    assertEquals(
                        "${surface.contractName}/${state.contractName}/${input.contractName}",
                        inputs.getValue(input.contractName).jsonPrimitive.content,
                        LiveTvInputPolicy.route(surface, state, input).contractName,
                    )
                }
            }
        }
    }

    @Test
    fun theEnumsNameTheSameSetTheFixtureDoes() {
        assertEquals(
            live.getValue("inputs").jsonArray.map { it.jsonPrimitive.content }.toSet(),
            LiveTvContractInput.entries.map { it.contractName }.toSet(),
        )
        // Android transcribes two of the fixture's three surfaces; `desktop` is
        // the web's. Every surface this client claims must exist in the
        // fixture, which is the direction that matters.
        val surfaces = live.getValue("routing").jsonObject.keys
        LiveTvInputSurface.entries.forEach {
            assertEquals("$it is in the fixture", true, surfaces.contains(it.contractName))
        }
    }

    @Test
    fun theRulingsOf20260902SurviveOnTheLiveSurface() {
        // A direction on a hidden overlay only reveals it — it never changes
        // channel behind a picture nobody can see.
        listOf(
            LiveTvContractInput.Left, LiveTvContractInput.Right,
            LiveTvContractInput.Up, LiveTvContractInput.Down,
        ).forEach {
            assertEquals(
                LiveTvInputOutcome.Reveal,
                LiveTvInputPolicy.route(LiveTvInputSurface.TenFoot, LiveTvInputState.FullscreenHidden, it),
            )
        }
        // The channel list is preview-then-commit.
        assertEquals(
            LiveTvInputOutcome.FocusControl,
            LiveTvInputPolicy.route(LiveTvInputSurface.TenFoot, LiveTvInputState.FullscreenControls, LiveTvContractInput.Up),
        )
        assertEquals(
            LiveTvInputOutcome.Activate,
            LiveTvInputPolicy.route(LiveTvInputSurface.TenFoot, LiveTvInputState.FullscreenControls, LiveTvContractInput.Select),
        )
        // Nothing on this surface seeks: the outcomes do not exist.
        assertFalse(LiveTvInputOutcome.entries.any { it.contractName in setOf("skip", "preview", "commit") })
    }

    @Test
    fun theTimingsAreTheFixturesTimings() {
        val timings = live.getValue("timings").jsonObject
        fun pinned(key: String) = timings.getValue(key).jsonPrimitive.int
        assertEquals(pinned("hide_after_ms").toLong(), LiveTvInputPolicy.HIDE_AFTER_MS)
        assertEquals(pinned("channel_coalesce_ms").toLong(), LiveTvInputPolicy.CHANNEL_COALESCE_MS)
        // §3.14: the six that pace Live TV's reliability flows, transcribed by
        // the same rule as the two above. `retire_liveness_probe_ms` and
        // `retire_orphan_after_keepalives` are the web's sibling-document
        // mechanics — Android has one process and one hint — but a
        // transcription that drifts from the fixture is worth nothing on any
        // client, so all six are pinned here.
        assertEquals(pinned("guide_poll_unavailable_s").toLong(), LiveTvInputPolicy.GUIDE_POLL_UNAVAILABLE_S)
        assertEquals(pinned("guide_poll_min_s").toLong(), LiveTvInputPolicy.GUIDE_POLL_MIN_S)
        assertEquals(
            pinned("guide_poll_after_next_refresh_s").toLong(),
            LiveTvInputPolicy.GUIDE_POLL_AFTER_NEXT_REFRESH_S,
        )
        assertEquals(pinned("guide_poll_ceiling_s").toLong(), LiveTvInputPolicy.GUIDE_POLL_CEILING_S)
        assertEquals(pinned("retire_liveness_probe_ms").toLong(), LiveTvInputPolicy.RETIRE_LIVENESS_PROBE_MS)
        assertEquals(
            pinned("retire_orphan_after_keepalives"),
            LiveTvInputPolicy.RETIRE_ORPHAN_AFTER_KEEPALIVES,
        )
        assertEquals(pinned("start_replay_attempts"), LiveTvInputPolicy.START_REPLAY_ATTEMPTS)
    }

    /**
     * The guide polls on the owner's clock. These are the three rules §3.14
     * names, against the contract's own numbers rather than repeated literals.
     */
    @Test
    fun theGuidePollFollowsTheOwnersNextRefresh() {
        val min = LiveTvInputPolicy.GUIDE_POLL_MIN_S
        val after = LiveTvInputPolicy.GUIDE_POLL_AFTER_NEXT_REFRESH_S
        val fresh = LiveTvGuide(freshness = "fresh")

        // An announced refresh far enough out is honoured exactly.
        assertEquals(
            600 + after,
            LiveTvGuideReducer.nextPollDelaySeconds(fresh.copy(next_refresh_at = 1_600), 1_000),
        )
        // One in the past, or so close that the answer would not exist yet,
        // still cannot poll faster than the floor.
        assertEquals(min, LiveTvGuideReducer.nextPollDelaySeconds(fresh.copy(next_refresh_at = 900), 1_000))
        assertEquals(min, LiveTvGuideReducer.nextPollDelaySeconds(fresh.copy(next_refresh_at = 1_001), 1_000))
        // An owner older than this contract says nothing about when it returns.
        assertEquals(min, LiveTvGuideReducer.nextPollDelaySeconds(fresh, 1_000))
        assertEquals(
            LiveTvInputPolicy.GUIDE_POLL_UNAVAILABLE_S,
            LiveTvGuideReducer.nextPollDelaySeconds(LiveTvGuide(freshness = "unavailable"), 1_000),
        )
        // An unavailable guide that DOES name its next refresh is scheduled on
        // it: "unavailable" is a rendered state, not a reason to guess.
        assertEquals(
            600 + after,
            LiveTvGuideReducer.nextPollDelaySeconds(
                LiveTvGuide(freshness = "unavailable", next_refresh_at = 1_600), 1_000,
            ),
        )
        // A read that did not answer carries exactly what an unavailable guide
        // with no next refresh does, and is paced the same way.
        assertEquals(
            LiveTvInputPolicy.GUIDE_POLL_UNAVAILABLE_S,
            LiveTvGuideReducer.nextPollDelaySeconds(null, 1_000),
        )
    }

    /**
     * The ceiling. An owner whose clock is skewed, whose refresh interval is
     * misconfigured, or whose loop has stopped can answer with a
     * `next_refresh_at` hours out; the grid must still come back on its own
     * rather than sit on a stale document until the viewer leaves the screen.
     */
    @Test
    fun aFarFutureNextRefreshCannotParkTheGrid() {
        val ceiling = LiveTvInputPolicy.GUIDE_POLL_CEILING_S
        val fresh = LiveTvGuide(freshness = "fresh")
        val now = 1_000L

        // Six hours out, an owner-day out, and the end of time all clamp.
        listOf(now + 6 * 3_600, now + 86_400, Long.MAX_VALUE).forEach { announced ->
            assertEquals(
                "next_refresh_at $announced must clamp to the ceiling",
                ceiling,
                LiveTvGuideReducer.nextPollDelaySeconds(fresh.copy(next_refresh_at = announced), now),
            )
        }
        // An unavailable guide with a far-future refresh clamps the same way:
        // the ceiling is about the answer, not about the freshness.
        assertEquals(
            ceiling,
            LiveTvGuideReducer.nextPollDelaySeconds(
                LiveTvGuide(freshness = "unavailable", next_refresh_at = Long.MAX_VALUE), now,
            ),
        )
        // Just inside the ceiling is still honoured exactly, so the clamp never
        // becomes the cadence for an owner that is behaving.
        val inside = now + ceiling - LiveTvInputPolicy.GUIDE_POLL_AFTER_NEXT_REFRESH_S - 1
        assertEquals(
            ceiling - 1,
            LiveTvGuideReducer.nextPollDelaySeconds(fresh.copy(next_refresh_at = inside), now),
        )
        // And the ceiling can never undercut the floor.
        assertTrue(ceiling > LiveTvInputPolicy.GUIDE_POLL_MIN_S)
        assertTrue(ceiling > LiveTvInputPolicy.GUIDE_POLL_UNAVAILABLE_S)
    }
}

/**
 * The routing table is only worth transcribing if something calls it. Nothing
 * did: `LiveTvInputPolicy.route` had one call site in the whole app and it was
 * this test file. These pin the wiring in the screen that turns the table into
 * behaviour — the source is read rather than composed, because a Compose
 * screen needs an instrumented device and these run on the JVM.
 */
class LiveTvScreenWiringTest {
    private val screen = java.io.File(
        "src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt",
    ).readText()

    @Test
    fun `every press goes through the shared table`() {
        // Key spellings and the handler both live in LiveTvKeyAdapter, which is
        // the only file `scripts/player-input-fence` lets hold them.
        assertTrue(screen.contains(".liveTvInputAdapter(enabled = fullscreen && state.playing && !isInPip)"))
        assertTrue(screen.contains("applyOutcome(LiveTvInputPolicy.route(surface, inputState(), input))"))
        val adapter = java.io.File(
            "src/main/java/tv/plurx/app/livetv/LiveTvKeyAdapter.kt",
        ).readText()
        assertTrue(adapter.contains("Key.DirectionLeft -> LiveTvContractInput.Left"))
        // Key-up is never an input: the contract is written in presses, and
        // routing both edges would double every outcome.
        assertTrue(adapter.contains("if (event.type != KeyEventType.KeyDown) return null"))
        // And hidden controls are `FullscreenHidden`, whose direction
        // rows say `reveal` and nothing else.
        assertTrue(screen.contains("else -> LiveTvInputState.FullscreenHidden"))
    }

    @Test
    fun `the overlay hides itself after the contract's four seconds`() {
        assertTrue(screen.contains("delay(LiveTvInputPolicy.HIDE_AFTER_MS)"))
        assertTrue(screen.contains("overlayVisible = false"))
    }

    @Test
    fun `tuning and layout changes do not force television fullscreen`() {
        assertFalse(screen.contains("if (television) fullscreen = state.playing"))
        assertTrue(screen.contains("WideLiveTvBrowser("))
    }

    @Test
    fun `delegated focus and activation reach Compose exactly once`() {
        assertTrue(screen.contains("LiveTvInputOutcome.Delegate,"))
        assertTrue(screen.contains("LiveTvInputOutcome.Activate,"))
        assertTrue(screen.contains("-> return false"))
    }

    @Test
    fun `a refused picture-in-picture never latches the retention flag`() {
        // Setting `retain` before the call and discarding the Boolean disabled
        // every lifecycle release path for the life of the process whenever the
        // system refused PiP — a keepalive loop holding the household's only
        // tuner against a screen nobody was on.
        assertTrue(screen.contains("val entered = target.enterPictureInPictureMode("))
        assertTrue(screen.contains("controller.setRetained(entered)"))
        assertFalse(screen.contains("controller.setRetained(true)\n        target.enterPictureInPictureMode"))
    }

    @Test
    fun `leaving picture-in-picture brings the chrome back`() {
        assertTrue(screen.contains("overlayVisible = !info.isInPictureInPictureMode"))
    }

    @Test
    fun `a channel key routes through the coalescer rather than straight to a tuner`() {
        assertTrue(screen.contains("controller.requestChannel(next)"))
    }
}
