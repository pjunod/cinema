@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.TrackGroup
import androidx.media3.common.TrackSelectionParameters
import androidx.media3.common.Tracks
import org.junit.Assert.*
import org.junit.Test

class LiveTvCaptionsTest {
    private fun caption(mime: String, channel: Int) = Format.Builder()
        .setSampleMimeType(mime).setLanguage("en").setLabel("English")
        .setAccessibilityChannel(channel).build()

    private fun group(
        format: Format,
        support: Int = C.FORMAT_HANDLED,
        selected: Boolean = false,
    ) = Tracks.Group(TrackGroup(format), false, intArrayOf(support), booleanArrayOf(selected))

    @Test fun captionOptionsNameOnlyActualTextTracksAndServices() {
        val tracks = Tracks(listOf(
            group(Format.Builder().setSampleMimeType(MimeTypes.AUDIO_AAC).build()),
            group(caption(MimeTypes.APPLICATION_CEA608, 1)),
            group(caption(MimeTypes.APPLICATION_CEA708, 3)),
        ))
        assertEquals(listOf("English · CC1", "English · Service 3"),
            liveTvCaptionChoices(tracks).map { it.label })
        assertTrue(liveTvCaptionChoices(Tracks.EMPTY).isEmpty())
    }

    @Test fun selectionDisablesOnlyTextAndPinsTheObservedService() {
        val text = group(caption(MimeTypes.APPLICATION_CEA708, 3))
        val tracks = Tracks(listOf(text))
        val audio = TrackGroup(Format.Builder().setSampleMimeType(MimeTypes.AUDIO_AAC).build())
        val audioOverride = TrackSelectionOverride(audio, 0)
        val initial = TrackSelectionParameters.Builder().setPreferredAudioLanguage("fr")
            .setOverrideForType(audioOverride).build()
        val selected = checkNotNull(liveTvCaptionSelection(tracks, initial,
            liveTvCaptionChoices(tracks).single()))
        assertEquals(initial.preferredAudioLanguages, selected.preferredAudioLanguages)
        assertFalse(C.TRACK_TYPE_TEXT in selected.disabledTrackTypes)
        assertEquals(listOf(0), selected.overrides.getValue(text.mediaTrackGroup).trackIndices)
        val off = checkNotNull(liveTvCaptionSelection(tracks, selected, null))
        assertTrue(C.TRACK_TYPE_TEXT in off.disabledTrackTypes)
        assertFalse(C.TRACK_TYPE_AUDIO in off.disabledTrackTypes)
        assertEquals(mapOf(audio to audioOverride), off.overrides)
        assertEquals(initial.preferredAudioLanguages, off.preferredAudioLanguages)
    }

    @Test fun selectionRejectsAReplacedOrUnsupportedTrackGroup() {
        val original = group(caption(MimeTypes.APPLICATION_CEA608, 1))
        val choice = liveTvCaptionChoices(Tracks(listOf(original))).single()
        val parameters = TrackSelectionParameters.Builder().build()
        // A new attachment can report equal formats; its old row still does
        // not own the replacement TrackGroup object.
        val replacement = group(caption(MimeTypes.APPLICATION_CEA608, 1))
        assertNull(liveTvCaptionSelection(Tracks(listOf(replacement)), parameters, choice))
        val unsupported = Tracks.Group(original.mediaTrackGroup, false,
            intArrayOf(C.FORMAT_UNSUPPORTED_TYPE), booleanArrayOf(false))
        assertNull(liveTvCaptionSelection(Tracks(listOf(unsupported)), parameters, choice))
        assertNull(liveTvCaptionSelection(Tracks.EMPTY, parameters, choice))
    }

    @Test fun captionSummarySeparatesSelectionFromCueDelivery() {
        val tracks = Tracks(listOf(group(caption(MimeTypes.APPLICATION_CEA608, 1), selected = true)))
        assertEquals("English · CC1 · no current text cues", liveTvCaptionSummary(tracks, false))
        assertEquals("English · CC1 · text cues present", liveTvCaptionSummary(tracks, true))
        assertEquals("Off", liveTvCaptionSummary(Tracks.EMPTY, true))
    }
}
