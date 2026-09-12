package tv.plurx.app.librarychannels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LibraryChannelStartFailureTest {
    @Test fun typedConflictKeepsItsRecoveryCodeAndMessage() {
        val failure = libraryChannelStartFailure(
            409,
            """{"code":"vod_source_rescan_required","message":"The source probe needs refreshing."}""",
        )

        assertEquals(409, failure.status)
        assertEquals("vod_source_rescan_required", failure.code)
        assertEquals(
            "The source probe needs refreshing. (vod_source_rescan_required, HTTP 409)",
            failure.message,
        )
        assertFalse(shouldRetryLibraryChannelStart(failure, retryOccurrenceChange = true))
    }

    @Test fun occurrenceChangeRetriesOnlyOnce() {
        val failure = libraryChannelStartFailure(
            409,
            """{"code":"channel_occurrence_changed","message":"Resolve the channel again."}""",
        )

        assertTrue(shouldRetryLibraryChannelStart(failure, retryOccurrenceChange = true))
        assertFalse(shouldRetryLibraryChannelStart(failure, retryOccurrenceChange = false))
        assertFalse(shouldRetryLibraryChannelStart(
            libraryChannelStartFailure(503, """{"code":"channel_occurrence_changed"}"""),
            retryOccurrenceChange = true,
        ))
    }

    @Test fun legacyAndMalformedBodiesStillProduceReadableFailures() {
        assertEquals(
            "The session already exists (HTTP 409)",
            libraryChannelStartFailure(409, """{"error":"The session already exists"}""").message,
        )
        assertEquals(
            "The scheduled programme could not start (HTTP 409).",
            libraryChannelStartFailure(409, "not JSON").message,
        )
    }
}
