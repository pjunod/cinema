package tv.plurx.app.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ReadAfterReceiptTest {
    @Test fun fullUnsignedIndexExpiresOnMonotonicTime() {
        var time = 10L
        val state = ReadAfterReceipt { time }
        val current = { 1L }
        val request = state.request(1, current)!!
        state.observe("18446744073709551615", true, request, current)
        time += 59_999_999_999L
        assertEquals("18446744073709551615", state.request(1, current)?.index)
        time++
        assertNull(state.request(1, current)?.index)
    }

    @Test fun lowerReplyCannotLowerOrRefreshFloor() {
        var time = 0L
        val state = ReadAfterReceipt { time }
        val current = { 1L }
        val request = state.request(1, current)!!
        state.observe("19", false, request, current)
        time = 59_000_000_000L
        state.observe("18", false, request, current)
        assertEquals("19", state.request(1, current)?.index)
        time = 60_000_000_000L
        assertNull(state.request(1, current)?.index)
    }

    @Test fun unknownWriteRetiresInFlightKnownReplies() {
        val state = ReadAfterReceipt()
        val current = { 1L }
        val old = state.request(1, current)!!
        state.observe("19", false, old, current)
        state.observe("unknown", true, old, current)
        state.observe("20", false, old, current)
        assertNull(state.request(1, current)?.index)
        val fresh = state.request(1, current)!!
        state.observe("21", false, fresh, current)
        assertEquals("21", state.request(1, current)?.index)
        state.observe(null, true, old, current)
        assertNull(state.request(1, current)?.index)
    }

    @Test fun transportFailureOnlyRetiresMutatingRequests() {
        val state = ReadAfterReceipt()
        val current = { 1L }
        val request = state.request(1, current)!!
        state.observe("19", false, request, current)
        state.transportFailed(false, request, current)
        state.observe(null, false, request, current)
        assertEquals("19", state.request(1, current)?.index)
        state.transportFailed(true, request, current)
        state.observe("20", false, request, current)
        assertNull(state.request(1, current)?.index)
    }

    @Test fun authChangeFencesOldRequestsAndResponses() {
        var generation = 1L
        val state = ReadAfterReceipt()
        val current = { generation }
        val old = state.request(1, current)!!
        state.observe("19", false, old, current)
        generation = 2
        assertNull(state.request(1, current))
        val fresh = state.request(2, current)!!
        assertNull(fresh.index)
        state.observe("20", false, fresh, current)
        state.observe("99", false, old, current)
        state.transportFailed(true, old, current)
        assertEquals("20", state.request(2, current)?.index)
    }

    @Test fun malformedIndicesCannotBecomeFloors() {
        val state = ReadAfterReceipt()
        val current = { 1L }
        listOf("0", "01", "-1", "+1", "18446744073709551616", "12, 13", " 12", "12\n", "").forEach {
            val request = state.request(1, current)!!
            state.observe("19", false, request, current)
            state.observe(it, false, request, current)
            assertNull(it, state.request(1, current)?.index)
        }
    }
}
