package tv.plurx.app.player

import tv.plurx.app.data.PlaybackQuality

/** Whether the in-session continuous attachment, rather than a reopen or a
 * prepared successor, executes a quality change. A closed attachment owns
 * nothing: once its admission is fenced (End, a fallback to Direct play or the
 * progressive remux, teardown) its change() can only refuse, so claiming the
 * change would strand the viewer behind "could not be selected continuously",
 * block Auto candidates and pin the recipe to the last continuous rung. */
internal object ContinuousQualityOwnership {
    fun owns(
        attached: Boolean,
        closed: Boolean,
        ownPlayer: Boolean,
        quality: PlaybackQuality,
        hasRendition: (Int) -> Boolean,
    ): Boolean = attached && !closed && ownPlayer &&
        (quality == PlaybackQuality.Auto || quality.rungHeight?.let(hasRendition) == true)
}
