package tv.plurx.app.livetv

/**
 * Every server Live TV card Android draws, and which settings screen owns it.
 *
 * Paul's Developer rule (AGENTS.md, SETTINGS-NAVIGATION-AND-DEVELOPER-STATUS):
 * Developer holds only features still in development, and Android has none: a
 * card that is a finished setting lives in its proper section. The web moved the Live TV
 * enable switch and the tuner, guide, recording and Library channel cards to
 * Settings → Live TV; this client follows it, in the web's order.
 */
enum class LiveTvSettingsCard(val title: String) {
    EnableLiveTv("Enable Live TV"),
    HdHomeRun("HDHomeRun Live TV"),
    ProgrammeGuide("Programme guide"),
    Recording("Recording"),
    LibraryChannels("Library channels"),
    DisplayCadence("Match television refresh rate"),
}

object LiveTvSettingsPlacement {
    /** Settings → Live TV, in the web's `liveTvPanel` order. */
    val liveTvSettings: List<LiveTvSettingsCard> = listOf(
        LiveTvSettingsCard.EnableLiveTv,
        LiveTvSettingsCard.HdHomeRun,
        LiveTvSettingsCard.ProgrammeGuide,
        LiveTvSettingsCard.Recording,
        LiveTvSettingsCard.LibraryChannels,
    )

    /** Settings → Playback: server-key switches that sit beside the device's own playback choices. */
    val playback: List<LiveTvSettingsCard> = listOf(
        LiveTvSettingsCard.DisplayCadence,
    )
}
