package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The recovery owner's one obligation, pinned.
 *
 * `Controller` cannot be built on the JVM — it needs a `Context`, an
 * `ExoPlayer` and a `MediaSession` — so each site lives on
 * [PlaybackSurfaceOwner] as its own named method, and that is where these tests
 * reach it. One test per site, and each site does its OWN stop, so deleting one
 * stop fails exactly one test and the failure names the site. The first review
 * of this file got that wrong: it was one helper called with three sentences,
 * which meant one mutation failed all three or none.
 *
 * The placement is the design rather than a workaround. There is no generic
 * "raise a blocking fault" call reachable from `Controller` — the two private
 * entry points are the only ones, `scripts/playback-surface-fence` fails on any
 * generic raising spelling in a scanned file, and a fault raised without the
 * stop is refused by the presenter outright, producing a
 * `blocking_without_stop` line and no surface at all rather than an overlay
 * over a running picture.
 *
 * Contract §3.0, §3.2 and §3.4;
 * PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md §4.4.
 */
class PlaybackSurfaceOwnerTest {

    private class FakeSurfacePlayer(
        override var playbackRequested: Boolean = true,
        override var positionMs: Long = 90_000,
    ) : SurfaceOwnerPlayer {
        // The shipped controller reports zero while the transport is stopped,
        // because `playbackParameters.speed` is the requested SETTING and never
        // moves. M4 measures "exhausted with rate 0", so the fake models the
        // same thing and the ledger assertion below is about something real.
        override val rate: Float get() = if (playbackRequested) 1.0f else 0f
    }

    private val epoch = 7L
    private var now = 0L
    private val player = FakeSurfacePlayer()
    private val logs = mutableListOf<SurfaceLog>()
    private val published = mutableListOf<PlaybackSurface>()
    private val owner = PlaybackSurfaceOwner(
        player = player,
        nowMs = { now },
        publish = { published += it },
        emit = { logs += it },
    )

    private fun attached() {
        owner.attach(epoch)
        now = 1_000
    }

    /**
     * The assertions every blocking site owes: the player is stopped when the
     * fault is raised, and the fault says so. [raise] is the site's own call,
     * so each test exercises one site and nothing else.
     */
    private fun blockingSite(cls: SurfaceClass, sentence: String, raise: () -> Unit) {
        attached()
        assertTrue("the fake player starts playing", player.playbackRequested)
        raise()
        assertFalse("the owner must stop the player before raising", player.playbackRequested)
        val surface = owner.current
        assertTrue("a blocking fault covers the picture", surface is PlaybackSurface.Blocking)
        val fault = checkNotNull(surface.fault)
        assertEquals(cls, fault.cls)
        assertTrue("the fault carries the owner's stop", fault.playerStopped)
        assertEquals(sentence, fault.detail)
        assertEquals(cls.defaultActions, fault.actions)
        assertTrue("the prompt enters the input contract's failed state", surface.entersFailedRouting)
        // The ledger's own record of the same fact, sampled AFTER the stop.
        val row = owner.history.last()
        assertTrue("the ledger records who stopped the player", row.playerAtRaise.stoppedByOwner)
        assertEquals("the ledger records a stopped transport", 0f, row.playerAtRaise.rate, 0f)
        assertTrue(logs.any { it.event == SurfaceLogEvents.RAISED })
        assertTrue(logs.none { it.error != null })
    }

    @Test
    fun theSessionlessSecondStallStopsBeforeItRaises() {
        blockingSite(SurfaceClass.Exhausted, PlaybackSurfaceOwner.SESSIONLESS_STALL_SPENT) {
            owner.exhaustedAfterSessionlessStall(epoch, positionMs = 90_000)
        }
    }

    @Test
    fun theSpentReopenBudgetStopsBeforeItRaises() {
        blockingSite(SurfaceClass.Exhausted, PlaybackSurfaceOwner.REOPEN_BUDGET_SPENT) {
            owner.exhaustedAfterReopenBudget(epoch, positionMs = 90_000)
        }
    }

