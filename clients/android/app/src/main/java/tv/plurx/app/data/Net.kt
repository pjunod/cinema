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
 *
 * The watch-write floor ([ReadAfter]) is echoed only on requests that carry a
 * [ReadAfterBinding]. That is a per-call-site choice, not a client one: this
 * client also serves images and HLS, which live under `/api/v1/` too, and the
 * untagged [api] overloads also serve the captured-token logout, offline books
 * and unverified LAN probes, none of which may send or capture it. Only
 * [profileApi] (the current profile) and the generation-bound [api] overload
 * (the library pager) bind their calls.
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
        .addInterceptor(ReadAfter.interceptor)
        .addInterceptor { chain ->
            val token = Session.token
            val req = chain.request()
            // `/server` is public and identifies rediscovered candidates.
            // Never leak a saved bearer token while probing the LAN.
            val publicIdentityRequest = req.url.encodedPath == "/api/v1/server"
            val out = if (token != null && !publicIdentityRequest) {
                req.newBuilder().header("Authorization", "Bearer $token").build()
            } else {
                req
            }
            chain.proceed(out)
        }
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
        .addInterceptor(ReadAfter.interceptor)
        .addInterceptor { chain ->
            chain.proceed(
                chain.request().newBuilder()
                    .header("Authorization", "Bearer $token")
                    .build(),
            )
        }
        .build()

    private val contentType = "application/json".toMediaType()

    /** Build an API bound to a server origin (`http://host:32400`). Its
     * calls neither send nor capture the watch-write floor: it probes
     * candidate servers, which must never see or set this session's index. */
    fun api(origin: String): PlurxApi =
        api(origin, client)

    /** An unbound API on [httpClient]: the captured-token logout and offline
     * books use it and must not echo or capture the floor. */
    fun api(origin: String, httpClient: OkHttpClient): PlurxApi =
        retrofit(origin, httpClient)

    /** The current profile's API. Each call is bound to the signed-in
     * session current when it is made; [client] reads the bearer the same way. */
    fun profileApi(origin: String, httpClient: OkHttpClient = client): PlurxApi =
        retrofit(origin, ReadAfter.binding(httpClient) { ReadAfter.floor.generation() })

    /** An API on a profile-bound client (the library pager), bound to the
     * signed-in session [readAfterGeneration] it was created under. After
     * that session ends its calls send nothing and their replies are ignored. */
    fun api(origin: String, httpClient: OkHttpClient, readAfterGeneration: Long): PlurxApi =
        retrofit(origin, ReadAfter.binding(httpClient) { readAfterGeneration })

    private fun retrofit(origin: String, calls: okhttp3.Call.Factory): PlurxApi =
        Retrofit.Builder()
            .baseUrl("$origin/api/v1/")
            .callFactory(calls)
            .addConverterFactory(json.asConverterFactory(contentType))
            .build()
            .create(PlurxApi::class.java)

    /** Media3 HTTP data source that carries the same auth header. */
    @UnstableApi
    fun dataSourceFactory(): OkHttpDataSource.Factory =
        OkHttpDataSource.Factory(client)
}
