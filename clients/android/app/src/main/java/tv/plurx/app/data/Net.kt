package tv.plurx.app.data

import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.okhttp.OkHttpDataSource
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import retrofit2.Retrofit
import retrofit2.converter.kotlinx.serialization.asConverterFactory
import tv.plurx.app.BuildConfig
import java.util.concurrent.TimeUnit

/**
 * One shared OkHttpClient (adds the bearer token from [Session] on every
 * request) drives Retrofit, Coil image loading, and Media3 playback — so
 * artwork and video streams authenticate exactly like the API.
 */
object Net {
    val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    val client: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(60, TimeUnit.SECONDS)
        .apply {
            // D-03 M5 measures contention before deciding whether Coil needs
            // a separate dispatcher. Release networking remains uninstrumented.
            if (BuildConfig.DEBUG) eventListenerFactory(NetCallDiagnostics.factory())
        }
        .addInterceptor { chain ->
            val captured = Session.readAfterAuthorization()
            val token = captured.token
            val req = chain.request()
            // `/server` is public and identifies rediscovered candidates.
            // Never leak a saved bearer token while probing the LAN.
            val publicIdentityRequest = req.url.encodedPath == "/api/v1/server"
            val out = if (token != null && !publicIdentityRequest) {
                req.newBuilder().header("Authorization", "Bearer $token")
                    .tag(ReadAfterAuthorization::class.java,
                        captured).build()
            } else {
                req
            }
            val authorization = Session.playbackAuthorization()
            val started = android.os.SystemClock.elapsedRealtime()
            val path = out.url.encodedPath
            val route = when { path.startsWith("/api/v1/auth/") -> "auth"; path.startsWith("/api/v1/diagnostics/") -> "diagnostics"; path.contains("/sessions") || path.endsWith("/decision") -> "playback"; else -> "browse" }
            fun observe(status: Int, failure: String? = null) {
                if (authorization.generation != Session.playbackAuthorization().generation || authorization.token != token) return
                val facts = mutableMapOf("route" to kotlinx.serialization.json.JsonPrimitive(route), "method" to kotlinx.serialization.json.JsonPrimitive(out.method), "status" to kotlinx.serialization.json.JsonPrimitive(status), "duration_ms" to kotlinx.serialization.json.JsonPrimitive((android.os.SystemClock.elapsedRealtime()-started).coerceIn(0,604800000)))
                if (failure != null) facts["failure"] = kotlinx.serialization.json.JsonPrimitive(failure)
                tv.plurx.app.diagnostics.DiagnosticRuntime.record("network","request",facts,authorization.generation)
            }
            try { chain.proceed(out).also { observe(it.code) } } catch (error: java.io.IOException) { observe(0,"network"); throw error }
        }
        .addInterceptor(ReadAfterNetworking.interceptor)
        .addNetworkInterceptor(ReadAfterNetworking.interceptor.redirectGuard)
        .build()

    /** Capability URLs already carry their narrow authority. Adding the
     * account bearer would widen that authority and expose it to a surface
     * that deliberately does not need it. */
    val capabilityClient: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(60, TimeUnit.SECONDS)
        .followRedirects(false)
        .followSslRedirects(false)
        .build()

    /** Bind background work to the profile that created it. The app-wide
     * client intentionally follows [Session], but a transfer can outlive a
     * screen and must never inherit the next signed-in profile's bearer. */
    fun profileClient(token: String): OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(60, TimeUnit.SECONDS)
        .followRedirects(false)
        .followSslRedirects(false)
        .addInterceptor { chain ->
            chain.proceed(
                chain.request().newBuilder()
                    .header("Authorization", "Bearer $token")
                    .build(),
            )
        }
        .build()

    /** Opt in only clients that already captured and fence the full account
     * generation. Ordinary background profile/capability clients stay opaque;
     * this never replaces their bearer or adopts a later login. */
    internal fun capturedReceiptClient(auth: Session.PlaybackAuthorization, transport: OkHttpClient): OkHttpClient =
        transport.newBuilder().apply {
            // Wrap existing interceptors as well: a test/cache interceptor
            // may answer without reaching the network or later interceptors.
            interceptors().add(0, okhttp3.Interceptor { chain ->
                val original = chain.request()
                val current = Session.readAfterAuthorization()
                val peers = if (current.generation == auth.generation && current.origin == auth.origin && current.token == auth.token) current.peers else emptyList()
                val captured = ReadAfterAuthorization(auth.origin, auth.token, auth.generation, peers)
                chain.proceed(original.newBuilder().tag(ReadAfterAuthorization::class.java, captured).build())
            })
            interceptors().add(1, ReadAfterNetworking.interceptor)
            addNetworkInterceptor(ReadAfterNetworking.interceptor.redirectGuard)
        }.build()

    private val contentType = "application/json".toMediaType()

    /** Build an API bound to a server origin (`http://host:32400`). */
    fun api(origin: String): PlurxApi =
        api(origin, client)

    fun api(origin: String, httpClient: OkHttpClient): PlurxApi =
        Retrofit.Builder()
            .baseUrl("$origin/api/v1/")
            .client(httpClient)
            .addConverterFactory(json.asConverterFactory(contentType))
            .build()
            .create(PlurxApi::class.java)

    /** Media3 HTTP data source that carries the same auth header. */
    @UnstableApi
    fun dataSourceFactory(): OkHttpDataSource.Factory =
        OkHttpDataSource.Factory(client)
}