    @Test
    fun theTargetPresentationDeadlineStopsBeforeItRaises() {
        blockingSite(SurfaceClass.Exhausted, PlaybackSurfaceOwner.TARGET_NEVER_PRESENTED) {
            owner.exhaustedAfterTargetDeadline(epoch, targetMs = 90_000)
        }
    }

    @Test
    fun onPlayerErrorFailStopsBeforeItRaises() {
        blockingSite(SurfaceClass.Stopped, "Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS).") {
            owner.stoppedAfterPlaybackError(
                attached = epoch,
                source = SurfaceSources.OWNER_STOPPED,
                positionMs = null,
                detail = "Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS).",
            )
        }
    }

    @Test
    fun aFailedSessionCreateWithNothingBehindItStopsBeforeItRaises() {
        blockingSite(SurfaceClass.Stopped, "The server couldn't start this stream.") {
            owner.stoppedAfterSessionCreate(
                attached = epoch,
                source = SurfaceSources.OWNER_STOPPED,
                positionMs = null,
                detail = "The server couldn't start this stream.",
            )
        }
    }

    @Test
    fun theControlPlaneTerminalVerdictStopsBeforeItRaises() {
        // R2: D1 keeps the verdict from tearing down — the caller still returns
        // true, so no reopen starts and no budget is spent — but the owner has
        // nothing left either, and nothing else will speak for it.
        blockingSite(SurfaceClass.Stopped, "The encoder stopped and will not restart.") {
            owner.stoppedAfterControlVerdict(
                attached = epoch,
                positionMs = 90_000,
                detail = "The encoder stopped and will not restart.",
            )
        }
    }

    @Test
    fun anAuthRefusalKeepsItsSignInThroughEverySite() {
        // R1: 401/403 is context-free and blocking, so it keeps its own row and
        // its own actions wherever it is met.
        attached()
        owner.stoppedAfterSessionCreate(
            attached = epoch,
            source = SurfaceSources.AUTH_401_403,
            positionMs = null,
            detail = "Your session has expired.",
        )
        assertFalse(player.playbackRequested)
        val fault = checkNotNull(owner.current.fault)
        assertEquals(SurfaceClass.Stopped, fault.cls)
        assertEquals(listOf(SurfaceAction.SignIn, SurfaceAction.Close), fault.actions)
    }

    @Test
    fun aRefusedChangeCarriesItsRetryAndNeverStopsThePlayer() {
        attached()
        owner.refusedChange(epoch, intent = 4, positionMs = null, detail = "Couldn't switch quality.")
        assertTrue("a refused change must not stop the predecessor", player.playbackRequested)
        val surface = owner.current
        assertTrue(surface is PlaybackSurface.Banner)
        val fault = checkNotNull(surface.fault)
        assertEquals(SurfaceClass.Refused, fault.cls)
        assertEquals(listOf(SurfaceAction.Retry), fault.actions)
        assertEquals(4L, fault.intent)
        assertFalse(surface.entersFailedRouting)
    }

    @Test
    fun aBlockingFaultRaisedWithoutTheStopIsRefusedOutright() {
        // What a site that lost its stop actually produces. The named methods
        // above all stop first, so this drives the notice path with a blocking
        // source — exactly the shape `stoppedAfterPlaybackError` would have if
        // its `player.playbackRequested = false` were deleted.
        attached()
        owner.controlHold(epoch, "unused")
        logs.clear()
        val leaked = PlaybackSurfaceReducer().apply(
            SurfaceState(attached = epoch),
            SurfaceEvent.Raise(
                source = SurfaceSources.OWNER_EXHAUSTED,
                context = SurfaceContext.Attached,
                attached = epoch,
                playerStopped = false,
            ),
            nowMs = 0,
        )
        assertEquals(PlaybackSurface.None, leaked.surface)
        assertEquals(
            listOf(SurfaceErrors.BLOCKING_WITHOUT_STOP),
            leaked.log.mapNotNull { it.error },
        )
    }

