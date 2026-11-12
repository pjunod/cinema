package tv.plurx.app.data

import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import retrofit2.Response
import retrofit2.http.GET
import retrofit2.http.POST

/** Offload: Curator removed a title's files to free disk; the item stays, flagged. */
class OffloadTest {
    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    @Test
    fun anOffloadedLeafDecodesFlaggedAndAnOlderServerDecodesUnflagged() {
        val off = json.decodeFromString(Item.serializer(), """{"id":1,"kind":"movie","title":"Dune","offloaded":true}""")
        assertTrue(off.offloaded)
        assertTrue(off.hasOffloadedLeaves)
        val plain = json.decodeFromString(Item.serializer(), """{"id":2,"kind":"movie","title":"Heat"}""")
        assertFalse(plain.offloaded)
        assertFalse(plain.hasOffloadedLeaves)
    }

    @Test
    fun aPartlyOffloadedShowCarriesACountNotTheFlag() {
        val show = json.decodeFromString(
            Item.serializer(),
            """{"id":3,"kind":"show","title":"Severance","rollup":{"leaves":10,"watched":2,"offloaded":4}}""",
        )
        assertFalse(show.offloaded)
        assertTrue(show.hasOffloadedLeaves)
        assertEquals(4L, show.rollup?.offloaded)
        val old = json.decodeFromString(Item.serializer(), """{"id":4,"kind":"show","title":"Old","rollup":{"leaves":10,"watched":2}}""")
        assertFalse(old.hasOffloadedLeaves)
    }

    @Test
    fun theRoutesMatchTheServerContract() {
        val ask = PlurxApi::class.java.declaredMethods.single { it.name == "getItBack" }
        assertEquals("items/{id}/get-it-back", requireNotNull(ask.getAnnotation(POST::class.java)).value)
        val status = PlurxApi::class.java.declaredMethods.single { it.name == "offloadStatus" }
        assertEquals("items/{id}/offload-status", requireNotNull(status.getAnnotation(GET::class.java)).value)
    }

    @Test
    fun theLibraryFilterNamesTheServersPresenceValues() {
        assertEquals(listOf("disk", "offloaded", "everything"), LibraryPresence.entries.map { it.storageValue })
        assertEquals(LibraryPresence.Disk, LibraryPresence.fromStorage(null))
        assertEquals(LibraryPresence.Offloaded, LibraryPresence.fromStorage("offloaded"))
    }

    @Test
    fun anAnswerIsReturnedAndARefusalSaysWhatTheServerSaid() {
        val ok = Response.success(OffloadAnswer(matched = true, state = "searching", message = "Curator is searching for it."))
        assertEquals("Offloaded · Curator is searching for it.", offloadStatusLine(offloadAnswerOrThrow(ok)))
        val body = """{"code":"curator_too_old","message":"Curator is too old for Get it back; update it"}"""
            .toResponseBody("application/json".toMediaType())
        val refused = Response.error<OffloadAnswer>(502, body)
        val thrown = assertThrows(Exception::class.java) { offloadAnswerOrThrow(refused) }
        assertEquals("Curator is too old for Get it back; update it", thrown.message)
        assertEquals("Offloaded · curator unreachable", OFFLOAD_UNREACHABLE)
    }

    @Test
    fun aTitleThatCameBackOrIsNoLongerOffloadedReloadsThePage() {
        assertTrue(offloadComeBack(OffloadAnswer(matched = true, state = "on_disk")))
        assertFalse(offloadComeBack(OffloadAnswer(matched = true, state = "downloading")))
        val body = """{"code":"not_offloaded","message":"this title is not offloaded"}"""
            .toResponseBody("application/json".toMediaType())
        val gone = retrofit2.HttpException(Response.error<OffloadAnswer>(409, body))
        assertTrue(offloadNoLongerOffloaded(gone))
        assertFalse(offloadNoLongerOffloaded(java.io.IOException("down")))
        assertEquals(15_000L, OFFLOAD_POLL_MS)
    }
}
