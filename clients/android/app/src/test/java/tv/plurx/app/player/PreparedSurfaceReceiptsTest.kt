package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class PreparedSurfaceReceiptsTest {
    @Test fun hiddenRenderingDoesNotProveExposureAndRollbackFencesLatePresentation() {
        val owner = PreparedSurfaceReceipts<Any>()
        val previous = Any(); val next = Any()
        owner.attach(previous); owner.attach(next)
        assertNull(owner.expose(next))
        owner.rendered(previous, PreparedSurfaceReceipts.Frame(10_000_000, 1280, 720, 1_000_000.0 / 24))
        owner.rendered(next, PreparedSurfaceReceipts.Frame(10_000_000, 1920, 1080, 1_000_000.0 / 24))
        assertFalse(owner.ready(next, 11_000))
        assertTrue(owner.ready(next, 10_000))
        val commit = owner.expose(next)!!
        val rollback = owner.expose(previous)!!
        assertFalse(owner.presented(commit))
        assertTrue(owner.presented(rollback))
    }

    @Test fun seekAndSurfaceRecreationRejectOldFramesAndCompositorCallbacks() {
        val owner = PreparedSurfaceReceipts<Any>()
        val old = Any(); owner.attach(old)
        owner.rendered(old, PreparedSurfaceReceipts.Frame(10_000_000, 1920, 1080, 1_000_000.0 / 24))
        val beforeSeek = owner.expose(old)!!
        owner.invalidate(old)
        assertFalse(owner.presented(beforeSeek)); assertFalse(owner.ready(old, 10_000))
        owner.rendered(old, PreparedSurfaceReceipts.Frame(30_000_000, 1920, 1080, 1_000_000.0 / 24))
        val beforeDestroy = owner.expose(old)!!
        owner.remove(old)
        val new = Any(); owner.attach(new)
        owner.rendered(old, PreparedSurfaceReceipts.Frame(30_000_000, 1920, 1080, 1_000_000.0 / 24))
        assertFalse(owner.ready(new)); assertFalse(owner.presented(beforeDestroy))
        owner.rendered(new, PreparedSurfaceReceipts.Frame(30_000_000, 1920, 1080, 1_000_000.0 / 24))
        assertTrue(owner.presented(owner.expose(new)!!))
    }

    @Test fun alignmentRequiresOneDecodedFrameRatherThanQuarterSecondSlack() {
        val owner = PreparedSurfaceReceipts<Any>(); val output = Any(); owner.attach(output)
        owner.rendered(output, PreparedSurfaceReceipts.Frame(10_000_000, 1920, 1080, 1_000_000.0 / 24))
        assertTrue(owner.ready(output, 10_041)); assertFalse(owner.ready(output, 10_042))
        assertFalse(owner.ready(output, 10_200))
        owner.rendered(output, PreparedSurfaceReceipts.Frame(10_000_000, 1920, 1080))
        assertFalse(owner.ready(output, 10_000))
        assertTrue(owner.ready(output)) // Raster readiness is separate from alignment proof.
    }

    @Test fun queuedMetadataCannotReplaceTheFrameNamedByFirstRender() {
        val slot = PreparedFirstFrameSlot()
        val first = PreparedSurfaceReceipts.Frame(10_000_000, 1920, 1080, 1_000_000.0 / 24)
        slot.offer(first)
        slot.offer(PreparedSurfaceReceipts.Frame(10_041_667, 1920, 1080, 1_000_000.0 / 24))
        val owner = PreparedSurfaceReceipts<Any>(); val output = Any(); owner.attach(output)
        owner.rendered(output, requireNotNull(slot.snapshot()))
        assertSame(first, slot.snapshot())
        assertTrue(owner.ready(output, 10_000)); assertFalse(owner.ready(output, 10_083))
        slot.clear(); owner.invalidate(output)
        assertNull(slot.snapshot()); assertFalse(owner.ready(output))
        slot.offer(PreparedSurfaceReceipts.Frame(-1, 1920, 1080))
        assertNull(slot.snapshot())
        val afterSeek = PreparedSurfaceReceipts.Frame(30_000_000, 1920, 1080, 1_000_000.0 / 24)
        slot.offer(afterSeek); assertSame(afterSeek, slot.snapshot())
    }

    @Test fun tunneledCodecClockMapsToTheSameItemFrameBoundary() {
        val offset = 1_000_000_000_000L
        val position = requireNotNull(preparedItemFramePositionUs(offset + 10_000_000, offset))
        val slot = PreparedFirstFrameSlot()
        slot.offer(PreparedSurfaceReceipts.Frame(position, 1920, 1080, 1_000_000.0 / 24))
        val owner = PreparedSurfaceReceipts<Any>(); val output = Any(); owner.attach(output)
        owner.rendered(output, requireNotNull(slot.snapshot()))
        assertTrue(owner.ready(output, 10_000)); assertFalse(owner.ready(output, 10_042))
        assertNull(preparedItemFramePositionUs(Long.MAX_VALUE, offset))
        assertNull(preparedItemFramePositionUs(10, 20))
        assertNull(preparedItemFramePositionUs(Long.MIN_VALUE, 1))
    }

    @Test fun outputOverlapIsBoundedAndRetirementAllowsTheNextPreparation() {
        val owner = PreparedSurfaceReceipts<Any>()
        val first = Any(); val second = Any(); val third = Any()
        owner.attach(first); owner.attach(second)
        assertThrows(IllegalStateException::class.java) { owner.attach(third) }
        owner.remove(first); owner.attach(third)
        owner.rendered(third, PreparedSurfaceReceipts.Frame(-1, 1920, 1080, 1_000_000.0 / 24))
        assertFalse(owner.ready(third))
    }
}
