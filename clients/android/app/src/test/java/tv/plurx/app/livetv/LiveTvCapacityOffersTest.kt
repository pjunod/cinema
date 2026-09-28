package tv.plurx.app.livetv

import org.junit.Assert.*
import org.junit.Test

class LiveTvCapacityOffersTest {
    private val row = LiveTvWatchable("known", "6.1")
    private val channel = LiveTvChannel("known", "6.1", "Synthetic News")
    private val offer = LiveTvCapacityOffer(7, listOf(row))

    @Test fun capacityOfferRequiresAnOwnerDecidedCapacityRefusal() {
        val failure = LiveTvFailure("tuner_capacity", ownerDecided = true, watchable = listOf(row))
        assertEquals(offer, liveTvCapacityOffer(failure, 7))
        assertNull(liveTvCapacityOffer(LiveTvFailure("tuner_capacity", watchable = listOf(row)), 7))
        assertNull(liveTvCapacityOffer(LiveTvFailure("tuner_capacity", ownerDecided = false, watchable = listOf(row)), 7))
        assertNull(liveTvCapacityOffer(LiveTvFailure("tuner_unavailable", ownerDecided = true, watchable = listOf(row)), 7))
    }

    @Test fun capacityOffersUseOnlyDistinctKnownPlayableChannels() {
        val choices = LiveTvCapacityOffer(7, listOf(row, row,
            LiveTvWatchable("unknown", "9.1"), LiveTvWatchable("protected", "9.2"),
            LiveTvWatchable("unsupported", "9.3"), LiveTvWatchable("wrong-number", "9.4")))
        val lineup = listOf(channel,
            LiveTvChannel("protected", "9.2", "Protected", drm = true),
            LiveTvChannel("unsupported", "9.3", "Unsupported", support = "drm_unsupported"),
            LiveTvChannel("wrong-number", "99.1", "Changed"))
        assertEquals(listOf(row to channel), liveTvWatchableChoices(choices, lineup))
        assertTrue(liveTvWatchableChoices(offer, listOf(channel, channel.copy(guide_name = "Ambiguous"))).isEmpty())
        assertTrue(liveTvWatchableChoices(offer, emptyList()).isEmpty())
    }

    @Test fun capacityChoiceRequiresSameRefusalGenerationAndIdleState() {
        fun resolve(current: LiveTvCapacityOffer? = offer, generation: Long = 7,
                    busy: Boolean = false, playing: Boolean = false,
                    lineup: List<LiveTvChannel> = listOf(channel)) =
            liveTvCapacityChoice(current, offer, row, generation, lineup, busy, playing)
        assertSame(channel, resolve())
        assertNull(resolve(offer.copy()))
        assertNull(resolve(null))
        assertNull(resolve(generation = 8))
        assertNull(resolve(busy = true))
        assertNull(resolve(playing = true))
        assertNull(resolve(lineup = listOf(channel.copy(drm = true))))
        assertNull(liveTvCapacityChoice(offer, offer, LiveTvWatchable("unknown", "9.1"),
            7, listOf(channel), busy = false, playing = false))
    }
}
