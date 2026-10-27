package tv.plurx.app.player

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.*

/** Synthetic grant-bound protocol evidence; no Source server or device playback is claimed. */
class SharedProgressiveCaptionsTest {
    private val asset = SharedCaptionAsset("b".repeat(64), "d".repeat(64), "model", "e".repeat(64), "c".repeat(64), 1, "ja", "ja", "transcribe", 120_000)
    private val lease = "44444444-4444-4444-8444-444444444444"
    private fun segment() = CaptionSegment(1, asset.asset_id, "f".repeat(64), 0, 30_000,
        listOf(CaptionCue("1".repeat(64), 1000, 2000, "<literal> 日本語")),
        CaptionProvenance(asset.profile_id, asset.content_revision, "1", asset.model_sha256))
    private fun bytes() = Json.encodeToString(segment()).encodeToByteArray()
    private fun ref() = CaptionSegmentRef(segment().segment_id, captionDigest(bytes()), 0, 30_000)
    private fun manifest() = SharedCaptionManifest(1, asset.asset_id, asset.content_revision, asset.profile_id, 1, "ja", "ja", "transcribe",
        120_000, "9007199254740993", "stopped", null, CaptionSnapshot(lease, "9007199254740993", 30_000),
        listOf(SharedCaptionCoverage(0, 30_000)), listOf(ref()))
    private suspend fun binding(): SharedCaptionReadBinding {
        val fixture = SharedFixture("901"); fixture.login()
        return SharedCaptionReadBinding(fixture.context().withSession(fixture.session(1)), 1, false)
    }
    private fun refuses(block: () -> Unit) { try { block(); fail("Expected protocol refusal") } catch (_: IllegalArgumentException) { } }

