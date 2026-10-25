package tv.plurx.app.ui

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.ItemAnnotations

class RatingsTest {
    private val json = Json { ignoreUnknownKeys = true }
    private val body = """{"ratings":[{"source":"imdb","value":8.1,"scale":10,"votes":214000},{"source":"rotten_tomatoes","value":94,"scale":100},{"source":"metacritic","value":81,"scale":100},{"source":"tmdb","value":78,"scale":100,"votes":3900}],"watchlist":true,"favorite":false}"""

    @Test
    fun annotationsDecodeWithScoresInServerOrder() {
        val a = json.decodeFromString<ItemAnnotations>(body)
        assertEquals(listOf("imdb", "rotten_tomatoes", "metacritic", "tmdb"), a.ratings.map { it.source })
        assertTrue(a.watchlist)
        assertFalse(a.favorite)
        assertEquals(214000L, a.ratings[0].votes)
    }

    @Test
    fun eachSourceReadsOnItsOwnScale() {
        val a = json.decodeFromString<ItemAnnotations>(body)
        assertEquals(listOf("8.1", "94%", "81", "78%"), a.ratings.map(::ratingText))
        assertEquals("IMDb 8.1, from 214000 votes", ratingDescription(a.ratings[0]))
    }

    @Test
    fun rottenTomatoesTurnsPositiveAtSixty() {
        assertFalse(rottenTomatoesPositive(59.0))
        assertTrue(rottenTomatoesPositive(60.0))
    }
}