    @Test
    fun aFilmClockAdvanceOnTheSameEpochAfterAStopIsADisagreement() {
        attached()
        owner.stoppedAfterPlaybackError(
            attached = epoch,
            source = SurfaceSources.OWNER_STOPPED,
            positionMs = null,
            detail = "Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS).",
        )
        logs.clear()
        // Something started the player again under the terminal — the lock
        // screen, a MediaSession transport, a bug. The film clock is what says
        // so, and the picture wins.
        player.playbackRequested = true
        val previousPositionMs = player.positionMs
        player.positionMs = previousPositionMs + 1_000
        now = 5_000
        owner.presenting(
            presentationEvidence(
                previousPositionMs = previousPositionMs,
                positionMs = player.positionMs,
                playbackRequested = player.playbackRequested,
                foreground = true,
            ),
            epoch,
        )
        val surface = owner.current
        assertTrue("the picture wins: the blocking surface is demoted", surface is PlaybackSurface.Banner)
        val fault = checkNotNull(surface.fault)
        assertEquals(SurfaceClass.Degraded, fault.cls)
        assertEquals("Playback recovered", fault.title)
        // The demotion keeps the fault's actions, so when the buffer drains the
        // viewer still gets the specific recovery.
        assertEquals(listOf(SurfaceAction.Retry, SurfaceAction.Close), fault.actions)
        assertEquals(
            listOf(SurfaceLogEvents.DISAGREEMENT),
            logs.map { it.event },
        )
        assertTrue("the ledger names the disagreement", owner.history.last().disagreed)
    }

    @Test
    fun anIsPlayingChangedAloneIsNotADisagreement() {
        attached()
        owner.stoppedAfterPlaybackError(
            attached = epoch,
            source = SurfaceSources.OWNER_STOPPED,
            positionMs = null,
            detail = "Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS).",
        )
        logs.clear()
        now = 5_000
        // The platform callback on its own, and then the evidence rule applied
        // to a clock that has not moved. Neither is proof of a picture.
        owner.inert("isPlayingChanged")
        player.playbackRequested = true
        owner.presenting(
            presentationEvidence(
                previousPositionMs = player.positionMs,
                positionMs = player.positionMs,
                playbackRequested = true,
                foreground = true,
            ),
            epoch,
        )
        assertTrue("the terminal stands", owner.current is PlaybackSurface.Blocking)
        assertEquals(SurfaceClass.Stopped, checkNotNull(owner.current.fault).cls)
        assertTrue("nothing was logged", logs.isEmpty())
        assertFalse(owner.history.last().disagreed)
    }

    @Test
    fun aHiddenAppSamplesNothing() {
        attached()
        owner.stoppedAfterPlaybackError(
            attached = epoch,
            source = SurfaceSources.OWNER_STOPPED,
            positionMs = null,
            detail = "Playback stopped.",
        )
        // `presentationForeground` false is Android's hidden page.
        owner.hidden(true)
        now = 5_000
        owner.presenting(true, epoch)
        assertTrue("a hidden app cannot demote anything", owner.current is PlaybackSurface.Blocking)
        assertTrue(logs.none { it.event == SurfaceLogEvents.DISAGREEMENT })
    }

    @Test
    fun theLedgerRingIsBounded() {
        attached()
        repeat(PlaybackSurfaceOwner.HISTORY_LIMIT + 4) { index ->
            now = 1_000L + index
            owner.degradedNotice(epoch, SurfaceContext.Attached, "notice $index")
        }
        assertEquals(PlaybackSurfaceOwner.HISTORY_LIMIT, owner.history.size)
    }

    @Test
    fun nothingIsPublishedUntilTheAnswerChanges() {
        attached()
        owner.inert("isPlayingChanged")
        owner.tick()
        assertEquals(listOf<PlaybackSurface>(), published)
        owner.degradedNotice(epoch, SurfaceContext.Attached, "Quality reduced to keep playing.")
        assertEquals(1, published.size)
        assertTrue(published.single() is PlaybackSurface.Banner)
    }
}
