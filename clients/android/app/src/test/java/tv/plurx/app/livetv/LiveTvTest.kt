package tv.plurx.app.livetv

import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import java.io.File
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.int
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Net

@OptIn(ExperimentalCoroutinesApi::class)
class LiveTvTest {
    @Test fun liveConfigurationLeavesTheTargetOffsetToThePlaylist() {
        val item = MediaItem.Builder()
            .setLiveConfiguration(
                MediaItem.LiveConfiguration.Builder().setMaxOffsetMs(8_000).build(),
            )
            .build()
        assertEquals(C.TIME_UNSET, item.liveConfiguration.targetOffsetMs)
        assertEquals(8_000L, item.liveConfiguration.maxOffsetMs)

        val source = listOf(
            File("app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt"),
            File("src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt"),
            File("clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvPlayer.kt"),
        ).firstOrNull(File::isFile)?.readText() ?: error("LiveTvPlayer.kt source not found")
        val mediaItem = source.substringAfter("output.setMediaItem(").substringBefore("output.prepare()")
        assertTrue(mediaItem.contains(
            ".setLiveConfiguration(MediaItem.LiveConfiguration.Builder().setMaxOffsetMs(8_000).build())",
        ))
        assertFalse(mediaItem.contains("setTargetOffsetMs"))
    }

    @Test fun liveStartWireIncludesRequiredProtocolVersions() {
        val caps = Net.json.decodeFromString<tv.plurx.app.data.DeviceCaps>("""{
            "v":2,"client":{"kind":"android","build":"test","ua":"fixture"},
            "video":[{"codec":"h264","present":["sdr"]}],
            "audio":["aac","ac3"],"containers":["ts"],"transports":["hls"],
            "display":{"hdr":false,"dolby_vision":false}
        }""")
        val wire = Net.json.encodeToJsonElement(LiveTvPlaybackEnvelope.from(caps)).jsonObject
        assertEquals("The start protocol version is required even with default omission", 1,
            wire["v"]?.jsonPrimitive?.int)
        assertEquals(2, wire.getValue("caps").jsonObject.getValue("v").jsonPrimitive.int)
        assertFalse(wire.containsKey("compatibility"))
    }

    private val channel = LiveTvChannel("one", "7.1", "Fixture News")

