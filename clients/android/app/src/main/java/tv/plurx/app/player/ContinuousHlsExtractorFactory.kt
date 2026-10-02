@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.net.Uri
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.TimestampAdjuster
import androidx.media3.common.util.ParsableByteArray
import androidx.media3.common.DataReader
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.hls.DefaultHlsExtractorFactory
import androidx.media3.exoplayer.hls.HlsExtractorFactory
import androidx.media3.exoplayer.hls.HlsMediaChunkExtractor
import androidx.media3.exoplayer.source.SampleQueue
import androidx.media3.extractor.ExtractorInput
import androidx.media3.extractor.ExtractorOutput
import androidx.media3.extractor.SeekMap
import androidx.media3.extractor.TrackOutput
import androidx.media3.extractor.text.SubtitleParser

/** Observe actual queue metadata acceptance, including queue rejection. The
 * extractor's creation URI is not provenance: reused extractors instead read
 * the current verified loader context on every metadata callback. */
internal class ContinuousHlsExtractorFactory(
    private val owner: Any,
    private val accepted: (AcceptedSample) -> Unit,
    private val completed: (ContinuousLoadContext.Verified) -> Unit,
    private val delegate: HlsExtractorFactory = DefaultHlsExtractorFactory(),
    private val writing: (ContinuousLoadContext.Verified) -> Unit = {},
) : HlsExtractorFactory {
    data class AcceptedSample(
        val load: ContinuousLoadContext.Verified,
        val queue: SampleQueue,
        val before: Int,
        val after: Int,
        val timeUs: Long,
        val flags: Int,
        val verifiedFormat: Boolean = true,
    )
    override fun createExtractor(uri: Uri, format: Format, muxedCaptionFormats: MutableList<Format>?,
        timestampAdjuster: TimestampAdjuster, responseHeaders: MutableMap<String, MutableList<String>>,
        sniffingExtractorInput: ExtractorInput, playerId: PlayerId): HlsMediaChunkExtractor =
        wrap(delegate.createExtractor(uri, format, muxedCaptionFormats, timestampAdjuster, responseHeaders, sniffingExtractorInput, playerId))

    override fun setSubtitleParserFactory(factory: SubtitleParser.Factory): HlsExtractorFactory {
        delegate.setSubtitleParserFactory(factory); return this
    }
    override fun experimentalParseSubtitlesDuringExtraction(enabled: Boolean): HlsExtractorFactory {
        delegate.experimentalParseSubtitlesDuringExtraction(enabled); return this
    }
    override fun experimentalSetCodecsToParseWithinGopSampleDependencies(codecs: Int): HlsExtractorFactory {
        delegate.experimentalSetCodecsToParseWithinGopSampleDependencies(codecs); return this
    }
    override fun getOutputTextFormat(format: Format): Format = delegate.getOutputTextFormat(format)

    private fun wrap(extractor: HlsMediaChunkExtractor): HlsMediaChunkExtractor = object : HlsMediaChunkExtractor {
        override fun init(output: ExtractorOutput) = extractor.init(object : ExtractorOutput {
            override fun track(id: Int, type: Int): TrackOutput {
                val target = output.track(id, type)
                return object : TrackOutput by target {
                    private var format: Format? = null
                    override fun format(format: Format) { this.format = format; target.format(format) }
                    private fun writing() {
                        ContinuousLoadContext.current()?.takeIf { it.owner === owner }?.let(writing)
                    }
                    override fun sampleData(input: DataReader, length: Int, allowEndOfInput: Boolean, sampleDataPart: Int): Int {
                        writing()
                        return target.sampleData(input, length, allowEndOfInput, sampleDataPart)
                    }
                    override fun sampleData(data: ParsableByteArray, length: Int, sampleDataPart: Int) {
                        writing()
                        target.sampleData(data, length, sampleDataPart)
                    }
                    override fun sampleMetadata(timeUs: Long, flags: Int, size: Int, offset: Int, cryptoData: TrackOutput.CryptoData?) {
                        val load = ContinuousLoadContext.current()
                        val queue = target as? SampleQueue
                        if (queue == null || load?.owner !== owner) {
                            target.sampleMetadata(timeUs, flags, size, offset, cryptoData)
                            return
                        }
                        // getWriteIndex is not synchronized in Media3. Its
                        // actual before/after observation shares the monitor
                        // used by metadata insertion and consumer disposal.
                        val indices = synchronized(queue) {
                            val before = queue.writeIndex
                            target.sampleMetadata(timeUs, flags, size, offset, cryptoData)
                            before to queue.writeIndex
                        }
                        if (indices.second.toLong() - indices.first.toLong() == 1L) {
                            accepted(AcceptedSample(load, queue, indices.first, indices.second, timeUs, flags, matches(format, load.resource)))
                        }
                    }
                }
            }
            override fun endTracks() = output.endTracks()
            override fun seekMap(seekMap: SeekMap) = output.seekMap(seekMap)
        })
        override fun read(input: ExtractorInput): Boolean {
            val more = extractor.read(input)
            if (!more) ContinuousLoadContext.current()?.takeIf { it.owner === owner }?.let(completed)
            return more
        }
        override fun isPackedAudioExtractor(): Boolean = extractor.isPackedAudioExtractor
        override fun isReusable(): Boolean = extractor.isReusable
        override fun recreate(): HlsMediaChunkExtractor = wrap(extractor.recreate())
        override fun onTruncatedSegmentParsed() = extractor.onTruncatedSegmentParsed()
    }
    private fun matches(format: Format?, resource: ContinuousQualityMedia.Resource): Boolean = if (resource.role == "video") {
        format?.sampleMimeType == MimeTypes.VIDEO_H264 && format.width.toLong() == resource.row.number("width") &&
            format.height.toLong() == resource.row.number("height") && format.drmInitData == null
    } else format?.sampleMimeType == MimeTypes.AUDIO_AAC && format.sampleRate.toLong() == resource.row.number("timescale") &&
        format.channelCount.toLong() == resource.row.number("channels") && format.drmInitData == null
}
