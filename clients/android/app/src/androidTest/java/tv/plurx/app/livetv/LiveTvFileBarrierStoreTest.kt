package tv.plurx.app.livetv

import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.After
import org.junit.Assert.*
import org.junit.Test

/**
 * The durable half of the start barrier, on a real device.
 *
 * Every other barrier case uses the in-memory `Store` fake, and the one that
 * calls itself a restart builds a second `LiveTvStartBarrier` over the same
 * live object — which proves the deadline arithmetic, not durability. This is
 * the component whose failure mode is a physical tuner nobody reclaims, so it
 * is worth exercising against the actual `AtomicFile` it ships with.
 */
class LiveTvFileBarrierStoreTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val marker = File(context.noBackupFilesDir, "live-tv-start.pending")

    private fun onDisk(): Boolean =
        marker.exists() || File(marker.path + ".bak").exists()

    @After fun clean() {
        context.noBackupFilesDir.setWritable(true)
        marker.delete()
        File(marker.path + ".bak").delete()
        File(marker.path + ".new").delete()
    }

    @Test fun aMarkerWrittenByOneProcessIsSeenByTheNext() {
        LiveTvFileBarrierStore(context).setPending(true)
        assertTrue(
            "a fresh store must observe the marker a previous process wrote",
            LiveTvFileBarrierStore(context).pending(),
        )
        assertTrue("and it must actually be on disk", onDisk())

        LiveTvFileBarrierStore(context).setPending(false)
        assertFalse(
            "a fresh store must observe the clear a previous process committed",
            LiveTvFileBarrierStore(context).pending(),
        )
        assertFalse(onDisk())
    }

    @Test fun clearingNeverReportsAnOutcomeTheDiskDoesNotShow() {
        val store = LiveTvFileBarrierStore(context)
        store.setPending(true)
        assertTrue(store.pending())

        // Take the write bit away so the unlink cannot commit. Whether the
        // kernel refuses it is the filesystem's business; the invariant is that
        // the store's answer always matches what is actually on disk, because a
        // clear it cannot prove is how a tuner gets forgotten.
        assertTrue("the fixture needs the directory to become unwritable",
            context.noBackupFilesDir.setWritable(false))
        val threw = try {
            store.setPending(false)
            false
        } catch (_: IllegalStateException) {
            true
        }
        assertEquals(
            "pending() disagreed with the disk after a clear that " +
                (if (threw) "was refused" else "reported success"),
            onDisk(),
            store.pending(),
        )
        if (threw) {
            assertTrue("a refused clear must leave the marker in place", onDisk())
        }
    }
}
