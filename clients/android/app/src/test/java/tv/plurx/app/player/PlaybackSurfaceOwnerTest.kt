package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
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

    // ------------------------------------------------- the three reach sites
    //
    // §3.3 rows 10, 12 and 18 had no Android raise site at all, which is what
    // made `client_preparing`, `buffering` and `surface_log_only` unreachable
    // on this client. `Controller` cannot be built on the JVM, so — as with the
    // blocking sites above — each one is a named method here and that is where
    // it is pinned.

    @Test
    fun aMediaWaitIsTheBufferingClassAndIsNotDrawnBeforeTheDebounce() {
        attached()
        owner.bufferingMediaWait(epoch, positionMs = 90_000)
        assertTrue("the wait must not stop the player", player.playbackRequested)
        // The fault exists from this instant; `buffering_min_ms` debounces the
        // SURFACE, which is the whole reason a fragment-boundary wait no longer
        // repaints the screen.
        assertEquals(PlaybackSurface.None, owner.current)
        assertTrue(logs.any { it.event == SurfaceLogEvents.RAISED })
        now += SurfaceTimings.BUFFERING_MIN_MS - 1
        owner.tick()
        assertEquals("one millisecond short is still nothing", PlaybackSurface.None, owner.current)
        now += 1
        owner.tick()
        val surface = owner.current
        assertTrue("nothing is presenting, so the wait covers the picture", surface is PlaybackSurface.Blocking)
        val fault = checkNotNull(surface.fault)
        assertEquals(SurfaceClass.Buffering, fault.cls)
        assertEquals(SurfaceSources.MEDIA_WAITING, fault.source)
        assertEquals(90_000L, fault.positionMs)
        assertFalse("a wait is not the input contract's failed state", surface.entersFailedRouting)
    }

    @Test
    fun aWaitIsRaisedOnceHoweverOftenTheSamplerSeesIt() {
        attached()
        repeat(6) {
            now += 100
            owner.bufferingMediaWait(epoch, positionMs = 90_000)
        }
        assertEquals(
            "a sampler on a loop must not push a fault per sample",
            1,
            logs.count { it.event == SurfaceLogEvents.RAISED },
        )
        // And the ONE fault's own raise time is what the debounce is measured
        // against, so a restated wait cannot postpone the surface for ever.
        owner.tick()
        assertTrue(owner.current is PlaybackSurface.Blocking)
        assertEquals(SurfaceClass.Buffering, checkNotNull(owner.current.fault).cls)
    }

    @Test
    fun aWaitIsRetiredByThePictureItself() {
        attached()
        owner.bufferingMediaWait(epoch, positionMs = 90_000)
        now += SurfaceTimings.BUFFERING_MIN_MS
        owner.tick()
        assertTrue(owner.current is PlaybackSurface.Blocking)
        logs.clear()
        owner.presenting(true, epoch)
        assertEquals(PlaybackSurface.None, owner.current)
        assertEquals(
            listOf(SurfaceLogEvents.CLEARED),
            logs.map { it.event },
        )
        assertEquals("presenting", logs.single().by)
    }

    @Test
    fun aClientOpenIsThePreparingClassAndIsRaisedOncePerGeneration() {
        attached()
        owner.preparingClientOpen(epoch, SurfaceContext.Start)
        owner.preparingClientOpen(epoch, SurfaceContext.Start)
        assertTrue("opening a stream must not stop the player", player.playbackRequested)
        assertEquals(1, logs.count { it.event == SurfaceLogEvents.RAISED })
        val surface = owner.current
        assertTrue(surface is PlaybackSurface.Blocking)
        val fault = checkNotNull(surface.fault)
        assertEquals(SurfaceClass.Preparing, fault.cls)
        assertEquals(SurfaceSources.CLIENT_PREPARING, fault.source)
        assertFalse("a cold start is not the input contract's failed state", surface.entersFailedRouting)
        // No sentence of its own: the screen's existing wait copy is what this
        // window said before the presenter owned it.
        assertNull(fault.detail)
        // It has no intent, so the picture is what retires it.
        owner.presenting(true, epoch)
        assertEquals(PlaybackSurface.None, owner.current)
    }

    @Test
    fun aRecoveryStepRaisedAfterAnOpenKeepsItsReason() {
        // The reopen paths raise their step after `restartAt`, and progress
        // classes tie on severity — so recency has to be what decides, or every
        // reopen would say "opening" instead of why.
        attached()
        owner.preparingClientOpen(epoch, SurfaceContext.Start)
        now += 10
        owner.recoveringOwnerStep(epoch, SurfaceContext.Start, "Reconnecting to the stream.")
        val fault = checkNotNull(owner.current.fault)
        assertEquals(SurfaceClass.Recovering, fault.cls)
        assertEquals("Reconnecting to the stream.", fault.detail)
    }

    @Test
    fun aLogOnlyEventMovesNothingAndIsStillRecorded() {
        attached()
        owner.degradedNotice(epoch, SurfaceContext.Attached, "Quality reduced to keep playing.")
        val before = owner.current
        val historyBefore = owner.history.size
        logs.clear()
        owner.logOnly(epoch, "prepared successor abandoned after it failed")
        assertEquals("row 18 leaves the surface exactly as it was", before, owner.current)
        assertEquals("and it is not a fault, so it takes no ledger row", historyBefore, owner.history.size)
        val entry = logs.single()
        assertEquals(SurfaceLogEvents.LOG_ONLY, entry.event)
        assertEquals(SurfaceSources.LOG_ONLY, entry.source)
        assertEquals(epoch, entry.attached)
        assertNull(entry.cls)
        // The event is the only trace there is, so it has to say what happened.
        assertEquals("prepared successor abandoned after it failed", entry.detail)
    }

    @Test
    fun aLogOnlyEventNeverEntersTheFailedRouting() {
        attached()
        owner.logOnly(epoch, "control reporting stopped (transport:404:session_gone)")
        assertEquals(PlaybackSurface.None, owner.current)
        assertFalse(owner.current.entersFailedRouting)
        assertTrue("the incumbent is untouched", player.playbackRequested)
    }

    // ------------------------------------------ the fixture's `codes` lookup

    @Test
    fun aRefusalCodeNamesOnlyARowThatListsItInAContextThatDeclaresIt() {
        // Row 6's create codes, in the context the row declares.
        assertEquals(
            SurfaceSources.CREATE_503_NOT_YET,
            surfaceSourceForCode("vod_index_pending", SurfaceContext.Start),
        )
        // The same code on a SEGMENT is not that row: row 6 is `start` only.
        assertNull(surfaceSourceForCode("vod_index_pending", SurfaceContext.Change))
        // A code in no row's list names no row, and neither does no code at all
        // — a 503 nobody explained may not borrow a class.
        assertNull(surfaceSourceForCode("something_else_entirely", SurfaceContext.Attached))
        assertNull(surfaceSourceForCode(null, SurfaceContext.Attached))
        assertNull(surfaceSourceForCode("   ", SurfaceContext.Attached))
        // Every code the fixture lists resolves to the row that lists it, in a
        // context that row declares. This is what makes the lookup honour a
        // list the fixture grows rather than one transcribed into the adapter.
        for (row in SURFACE_SOURCES) {
            for (code in row.codes) {
                val context = SurfaceContext.entries.first { row.matches(it) }
                assertEquals(code, row.id, surfaceSourceForCode(code, context))
            }
        }
    }

    // ------------------------------------ the viewer's own transport intent

    @Test
    fun theOwnersOwnStopIsNotAViewerPause() {
        val intent = ViewerTransportIntent()
        // An ordinary pause and an ordinary resume are the viewer's.
        assertEquals(false, intent.report(false))
        assertEquals(true, intent.report(true))
        // The owner's stop-before-raise is not, however Media3 attributes it.
        intent.ownerStopping()
        assertNull("the owner's own stop must never retire a fault", intent.report(false))
        // And the level clears on the next request for playback, not on the
        // first report — a stop the platform never reported (already paused)
        // would otherwise swallow the viewer's next real pause.
        assertEquals(true, intent.report(true))
        assertEquals(false, intent.report(false))
    }

    @Test
    fun aStopTheplatformNeverReportedDoesNotSwallowALaterPause() {
        val intent = ViewerTransportIntent()
        // The owner stops a player that is already paused: Media3 reports
        // nothing at all, so the level is still in force.
        intent.ownerStopping()
        // The viewer presses play — the only thing that can follow — and then
        // pauses for real.
        assertEquals(true, intent.report(true))
        assertEquals(false, intent.report(false))
    }

    @Test
    fun aBufferingFaultRetiresWhenTheViewerPauses() {
        attached()
        owner.bufferingMediaWait(epoch, positionMs = 90_000)
        now += SurfaceTimings.BUFFERING_MIN_MS
        owner.tick()
        assertTrue("the wait is on screen", owner.current is PlaybackSurface.Blocking)
        logs.clear()
        // A paused picture produces no more presentation samples, so evidence
        // can never retire this — the spinner would sit over a still frame
        // until the generation changed.
        owner.playbackRequested(false)
        assertEquals(PlaybackSurface.None, owner.current)
        assertEquals(listOf(SurfaceLogEvents.CLEARED), logs.map { it.event })
        assertEquals("playback_not_requested", logs.single().by)
    }

    @Test
    fun resumingRaisesNothingBack() {
        attached()
        owner.bufferingMediaWait(epoch, positionMs = 90_000)
        owner.playbackRequested(false)
        logs.clear()
        now += 5_000
        owner.playbackRequested(true)
        owner.tick()
        assertEquals("the raise sites decide what comes back", PlaybackSurface.None, owner.current)
        assertTrue(logs.none { it.event == SurfaceLogEvents.RAISED })
    }

    @Test
    fun aPauseDoesNotAnswerAPrompt() {
        attached()
        owner.exhaustedAfterReopenBudget(epoch, positionMs = 90_000)
        owner.bufferingMediaWait(epoch, positionMs = 90_000)
        logs.clear()
        owner.playbackRequested(false)
        val surface = owner.current
        assertTrue("a prompt is answered by the viewer, not by a transport change",
            surface is PlaybackSurface.Blocking)
        val fault = checkNotNull(surface.fault)
        assertEquals(SurfaceClass.Exhausted, fault.cls)
        // The wait underneath it went, and only the wait.
        assertEquals(listOf(SurfaceLogEvents.CLEARED), logs.map { it.event })
        assertEquals(SurfaceClass.Buffering, logs.single().cls)
        // And the way out is still there.
        assertTrue(fault.actions.contains(SurfaceAction.Close))
        owner.userAction(SurfaceAction.Close)
        assertEquals(PlaybackSurface.None, owner.current)
    }

    @Test
    fun aPauseLeavesAClientOpenAlone() {
        // `playback_not_requested` is on `buffering` and no other class: a start
        // the viewer has not seen yet has not been paused by them.
        attached()
        owner.preparingClientOpen(epoch, SurfaceContext.Start)
        owner.playbackRequested(false)
        val fault = checkNotNull(owner.current.fault)
        assertEquals(SurfaceClass.Preparing, fault.cls)
    }

    @Test
    fun theSegmentRefusalRowAdmitsOnlyTheCodesTheFixtureLists() {
        // Contract §3.3 row 8 is a 503 WITH a "not yet" code, and this client is
        // the one that can read the body. `vod_disabled` is the service being
        // switched off; a 503 carrying it is not a recovery in progress.
        assertEquals(
            SurfaceSources.SEGMENT_503_NOT_YET,
            surfaceSourceForCode("segment_pending", SurfaceContext.Attached),
        )
        assertTrue(
            surfaceRowAdmitsCode(
                SurfaceSources.SEGMENT_503_NOT_YET,
                "node_wait_capacity",
                SurfaceContext.Attached,
            ),
        )
        assertFalse(
            surfaceRowAdmitsCode(
                SurfaceSources.SEGMENT_503_NOT_YET,
                "vod_disabled",
                SurfaceContext.Attached,
            ),
        )
        assertFalse(
            "a 503 nobody explained is not a \"still building\" answer either",
            surfaceRowAdmitsCode(SurfaceSources.SEGMENT_503_NOT_YET, null, SurfaceContext.Attached),
        )
    }

    @Test
    fun aRowThatListsNoCodesClaimsItsStatusOutright() {
        // The adapter has to be right BEFORE and AFTER a `codes` list is added
        // to a row, because demanding a code from a row that lists none would
        // make that row unreachable — the defect this whole change removes.
        for (row in SURFACE_SOURCES) {
            val context = SurfaceContext.entries.first { row.matches(it) }
            if (row.codes.isEmpty()) {
                assertTrue(
                    "${'$'}{row.id} lists no codes, so any refusal is its refusal",
                    surfaceRowAdmitsCode(row.id, null, context),
                )
                assertTrue(row.id, surfaceRowAdmitsCode(row.id, "anything_at_all", context))
            } else {
                assertFalse(
                    "${'$'}{row.id} lists codes, so a bodiless refusal is not its refusal",
                    surfaceRowAdmitsCode(row.id, null, context),
                )
                assertFalse(row.id, surfaceRowAdmitsCode(row.id, "not_in_the_list", context))
                for (code in row.codes) {
                    assertTrue("${'$'}{row.id}/${'$'}code", surfaceRowAdmitsCode(row.id, code, context))
                }
            }
            // A row is never consulted in a context it does not declare.
            for (other in SurfaceContext.entries.filterNot { row.matches(it) }) {
                assertFalse(
                    "${'$'}{row.id} in ${'$'}{other.wire}",
                    surfaceRowAdmitsCode(row.id, row.codes.firstOrNull(), other),
                )
            }
        }
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
