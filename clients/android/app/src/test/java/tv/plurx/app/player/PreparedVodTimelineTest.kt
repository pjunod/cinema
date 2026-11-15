package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class PreparedVodTimelineTest {
    private val segments = List(3) { index ->
        PreparedVodSegment("seg${index.toString().padStart(5, '0')}.m4s", index * 1_000_000L,
            1_000_000L, "init.mp4")
    }
    private fun fullVod(origin: Long = 0, vod: Boolean = true, end: Boolean = true, sequence: Long = 0,
        durationUs: Long = 3_000_000, sourceMs: Long = 3000, startUs: Long? = null,
        positiveHint: Boolean = false, precise: Boolean = false, noParts: Boolean = true,
        entries: List<PreparedVodSegment> = segments): Boolean =
        preparedFilmLocalVod(origin, vod, end, sequence, durationUs, sourceMs, startUs,
            positiveHint, precise, noParts, entries)

    @Test fun serverDirectedFullVodUsesFilmTimeWithoutAnAutoCandidateRequest() {
        // No Auto policy state participates: these are the received full-film
        // manifest facts of the server-directed processed successor.
        assertTrue(fullVod())
        assertTrue(fullVod(startUs = 1_404_089L, positiveHint = true))
        assertFalse(fullVod(origin = 1_404_089L))
    }

    @Test fun acquisitionHintNeverRebasesOrTruncatesTheFilmTimeline() {
        assertTrue(fullVod(startUs = 2_000_000, positiveHint = true))
        assertFalse(fullVod(startUs = 3_000_001, positiveHint = true))
        assertFalse(fullVod(startUs = 2_000_000, positiveHint = false))
        assertFalse(fullVod(startUs = 2_000_000, positiveHint = true, precise = true))
        assertFalse(fullVod(entries = segments.drop(1), sequence = 1))
        assertFalse(fullVod(entries = segments.take(2), durationUs = 2_000_000))
    }

    @Test fun immutableProofRejectsGrowingPartialGappedAndForeignMedia() {
        assertFalse(fullVod(vod = false))
        assertFalse(fullVod(end = false))
        assertFalse(fullVod(noParts = false))
        assertFalse(fullVod(entries = emptyList()))
        assertFalse(fullVod(sourceMs = 0))
        assertFalse(fullVod(entries = segments.toMutableList().also { it[1] = it[1].copy(gap = true) }))
        assertFalse(fullVod(entries = segments.toMutableList().also { it[1] = it[1].copy(startUs = 1_000_001) }))
        assertFalse(fullVod(entries = segments.toMutableList().also { it[1] = it[1].copy(url = "../foreign.m4s") }))
        assertFalse(fullVod(entries = segments.toMutableList().also { it[1] = it[1].copy(initializationUrl = "other.mp4") }))
        assertFalse(fullVod(durationUs = 4_000_000))
    }

    @Test fun onlyMicrosecondExtinfRoundingCanFallShortOfKnownSourceDuration() {
        assertTrue(fullVod(sourceMs = 3001))
        assertFalse(fullVod(sourceMs = 3002))
        assertTrue(fullVod(sourceMs = 2500))
        assertFalse(fullVod(sourceMs = 1000))
        assertFalse(fullVod(sourceMs = Long.MAX_VALUE))
    }

    @Test fun frameGridRoundingIsOutwardRatherThanAnExtraShortfallAllowance() {
        // ceil(1001ms * 24fps) = 25 frames; six-decimal EXTINF is 1.041667s.
        assertTrue(fullVod(durationUs = 1_041_667, sourceMs = 1001,
            entries = listOf(PreparedVodSegment("seg00000.m4s", 0, 1_041_667, "init.mp4"))))
        assertFalse(fullVod(durationUs = 1_000_000, sourceMs = 1042,
            entries = listOf(PreparedVodSegment("seg00000.m4s", 0, 1_000_000, "init.mp4"))))
    }

    @Test fun longFilmTransportIsIndependentOfCompletedArtifactCountLimits() {
        val count = 8400 // 4h40m at the served two-second grid exceeds the artifact policy's 8192.
        val entries = (0 until count).asSequence().map { index ->
            PreparedVodSegment("seg${index.toString().padStart(5, '0')}.m4s", index * 2_000_000L,
                2_000_000L, "init.mp4")
        }.asIterable()
        assertTrue(preparedFilmLocalVod(0, true, true, 0, count * 2_000_000L, count * 2000L,
            null, false, false, true, entries))
    }

    @Test fun transportAdoptionDiagnosticExposesOnlyBoundedClockFactsAndFiniteReason() {
        val detail = preparedVodTimelineDetail(PreparedVodTimelineProof(PreparedVodProofReason.FullVod,
            1_413_412_000), 0, 1_413_400)
        assertEquals("vod=true o=0 d_us=1413412000 source_ms=1413400 reason=full_vod", detail)
        for (reason in PreparedVodProofReason.entries) {
            val rejected = preparedVodTimelineDetail(PreparedVodTimelineProof(reason),
                Long.MAX_VALUE, PlaybackControl.MAX_MEDIA_MILLIS)
            assertTrue(rejected.length <= 200)
            assertTrue(rejected.endsWith("reason=${reason.wire}"))
        }
    }

    @Test fun timelineProofCannotMigrateAcrossPlayerStageOriginMutationOrAuthorization() {
        val player = Any()
        val owner = PreparedTimelineOwner(player, "action", "session", 0, 7,
            "https://cinema.example", 9)
        fun current(pipeline: Any? = player, action: String? = "action", session: String? = "session",
            origin: Long? = 0, mutation: Long = 7, credentialOrigin: String = "https://cinema.example",
            authorization: Long = 9) = owner.isCurrent(pipeline, action, session, origin, mutation,
                credentialOrigin, authorization)
        assertTrue(current())
        assertFalse(current(pipeline = Any())) // rollback to predecessor / later player
        assertFalse(current(pipeline = null)) // released stage
        assertFalse(current(action = "later-action"))
        assertFalse(current(session = "later-session"))
        assertFalse(current(origin = 1000))
        assertFalse(current(mutation = 8)) // seek or media replacement
        assertFalse(current(credentialOrigin = "https://other.example"))
        assertFalse(current(authorization = 10)) // same origin, new credential owner
    }
}
