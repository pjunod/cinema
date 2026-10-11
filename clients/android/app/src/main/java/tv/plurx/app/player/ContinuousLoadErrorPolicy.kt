@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.upstream.DefaultLoadErrorHandlingPolicy
import androidx.media3.exoplayer.upstream.LoadErrorHandlingPolicy
import java.io.IOException

/** Raised before fetching or exposing any bytes for a superseded cold chunk. */
internal class ContinuousStaleVideoLoad : IOException("Unreserved superseded continuous video load")

/** Only the controlled source uses this policy. Its retained selection accepts
 * track fallback solely when another supported rendition is already reserved.
 * Ordinary network and extraction failures keep Media3's default policy. */
internal class ContinuousLoadErrorPolicy : DefaultLoadErrorHandlingPolicy() {
    override fun getFallbackSelectionFor(
        fallbackOptions: LoadErrorHandlingPolicy.FallbackOptions,
        loadErrorInfo: LoadErrorHandlingPolicy.LoadErrorInfo,
    ): LoadErrorHandlingPolicy.FallbackSelection? {
        if (loadErrorInfo.exception is ContinuousStaleVideoLoad && loadErrorInfo.loadEventInfo.bytesLoaded == 0L &&
            fallbackOptions.isFallbackAvailable(LoadErrorHandlingPolicy.FALLBACK_TYPE_TRACK)) {
            return LoadErrorHandlingPolicy.FallbackSelection(LoadErrorHandlingPolicy.FALLBACK_TYPE_TRACK, 1)
        }
        return super.getFallbackSelectionFor(fallbackOptions, loadErrorInfo)
    }
}
