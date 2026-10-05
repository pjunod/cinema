package tv.plurx.app.livetv

import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import coil.ImageLoader
import coil.compose.AsyncImage
import coil.request.ImageRequest
import okhttp3.CookieJar
import okhttp3.OkHttpClient
import java.util.concurrent.TimeUnit

/**
 * Station artwork for the Live TV guide.
 *
 * Which address a channel's logo comes from is the shared guide contract's
 * answer ([LiveTvGuideReducer.stationLogoUrl], pinned by
 * `tests/playback/live-tv-guide-cases.json` `station_logo`). This file only
 * fetches and draws it.
 *
 * The address is HDHomeRun's, not this server's. The app-wide Coil loader runs
 * on [tv.plurx.app.data.Net.client], which adds the account bearer to every
 * request it makes, so a station logo must never go through it: this loader
 * owns a bare client with no interceptors, no cookies and no redirect between
 * HTTPS and HTTP.
 */
object LiveTvStationLogos {
    @Volatile
    private var loader: ImageLoader? = null

    fun loader(context: Context): ImageLoader =
        loader ?: synchronized(this) {
            loader ?: ImageLoader.Builder(context.applicationContext)
                .okHttpClient {
                    OkHttpClient.Builder()
                        .connectTimeout(15, TimeUnit.SECONDS)
                        .readTimeout(15, TimeUnit.SECONDS)
                        .cookieJar(CookieJar.NO_COOKIES)
                        // The contract only admits `https://`; a redirect must
                        // not walk the request off it.
                        .followSslRedirects(false)
                        .build()
                }
                .crossfade(false)
                .build()
                .also { loader = it }
        }
}

/**
 * The station tile: the callsign, replaced by the logo once it has decoded.
 *
 * The callsign is the fallback and the layout: it is drawn immediately, the
 * tile never changes size, and an image that never arrives — no artwork, a
 * refused address, a failed fetch — leaves exactly what was there before
 * logos existed. Loading a logo never delays anything else on the page.
 */
@Composable
fun LiveTvStationChip(
    name: String,
    logo: String?,
    width: Dp,
    height: Dp,
    style: TextStyle,
    shape: Shape,
    modifier: Modifier = Modifier,
    background: Color = MaterialTheme.colorScheme.surfaceVariant,
    textColor: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    padding: Dp = 4.dp,
) {
    var loaded by remember(logo) { mutableStateOf(false) }
    Box(
        modifier.size(width = width, height = height).clip(shape).background(background),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            name,
            style = style,
            color = textColor,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            textAlign = TextAlign.Center,
            modifier = Modifier.padding(horizontal = 3.dp).alpha(if (loaded) 0f else 1f),
        )
        if (logo != null) {
            val context = LocalContext.current
            AsyncImage(
                model = remember(logo) { ImageRequest.Builder(context).data(logo).build() },
                contentDescription = null,
                imageLoader = LiveTvStationLogos.loader(context),
                contentScale = ContentScale.Fit,
                onSuccess = { loaded = true },
                onError = { loaded = false },
                modifier = Modifier.fillMaxSize().padding(padding).alpha(if (loaded) 1f else 0f),
            )
        }
    }
}
