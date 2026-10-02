package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class PreparedSurfaceReceiptsTest {
    @Test fun hiddenRenderingDoesNotProveExposureAndRollbackFencesLatePresentation() {
        val owner = PreparedSurfaceReceipts<Any>()
        val previous = Any(); val next = Any()
        owner.attach(previous); owner.attach(next)
        assertNull(owner.expose(next))
        owner.rendered(previous, PreparedSurfaceReceipts.Frame(10_000_000, 1280, 720))
        owner.rendered(next, PreparedSurfaceReceipts.Frame(10_000_000, 1920, 1080))
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
        owner.rendered(old, PreparedSurfaceReceipts.Frame(10_000_000, 1920, 1080))
        val beforeSeek = owner.expose(old)!!
        owner.invalidate(old)
        assertFalse(owner.presented(beforeSeek)); assertFalse(owner.ready(old, 10_000))
        owner.rendered(old, PreparedSurfaceReceipts.Frame(30_000_000, 1920, 1080))
        val beforeDestroy = owner.expose(old)!!
        owner.remove(old)
        val new = Any(); owner.attach(new)
        owner.rendered(old, PreparedSurfaceReceipts.Frame(30_000_000, 1920, 1080))
        assertFalse(owner.ready(new)); assertFalse(owner.presented(beforeDestroy))
        owner.rendered(new, PreparedSurfaceReceipts.Frame(30_000_000, 1920, 1080))
        assertTrue(owner.presented(owner.expose(new)!!))
    }

    @Test fun outputOverlapIsBoundedAndRetirementAllowsTheNextPreparation() {
        val owner = PreparedSurfaceReceipts<Any>()
        val first = Any(); val second = Any(); val third = Any()
        owner.attach(first); owner.attach(second)
        assertThrows(IllegalStateException::class.java) { owner.attach(third) }
        owner.remove(first); owner.attach(third)
        owner.rendered(third, PreparedSurfaceReceipts.Frame(-1, 1920, 1080))
        assertFalse(owner.ready(third))
    }
}
