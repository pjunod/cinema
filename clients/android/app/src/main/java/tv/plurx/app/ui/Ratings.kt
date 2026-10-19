package tv.plurx.app.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Group
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import tv.plurx.app.data.Rating
import tv.plurx.app.ui.theme.Muted

private val Amber = Color(0xFFFBBF24)
private val Fresh = Color(0xFFEF4444)
private val Rotten = Color(0xFF65A30D)
private val Good = Color(0xFF22C55E)
private val Ink = Color(0xFF07101F)
private val Blue = Color(0xFF6EA8FE)

/** Full and short names of each score source, in the order the server sends them. */
fun ratingSourceName(source: String, short: Boolean = false): String = when (source) {
    "imdb" -> "IMDb"
    "rotten_tomatoes" -> if (short) "RT" else "Rotten Tomatoes"
    "metacritic" -> "Metacritic"
    "tmdb" -> "TMDB"
    else -> source
}

/** How a score reads: IMDb out of ten with one decimal, Metacritic as a whole number, the rest as a percentage. */
fun ratingText(rating: Rating): String = when {
    rating.scale == 10 -> String.format(java.util.Locale.ROOT, "%.1f", rating.value)
    rating.source == "metacritic" -> Math.round(rating.value).toString()
    else -> "${Math.round(rating.value)}%"
}

/** A Rotten Tomatoes score of 60% or more is a positive critics' verdict. */
fun rottenTomatoesPositive(value: Double): Boolean = value >= 60.0

/** What a screen reader says for one score. */
fun ratingDescription(rating: Rating): String =
    "${ratingSourceName(rating.source)} ${ratingText(rating)}" + (rating.votes?.let { ", from $it votes" } ?: "")

/**
 * The one place a score source's mark is drawn. Original glyphs keyed by
 * source; change a source's artwork here and every rating in the app follows,
 * without touching any layout.
 */
@Composable
fun RatingMark(source: String, value: Double, size: Dp = 18.dp) {
    when (source) {
        "imdb" -> Icon(Icons.Filled.Star, contentDescription = null, tint = Amber, modifier = Modifier.size(size))
        "rotten_tomatoes" -> {
            val positive = rottenTomatoesPositive(value)
            Canvas(Modifier.size(size)) {
                drawCircle(if (positive) Fresh else Rotten)
                val w = this.size.width
                val stroke = w * 0.11f
                if (positive) {
                    drawLine(Color.White, Offset(w * 0.30f, w * 0.52f), Offset(w * 0.44f, w * 0.66f), stroke, StrokeCap.Round)
                    drawLine(Color.White, Offset(w * 0.44f, w * 0.66f), Offset(w * 0.71f, w * 0.38f), stroke, StrokeCap.Round)
                } else {
                    drawLine(Color.White, Offset(w * 0.34f, w * 0.34f), Offset(w * 0.66f, w * 0.66f), stroke, StrokeCap.Round)
                    drawLine(Color.White, Offset(w * 0.66f, w * 0.34f), Offset(w * 0.34f, w * 0.66f), stroke, StrokeCap.Round)
                }
            }
        }
        "metacritic" -> {
            val band = when {
                value >= 61 -> Good
                value >= 40 -> Amber
                else -> Fresh
            }
            Box(Modifier.size(size).background(band, RoundedCornerShape(size / 4)), contentAlignment = Alignment.Center) {
                Text(Math.round(value).toString(), color = Ink, fontWeight = FontWeight.ExtraBold, fontSize = (size.value * 0.45f).sp)
            }
        }
        "tmdb" -> Icon(Icons.Filled.Group, contentDescription = null, tint = Blue, modifier = Modifier.size(size))
        else -> Unit
    }
}

/** One score as a chip: its mark, the score and the short source name. */
@Composable
fun RatingChip(rating: Rating) {
    Row(
        Modifier
            .border(1.dp, MaterialTheme.colorScheme.outlineVariant, RoundedCornerShape(8.dp))
            .padding(horizontal = 8.dp, vertical = 4.dp)
            .semantics { contentDescription = ratingDescription(rating) },
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RatingMark(rating.source, rating.value)
        Text(ratingText(rating), fontWeight = FontWeight.Bold, style = MaterialTheme.typography.labelLarge)
        Text(ratingSourceName(rating.source, short = true), color = Muted, style = MaterialTheme.typography.labelSmall)
    }
}
