package tv.plurx.app.livetv

import androidx.media3.common.PlaybackException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Net

@OptIn(ExperimentalCoroutinesApi::class)
class LiveTvTest {
    private val channel = LiveTvChannel("one", "7.1", "Fixture News")
    private class Store : LiveTvBarrierStore {
        var value = false
        var fail = false
        override fun pending() = value
        override fun setPending(pending: Boolean) {
            if (fail) error("storage unavailable")
            value = pending
        }
    }
    private inner class Requests : LiveTvRequests {
        val events = mutableListOf<String>()
        var error: String? = null
        var releaseFails = false
        var response: CompletableDeferred<LiveTvStarted>? = null
        override suspend fun start(channel: String): LiveTvStarted {
            events += "start:$channel"
            error?.let { throw LiveTvFailure(it) }
            return response?.await() ?: LiveTvStarted("cap-$channel", this@LiveTvTest.channel, true)
        }
        override suspend fun release(capability: String) {
            events += "release:$capability"
            if (releaseFails) throw LiveTvFailure("owner_unavailable")
        }
    }
    private suspend fun failure(code: String, action: suspend () -> Unit) {
        try { action(); fail("expected $code") } catch (error: LiveTvFailure) { assertEquals(code, error.code) }
    }

    @Test fun protectedChannelRemainsVisibleButCannotBeWatched() {
        assertTrue(channel.watchable)
        assertFalse(channel.copy(drm = true).watchable)
        assertFalse(channel.copy(support = "drm_unsupported").watchable)
        assertEquals("7.1 · Fixture News", channel.title)
    }

