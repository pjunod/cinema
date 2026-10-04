package tv.plurx.app.player

import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody
import okio.Buffer
import okio.BufferedSource
import okio.ForwardingSource
import okio.buffer

/** Keep each captured media call cancellable through body retirement, not
 * merely response headers. JSON/control calls use a separate owner. */
internal class ContinuousMediaCalls(private val delegate: Call.Factory) : Call.Factory {
    private val closed = AtomicBoolean()
    private val calls = ConcurrentHashMap.newKeySet<Call>()
    override fun newCall(request: Request): Call {
        if (closed.get()) throw IOException("Continuous media calls closed")
        val call = delegate.newCall(request)
        calls.add(call)
        if (calls.size > 32) { calls.remove(call); call.cancel(); throw IOException("Continuous media call bound") }
        if (closed.get()) call.cancel()
        return object : Call by call {
            override fun execute(): Response = try { retainBody(call, call.execute()) }
                catch (error: Exception) { calls.remove(call); throw error }
            override fun enqueue(responseCallback: Callback) {
                try {
                    call.enqueue(object : Callback {
                        override fun onFailure(call: Call, e: IOException) { calls.remove(call); responseCallback.onFailure(call, e) }
                        override fun onResponse(call: Call, response: Response) {
                            val retained = retainBody(call, response)
                            try { responseCallback.onResponse(call, retained) }
                            catch (error: Exception) { retained.close(); throw error }
                        }
                    })
                } catch (error: Exception) { calls.remove(call); throw error }
            }
            override fun clone(): Call = newCall(request)
        }
    }
    private fun retainBody(call: Call, response: Response): Response {
        val body = response.body ?: run { calls.remove(call); return response }
        val input = object : ForwardingSource(body.source()) {
            override fun read(sink: Buffer, byteCount: Long): Long = try { super.read(sink, byteCount) }
                catch (error: Exception) { call.cancel(); throw error }
            override fun close() { try { super.close() } finally { calls.remove(call) } }
        }.buffer()
        return response.newBuilder().body(object : ResponseBody() {
            override fun contentType() = body.contentType()
            override fun contentLength() = body.contentLength()
            override fun source(): BufferedSource = input
        }).build()
    }
    fun cancel() { closed.set(true); calls.toList().forEach { it.cancel() } }
    fun isQuiescent(): Boolean = calls.isEmpty()
}
