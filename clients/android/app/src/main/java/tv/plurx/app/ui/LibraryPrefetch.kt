package tv.plurx.app.ui

/** ensure() takes an exclusive item count; the last visible card is zero-based.
 * A-03 requests exactly two rows using the measured adaptive grid column count. */
internal fun libraryPrefetchExclusive(lastVisibleIndex: Int, columns: Int): Int {
    if (lastVisibleIndex < 0 || columns < 1) return 0
    return (lastVisibleIndex.toLong() + 1L + 2L * columns).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
}
