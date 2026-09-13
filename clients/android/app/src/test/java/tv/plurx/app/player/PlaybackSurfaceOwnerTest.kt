package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The recovery owner's one obligation, pinned.
 *
 * `Controller` cannot be built on the JVM — it needs a `Context`, an
 * `ExoPlayer` and a `MediaSession` — so the obligation lives in
 * [PlaybackSurfaceOwner], which is where these tests reach it. That placement
 * is the point rather than a workaround: [PlaybackSurfaceOwner.stopAndRaise] is
 * the ONLY way to reach a blocking class from `Controller`, because
 * [PlaybackSurfaceOwner.raise] raises with `player_stopped` false and the
 * presenter refuses to draw such a fault at all. A site that forgot to stop
 * cannot produce an overlay over a running picture; it produces a
 * `blocking_without_stop` line and no surface, which is what
 * `blockingWithoutAStopIsRefusedOutright` pins.
 *
 * Contract §3.0, §3.2 and §3.4;
 * PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md §4.4.
 */
class PlaybackSurfaceOwnerTest {

    private class FakeSurfacePlayer(
        override var playbackRequested: Boolean = true,
        override var positionMs: Long = 90_000,
        override var rate: Float = 1.0f,
    ) : SurfaceOwnerPlayer

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
     * Every exhausted site in `Controller` is this call with that site's
     * sentence. The assertion is the obligation: the player is stopped when the
     * fault is raised, and the fault says so.
     */
    private fun exhaustedSite(sentence: String) {
        attached()
        assertTrue("the fake player starts playing", player.playbackRequested)
        owner.stopAndRaise(
            source = SurfaceSources.OWNER_EXHAUSTED,
            context = SurfaceContext.Attached,
            attached = epoch,
            positionMs = 90_000,
            detail = sentence,
        )
        assertFalse("the owner must stop the player before raising", player.playbackRequested)
        val surface = owner.current
        assertTrue("an exhausted prompt covers the picture", surface is PlaybackSurface.Blocking)
        val fault = checkNotNull(surface.fault)
        assertEquals(SurfaceClass.Exhausted, fault.cls)
        assertTrue("the fault carries the owner's stop", fault.playerStopped)
        assertEquals(sentence, fault.detail)
        assertEquals(
            listOf(SurfaceAction.KeepWaiting, SurfaceAction.Retry, SurfaceAction.Close),
            fault.actions,
        )
        assertTrue("the prompt enters the input contract's failed state", surface.entersFailedRouting)
        // The ledger's own record of the same fact, sampled AFTER the stop.
        val row = owner.history.last()
        assertTrue("the ledger records who stopped the player", row.playerAtRaise.stoppedByOwner)
        assertTrue(logs.any { it.event == SurfaceLogEvents.RAISED })
        assertTrue(logs.none { it.error != null })
    }

    @Test
    fun theSessionlessSecondStallStopsBeforeItRaises() {
        exhaustedSite("Playback stopped responding after retrying this stream.")
    }

    @Test
    fun theSpentReopenBudgetStopsBeforeItRaises() {
        exhaustedSite("Playback stopped responding after exhausting recovery attempts.")
    }

    @Test
    fun theTargetPresentationDeadlineStopsBeforeItRaises() {
        exhaustedSite(
            "Playback couldn't reach the requested position after retrying. Your place is saved.",
        )
    }

    @Test
    fun onPlayerErrorFailStopsBeforeItRaisesAndKeepsTheTransportWording() {
        attached()
        owner.stopAndRaise(
            source = SurfaceSources.OWNER_STOPPED,
            context = SurfaceContext.Attached,
            attached = epoch,
            detail = "Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS).",
        )
        assertFalse(player.playbackRequested)
        val fault = checkNotNull(owner.current.fault)
        assertEquals(SurfaceClass.Stopped, fault.cls)
        assertEquals("Playback stopped (ERROR_CODE_IO_BAD_HTTP_STATUS).", fault.detail)
        assertEquals(listOf(SurfaceAction.Retry, SurfaceAction.Close), fault.actions)
    }

    @Test
    fun blockingWithoutAStopIsRefusedOutright() {
        // The mutation-killer for every site above: drop the stop and the
        // viewer gets no overlay at all, not an overlay over a moving picture.
        attached()
        owner.raise(
            source = SurfaceSources.OWNER_EXHAUSTED,
            context = SurfaceContext.Attached,
            attached = epoch,
            detail = "Playback stopped responding after retrying this stream.",
        )
        assertTrue("the player was never asked to stop", player.playbackRequested)
        assertEquals(PlaybackSurface.None, owner.current)
        assertEquals(
            listOf(SurfaceErrors.BLOCKING_WITHOUT_STOP),
            logs.mapNotNull { it.error },
        )
        assertTrue("a refused fault leaves no ledger row", owner.history.isEmpty())
    }

    @Test
    fun aFilmClockAdvanceOnTheSameEpochAfterAStopIsADisagreement() {
        attached()
        owner.stopAndRaise(
            source = SurfaceSources.OWNER_STOPPED,
            context = SurfaceContext.Attached,
            attached = epoch,
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
        owner.stopAndRaise(
            source = SurfaceSources.OWNER_STOPPED,
            context = SurfaceContext.Attached,
            attached = epoch,
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
        owner.stopAndRaise(
            source = SurfaceSources.OWNER_STOPPED,
            context = SurfaceContext.Attached,
            attached = epoch,
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
            owner.raise(
                source = SurfaceSources.DEGRADED_NOTICE,
                context = SurfaceContext.Attached,
                attached = epoch,
                detail = "notice $index",
            )
        }
        assertEquals(PlaybackSurfaceOwner.HISTORY_LIMIT, owner.history.size)
    }

    @Test
    fun nothingIsPublishedUntilTheAnswerChanges() {
        attached()
        owner.inert("isPlayingChanged")
        owner.tick()
        assertEquals(listOf<PlaybackSurface>(), published)
        owner.raise(
            source = SurfaceSources.DEGRADED_NOTICE,
            context = SurfaceContext.Attached,
            attached = epoch,
            detail = "Quality reduced to keep playing.",
        )
        assertEquals(1, published.size)
        assertTrue(published.single() is PlaybackSurface.Banner)
    }
}
