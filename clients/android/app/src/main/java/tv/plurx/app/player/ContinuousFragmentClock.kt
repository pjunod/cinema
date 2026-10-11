package tv.plurx.app.player

import java.io.IOException
import java.math.BigInteger
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Bounded structural clock inspection for choosing an AAC reservation window.
 * This is not append or presentation proof; exact byte pins and actual queue
 * acceptance remain separate requirements. */
internal object ContinuousFragmentClock {
    private data class Box(val type: String, val body: Int, val end: Int)
    fun firstDecodeTick(bytes: ByteArray): Long {
        if (bytes.isEmpty() || bytes.size > ContinuousQualityMedia.MAX_MEDIA_BYTES) throw IOException("Continuous fragment clock payload bound")
        val input = ByteBuffer.wrap(bytes).order(ByteOrder.BIG_ENDIAN)
        var count = 0
        fun boxes(start: Int, end: Int): List<Box> {
            val result = ArrayList<Box>()
            var at = start
            while (at < end) {
                if (++count > 4096 || end - at < 8) throw IOException("Continuous fragment box bound")
                val length32 = input.getInt(at).toLong() and 0xffffffffL
                val type = String(bytes, at + 4, 4, Charsets.US_ASCII)
                val header = if (length32 == 1L) 16 else 8
                if (end - at < header) throw IOException("Continuous fragment extended box truncated")
                val length = when (length32) { 0L -> (end - at).toLong(); 1L -> input.getLong(at + 8); else -> length32 }
                if (length < header || length > end - at) throw IOException("Continuous fragment box extent")
                val through = at + length.toInt()
                result.add(Box(type, at + header, through))
                at = through
            }
            return result
        }
        val first = boxes(0, bytes.size).firstOrNull { it.type == "moof" } ?: throw IOException("Continuous fragment has no movie fragment")
        val tracks = boxes(first.body, first.end).filter { it.type == "traf" }
        if (tracks.size != 1) throw IOException("Continuous fragment track count")
        val track = tracks.single()
        val clocks = boxes(track.body, track.end).filter { it.type == "tfdt" }
        if (clocks.size != 1) throw IOException("Continuous fragment decode clock count")
        val clock = clocks.single()
        if (clock.end - clock.body < 4) throw IOException("Continuous fragment decode clock truncated")
        val version = bytes[clock.body].toInt() and 255
        if (bytes[clock.body + 1] != 0.toByte() || bytes[clock.body + 2] != 0.toByte() || bytes[clock.body + 3] != 0.toByte())
            throw IOException("Continuous fragment decode clock flags")
        val tick = when {
            version == 0 && clock.end - clock.body == 8 -> input.getInt(clock.body + 4).toLong() and 0xffffffffL
            version == 1 && clock.end - clock.body == 12 -> input.getLong(clock.body + 4)
            else -> throw IOException("Continuous fragment decode clock version or extent")
        }
        if (tick !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER) throw IOException("Continuous fragment decode clock bound")
        return tick
    }

    /** Use exact integer scaling before rounding to the containing video entry.
     * AAC boundaries can lie on either side of the nominal video boundary. */
    fun videoEntry(audioTick: Long, audioTimescale: Long, videoTimescale: Long, segmentTicks: Long): Long {
        if (audioTick !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER || audioTimescale !in 1..1_000_000 ||
            videoTimescale !in 1..1_000_000 || segmentTicks !in 1..ContinuousQualityWire.MAX_SAFE_INTEGER) {
            throw IOException("Continuous audio frontier clock bound")
        }
        val scaled = BigInteger.valueOf(audioTick).multiply(BigInteger.valueOf(videoTimescale))
            .divide(BigInteger.valueOf(audioTimescale))
        val segment = BigInteger.valueOf(segmentTicks)
        val entry = scaled.divide(segment).multiply(segment)
        if (entry > BigInteger.valueOf(ContinuousQualityWire.MAX_SAFE_INTEGER)) throw IOException("Continuous audio frontier bound")
        return entry.toLong()
    }
}