    @Test fun everySwitchConfirmsReleaseBeforeNextStart() = runTest {
        val requests = Requests()
        val store = Store()
        val lease = LiveTvLease(requests, LiveTvStartBarrier(store) { 0 }, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        lease.start("one").await()
        assertTrue(store.value)
        lease.start("two").await()
        lease.stop().await()
        assertEquals(listOf("start:one", "release:cap-one", "start:two", "release:cap-two"), requests.events)
        assertNull(lease.current)
        assertFalse(store.value)
    }

    @Test fun failedDeleteRetainsCapabilityAndBlocksSecondAllocation() = runTest {
        val requests = Requests()
        val store = Store()
        val lease = LiveTvLease(requests, LiveTvStartBarrier(store) { 0 }, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        lease.start("one").await()
        requests.releaseFails = true
        failure("owner_unavailable") { lease.stop().await() }
        failure("owner_unavailable") { lease.start("two").await() }
        assertEquals("cap-one", lease.current?.session_id)
        assertTrue(store.value)
        assertEquals(1, requests.events.count { it.startsWith("start:") })
        requests.releaseFails = false
        lease.stop().await()
        assertFalse(store.value)
    }

    @Test fun lateStartAfterStopIsReleasedBeforeItsCallerReturns() = runTest {
        val requests = Requests().apply { response = CompletableDeferred() }
        val lease = LiveTvLease(requests, LiveTvStartBarrier(Store()) { 0 }, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        val started = lease.start("one")
        val stopped = lease.stop()
        requests.response!!.complete(LiveTvStarted("late", channel, true))
        assertNull(started.await())
        stopped.await()
        assertEquals(listOf("start:one", "release:late"), requests.events)
    }

    @Test fun cancellingCallerDoesNotCancelPendingLeaseBookkeeping() = runTest {
        val requests = Requests().apply { response = CompletableDeferred() }
        val scope = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))
        val lease = LiveTvLease(requests, LiveTvStartBarrier(Store()) { 0 }, scope)
        val started = lease.start("one")
        // A screen cancels its awaiter, not the application-owned deferred.
        val waiter = launch { started.await() }
        waiter.cancel()
        val stopped = lease.stop()
        requests.response!!.complete(LiveTvStarted("late", channel, true))
        stopped.await()
        assertNull(lease.current)
        assertEquals(listOf("start:one", "release:late"), requests.events)
    }

    @Test fun ambiguousTypedOwnerFailurePersistsAcrossProfilesAndRestart() = runTest {
        var now = 0L
        val store = Store()
        val scope = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))
        val barrier = LiveTvStartBarrier(store) { now }
        val first = Requests().apply { error = "owner_unavailable" }
        failure("start_outcome_unknown") { LiveTvLease(first, barrier, scope).start("one").await() }
        val other = Requests()
        failure("start_outcome_unknown") { LiveTvLease(other, barrier, scope).start("two").await() }
        now = 50_000
        val restarted = LiveTvStartBarrier(store) { now }
        failure("start_outcome_unknown") { LiveTvLease(other, restarted, scope).start("two").await() }
        now = 139_999
        assertTrue(restarted.pending)
        now = 140_000
        assertFalse(restarted.pending)
        assertTrue(other.events.isEmpty())
    }

    @Test fun definitiveCapacityRejectionClearsMarker() = runTest {
        val store = Store()
        val requests = Requests().apply { error = "tuner_capacity" }
        val lease = LiveTvLease(requests, LiveTvStartBarrier(store) { 0 }, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        failure("tuner_capacity") { lease.start("one").await() }
        assertFalse(store.value)
        requests.error = null
        lease.start("one").await()
        lease.stop().await()
    }

    @Test fun activePlaybackRetainsDiskMarkerForCrashRecovery() = runTest {
        val store = Store()
        val barrier = LiveTvStartBarrier(store) { 0 }
        val lease = LiveTvLease(Requests(), barrier, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        lease.start("one").await()
        assertFalse(barrier.pending)
        assertTrue(store.value)
        assertTrue(LiveTvStartBarrier(store) { 200_000 }.pending)
        lease.stop().await()
        assertFalse(LiveTvStartBarrier(store) { 200_000 }.pending)
    }

    @Test fun unavailableStoragePreventsStartButDoesNotPreventCleanup() = runTest {
        val store = Store().apply { fail = true }
        val requests = Requests()
        val lease = LiveTvLease(requests, LiveTvStartBarrier(store) { 0 }, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        failure("storage_unavailable") { lease.start("one").await() }
        assertTrue(requests.events.isEmpty())
        store.fail = false
        lease.start("one").await()
        store.fail = true
        lease.stop().await()
        assertEquals(listOf("start:one", "release:cap-one"), requests.events)
    }

    @Test fun renderedFramesSurviveSlidingWindowPositionsAndFrozenVideoExpires() {
        var now = 0L
        val watchdog = LiveTvWatchdog { now }
        // These healthy window-relative positions regress; they must not be
        // used as an all-time progress maximum. Only rendered frames count.
        listOf(20, 21, 18, 19, 16, 17, 14, 15, 12).forEachIndexed { index, _ ->
            now += 5_000
            assertTrue(watchdog.observe((index + 1) * 120, true))
            assertFalse(watchdog.expired)
        }
        now += 29_999
        assertFalse(watchdog.observe(1080, true))
        assertFalse(watchdog.expired)
        now += 1
        assertTrue(watchdog.expired)
        assertTrue(watchdog.observe(24, true)) // Decoder replacement counter reset.
        now += 30_000
        assertFalse(watchdog.observe(48, false)) // Paused frames cannot renew.
        assertTrue(watchdog.expired)
    }

    @Test fun decoderErrorsAreNotConfusedWithNetworkFailures() {
        assertEquals("codec_unsupported", liveTvPlaybackErrorCode(PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED))
        assertEquals("stream_failed", liveTvPlaybackErrorCode(PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED))
    }

    @Test fun configurationEnableAndExactOwnerRecoveryHaveSeparateCasShapes() {
        val configuration = LiveTvSettingsChange.Configure("192.168.4.20", "owner-new", 2, 720).body(19)
        assertEquals(setOf("live_tv_config_generation", "live_tv_device_ipv4", "live_tv_owner_node_id", "live_tv_max_sessions", "live_tv_output_height"), configuration.keys)
        val enabled = LiveTvSettingsChange.Enabled(true).body(20)
        assertEquals(setOf("live_tv_config_generation", "live_tv_enabled"), enabled.keys)
        val recovery = LiveTvSettingsChange.FencedOwner("owner-original", 18).body(21)
        assertEquals(setOf("live_tv_config_generation", "live_tv_fenced_owner"), recovery.keys)
        val tuple = recovery.getValue("live_tv_fenced_owner").jsonObject
        assertEquals("owner-original", tuple.getValue("owner_node_id").jsonPrimitive.content)
        assertEquals("18", tuple.getValue("drain_before_generation").jsonPrimitive.content)
        assertEquals("true", tuple.getValue("stopped_and_restart_prevented").jsonPrimitive.content)
    }

    @Test fun actualSnakeCaseSettingsContractPreservesOldOwnerTuple() {
        val settings = Net.json.decodeFromString<LiveTvSettings>("""{
            "live_tv_enabled":false,"live_tv_device_ipv4":"192.168.4.20",
            "live_tv_owner_node_id":"next","live_tv_max_sessions":2,"live_tv_output_height":720,
            "live_tv_config_generation":23,"live_tv_transition_from_owner_node_id":"original",
            "live_tv_transition_drain_before":21,"unrelated_secret":"ignored"
        }""")
        assertEquals("original", settings.live_tv_transition_from_owner_node_id)
        assertEquals(21L, settings.live_tv_transition_drain_before)
        assertEquals(23L, settings.live_tv_config_generation)
    }

    @Test fun playlistStaysAtOriginalOriginAndDoesNotCarryAccountToken() {
        val api = LiveTvApi("http://192.168.4.10:32400", "fixture-account-secret")
        val playlist = api.playlistUrl("cap/with?reserved#text")
        assertEquals("http://192.168.4.10:32400/api/v1/live-tv/sessions/cap%2Fwith%3Freserved%23text/index.m3u8", playlist)
        assertFalse(playlist.contains("fixture-account-secret"))
        assertTrue(api.mediaClient.interceptors.isEmpty())
        assertFalse(api.mediaClient.followRedirects)
    }
}
