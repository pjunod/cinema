package tv.plurx.app.livetv

import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

/**
 * The durable half of the start hint, on a real device.
 *
 * Every other hint case uses the in-memory fake, which proves the reducer and
 * the press flow but not durability — and durability is the whole point: the
 * process this hint outlives is one that died with a tuner open. This is the
 * component whose failure mode is a physical tuner nobody reclaims, so it is
 * worth exercising against the actual `AtomicFile` it ships with.
 */
class LiveTvStartHintStoreTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val hint = File(context.noBackupFilesDir, "live-tv-start.hint")
    private val barrier = File(context.noBackupFilesDir, "live-tv-start.pending")
    private val id = "7de1a4c09f2b4e5d8a1c3f6b9d2e4a70"

    private fun clean(file: File) {
        file.delete()
        File(file.path + ".bak").delete()
        File(file.path + ".new").delete()
    }

    @Before fun start() { clean(hint); clean(barrier) }
    @After fun finish() { clean(hint); clean(barrier) }

    @Test fun aHintWrittenByOneProcessIsReadBackByTheNext() {
        LiveTvStartHintStore(context).remember(LiveTvStartHint(id, 1_789_000_800_000))

        val next = LiveTvStartHintStore(context).read()
        assertEquals("a fresh store must observe what a previous process wrote", id, next?.request_id)
        assertEquals(1_789_000_800_000L, next?.touched_at)

        LiveTvStartHintStore(context).forget()
        assertNull(
            "a fresh store must observe the clear a previous process committed",
            LiveTvStartHintStore(context).read(),
        )
        assertFalse(hint.exists() || File(hint.path + ".bak").exists())
    }

    @Test fun theBarriersLeftoverMarkerIsDeletedOnFirstRun() {
        // Nothing reads it any more, and `noBackupFilesDir` is not somewhere a
        // viewer can reach. Every install upgrading from build 89 has one.
        barrier.writeBytes(byteArrayOf(1))
        File(barrier.path + ".bak").writeBytes(byteArrayOf(1))
        assertTrue(barrier.exists())

        LiveTvStartHintStore(context)

        assertFalse("the barrier marker must not survive", barrier.exists())
        assertFalse(File(barrier.path + ".bak").exists())
    }

    @Test fun theHeartbeatDoesNotWriteTheDiskEveryFiveSeconds() {
        // The 2026-09-05 review's ANR: the barrier fsynced an unchanged byte on
        // the main thread twelve times a minute. A heartbeat must move
        // `touched_at` in memory and commit it rarely.
        val store = LiveTvStartHintStore(context)
        store.remember(LiveTvStartHint(id, 0))
        val committed = hint.lastModified()

        var at = 0L
        repeat(11) { at += 5_000; store.touch(at) } // 55 s of heartbeats.
        assertEquals("nothing may reach the disk inside the interval", committed, hint.lastModified())
        assertEquals("but the value is current in memory", at, store.read()?.touched_at)

        store.touch(LIVE_TV_HINT_WRITE_INTERVAL_MS)
        assertEquals(
            "and the interval does commit it",
            LIVE_TV_HINT_WRITE_INTERVAL_MS,
            LiveTvStartHintStore(context).read()?.touched_at,
        )
    }

    @Test fun anUnreadableOrNonsenseHintIsNoHintRatherThanACrash() {
        hint.parentFile?.mkdirs()
        hint.writeText("{ not json")
        assertNull(LiveTvStartHintStore(context).read())

        // A well-formed file whose id is not a request id is not a handle the
        // owner would recognise, so it is not one this client will send.
        hint.writeText("""{"request_id":"NOT-HEX","touched_at":1}""")
        assertNull(LiveTvStartHintStore(context).read())
    }

    @Test fun aWriteThatCannotCommitLeavesTheStoreUsableAndSilent() {
        val store = LiveTvStartHintStore(context)
        assertTrue(
            "the fixture needs the directory to become unwritable",
            context.noBackupFilesDir.setWritable(false),
        )
        try {
            // §3.16 leaves no unavailable-storage refusal: a hint that cannot
            // be written costs a stray the owner reaps, never a refusal to tune.
            store.remember(LiveTvStartHint(id, 5))
        } finally {
            assertTrue(context.noBackupFilesDir.setWritable(true))
        }
    }
}
