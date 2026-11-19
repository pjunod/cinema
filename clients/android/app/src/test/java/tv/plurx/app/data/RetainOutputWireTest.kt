package tv.plurx.app.data

import kotlinx.serialization.encodeToString
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The mid-playback "Keep a reusable copy" off call must put the field on the
 * wire. The shared [Net.json] omits default values, and the server refuses
 * `{}` with 400, so a defaulted field would silently never work.
 */
class RetainOutputWireTest {
    @Test
    fun midPlaybackOffBodyNamesTheField() {
        assertEquals("""{"retain_output":false}""", Net.json.encodeToString(RetainOutputReq(retain_output = false)))
    }

    @Test
    fun createOverrideIsOmittedUntilTheViewerTurnsItOff() {
        assertEquals("{}", Net.json.encodeToString(SessionOverrides()))
        assertEquals("""{"retain_output":false}""", Net.json.encodeToString(SessionOverrides(retain_output = false)))
    }
}