    /**
     * The hint store's in-memory half. [fail] makes every write fail the way a
     * full or read-only `noBackupFilesDir` does — which, since §3.16, must cost
     * a start nothing at all.
     */
    private class Store : LiveTvHintStore {
        var value: LiveTvStartHint? = null
        var fail = false
        override fun read() = value
        override fun remember(hint: LiveTvStartHint) { if (!fail) value = hint }
        override fun touch(at: Long) { if (!fail) value = value?.copy(touched_at = at) }
        override fun forget() { value = null }
    }
    private inner class Requests : LiveTvRequests {
        val events = mutableListOf<String>()
        var error: String? = null
        var ownerDecided: Boolean? = null
        var releaseFails = false
        var response: CompletableDeferred<LiveTvStarted>? = null
        override val recoveryRoutes = true
        override suspend fun start(channel: String, requestId: String): LiveTvStarted {
            events += "start:$channel"
            error?.let { throw LiveTvFailure(it, ownerDecided = ownerDecided) }
            return response?.await() ?: LiveTvStarted("cap-$channel", this@LiveTvTest.channel, true)
        }
        override suspend fun release(capability: String) {
            events += "release:$capability"
            if (releaseFails) throw LiveTvFailure("owner_unavailable")
        }
        override suspend fun retire(requestId: String) { events += "retire:$requestId" }
        override suspend fun resume(requestId: String) = LiveTvResumeAnswer("ended")
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

    @Test fun lineupFormatsAndLiveSignalDecodeWithoutGuessing() {
        val formatted = Net.json.decodeFromString<LiveTvChannel>("""{
            "id":"7.1","guide_number":"7.1","guide_name":"Fixture News",
            "hd":true,"video_codec":"HEVC","audio_codec":"AC4"
        }""")
        assertEquals(listOf("HD", "HEVC", "AC4"), formatted.formatBadges)
        assertEquals("HD · HEVC video · AC4 audio", formatted.sourceFormatDescription)
        assertTrue(channel.formatBadges.isEmpty())

        val measured = LiveTvChannel(
            id = "5.1", guide_number = "5.1", guide_name = "WXYZ",
            hd = true, video_codec = "hevc", audio_codec = "ac3",
            source_format = LiveTvSourceFormat(
                video_width = 3840, video_height = 2160, scan = "progressive",
                audio_channels = 6, audio_layout = "5.1", observed_at = 1_788_998_400,
            ),
        )
        assertEquals(listOf("4K", "HEVC", "AC3 5.1"), measured.formatBadges)
        assertEquals("3840×2160p · HEVC video · AC3 5.1 audio", measured.sourceFormatDescription)
        val malformed = Net.json.decodeFromString<LiveTvChannel>("""{
            "id":"9.1","guide_number":"9.1","guide_name":"Bad Optional",
            "source_format":{"video_width":{},"video_height":1080,"scan":"wrong",
            "audio_channels":2,"audio_layout":"stereo","observed_at":1788998400}
        }""")
        assertEquals(null, malformed.source_format?.validVideoWidth)
        assertEquals(1080, malformed.source_format?.validVideoHeight)
        assertEquals(null, malformed.source_format?.validScan)
        assertEquals("stereo", malformed.source_format?.validAudioLayout)

        val status = Net.json.decodeFromString<LiveTvStatus>("""{
            "state":"active","encoder":"vaapi","output_height":720,
            "signal":{"strength_percent":96,"quality_percent":89,"symbol_quality_percent":100}
        }""")
        assertEquals(96, status.signal?.strength_percent)
        assertEquals(89, status.signal?.quality_percent)
        assertEquals(100, status.signal?.symbol_quality_percent)
    }

    private fun lease(
        requests: LiveTvRequests,
        store: LiveTvHintStore,
        scope: CoroutineScope,
    ) = LiveTvLease(requests, store, scope) { 1_000 }

    @Test fun everySwitchConfirmsReleaseBeforeNextStart() = runTest {
        val requests = Requests()
        val store = Store()
        val lease = lease(requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        lease.start("one").await()
        assertNotNull(store.value)
        lease.start("two").await()
        lease.stop().await()
        assertEquals(
            listOf("start:one", "release:cap-one", "start:two", "release:cap-two"),
            requests.events.filterNot { it.startsWith("retire:") },
        )
        assertNull(lease.current)
        assertNull("a confirmed DELETE forgets the hint", store.value)
    }

    @Test fun failedDeleteRetainsCapabilityAndBlocksSecondAllocation() = runTest {
        val requests = Requests()
        val store = Store()
        val lease = lease(requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        lease.start("one").await()
        requests.releaseFails = true
        failure("owner_unavailable") { lease.stop().await() }
        failure("owner_unavailable") { lease.start("two").await() }
        assertEquals("cap-one", lease.current?.session_id)
        assertNotNull(store.value)
        assertEquals(1, requests.events.count { it.startsWith("start:") })
        requests.releaseFails = false
        lease.stop().await()
        assertNull(store.value)
    }

    @Test fun lateStartAfterStopIsReleasedBeforeItsCallerReturns() = runTest {
        val requests = Requests().apply { response = CompletableDeferred() }
        val lease = lease(requests, Store(), CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
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
        val lease = lease(requests, Store(), scope)
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

    @Test fun anUndecidedFailureKeepsItsHintAndNeverRefusesTheNextPress() = runTest {
        // The barrier this replaced turned exactly this answer into a
        // ninety-second client-side refusal that survived a restart and a
        // profile change. Guardrail §4.4: the press always reaches the owner,
        // and the hint is what lets the stray be retired first.
        val store = Store()
        val scope = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))
        val first = Requests().apply { error = "owner_unavailable"; ownerDecided = false }
        failure("owner_unavailable") { lease(first, store, scope).start("one").await() }
        val stray = checkNotNull(store.value).request_id
        assertTrue(isLiveTvRequestId(stray))

        val other = Requests()
        assertNotNull("the next press must start", lease(other, store, scope).start("two").await())
        assertTrue("and must hand the stray back first", "retire:$stray" in other.events)
        assertNotEquals(stray, checkNotNull(store.value).request_id)
    }

    @Test fun unrecognisedStartFailureKeepsTheHint() = runTest {
        // A typed 5xx this client has never seen, with no verdict. The owner may
        // already have opened a tuner, so the id must survive — and the viewer
        // is told the owner is unavailable rather than shown a code nobody
        // wrote copy for.
        val store = Store()
        val scope = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))
        val requests = Requests().apply { error = "encoder_spawn_failed" }
        failure("owner_unavailable") { lease(requests, store, scope).start("one").await() }
        assertNotNull("an unknown failure must leave the handle", store.value)
    }

    @Test fun anUnconfirmedReleaseRecoversWithoutAUserGesture() = runTest {
        // The heartbeat is cancelled and, on sign-out, the screen offering
        // "Stop / retry cleanup" no longer exists. The lease must come back
        // for its own capability rather than hold a tuner until idle expiry.
        val store = Store()
        val requests = Requests()
        val scope = CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler))
        val lease = lease(requests, store, scope)
        assertNotNull(lease.start("one").await())
        requests.releaseFails = true
        try { lease.stop().await(); fail("expected the DELETE to fail") } catch (_: Exception) { }
        assertNotNull("a failed release must retain its capability", lease.current)
        requests.releaseFails = false
        testScheduler.advanceTimeBy(2_001)
        testScheduler.runCurrent()
        assertNull("the retry must release it without a user gesture", lease.current)
        assertNull("and forget the hint", store.value)
        assertEquals(listOf("start:one", "release:cap-one", "release:cap-one"), requests.events)
    }

    @Test fun anOwnerDecidedCapacityRejectionForgetsItsHint() = runTest {
        val store = Store()
        val requests = Requests().apply { error = "tuner_capacity"; ownerDecided = true }
        val lease = lease(requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        failure("tuner_capacity") { lease.start("one").await() }
        assertNull("the owner decided; nothing is open to retire", store.value)
        requests.error = null
        lease.start("one").await()
        lease.stop().await()
    }

    @Test fun anActiveSessionLeavesItsHintOnDiskForTheNextProcess() = runTest {
        val store = Store()
        val lease = lease(Requests(), store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        lease.start("one").await()
        // This is the crash case: the hint is what the next run resumes from.
        assertNotNull(store.value)
        lease.stop().await()
        assertNull(store.value)
    }

    @Test fun aHintStoreThatCannotWriteNeverPreventsAStart() = runTest {
        // Guardrail §4.4. The barrier refused to start on a storage failure;
        // the cost of not writing a hint is a stray the owner reaps after 45 s,
        // which is nothing next to a television that will not tune.
        val store = Store().apply { fail = true }
        val requests = Requests()
        val lease = lease(requests, store, CoroutineScope(SupervisorJob() + UnconfinedTestDispatcher(testScheduler)))
        assertNotNull(lease.start("one").await())
        assertNull(store.value)
        lease.stop().await()
        assertEquals(listOf("start:one", "release:cap-one"), requests.events)
    }

    @Test fun renderedFrameCountRenewsOnChangeAndFrozenVideoExpires() {
        var now = 0L
        val watchdog = LiveTvWatchdog { now }
        // This pins the counter semantics the call site depends on: a changed
        // count is progress, a repeated count is a frozen picture. It does NOT
        // prove the position-versus-frames choice — the watchdog takes no
        // position at all. That choice lives at the call site in LiveTvPlayer,
        // which feeds videoDecoderCounters.renderedOutputBufferCount because
        // the sliding live window moves position backwards while healthy, and
        // it is unproven until a fake player drives that heartbeat.
        listOf(120, 240, 360, 480, 600, 720, 840, 960, 1080).forEach { frames ->
            now += 5_000
            assertTrue(watchdog.observe(frames, true))
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
        assertEquals(setOf("live_tv_config_generation", "live_tv_device_ipv4", "live_tv_owner_node_id", "live_tv_max_sessions", "live_tv_output_height", "live_tv_max_output_height"), configuration.keys)
        assertEquals("0", configuration.getValue("live_tv_max_output_height").jsonPrimitive.content)
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
