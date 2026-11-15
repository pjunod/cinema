package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class PreparedAcquisitionTest {
    @Test fun zeroOriginUsesNativeVerifiedEntryRatherThanExplicitUncachedPredecessor() {
        // Reproduced resume: explicit 1410.332s requested segment704, while
        // the verified server START/default entry was705 at1411.410s.
        assertNull(preparedInitialAttachPositionMs(0, 1_410_332))
        val resolved = preparedAcquisitionResolution(true, 0, 1_410_332, 1_411_410)!!
        assertEquals(PreparedAcquisitionKind.Hint, resolved.kind)
        assertEquals(1_411_410L, resolved.floorFilmMs)
        assertNull(resolved.localSeekMs)
        // Acquisition and meeting floor never change the full film origin.
        assertEquals(1_411_410L, successorFilmPositionMs(0, 1_411_410))
        val hold = RendezvousHold(floorFilmMs = resolved.floorFilmMs)
        assertEquals(1_411_410L, hold.park(0, 1_409_000).rendezvousFilmMs)
    }

    @Test fun missingHintPreservesLegacyIncumbentAttachmentBeforeMetadataReady() {
        assertNull(preparedInitialAttachPositionMs(0, 1_404_173))
        val resolved = preparedAcquisitionResolution(true, 0, 1_410_332, null)!!
        assertEquals(PreparedAcquisitionKind.LegacyFallback, resolved.kind)
        assertEquals(0L, resolved.floorFilmMs)
        assertEquals(1_410_332L, resolved.localSeekMs)
        assertEquals(0L, preparedAcquisitionResolution(true, 0, -1, null)!!.localSeekMs)
    }

    @Test fun nonzeroAndNegativeOriginsKeepExplicitLocalConversionAndIgnoreFilmHints() {
        assertEquals(3_590L, preparedInitialAttachPositionMs(1_411_410, 1_415_000))
        val resolved = preparedAcquisitionResolution(true, 1_411_410, 1_415_000, 1_500_000)!!
        assertEquals(PreparedAcquisitionKind.ExplicitOrigin, resolved.kind)
        assertEquals(1_411_410L, resolved.floorFilmMs)
        assertNull(resolved.localSeekMs)
        assertEquals(1_415_000L, successorFilmPositionMs(1_411_410, 3_590))
        assertEquals(1_487L, preparedInitialAttachPositionMs(-83, 1_404))
    }

    @Test fun staleStageCannotResolveOrSeekAfterReleaseRollbackOrNewAuthorization() {
        assertNull(preparedAcquisitionResolution(false, 0, 1_410_332, 1_411_410))
        assertNull(preparedAcquisitionResolution(false, 0, 1_410_332, null))
        assertNull(preparedAcquisitionResolution(false, 1_411_410, 1_415_000, null))
        val player = Any()
        val owner = PreparedTimelineOwner(player, "action", "session", 0, 1, "https://cinema.example", 2)
        val current = owner.isCurrent(Any(), "action", "session", 0, 1, "https://cinema.example", 2)
        assertNull(preparedAcquisitionResolution(current, 0, 1_410_332, 1_411_410))
    }

    @Test fun outOfRangeEntryCannotBecomeAnAcquisitionFloor() {
        for (entry in listOf(-1L, Long.MAX_VALUE)) {
            val resolved = preparedAcquisitionResolution(true, 0, 1_410_332, entry)!!
            assertEquals(PreparedAcquisitionKind.LegacyFallback, resolved.kind)
            assertEquals(1_410_332L, resolved.localSeekMs)
        }
    }

    @Test fun acquisitionDiagnosticIsFiniteAndDoesNotCallTheEntryAnOrigin() {
        val resolved = preparedAcquisitionResolution(true, 0, 1_410_332, 1_411_410)!!
        val detail = preparedAcquisitionDetail(resolved, 0, null, 1_411_410, 1_411_410, 1_410_332)
        assertTrue(detail.startsWith("kind=hint o=0 initial_ms=default hint_ms=1411410 entry_ms=1411410"))
        assertTrue(detail.contains("floor_ms=1411410 seek_ms=none"))
        assertTrue(detail.length <= 200)
        assertTrue(preparedAcquisitionDetail(resolved, Long.MAX_VALUE, Long.MAX_VALUE,
            Long.MAX_VALUE, Long.MAX_VALUE, Long.MAX_VALUE).length <= 200)
    }
}
