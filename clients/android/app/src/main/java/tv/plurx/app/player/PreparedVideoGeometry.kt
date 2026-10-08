package tv.plurx.app.player

internal data class PreparedVideoGeometry(
    val scaleX: Float,
    val scaleY: Float,
    val left: Float,
    val top: Float,
)

/**
 * Surface(control)'s buffer queue maps decoded frames into the size supplied
 * when the SurfaceControl was built. Its parent transform therefore operates
 * in that surface space, not decoder pixels. Keep that space stable across
 * video-size changes and window resizing; fit the video aspect only once.
 */
internal fun preparedVideoGeometry(
    hostWidth: Int,
    hostHeight: Int,
    surfaceWidth: Int,
    surfaceHeight: Int,
    videoWidth: Int,
    videoHeight: Int,
    pixelRatio: Float,
): PreparedVideoGeometry? {
    if (minOf(hostWidth, hostHeight, surfaceWidth, surfaceHeight, videoWidth, videoHeight) <= 0 ||
        !pixelRatio.isFinite() || pixelRatio <= 0f) return null
    val displayWidth = videoWidth * pixelRatio
    if (!displayWidth.isFinite()) return null
    val fit = minOf(hostWidth / displayWidth, hostHeight.toFloat() / videoHeight)
    val width = displayWidth * fit
    val height = videoHeight * fit
    return PreparedVideoGeometry(
        width / surfaceWidth, height / surfaceHeight,
        (hostWidth - width) / 2f, (hostHeight - height) / 2f,
    )
}
