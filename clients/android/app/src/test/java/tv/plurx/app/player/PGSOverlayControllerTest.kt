package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The overlay controller itself, driven the way `Controller.kt` drives it:
 * `select`, then `reconcile(seeked = true)` for a position discontinuity and
 * `reconcile()` for every other player event. Only the byte source and the
 * clock are fakes; the image type is a plain object because a JVM test cannot
 * make an `android.graphics.Bitmap`. The controller's jobs live in
 * `backgroundScope`, which `advanceUntilIdle` does not run, so every step here
 * is `runCurrent` or an explicit `advanceTimeBy`.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PGSOverlayControllerTest {
    private val generation = "a".repeat(64)

    private data class FakeImage(val hash: String)

    /** Images named in [gated] wait for the test; [failing] ones throw. */
    private class FakeSource(private val manifest: PGSOverlayManifest) : PGSOverlaySource<FakeImage> {
        val requests = mutableListOf<String>()
        val gated = mutableMapOf<String, CompletableDeferred<Unit>>()
        val failing = mutableSetOf<String>()

        override suspend fun manifest(fileId: Long, index: Long): PGSOverlayManifestFetch =
            PGSOverlayManifestFetch.Ready(manifest)

        override suspend fun image(
            fileId: Long,
            index: Long,
            generation: String,
            hash: String,
            object_: PGSOverlayObject,
        ): FakeImage {
            requests += hash
            gated[hash]?.await()
            if (hash in failing) error("The PGS subtitle image request failed (503).")
            return FakeImage(hash)
        }

        override fun imageBytes(image: FakeImage): Long = 4
    }

    private class Harness(scope: TestScope, source: FakeSource) {
        var position = 0L
        val frames = mutableListOf<String?>()
        val notices = mutableListOf<String>()
        val controller = PGSOverlayController(
            source = source,
            scope = scope.backgroundScope,
            fileId = 42,
            sourcePositionMs = { position },
            isPlaying = { false },
            playbackSpeed = { 1f },
            onFrame = { frames += it?.cue?.id },
            onStatus = {},
            onFailure = { notices += it },
            clockMs = { scope.testScheduler.currentTime },
            workDispatcher = StandardTestDispatcher(scope.testScheduler),
        )
        val shown: String?
            get() = frames.lastOrNull()
    }

    private fun hash(letter: Char) = letter.toString().repeat(64)

    private fun cue(id: String, start: Long, end: Long, letter: Char) = PGSOverlayCue(
        id,
        start,
        end,
        1920,
        1080,
        listOf(PGSOverlayObject("overlay/$generation/objects/${hash(letter)}.png", 0, 0, 10, 10)),
    )

    // The fixture's manifest shape: c2 is on screen at 100 s, c5 starts in the
    // 20 s refresh margin of the window loaded there, c6 is out past it.
    private val manifest = PGSOverlayManifest(
        schema = 1,
        generation = generation,
        fileId = 42,
        trackIndex = 3,
        kind = "pgs",
        timebase = "source_ms",
        durationMs = 600_000,
        cues = listOf(
            cue("c2", 100_000, 104_000, 'b'),
            cue("c4", 160_000, 175_000, 'c'),
            cue("c5", 176_000, 180_000, 'd'),
            cue("c6", 300_000, 302_000, 'e'),
        ),
    )

    @Test
    fun aSeekIntoTheRefreshMarginDropsTheStaleFrameAtOnce() = runTest {
        val source = FakeSource(manifest)
        // c5's image is still on its way when the viewer seeks onto it.
        source.gated[hash('d')] = CompletableDeferred()
        val harness = Harness(this, source)
        harness.position = 101_000
        harness.controller.select(3)
        runCurrent()
        assertEquals("c2", harness.shown)

        harness.position = 177_000
        harness.controller.reconcile(seeked = true)
        runCurrent()
        // Before the fix this was still "c2", a cue that ended at 104 s, and
        // it stayed until c5's image arrived.
        assertNull(harness.shown)

        source.gated.getValue(hash('d')).complete(Unit)
        runCurrent()
        assertEquals("c5", harness.shown)
    }

    @Test
    fun aPlayerEventDoesNotCancelTheLoadThatCoversItsPosition() = runTest {
        val source = FakeSource(manifest)
        source.gated[hash('e')] = CompletableDeferred()
        val harness = Harness(this, source)
        harness.position = 101_000
        harness.controller.select(3)
        runCurrent()

        harness.position = 301_000
        harness.controller.reconcile(seeked = true)
        runCurrent()
        assertEquals(1, source.requests.count { it == hash('e') })

        // Play, pause and speed changes all call reconcile(). Each used to see
        // a position outside the loaded window and restart the load, dropping
        // the image in flight.
        repeat(3) {
            harness.controller.reconcile()
            runCurrent()
        }
        assertEquals(1, source.requests.count { it == hash('e') })

        source.gated.getValue(hash('e')).complete(Unit)
        runCurrent()
        assertEquals("c6", harness.shown)
    }

    @Test
    fun aFailedWindowIsSaidOnceAndRetriedOnABoundedBackoff() = runTest {
        val source = FakeSource(manifest)
        source.failing += hash('b')
        val harness = Harness(this, source)
        harness.position = 101_000
        harness.controller.select(3)
        runCurrent()
        assertEquals(1, harness.notices.size)
        assertEquals(1, source.requests.count { it == hash('b') })

        // Player events inside the backoff neither refetch nor say it again.
        repeat(3) {
            harness.controller.reconcile()
            runCurrent()
        }
        assertEquals(1, source.requests.count { it == hash('b') })

        // The retries: after 5 s, then 30 s, then nothing more.
        advanceTimeBy(5_001)
        assertEquals(2, source.requests.count { it == hash('b') })
        advanceTimeBy(30_001)
        assertEquals(3, source.requests.count { it == hash('b') })
        advanceTimeBy(600_000)
        assertEquals(3, source.requests.count { it == hash('b') })
        assertEquals(1, harness.notices.size)

        // The viewer's seek ends the backoff, and a success is shown.
        source.failing.clear()
        harness.controller.reconcile(seeked = true)
        runCurrent()
        assertEquals(4, source.requests.count { it == hash('b') })
        assertEquals("c2", harness.shown)
        assertEquals(1, harness.notices.size)
    }
}