    @Test fun sharedReadsUseOnlyClosedOpaqueGetResourcesAndNeverEnqueue(): Unit = runBlocking {
        val binding = binding(); val calls = mutableListOf<Pair<String, Map<String, String>>>()
        val source = SharedCaptionReadTransport { actual, resource, query, maximum ->
            assertEquals(binding.identity, actual.identity); assertTrue(maximum in listOf(16_384L, 65_536L))
            calls += resource to query
            when { resource == "captions" -> Json.encodeToString(SharedCaptionCatalog(1, listOf(asset))).encodeToByteArray()
                resource.endsWith("/manifest") -> Json.encodeToString(manifest()).encodeToByteArray()
                else -> bytes() }
        }
        val reader = SharedCaptionReader(source, { binding.identity })
        assertEquals(asset, reader.catalog(binding).assets.single())
        val value = reader.window(binding, asset, 1500)
        assertEquals("<literal> 日本語", value.segments.values.single().cues.single().text)
        assertEquals(3, calls.size)
        assertEquals("1500", calls[1].second["position_ms"]); assertEquals(lease, calls[2].second["lease"])
        assertTrue(calls.all { it.first == "captions" || it.first.startsWith("captions/${asset.asset_id}/") })
        assertFalse(calls.any { listOf("operations", "demands", "retry", "install", "stop").any(it.first::contains) })
        val method = PlurxApi::class.java.methods.single { it.name == "sharedCaptionReadPath" }
        assertNotNull(method.getAnnotation(retrofit2.http.GET::class.java)); assertNull(method.getAnnotation(retrofit2.http.POST::class.java))
    }
    @Test fun canonicalOpaqueGrammarRefusesProducerPathsQueriesAndBorrowedLocalIds(): Unit = runBlocking {
        val bound = binding().context
        assertTrue(bound.path("captions").contains("?session="))
        assertTrue(bound.path("captions", mapOf("after" to asset.asset_id)).contains("after=${asset.asset_id}"))
        assertTrue(bound.path("captions/${asset.asset_id}/manifest", mapOf("position_ms" to "1500")).contains("position_ms=1500"))
        refuses { bound.path("captions/${asset.asset_id}/manifest") }
        refuses { bound.path("captions/${asset.asset_id}/manifest", mapOf("position_ms" to "01")) }
        refuses { bound.path("captions/${asset.asset_id}/manifest", mapOf("position_ms" to "14400001")) }
        refuses { bound.path("captions/${asset.asset_id}/segments/${ref().segment_id}", mapOf("lease" to "00000000-0000-0000-0000-000000000000")) }
        refuses { bound.path("captions", mapOf("demand" to "administrator")) }
        refuses { bound.path("captions/${asset.asset_id}/stop") }
        refuses { PlaybackFileContext.local(7).path("captions") }
    }
    @Test fun aPrincipalChangeDuringTheStreamCannotAdoptEvenValidCaptionBytes(): Unit = runBlocking {
        val binding = binding(); var current: String? = binding.identity
        val source = SharedCaptionReadTransport { _, _, _, _ -> current = "different-grant"; Json.encodeToString(SharedCaptionCatalog(1, listOf(asset))).encodeToByteArray() }
        try { SharedCaptionReader(source, { current }).catalog(binding); fail("Expected stale grant refusal") } catch (_: IllegalArgumentException) { }
    }
    @Test fun sourceReadLeasesCannotCrossAStartedSessionPrincipal(): Unit = runBlocking {
        val first = binding(); val other = first.copy(context = first.context.withSession("aaaaaaaa-aaaa-4aaa-8aaa-000000000002"))
        assertNotEquals(first.identity, other.identity)
        var reads = 0
        val reader = SharedCaptionReader(SharedCaptionReadTransport { _, _, _, _ -> reads++; error("Stale principal reached transport") }, { other.identity })
        try { reader.window(first, asset, 1000); fail("Expected principal refusal") } catch (_: IllegalArgumentException) { }
        assertEquals(0, reads)
    }
    @Test fun hashVerificationPrecedesUtf8AndRejectsWrongModelOrSourceProvenance() {
        try { verifiedSharedCaptionSegment(byteArrayOf(-1), ref(), asset); fail("Expected digest refusal") }
        catch (e: IllegalArgumentException) { assertTrue(e.message!!.contains("checksum")) }
        val wrong = segment().copy(provenance = segment().provenance.copy(model_sha256 = "2".repeat(64)))
        val wrongBytes = Json.encodeToString(wrong).encodeToByteArray()
        refuses { verifiedSharedCaptionSegment(wrongBytes, ref().copy(digest = captionDigest(wrongBytes)), asset) }
    }
    @Test fun seekReadsTheRequestedSourceWindowAndEvictsEarlierCuesWithoutCreatingWork(): Unit = runBlocking {
        val binding = binding(); val positions = mutableListOf<String>()
        val reader = SharedCaptionReader(SharedCaptionReadTransport { _, resource, query, _ ->
            if (resource.endsWith("/manifest")) {
                positions += query.getValue("position_ms")
                Json.encodeToString(if (query["position_ms"] == "100000") manifest().copy(coverage = emptyList(), segments = emptyList()) else manifest()).encodeToByteArray()
            } else bytes()
        }, { binding.identity })
        val window = CaptionWindow(); val first = reader.window(binding, asset, 1500)
        window.replace(first.segments.values.toList(), 1500, asset.duration_ms); assertFalse(window.textAt(1500).isEmpty())
        val later = reader.window(binding, asset, 100_000, first.segments)
        window.replace(later.segments.values.toList(), 100_000, asset.duration_ms); assertTrue(window.cues.isEmpty())
        assertEquals(listOf("1500", "100000"), positions)
    }
    @Test fun anExpiredReadLeaseCannotAdoptAWindowAfterSegmentTransfer(): Unit = runBlocking {
        val binding = binding(); var now = 1000L
        val reader = SharedCaptionReader(SharedCaptionReadTransport { _, resource, _, _ ->
            if (resource.endsWith("/manifest")) Json.encodeToString(manifest()).encodeToByteArray() else { now = 31_001; bytes() }
        }, { binding.identity }, { now })
        try { reader.window(binding, asset, 1500); fail("Expected lease expiry") } catch (_: IllegalArgumentException) { }
    }
    @Test fun exactSourceAudioIsRequiredAndNoLanguageGuessSelectsCachedCaptions(): Unit = runBlocking {
        val binding = binding().copy(audioOrdinal = 2)
        val reader = SharedCaptionReader(SharedCaptionReadTransport { _, _, _, _ -> error("Wrong audible ordinal reached Source") }, { binding.identity })
        try { reader.window(binding, asset, 1000); fail("Expected audio mismatch") } catch (_: IllegalArgumentException) { }
    }

    @Test fun sourceVerificationIsAReadOnlyWaitRatherThanMissingCoverageOrPermission() {
        assertTrue(sharedCaptionReadVerifying(RefusalException(425, "source_verifying", "private Source detail", null)))
        assertFalse(sharedCaptionReadVerifying(RefusalException(403, "permission_denied", "private Source detail", null)))
        assertFalse(sharedCaptionReadVerifying(RefusalException(425, "other_reason", "private Source detail", null)))
    }

}
