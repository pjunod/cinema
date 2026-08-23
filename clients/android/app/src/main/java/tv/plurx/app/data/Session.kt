package tv.plurx.app.data

import java.net.URLEncoder
import java.nio.charset.StandardCharsets

/**
 * Live connection state, read by the OkHttp auth interceptor (and the image /
 * player data sources) on every request. Set once at connect and after login;
 * kept as a plain holder so a single OkHttpClient serves the whole app and the
 * token can change without rebuilding it.
 */
object Session {
    /** Server origin, no trailing slash, e.g. `http://192.168.1.10:32400`. */
    @Volatile
    var origin: String = ""

    /** Bearer token, or null when signed out. */
    @Volatile
    var token: String? = null

    private val nodeLock = Any()
    private var mediaFailoverOrigins: List<String> = emptyList()
    private var mediaFailoverIndex: Int = 0

    fun configureNodeOrigins(peers: List<String>, primary: String) = synchronized(nodeLock) {
        val canonicalPrimary = canonicalOrigin(primary)
        mediaFailoverOrigins = peers.mapNotNull(::canonicalOrigin)
            .filter { it != canonicalPrimary }
            .distinct()
        mediaFailoverIndex = 0
    }

    fun resetMediaFailover() = synchronized(nodeLock) {
        mediaFailoverIndex = 0
    }

    /** Rebind a relative capability to another ingress without changing the
     * API/account origin or spending the playback compatibility ladder. */
    fun nextMediaFailoverUrl(path: String, authenticated: Boolean): String? {
        if (!path.startsWith('/') || path.startsWith("//")) return null
        val candidate = synchronized(nodeLock) {
            if (mediaFailoverIndex >= mediaFailoverOrigins.size) return null
            mediaFailoverOrigins[mediaFailoverIndex++]
        }
        val base = candidate + path
        val credential = token?.takeIf { authenticated } ?: return base
        val joiner = if ('?' in base) '&' else '?'
        return base + joiner + "token=" +
            URLEncoder.encode(credential, StandardCharsets.UTF_8.toString())
    }

    private fun canonicalOrigin(raw: String): String? = runCatching {
        val uri = java.net.URI(raw)
        if (
            uri.scheme?.lowercase() !in setOf("http", "https") ||
            uri.host == null || uri.userInfo != null || uri.query != null || uri.fragment != null ||
            (uri.path.isNotEmpty() && uri.path != "/")
        ) return null
        val host = if (':' in uri.host) "[${uri.host}]" else uri.host
        val port = if (uri.port >= 0) ":${uri.port}" else ""
        "${uri.scheme.lowercase()}://$host$port"
    }.getOrNull()

    /** Absolute URL for a server-relative path (`/api/v1/images/…`). */
    fun url(path: String): String =
        if (path.startsWith("http")) path else origin + path

    /** URL for media consumers that cannot attach the bearer header. */
    fun mediaUrl(path: String): String {
        val base = url(path)
        val credential = token ?: return base
        val joiner = if ('?' in base) '&' else '?'
        return base + joiner + "token=" + URLEncoder.encode(credential, StandardCharsets.UTF_8.toString())
    }
}
