package tv.plurx.app.player

import java.io.IOException
import okhttp3.*
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Timeout
import org.junit.Assert.*
import org.junit.Test

class ContinuousMediaCallsTest {
    private class PendingCall(private val request: Request) : Call {
        var cancelled = false
        private var callback: Callback? = null
        override fun request() = request
        override fun execute(): Response = Response.Builder().request(request).protocol(Protocol.HTTP_1_1)
            .code(200).message("OK").body("verified media".toResponseBody()).build()
        override fun enqueue(responseCallback: Callback) { callback = responseCallback }
        override fun cancel() {
            cancelled = true
            callback?.onFailure(this, IOException("cancelled before headers"))
        }
        override fun isExecuted() = callback != null
        override fun isCanceled() = cancelled
        override fun timeout() = Timeout()
        override fun clone(): Call = PendingCall(request)
    }

    @Test fun mediaCallsRemainOwnedThroughBodyCloseAndCancelBeforeHeadersWithoutTouchingControl() {
        val request = Request.Builder().url("http://localhost/media").build()
        val created = mutableListOf<PendingCall>()
        val factory = Call.Factory { PendingCall(it).also(created::add) }
        val control = factory.newCall(request)
        val media = ContinuousMediaCalls(factory)
        val response = media.newCall(request).execute()
        assertFalse(media.isQuiescent())
        assertEquals("verified media", response.body!!.string())
        assertTrue(media.isQuiescent())
        var failed = false
        media.newCall(request).enqueue(object : Callback {
            override fun onResponse(call: Call, response: Response) { error("No headers arrived") }
            override fun onFailure(call: Call, e: IOException) { failed = true }
        })
        assertFalse(media.isQuiescent())
        media.cancel()
        assertTrue(failed)
        assertTrue(created.last().cancelled)
        assertFalse(control.isCanceled())
        assertTrue(media.isQuiescent())
        val count = created.size
        assertTrue(runCatching { media.newCall(request) }.exceptionOrNull() is IOException)
        assertEquals(count, created.size)
    }
}
