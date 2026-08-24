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

    /**
     * Install the server-advertised alternatives for this exact instance.
     *
     * Origins are re-validated here because they become request authorities.
     * A candidate whose scheme is weaker than the one this session is already
     * using is refused outright: an `http` voter in an `https` deployment
     * would put the household bearer — which this app's data source attaches
     * to every request, on any host — on the wire in cleartext.
     */
    fun configureNodeOrigins(peers: List<String>, primary: String) = synchronized(nodeLock) {
        val canonicalPrimary = canonicalOrigin(primary)
        val primaryScheme = canonicalPrimary?.substringBefore("://")
        mediaFailoverOrigins = peers.mapNotNull(::canonicalOrigin)
            .filter { it != canonicalPrimary }
            .filter { primaryScheme != "https" || it.startsWith("https://") }
            .distinct()
        mediaFailoverIndex = 0
    }

    fun resetMediaFailover() = synchronized(nodeLock) {
        mediaFailoverIndex = 0
    }

    /**
     * Rebind a relative capability to another ingress without changing the
     * API/account origin or spending the playback compatibility ladder.
     *
     * The account bearer travels with it. Media3's data source is built on
     * the app-wide client, whose interceptor attaches the header to every
     * request on every host, so there is no per-request choice to make here —
     * which is exactly why `configureNodeOrigins` refuses a scheme downgrade
     * and why only server-advertised origins ever reach this function.
     */
    fun nextMediaFailoverUrl(path: String): String? {
        if (!path.startsWith('/') || path.startsWith("//")) return null
        return synchronized(nodeLock) {
            if (mediaFailoverIndex >= mediaFailoverOrigins.size) {
                null
            } else {
                mediaFailoverOrigins[mediaFailoverIndex++] + path
            }
        }
    }

    /**
     * The origin this session is bound to, in the same canonical form the
     * failover list uses, or null when it is unusable. Callers compare
     * absolute URLs against this rather than against the raw [origin]
     * string, so `https://h` and `https://h:443` do not read as two servers.
     */
    fun canonicalPrimaryOrigin(): String? = canonicalOrigin(origin)

    /**
     * Scheme and host lowercased, default ports removed, IPv6 re-bracketed —
     * so two spellings of one address compare equal. Userinfo, a path, a
     * query, a fragment, or a non-HTTP scheme make it unusable: this string
     * becomes a request authority.
     */
    fun canonicalOrigin(raw: String): String? = runCatching {
        val uri = java.net.URI(raw)
        val scheme = uri.scheme?.lowercase()
        if (scheme != "http" && scheme != "https") return null
        if (uri.userInfo != null || uri.query != null || uri.fragment != null) return null
        if (uri.path.isNotEmpty() && uri.path != "/") return null
        // `URI` only exposes host/port for names it recognises, and a
        // container hostname with an underscore is not one of them — it lands
        // in `authority` whole. Splitting it here keeps `http://plurx_b:32400`
        // from silently canonicalising to `http://plurx_b`.
        val authority = uri.authority?.takeIf { it.isNotEmpty() && '@' !in it } ?: return null
        val closingBracket = authority.lastIndexOf(']')
        val colon = authority.lastIndexOf(':')
        val hasPort = colon > closingBracket && colon >= 0
        val rawHost = if (hasPort) authority.substring(0, colon) else authority
        val port = if (hasPort) {
            authority.substring(colon + 1).toIntOrNull() ?: return null
        } else {
            -1
        }
        if (rawHost.isEmpty()) return null
        if (hasPort && (port < 1 || port > 65_535)) return null
        val lowered = rawHost.lowercase().removeSurrounding("[", "]")
        if (lowered.isEmpty()) return null
        val host = if (':' in lowered) "[$lowered]" else lowered
        val defaultPort = if (scheme == "https") 443 else 80
        val suffix = if (port >= 0 && port != defaultPort) ":$port" else ""
        "$scheme://$host$suffix"
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
