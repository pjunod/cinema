package tv.plurx.app.data

import okhttp3.HttpUrl
import okhttp3.Interceptor
import okhttp3.Response

/** Capture is deliberately not a data class: its default toString must not
 * turn a bearer into a diagnostic log field. */
internal class ReadAfterAuthorization(val origin: String, val token: String?, val generation: Long)

/** Actual exchange adapter; preserves the existing auth/status/error policy. */
internal class ReadAfterInterceptor(
    private val authorization: () -> ReadAfterAuthorization,
    private val receipts: ReadAfterReceipt = ReadAfterReceipt(),
) : Interceptor {
    private class Scope(val url: HttpUrl, val generation: Long)

    override fun intercept(chain: Interceptor.Chain): Response {
        val original = chain.request()
        val captured = authorization()
        val ticket = if (!captured.token.isNullOrEmpty() &&
            original.header("Authorization") == "Bearer ${captured.token}" &&
            isAccountAPI(original.url, captured.origin)
        ) receipts.request(captured.generation) { authorization().generation } else null
        val request = original.newBuilder().removeHeader("X-Plurx-Read-After").apply {
            if (ticket?.index != null) header("X-Plurx-Read-After", ticket.index)
            if (ticket != null) tag(Scope::class.java, Scope(original.url, ticket.generation))
        }.build()
        val mutating = request.method != "GET" && request.method != "HEAD"
        val response = try {
            chain.proceed(request)
        } catch (error: Exception) {
            if (ticket != null) receipts.transportFailed(mutating, ticket) { authorization().generation }
            throw error
        }
        if (ticket != null) {
            // Redirect/auth-retry answers cannot attest the original write.
            if (response.priorResponse != null || response.request.url != original.url) {
                receipts.transportFailed(mutating, ticket) { authorization().generation }
            } else {
                // Duplicate headers are malformed, not an arbitrarily chosen
                // first value that might pretend to be an indexed write.
                val values = response.headers.values("X-Plurx-Commit-Index")
                val value = when (values.size) { 0 -> null; 1 -> values.single(); else -> "unknown" }
                receipts.observe(value, mutating, ticket) { authorization().generation }
            }
        }
        return response
    }

    /** Must be installed after auth as a network interceptor as well. OkHttp
     * handles redirects internally, without revisiting application interceptors. */
    val redirectGuard = Interceptor { chain ->
        val request = chain.request()
        val scope = request.tag(Scope::class.java)
        val safe = scope != null && request.url == scope.url &&
            authorization().generation == scope.generation
        chain.proceed(if (safe) request else request.newBuilder().removeHeader("X-Plurx-Read-After").build())
    }

    private fun isAccountAPI(url: HttpUrl, origin: String): Boolean {
        val primary = Session.canonicalOrigin(origin) ?: return false
        val host = if (':' in url.host) "[${url.host}]" else url.host
        val default = if (url.scheme == "https") 443 else 80
        val port = if (url.port == default) "" else ":${url.port}"
        return url.username.isEmpty() && url.password.isEmpty() &&
            "${url.scheme}://$host$port" == primary &&
            url.encodedPath.startsWith("/api/v1/") && url.pathSegments.getOrNull(2) != "server"
    }
}

internal object ReadAfterNetworking {
    val interceptor = ReadAfterInterceptor({
        val captured = Session.playbackAuthorization()
        ReadAfterAuthorization(captured.origin, captured.token, captured.generation)
    })
}
